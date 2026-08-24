//! MongoDB monitor.
//!
//! Four admin commands, all read-only and all cheap: `serverStatus` for
//! connections and throughput, `replSetGetStatus` for replica health and lag,
//! `currentOp` for operations that have been running too long, and
//! `listDatabases` for storage.
//!
//! Two of those fail by design on a perfectly healthy server —
//! `replSetGetStatus` errors on a standalone, and `currentOp` needs a
//! privileged user. Treating either as a collection failure would report a
//! working database as down, so both degrade to an empty section instead.

use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mongodb::bson::{doc, Document};
use mongodb::options::ClientOptions;
use mongodb::Client;
use tokio::sync::Mutex;

use super::{elapsed_ms, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, Section, ServiceKind,
};

/// Connections in use above this share of the limit is worth flagging.
const CONNECTION_WARN_PERCENT: f64 = 85.0;
/// Replica lag past this many seconds counts as degraded.
const LAG_WARN_SECS: f64 = 10.0;

pub struct MongoCollector {
    client: Client,
    slow_op_millis: f64,
    endpoint: String,
    profile_id: String,
    profile_name: String,
    /// Operation ids already logged, so a query running for ten minutes is
    /// reported once rather than on every poll.
    reported_ops: Mutex<Vec<String>>,
}

impl MongoCollector {
    pub async fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim();
        if raw.is_empty() {
            return Err(AppError::Config(
                "MongoDB connections need a connection string, e.g. mongodb://host:27017"
                    .to_string(),
            ));
        }

        let mut options = ClientOptions::parse(raw)
            .await
            .map_err(|e| AppError::Config(format!("`{raw}` is not a valid connection string: {e}")))?;

        // Credentials come from the keychain rather than the URL, so they are
        // applied to the parsed options instead of string-splicing them into
        // something the UI displays back.
        if let Some(username) = profile.username.clone().filter(|u| !u.is_empty()) {
            let mut credential = mongodb::options::Credential::default();
            credential.username = Some(username);
            credential.password = secret.filter(|s| !s.is_empty());
            credential.source = profile.options.string("authSource").filter(|s| !s.is_empty());
            options.credential = Some(credential);
        }

        // Without this a dead server makes every poll hang for 30s, and the
        // supervisor's cycle stacks up behind it.
        options.server_selection_timeout = Some(std::time::Duration::from_secs(5));
        options.connect_timeout = Some(std::time::Duration::from_secs(5));
        options.app_name = Some("root-monitor".to_string());

        let client = Client::with_options(options)
            .map_err(|e| AppError::Config(format!("could not build a MongoDB client: {e}")))?;

        Ok(MongoCollector {
            client,
            slow_op_millis: profile.options.u64("slowOpMillis").unwrap_or(1000) as f64,
            endpoint: raw.to_string(),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            reported_ops: Mutex::new(Vec::new()),
        })
    }

    async fn admin_command(&self, command: Document) -> Option<Document> {
        self.client
            .database("admin")
            .run_command(command)
            .await
            .ok()
    }
}

/// BSON numbers arrive as Int32, Int64, or Double depending on magnitude and
/// server version, so every read has to accept all three.
fn number(doc: &Document, key: &str) -> Option<f64> {
    match doc.get(key)? {
        mongodb::bson::Bson::Int32(v) => Some(*v as f64),
        mongodb::bson::Bson::Int64(v) => Some(*v as f64),
        mongodb::bson::Bson::Double(v) => Some(*v),
        _ => None,
    }
}

fn nested<'a>(doc: &'a Document, path: &[&str]) -> Option<&'a Document> {
    let mut current = doc;
    for key in path {
        current = current.get_document(key).ok()?;
    }
    Some(current)
}

/// Replica member state name plus whether it is a healthy state to be in.
fn member_health(state: &str) -> Health {
    match state {
        "PRIMARY" | "SECONDARY" | "ARBITER" => Health::Healthy,
        "STARTUP" | "STARTUP2" | "RECOVERING" | "ROLLBACK" => Health::Degraded,
        // DOWN, UNKNOWN, REMOVED and anything unrecognised.
        _ => Health::Down,
    }
}

#[async_trait]
impl Collector for MongoCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Mongodb
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let Some(status) = self.admin_command(doc! { "serverStatus": 1 }).await else {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Mongodb,
                format!("{} did not answer serverStatus", self.endpoint),
                elapsed_ms(start),
            ));
        };
        let latency = elapsed_ms(start);

        let connections = status.get_document("connections").ok();
        let current = connections.and_then(|c| number(c, "current")).unwrap_or(0.0);
        let available = connections
            .and_then(|c| number(c, "available"))
            .unwrap_or(0.0);
        let total_created = connections
            .and_then(|c| number(c, "totalCreated"))
            .unwrap_or(0.0);
        let limit = current + available;
        let connection_percent = (limit > 0.0).then(|| current / limit * 100.0);

        let resident_mb = nested(&status, &["mem"])
            .and_then(|m| number(m, "resident"))
            .unwrap_or(0.0);
        let uptime = number(&status, "uptime").unwrap_or(0.0);
        let version = status.get_str("version").ok().map(str::to_string);

        let opcounters = status.get_document("opcounters").ok();
        let op = |key: &str| opcounters.and_then(|o| number(o, key)).unwrap_or(0.0);

        // ------------------------------------------------ replica set state
        let repl = self.admin_command(doc! { "replSetGetStatus": 1 }).await;
        let mut member_rows = Vec::new();
        let mut worst_lag = 0.0_f64;
        let mut replica_unhealthy = false;

        if let Some(repl) = &repl {
            // Lag is measured against the primary's last write, so the primary
            // has to be found before any member can be judged.
            let members = repl.get_array("members").ok();
            let primary_optime = members.and_then(|list| {
                list.iter().find_map(|m| {
                    let member = m.as_document()?;
                    (member.get_str("stateStr").ok()? == "PRIMARY")
                        .then(|| member.get_datetime("optimeDate").ok().copied())
                        .flatten()
                })
            });

            for entry in members.into_iter().flatten() {
                let Some(member) = entry.as_document() else {
                    continue;
                };
                let name = member.get_str("name").unwrap_or("unknown").to_string();
                let state = member.get_str("stateStr").unwrap_or("UNKNOWN").to_string();
                let health = member_health(&state);
                if health != Health::Healthy {
                    replica_unhealthy = true;
                }

                let lag = match (primary_optime, member.get_datetime("optimeDate").ok()) {
                    (Some(primary), Some(mine)) => {
                        let seconds =
                            (primary.timestamp_millis() - mine.timestamp_millis()) as f64 / 1000.0;
                        // Clock skew can make a secondary look ahead of the
                        // primary; a negative lag is noise, not information.
                        seconds.max(0.0)
                    }
                    _ => 0.0,
                };
                worst_lag = worst_lag.max(lag);

                member_rows.push(
                    Row::new(
                        format!("member:{name}"),
                        &name,
                        if lag > LAG_WARN_SECS { Health::Degraded } else { health },
                    )
                    .summary(format!("{state} · {lag:.0}s behind"))
                    .cell("name", CellValue::text(&name))
                    .cell("state", CellValue::text(&state))
                    .cell("lag", CellValue::duration(lag))
                    .cell(
                        "uptime",
                        CellValue::duration(number(member, "uptime").unwrap_or(0.0)),
                    ),
                );
            }
        }

        // -------------------------------------------------- long operations
        let long_ops = self.long_running_ops().await;
        let op_rows: Vec<Row> = long_ops
            .iter()
            .map(|op| {
                Row::new(format!("op:{}", op.id), op.description.clone(), Health::Degraded)
                    .summary(format!("{:.0}s in {}", op.seconds, op.namespace))
                    .cell("op", CellValue::text(&op.description))
                    .cell("namespace", CellValue::text(&op.namespace))
                    .cell("duration", CellValue::duration(op.seconds))
                    .cell("type", CellValue::text(&op.op_type))
            })
            .collect();

        // ------------------------------------------------------- databases
        let mut db_rows = Vec::new();
        if let Some(list) = self.admin_command(doc! { "listDatabases": 1 }).await {
            if let Ok(databases) = list.get_array("databases") {
                for entry in databases {
                    let Some(database) = entry.as_document() else {
                        continue;
                    };
                    let name = database.get_str("name").unwrap_or_default().to_string();
                    let size = number(database, "sizeOnDisk").unwrap_or(0.0);
                    db_rows.push(
                        Row::new(format!("db:{name}"), &name, Health::Healthy)
                            .summary(format!("{:.1} MB on disk", size / 1_048_576.0))
                            .cell("name", CellValue::text(&name))
                            .cell("size", CellValue::number(size)),
                    );
                }
                db_rows.sort_by(|a, b| b.id.cmp(&a.id));
            }
        }

        // ---------------------------------------------------------- rollup
        let connections_tight =
            connection_percent.is_some_and(|p| p >= CONNECTION_WARN_PERCENT);

        let health = if replica_unhealthy {
            Health::Down
        } else if connections_tight || worst_lag > LAG_WARN_SECS || !op_rows.is_empty() {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if replica_unhealthy {
            "a replica set member is not healthy".to_string()
        } else if worst_lag > LAG_WARN_SECS {
            format!("replica lag {worst_lag:.0}s")
        } else if connections_tight {
            format!(
                "{:.0}% of connections in use",
                connection_percent.unwrap_or(0.0)
            )
        } else if !op_rows.is_empty() {
            format!(
                "{} long-running operation{}",
                op_rows.len(),
                if op_rows.len() == 1 { "" } else { "s" }
            )
        } else {
            format!("{current:.0} connections · {resident_mb:.0} MB resident")
        };

        let mut metrics = vec![
            Metric::new("connections", "Connections", current, MetricUnit::Count),
            // serverStatus reports MB; converted so the formatter can scale it.
            Metric::new(
                "resident",
                "Resident memory",
                resident_mb * 1_048_576.0,
                MetricUnit::Bytes,
            ),
            Metric::new("uptime", "Uptime", uptime, MetricUnit::Seconds),
            Metric::new("op_query", "Queries", op("query"), MetricUnit::Count),
            Metric::new("op_insert", "Inserts", op("insert"), MetricUnit::Count),
            Metric::new("op_update", "Updates", op("update"), MetricUnit::Count),
            Metric::new("op_delete", "Deletes", op("delete"), MetricUnit::Count),
            Metric::new("connections_created", "Connections created", total_created, MetricUnit::Count),
        ];
        if let Some(percent) = connection_percent {
            metrics.push(
                Metric::new("connection_percent", "Connections used", percent, MetricUnit::Percent)
                    .warn_above(CONNECTION_WARN_PERCENT),
            );
        }
        if repl.is_some() {
            metrics.push(
                Metric::new("replica_lag", "Replica lag", worst_lag, MetricUnit::Seconds)
                    .warn_above(LAG_WARN_SECS),
            );
        }

        let server_row = Row::new("server", &self.endpoint, health)
            .summary(&summary)
            .cell("host", CellValue::text(&self.endpoint))
            .cell("version", CellValue::maybe_text(version))
            .cell("uptime", CellValue::duration(uptime))
            .cell("connections", CellValue::number(current))
            .cell(
                "saturation",
                match connection_percent {
                    Some(p) => CellValue::gauge_warn(p, Some(100.0), CONNECTION_WARN_PERCENT),
                    None => CellValue::Null,
                },
            );

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Mongodb,
            health,
            summary,
            metrics,
            sections: vec![
                Section::new(
                    "server",
                    "Server",
                    vec![
                        ColumnSpec::text("host", "Host").primary(),
                        ColumnSpec::text("version", "Version"),
                        ColumnSpec::duration("uptime", "Uptime"),
                        ColumnSpec::metric("connections", "Connections", MetricUnit::Count),
                        ColumnSpec::gauge("saturation", "Saturation", MetricUnit::Percent),
                    ],
                )
                .rows(vec![server_row])
                .priority(10),
                Section::new(
                    "operations",
                    "Long-running operations",
                    vec![
                        ColumnSpec::code("op", "Operation").primary(),
                        ColumnSpec::text("namespace", "Namespace"),
                        ColumnSpec::duration("duration", "Running for"),
                        ColumnSpec::text("type", "Type"),
                    ],
                )
                .rows(op_rows)
                .priority(5)
                .empty_text("Nothing has been running longer than the threshold."),
                Section::new(
                    "replication",
                    "Replica set",
                    vec![
                        ColumnSpec::text("name", "Member").primary(),
                        ColumnSpec::text("state", "State"),
                        ColumnSpec::duration("lag", "Lag"),
                        ColumnSpec::duration("uptime", "Uptime"),
                    ],
                )
                .rows(member_rows)
                .priority(15)
                .sorted_by_health()
                .empty_text("Standalone server — not a replica set."),
                Section::new(
                    "databases",
                    "Databases",
                    vec![
                        ColumnSpec::text("name", "Database").primary(),
                        ColumnSpec::metric("size", "Size on disk", MetricUnit::Bytes),
                    ],
                )
                .rows(db_rows)
                .priority(20)
                .empty_text("No databases visible to this user."),
            ],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    async fn fetch_logs(&self, _since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let ops = self.long_running_ops().await;
        let mut reported = self.reported_ops.lock().await;
        let mut out = Vec::new();

        for op in ops {
            if reported.contains(&op.id) {
                continue;
            }
            reported.push(op.id.clone());
            out.push(
                LogEvent::new(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Mongodb,
                    &op.namespace,
                    LogLevel::Warn,
                    format!(
                        "operation running {:.0}s: {}",
                        op.seconds, op.description
                    ),
                    Utc::now(),
                )
                .stream(LogStream::Diagnostic),
            );
        }

        if reported.len() > 512 {
            let drop_to = reported.len() - 256;
            reported.drain(..drop_to);
        }

        if out.len() > limit {
            out.truncate(limit);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
struct LongOp {
    id: String,
    namespace: String,
    op_type: String,
    seconds: f64,
    description: String,
}

impl MongoCollector {
    async fn long_running_ops(&self) -> Vec<LongOp> {
        let threshold_secs = self.slow_op_millis / 1000.0;
        let command = doc! {
            "currentOp": 1,
            "active": true,
            "secs_running": { "$gte": threshold_secs as i64 },
        };

        // Fails without the `inprog` privilege, which is a permissions
        // situation rather than an outage — an empty list is the right answer.
        let Some(result) = self.admin_command(command).await else {
            return Vec::new();
        };
        let Ok(inprog) = result.get_array("inprog") else {
            return Vec::new();
        };

        inprog
            .iter()
            .filter_map(|entry| {
                let op = entry.as_document()?;
                let seconds = number(op, "secs_running").unwrap_or(0.0);
                // Idle connections report as active with no command; they are
                // not long-running work.
                let namespace = op.get_str("ns").unwrap_or("").to_string();
                if namespace.is_empty() {
                    return None;
                }
                let op_type = op.get_str("op").unwrap_or("unknown").to_string();
                let id = op
                    .get("opid")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| format!("{namespace}:{seconds}"));

                let description = op
                    .get_document("command")
                    .ok()
                    .map(|c| summarize_command(c))
                    .unwrap_or_else(|| op_type.clone());

                Some(LongOp {
                    id,
                    namespace,
                    op_type,
                    seconds,
                    description,
                })
            })
            .collect()
    }
}

/// A short, readable form of a command document.
///
/// The full document can be enormous — an aggregation pipeline or a bulk write
/// payload — so this keeps the operation name and its target only.
fn summarize_command(command: &Document) -> String {
    let mut parts = Vec::new();
    for (key, value) in command.iter().take(3) {
        if key.starts_with('$') || key == "lsid" || key == "$db" {
            continue;
        }
        let rendered = match value {
            mongodb::bson::Bson::String(s) => s.clone(),
            mongodb::bson::Bson::Document(_) => "{…}".to_string(),
            mongodb::bson::Bson::Array(_) => "[…]".to_string(),
            other => other.to_string(),
        };
        parts.push(format!("{key}: {rendered}"));
    }
    if parts.is_empty() {
        "operation".to_string()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::Bson;

    #[test]
    fn reads_numbers_in_every_bson_width() {
        // Mongo returns Int32, Int64 or Double for the same field depending on
        // magnitude and server version, so all three must work.
        let doc = doc! { "a": 1i32, "b": 2i64, "c": 3.5f64, "d": "no" };
        assert_eq!(number(&doc, "a"), Some(1.0));
        assert_eq!(number(&doc, "b"), Some(2.0));
        assert_eq!(number(&doc, "c"), Some(3.5));
        assert_eq!(number(&doc, "d"), None);
        assert_eq!(number(&doc, "missing"), None);
    }

    #[test]
    fn walks_nested_documents() {
        let doc = doc! { "mem": { "resident": 512i32 } };
        let mem = nested(&doc, &["mem"]).expect("mem");
        assert_eq!(number(mem, "resident"), Some(512.0));
        assert!(nested(&doc, &["nope"]).is_none());
    }

    #[test]
    fn member_states_map_to_health() {
        assert_eq!(member_health("PRIMARY"), Health::Healthy);
        assert_eq!(member_health("SECONDARY"), Health::Healthy);
        assert_eq!(member_health("ARBITER"), Health::Healthy);
        assert_eq!(member_health("RECOVERING"), Health::Degraded);
        assert_eq!(member_health("STARTUP2"), Health::Degraded);
        assert_eq!(member_health("DOWN"), Health::Down);
        // Anything unrecognised is treated as bad rather than assumed fine.
        assert_eq!(member_health("SOMETHING_NEW"), Health::Down);
    }

    #[test]
    fn command_summary_stays_short() {
        let command = doc! {
            "aggregate": "orders",
            "pipeline": Bson::Array(vec![]),
            "cursor": doc! {},
            "lsid": doc! { "id": "x" },
        };
        let summary = summarize_command(&command);
        assert!(summary.contains("aggregate: orders"), "{summary}");
        assert!(summary.contains("[…]"), "pipeline should be elided: {summary}");
        assert!(!summary.contains("lsid"), "session id is noise: {summary}");
    }

    #[test]
    fn command_summary_handles_an_empty_document() {
        assert_eq!(summarize_command(&doc! {}), "operation");
    }
}
