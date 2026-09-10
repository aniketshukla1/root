//! etcd monitor, via `/metrics` and `/health`.
//!
//! `/health` answers `{"health":"true"}` and is the check everyone wires up.
//! It is also the one that will not warn you about the outage etcd is most
//! famous for.
//!
//! etcd enforces a backend quota — 2 GiB by default, and Kubernetes clusters
//! routinely run without ever raising it. When the mvcc database reaches that
//! quota, etcd raises a `NOSPACE` alarm and goes **read-only**. Reads keep
//! answering, `/health` keeps saying true for a while, and every write in the
//! cluster fails: no pod scheduling, no deployments, no leader elections in
//! anything that depends on it. The fix — compact and defragment — takes
//! minutes; noticing takes most people much longer, because nothing was
//! watching the one number that saw it coming.
//!
//! So the headline here is database size against quota, and it is a warning
//! long before it is a failure. `etcd_server_has_leader` and the leader-change
//! counter cover the other way an etcd cluster stops serving: raft churn, which
//! usually means the disk is too slow to keep up with fsync.
//!
//! `/metrics` is the Prometheus text format, parsed here rather than pulled in
//! as a dependency — the subset etcd emits is plain `name{labels} value` lines.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct EtcdCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    /// Percent of the backend quota in use above which etcd is degraded.
    quota_warn: f64,
    profile_id: String,
    profile_name: String,
}

/// One sample from the Prometheus text exposition format.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sample {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

/// Parse the Prometheus text format.
///
/// Only what etcd actually emits: `# HELP` and `# TYPE` comments, then
/// `name value` or `name{k="v",k2="v2"} value`. Histogram buckets come through
/// as ordinary samples with a `le` label, which is correct — nothing here
/// needs to reassemble them.
pub(crate) fn parse_prometheus(body: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // The value is the last whitespace-separated token; a label value may
        // contain spaces, so the split has to come from the right.
        let Some((head, value)) = line.rsplit_once(char::is_whitespace) else {
            continue;
        };
        // NaN, +Inf and -Inf are all legal in this format — and Rust parses
        // every one of them happily, so rejecting them takes an explicit
        // check. Letting one through means a quota percentage of NaN or a
        // byte count of infinity reaching the screen as if it were measured.
        let Ok(value) = value.trim().parse::<f64>() else {
            continue;
        };
        if !value.is_finite() {
            continue;
        }

        let head = head.trim();
        let (name, labels) = match head.split_once('{') {
            Some((name, rest)) => (name, parse_labels(rest.trim_end_matches('}'))),
            None => (head, BTreeMap::new()),
        };

        out.push(Sample {
            name: name.trim().to_string(),
            labels,
            value,
        });
    }
    out
}

fn parse_labels(body: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    for pair in body.split(',') {
        if let Some((key, value)) = pair.split_once('=') {
            labels.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    labels
}

/// First sample with this name, ignoring labels.
pub(crate) fn gauge(samples: &[Sample], name: &str) -> Option<f64> {
    samples.iter().find(|s| s.name == name).map(|s| s.value)
}

/// Every sample with this name added together, for counters split by label.
pub(crate) fn total(samples: &[Sample], name: &str) -> Option<f64> {
    let matching: Vec<f64> = samples
        .iter()
        .filter(|s| s.name == name)
        .map(|s| s.value)
        .collect();
    (!matching.is_empty()).then(|| matching.iter().sum())
}

impl EtcdCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim().trim_end_matches('/');
        if raw.is_empty() {
            return Err(AppError::Config(
                "etcd connections need a client URL, e.g. http://etcd-1.internal:2379".into(),
            ));
        }
        let base = raw.trim_end_matches("/metrics").to_string();

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        Ok(EtcdCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            base,
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            quota_warn: profile
                .options
                .get("quotaWarn")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(80.0),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }

    async fn text(&self, path: &str) -> std::result::Result<String, String> {
        let url = format!("{}{path}", self.base);
        let request = match (&self.username, &self.secret) {
            (Some(user), secret) => self.client.get(&url).basic_auth(user, secret.as_ref()),
            _ => self.client.get(&url),
        };
        let response = request.send().await.map_err(|e| format!("{url}: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("{url} returned {}", response.status()));
        }
        response
            .text()
            .await
            .map_err(|e| format!("{url} sent a body root could not read: {e}"))
    }
}

#[async_trait]
impl Collector for EtcdCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Etcd
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let body = match self.text("/metrics").await {
            Ok(body) => body,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Etcd,
                    message,
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let samples = parse_prometheus(&body);
        if gauge(&samples, "etcd_server_has_leader").is_none() {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Etcd,
                format!(
                    "{}/metrics answered, but with no etcd metrics — check the URL points at \
                     an etcd client port",
                    self.base
                ),
                latency,
            ));
        }

        let has_leader = gauge(&samples, "etcd_server_has_leader").unwrap_or(0.0) > 0.0;
        let is_leader = gauge(&samples, "etcd_server_is_leader").unwrap_or(0.0) > 0.0;
        let leader_changes = total(&samples, "etcd_server_leader_changes_seen_total").unwrap_or(0.0);
        let proposals_failed = total(&samples, "etcd_server_proposals_failed_total").unwrap_or(0.0);
        let proposals_pending = gauge(&samples, "etcd_server_proposals_pending").unwrap_or(0.0);

        let db_size = gauge(&samples, "etcd_mvcc_db_total_size_in_bytes");
        let db_in_use = gauge(&samples, "etcd_mvcc_db_total_size_in_use_in_bytes");
        let quota = gauge(&samples, "etcd_server_quota_backend_bytes").filter(|q| *q > 0.0);
        let quota_use = match (db_size, quota) {
            (Some(size), Some(quota)) => Some((size / quota) * 100.0),
            _ => None,
        };

        let over_quota = quota_use.is_some_and(|pct| pct >= 100.0);
        let near_quota = quota_use.is_some_and(|pct| pct >= self.quota_warn);

        let health = if !has_leader || over_quota {
            Health::Down
        } else if near_quota || proposals_pending > 0.0 {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if !has_leader {
            "no raft leader — every write is failing".to_string()
        } else if over_quota {
            "backend quota reached — etcd is read-only until it is compacted and defragmented"
                .to_string()
        } else if near_quota {
            format!(
                "{:.0}% of the backend quota used — read-only at 100%",
                quota_use.unwrap_or(0.0)
            )
        } else if proposals_pending > 0.0 {
            format!("{proposals_pending:.0} raft proposals pending")
        } else {
            format!(
                "{} · {} leader change{} since start",
                if is_leader { "leader" } else { "follower" },
                leader_changes as u64,
                if leader_changes == 1.0 { "" } else { "s" }
            )
        };

        let mut metrics = vec![
            Metric::new(
                "has_leader",
                "Has a leader",
                if has_leader { 1.0 } else { 0.0 },
                MetricUnit::Count,
            ),
            Metric::new(
                "leader_changes",
                "Leader changes",
                leader_changes,
                MetricUnit::Count,
            ),
            Metric::new(
                "proposals_pending",
                "Proposals pending",
                proposals_pending,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new(
                "proposals_failed",
                "Proposals failed",
                proposals_failed,
                MetricUnit::Count,
            )
            .warn_above(0.0),
        ];

        if let Some(pct) = quota_use {
            metrics.push(
                Metric::new("quota_use", "Backend quota used", pct, MetricUnit::Percent)
                    .warn_above(self.quota_warn),
            );
        }
        if let Some(size) = db_size {
            let mut metric = Metric::new("db_size", "Database size", size, MetricUnit::Bytes);
            if let Some(quota) = quota {
                metric = metric.warn_above(quota * (self.quota_warn / 100.0));
            }
            metrics.push(metric);
        }
        if let Some(in_use) = db_in_use {
            metrics.push(Metric::new(
                "db_in_use",
                "Database in use",
                in_use,
                MetricUnit::Bytes,
            ));
        }

        let mut sections = Vec::new();
        if let (Some(size), Some(quota)) = (db_size, quota) {
            // Size versus in-use is the compact/defragment question: a large
            // gap means the space is already reclaimable and a defrag would
            // return it, which is a different action from raising the quota.
            let reclaimable = db_in_use.map(|in_use| (size - in_use).max(0.0));
            sections.push(
                Section::new(
                    "backend",
                    "Backend",
                    vec![
                        ColumnSpec::text("what", "Measure").primary(),
                        ColumnSpec::gauge("bytes", "Bytes", MetricUnit::Bytes),
                    ],
                )
                .rows(
                    [
                        Some((
                            "allocated",
                            "Database size",
                            size,
                            if over_quota {
                                Health::Down
                            } else if near_quota {
                                Health::Degraded
                            } else {
                                Health::Healthy
                            },
                            "What the backend file occupies, and what the quota measures",
                        )),
                        db_in_use.map(|in_use| {
                            (
                                "in_use",
                                "In use",
                                in_use,
                                Health::Healthy,
                                "Live keys — the rest is reclaimable by a defragment",
                            )
                        }),
                        reclaimable.map(|free| {
                            (
                                "reclaimable",
                                "Reclaimable",
                                free,
                                Health::Healthy,
                                "Freed by a defragment without touching any data",
                            )
                        }),
                    ]
                    .into_iter()
                    .flatten()
                    .map(|(id, label, value, state, note)| {
                        Row::new(id, label, state)
                            .summary(note)
                            .cell("what", CellValue::text(label))
                            .cell("bytes", CellValue::gauge(value, Some(quota)))
                    })
                    .collect(),
                )
                .tracked()
                .priority(10),
            );
        }

        sections.push(
            Section::new(
                "raft",
                "Raft",
                vec![
                    ColumnSpec::text("what", "Measure").primary(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::metric("value", "Value", MetricUnit::Count),
                ],
            )
            .rows(vec![
                Row::new(
                    "leader",
                    "Leader",
                    if has_leader { Health::Healthy } else { Health::Down },
                )
                .summary(if has_leader {
                    "This member can see a leader, so writes can be proposed"
                } else {
                    "No leader visible — every write fails until an election completes"
                })
                .cell("what", CellValue::text("Leader visible"))
                .cell(
                    "state",
                    CellValue::status(if has_leader { Health::Healthy } else { Health::Down }),
                )
                .cell("value", CellValue::number(if has_leader { 1.0 } else { 0.0 })),
                Row::new("changes", "Leader changes", Health::Healthy)
                    .summary("Since this member started — churn usually means slow disks")
                    .cell("what", CellValue::text("Leader changes"))
                    .cell("state", CellValue::status(Health::Healthy))
                    .cell("value", CellValue::number(leader_changes)),
                Row::new(
                    "pending",
                    "Proposals pending",
                    if proposals_pending > 0.0 { Health::Degraded } else { Health::Healthy },
                )
                .summary("Writes accepted but not yet committed")
                .cell("what", CellValue::text("Proposals pending"))
                .cell(
                    "state",
                    CellValue::status(if proposals_pending > 0.0 {
                        Health::Degraded
                    } else {
                        Health::Healthy
                    }),
                )
                .cell("value", CellValue::number(proposals_pending)),
            ])
            .tracked()
            .priority(20),
        );

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Etcd,
            health,
            summary,
            metrics,
            sections,
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// etcd logs to the host's journal. `/metrics` is a gauge.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# HELP etcd_server_has_leader Whether or not a leader exists.\n\
                          # TYPE etcd_server_has_leader gauge\n\
                          etcd_server_has_leader 1\n\
                          etcd_server_is_leader 0\n\
                          etcd_server_leader_changes_seen_total 3\n\
                          etcd_server_proposals_pending 0\n\
                          etcd_mvcc_db_total_size_in_bytes 1.717986918e+09\n\
                          etcd_mvcc_db_total_size_in_use_in_bytes 4.2949673e+08\n\
                          etcd_server_quota_backend_bytes 2.147483648e+09\n\
                          etcd_network_peer_sent_bytes_total{To=\"8211f1d0f64f3269\"} 1.234e+06\n\
                          etcd_network_peer_sent_bytes_total{To=\"91bc3c398fb3c146\"} 2.0e+06\n";

    #[test]
    fn parses_plain_and_labelled_samples() {
        let samples = parse_prometheus(SAMPLE);
        assert_eq!(gauge(&samples, "etcd_server_has_leader"), Some(1.0));
        assert_eq!(
            samples
                .iter()
                .find(|s| s.name == "etcd_network_peer_sent_bytes_total")
                .and_then(|s| s.labels.get("To").cloned()),
            Some("8211f1d0f64f3269".to_string())
        );
    }

    #[test]
    fn comments_are_not_samples() {
        let samples = parse_prometheus("# HELP a thing\n# TYPE a gauge\na 1\n");
        assert_eq!(samples.len(), 1);
    }

    #[test]
    fn a_counter_split_by_label_is_summed() {
        let samples = parse_prometheus(SAMPLE);
        assert_eq!(
            total(&samples, "etcd_network_peer_sent_bytes_total"),
            Some(3_234_000.0)
        );
    }

    #[test]
    fn scientific_notation_parses() {
        let samples = parse_prometheus(SAMPLE);
        assert_eq!(gauge(&samples, "etcd_mvcc_db_total_size_in_bytes"), Some(1_717_986_918.0));
    }

    /// NaN and +Inf are legal in this format and are not numbers this
    /// collector can act on.
    #[test]
    fn non_numeric_values_are_skipped_rather_than_defaulted() {
        let samples = parse_prometheus("a NaN\nb +Inf\nc 5\n");
        assert_eq!(samples.len(), 1);
        assert_eq!(gauge(&samples, "c"), Some(5.0));
    }

    /// The outage the module exists for: /health still says true here.
    #[test]
    fn database_size_is_measured_against_the_quota() {
        let samples = parse_prometheus(SAMPLE);
        let size = gauge(&samples, "etcd_mvcc_db_total_size_in_bytes").unwrap();
        let quota = gauge(&samples, "etcd_server_quota_backend_bytes").unwrap();
        let pct = (size / quota) * 100.0;
        assert!((pct - 80.0).abs() < 0.01, "{pct}");
    }

    /// The gap between allocated and in-use is what a defragment returns, and
    /// it is a different action from raising the quota.
    #[test]
    fn the_reclaimable_gap_is_visible() {
        let samples = parse_prometheus(SAMPLE);
        let size = gauge(&samples, "etcd_mvcc_db_total_size_in_bytes").unwrap();
        let in_use = gauge(&samples, "etcd_mvcc_db_total_size_in_use_in_bytes").unwrap();
        assert!(size - in_use > 1_000_000_000.0);
    }

    #[test]
    fn a_missing_metric_is_absent_rather_than_zero() {
        let samples = parse_prometheus("a 1\n");
        assert_eq!(gauge(&samples, "etcd_server_has_leader"), None);
        assert_eq!(total(&samples, "nothing_here"), None);
    }

    #[test]
    fn a_non_etcd_body_yields_no_leader_metric() {
        let samples = parse_prometheus("<html><body>404</body></html>");
        assert_eq!(gauge(&samples, "etcd_server_has_leader"), None);
    }
}
