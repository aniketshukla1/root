//! ClickHouse monitor, via the HTTP interface.
//!
//! ClickHouse answers SQL over HTTP, so this collector asks it about itself:
//! `system.metrics` for what the server is doing now, and `system.replicas`
//! for whether replication is actually working. `FORMAT JSON` makes the
//! answers parseable without a client library.
//!
//! `is_readonly` on a replica is the thing to watch, and it is genuinely
//! silent.
//!
//! A replicated table keeps its coordination state in ZooKeeper or Keeper.
//! When a replica loses that session — Keeper restarted, the network blipped,
//! the session expired under load — ClickHouse marks the table read-only. The
//! server stays up. Queries keep answering, and answering *correctly*, from
//! whatever data the replica already had. Inserts to that replica fail, it
//! stops fetching parts from its peers, and it drifts further behind for as
//! long as nobody looks. There is no error anywhere except in that column and
//! in `absolute_delay` growing.
//!
//! `queue_size` is the earlier warning: replication tasks piling up faster
//! than they are executed, usually because merges cannot keep up with inserts.
//! It moves before the delay does.

use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

pub struct ClickhouseCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    /// Replication delay in seconds above which a replica is degraded.
    delay_warn: f64,
    profile_id: String,
    profile_name: String,
}

/// `FORMAT JSON` wraps rows in an envelope with the column metadata.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsonResult<T> {
    #[serde(default = "Vec::new")]
    pub data: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MetricRow {
    pub metric: String,
    /// ClickHouse sends Int64 as a JSON number here, but string-typed columns
    /// arrive quoted — so this is taken as a value and coerced.
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReplicaRow {
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub table: String,
    #[serde(default)]
    pub is_readonly: serde_json::Value,
    #[serde(default)]
    pub is_session_expired: serde_json::Value,
    #[serde(default)]
    pub absolute_delay: serde_json::Value,
    #[serde(default)]
    pub queue_size: serde_json::Value,
}

/// Coerce a ClickHouse JSON cell to a number.
///
/// ClickHouse quotes 64-bit integers in JSON output by default, so the same
/// column arrives as `5` from one build and `"5"` from another. Reading only
/// one shape means a delay of `"420"` silently becomes zero, which is the
/// wrong answer in the reassuring direction.
pub(crate) fn number(value: &serde_json::Value) -> f64 {
    match value {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
        serde_json::Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

impl ReplicaRow {
    pub fn name(&self) -> String {
        format!("{}.{}", self.database, self.table)
    }

    pub fn readonly(&self) -> bool {
        number(&self.is_readonly) > 0.0
    }

    pub fn session_expired(&self) -> bool {
        number(&self.is_session_expired) > 0.0
    }

    pub fn delay(&self) -> f64 {
        number(&self.absolute_delay)
    }

    pub fn queue(&self) -> f64 {
        number(&self.queue_size)
    }
}

impl ClickhouseCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim().trim_end_matches('/');
        if raw.is_empty() {
            return Err(AppError::Config(
                "ClickHouse connections need the HTTP interface URL, e.g. \
                 http://clickhouse.internal:8123"
                    .into(),
            ));
        }
        // The native protocol port (9000) will not answer HTTP, and pointing
        // at it is the likely mistake — but rewriting someone's port silently
        // would be worse than the error they get, so this only strips a path.
        let base = raw.to_string();

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        Ok(ClickhouseCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            base,
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            delay_warn: profile
                .options
                .get("delayWarn")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(60.0),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }

    async fn query<T: serde::de::DeserializeOwned>(
        &self,
        sql: &str,
    ) -> std::result::Result<Vec<T>, String> {
        let request = match (&self.username, &self.secret) {
            (Some(user), secret) => self.client.post(&self.base).basic_auth(user, secret.as_ref()),
            _ => self.client.post(&self.base),
        };
        let response = request
            .body(format!("{sql} FORMAT JSON"))
            .send()
            .await
            .map_err(|e| format!("{}: {e}", self.base))?;

        let status = response.status();
        if !status.is_success() {
            // ClickHouse puts a readable explanation in the body — a missing
            // grant, a syntax error — and it is far more useful than the code.
            let detail = response.text().await.unwrap_or_default();
            let first = detail.lines().next().unwrap_or("").trim();
            return Err(if first.is_empty() {
                format!("{} returned {status}", self.base)
            } else {
                format!("{} returned {status}: {first}", self.base)
            });
        }

        response
            .json::<JsonResult<T>>()
            .await
            .map(|result| result.data)
            .map_err(|e| format!("{} sent a body root could not read: {e}", self.base))
    }
}

#[async_trait]
impl Collector for ClickhouseCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Clickhouse
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let metrics_rows: Vec<MetricRow> = match self
            .query(
                "SELECT metric, value FROM system.metrics WHERE metric IN \
                 ('Query','Merge','PartMutation','ReplicatedFetch','ReplicatedSend', \
                 'ReadonlyReplica','DelayedInserts','DistributedSend','TCPConnection', \
                 'HTTPConnection','BackgroundMergesAndMutationsPoolTask')",
            )
            .await
        {
            Ok(rows) => rows,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Clickhouse,
                    message,
                    elapsed_ms(start),
                ));
            }
        };

        // Replication is optional. A server with no replicated tables answers
        // with no rows, and one whose user lacks the grant errors — both mean
        // "nothing to say about replication", not a failed poll.
        let replicas: Vec<ReplicaRow> = self
            .query(
                "SELECT database, table, is_readonly, is_session_expired, absolute_delay, \
                 queue_size FROM system.replicas",
            )
            .await
            .unwrap_or_default();
        let latency = elapsed_ms(start);

        let metric = |name: &str| {
            metrics_rows
                .iter()
                .find(|r| r.metric == name)
                .map(|r| number(&r.value))
                .unwrap_or(0.0)
        };

        let readonly: Vec<&ReplicaRow> = replicas.iter().filter(|r| r.readonly()).collect();
        let delayed: Vec<&ReplicaRow> = replicas
            .iter()
            .filter(|r| !r.readonly() && r.delay() >= self.delay_warn)
            .collect();
        let delayed_inserts = metric("DelayedInserts");

        let health = if !readonly.is_empty() {
            Health::Down
        } else if !delayed.is_empty() || delayed_inserts > 0.0 {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if !readonly.is_empty() {
            format!(
                "{} read-only — replication has stopped: {}",
                if readonly.len() == 1 { "table" } else { "tables" },
                readonly.iter().map(|r| r.name()).collect::<Vec<_>>().join(", ")
            )
        } else if !delayed.is_empty() {
            let worst = delayed
                .iter()
                .max_by(|a, b| a.delay().total_cmp(&b.delay()))
                .expect("non-empty");
            format!("{} is {:.0}s behind its replicas", worst.name(), worst.delay())
        } else if delayed_inserts > 0.0 {
            // ClickHouse throttles inserts when parts pile up; left alone it
            // starts rejecting them outright.
            format!("{delayed_inserts:.0} inserts being throttled — too many parts")
        } else {
            format!(
                "{:.0} running queries · {} replicated table{}",
                metric("Query"),
                replicas.len(),
                if replicas.len() == 1 { "" } else { "s" }
            )
        };

        let metrics = vec![
            Metric::new("queries", "Running queries", metric("Query"), MetricUnit::Count),
            Metric::new("merges", "Merges", metric("Merge"), MetricUnit::Count),
            Metric::new(
                "readonly_replicas",
                "Read-only replicas",
                metric("ReadonlyReplica").max(readonly.len() as f64),
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new(
                "delayed_inserts",
                "Throttled inserts",
                delayed_inserts,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new(
                "replication_queue",
                "Replication queue",
                replicas.iter().map(ReplicaRow::queue).sum(),
                MetricUnit::Count,
            ),
            Metric::new(
                "max_delay",
                "Worst replica delay",
                replicas.iter().map(ReplicaRow::delay).fold(0.0, f64::max),
                MetricUnit::Seconds,
            )
            .warn_above(self.delay_warn),
            Metric::new(
                "tcp_connections",
                "TCP connections",
                metric("TCPConnection"),
                MetricUnit::Count,
            ),
        ];

        let replica_rows: Vec<Row> = replicas
            .iter()
            .map(|replica| {
                let state = if replica.readonly() {
                    Health::Down
                } else if replica.delay() >= self.delay_warn {
                    Health::Degraded
                } else {
                    Health::Healthy
                };
                Row::new(format!("replica:{}", replica.name()), replica.name(), state)
                    .summary(if replica.readonly() {
                        if replica.session_expired() {
                            "read-only — the Keeper session expired, so replication has stopped"
                                .to_string()
                        } else {
                            "read-only — replication has stopped".to_string()
                        }
                    } else {
                        format!("{:.0}s behind, {:.0} tasks queued", replica.delay(), replica.queue())
                    })
                    .cell("table", CellValue::text(replica.name()))
                    .cell("state", CellValue::status(state))
                    .cell("delay", CellValue::duration(replica.delay()))
                    .cell("queue", CellValue::number(replica.queue()))
            })
            .collect();

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Clickhouse,
            health,
            summary,
            metrics,
            sections: vec![Section::new(
                "replicas",
                "Replicated tables",
                vec![
                    ColumnSpec::text("table", "Table").primary(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::duration("delay", "Behind by"),
                    ColumnSpec::metric("queue", "Queued tasks", MetricUnit::Count),
                ],
            )
            .rows(replica_rows)
            .tracked()
            .empty_text(
                "No replicated tables, or the user has no grant on system.replicas. Either way \
                 there is nothing to report about replication.",
            )
            .priority(10)],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// `system.query_log` is a table this could read, but it is disabled on
    /// plenty of installs and querying it every poll is expensive on the ones
    /// where it is not.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_json_envelope() {
        let result: JsonResult<MetricRow> = serde_json::from_str(
            r#"{"meta":[{"name":"metric","type":"String"}],
                "data":[{"metric":"Query","value":3}],"rows":1}"#,
        )
        .expect("envelope");
        assert_eq!(result.data.len(), 1);
        assert_eq!(number(&result.data[0].value), 3.0);
    }

    /// ClickHouse quotes 64-bit integers in JSON by default, so the same
    /// column arrives as a number from one build and a string from another.
    /// Reading only one shape turns a delay of 420 seconds into zero.
    #[test]
    fn a_quoted_integer_is_still_a_number() {
        assert_eq!(number(&serde_json::json!("420")), 420.0);
        assert_eq!(number(&serde_json::json!(420)), 420.0);
    }

    #[test]
    fn a_boolean_column_coerces() {
        assert_eq!(number(&serde_json::json!(true)), 1.0);
        assert_eq!(number(&serde_json::json!(false)), 0.0);
    }

    #[test]
    fn an_unreadable_cell_is_zero_rather_than_a_panic() {
        assert_eq!(number(&serde_json::Value::Null), 0.0);
        assert_eq!(number(&serde_json::json!("not a number")), 0.0);
    }

    /// The failure the module exists for: the server is up, queries answer,
    /// and this table has quietly stopped replicating.
    #[test]
    fn a_readonly_replica_is_detected_however_the_flag_is_typed() {
        let rows: Vec<ReplicaRow> = serde_json::from_str(
            r#"[{"database":"analytics","table":"events","is_readonly":1,
                 "is_session_expired":"1","absolute_delay":"0","queue_size":0}]"#,
        )
        .expect("replicas");
        assert!(rows[0].readonly());
        assert!(rows[0].session_expired());
        assert_eq!(rows[0].name(), "analytics.events");
    }

    #[test]
    fn a_healthy_replica_reports_its_delay_and_queue() {
        let rows: Vec<ReplicaRow> = serde_json::from_str(
            r#"[{"database":"analytics","table":"events","is_readonly":0,
                 "is_session_expired":0,"absolute_delay":12,"queue_size":4}]"#,
        )
        .expect("replicas");
        assert!(!rows[0].readonly());
        assert_eq!(rows[0].delay(), 12.0);
        assert_eq!(rows[0].queue(), 4.0);
    }

    /// A server with no replicated tables answers with no rows, which is not a
    /// failure and not an outage.
    #[test]
    fn no_replicated_tables_is_an_empty_result_not_an_error() {
        let result: JsonResult<ReplicaRow> =
            serde_json::from_str(r#"{"meta":[],"data":[],"rows":0}"#).expect("envelope");
        assert!(result.data.is_empty());
    }

    #[test]
    fn a_missing_data_key_does_not_fail_the_parse() {
        let result: JsonResult<MetricRow> = serde_json::from_str(r#"{"rows":0}"#).expect("envelope");
        assert!(result.data.is_empty());
    }
}
