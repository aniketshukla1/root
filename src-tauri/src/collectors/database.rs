//! PostgreSQL and MySQL collector.
//!
//! Databases have no log stream reachable over SQL, and this collector
//! deliberately does not invent one: long-running queries, locks and
//! saturation are surfaced as *sections* — typed rows carrying the full
//! statement and a terminate action — rather than as synthesized log lines
//! duplicating the same fact into the shared stream.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::{MySqlPool, PgPool, Row};

use super::{elapsed_ms, Collector, CollectorAction, DetailQuery};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
// `sqlx::Row` (the trait providing `try_get`) and `model::Row` (a table row)
// share a name; alias ours to keep both usable in this file.
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit,
    Row as TableRow, RowAction, Section, ServiceKind,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Monitoring must never be the thing that exhausts the connection pool.
const MAX_POOL_CONNECTIONS: u32 = 2;

enum Backend {
    Postgres(PgPool),
    MySql(MySqlPool),
}

/// One long-running query as reported by the server.
#[derive(Debug, Clone)]
struct LongQuery {
    pid: String,
    seconds: f64,
    state: String,
    query: String,
}

pub struct DatabaseCollector {
    backend: Backend,
    kind: ServiceKind,
    profile_id: String,
    profile_name: String,
    slow_query_secs: u64,
}

impl DatabaseCollector {
    pub async fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let url = build_connection_url(profile, secret.as_deref())?;

        let backend = match profile.kind {
            ServiceKind::Postgres => Backend::Postgres(
                PgPoolOptions::new()
                    .max_connections(MAX_POOL_CONNECTIONS)
                    .acquire_timeout(CONNECT_TIMEOUT)
                    .connect_lazy(&url)
                    .map_err(|e| {
                        AppError::Config(format!("invalid PostgreSQL connection URL: {e}"))
                    })?,
            ),
            ServiceKind::Mysql => Backend::MySql(
                MySqlPoolOptions::new()
                    .max_connections(MAX_POOL_CONNECTIONS)
                    .acquire_timeout(CONNECT_TIMEOUT)
                    .connect_lazy(&url)
                    .map_err(|e| AppError::Config(format!("invalid MySQL connection URL: {e}")))?,
            ),
            other => {
                return Err(AppError::Config(format!(
                    "{} is not a database connection",
                    other.label()
                )))
            }
        };

        Ok(DatabaseCollector {
            backend,
            kind: profile.kind,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            slow_query_secs: profile.options.u64("slowQuerySecs").unwrap_or(5),
        })
    }

    async fn long_queries(&self) -> Result<Vec<LongQuery>> {
        let threshold = self.slow_query_secs as f64;
        match &self.backend {
            Backend::Postgres(pool) => {
                let rows = sqlx::query(
                    "SELECT pid::text AS pid, \
                            EXTRACT(EPOCH FROM (now() - query_start))::float8 AS seconds, \
                            COALESCE(state, '') AS state, \
                            COALESCE(query, '') AS query \
                     FROM pg_stat_activity \
                     WHERE state = 'active' \
                       AND pid <> pg_backend_pid() \
                       AND backend_type = 'client backend' \
                       AND query_start IS NOT NULL \
                       AND now() - query_start > make_interval(secs => $1) \
                     ORDER BY query_start ASC \
                     LIMIT 50",
                )
                .bind(threshold)
                .fetch_all(pool)
                .await?;

                Ok(rows
                    .into_iter()
                    .map(|row| LongQuery {
                        pid: row.try_get("pid").unwrap_or_default(),
                        seconds: row.try_get("seconds").unwrap_or(0.0),
                        state: row.try_get("state").unwrap_or_default(),
                        query: row.try_get("query").unwrap_or_default(),
                    })
                    .collect())
            }
            Backend::MySql(pool) => {
                // `Daemon` threads (the event scheduler) and threads with no
                // statement text sit in the processlist permanently. Reporting
                // them as long-running queries would raise a standing alert on
                // every MySQL server, drowning out real ones.
                let rows = sqlx::query(
                    "SELECT CAST(ID AS CHAR) AS pid, \
                            CAST(TIME AS DOUBLE) AS seconds, \
                            COALESCE(STATE, '') AS state, \
                            COALESCE(INFO, '') AS query \
                     FROM information_schema.PROCESSLIST \
                     WHERE COMMAND NOT IN ('Sleep', 'Daemon') \
                       AND ID <> CONNECTION_ID() \
                       AND INFO IS NOT NULL \
                       AND INFO <> '' \
                       AND TIME >= ? \
                     ORDER BY TIME DESC \
                     LIMIT 50",
                )
                .bind(threshold)
                .fetch_all(pool)
                .await?;

                Ok(rows
                    .into_iter()
                    .map(|row| LongQuery {
                        pid: row.try_get("pid").unwrap_or_default(),
                        seconds: row.try_get("seconds").unwrap_or(0.0),
                        state: row.try_get("state").unwrap_or_default(),
                        query: row.try_get("query").unwrap_or_default(),
                    })
                    .collect())
            }
        }
    }

    async fn collect_postgres(&self, pool: &PgPool, start: Instant) -> Result<HealthSnapshot> {
        let version: String = sqlx::query_scalar("SELECT version()").fetch_one(pool).await?;

        let connections: i64 = sqlx::query_scalar("SELECT count(*)::int8 FROM pg_stat_activity")
            .fetch_one(pool)
            .await
            .unwrap_or(0);
        let max_connections: i64 = sqlx::query_scalar(
            "SELECT setting::int8 FROM pg_settings WHERE name = 'max_connections'",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(0);

        let db_size: i64 =
            sqlx::query_scalar("SELECT pg_database_size(current_database())::int8")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        let blocked_locks: i64 =
            sqlx::query_scalar("SELECT count(*)::int8 FROM pg_locks WHERE NOT granted")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        let cache_hit_ratio: f64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(blks_hit)::float8 / NULLIF(sum(blks_hit) + sum(blks_read), 0), 1)::float8 \
             FROM pg_stat_database",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(1.0);

        let deadlocks: i64 =
            sqlx::query_scalar("SELECT COALESCE(sum(deadlocks), 0)::int8 FROM pg_stat_database")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        // Zero on a primary; seconds behind the primary on a replica.
        let replication_lag: f64 = sqlx::query_scalar(
            "SELECT CASE WHEN pg_is_in_recovery() \
                    THEN COALESCE(EXTRACT(EPOCH FROM (now() - pg_last_xact_replay_timestamp())), 0) \
                    ELSE 0 END::float8",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(0.0);

        let long_queries = self.long_queries().await.unwrap_or_default();

        let databases = sqlx::query(
            "SELECT datname, pg_database_size(datname)::int8 AS size \
             FROM pg_database WHERE datistemplate = false ORDER BY 2 DESC LIMIT 25",
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        let database_rows: Vec<TableRow> = databases
            .into_iter()
            .map(|row| {
                let name: String = row.try_get("datname").unwrap_or_default();
                let size: i64 = row.try_get("size").unwrap_or(0);
                TableRow::new(format!("db:{name}"), &name, Health::Healthy)
                    .summary(human_bytes(size as f64))
                    .cell("database", CellValue::text(&name))
                    .cell("size", CellValue::number(size as f64))
            })
            .collect();

        let sections = database_sections(
            "databases",
            "Databases",
            database_rows,
            &long_queries,
            self.slow_query_secs,
        );

        let connection_pct = if max_connections > 0 {
            connections as f64 / max_connections as f64 * 100.0
        } else {
            0.0
        };

        let health = if connection_pct > 90.0 || blocked_locks > 0 || replication_lag > 60.0 {
            Health::Degraded
        } else if !long_queries.is_empty() {
            Health::Degraded
        } else {
            Health::Healthy
        };

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Postgres,
            health,
            summary: format!(
                "{} — {connections}/{max_connections} connections, {} long-running queries",
                version.split(' ').take(2).collect::<Vec<_>>().join(" "),
                long_queries.len()
            ),
            metrics: vec![
                Metric::new("connections", "Connections", connections as f64, MetricUnit::Count),
                Metric::new(
                    "connection_pct",
                    "Connection use",
                    connection_pct,
                    MetricUnit::Percent,
                )
                .warn_above(80.0),
                Metric::new(
                    "max_connections",
                    "Max connections",
                    max_connections as f64,
                    MetricUnit::Count,
                ),
                Metric::new("db_size", "Database size", db_size as f64, MetricUnit::Bytes),
                Metric::new(
                    "long_queries",
                    "Long queries",
                    long_queries.len() as f64,
                    MetricUnit::Count,
                )
                .warn_above(0.0),
                Metric::new(
                    "blocked_locks",
                    "Blocked locks",
                    blocked_locks as f64,
                    MetricUnit::Count,
                )
                .warn_above(0.0),
                Metric::new(
                    "cache_hit_ratio",
                    "Cache hit ratio",
                    cache_hit_ratio * 100.0,
                    MetricUnit::Percent,
                ),
                Metric::new("deadlocks", "Deadlocks", deadlocks as f64, MetricUnit::Count)
                    .warn_above(0.0),
                Metric::new(
                    "replication_lag",
                    "Replication lag",
                    replication_lag,
                    MetricUnit::Seconds,
                )
                .warn_above(30.0),
            ],
            sections,
            checked_at: Utc::now(),
            latency_ms: elapsed_ms(start),
            error: None,
        })
    }

    async fn collect_mysql(&self, pool: &MySqlPool, start: Instant) -> Result<HealthSnapshot> {
        let version: String = sqlx::query_scalar("SELECT VERSION()").fetch_one(pool).await?;

        // A failed lookup degrades to 0 so one missing counter cannot blank the
        // whole snapshot — but it is logged, because a silently-zero metric
        // looks identical to a genuinely idle server.
        async fn status(pool: &MySqlPool, name: &'static str) -> f64 {
            match mysql_status_value(pool, name).await {
                Ok(value) => value,
                Err(e) => {
                    tracing::warn!("could not read the MySQL status `{name}`: {e}");
                    0.0
                }
            }
        }

        let connections = status(pool, "Threads_connected").await;
        let running = status(pool, "Threads_running").await;
        let aborted = status(pool, "Aborted_connects").await;
        let slow_queries = status(pool, "Slow_queries").await;
        let max_connections = match mysql_variable_value(pool, "max_connections").await {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!("could not read the MySQL variable `max_connections`: {e}");
                0.0
            }
        };

        let total_size: f64 = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT CAST(COALESCE(SUM(data_length + index_length), 0) AS DOUBLE) \
             FROM information_schema.tables",
        )
        .fetch_one(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0.0);

        let long_queries = self.long_queries().await.unwrap_or_default();

        let schemas = sqlx::query(
            "SELECT table_schema AS name, \
                    CAST(COALESCE(SUM(data_length + index_length), 0) AS DOUBLE) AS size \
             FROM information_schema.tables \
             WHERE table_schema NOT IN ('information_schema','performance_schema','mysql','sys') \
             GROUP BY table_schema ORDER BY size DESC LIMIT 25",
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        let schema_rows: Vec<TableRow> = schemas
            .into_iter()
            .map(|row| {
                let name: String = row.try_get("name").unwrap_or_default();
                let size: f64 = row.try_get("size").unwrap_or(0.0);
                TableRow::new(format!("schema:{name}"), &name, Health::Healthy)
                    .summary(human_bytes(size))
                    .cell("database", CellValue::text(&name))
                    .cell("size", CellValue::number(size))
            })
            .collect();

        let sections = database_sections(
            "schemas",
            "Schemas",
            schema_rows,
            &long_queries,
            self.slow_query_secs,
        );

        let connection_pct = if max_connections > 0.0 {
            connections / max_connections * 100.0
        } else {
            0.0
        };

        let health = if connection_pct > 90.0 || !long_queries.is_empty() {
            Health::Degraded
        } else {
            Health::Healthy
        };

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Mysql,
            health,
            summary: format!(
                "MySQL {version} — {connections:.0}/{max_connections:.0} connections, \
                 {} long-running queries",
                long_queries.len()
            ),
            metrics: vec![
                Metric::new("connections", "Connections", connections, MetricUnit::Count),
                Metric::new(
                    "connection_pct",
                    "Connection use",
                    connection_pct,
                    MetricUnit::Percent,
                )
                .warn_above(80.0),
                Metric::new("max_connections", "Max connections", max_connections, MetricUnit::Count),
                Metric::new("threads_running", "Threads running", running, MetricUnit::Count),
                Metric::new("db_size", "Data size", total_size, MetricUnit::Bytes),
                Metric::new(
                    "long_queries",
                    "Long queries",
                    long_queries.len() as f64,
                    MetricUnit::Count,
                )
                .warn_above(0.0),
                Metric::new("slow_queries", "Slow queries", slow_queries, MetricUnit::Count),
                Metric::new("aborted_connects", "Aborted connects", aborted, MetricUnit::Count),
            ],
            sections,
            checked_at: Utc::now(),
            latency_ms: elapsed_ms(start),
            error: None,
        })
    }
}

#[async_trait]
impl Collector for DatabaseCollector {
    fn kind(&self) -> ServiceKind {
        self.kind
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();
        let result = match &self.backend {
            Backend::Postgres(pool) => self.collect_postgres(pool, start).await,
            Backend::MySql(pool) => self.collect_mysql(pool, start).await,
        };

        match result {
            Ok(snapshot) => Ok(snapshot),
            // A connection or auth failure is a legitimate "down" state, not an
            // internal fault — surface it on the dashboard instead of erroring.
            Err(AppError::Collector(detail)) => Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                self.kind,
                detail,
                elapsed_ms(start),
            )),
            Err(other) => Err(other),
        }
    }

    /// Databases contribute no log lines.
    ///
    /// Long-running queries used to be synthesized into the shared stream, but
    /// they are already rows in the Active queries section with the full
    /// statement, a duration, and a terminate action. Emitting them twice made
    /// the same fact appear in two places and pushed real container output out
    /// of the buffer.
    /// The server's running parameters.
    ///
    /// `pg_settings` and `SHOW VARIABLES` are the effective values — after the
    /// config file, after any `ALTER SYSTEM`, after command-line overrides.
    /// The file on disk frequently disagrees, and when it does, the file is
    /// the thing that is wrong.
    ///
    /// Only settings whose source is not the built-in default come back for
    /// Postgres. Nine hundred defaults would bury the fifteen lines somebody
    /// actually chose, and it is the chosen ones that explain behaviour.
    async fn config(&self) -> Result<Vec<crate::configsource::ConfigDocument>> {
        use crate::configsource::ConfigDocument;

        let (via, map) = match &self.backend {
            Backend::Postgres(pool) => {
                let rows = sqlx::query(
                    "SELECT name, setting, unit, source                      FROM pg_settings                      WHERE source <> 'default'                      ORDER BY name",
                )
                .fetch_all(pool)
                .await?;

                let mut map = serde_json::Map::new();
                for row in &rows {
                    let name: String = row.try_get("name").unwrap_or_default();
                    let setting: String = row.try_get("setting").unwrap_or_default();
                    let unit: Option<String> = row.try_get("unit").ok();
                    let source: String = row.try_get("source").unwrap_or_default();
                    // The source travels with the value: "this came from
                    // ALTER SYSTEM" and "this came from postgresql.conf" send
                    // an operator to different files.
                    map.insert(
                        name,
                        serde_json::json!({
                            "value": match unit {
                                Some(u) if !u.is_empty() => format!("{setting}{u}"),
                                _ => setting,
                            },
                            "source": source,
                        }),
                    );
                }
                ("pg_settings (non-default)", map)
            }
            Backend::MySql(pool) => {
                let rows = sqlx::query("SHOW GLOBAL VARIABLES")
                    .fetch_all(pool)
                    .await?;

                let mut map = serde_json::Map::new();
                for row in &rows {
                    let name: String = row.try_get(0).unwrap_or_default();
                    let value: String = row.try_get(1).unwrap_or_default();
                    map.insert(name, serde_json::Value::String(value));
                }
                ("SHOW GLOBAL VARIABLES", map)
            }
        };

        Ok(vec![ConfigDocument::live(
            &self.profile_id,
            "Running parameters",
            via,
            &serde_json::Value::Object(map),
        )])
    }

    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }

    async fn detail(&self, query: &DetailQuery) -> Result<serde_json::Value> {
        let DetailQuery::DbDiagnostic { check } = query else {
            return Err(AppError::UnsupportedAction(self.kind.label().to_string()));
        };

        match check.as_str() {
            "long_queries" => Ok(serde_json::json!({
                "thresholdSeconds": self.slow_query_secs,
                "queries": self
                    .long_queries()
                    .await?
                    .into_iter()
                    .map(|q| serde_json::json!({
                        "pid": q.pid,
                        "seconds": q.seconds,
                        "state": q.state,
                        "query": q.query,
                    }))
                    .collect::<Vec<_>>(),
            })),
            "connections" | "table_sizes" | "locks" | "replication" => {
                self.run_diagnostic(check).await
            }
            other => Err(AppError::Collector(format!(
                "unknown diagnostic `{other}`; valid checks are: connections, long_queries, \
                 table_sizes, locks, replication"
            ))),
        }
    }

    async fn run_action(&self, action: &CollectorAction) -> Result<String> {
        let CollectorAction::TerminateDbQuery { pid } = action else {
            return Err(AppError::UnsupportedAction(self.kind.label().to_string()));
        };

        // The pid is interpolated rather than bound because MySQL's KILL takes
        // no placeholders, so it is validated as an integer first.
        let numeric: i64 = pid
            .trim()
            .parse()
            .map_err(|_| AppError::Collector(format!("`{pid}` is not a valid process id")))?;

        match &self.backend {
            Backend::Postgres(pool) => {
                let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
                    .bind(numeric as i32)
                    .fetch_one(pool)
                    .await?;
                if terminated {
                    Ok(format!("Terminated PostgreSQL backend {numeric}."))
                } else {
                    Ok(format!("Backend {numeric} was no longer running."))
                }
            }
            Backend::MySql(pool) => {
                // MySQL's KILL accepts no bind parameters, so the id is
                // interpolated. `numeric` is the result of parsing into i64
                // above, so nothing but digits can reach the statement.
                sqlx::raw_sql(sqlx::AssertSqlSafe(format!("KILL {numeric}")))
                    .execute(pool)
                    .await?;
                Ok(format!("Killed MySQL thread {numeric}."))
            }
        }
    }
}

impl DatabaseCollector {
    /// The fixed set of diagnostics the assistant may run. Deliberately not
    /// arbitrary SQL — the agent gets named, audited queries only.
    async fn run_diagnostic(&self, check: &str) -> Result<serde_json::Value> {
        match (&self.backend, check) {
            (Backend::Postgres(pool), "connections") => {
                let rows = sqlx::query(
                    "SELECT COALESCE(datname,'') AS datname, COALESCE(usename,'') AS usename, \
                            COALESCE(state,'') AS state, count(*)::int8 AS count \
                     FROM pg_stat_activity GROUP BY 1,2,3 ORDER BY 4 DESC LIMIT 50",
                )
                .fetch_all(pool)
                .await?;
                Ok(serde_json::Value::Array(
                    rows.into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "database": r.try_get::<String, _>("datname").unwrap_or_default(),
                                "user": r.try_get::<String, _>("usename").unwrap_or_default(),
                                "state": r.try_get::<String, _>("state").unwrap_or_default(),
                                "count": r.try_get::<i64, _>("count").unwrap_or(0),
                            })
                        })
                        .collect(),
                ))
            }
            (Backend::Postgres(pool), "table_sizes") => {
                let rows = sqlx::query(
                    "SELECT schemaname || '.' || relname AS name, \
                            pg_total_relation_size(relid)::int8 AS bytes, \
                            n_live_tup::int8 AS row_count \
                     FROM pg_stat_user_tables ORDER BY 2 DESC LIMIT 30",
                )
                .fetch_all(pool)
                .await?;
                Ok(serde_json::Value::Array(
                    rows.into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "table": r.try_get::<String, _>("name").unwrap_or_default(),
                                "bytes": r.try_get::<i64, _>("bytes").unwrap_or(0),
                                "rows": r.try_get::<i64, _>("row_count").unwrap_or(0),
                            })
                        })
                        .collect(),
                ))
            }
            (Backend::Postgres(pool), "locks") => {
                let rows = sqlx::query(
                    "SELECT blocked.pid::text AS blocked_pid, \
                            COALESCE(blocked.query,'') AS blocked_query, \
                            blocking.pid::text AS blocking_pid, \
                            COALESCE(blocking.query,'') AS blocking_query \
                     FROM pg_stat_activity blocked \
                     JOIN pg_stat_activity blocking \
                       ON blocking.pid = ANY(pg_blocking_pids(blocked.pid)) \
                     WHERE cardinality(pg_blocking_pids(blocked.pid)) > 0 LIMIT 30",
                )
                .fetch_all(pool)
                .await?;
                Ok(serde_json::Value::Array(
                    rows.into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "blockedPid": r.try_get::<String, _>("blocked_pid").unwrap_or_default(),
                                "blockedQuery": r.try_get::<String, _>("blocked_query").unwrap_or_default(),
                                "blockingPid": r.try_get::<String, _>("blocking_pid").unwrap_or_default(),
                                "blockingQuery": r.try_get::<String, _>("blocking_query").unwrap_or_default(),
                            })
                        })
                        .collect(),
                ))
            }
            (Backend::Postgres(pool), "replication") => {
                let in_recovery: bool = sqlx::query_scalar("SELECT pg_is_in_recovery()")
                    .fetch_one(pool)
                    .await?;
                let lag: f64 = sqlx::query_scalar(
                    "SELECT CASE WHEN pg_is_in_recovery() \
                            THEN COALESCE(EXTRACT(EPOCH FROM (now() - pg_last_xact_replay_timestamp())), 0) \
                            ELSE 0 END::float8",
                )
                .fetch_one(pool)
                .await
                .unwrap_or(0.0);
                Ok(serde_json::json!({
                    "role": if in_recovery { "replica" } else { "primary" },
                    "lagSeconds": lag,
                }))
            }
            (Backend::MySql(pool), "connections") => {
                let rows = sqlx::query(
                    "SELECT COALESCE(USER,'') AS user, COALESCE(DB,'') AS db, \
                            COALESCE(COMMAND,'') AS command, COUNT(*) AS count \
                     FROM information_schema.PROCESSLIST GROUP BY 1,2,3 ORDER BY 4 DESC LIMIT 50",
                )
                .fetch_all(pool)
                .await?;
                Ok(serde_json::Value::Array(
                    rows.into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "user": r.try_get::<String, _>("user").unwrap_or_default(),
                                "database": r.try_get::<String, _>("db").unwrap_or_default(),
                                "command": r.try_get::<String, _>("command").unwrap_or_default(),
                                "count": r.try_get::<i64, _>("count").unwrap_or(0),
                            })
                        })
                        .collect(),
                ))
            }
            (Backend::MySql(pool), "table_sizes") => {
                let rows = sqlx::query(
                    // `rows` is a reserved word in MySQL 8, so the alias is
                    // `row_count`.
                    "SELECT CONCAT(table_schema, '.', table_name) AS name, \
                            CAST(COALESCE(data_length + index_length, 0) AS DOUBLE) AS bytes, \
                            CAST(COALESCE(table_rows, 0) AS DOUBLE) AS row_count \
                     FROM information_schema.tables \
                     WHERE table_schema NOT IN \
                           ('information_schema','performance_schema','mysql','sys') \
                     ORDER BY bytes DESC LIMIT 30",
                )
                .fetch_all(pool)
                .await?;
                Ok(serde_json::Value::Array(
                    rows.into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "table": r.try_get::<String, _>("name").unwrap_or_default(),
                                "bytes": r.try_get::<f64, _>("bytes").unwrap_or(0.0),
                                "rows": r.try_get::<f64, _>("row_count").unwrap_or(0.0),
                            })
                        })
                        .collect(),
                ))
            }
            (Backend::MySql(pool), "locks") => {
                // performance_schema.data_lock_waits exists on MySQL 8+; older
                // servers simply return nothing useful here.
                let rows = sqlx::query(
                    "SELECT CAST(w.REQUESTING_ENGINE_TRANSACTION_ID AS CHAR) AS waiting, \
                            CAST(w.BLOCKING_ENGINE_TRANSACTION_ID AS CHAR) AS blocking \
                     FROM performance_schema.data_lock_waits w LIMIT 30",
                )
                .fetch_all(pool)
                .await
                .unwrap_or_default();
                Ok(serde_json::Value::Array(
                    rows.into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "waitingTransaction": r.try_get::<String, _>("waiting").unwrap_or_default(),
                                "blockingTransaction": r.try_get::<String, _>("blocking").unwrap_or_default(),
                            })
                        })
                        .collect(),
                ))
            }
            (Backend::MySql(pool), "replication") => {
                // `SHOW REPLICA STATUS` on 8.0.22+, `SHOW SLAVE STATUS` before.
                let rows = match sqlx::query("SHOW REPLICA STATUS").fetch_all(pool).await {
                    Ok(rows) => rows,
                    Err(_) => sqlx::query("SHOW SLAVE STATUS")
                        .fetch_all(pool)
                        .await
                        .unwrap_or_default(),
                };
                if rows.is_empty() {
                    return Ok(serde_json::json!({ "role": "primary", "lagSeconds": 0 }));
                }
                let row = &rows[0];
                let lag = row
                    .try_get::<Option<i64>, _>("Seconds_Behind_Source")
                    .or_else(|_| row.try_get::<Option<i64>, _>("Seconds_Behind_Master"))
                    .ok()
                    .flatten();
                Ok(serde_json::json!({
                    "role": "replica",
                    "lagSeconds": lag,
                }))
            }
            _ => Err(AppError::Collector(format!(
                "diagnostic `{check}` is not available for {}",
                self.kind.label()
            ))),
        }
    }
}

/// Read one server status counter or configuration variable.
///
/// The obvious spelling — `SHOW GLOBAL STATUS LIKE ?` — cannot be used: MySQL
/// does not accept placeholders in `SHOW`, and sqlx sends everything as a
/// prepared statement, so the bind fails with a syntax error. The
/// `performance_schema` tables expose the same data and do take binds.
/// MariaDB and older MySQL keep them under `information_schema` instead, hence
/// the fallback.
///
/// Values are stored as strings even for numeric counters.
async fn mysql_lookup(pool: &MySqlPool, table: &str, name: &str) -> Result<f64> {
    let (primary, fallback) = match table {
        "status" => (
            "SELECT VARIABLE_VALUE FROM performance_schema.global_status WHERE VARIABLE_NAME = ?",
            "SELECT VARIABLE_VALUE FROM information_schema.GLOBAL_STATUS WHERE VARIABLE_NAME = ?",
        ),
        _ => (
            "SELECT VARIABLE_VALUE FROM performance_schema.global_variables WHERE VARIABLE_NAME = ?",
            "SELECT VARIABLE_VALUE FROM information_schema.GLOBAL_VARIABLES WHERE VARIABLE_NAME = ?",
        ),
    };

    let value: String = match sqlx::query_scalar(primary).bind(name).fetch_one(pool).await {
        Ok(value) => value,
        Err(_) => sqlx::query_scalar(fallback).bind(name).fetch_one(pool).await?,
    };

    value
        .parse()
        .map_err(|_| AppError::Collector(format!("`{name}` is not numeric: {value}")))
}

async fn mysql_status_value(pool: &MySqlPool, name: &str) -> Result<f64> {
    mysql_lookup(pool, "status", name).await
}

async fn mysql_variable_value(pool: &MySqlPool, name: &str) -> Result<f64> {
    mysql_lookup(pool, "variables", name).await
}

/// Build the section list shared by both database engines.
///
/// Active queries lead when there are any: during an incident that table *is*
/// the incident, and the SQL text needs to be readable in full rather than
/// truncated into a column shared with inventory rows.
fn database_sections(
    inventory_id: &str,
    inventory_label: &str,
    inventory_rows: Vec<TableRow>,
    long_queries: &[LongQuery],
    threshold_secs: u64,
) -> Vec<Section> {
    let query_rows: Vec<TableRow> = long_queries
        .iter()
        .map(|query| {
            let health = if query.seconds > 60.0 {
                Health::Down
            } else {
                Health::Degraded
            };
            TableRow::new(
                format!("query:{}", query.pid),
                format!("pid {}", query.pid),
                health,
            )
            .summary(format!(
                "running {:.0}s: {}",
                query.seconds,
                truncate(&query.query, 120)
            ))
            .cell("pid", CellValue::text(&query.pid))
            .cell("duration", CellValue::duration(query.seconds))
            .cell(
                "state",
                CellValue::maybe_text(Some(query.state.clone())),
            )
            // The full statement, in its own monospace column.
            .cell("query", CellValue::text(&query.query))
            .action(
                RowAction::new(
                    "terminate_database_query",
                    "Terminate",
                    serde_json::json!({ "pid": query.pid }),
                )
                .destructive(),
            )
        })
        .collect();

    let mut sections = Vec::new();

    if !query_rows.is_empty() {
        sections.push(
            Section::new(
                "active_queries",
                "Active queries",
                vec![
                    ColumnSpec::text("pid", "PID").primary(),
                    ColumnSpec::duration("duration", "Running for"),
                    ColumnSpec::text("state", "State"),
                    ColumnSpec::code("query", "Statement"),
                ],
            )
            .rows(query_rows)
            .priority(5)
            .empty_text(&format!(
                "Nothing has been running longer than {threshold_secs}s."
            ))
            .sorted_by_health(),
        );
    }

    sections.push(
        Section::new(
            inventory_id,
            inventory_label,
            vec![
                ColumnSpec::text("database", inventory_label.trim_end_matches('s')).primary(),
                ColumnSpec::metric("size", "Size", MetricUnit::Bytes),
            ],
        )
        .rows(inventory_rows)
        .priority(20),
    );

    sections
}

/// Merge the stored credential into the connection URL. The password is kept
/// out of the config file, so it has to be injected at connect time.
fn build_connection_url(profile: &ConnectionProfile, secret: Option<&str>) -> Result<String> {
    let raw = profile.endpoint.trim();
    if raw.is_empty() {
        return Err(AppError::Config(format!(
            "{} connections need a URL, e.g. postgres://user@host:5432/dbname",
            profile.kind.label()
        )));
    }

    let mut url = url::Url::parse(raw)
        .map_err(|e| AppError::Config(format!("`{raw}` is not a valid connection URL: {e}")))?;

    if let Some(username) = profile.username.as_deref().filter(|u| !u.is_empty()) {
        if url.username().is_empty() {
            url.set_username(username)
                .map_err(|_| AppError::Config("could not set the username on the URL".into()))?;
        }
    }
    if let Some(secret) = secret.filter(|s| !s.is_empty()) {
        if url.password().is_none() {
            url.set_password(Some(secret))
                .map_err(|_| AppError::Config("could not set the password on the URL".into()))?;
        }
    }
    if let Some(database) = profile.options.string("database") {
        if url.path().trim_matches('/').is_empty() {
            url.set_path(&database);
        }
    }

    Ok(url.to_string())
}

fn truncate(text: &str, max: usize) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
    }
}

fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileOptions;

    fn profile(endpoint: &str, username: Option<&str>) -> ConnectionProfile {
        ConnectionProfile {
            id: "p1".into(),
            name: "db".into(),
            kind: ServiceKind::Postgres,
            endpoint: endpoint.into(),
            username: username.map(str::to_string),
            has_secret: false,
            enabled: true,
            poll_interval_secs: 10,
            options: ProfileOptions::default(),
            depends_on: Vec::new(),
            config_sources: Vec::new(),
            history_source: None,
            metadata: ProfileOptions::default(),
        }
    }

    #[test]
    fn injects_credentials_into_url() {
        let url = build_connection_url(
            &profile("postgres://localhost:5432/app", Some("monitor")),
            Some("s3cret"),
        )
        .unwrap();
        assert!(url.starts_with("postgres://monitor:s3cret@localhost:5432/app"), "{url}");
    }

    #[test]
    fn keeps_credentials_already_in_the_url() {
        let url = build_connection_url(
            &profile("postgres://inline:pw@localhost/app", Some("other")),
            Some("ignored"),
        )
        .unwrap();
        assert!(url.contains("inline:pw@"), "{url}");
    }

    #[test]
    fn rejects_an_empty_endpoint() {
        assert!(build_connection_url(&profile("", None), None).is_err());
    }

    #[test]
    fn truncate_collapses_whitespace() {
        assert_eq!(truncate("select   *\n from  t", 100), "select * from t");
        assert_eq!(truncate("abcdefghij", 4), "abcd…");
    }
}
