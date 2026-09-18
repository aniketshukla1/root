//! RabbitMQ monitor, via the management plugin's HTTP API.
//!
//! Three calls: `/api/overview` for cluster totals, `/api/nodes` for memory and
//! disk alarms, `/api/queues` for per-queue depth and consumers.
//!
//! The judgement that matters is which queues are actually in trouble. Depth
//! alone is a poor signal — a work queue sitting at 50,000 with twenty
//! consumers draining it is fine, while a queue at 200 with *zero* consumers is
//! a stuck pipeline. So a queue with no consumers and messages waiting is
//! treated as worse than a deep queue that is being served.

use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, Section, ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Queues listed. Sorted worst-first, so the tail is the least interesting.
const MAX_QUEUE_ROWS: usize = 80;

pub struct RabbitCollector {
    client: reqwest::Client,
    base: String,
    username: String,
    secret: Option<String>,
    queue_depth_warn: f64,
    vhost: Option<String>,
    profile_id: String,
    profile_name: String,
    /// Node alarms already logged, so a standing alarm is reported once.
    reported_alarms: Mutex<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct Overview {
    #[serde(default)]
    rabbitmq_version: String,
    #[serde(default)]
    cluster_name: String,
    #[serde(default)]
    object_totals: ObjectTotals,
    #[serde(default)]
    queue_totals: QueueTotals,
}

#[derive(Debug, Deserialize, Default)]
struct ObjectTotals {
    #[serde(default)]
    connections: f64,
    #[serde(default)]
    channels: f64,
    #[serde(default)]
    queues: f64,
    #[serde(default)]
    consumers: f64,
}

#[derive(Debug, Deserialize, Default)]
struct QueueTotals {
    #[serde(default)]
    messages: f64,
    #[serde(default)]
    messages_ready: f64,
    #[serde(default)]
    messages_unacknowledged: f64,
}

#[derive(Debug, Deserialize)]
struct Node {
    #[serde(default)]
    name: String,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    mem_used: f64,
    #[serde(default)]
    mem_limit: f64,
    #[serde(default)]
    mem_alarm: bool,
    #[serde(default)]
    disk_free_alarm: bool,
    #[serde(default)]
    disk_free: f64,
    #[serde(default)]
    fd_used: f64,
    #[serde(default)]
    fd_total: f64,
}

#[derive(Debug, Deserialize)]
struct Queue {
    #[serde(default)]
    name: String,
    #[serde(default)]
    vhost: String,
    #[serde(default)]
    state: String,
    // No `messages` here: per queue it is just ready + unacknowledged, and both
    // of those are shown as their own columns.
    #[serde(default)]
    messages_ready: f64,
    #[serde(default)]
    messages_unacknowledged: f64,
    #[serde(default)]
    consumers: f64,
}

impl RabbitCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let base = profile.endpoint.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err(AppError::Config(
                "RabbitMQ connections need the management URL, e.g. http://rabbit.internal:15672"
                    .to_string(),
            ));
        }
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(AppError::Config(format!(
                "`{base}` must start with http:// or https:// — this is the management \
                 plugin's HTTP port, not the AMQP port"
            )));
        }

        Ok(RabbitCollector {
            client: http_client(profile.options.insecure_tls(), REQUEST_TIMEOUT)?,
            base,
            username: profile
                .username
                .clone()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| "guest".to_string()),
            secret,
            queue_depth_warn: profile.options.u64("queueDepthWarn").unwrap_or(1000) as f64,
            vhost: profile.options.string("vhost").filter(|v| !v.is_empty()),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            reported_alarms: Mutex::new(Vec::new()),
        })
    }

    async fn api<T: serde::de::DeserializeOwned>(&self, path: &str) -> Option<T> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .basic_auth(&self.username, self.secret.as_ref())
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json().await.ok()
    }
}

/// How worried to be about one queue.
///
/// Consumer count dominates depth: messages with nobody to read them are a
/// stalled pipeline regardless of how few there are, while a deep queue with
/// consumers is usually just busy.
fn queue_health(queue: &Queue, depth_warn: f64) -> Health {
    if queue.state == "flow" {
        // Publishers are being throttled — the broker is applying back pressure.
        return Health::Degraded;
    }
    if queue.messages_ready > 0.0 && queue.consumers == 0.0 {
        return Health::Down;
    }
    if queue.messages_ready >= depth_warn {
        return Health::Degraded;
    }
    Health::Healthy
}

fn node_health(node: &Node) -> Health {
    if !node.running {
        Health::Down
    } else if node.mem_alarm || node.disk_free_alarm {
        Health::Down
    } else {
        Health::Healthy
    }
}

#[async_trait]
impl Collector for RabbitCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Rabbitmq
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let Some(overview) = self.api::<Overview>("/api/overview").await else {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Rabbitmq,
                format!(
                    "{} did not answer /api/overview — is the management plugin enabled?",
                    self.base
                ),
                elapsed_ms(start),
            ));
        };
        let latency = elapsed_ms(start);

        let nodes = self.api::<Vec<Node>>("/api/nodes").await.unwrap_or_default();
        let queue_path = match &self.vhost {
            Some(vhost) => format!("/api/queues/{}", urlencoding(vhost)),
            None => "/api/queues".to_string(),
        };
        let queues = self.api::<Vec<Queue>>(&queue_path).await.unwrap_or_default();

        // ---------------------------------------------------------- nodes
        let mut node_rows = Vec::new();
        let mut alarms = Vec::new();
        for node in &nodes {
            let health = node_health(node);
            if node.mem_alarm {
                alarms.push(format!("{}: memory alarm", node.name));
            }
            if node.disk_free_alarm {
                alarms.push(format!("{}: disk alarm", node.name));
            }
            let mem_percent = (node.mem_limit > 0.0)
                .then(|| node.mem_used / node.mem_limit * 100.0);

            node_rows.push(
                Row::new(format!("node:{}", node.name), &node.name, health)
                    .summary(if node.running {
                        match mem_percent {
                            Some(p) => format!("running · memory {p:.0}%"),
                            None => "running".to_string(),
                        }
                    } else {
                        "not running".to_string()
                    })
                    .cell("name", CellValue::text(&node.name))
                    .cell(
                        "state",
                        CellValue::text(if node.running { "running" } else { "stopped" }),
                    )
                    .cell(
                        "memory",
                        match mem_percent {
                            Some(p) => CellValue::gauge_warn(p, Some(100.0), 80.0),
                            None => CellValue::number(node.mem_used),
                        },
                    )
                    .cell("disk_free", CellValue::number(node.disk_free))
                    .cell(
                        "fds",
                        CellValue::text(format!("{:.0}/{:.0}", node.fd_used, node.fd_total)),
                    )
                    .cell(
                        "alarms",
                        CellValue::tags(
                            [
                                node.mem_alarm.then(|| "memory".to_string()),
                                node.disk_free_alarm.then(|| "disk".to_string()),
                            ]
                            .into_iter()
                            .flatten()
                            .collect(),
                        ),
                    ),
            );
        }

        // --------------------------------------------------------- queues
        let mut queue_rows: Vec<Row> = queues
            .iter()
            .map(|queue| {
                let health = queue_health(queue, self.queue_depth_warn);
                Row::new(
                    format!("queue:{}/{}", queue.vhost, queue.name),
                    &queue.name,
                    health,
                )
                .summary(if queue.consumers == 0.0 && queue.messages_ready > 0.0 {
                    format!("{:.0} messages with no consumers", queue.messages_ready)
                } else {
                    format!(
                        "{:.0} ready · {:.0} consumer{}",
                        queue.messages_ready,
                        queue.consumers,
                        if queue.consumers == 1.0 { "" } else { "s" }
                    )
                })
                .cell("name", CellValue::text(&queue.name))
                .cell("vhost", CellValue::text(&queue.vhost))
                .cell("ready", CellValue::number(queue.messages_ready))
                .cell("unacked", CellValue::number(queue.messages_unacknowledged))
                .cell("consumers", CellValue::number(queue.consumers))
                .cell("state", CellValue::text(&queue.state))
            })
            .collect();

        // Worst first, then deepest — so the truncated tail is the dull part.
        queue_rows.sort_by(|a, b| {
            b.health
                .cmp(&a.health)
                .then_with(|| {
                    let depth = |row: &Row| match row.cells.get("ready") {
                        Some(crate::model::CellValue::Number { value }) => *value,
                        _ => 0.0,
                    };
                    depth(b).partial_cmp(&depth(a)).unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let truncated = queue_rows.len().saturating_sub(MAX_QUEUE_ROWS);
        queue_rows.truncate(MAX_QUEUE_ROWS);

        // ---------------------------------------------------------- rollup
        let stuck = queues
            .iter()
            .filter(|q| q.messages_ready > 0.0 && q.consumers == 0.0)
            .count();
        let deep = queues
            .iter()
            .filter(|q| q.messages_ready >= self.queue_depth_warn)
            .count();
        let nodes_down = nodes.iter().filter(|n| node_health(n) == Health::Down).count();

        let health = if nodes_down > 0 || !alarms.is_empty() {
            Health::Down
        } else if stuck > 0 {
            Health::Down
        } else if deep > 0 {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if !alarms.is_empty() {
            alarms.join(" · ")
        } else if nodes_down > 0 {
            format!("{nodes_down} node{} down", if nodes_down == 1 { "" } else { "s" })
        } else if stuck > 0 {
            format!(
                "{stuck} queue{} with messages and no consumers",
                if stuck == 1 { "" } else { "s" }
            )
        } else if deep > 0 {
            format!("{deep} queue{} over the depth threshold", if deep == 1 { "" } else { "s" })
        } else {
            format!(
                "{:.0} queues · {:.0} messages · {:.0} consumers",
                overview.object_totals.queues,
                overview.queue_totals.messages,
                overview.object_totals.consumers
            )
        };

        let mut queue_section = Section::new(
            "queues",
            "Queues",
            vec![
                ColumnSpec::text("name", "Queue").primary(),
                ColumnSpec::text("vhost", "Vhost"),
                ColumnSpec::metric("ready", "Ready", MetricUnit::Count),
                ColumnSpec::metric("unacked", "Unacked", MetricUnit::Count),
                ColumnSpec::metric("consumers", "Consumers", MetricUnit::Count),
                ColumnSpec::text("state", "State"),
            ],
        )
        .rows(queue_rows)
        .priority(5)
        .tracked()
        .empty_text("No queues declared.");
        if truncated > 0 {
            queue_section = queue_section
                .empty_text(&format!("{truncated} more queues not shown."));
        }

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Rabbitmq,
            health,
            summary,
            metrics: vec![
                Metric::new("messages", "Messages", overview.queue_totals.messages, MetricUnit::Count),
                Metric::new(
                    "messages_ready",
                    "Ready",
                    overview.queue_totals.messages_ready,
                    MetricUnit::Count,
                ),
                Metric::new(
                    "messages_unacked",
                    "Unacknowledged",
                    overview.queue_totals.messages_unacknowledged,
                    MetricUnit::Count,
                ),
                Metric::new("queues", "Queues", overview.object_totals.queues, MetricUnit::Count),
                Metric::new(
                    "consumers",
                    "Consumers",
                    overview.object_totals.consumers,
                    MetricUnit::Count,
                ),
                Metric::new(
                    "connections",
                    "Connections",
                    overview.object_totals.connections,
                    MetricUnit::Count,
                ),
                Metric::new("channels", "Channels", overview.object_totals.channels, MetricUnit::Count),
                Metric::new("queues_stuck", "Queues without consumers", stuck as f64, MetricUnit::Count)
                    .warn_above(0.0),
            ],
            sections: vec![
                queue_section,
                Section::new(
                    "nodes",
                    "Nodes",
                    vec![
                        ColumnSpec::text("name", "Node").primary(),
                        ColumnSpec::text("state", "State"),
                        ColumnSpec::gauge("memory", "Memory", MetricUnit::Percent),
                        ColumnSpec::metric("disk_free", "Disk free", MetricUnit::Bytes),
                        ColumnSpec::text("fds", "File descriptors"),
                        ColumnSpec::tags("alarms", "Alarms"),
                    ],
                )
                .rows(node_rows)
                .priority(10)
                .sorted_by_health()
                .tracked()
                .empty_text("No nodes reported."),
                Section::new(
                    "broker",
                    "Broker",
                    vec![
                        ColumnSpec::text("cluster", "Cluster").primary(),
                        ColumnSpec::text("version", "Version"),
                        ColumnSpec::metric("connections", "Connections", MetricUnit::Count),
                        ColumnSpec::metric("channels", "Channels", MetricUnit::Count),
                        ColumnSpec::metric("consumers", "Consumers", MetricUnit::Count),
                    ],
                )
                .rows(vec![Row::new(
                    "broker",
                    if overview.cluster_name.is_empty() {
                        self.base.clone()
                    } else {
                        overview.cluster_name.clone()
                    },
                    Health::Healthy,
                )
                .summary(format!("RabbitMQ {}", overview.rabbitmq_version))
                .cell("cluster", CellValue::text(&overview.cluster_name))
                .cell("version", CellValue::text(&overview.rabbitmq_version))
                .cell(
                    "connections",
                    CellValue::number(overview.object_totals.connections),
                )
                .cell("channels", CellValue::number(overview.object_totals.channels))
                .cell(
                    "consumers",
                    CellValue::number(overview.object_totals.consumers),
                )])
                .priority(20),
            ],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    async fn fetch_logs(&self, _since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let nodes = self.api::<Vec<Node>>("/api/nodes").await.unwrap_or_default();
        let mut reported = self.reported_alarms.lock().await;
        let mut out = Vec::new();

        for node in &nodes {
            for (flag, label) in [(node.mem_alarm, "memory"), (node.disk_free_alarm, "disk")] {
                if !flag {
                    continue;
                }
                let key = format!("{}:{label}", node.name);
                if reported.contains(&key) {
                    continue;
                }
                reported.push(key);
                out.push(
                    LogEvent::new(
                        &self.profile_id,
                        &self.profile_name,
                        ServiceKind::Rabbitmq,
                        &node.name,
                        LogLevel::Error,
                        format!("{label} alarm raised on {}", node.name),
                        Utc::now(),
                    )
                    .stream(LogStream::Event),
                );
            }
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

/// Percent-encode a vhost for use in a path segment.
///
/// The default vhost is `/`, which must become `%2F` or the URL points at the
/// wrong resource entirely — the single most common mistake against this API.
fn urlencoding(value: &str) -> String {
    super::path_segment(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(ready: f64, consumers: f64, state: &str) -> Queue {
        Queue {
            name: "work".to_string(),
            vhost: "/".to_string(),
            state: state.to_string(),
            messages_ready: ready,
            messages_unacknowledged: 0.0,
            consumers,
        }
    }

    #[test]
    fn a_queue_with_no_consumers_is_down_however_shallow() {
        // Two messages and nobody reading is a stalled pipeline; depth is not
        // what makes it serious.
        assert_eq!(queue_health(&queue(2.0, 0.0, "running"), 1000.0), Health::Down);
    }

    #[test]
    fn a_deep_queue_with_consumers_is_only_degraded() {
        assert_eq!(
            queue_health(&queue(50_000.0, 20.0, "running"), 1000.0),
            Health::Degraded
        );
    }

    #[test]
    fn an_empty_queue_with_no_consumers_is_fine() {
        // An idle queue nobody is listening to is normal, not a fault.
        assert_eq!(queue_health(&queue(0.0, 0.0, "running"), 1000.0), Health::Healthy);
    }

    #[test]
    fn a_busy_queue_under_the_threshold_is_healthy() {
        assert_eq!(queue_health(&queue(10.0, 3.0, "running"), 1000.0), Health::Healthy);
    }

    #[test]
    fn flow_state_means_back_pressure() {
        assert_eq!(queue_health(&queue(5.0, 2.0, "flow"), 1000.0), Health::Degraded);
    }

    fn node(running: bool, mem: bool, disk: bool) -> Node {
        Node {
            name: "rabbit@host".to_string(),
            running,
            mem_used: 100.0,
            mem_limit: 1000.0,
            mem_alarm: mem,
            disk_free_alarm: disk,
            disk_free: 5_000_000.0,
            fd_used: 40.0,
            fd_total: 1024.0,
        }
    }

    #[test]
    fn node_alarms_are_down_not_degraded() {
        // A memory alarm means RabbitMQ has stopped accepting publishes. That
        // is an outage, not a warning.
        assert_eq!(node_health(&node(true, true, false)), Health::Down);
        assert_eq!(node_health(&node(true, false, true)), Health::Down);
        assert_eq!(node_health(&node(false, false, false)), Health::Down);
        assert_eq!(node_health(&node(true, false, false)), Health::Healthy);
    }

    #[test]
    fn the_default_vhost_is_percent_encoded() {
        // `/` unencoded would address a completely different API path.
        assert_eq!(urlencoding("/"), "%2F");
        assert_eq!(urlencoding("prod"), "prod");
        assert_eq!(urlencoding("a/b"), "a%2Fb");
    }

    #[test]
    fn parses_the_overview_payload() {
        let raw = r#"{"rabbitmq_version":"3.13.0","cluster_name":"rabbit@node",
          "object_totals":{"connections":2,"channels":4,"queues":7,"consumers":3},
          "queue_totals":{"messages":12,"messages_ready":10,"messages_unacknowledged":2}}"#;
        let overview: Overview = serde_json::from_str(raw).expect("parse");
        assert_eq!(overview.object_totals.queues, 7.0);
        assert_eq!(overview.queue_totals.messages_ready, 10.0);
    }

    #[test]
    fn overview_tolerates_missing_totals() {
        // Older brokers and restricted users omit whole objects; defaults keep
        // the snapshot renderable instead of failing the poll.
        let overview: Overview = serde_json::from_str("{}").expect("parse");
        assert_eq!(overview.object_totals.queues, 0.0);
        assert_eq!(overview.rabbitmq_version, "");
    }
}
