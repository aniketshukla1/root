//! The estate's history, on disk, in Postgres.
//!
//! Everything root computes has until now lived in memory and died with the
//! process. `history.rs` says so in its first paragraph and is right to: a ring
//! buffer has no retention policy to manage, and root should not become a
//! time-series database. That argument is about what root *builds*, not about
//! what root *writes to*.
//!
//! So the ring buffer stays exactly as it is — it is the live layer, and every
//! chart still reads it. This is the layer underneath, and it exists to answer
//! the one question the ring buffer cannot: what happened while nobody was
//! watching.
//!
//! ## One engine, two shapes of data
//!
//! Events — alerts and their whole lifecycle, deploys, changes — are sparse.
//! Hundreds of rows a day, relational, worth keeping for years, and the ones
//! with narrative in them. They go in ordinary tables.
//!
//! Telemetry — metric samples and log lines — is the opposite: millions of
//! rows, uniform, worth less individually and more in aggregate. Those go in
//! hypertables, and TimescaleDB does three things to them that are the entire
//! reason it is worth a dependency:
//!
//!   1. **Continuous aggregates.** Hourly min/max/avg is maintained by the
//!      database as rows arrive, not by a rollup task here that has to be
//!      correct about clock skew, restarts and late samples.
//!   2. **Compression.** Columnar compression on a week-old chunk, which is
//!      ten to twenty times on log text.
//!   3. **Retention as a policy.** `add_retention_policy` drops old chunks.
//!      The retention question gets a built-in answer instead of a cron job.
//!
//! Raw samples are dropped after thirty days. The hourly aggregate is kept
//! forever, and it is small — and it is what makes `forecast.rs` mean
//! something, since that module refuses a window under fifteen minutes and has
//! only ever had three hours of buffer to look at.
//!
//! ## Timescale is preferred, not required
//!
//! Every hypertable, aggregate, compression and retention statement is issued
//! optimistically and its failure is logged rather than propagated. On a plain
//! Postgres with no extension the tables are still created, the writes still
//! work, and the reads still return the right answers — just without
//! compression or automatic rollup. An operator who has Postgres and not
//! Timescale gets a slower version of the same product, not an error.
//!
//! ## It may never hold up a poll
//!
//! Same posture as `influx.rs`: every write here is best-effort, logged on
//! failure, and never surfaced as connection state. A database that is down
//! must not make a healthy service look unhealthy — root failing to write its
//! notes is not an outage, and reporting it as one would be root monitoring
//! itself and calling it infrastructure.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgPool, Row};

use crate::error::{AppError, Result};
use crate::model::{Alert, ChangeEvent, HealthSnapshot, LogEvent};

/// Connections in the pool. Writes are small and frequent and reads are rare;
/// this is a desktop-sized workload even when it runs on a server.
const MAX_CONNECTIONS: u32 = 8;

/// Days of raw metric samples kept before the chunks are dropped. The hourly
/// aggregate outlives them and is what long-range questions actually read.
///
/// Must match the `add_retention_policy` interval in [`enable_timescale`],
/// which spells it out in SQL rather than interpolating this — see the note
/// there for why.
const RAW_METRIC_DAYS: i64 = 30;

/// Rows written in one statement. Log pulls arrive in batches of up to a few
/// thousand and a single multi-row insert is an order of magnitude cheaper
/// than a thousand round trips.
const MAX_BATCH: usize = 1_000;

/// One bucket of a metric's history: an hour from Postgres, and from the
/// local archive whatever resolution it still keeps for the window asked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BucketPoint {
    pub at: DateTime<Utc>,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub count: i64,
}

/// An alert as it was, including when it ended.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastAlert {
    pub fingerprint: String,
    pub profile_id: String,
    pub profile_name: String,
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub component: Option<String>,
    /// Kept apart on disk for the same reason it is kept apart in memory: a
    /// forecast is not an alert, and a history that mixed them would make
    /// every count over it wrong.
    pub predicted: bool,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// A change as it was recorded.
///
/// Enums arrive as the strings they were stored as, exactly as [`PastAlert`]
/// does. A history row is read to be displayed and counted, never to be acted
/// on, and parsing back into the live enums would buy nothing while adding a
/// way for a row written by an older build to fail to load at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastChange {
    pub id: String,
    pub at: DateTime<Utc>,
    pub profile_id: String,
    pub profile_name: String,
    pub kind: String,
    pub change: String,
    pub source: String,
    pub section: Option<String>,
    pub component: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub summary: String,
}

/// The persistent half of root's memory, in an operator's Postgres.
#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
    /// Whether the hypertable statements took. Reads do not branch on this —
    /// the queries are the same either way — but it is reported to the UI so
    /// an operator can tell a slow install from a broken one.
    timescale: bool,
}

impl PgStore {
    /// Connect and bring the schema up to date.
    pub async fn open(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .connect(url)
            .await
            .map_err(|e| AppError::Config(format!("could not reach the database: {e}")))?;

        migrate(&pool).await?;
        let timescale = enable_timescale(&pool).await;

        Ok(PgStore { pool, timescale })
    }

    /// Whether the TimescaleDB extension is doing the aggregation, the
    /// compression and the retention.
    pub fn has_timescale(&self) -> bool {
        self.timescale
    }

    // ------------------------------------------------------------- writing

    /// Record every metric in one snapshot.
    ///
    /// Called after each successful poll, from the same place the ring buffer
    /// is updated, so the two layers never disagree about what was seen.
    pub async fn write_snapshot(&self, snapshot: &HealthSnapshot) -> Result<()> {
        let finite: Vec<&crate::model::Metric> = snapshot
            .metrics
            .iter()
            .filter(|m| m.value.is_finite())
            .collect();
        if finite.is_empty() {
            return Ok(());
        }

        // A value that is not finite was not measured. Writing it as zero
        // would put a cliff in a chart that nothing in the estate did.
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO metric_samples (at, profile_id, metric_key, value) ",
        );
        builder.push_values(finite.iter().take(MAX_BATCH), |mut row, metric| {
            row.push_bind(snapshot.checked_at)
                .push_bind(&snapshot.profile_id)
                .push_bind(&metric.key)
                .push_bind(metric.value);
        });
        builder.build().execute(&self.pool).await?;
        Ok(())
    }

    /// Record log lines. Ignores duplicates by leaving no uniqueness to
    /// violate: the watermark upstream already stops the same line arriving
    /// twice, and a primary key over message text would cost more than the
    /// occasional repeat.
    pub async fn write_logs(&self, events: &[LogEvent]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        for chunk in events.chunks(MAX_BATCH) {
            let mut builder = sqlx::QueryBuilder::new(
                "INSERT INTO log_lines (at, profile_id, source, level, stream, message) ",
            );
            builder.push_values(chunk, |mut row, event| {
                row.push_bind(event.timestamp)
                    .push_bind(&event.profile_id)
                    .push_bind(&event.source)
                    .push_bind(wire(event.level))
                    .push_bind(wire(event.stream))
                    .push_bind(&event.message);
            });
            builder.build().execute(&self.pool).await?;
        }
        Ok(())
    }

    /// Record an alert, or move the one already open.
    ///
    /// Keyed on `(fingerprint, first_seen)` rather than on the alert's own id,
    /// because the id is regenerated each time the engine raises it while the
    /// fingerprint is the condition. The same condition firing, resolving and
    /// firing again is two rows, which is what makes "how often does this
    /// happen" answerable.
    pub async fn write_alert(&self, alert: &Alert, predicted: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO alerts (
                 fingerprint, first_seen, profile_id, profile_name, kind, severity,
                 title, detail, component, predicted, last_seen, resolved_at, acknowledged
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
             ON CONFLICT (fingerprint, first_seen) DO UPDATE SET
                 last_seen    = EXCLUDED.last_seen,
                 resolved_at  = EXCLUDED.resolved_at,
                 acknowledged = EXCLUDED.acknowledged,
                 severity     = EXCLUDED.severity,
                 detail       = EXCLUDED.detail",
        )
        .bind(&alert.fingerprint)
        .bind(alert.first_seen)
        .bind(&alert.profile_id)
        .bind(&alert.profile_name)
        .bind(wire(alert.kind))
        .bind(wire(alert.severity))
        .bind(&alert.title)
        .bind(&alert.detail)
        .bind(&alert.component)
        .bind(predicted)
        .bind(alert.last_seen)
        // An inactive alert is a resolved one, and the moment it was last seen
        // is the moment it stopped being true.
        .bind((!alert.active).then_some(alert.last_seen))
        .bind(alert.acknowledged)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record a deploy or other observed change.
    pub async fn write_change(&self, change: &ChangeEvent) -> Result<()> {
        sqlx::query(
            "INSERT INTO changes (
                 id, at, profile_id, profile_name, kind, change, source,
                 section, component, from_value, to_value, summary
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&change.id)
        .bind(change.at)
        .bind(&change.profile_id)
        .bind(&change.profile_name)
        .bind(wire(change.kind))
        .bind(wire(change.change))
        .bind(wire(change.source))
        .bind(&change.section)
        .bind(&change.component)
        .bind(&change.from)
        .bind(&change.to)
        .bind(&change.summary)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ------------------------------------------------------------- reading

    /// Hourly history for one metric, from the continuous aggregate.
    ///
    /// Reads the aggregate where Timescale maintains one and falls back to
    /// aggregating the raw table otherwise, so the answer is the same shape on
    /// both installs — only the cost differs.
    pub async fn metric_hours(
        &self,
        profile_id: &str,
        metric_key: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<BucketPoint>> {
        let sql = if self.timescale {
            "SELECT bucket AS at, min_value, max_value, avg_value, sample_count
             FROM metric_hourly
             WHERE profile_id = $1 AND metric_key = $2 AND bucket >= $3
             ORDER BY bucket"
        } else {
            "SELECT date_trunc('hour', at) AS at,
                    MIN(value) AS min_value, MAX(value) AS max_value,
                    AVG(value)::double precision AS avg_value,
                    COUNT(*) AS sample_count
             FROM metric_samples
             WHERE profile_id = $1 AND metric_key = $2 AND at >= $3
             GROUP BY 1 ORDER BY 1"
        };

        let rows = sqlx::query(sql)
            .bind(profile_id)
            .bind(metric_key)
            .bind(since)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.iter().map(hourly_point).collect())
    }

    /// Every time this condition has fired, newest first.
    ///
    /// The question a ring buffer can never answer: is this the first time, or
    /// the ninth time this month?
    pub async fn alert_history(
        &self,
        profile_id: &str,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<PastAlert>> {
        let rows = sqlx::query(
            "SELECT fingerprint, profile_id, profile_name, severity, title, detail,
                    component, predicted, first_seen, last_seen, resolved_at
             FROM alerts
             WHERE profile_id = $1 AND first_seen >= $2 AND predicted = FALSE
             ORDER BY first_seen DESC
             LIMIT $3",
        )
        .bind(profile_id)
        .bind(since)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(past_alert).collect())
    }

    /// How many times each condition fired over the window, worst first.
    ///
    /// The flapping question. One alert that fired forty times is a different
    /// problem from forty alerts that fired once, and a list ordered by
    /// recency shows them identically.
    pub async fn alert_frequency(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<(String, String, i64)>> {
        // Grouped by fingerprint, which is the identity of a condition, and
        // not by title, which is how it happens to read this week. Two
        // different conditions can share a title and one condition's title can
        // be reworded; either would make a count keyed on the string answer
        // confidently and wrongly. The most recent title is carried for
        // display, because that is the wording the operator last saw.
        let rows = sqlx::query(
            "SELECT DISTINCT ON (profile_id, fingerprint)
                    profile_name,
                    title,
                    COUNT(*) OVER (PARTITION BY profile_id, fingerprint) AS times
             FROM alerts
             WHERE first_seen >= $1 AND predicted = FALSE
             ORDER BY profile_id, fingerprint, first_seen DESC",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;

        // A single occurrence is not a pattern, and fifty rows of "happened
        // once" would bury the one that happened forty times.
        let mut out: Vec<(String, String, i64)> = rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("profile_name"),
                    r.get::<String, _>("title"),
                    r.get::<i64, _>("times"),
                )
            })
            .filter(|(_, _, times)| *times > 1)
            .collect();
        out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        out.truncate(50);
        Ok(out)
    }

    /// Changes since a moment, newest first. One connection, or the estate.
    ///
    /// Every column, not the three a tooltip needs. The write path has always
    /// stored the whole event and a read that dropped nine of its twelve
    /// columns made the table unable to answer the questions it was already
    /// holding the data for — which component, what it moved from, whether a
    /// human or a webhook reported it.
    pub async fn changes_since(
        &self,
        profile_id: Option<&str>,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<PastChange>> {
        // Two statements rather than one with `($1 IS NULL OR profile_id = $1)`,
        // which reads as if it were free and costs the index: Postgres cannot
        // plan the estate-wide case and the single-connection case the same
        // way, and the null-guard form plans for neither.
        let rows = match profile_id {
            Some(id) => {
                sqlx::query(
                    "SELECT id, at, profile_id, profile_name, kind, change, source,
                            section, component, from_value, to_value, summary
                     FROM changes
                     WHERE profile_id = $1 AND at >= $2
                     ORDER BY at DESC LIMIT $3",
                )
                .bind(id)
                .bind(since)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, at, profile_id, profile_name, kind, change, source,
                            section, component, from_value, to_value, summary
                     FROM changes
                     WHERE at >= $1
                     ORDER BY at DESC LIMIT $2",
                )
                .bind(since)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };

        Ok(rows.iter().map(past_change).collect())
    }

    /// Rows held, per table, for the Settings panel.
    ///
    /// Shown because a database nobody can see the size of is a database
    /// somebody eventually finds full.
    pub async fn counts(&self) -> Result<HashMap<String, i64>> {
        // Written out rather than interpolated. sqlx refuses a dynamic query
        // string and is right to: four literals cost nothing and cannot be
        // turned into a fifth by accident.
        const QUERIES: [(&str, &str); 4] = [
            ("alerts", "SELECT COUNT(*) AS n FROM alerts"),
            ("changes", "SELECT COUNT(*) AS n FROM changes"),
            ("metric_samples", "SELECT COUNT(*) AS n FROM metric_samples"),
            ("log_lines", "SELECT COUNT(*) AS n FROM log_lines"),
        ];

        let mut out = HashMap::new();
        for (table, sql) in QUERIES {
            let row = sqlx::query(sql).fetch_one(&self.pool).await?;
            out.insert(table.to_string(), row.get::<i64, _>("n"));
        }
        Ok(out)
    }

    /// Drop everything the estate recorded, leaving the judgement record alone.
    ///
    /// The reason this lives in its own database rather than beside the
    /// journal: operational history is regenerable and large, and the
    /// judgement record is irreplaceable and small. "Clear my history" must
    /// never be able to reach the latter.
    pub async fn clear(&self) -> Result<()> {
        // One statement, so a failure part-way cannot leave half an estate's
        // history behind.
        sqlx::query("TRUNCATE TABLE metric_samples, log_lines, alerts, changes")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Where history is kept: an operator's Postgres when one is configured, and
/// otherwise a SQLite file beside the config — so every install keeps a month
/// and survives a restart, and nobody has to run a database for that.
#[derive(Clone)]
pub enum Store {
    Postgres(PgStore),
    Local(crate::archive::Archive),
}

impl Store {
    /// Connect to Postgres and bring its schema up to date.
    pub async fn open(url: &str) -> Result<Self> {
        PgStore::open(url).await.map(Store::Postgres)
    }

    /// Open (creating if needed) the archive file at `path`.
    pub async fn open_local(path: &std::path::Path) -> Result<Self> {
        crate::archive::Archive::open(path).await.map(Store::Local)
    }

    pub fn has_timescale(&self) -> bool {
        matches!(self, Store::Postgres(pg) if pg.has_timescale())
    }

    /// Which kind of store this is, as the Settings panel names it.
    pub fn backend(&self) -> &'static str {
        match self {
            Store::Postgres(_) => "postgres",
            Store::Local(_) => "local",
        }
    }

    /// Size on disk, where root can see it — only for the local file.
    pub fn bytes(&self) -> Option<u64> {
        match self {
            Store::Postgres(_) => None,
            Store::Local(archive) => archive.bytes(),
        }
    }

    pub async fn write_snapshot(&self, snapshot: &HealthSnapshot) -> Result<()> {
        match self {
            Store::Postgres(pg) => pg.write_snapshot(snapshot).await,
            Store::Local(archive) => archive.write_snapshot(snapshot).await,
        }
    }

    pub async fn write_logs(&self, events: &[LogEvent]) -> Result<()> {
        match self {
            Store::Postgres(pg) => pg.write_logs(events).await,
            Store::Local(archive) => archive.write_logs(events).await,
        }
    }

    pub async fn write_alert(&self, alert: &Alert, predicted: bool) -> Result<()> {
        match self {
            Store::Postgres(pg) => pg.write_alert(alert, predicted).await,
            Store::Local(archive) => archive.write_alert(alert, predicted).await,
        }
    }

    pub async fn write_change(&self, change: &ChangeEvent) -> Result<()> {
        match self {
            Store::Postgres(pg) => pg.write_change(change).await,
            Store::Local(archive) => archive.write_change(change).await,
        }
    }

    /// A connection metric's history since `since`: hourly from Postgres, and
    /// as fine as the archive still keeps for the window from the local file.
    pub async fn metric_buckets(
        &self,
        profile_id: &str,
        metric_key: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<BucketPoint>> {
        match self {
            Store::Postgres(pg) => pg.metric_hours(profile_id, metric_key, since).await,
            Store::Local(archive) => archive.buckets(profile_id, "", metric_key, since).await,
        }
    }

    /// A component column's history. Postgres keeps none, so it answers empty.
    pub async fn entity_buckets(
        &self,
        profile_id: &str,
        section: &str,
        row: &str,
        column: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<BucketPoint>> {
        match self {
            Store::Postgres(_) => Ok(Vec::new()),
            Store::Local(archive) => {
                let entity = crate::archive::Archive::entity(section, row);
                archive.buckets(profile_id, &entity, column, since).await
            }
        }
    }

    pub async fn alert_history(
        &self,
        profile_id: &str,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<PastAlert>> {
        match self {
            Store::Postgres(pg) => pg.alert_history(profile_id, since, limit).await,
            Store::Local(archive) => archive.alert_history(profile_id, since, limit).await,
        }
    }

    pub async fn alert_frequency(&self, since: DateTime<Utc>) -> Result<Vec<(String, String, i64)>> {
        match self {
            Store::Postgres(pg) => pg.alert_frequency(since).await,
            Store::Local(archive) => archive.alert_frequency(since).await,
        }
    }

    pub async fn changes_since(
        &self,
        profile_id: Option<&str>,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<PastChange>> {
        match self {
            Store::Postgres(pg) => pg.changes_since(profile_id, since, limit).await,
            Store::Local(archive) => archive.changes_since(profile_id, since, limit).await,
        }
    }

    pub async fn counts(&self) -> Result<HashMap<String, i64>> {
        match self {
            Store::Postgres(pg) => pg.counts().await,
            Store::Local(archive) => archive.counts().await,
        }
    }

    pub async fn clear(&self) -> Result<()> {
        match self {
            Store::Postgres(pg) => pg.clear().await,
            Store::Local(archive) => archive.clear().await,
        }
    }
}

/// An enum's wire name, as serde writes it everywhere else in the app.
///
/// Not `format!("{:?}").to_lowercase()`, which is what this used to be and
/// which agrees with serde only by coincidence. Every one of these enums is
/// `rename_all`, so a multi-word variant diverges the moment one exists:
/// `ServiceKind::LinuxHost` is `linux_host` to serde and `linuxhost` to
/// `Debug`. The column is read back now, and a kind that matches nothing in
/// the registry renders as an unknown service.
fn wire<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn hourly_point(row: &PgRow) -> BucketPoint {
    BucketPoint {
        at: row.get("at"),
        min: row.try_get("min_value").unwrap_or(0.0),
        max: row.try_get("max_value").unwrap_or(0.0),
        // Both queries cast AVG to double precision, so the aggregate and the
        // fallback hand back the same type and this never has to know which
        // install it is reading from.
        avg: row.try_get("avg_value").unwrap_or(0.0),
        count: row.try_get("sample_count").unwrap_or(0),
    }
}

fn past_alert(row: &PgRow) -> PastAlert {
    PastAlert {
        fingerprint: row.get("fingerprint"),
        profile_id: row.get("profile_id"),
        profile_name: row.get("profile_name"),
        severity: row.get("severity"),
        title: row.get("title"),
        detail: row.get("detail"),
        component: row.try_get("component").ok(),
        predicted: row.try_get("predicted").unwrap_or(false),
        first_seen: row.get("first_seen"),
        last_seen: row.get("last_seen"),
        resolved_at: row.try_get("resolved_at").ok(),
    }
}

fn past_change(row: &PgRow) -> PastChange {
    PastChange {
        id: row.get("id"),
        at: row.get("at"),
        profile_id: row.get("profile_id"),
        profile_name: row.get("profile_name"),
        kind: row.get("kind"),
        change: row.get("change"),
        source: row.get("source"),
        section: row.try_get("section").ok(),
        component: row.try_get("component").ok(),
        from: row.try_get("from_value").ok(),
        to: row.try_get("to_value").ok(),
        summary: row.get("summary"),
    }
}

/// Schema, applied on every open.
///
/// `IF NOT EXISTS` throughout rather than a migration framework, for the same
/// reason `journal.rs` gives: the tables are append-shaped, and the day a
/// column has to change the honest move is a new column with a default rather
/// than a rewrite of history.
async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS alerts (
            fingerprint  TEXT        NOT NULL,
            first_seen   TIMESTAMPTZ NOT NULL,
            profile_id   TEXT        NOT NULL,
            profile_name TEXT        NOT NULL,
            kind         TEXT        NOT NULL,
            severity     TEXT        NOT NULL,
            title        TEXT        NOT NULL,
            detail       TEXT        NOT NULL,
            component    TEXT,
            predicted    BOOLEAN     NOT NULL DEFAULT FALSE,
            last_seen    TIMESTAMPTZ NOT NULL,
            resolved_at  TIMESTAMPTZ,
            acknowledged BOOLEAN     NOT NULL DEFAULT FALSE,
            PRIMARY KEY (fingerprint, first_seen)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS alerts_profile ON alerts (profile_id, first_seen DESC)")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS changes (
            id           TEXT PRIMARY KEY,
            at           TIMESTAMPTZ NOT NULL,
            profile_id   TEXT NOT NULL,
            profile_name TEXT NOT NULL,
            kind         TEXT NOT NULL,
            change       TEXT NOT NULL,
            source       TEXT NOT NULL,
            section      TEXT,
            component    TEXT,
            from_value   TEXT,
            to_value     TEXT,
            summary      TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS changes_at ON changes (at DESC)")
        .execute(pool)
        .await?;

    // The per-connection read. Without it, asking one connection for its
    // changes scans every connection's.
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS changes_profile_at ON changes (profile_id, at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS metric_samples (
            at         TIMESTAMPTZ NOT NULL,
            profile_id TEXT NOT NULL,
            metric_key TEXT NOT NULL,
            value      DOUBLE PRECISION NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS metric_samples_series
         ON metric_samples (profile_id, metric_key, at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS log_lines (
            at         TIMESTAMPTZ NOT NULL,
            profile_id TEXT NOT NULL,
            source     TEXT NOT NULL,
            level      TEXT NOT NULL,
            stream     TEXT NOT NULL,
            message    TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS log_lines_profile ON log_lines (profile_id, at DESC)")
        .execute(pool)
        .await?;

    Ok(())
}

/// Turn the two big tables into hypertables and let Timescale look after them.
///
/// Every statement here is allowed to fail. On a plain Postgres the extension
/// is absent, all of this is a no-op, and the product still works — reads fall
/// back to aggregating the raw table and nothing is compressed or expired.
/// Returning `false` is a fact about the install, not an error in it.
async fn enable_timescale(pool: &PgPool) -> bool {
    if sqlx::query("CREATE EXTENSION IF NOT EXISTS timescaledb")
        .execute(pool)
        .await
        .is_err()
    {
        tracing::info!(
            "TimescaleDB is not available; history will be kept in plain Postgres \
             without compression or automatic rollup"
        );
        return false;
    }

    const HYPERTABLES: [(&str, &str); 2] = [
        (
            "metric_samples",
            "SELECT create_hypertable('metric_samples', 'at', \
             if_not_exists => TRUE, migrate_data => TRUE)",
        ),
        (
            "log_lines",
            "SELECT create_hypertable('log_lines', 'at', \
             if_not_exists => TRUE, migrate_data => TRUE)",
        ),
    ];

    for (table, sql) in HYPERTABLES {
        if let Err(e) = sqlx::query(sql).execute(pool).await {
            tracing::warn!("could not make {table} a hypertable: {e}");
            return false;
        }
    }

    // The hourly rollup, maintained by the database. This is the row that
    // outlives its own raw samples and the one long-range questions read.
    let aggregate = sqlx::query(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS metric_hourly
         WITH (timescaledb.continuous) AS
         SELECT time_bucket('1 hour', at) AS bucket,
                profile_id,
                metric_key,
                MIN(value)   AS min_value,
                MAX(value)   AS max_value,
                AVG(value)::double precision AS avg_value,
                COUNT(*)     AS sample_count
         FROM metric_samples
         GROUP BY bucket, profile_id, metric_key
         WITH NO DATA",
    )
    .execute(pool)
    .await;
    if let Err(e) = aggregate {
        tracing::warn!("could not create the hourly rollup: {e}");
        return false;
    }

    // Refresh the last three days on a twenty minute cadence. Anything older
    // than that has already been aggregated and does not change.
    let _ = sqlx::query(
        "SELECT add_continuous_aggregate_policy('metric_hourly',
             start_offset => INTERVAL '3 days',
             end_offset   => INTERVAL '1 hour',
             schedule_interval => INTERVAL '20 minutes',
             if_not_exists => TRUE)",
    )
    .execute(pool)
    .await;

    // Compression and retention, spelled out. The intervals are in the SQL
    // rather than interpolated from the constants above, because sqlx will not
    // take a built string and a policy that silently became `INTERVAL '0 days'`
    // would delete an estate's history rather than fail loudly.
    const POLICIES: [&str; 6] = [
        "ALTER TABLE metric_samples SET (timescaledb.compress,
             timescaledb.compress_segmentby = 'profile_id')",
        "ALTER TABLE log_lines SET (timescaledb.compress,
             timescaledb.compress_segmentby = 'profile_id')",
        "SELECT add_compression_policy('metric_samples', INTERVAL '7 days',
             if_not_exists => TRUE)",
        "SELECT add_compression_policy('log_lines', INTERVAL '7 days',
             if_not_exists => TRUE)",
        // Retention. The raw samples go; `metric_hourly` is untouched by this
        // and keeps the shape of every series for as long as the database
        // lives.
        "SELECT add_retention_policy('metric_samples', INTERVAL '30 days',
             if_not_exists => TRUE)",
        "SELECT add_retention_policy('log_lines', INTERVAL '7 days',
             if_not_exists => TRUE)",
    ];

    for sql in POLICIES {
        let _ = sqlx::query(sql).execute(pool).await;
    }

    true
}

/// How far back a long-range read should reach by default.
pub fn default_window() -> DateTime<Utc> {
    Utc::now() - Duration::days(RAW_METRIC_DAYS)
}
