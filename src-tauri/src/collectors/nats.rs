//! NATS monitor, via the HTTP monitoring port.
//!
//! `GET /varz` on the monitoring port (8222 by default) returns the server's
//! own view of itself as JSON. `/healthz` answers liveness, and `/routez`
//! describes the cluster mesh.
//!
//! The counter worth watching is `slow_consumers`, and it is the only evidence
//! that messages were lost.
//!
//! Core NATS is fire-and-forget. When a subscriber cannot keep up with its
//! subject, the server does not buffer indefinitely and it does not apply
//! back-pressure to publishers — it declares the client a slow consumer,
//! **drops its pending messages, and disconnects it**. The publisher sees
//! nothing. The subscriber reconnects and carries on, minus whatever it missed.
//! Nothing in the request path fails, no error is returned to anyone, and the
//! only trace left anywhere is this counter going up.
//!
//! It is cumulative since the server started, so the rate is what matters: a
//! server up for a month with a handful is a client that had a bad afternoon;
//! several a minute is a subscriber that is permanently behind and quietly
//! losing data.
//!
//! Route count is the other one. A clustered server that has lost its routes
//! is still perfectly healthy from its own side — it just cannot see the
//! subjects on the other half of a partitioned cluster.

use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct NatsCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    /// Slow-consumer events per second above which the server is degraded.
    slow_warn: f64,
    profile_id: String,
    profile_name: String,
    previous: Mutex<Option<Sample>>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    slow_consumers: f64,
    at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct Varz {
    #[serde(default)]
    pub server_id: String,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub connections: u64,
    #[serde(default)]
    pub total_connections: u64,
    #[serde(default)]
    pub routes: u64,
    #[serde(default)]
    pub remotes: u64,
    #[serde(default)]
    pub subscriptions: u64,
    #[serde(default)]
    pub slow_consumers: u64,
    #[serde(default)]
    pub in_msgs: i64,
    #[serde(default)]
    pub out_msgs: i64,
    #[serde(default)]
    pub in_bytes: i64,
    #[serde(default)]
    pub out_bytes: i64,
    #[serde(default)]
    pub mem: i64,
    #[serde(default)]
    pub cpu: f64,
    /// Zero means unlimited, which is how NATS reports "no limit set".
    #[serde(default)]
    pub max_connections: u64,
    #[serde(default)]
    pub cluster: Option<Cluster>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct Cluster {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub urls: Vec<String>,
}

impl Varz {
    /// Connections as a percentage of the configured ceiling.
    ///
    /// `None` when `max_connections` is zero, which NATS uses for unlimited —
    /// dividing by it would report either infinity or a comfortable nothing,
    /// and neither is true.
    pub fn connection_use(&self) -> Option<f64> {
        (self.max_connections > 0).then(|| {
            (self.connections as f64 / self.max_connections as f64) * 100.0
        })
    }

    /// Whether this server is configured as part of a cluster at all.
    pub fn is_clustered(&self) -> bool {
        self.cluster
            .as_ref()
            .is_some_and(|c| !c.name.is_empty() || !c.urls.is_empty())
    }
}

impl NatsCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim().trim_end_matches('/');
        if raw.is_empty() {
            return Err(AppError::Config(
                "NATS connections need the monitoring URL, e.g. http://nats.internal:8222".into(),
            ));
        }
        // The monitoring port, not the client port. Pasting a `nats://` client
        // URL here is the likely mistake, and it will not answer HTTP — so the
        // scheme is corrected rather than silently failing later.
        let base = raw
            .replace("nats://", "http://")
            .trim_end_matches("/varz")
            .to_string();

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        Ok(NatsCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            base,
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            slow_warn: profile
                .options
                .get("slowConsumerWarn")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.1),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            previous: Mutex::new(None),
        })
    }

    async fn varz(&self) -> std::result::Result<Varz, String> {
        let url = format!("{}/varz", self.base);
        let request = match (&self.username, &self.secret) {
            (Some(user), secret) => self.client.get(&url).basic_auth(user, secret.as_ref()),
            _ => self.client.get(&url),
        };
        let response = request.send().await.map_err(|e| format!("{url}: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "{url} returned {} — the monitoring port needs `http_port` or `https_port` set",
                response.status()
            ));
        }
        response
            .json::<Varz>()
            .await
            .map_err(|e| format!("{url} sent a body root could not read: {e}"))
    }
}

#[async_trait]
impl Collector for NatsCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Nats
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let varz = match self.varz().await {
            Ok(varz) => varz,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Nats,
                    message,
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let now = Utc::now();
        let slow_total = varz.slow_consumers as f64;
        let mut previous = self.previous.lock().await;
        let slow_rate = previous.and_then(|last| {
            let seconds = (now - last.at).num_milliseconds() as f64 / 1000.0;
            // Backwards means the server restarted, which resets the counter.
            if seconds <= 0.0 || slow_total < last.slow_consumers {
                return None;
            }
            Some((slow_total - last.slow_consumers) / seconds)
        });
        *previous = Some(Sample {
            slow_consumers: slow_total,
            at: now,
        });
        drop(previous);

        let dropping = slow_rate.is_some_and(|rate| rate >= self.slow_warn);
        // Clustered but with no routes is a partition. A single server with no
        // routes is just a single server, which is not a fault.
        let partitioned = varz.is_clustered() && varz.routes == 0;
        let connection_use = varz.connection_use();
        let near_limit = connection_use.is_some_and(|pct| pct >= 90.0);

        let health = if partitioned {
            Health::Down
        } else if dropping || near_limit {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if partitioned {
            "clustered, but connected to no other server — this half cannot see the other"
                .to_string()
        } else if dropping {
            format!(
                "dropping messages to slow consumers, {:.2}/sec — nothing else reports this loss",
                slow_rate.unwrap_or(0.0)
            )
        } else if near_limit {
            format!(
                "{:.0}% of the connection limit in use",
                connection_use.unwrap_or(0.0)
            )
        } else {
            format!(
                "{} connections · {} subscriptions{}",
                varz.connections,
                varz.subscriptions,
                if varz.routes > 0 {
                    format!(" · {} routes", varz.routes)
                } else {
                    String::new()
                }
            )
        };

        let mut metrics = vec![
            Metric::new("connections", "Connections", varz.connections as f64, MetricUnit::Count),
            Metric::new(
                "subscriptions",
                "Subscriptions",
                varz.subscriptions as f64,
                MetricUnit::Count,
            ),
            Metric::new("routes", "Cluster routes", varz.routes as f64, MetricUnit::Count),
            Metric::new(
                "slow_consumers",
                "Slow consumers",
                slow_total,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new("in_msgs", "Messages in", varz.in_msgs as f64, MetricUnit::Count),
            Metric::new("out_msgs", "Messages out", varz.out_msgs as f64, MetricUnit::Count),
            Metric::new("in_bytes", "Bytes in", varz.in_bytes as f64, MetricUnit::Bytes),
            Metric::new("out_bytes", "Bytes out", varz.out_bytes as f64, MetricUnit::Bytes),
            // Lifetime, not current — a server with 42 connections now and
            // nine thousand since boot is churning clients, which is worth
            // seeing next to the current count rather than instead of it.
            Metric::new(
                "total_connections",
                "Connections since start",
                varz.total_connections as f64,
                MetricUnit::Count,
            ),
            Metric::new("mem", "Memory", varz.mem as f64, MetricUnit::Bytes),
            Metric::new("cpu", "CPU", varz.cpu, MetricUnit::Percent),
        ];
        if let Some(rate) = slow_rate {
            metrics.push(
                Metric::new(
                    "slow_consumer_rate",
                    "Slow consumers/sec",
                    rate,
                    MetricUnit::PerSecond,
                )
                .warn_above(self.slow_warn),
            );
        }
        if let Some(pct) = connection_use {
            metrics.push(
                Metric::new("connection_use", "Connection limit used", pct, MetricUnit::Percent)
                    .warn_above(90.0),
            );
        }

        let mut sections = vec![Section::new(
            "server",
            "Server",
            vec![
                ColumnSpec::text("field", "Field").primary(),
                ColumnSpec::text("value", "Value"),
            ],
        )
        .rows(
            [
                ("Name", varz.server_name.clone()),
                ("Version", varz.version.clone()),
                ("Server ID", varz.server_id.clone()),
                (
                    "Cluster",
                    varz.cluster
                        .as_ref()
                        .map(|c| c.name.clone())
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| "not clustered".into()),
                ),
                (
                    "Connection limit",
                    if varz.max_connections == 0 {
                        "unlimited".to_string()
                    } else {
                        varz.max_connections.to_string()
                    },
                ),
            ]
            .into_iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(label, value)| {
                Row::new(label.to_lowercase().replace(' ', "_"), label, Health::Healthy)
                    .cell("field", CellValue::text(label))
                    .cell("value", CellValue::text(value))
            })
            .collect(),
        )
        .priority(20)];

        if varz.is_clustered() {
            sections.insert(
                0,
                Section::new(
                    "cluster",
                    "Cluster",
                    vec![
                        ColumnSpec::text("what", "Measure").primary(),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::metric("count", "Count", MetricUnit::Count),
                    ],
                )
                .rows(vec![
                    Row::new(
                        "routes",
                        "Routes to other servers",
                        if partitioned { Health::Down } else { Health::Healthy },
                    )
                    .summary(if partitioned {
                        "This server is configured for a cluster and connected to none of it"
                    } else {
                        "Connections to the rest of the cluster"
                    })
                    .cell("what", CellValue::text("Routes to other servers"))
                    .cell(
                        "state",
                        CellValue::status(if partitioned { Health::Down } else { Health::Healthy }),
                    )
                    .cell("count", CellValue::number(varz.routes as f64)),
                    Row::new("remotes", "Leaf node connections", Health::Healthy)
                        .cell("what", CellValue::text("Leaf node connections"))
                        .cell("state", CellValue::status(Health::Healthy))
                        .cell("count", CellValue::number(varz.remotes as f64)),
                ])
                .tracked()
                .priority(10),
            );
        }

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Nats,
            health,
            summary,
            metrics,
            sections,
            checked_at: now,
            latency_ms: latency,
            error: None,
        })
    }

    /// NATS logs to stdout or a file on the host. `/varz` is a gauge.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VARZ: &str = r#"{
        "server_id":"NAABC","server_name":"nats-1","version":"2.10.7",
        "connections":42,"total_connections":9001,"routes":2,"remotes":0,
        "subscriptions":88,"slow_consumers":17,
        "in_msgs":1000,"out_msgs":2000,"in_bytes":5000,"out_bytes":9000,
        "mem":104857600,"cpu":3.5,"max_connections":65536,
        "cluster":{"name":"prod","urls":["nats://nats-2:6222"]}
    }"#;

    #[test]
    fn parses_varz() {
        let varz: Varz = serde_json::from_str(VARZ).expect("varz");
        assert_eq!(varz.server_name, "nats-1");
        assert_eq!(varz.connections, 42);
        assert_eq!(varz.slow_consumers, 17);
        assert!(varz.is_clustered());
    }

    #[test]
    fn connection_use_is_a_percentage_of_the_limit() {
        let varz: Varz = serde_json::from_str(VARZ).expect("varz");
        let pct = varz.connection_use().expect("a limit is set");
        assert!((pct - 0.064).abs() < 0.001, "{pct}");
    }

    /// NATS reports "no limit" as zero, and a percentage of it is not a number.
    #[test]
    fn an_unlimited_server_has_no_connection_percentage() {
        let varz: Varz = serde_json::from_str(r#"{"connections":42,"max_connections":0}"#)
            .expect("varz");
        assert_eq!(varz.connection_use(), None);
    }

    /// A single server with no routes is a single server, not a partition.
    #[test]
    fn a_standalone_server_is_not_clustered() {
        let varz: Varz = serde_json::from_str(r#"{"connections":1,"routes":0}"#).expect("varz");
        assert!(!varz.is_clustered());
    }

    /// But a server configured for a cluster and connected to none of it is.
    #[test]
    fn a_clustered_server_with_no_routes_is_partitioned() {
        let varz: Varz =
            serde_json::from_str(r#"{"routes":0,"cluster":{"name":"prod","urls":[]}}"#)
                .expect("varz");
        assert!(varz.is_clustered());
        assert_eq!(varz.routes, 0);
    }

    /// A cluster block with only URLs and no name still means clustered.
    #[test]
    fn a_cluster_named_only_by_its_urls_still_counts() {
        let varz: Varz =
            serde_json::from_str(r#"{"cluster":{"urls":["nats://nats-2:6222"]}}"#).expect("varz");
        assert!(varz.is_clustered());
    }

    /// Fields NATS omits on older builds must not fail the whole parse.
    #[test]
    fn a_sparse_varz_still_parses() {
        let varz: Varz = serde_json::from_str(r#"{"server_name":"nats-1"}"#).expect("varz");
        assert_eq!(varz.server_name, "nats-1");
        assert_eq!(varz.connections, 0);
        assert_eq!(varz.slow_consumers, 0);
    }
}
