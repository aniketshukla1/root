//! Kafka monitor.
//!
//! Kafka has no HTTP admin API, so this speaks the native protocol through
//! librdkafka. Three things are worth knowing about the shape of that:
//!
//! 1. **Every librdkafka call blocks.** They are wrapped in one
//!    `spawn_blocking` per poll rather than sprinkled through async code —
//!    otherwise a slow broker stalls the whole runtime, not just this poll.
//! 2. **Consumer lag needs a consumer.** There is no "get me group X's
//!    offsets" call on a plain client; you create a consumer configured with
//!    that `group.id` and ask what it has committed. It does not join the
//!    group or consume anything.
//! 3. **Everything is per-partition.** A cluster with 400 partitions means 400
//!    watermark calls, so the work is bounded rather than proportional to
//!    cluster size.
//!
//! The health judgement mirrors RabbitMQ's: a consumer group that has fallen
//! behind matters more than a topic that is merely large, and a group with lag
//! and *no members* is a stopped pipeline rather than a slow one.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{BaseConsumer, Consumer};
use rdkafka::{Offset, TopicPartitionList};
use tokio::sync::Mutex;

use super::{elapsed_ms, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, Section, ServiceKind,
};

const API_TIMEOUT: Duration = Duration::from_secs(10);
/// Watermark lookups are one round trip per partition, so the work is capped
/// rather than left proportional to cluster size.
const MAX_PARTITIONS: usize = 400;
/// Each group costs a client and an offset fetch.
const MAX_GROUPS: usize = 25;
const MAX_TOPIC_ROWS: usize = 60;

pub struct KafkaCollector {
    config: ClientConfig,
    bootstrap: String,
    lag_warn: i64,
    profile_id: String,
    profile_name: String,
    /// Problems already turned into log lines, so a standing fault is reported
    /// once rather than every poll.
    reported: Mutex<Vec<String>>,
}

// ------------------------------------------------------------------ facts
//
// Plain data, gathered on the blocking thread and rendered on the async side.

#[derive(Debug, Default)]
struct ClusterFacts {
    brokers: Vec<BrokerFact>,
    topics: Vec<TopicFact>,
    groups: Vec<GroupFact>,
    partitions_sampled: usize,
    partitions_total: usize,
}

#[derive(Debug)]
struct BrokerFact {
    id: i32,
    host: String,
    port: i32,
}

#[derive(Debug)]
struct TopicFact {
    name: String,
    partitions: usize,
    offline: usize,
    under_replicated: usize,
    messages: i64,
}

#[derive(Debug)]
struct GroupFact {
    name: String,
    state: String,
    members: usize,
    lag: Option<i64>,
}

impl KafkaCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let bootstrap = profile.endpoint.trim().to_string();
        if bootstrap.is_empty() {
            return Err(AppError::Config(
                "Kafka connections need bootstrap servers, e.g. broker1:9092,broker2:9092"
                    .to_string(),
            ));
        }
        // A URL scheme here is the most common mistake — bootstrap.servers is
        // host:port pairs, not a URL, and librdkafka's error for it is opaque.
        if bootstrap.contains("://") {
            return Err(AppError::Config(format!(
                "`{bootstrap}` should be host:port pairs without a scheme, e.g. broker:9092"
            )));
        }

        let mut config = ClientConfig::new();
        config
            .set("bootstrap.servers", &bootstrap)
            .set("client.id", "root-monitor")
            // Nothing here ever consumes; this keeps librdkafka from trying.
            .set("enable.auto.commit", "false")
            .set("socket.timeout.ms", "10000")
            .set("api.version.request", "true");

        let protocol = profile
            .options
            .string("securityProtocol")
            .unwrap_or_else(|| "PLAINTEXT".to_string());
        config.set("security.protocol", &protocol);

        if protocol.starts_with("SASL") {
            let mechanism = profile
                .options
                .string("saslMechanism")
                .unwrap_or_else(|| "PLAIN".to_string());
            config.set("sasl.mechanism", &mechanism);
            if let Some(username) = profile.username.clone().filter(|u| !u.is_empty()) {
                config.set("sasl.username", &username);
            }
            if let Some(secret) = secret.filter(|s| !s.is_empty()) {
                config.set("sasl.password", &secret);
            }
        }

        if profile.options.insecure_tls() {
            config.set("enable.ssl.certificate.verification", "false");
        }

        Ok(KafkaCollector {
            config,
            bootstrap,
            lag_warn: profile.options.u64("lagWarn").unwrap_or(10_000) as i64,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            reported: Mutex::new(Vec::new()),
        })
    }

    /// All librdkafka work for one poll, on a blocking thread.
    fn gather(config: ClientConfig) -> std::result::Result<ClusterFacts, String> {
        let consumer: BaseConsumer = config
            .create()
            .map_err(|e| format!("could not create a Kafka client: {e}"))?;

        let metadata = consumer
            .fetch_metadata(None, API_TIMEOUT)
            .map_err(|e| format!("metadata request failed: {e}"))?;

        let mut facts = ClusterFacts {
            brokers: metadata
                .brokers()
                .iter()
                .map(|broker| BrokerFact {
                    id: broker.id(),
                    host: broker.host().to_string(),
                    port: broker.port(),
                })
                .collect(),
            ..Default::default()
        };

        // Internal topics are Kafka's own bookkeeping; surfacing __consumer_offsets
        // as a monitored topic is noise in every cluster.
        let topics: Vec<_> = metadata
            .topics()
            .iter()
            .filter(|t| !t.name().starts_with("__"))
            .collect();

        let mut sampled = 0usize;
        for topic in &topics {
            let mut offline = 0usize;
            let mut under_replicated = 0usize;
            let mut messages = 0i64;

            for partition in topic.partitions() {
                facts.partitions_total += 1;

                // Leader -1 means no broker owns the partition: it is offline
                // and unavailable for reads and writes.
                if partition.leader() < 0 {
                    offline += 1;
                }
                if partition.isr().len() < partition.replicas().len() {
                    under_replicated += 1;
                }

                if sampled < MAX_PARTITIONS {
                    sampled += 1;
                    if let Ok((low, high)) =
                        consumer.fetch_watermarks(topic.name(), partition.id(), API_TIMEOUT)
                    {
                        messages += (high - low).max(0);
                    }
                }
            }

            facts.topics.push(TopicFact {
                name: topic.name().to_string(),
                partitions: topic.partitions().len(),
                offline,
                under_replicated,
                messages,
            });
        }
        facts.partitions_sampled = sampled;

        // ------------------------------------------------------- groups
        let group_list = consumer
            .fetch_group_list(None, API_TIMEOUT)
            .map_err(|e| format!("group list request failed: {e}"))?;

        // Every known topic-partition, reused for each group's offset fetch.
        let mut all_partitions = TopicPartitionList::new();
        for topic in &topics {
            for partition in topic.partitions() {
                let _ = all_partitions.add_partition(topic.name(), partition.id());
            }
        }

        for group in group_list.groups().iter().take(MAX_GROUPS) {
            let lag = group_lag(&config, group.name(), &all_partitions, &consumer);
            facts.groups.push(GroupFact {
                name: group.name().to_string(),
                state: group.state().to_string(),
                members: group.members().len(),
                lag,
            });
        }

        Ok(facts)
    }
}

/// Total lag for one consumer group across every known partition.
///
/// Kafka has no "read another group's offsets" call, so this builds a client
/// carrying that `group.id` and asks what it has committed. The client never
/// subscribes, so it neither joins the group nor triggers a rebalance —
/// important, because accidentally joining would disrupt the very consumers
/// being measured.
fn group_lag(
    config: &ClientConfig,
    group: &str,
    partitions: &TopicPartitionList,
    watermark_source: &BaseConsumer,
) -> Option<i64> {
    let mut group_config = config.clone();
    group_config.set("group.id", group);
    let probe: BaseConsumer = group_config.create().ok()?;

    let committed = probe
        .committed_offsets(partitions.clone(), API_TIMEOUT)
        .ok()?;

    let mut total = 0i64;
    let mut saw_any = false;

    for element in committed.elements() {
        // Partitions this group has never committed to come back Invalid;
        // counting them as offset 0 would report the entire topic as lag.
        let Offset::Offset(position) = element.offset() else {
            continue;
        };
        let Ok((_, high)) =
            watermark_source.fetch_watermarks(element.topic(), element.partition(), API_TIMEOUT)
        else {
            continue;
        };
        saw_any = true;
        total += (high - position).max(0);
    }

    saw_any.then_some(total)
}

/// A group with committed offsets but nobody consuming is stopped, not slow.
fn group_health(group: &GroupFact, lag_warn: i64) -> Health {
    let lag = group.lag.unwrap_or(0);
    if group.state == "Dead" {
        return Health::Down;
    }
    if lag > 0 && group.members == 0 {
        return Health::Down;
    }
    if lag > lag_warn {
        return Health::Degraded;
    }
    // Rebalancing is transient but worth showing while it lasts.
    if group.state == "PreparingRebalance" || group.state == "CompletingRebalance" {
        return Health::Degraded;
    }
    Health::Healthy
}

fn topic_health(topic: &TopicFact) -> Health {
    if topic.offline > 0 {
        Health::Down
    } else if topic.under_replicated > 0 {
        Health::Degraded
    } else {
        Health::Healthy
    }
}

#[async_trait]
impl Collector for KafkaCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Kafka
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();
        let config = self.config.clone();

        let gathered = tokio::task::spawn_blocking(move || KafkaCollector::gather(config))
            .await
            .map_err(|e| AppError::Collector(format!("Kafka poll panicked: {e}")))?;

        let latency = elapsed_ms(start);

        let facts = match gathered {
            Ok(facts) => facts,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Kafka,
                    format!("{}: {message}", self.bootstrap),
                    latency,
                ));
            }
        };

        // -------------------------------------------------------- groups
        let mut group_rows: Vec<Row> = facts
            .groups
            .iter()
            .map(|group| {
                let health = group_health(group, self.lag_warn);
                Row::new(format!("group:{}", group.name), &group.name, health)
                    .summary(match group.lag {
                        Some(lag) if group.members == 0 && lag > 0 => {
                            format!("{lag} behind with no members")
                        }
                        Some(lag) => format!("{lag} behind · {} members", group.members),
                        None => format!("{} · {} members", group.state, group.members),
                    })
                    .cell("group", CellValue::text(&group.name))
                    .cell("state", CellValue::text(&group.state))
                    .cell("members", CellValue::number(group.members as f64))
                    .cell(
                        "lag",
                        match group.lag {
                            Some(lag) => CellValue::number(lag as f64),
                            None => CellValue::Null,
                        },
                    )
            })
            .collect();
        group_rows.sort_by(|a, b| b.health.cmp(&a.health));

        // -------------------------------------------------------- topics
        let mut topic_rows: Vec<Row> = facts
            .topics
            .iter()
            .map(|topic| {
                let health = topic_health(topic);
                Row::new(format!("topic:{}", topic.name), &topic.name, health)
                    .summary(if topic.offline > 0 {
                        format!("{} partitions offline", topic.offline)
                    } else if topic.under_replicated > 0 {
                        format!("{} partitions under-replicated", topic.under_replicated)
                    } else {
                        format!("{} partitions · {} messages", topic.partitions, topic.messages)
                    })
                    .cell("topic", CellValue::text(&topic.name))
                    .cell("partitions", CellValue::number(topic.partitions as f64))
                    .cell("offline", CellValue::number(topic.offline as f64))
                    .cell(
                        "under_replicated",
                        CellValue::number(topic.under_replicated as f64),
                    )
                    .cell("messages", CellValue::number(topic.messages as f64))
            })
            .collect();
        topic_rows.sort_by(|a, b| b.health.cmp(&a.health));
        topic_rows.truncate(MAX_TOPIC_ROWS);

        // ------------------------------------------------------- brokers
        let broker_rows: Vec<Row> = facts
            .brokers
            .iter()
            .map(|broker| {
                Row::new(
                    format!("broker:{}", broker.id),
                    format!("broker {}", broker.id),
                    Health::Healthy,
                )
                .summary(format!("{}:{}", broker.host, broker.port))
                .cell("id", CellValue::number(broker.id as f64))
                .cell("host", CellValue::text(&broker.host))
                .cell("port", CellValue::number(broker.port as f64))
            })
            .collect();

        // -------------------------------------------------------- rollup
        let offline: usize = facts.topics.iter().map(|t| t.offline).sum();
        let under_replicated: usize = facts.topics.iter().map(|t| t.under_replicated).sum();
        let stalled = facts
            .groups
            .iter()
            .filter(|g| g.members == 0 && g.lag.unwrap_or(0) > 0)
            .count();
        let total_lag: i64 = facts.groups.iter().filter_map(|g| g.lag).sum();

        let health = if facts.brokers.is_empty() {
            Health::Down
        } else if offline > 0 || stalled > 0 {
            Health::Down
        } else if under_replicated > 0 || total_lag > self.lag_warn {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if offline > 0 {
            format!("{offline} partitions offline")
        } else if stalled > 0 {
            format!(
                "{stalled} consumer group{} behind with no members",
                if stalled == 1 { "" } else { "s" }
            )
        } else if under_replicated > 0 {
            format!("{under_replicated} partitions under-replicated")
        } else if total_lag > self.lag_warn {
            format!("{total_lag} messages of consumer lag")
        } else {
            format!(
                "{} brokers · {} topics · {} groups",
                facts.brokers.len(),
                facts.topics.len(),
                facts.groups.len()
            )
        };

        let sampled_note = if facts.partitions_total > facts.partitions_sampled {
            format!(
                "Message counts cover {} of {} partitions.",
                facts.partitions_sampled, facts.partitions_total
            )
        } else {
            "No topics yet.".to_string()
        };

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Kafka,
            health,
            summary,
            metrics: vec![
                Metric::new("brokers", "Brokers", facts.brokers.len() as f64, MetricUnit::Count),
                Metric::new("topics", "Topics", facts.topics.len() as f64, MetricUnit::Count),
                Metric::new(
                    "partitions",
                    "Partitions",
                    facts.partitions_total as f64,
                    MetricUnit::Count,
                ),
                Metric::new("groups", "Consumer groups", facts.groups.len() as f64, MetricUnit::Count),
                Metric::new("lag", "Total consumer lag", total_lag as f64, MetricUnit::Count)
                    .warn_above(self.lag_warn as f64),
                Metric::new(
                    "offline_partitions",
                    "Offline partitions",
                    offline as f64,
                    MetricUnit::Count,
                )
                .warn_above(0.0),
                Metric::new(
                    "under_replicated",
                    "Under-replicated partitions",
                    under_replicated as f64,
                    MetricUnit::Count,
                )
                .warn_above(0.0),
            ],
            sections: vec![
                Section::new(
                    "groups",
                    "Consumer groups",
                    vec![
                        ColumnSpec::text("group", "Group").primary(),
                        ColumnSpec::text("state", "State"),
                        ColumnSpec::metric("members", "Members", MetricUnit::Count),
                        ColumnSpec::metric("lag", "Lag", MetricUnit::Count),
                    ],
                )
                .rows(group_rows)
                .priority(5)
                .tracked()
                .empty_text("No consumer groups."),
                Section::new(
                    "topics",
                    "Topics",
                    vec![
                        ColumnSpec::text("topic", "Topic").primary(),
                        ColumnSpec::metric("partitions", "Partitions", MetricUnit::Count),
                        ColumnSpec::metric("offline", "Offline", MetricUnit::Count),
                        ColumnSpec::metric("under_replicated", "Under-replicated", MetricUnit::Count),
                        ColumnSpec::metric("messages", "Messages", MetricUnit::Count),
                    ],
                )
                .rows(topic_rows)
                .priority(10)
                .empty_text(&sampled_note),
                Section::new(
                    "brokers",
                    "Brokers",
                    vec![
                        ColumnSpec::text("host", "Host").primary(),
                        ColumnSpec::metric("id", "ID", MetricUnit::Count),
                        ColumnSpec::metric("port", "Port", MetricUnit::Count),
                    ],
                )
                .rows(broker_rows)
                .priority(15)
                .tracked()
                .empty_text("No brokers reachable."),
            ],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// Kafka exposes no log stream to a client, so these are derived events:
    /// a partition going offline, or a group falling behind with nobody on it.
    async fn fetch_logs(&self, _since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let config = self.config.clone();
        let Ok(Ok(facts)) = tokio::task::spawn_blocking(move || KafkaCollector::gather(config)).await
        else {
            return Ok(Vec::new());
        };

        let mut reported = self.reported.lock().await;
        let mut out = Vec::new();

        for topic in &facts.topics {
            if topic.offline == 0 {
                continue;
            }
            let key = format!("offline:{}", topic.name);
            if reported.contains(&key) {
                continue;
            }
            reported.push(key);
            out.push(
                LogEvent::new(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Kafka,
                    &topic.name,
                    LogLevel::Error,
                    format!("{} partitions offline on {}", topic.offline, topic.name),
                    Utc::now(),
                )
                .stream(LogStream::Event),
            );
        }

        for group in &facts.groups {
            if group.members > 0 || group.lag.unwrap_or(0) == 0 {
                continue;
            }
            let key = format!("stalled:{}", group.name);
            if reported.contains(&key) {
                continue;
            }
            reported.push(key);
            out.push(
                LogEvent::new(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Kafka,
                    &group.name,
                    LogLevel::Error,
                    format!(
                        "consumer group {} is {} behind with no members",
                        group.name,
                        group.lag.unwrap_or(0)
                    ),
                    Utc::now(),
                )
                .stream(LogStream::Event),
            );
        }

        if reported.len() > 256 {
            let drop_to = reported.len() - 128;
            reported.drain(..drop_to);
        }
        if out.len() > limit {
            out.truncate(limit);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(state: &str, members: usize, lag: Option<i64>) -> GroupFact {
        GroupFact {
            name: "orders".to_string(),
            state: state.to_string(),
            members,
            lag,
        }
    }

    #[test]
    fn a_group_behind_with_no_members_is_down() {
        // Nobody is consuming. Depth of the backlog is not what makes this
        // serious — the same judgement as a RabbitMQ queue with no consumers.
        assert_eq!(group_health(&group("Empty", 0, Some(5)), 10_000), Health::Down);
    }

    #[test]
    fn an_idle_group_with_no_lag_is_healthy() {
        // An Empty group that is fully caught up is just a stopped batch job.
        assert_eq!(
            group_health(&group("Empty", 0, Some(0)), 10_000),
            Health::Healthy
        );
    }

    #[test]
    fn lag_over_the_threshold_is_degraded_while_members_are_present() {
        assert_eq!(
            group_health(&group("Stable", 3, Some(50_000)), 10_000),
            Health::Degraded
        );
    }

    #[test]
    fn lag_under_the_threshold_is_healthy() {
        assert_eq!(
            group_health(&group("Stable", 3, Some(12)), 10_000),
            Health::Healthy
        );
    }

    #[test]
    fn a_dead_group_is_down() {
        assert_eq!(group_health(&group("Dead", 0, None), 10_000), Health::Down);
    }

    #[test]
    fn rebalancing_shows_as_degraded() {
        assert_eq!(
            group_health(&group("PreparingRebalance", 2, Some(0)), 10_000),
            Health::Degraded
        );
    }

    #[test]
    fn unknown_lag_does_not_imply_a_problem() {
        // A group that has never committed anywhere returns no lag at all.
        // Treating that as zero-behind-with-no-members would flag every new
        // group as a stopped pipeline.
        assert_eq!(
            group_health(&group("Stable", 1, None), 10_000),
            Health::Healthy
        );
    }

    fn topic(offline: usize, under_replicated: usize) -> TopicFact {
        TopicFact {
            name: "events".to_string(),
            partitions: 6,
            offline,
            under_replicated,
            messages: 100,
        }
    }

    #[test]
    fn offline_partitions_are_down_and_under_replicated_are_degraded() {
        // Offline means no leader: unavailable for reads and writes.
        // Under-replicated is still serving, just without full redundancy.
        assert_eq!(topic_health(&topic(1, 0)), Health::Down);
        assert_eq!(topic_health(&topic(0, 2)), Health::Degraded);
        assert_eq!(topic_health(&topic(0, 0)), Health::Healthy);
    }
}
