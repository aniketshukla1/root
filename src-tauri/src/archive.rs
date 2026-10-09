//! The estate's history on this machine, in SQLite.
//!
//! `store.rs` keeps history in Postgres, and only for someone who runs one.
//! Everybody else got the ring buffer: a few hours, gone on restart. This is
//! the store nobody has to run — a file beside the config, opened whenever no
//! database is configured — so every install keeps thirty days and a restart
//! picks up where the last session stopped. Postgres stays the choice for a
//! team that wants history in one shared place.
//!
//! ## Three resolutions, one transaction
//!
//! Each sample lands in three tables at once: raw, and a one-minute and a
//! ten-minute rollup that each keep a running low, high, total and count.
//! Raw samples are kept a day, the minute rollup a week, the ten-minute rollup
//! thirty days — so a 24h chart draws every poll, 7d draws minutes and 30d
//! draws ten-minute buckets, and nothing anywhere needs a rollup job that has
//! to be right about restarts and late samples. Kept that way, a laptop-sized
//! estate's month stays far under a gigabyte (`a_month_fits_well_under_a_gigabyte`).
//!
//! ## It may never hold up a poll
//!
//! The same posture as `store.rs`: every write is best-effort and logged on
//! failure. A full disk is not an outage of the service being polled.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use sqlx::sqlite::{
    SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow,
    SqliteSynchronous,
};
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex;

use crate::error::Result;
use crate::history::{entity_readings, HealthSample, HistoryStore, Point};
use crate::model::{Alert, ChangeEvent, Health, HealthSnapshot, LogEvent, MetricUnit};
use crate::store::{BucketPoint, PastAlert, PastChange};

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// How long each resolution is kept.
const RAW_MS: i64 = DAY_MS;
const MINUTE_MS: i64 = 7 * DAY_MS;
const TEN_MINUTE_MS: i64 = 30 * DAY_MS;

/// Log lines: a day, and never more than this many — log volume varies by
/// orders of magnitude between estates, and the size promise must not.
const LOG_MS: i64 = DAY_MS;
const MAX_LOG_LINES: i64 = 500_000;

/// How often expired rows are deleted, at most.
const PRUNE_EVERY_MS: i64 = 10 * 60 * 1000;

/// How far back a restart refills the ring from. The ring holds 720 samples;
/// twelve hours covers that at a one-minute poll.
const RESTORE_MS: i64 = 12 * HOUR_MS;

/// Separates section from row in a component's identity. A control
/// character, so no section or row name can contain it.
const ENTITY_SEP: char = '\u{1f}';

/// (connection, component — empty for the connection itself, metric)
type SeriesKey = (String, String, String);

/// One value to record: (component, key, label, unit, limit, value).
type Reading<'a> = (String, &'a str, &'a str, MetricUnit, Option<f64>, f64);

/// A series' row id and the definition last written for it.
#[derive(Clone, PartialEq)]
struct Known {
    id: i64,
    label: String,
    unit: String,
    warn_above: Option<f64>,
}

#[derive(Clone)]
pub struct Archive {
    pool: SqlitePool,
    series: Arc<Mutex<HashMap<SeriesKey, Known>>>,
    pruned_at: Arc<AtomicI64>,
    path: Option<std::path::PathBuf>,
}

impl Archive {
    /// Open (creating if needed) the archive at `path`.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // Regenerable data: losing the last second to a power cut costs
            // less than an fsync on every poll, which the journal pays because
            // its contents are irreplaceable and these are not.
            .synchronous(SqliteSynchronous::Normal)
            // Deleted rows give their pages back, so pruning shrinks the file
            // rather than leaving a month-sized hole in it.
            .auto_vacuum(SqliteAutoVacuum::Incremental);
        let pool = SqlitePoolOptions::new().max_connections(4).connect_with(options).await?;
        migrate(&pool).await?;
        Ok(Archive {
            pool,
            series: Arc::default(),
            pruned_at: Arc::default(),
            path: Some(path.to_path_buf()),
        })
    }

    /// The file's size on disk, WAL included, for the Settings panel.
    pub fn bytes(&self) -> Option<u64> {
        let path = self.path.as_ref()?;
        let wal = path.with_extension("db-wal");
        let main = std::fs::metadata(path).ok()?.len();
        Some(main + std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0))
    }

    // ------------------------------------------------------------- writing

    /// Record every metric and component reading in one snapshot, and its
    /// health, in one transaction.
    pub async fn write_snapshot(&self, snapshot: &HealthSnapshot) -> Result<()> {
        let at = snapshot.checked_at.timestamp_millis();
        let entity_of = |section: &str, row: &str| format!("{section}{ENTITY_SEP}{row}");

        let mut readings: Vec<Reading> = snapshot
            .metrics
            .iter()
            // Not finite means not measured; a zero would draw a cliff that
            // nothing in the estate did.
            .filter(|m| m.value.is_finite())
            .map(|m| (String::new(), m.key.as_str(), m.label.as_str(), m.unit, m.warn_above, m.value))
            .collect();
        for r in entity_readings(snapshot) {
            readings.push((entity_of(r.section, r.row), r.column, r.label, r.unit, r.warn_above, r.value));
        }

        let mut tx = self.pool.begin().await?;
        for (entity, key, label, unit, warn_above, value) in &readings {
            let wanted = Known {
                id: 0,
                label: (*label).to_string(),
                unit: wire(unit),
                warn_above: *warn_above,
            };
            let id = self
                .series_id(&mut tx, (&snapshot.profile_id, entity, key), wanted)
                .await?;

            sqlx::query("INSERT OR REPLACE INTO samples (series_id, at, value) VALUES (?1, ?2, ?3)")
                .bind(id)
                .bind(at)
                .bind(value)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO minutes (series_id, at, lo, hi, total, n) VALUES (?1, ?2, ?3, ?3, ?3, 1)
                 ON CONFLICT (series_id, at) DO UPDATE SET
                     lo = MIN(lo, excluded.lo), hi = MAX(hi, excluded.hi),
                     total = total + excluded.total, n = n + 1",
            )
            .bind(id)
            .bind(at - at.rem_euclid(60_000))
            .bind(value)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO ten_minutes (series_id, at, lo, hi, total, n) VALUES (?1, ?2, ?3, ?3, ?3, 1)
                 ON CONFLICT (series_id, at) DO UPDATE SET
                     lo = MIN(lo, excluded.lo), hi = MAX(hi, excluded.hi),
                     total = total + excluded.total, n = n + 1",
            )
            .bind(id)
            .bind(at - at.rem_euclid(600_000))
            .bind(value)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query("INSERT OR REPLACE INTO health (profile_id, at, health) VALUES (?1, ?2, ?3)")
            .bind(&snapshot.profile_id)
            .bind(at)
            .bind(wire(&snapshot.health))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        self.maybe_prune(at).await;
        Ok(())
    }

    /// A series' id, creating it the first time and rewriting its label, unit
    /// or limit when a collector changes them — the newest definition wins,
    /// as it does in the ring.
    async fn series_id(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        (profile_id, entity, key): (&str, &str, &str),
        wanted: Known,
    ) -> Result<i64> {
        let cache_key = (profile_id.to_string(), entity.to_string(), key.to_string());
        if let Some(known) = self.series.lock().await.get(&cache_key) {
            if (Known { id: 0, ..known.clone() }) == wanted {
                return Ok(known.id);
            }
        }
        let row = sqlx::query(
            "INSERT INTO series (profile_id, entity, metric_key, label, unit, warn_above)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (profile_id, entity, metric_key) DO UPDATE SET
                 label = excluded.label, unit = excluded.unit, warn_above = excluded.warn_above
             RETURNING id",
        )
        .bind(profile_id)
        .bind(entity)
        .bind(key)
        .bind(&wanted.label)
        .bind(&wanted.unit)
        .bind(wanted.warn_above)
        .fetch_one(&mut **tx)
        .await?;
        let id: i64 = row.get("id");
        self.series
            .lock()
            .await
            .insert(cache_key, Known { id, ..wanted });
        Ok(id)
    }

    /// Record log lines, a day's worth at most.
    pub async fn write_logs(&self, events: &[LogEvent]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for event in events {
            sqlx::query(
                "INSERT INTO log_lines (at, profile_id, source, level, stream, message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(event.timestamp.timestamp_millis())
            .bind(&event.profile_id)
            .bind(&event.source)
            .bind(wire(&event.level))
            .bind(wire(&event.stream))
            .bind(&event.message)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Record an alert, or move the one already open — keyed as in Postgres,
    /// on the condition and when it started.
    pub async fn write_alert(&self, alert: &Alert, predicted: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO alerts (
                 fingerprint, first_seen, profile_id, profile_name, kind, severity,
                 title, detail, component, predicted, last_seen, resolved_at, acknowledged
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (fingerprint, first_seen) DO UPDATE SET
                 last_seen    = excluded.last_seen,
                 resolved_at  = excluded.resolved_at,
                 acknowledged = excluded.acknowledged,
                 severity     = excluded.severity,
                 detail       = excluded.detail",
        )
        .bind(&alert.fingerprint)
        .bind(alert.first_seen.timestamp_millis())
        .bind(&alert.profile_id)
        .bind(&alert.profile_name)
        .bind(wire(&alert.kind))
        .bind(wire(&alert.severity))
        .bind(&alert.title)
        .bind(&alert.detail)
        .bind(&alert.component)
        .bind(predicted)
        .bind(alert.last_seen.timestamp_millis())
        .bind((!alert.active).then_some(alert.last_seen.timestamp_millis()))
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
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&change.id)
        .bind(change.at.timestamp_millis())
        .bind(&change.profile_id)
        .bind(&change.profile_name)
        .bind(wire(&change.kind))
        .bind(wire(&change.change))
        .bind(wire(&change.source))
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

    /// One series since a moment, at the finest resolution still kept for a
    /// window that long: every sample up to a day, minutes up to a week,
    /// ten-minute buckets beyond.
    pub async fn buckets(
        &self,
        profile_id: &str,
        entity: &str,
        key: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<BucketPoint>> {
        let since_ms = since.timestamp_millis();
        let span = Utc::now().timestamp_millis() - since_ms;
        let Some(row) = sqlx::query(
            "SELECT id FROM series WHERE profile_id = ?1 AND entity = ?2 AND metric_key = ?3",
        )
        .bind(profile_id)
        .bind(entity)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(Vec::new());
        };
        let id: i64 = row.get("id");

        let rows = if span <= RAW_MS {
            sqlx::query(
                "SELECT at, value AS lo, value AS hi, value AS total, 1 AS n
                 FROM samples WHERE series_id = ?1 AND at >= ?2 ORDER BY at",
            )
            .bind(id)
            .bind(since_ms)
            .fetch_all(&self.pool)
            .await?
        } else if span <= MINUTE_MS {
            sqlx::query(
                "SELECT at, lo, hi, total, n FROM minutes
                 WHERE series_id = ?1 AND at >= ?2 ORDER BY at",
            )
            .bind(id)
            .bind(since_ms)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT at, lo, hi, total, n FROM ten_minutes
                 WHERE series_id = ?1 AND at >= ?2 ORDER BY at",
            )
            .bind(id)
            .bind(since_ms)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows.iter().map(bucket).collect())
    }

    /// The component buckets for one row, every numeric column of it.
    pub fn entity(section: &str, row: &str) -> String {
        format!("{section}{ENTITY_SEP}{row}")
    }

    /// Every time this condition has fired, newest first.
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
             WHERE profile_id = ?1 AND first_seen >= ?2 AND predicted = 0
             ORDER BY first_seen DESC
             LIMIT ?3",
        )
        .bind(profile_id)
        .bind(since.timestamp_millis())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(past_alert).collect())
    }

    /// How many times each condition fired over the window, worst first;
    /// a condition that fired once is not a pattern and is left out.
    pub async fn alert_frequency(&self, since: DateTime<Utc>) -> Result<Vec<(String, String, i64)>> {
        let rows = sqlx::query(
            "SELECT profile_name, title, times FROM (
                 SELECT profile_name, title,
                        COUNT(*) OVER (PARTITION BY profile_id, fingerprint) AS times,
                        ROW_NUMBER() OVER (PARTITION BY profile_id, fingerprint ORDER BY first_seen DESC) AS newest
                 FROM alerts
                 WHERE first_seen >= ?1 AND predicted = 0
             ) WHERE newest = 1 AND times > 1
             ORDER BY times DESC, title
             LIMIT 50",
        )
        .bind(since.timestamp_millis())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("profile_name"), r.get("title"), r.get("times")))
            .collect())
    }

    /// Changes since a moment, newest first. One connection, or the estate.
    pub async fn changes_since(
        &self,
        profile_id: Option<&str>,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<PastChange>> {
        let rows = match profile_id {
            Some(id) => {
                sqlx::query(
                    "SELECT id, at, profile_id, profile_name, kind, change, source,
                            section, component, from_value, to_value, summary
                     FROM changes WHERE profile_id = ?1 AND at >= ?2
                     ORDER BY at DESC LIMIT ?3",
                )
                .bind(id)
                .bind(since.timestamp_millis())
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, at, profile_id, profile_name, kind, change, source,
                            section, component, from_value, to_value, summary
                     FROM changes WHERE at >= ?1
                     ORDER BY at DESC LIMIT ?2",
                )
                .bind(since.timestamp_millis())
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.iter().map(past_change).collect())
    }

    /// Rows held, per table, under the same names the Postgres store reports.
    pub async fn counts(&self) -> Result<HashMap<String, i64>> {
        const QUERIES: [(&str, &str); 4] = [
            ("alerts", "SELECT COUNT(*) AS n FROM alerts"),
            ("changes", "SELECT COUNT(*) AS n FROM changes"),
            ("metric_samples", "SELECT COUNT(*) AS n FROM samples"),
            ("log_lines", "SELECT COUNT(*) AS n FROM log_lines"),
        ];
        let mut out = HashMap::new();
        for (table, sql) in QUERIES {
            let row = sqlx::query(sql).fetch_one(&self.pool).await?;
            out.insert(table.to_string(), row.get::<i64, _>("n"));
        }
        Ok(out)
    }

    /// Drop everything the estate recorded. The journal is a different file
    /// and is never reached from here.
    pub async fn clear(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for sql in [
            "DELETE FROM samples",
            "DELETE FROM minutes",
            "DELETE FROM ten_minutes",
            "DELETE FROM health",
            "DELETE FROM log_lines",
            "DELETE FROM alerts",
            "DELETE FROM changes",
            "DELETE FROM series",
        ] {
            sqlx::query(sql).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.series.lock().await.clear();
        let _ = sqlx::query("PRAGMA incremental_vacuum").execute(&self.pool).await;
        Ok(())
    }

    // ----------------------------------------------------- restart, expiry

    /// Refill the ring from disk: the last twelve hours of every series and
    /// every connection's health, so charts and lanes after a restart carry
    /// on rather than starting blank. Returns how many series came back.
    pub async fn restore(&self, history: &mut HistoryStore) -> Result<usize> {
        let since = Utc::now().timestamp_millis() - RESTORE_MS;
        let rows = sqlx::query(
            "SELECT s.id, s.profile_id, s.entity, s.metric_key, s.label, s.unit, s.warn_above,
                    x.at, x.value
             FROM samples x JOIN series s ON s.id = x.series_id
             WHERE x.at >= ?1
             ORDER BY s.id, x.at",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;

        let mut restored = 0;
        let mut i = 0;
        while i < rows.len() {
            let id: i64 = rows[i].get("id");
            let mut points = Vec::new();
            let mut j = i;
            while j < rows.len() && rows[j].get::<i64, _>("id") == id {
                points.push(Point {
                    t: millis(rows[j].get("at")),
                    v: rows[j].get("value"),
                });
                j += 1;
            }
            let row = &rows[i];
            let profile_id: String = row.get("profile_id");
            let entity: String = row.get("entity");
            let key: String = row.get("metric_key");
            let label: String = row.get("label");
            let unit: MetricUnit = parse(&row.get::<String, _>("unit")).unwrap_or(MetricUnit::Count);
            let warn_above: Option<f64> = row.get("warn_above");
            match entity.split_once(ENTITY_SEP) {
                None => history.restore_metric(&profile_id, &key, &label, unit, warn_above, points),
                Some((section, row_id)) => history.restore_entity(
                    &profile_id, section, row_id, &key, &label, unit, warn_above, points,
                ),
            }
            restored += 1;
            i = j;
        }

        let health = sqlx::query(
            "SELECT profile_id, at, health FROM health WHERE at >= ?1 ORDER BY profile_id, at",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        let mut by_profile: HashMap<String, Vec<HealthSample>> = HashMap::new();
        for row in &health {
            by_profile
                .entry(row.get("profile_id"))
                .or_default()
                .push(HealthSample {
                    t: millis(row.get("at")),
                    health: parse(&row.get::<String, _>("health")).unwrap_or(Health::Unknown),
                });
        }
        for (profile_id, samples) in by_profile {
            history.restore_health(&profile_id, samples);
        }
        Ok(restored)
    }

    async fn maybe_prune(&self, now: i64) {
        if now - self.pruned_at.load(Ordering::Relaxed) < PRUNE_EVERY_MS {
            return;
        }
        self.pruned_at.store(now, Ordering::Relaxed);
        if let Err(e) = self.prune(now).await {
            tracing::warn!("history prune failed: {e}");
        }
    }

    /// Delete what has outlived its resolution. Per series, so every delete is
    /// a range on the primary key rather than a scan of the table.
    pub(crate) async fn prune(&self, now: i64) -> Result<()> {
        let ids: Vec<i64> = sqlx::query("SELECT id FROM series")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| r.get("id"))
            .collect();
        for id in ids {
            sqlx::query("DELETE FROM samples WHERE series_id = ?1 AND at < ?2")
                .bind(id)
                .bind(now - RAW_MS)
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM minutes WHERE series_id = ?1 AND at < ?2")
                .bind(id)
                .bind(now - MINUTE_MS)
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM ten_minutes WHERE series_id = ?1 AND at < ?2")
                .bind(id)
                .bind(now - TEN_MINUTE_MS)
                .execute(&self.pool)
                .await?;
        }
        sqlx::query("DELETE FROM health WHERE at < ?1")
            .bind(now - RAW_MS)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM log_lines WHERE at < ?1")
            .bind(now - LOG_MS)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "DELETE FROM log_lines WHERE rowid IN
                 (SELECT rowid FROM log_lines ORDER BY at DESC LIMIT -1 OFFSET ?1)",
        )
        .bind(MAX_LOG_LINES)
        .execute(&self.pool)
        .await?;
        let _ = sqlx::query("PRAGMA incremental_vacuum").execute(&self.pool).await;
        Ok(())
    }
}

/// An enum's wire name, as serde writes it — the same rule as `store.rs`.
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn parse<T: serde::de::DeserializeOwned>(wire: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(wire.to_string())).ok()
}

fn millis(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

fn bucket(row: &SqliteRow) -> BucketPoint {
    let n: i64 = row.get("n");
    let total: f64 = row.get("total");
    BucketPoint {
        at: millis(row.get("at")),
        min: row.get("lo"),
        max: row.get("hi"),
        avg: total / n.max(1) as f64,
        count: n,
    }
}

fn past_alert(row: &SqliteRow) -> PastAlert {
    PastAlert {
        fingerprint: row.get("fingerprint"),
        profile_id: row.get("profile_id"),
        profile_name: row.get("profile_name"),
        severity: row.get("severity"),
        title: row.get("title"),
        detail: row.get("detail"),
        component: row.try_get("component").ok().flatten(),
        predicted: row.try_get("predicted").unwrap_or(false),
        first_seen: millis(row.get("first_seen")),
        last_seen: millis(row.get("last_seen")),
        resolved_at: row.try_get::<Option<i64>, _>("resolved_at").ok().flatten().map(millis),
    }
}

fn past_change(row: &SqliteRow) -> PastChange {
    PastChange {
        id: row.get("id"),
        at: millis(row.get("at")),
        profile_id: row.get("profile_id"),
        profile_name: row.get("profile_name"),
        kind: row.get("kind"),
        change: row.get("change"),
        source: row.get("source"),
        section: row.try_get("section").ok().flatten(),
        component: row.try_get("component").ok().flatten(),
        from: row.try_get("from_value").ok().flatten(),
        to: row.try_get("to_value").ok().flatten(),
        summary: row.get("summary"),
    }
}

/// Schema, applied on every open. `WITHOUT ROWID` on the sample tables: they
/// are looked up only by their key, and a separate rowid would cost a third of
/// their size for nothing.
async fn migrate(pool: &SqlitePool) -> Result<()> {
    for sql in [
        "CREATE TABLE IF NOT EXISTS series (
            id          INTEGER PRIMARY KEY,
            profile_id  TEXT NOT NULL,
            entity      TEXT NOT NULL,
            metric_key  TEXT NOT NULL,
            label       TEXT NOT NULL,
            unit        TEXT NOT NULL,
            warn_above  REAL,
            UNIQUE (profile_id, entity, metric_key)
        )",
        "CREATE TABLE IF NOT EXISTS samples (
            series_id INTEGER NOT NULL,
            at        INTEGER NOT NULL,
            value     REAL NOT NULL,
            PRIMARY KEY (series_id, at)
        ) WITHOUT ROWID",
        "CREATE TABLE IF NOT EXISTS minutes (
            series_id INTEGER NOT NULL,
            at        INTEGER NOT NULL,
            lo REAL NOT NULL, hi REAL NOT NULL, total REAL NOT NULL, n INTEGER NOT NULL,
            PRIMARY KEY (series_id, at)
        ) WITHOUT ROWID",
        "CREATE TABLE IF NOT EXISTS ten_minutes (
            series_id INTEGER NOT NULL,
            at        INTEGER NOT NULL,
            lo REAL NOT NULL, hi REAL NOT NULL, total REAL NOT NULL, n INTEGER NOT NULL,
            PRIMARY KEY (series_id, at)
        ) WITHOUT ROWID",
        "CREATE TABLE IF NOT EXISTS health (
            profile_id TEXT NOT NULL,
            at         INTEGER NOT NULL,
            health     TEXT NOT NULL,
            PRIMARY KEY (profile_id, at)
        ) WITHOUT ROWID",
        "CREATE TABLE IF NOT EXISTS log_lines (
            at         INTEGER NOT NULL,
            profile_id TEXT NOT NULL,
            source     TEXT NOT NULL,
            level      TEXT NOT NULL,
            stream     TEXT NOT NULL,
            message    TEXT NOT NULL
        )",
        "CREATE INDEX IF NOT EXISTS log_lines_at ON log_lines (at)",
        "CREATE TABLE IF NOT EXISTS alerts (
            fingerprint  TEXT NOT NULL,
            first_seen   INTEGER NOT NULL,
            profile_id   TEXT NOT NULL,
            profile_name TEXT NOT NULL,
            kind         TEXT NOT NULL,
            severity     TEXT NOT NULL,
            title        TEXT NOT NULL,
            detail       TEXT NOT NULL,
            component    TEXT,
            predicted    INTEGER NOT NULL DEFAULT 0,
            last_seen    INTEGER NOT NULL,
            resolved_at  INTEGER,
            acknowledged INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (fingerprint, first_seen)
        )",
        "CREATE INDEX IF NOT EXISTS alerts_profile ON alerts (profile_id, first_seen DESC)",
        "CREATE TABLE IF NOT EXISTS changes (
            id           TEXT PRIMARY KEY,
            at           INTEGER NOT NULL,
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
        "CREATE INDEX IF NOT EXISTS changes_at ON changes (at DESC)",
        "CREATE INDEX IF NOT EXISTS changes_profile_at ON changes (profile_id, at DESC)",
    ] {
        sqlx::query(sql).execute(pool).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellValue, ColumnSpec, Metric, Row as InventoryRow, Section, ServiceKind};
    use chrono::Duration;

    fn temp_path() -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("root-archive-{}", uuid::Uuid::new_v4()))
            .join("history.db")
    }

    fn snapshot(cpu: f64, at: DateTime<Utc>) -> HealthSnapshot {
        let mut containers = Section::new(
            "containers",
            "Containers",
            vec![ColumnSpec::text("name", "Name"), ColumnSpec::metric("cpu", "CPU", MetricUnit::Percent)],
        )
        .tracked();
        containers.rows = vec![InventoryRow::new("c1", "web-1", Health::Healthy)
            .cell("name", CellValue::text("web-1"))
            .cell("cpu", CellValue::number(cpu / 2.0))];
        HealthSnapshot {
            profile_id: "p1".into(),
            profile_name: "orders-api".into(),
            kind: ServiceKind::Docker,
            health: Health::Healthy,
            summary: String::new(),
            metrics: vec![Metric::new("cpu", "CPU", cpu, MetricUnit::Percent)],
            sections: vec![containers],
            checked_at: at,
            latency_ms: 10,
            error: None,
        }
    }

    #[tokio::test]
    async fn a_restart_loses_nothing() {
        let path = temp_path();
        let now = Utc::now();
        {
            let archive = Archive::open(&path).await.unwrap();
            for i in 0..30 {
                archive
                    .write_snapshot(&snapshot(10.0 + i as f64, now - Duration::seconds(10 * (30 - i))))
                    .await
                    .unwrap();
            }
        } // dropped, as on quit

        let reopened = Archive::open(&path).await.unwrap();
        let mut ring = HistoryStore::default();
        let restored = reopened.restore(&mut ring).await.unwrap();
        assert_eq!(restored, 2, "the connection's metric and the container's column");

        let series = ring.series("p1", None);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].points.len(), 30);
        assert_eq!(series[0].latest, 39.0);
        assert_eq!(series[0].label, "CPU");

        let container = ring.entity_series("p1", "containers", "c1", None);
        assert_eq!(container.len(), 1);
        assert_eq!(container[0].points.len(), 30);

        let lane = ring.health_runs("p1", now - Duration::minutes(5), now);
        assert!(lane.iter().any(|r| r.health == Health::Healthy), "{lane:?}");
    }

    #[tokio::test]
    async fn each_resolution_expires_on_its_own_schedule() {
        let archive = Archive::open(&temp_path()).await.unwrap();
        let now = Utc::now();
        for age in [Duration::hours(30), Duration::days(8), Duration::days(31)] {
            archive.write_snapshot(&snapshot(50.0, now - age)).await.unwrap();
        }
        archive.prune(now.timestamp_millis()).await.unwrap();

        // Raw keeps a day: the 30-hour-old sample is gone from it...
        let day = archive.buckets("p1", "", "cpu", now - Duration::hours(23)).await.unwrap();
        assert!(day.is_empty(), "{day:?}");
        // ...and lives on as a minute bucket, read for a two-day window.
        let two_days = archive.buckets("p1", "", "cpu", now - Duration::days(2)).await.unwrap();
        assert_eq!(two_days.len(), 1, "{two_days:?}");
        // The 8-day-old one is past the minute rollup, kept at ten minutes.
        let month = archive.buckets("p1", "", "cpu", now - Duration::days(29)).await.unwrap();
        assert_eq!(month.len(), 2, "30h and 8d survive at ten minutes; 31d is gone: {month:?}");
        let all = archive.buckets("p1", "", "cpu", now - Duration::days(60)).await.unwrap();
        assert_eq!(all.len(), 2, "nothing older than thirty days: {all:?}");
    }

    #[tokio::test]
    async fn a_long_window_reads_the_coarser_rollup() {
        let archive = Archive::open(&temp_path()).await.unwrap();
        // Six samples inside one minute, three hours ago.
        let base = Utc::now() - Duration::hours(3);
        let base = base - Duration::milliseconds(base.timestamp_millis().rem_euclid(60_000));
        for i in 0..6 {
            archive
                .write_snapshot(&snapshot(10.0 * (i + 1) as f64, base + Duration::seconds(10 * i)))
                .await
                .unwrap();
        }
        let raw = archive.buckets("p1", "", "cpu", Utc::now() - Duration::hours(12)).await.unwrap();
        assert_eq!(raw.len(), 6);
        let minutes = archive.buckets("p1", "", "cpu", Utc::now() - Duration::days(3)).await.unwrap();
        assert_eq!(minutes.len(), 1);
        assert_eq!((minutes[0].min, minutes[0].max, minutes[0].avg, minutes[0].count), (10.0, 60.0, 35.0, 6));
        // A component column is read the same way, by its own identity.
        let entity = Archive::entity("containers", "c1");
        let column = archive.buckets("p1", &entity, "cpu", Utc::now() - Duration::hours(12)).await.unwrap();
        assert_eq!(column.len(), 6);
    }

    /// The promise in the module note, measured: every series full to its
    /// retention at a 10-second poll, projected to 200 series — more than a
    /// laptop estate of a dozen connections and their containers holds.
    #[tokio::test]
    async fn a_month_fits_well_under_a_gigabyte() {
        let path = temp_path();
        let archive = Archive::open(&path).await.unwrap();
        let now = Utc::now().timestamp_millis();
        const SERIES: i64 = 10;
        let mut tx = archive.pool.begin().await.unwrap();
        for id in 1..=SERIES {
            sqlx::query("INSERT INTO series (id, profile_id, entity, metric_key, label, unit) VALUES (?1, 'p', '', ?2, 'x', 'percent')")
                .bind(id)
                .bind(format!("m{id}"))
                .execute(&mut *tx)
                .await
                .unwrap();
            for (table, step, span) in [
                ("samples", 10_000, RAW_MS),
                ("minutes", 60_000, MINUTE_MS),
                ("ten_minutes", 600_000, TEN_MINUTE_MS),
            ] {
                let ats: Vec<i64> = (0..span / step).map(|i| now - span + i * step).collect();
                for chunk in ats.chunks(400) {
                    let mut builder = sqlx::QueryBuilder::new(if table == "samples" {
                        "INSERT INTO samples (series_id, at, value) "
                    } else if table == "minutes" {
                        "INSERT INTO minutes (series_id, at, lo, hi, total, n) "
                    } else {
                        "INSERT INTO ten_minutes (series_id, at, lo, hi, total, n) "
                    });
                    builder.push_values(chunk, |mut row, at| {
                        let v = (*at % 997) as f64 / 7.0;
                        row.push_bind(id).push_bind(*at).push_bind(v);
                        if table != "samples" {
                            row.push_bind(v).push_bind(v * 6.0).push_bind(6_i64);
                        }
                    });
                    builder.build().execute(&mut *tx).await.unwrap();
                }
            }
        }
        tx.commit().await.unwrap();
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&archive.pool).await.unwrap();

        let bytes = archive.bytes().unwrap();
        let projected = bytes / SERIES as u64 * 200;
        eprintln!("archive: {SERIES} full series = {bytes} bytes; 200 series = {projected} bytes");
        assert!(projected < 1_000_000_000, "{SERIES} full series took {bytes} bytes, so 200 would take {projected}");
    }
}
