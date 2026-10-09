//! Rolling metric history and log bucketing — the data behind every chart.
//!
//! History is kept in memory only, like the log buffer. It is deliberately not
//! a time-series database: the point is to answer "what has this been doing for
//! the last while", both for the charts and for the assistant, without adding
//! storage or a retention policy an operator would have to manage.

use std::collections::HashMap;
use std::collections::VecDeque;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{Health, HealthSnapshot, LogEvent, LogLevel, MetricUnit};

/// Samples retained per metric. At a 15s poll this is roughly three hours;
/// at Docker's 5s default, about half an hour.
const MAX_POINTS: usize = 720;

/// Health samples retained per connection. Matched to `MAX_POINTS` so a lane
/// and a chart of the same connection always cover the same window.
const MAX_HEALTH: usize = 720;

/// How far apart two samples may sit before the span between them is reported
/// as unknown rather than assumed to continue. Inferred per connection from
/// its own cadence, because poll intervals differ by an order of magnitude
/// between collectors; this multiple only decides how much jitter to forgive.
const GAP_TOLERANCE: f64 = 2.5;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub t: DateTime<Utc>,
    pub v: f64,
}

/// One metric's history, carrying enough metadata for a chart to label and
/// format itself without consulting the live snapshot.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricSeries {
    pub key: String,
    pub label: String,
    pub unit: MetricUnit,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn_above: Option<f64>,
    pub points: Vec<Point>,
    /// Percentage change between the first and last sample, for a stat-tile
    /// delta. `None` when there is too little history, or when the baseline is
    /// zero and a percentage would be meaningless.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_pct: Option<f64>,
    pub min: f64,
    pub max: f64,
    pub latest: f64,
    /// Where this metric is heading, and when it reaches `warn_above`.
    ///
    /// Absent when the history is too short, too brief, or too noisy to
    /// extrapolate from — see `forecast`. Every metric that declares a limit
    /// gets this for free, which is the point: "when does this become a
    /// problem" is the same question about a disk, a connection pool and an
    /// inode table, and it should not be answered once per collector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend: Option<crate::forecast::Trend>,
}

/// Which component a capacity risk belongs to, when it is not the connection
/// itself. Ids only — the caller holds the live snapshot, which carries the
/// current labels; a second copy here would only let the two drift.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    pub section: String,
    pub row: String,
}

/// One measure heading toward a limit, anywhere in the estate.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityRisk {
    pub profile_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<Component>,
    pub key: String,
    pub label: String,
    pub unit: MetricUnit,
    pub value: f64,
    pub limit: f64,
    /// Fraction of the limit in use. Can exceed 1 — being over a threshold is
    /// a state to render, not a value to clamp away.
    pub used: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend: Option<crate::forecast::Trend>,
}

impl CapacityRisk {
    fn build(
        profile_id: &str,
        component: Option<Component>,
        key: &str,
        label: &str,
        unit: MetricUnit,
        warn_above: Option<f64>,
        points: &VecDeque<Point>,
    ) -> Option<Self> {
        // No declared limit means no headroom to report. A number with no
        // ceiling is not a warning waiting to happen, and inventing one would
        // be exactly the guess this is here to avoid.
        let limit = warn_above.filter(|l| *l > 0.0)?;
        let value = points.back()?.v;
        let series: Vec<Point> = points.iter().copied().collect();

        Some(CapacityRisk {
            profile_id: profile_id.to_string(),
            component,
            key: key.to_string(),
            label: label.to_string(),
            unit,
            value,
            limit,
            used: value / limit,
            trend: crate::forecast::trend(&series, Some(limit)),
        })
    }
}

#[derive(Debug, Clone)]
struct Track {
    label: String,
    unit: MetricUnit,
    warn_above: Option<f64>,
    points: VecDeque<Point>,
}

/// Most component entities one connection may track.
///
/// The ceiling exists so a section marked `tracked` that turns out to be
/// unbounded in the field cannot grow the store without limit. It is generous
/// for the intended cases — a busy Docker host runs ~25 containers, a large
/// Elasticsearch cluster ~30 nodes.
const MAX_TRACKED_ENTITIES: usize = 250;

/// Polls an entity may be absent before its history is dropped.
///
/// This is what answers the "containers come and go" objection: a container
/// that is removed stops consuming memory shortly after, while one that merely
/// restarts keeps its series across the gap.
const EVICT_AFTER_MISSES: u32 = 40;

/// Identifies one component's one numeric column within a connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EntityKey {
    section: String,
    row: String,
    column: String,
}

// The section and row *labels* are deliberately absent: the caller already has
// the live snapshot, which carries the current names. Storing a second copy
// here would only let the two drift when a collector renames something.
#[derive(Debug, Clone)]
struct EntityTrack {
    label: String,
    unit: MetricUnit,
    warn_above: Option<f64>,
    points: VecDeque<Point>,
    /// Consecutive polls this entity has not appeared in.
    misses: u32,
}

/// Median spacing between samples, in milliseconds. The median rather than the
/// mean so one long stall does not redefine a connection's normal cadence.
fn median_gap_ms(samples: &[HealthSample]) -> Option<f64> {
    if samples.len() < 2 {
        return None;
    }
    let mut gaps: Vec<f64> = samples
        .windows(2)
        .map(|w| (w[1].t - w[0].t).num_milliseconds() as f64)
        .filter(|ms| *ms > 0.0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(gaps[gaps.len() / 2])
}

/// Metric history for every connection.
#[derive(Default)]
pub struct HistoryStore {
    /// profile id → metric key → track
    tracks: HashMap<String, HashMap<String, Track>>,
    /// profile id → (section, row, column) → track. Only sections a collector
    /// marked `tracked` land here.
    entities: HashMap<String, HashMap<EntityKey, EntityTrack>>,
    /// profile id → one health sample per poll. Cheap enough to keep for every
    /// connection: health is a single byte and a timestamp.
    health: HashMap<String, VecDeque<HealthSample>>,
}

/// What a connection's health was at one poll.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSample {
    pub t: DateTime<Utc>,
    pub health: Health,
}

/// A period a connection spent in one state.
///
/// Runs are what a lane draws, and they are derived rather than stored: a gap
/// wider than the connection's own cadence becomes an `Unknown` run instead of
/// silently extending whatever came before it. That distinction is the whole
/// point — a collector nobody could reach is not a service that was fine.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthRun {
    pub health: Health,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// One tracked component's numeric cell in a snapshot.
///
/// The ring and the on-disk archive both record exactly these, so the two can
/// never disagree about which component readings exist.
pub(crate) struct EntityReading<'a> {
    pub section: &'a str,
    pub row: &'a str,
    pub column: &'a str,
    pub label: &'a str,
    pub unit: MetricUnit,
    pub warn_above: Option<f64>,
    pub value: f64,
}

/// Every finite number or gauge in a snapshot's tracked sections.
pub(crate) fn entity_readings(snapshot: &HealthSnapshot) -> Vec<EntityReading<'_>> {
    let mut out = Vec::new();
    for section in snapshot.sections.iter().filter(|s| s.tracked) {
        let numeric: Vec<&crate::model::ColumnSpec> = section
            .columns
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    crate::model::CellKind::Metric { .. } | crate::model::CellKind::Gauge { .. }
                )
            })
            .collect();

        for row in &section.rows {
            for column in &numeric {
                let Some(cell) = row.cells.get(&column.key) else {
                    continue;
                };
                let (value, warn_above) = match cell {
                    crate::model::CellValue::Number { value } => (*value, None),
                    crate::model::CellValue::Gauge {
                        value, warn_above, ..
                    } => (*value, *warn_above),
                    _ => continue,
                };
                if !value.is_finite() {
                    continue;
                }
                out.push(EntityReading {
                    section: &section.id,
                    row: &row.id,
                    column: &column.key,
                    label: &column.label,
                    unit: column.unit().unwrap_or(MetricUnit::Count),
                    warn_above,
                    value,
                });
            }
        }
    }
    out
}

/// The newest `MAX_POINTS` of a run of samples, as a ring holds them.
fn tail(points: Vec<Point>, cap: usize) -> VecDeque<Point> {
    let skip = points.len().saturating_sub(cap);
    points.into_iter().skip(skip).collect()
}

impl HistoryStore {
    /// Put a connection metric's samples, read back from disk, into the ring —
    /// so a session starts where the last one stopped instead of blank.
    pub fn restore_metric(
        &mut self,
        profile_id: &str,
        key: &str,
        label: &str,
        unit: MetricUnit,
        warn_above: Option<f64>,
        points: Vec<Point>,
    ) {
        self.tracks.entry(profile_id.to_string()).or_default().insert(
            key.to_string(),
            Track {
                label: label.to_string(),
                unit,
                warn_above,
                points: tail(points, MAX_POINTS),
            },
        );
    }

    /// The same, for one component's one column.
    #[allow(clippy::too_many_arguments)]
    pub fn restore_entity(
        &mut self,
        profile_id: &str,
        section: &str,
        row: &str,
        column: &str,
        label: &str,
        unit: MetricUnit,
        warn_above: Option<f64>,
        points: Vec<Point>,
    ) {
        let entities = self.entities.entry(profile_id.to_string()).or_default();
        if entities.len() >= MAX_TRACKED_ENTITIES {
            return;
        }
        entities.insert(
            EntityKey {
                section: section.to_string(),
                row: row.to_string(),
                column: column.to_string(),
            },
            EntityTrack {
                label: label.to_string(),
                unit,
                warn_above,
                points: tail(points, MAX_POINTS),
                misses: 0,
            },
        );
    }

    /// And a connection's health samples, so its lane does not open on a gap.
    pub fn restore_health(&mut self, profile_id: &str, samples: Vec<HealthSample>) {
        let skip = samples.len().saturating_sub(MAX_HEALTH);
        self.health
            .insert(profile_id.to_string(), samples.into_iter().skip(skip).collect());
    }
    /// Fold one snapshot's service-level metrics into the history.
    ///
    /// Component-level metrics used to be skipped entirely, on the grounds that
    /// containers and jobs come and go so the cardinality is unbounded. That
    /// objection is answered rather than avoided now: a collector opts a
    /// section in with `Section::tracked` only when its entities are bounded
    /// and long-lived, the store caps how many it will admit, and an entity
    /// that stops appearing is evicted. See [`HistoryStore::record_entities`].
    pub fn record(&mut self, snapshot: &HealthSnapshot) {
        let now = snapshot.checked_at;
        let profile = self.tracks.entry(snapshot.profile_id.clone()).or_default();

        for metric in &snapshot.metrics {
            let track = profile.entry(metric.key.clone()).or_insert_with(|| Track {
                label: metric.label.clone(),
                unit: metric.unit,
                warn_above: metric.warn_above,
                points: VecDeque::with_capacity(64),
            });

            // A collector may retitle or re-threshold a metric between
            // versions; the newest definition wins.
            track.label = metric.label.clone();
            track.unit = metric.unit;
            track.warn_above = metric.warn_above;

            if track.points.len() >= MAX_POINTS {
                track.points.pop_front();
            }
            track.points.push_back(Point {
                t: now,
                v: metric.value,
            });
        }

        self.record_entities(snapshot);
        self.record_health(snapshot);
    }

    /// One sample per poll, so a lane can be drawn from observation rather than
    /// inferred from whatever happened to raise an alert.
    fn record_health(&mut self, snapshot: &HealthSnapshot) {
        let ring = self.health.entry(snapshot.profile_id.clone()).or_default();
        if ring.len() >= MAX_HEALTH {
            ring.pop_front();
        }
        ring.push_back(HealthSample {
            t: snapshot.checked_at,
            health: snapshot.health,
        });
    }

    /// Collapse a connection's samples into runs across `[since, now]`.
    ///
    /// Three things are deliberate. A window that begins before the first
    /// sample opens with an `Unknown` run rather than back-filling the earliest
    /// state, because nothing was observed then. A gap wider than the
    /// connection's own median cadence becomes `Unknown` for exactly its
    /// duration. And the final run is closed at `now`, not at the last sample,
    /// so a connection that stopped reporting shows the silence rather than
    /// ending tidily.
    pub fn health_runs(
        &self,
        profile_id: &str,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Vec<HealthRun> {
        let Some(ring) = self.health.get(profile_id) else {
            return vec![HealthRun {
                health: Health::Unknown,
                from: since,
                to: now,
            }];
        };

        let samples: Vec<HealthSample> = ring.iter().copied().filter(|s| s.t >= since).collect();
        if samples.is_empty() {
            return vec![HealthRun {
                health: Health::Unknown,
                from: since,
                to: now,
            }];
        }

        let tolerance = Duration::milliseconds(
            (median_gap_ms(&samples).unwrap_or(60_000.0) * GAP_TOLERANCE) as i64,
        );

        let mut runs: Vec<HealthRun> = Vec::new();
        let push = |health: Health, from: DateTime<Utc>, to: DateTime<Utc>, runs: &mut Vec<HealthRun>| {
            if to <= from {
                return;
            }
            match runs.last_mut() {
                Some(last) if last.health == health && last.to >= from => last.to = to,
                _ => runs.push(HealthRun { health, from, to }),
            }
        };

        if samples[0].t > since {
            push(Health::Unknown, since, samples[0].t, &mut runs);
        }

        for (i, sample) in samples.iter().enumerate() {
            let next = samples.get(i + 1).map(|s| s.t).unwrap_or(now);
            if next - sample.t > tolerance {
                // Hold the observed state for one cadence, then admit we stopped
                // knowing rather than extending it across the silence.
                push(sample.health, sample.t, sample.t + tolerance, &mut runs);
                push(Health::Unknown, sample.t + tolerance, next, &mut runs);
            } else {
                push(sample.health, sample.t, next, &mut runs);
            }
        }

        runs
    }

    /// Per-row history for sections a collector marked `tracked`.
    ///
    /// Only numeric cells are recorded — a text state or a tag list has no
    /// series. Everything else about a component is still readable live from
    /// the snapshot; this is only about what can be charted over time.
    fn record_entities(&mut self, snapshot: &HealthSnapshot) {
        // Nothing to do, and nothing to expire against either — a collector
        // that reports no tracked sections this cycle (an error snapshot, say)
        // must not evict everything it recorded before.
        if !snapshot.sections.iter().any(|s| s.tracked) {
            return;
        }

        let now = snapshot.checked_at;
        let entities = self.entities.entry(snapshot.profile_id.clone()).or_default();
        let mut seen: Vec<EntityKey> = Vec::new();

        for reading in entity_readings(snapshot) {
            let key = EntityKey {
                section: reading.section.to_string(),
                row: reading.row.to_string(),
                column: reading.column.to_string(),
            };

            // Admit new entities only under the ceiling. Existing ones keep
            // recording, so a cluster that outgrows the cap degrades by
            // stopping at the edge rather than thrashing.
            if !entities.contains_key(&key) && entities.len() >= MAX_TRACKED_ENTITIES {
                continue;
            }

            let track = entities.entry(key.clone()).or_insert_with(|| EntityTrack {
                label: reading.label.to_string(),
                unit: reading.unit,
                warn_above: reading.warn_above,
                points: VecDeque::with_capacity(32),
                misses: 0,
            });

            track.label = reading.label.to_string();
            track.unit = reading.unit;
            track.warn_above = reading.warn_above;
            track.misses = 0;

            if track.points.len() >= MAX_POINTS {
                track.points.pop_front();
            }
            track.points.push_back(Point { t: now, v: reading.value });
            seen.push(key);
        }

        // Age out anything absent this cycle, and drop it once it has been gone
        // long enough to be genuinely removed rather than restarting.
        for (key, track) in entities.iter_mut() {
            if !seen.contains(key) {
                track.misses = track.misses.saturating_add(1);
            }
        }
        entities.retain(|_, track| track.misses < EVICT_AFTER_MISSES);
    }

    /// Series for one component, newest last.
    pub fn entity_series(
        &self,
        profile_id: &str,
        section_id: &str,
        row_id: &str,
        since: Option<DateTime<Utc>>,
    ) -> Vec<MetricSeries> {
        let Some(entities) = self.entities.get(profile_id) else {
            return Vec::new();
        };

        let mut series: Vec<MetricSeries> = entities
            .iter()
            .filter(|(key, _)| key.section == section_id && key.row == row_id)
            .map(|(key, track)| {
                let points: Vec<Point> = track
                    .points
                    .iter()
                    .filter(|p| since.is_none_or(|cutoff| p.t >= cutoff))
                    .copied()
                    .collect();

                let latest = points.last().map(|p| p.v).unwrap_or(0.0);
                let first = points.first().map(|p| p.v).unwrap_or(0.0);
                let min = points.iter().map(|p| p.v).fold(f64::INFINITY, f64::min);
                let max = points.iter().map(|p| p.v).fold(f64::NEG_INFINITY, f64::max);

                MetricSeries {
                    key: key.column.clone(),
                    label: track.label.clone(),
                    unit: track.unit,
                    warn_above: track.warn_above,
                    change_pct: if points.len() >= 2 && first.abs() > f64::EPSILON {
                        Some((latest - first) / first.abs() * 100.0)
                    } else {
                        None
                    },
                    min: if min.is_finite() { min } else { 0.0 },
                    max: if max.is_finite() { max } else { 0.0 },
                    latest,
                    trend: crate::forecast::trend(&points, track.warn_above),
                    points,
                }
            })
            .collect();

        series.sort_by(|a, b| a.key.cmp(&b.key));
        series
    }

    /// Everything in the estate that has a declared limit, ranked by when it
    /// reaches it.
    ///
    /// This exists because the interesting capacity answer is almost never at
    /// connection level. An NFS connection reports "fullest export 71%"; what
    /// the storage owner needs is *which* export, whose team owns it, and how
    /// many days are left. Same for the one container on a busy host whose
    /// volume is climbing, or the single hot node in a cluster. Those live in
    /// the tracked entity series, one row deep, and no screen that only reads
    /// connection metrics can see them.
    ///
    /// Ranking happens here rather than in the client for one reason: the
    /// client would have to pull every entity's history for every connection
    /// to do it, which is the whole store over IPC to compute a list of six.
    ///
    /// Deliberately ignores any caller window and fits over the full retained
    /// history. The window on the metrics screen is a viewing choice; a fit is
    /// a measurement, and a fifteen-minute view cannot see a disk three weeks
    /// out. There is only ever one right answer to "when does this fill", and
    /// it does not change because somebody clicked 1h.
    pub fn capacity_risks(&self, take: usize) -> Vec<CapacityRisk> {
        let mut out: Vec<CapacityRisk> = Vec::new();

        for (profile_id, tracks) in &self.tracks {
            for (key, track) in tracks {
                if let Some(risk) =
                    CapacityRisk::build(profile_id, None, key, &track.label, track.unit,
                                        track.warn_above, &track.points)
                {
                    out.push(risk);
                }
            }
        }

        for (profile_id, entities) in &self.entities {
            for (key, track) in entities {
                let component = Some(Component {
                    section: key.section.clone(),
                    row: key.row.clone(),
                });
                if let Some(risk) =
                    CapacityRisk::build(profile_id, component, &key.column, &track.label,
                                        track.unit, track.warn_above, &track.points)
                {
                    out.push(risk);
                }
            }
        }

        // Soonest first; anything with no projection falls back to how full it
        // is, which is all that can be said about it. A row at 80% that has
        // not moved in a month needs nobody, and one at 40% doubling weekly
        // needs somebody this week — so fill is the tiebreak, never the key.
        out.sort_by(|a, b| {
            match (
                a.trend.as_ref().and_then(|t| t.seconds_to_limit),
                b.trend.as_ref().and_then(|t| t.seconds_to_limit),
            ) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b.used.total_cmp(&a.used),
            }
        });
        out.truncate(take);
        out
    }

    /// How many component series this connection is holding, for diagnostics.
    pub fn entity_count(&self, profile_id: &str) -> usize {
        self.entities.get(profile_id).map_or(0, |e| e.len())
    }

    /// Series for one connection, newest last, optionally limited to a window.
    pub fn series(&self, profile_id: &str, since: Option<DateTime<Utc>>) -> Vec<MetricSeries> {
        let Some(profile) = self.tracks.get(profile_id) else {
            return Vec::new();
        };

        let mut series: Vec<MetricSeries> = profile
            .iter()
            .map(|(key, track)| {
                let points: Vec<Point> = track
                    .points
                    .iter()
                    .filter(|p| since.is_none_or(|cutoff| p.t >= cutoff))
                    .copied()
                    .collect();

                let latest = points.last().map(|p| p.v).unwrap_or(0.0);
                let first = points.first().map(|p| p.v).unwrap_or(0.0);
                let min = points.iter().map(|p| p.v).fold(f64::INFINITY, f64::min);
                let max = points.iter().map(|p| p.v).fold(f64::NEG_INFINITY, f64::max);

                MetricSeries {
                    key: key.clone(),
                    label: track.label.clone(),
                    unit: track.unit,
                    warn_above: track.warn_above,
                    // A single point is a reading, not a trend.
                    change_pct: if points.len() >= 2 && first.abs() > f64::EPSILON {
                        Some((latest - first) / first.abs() * 100.0)
                    } else {
                        None
                    },
                    min: if min.is_finite() { min } else { 0.0 },
                    max: if max.is_finite() { max } else { 0.0 },
                    latest,
                    trend: crate::forecast::trend(&points, track.warn_above),
                    points,
                }
            })
            .collect();

        series.sort_by(|a, b| a.key.cmp(&b.key));
        series
    }

    pub fn drop_profile(&mut self, profile_id: &str) {
        self.health.remove(profile_id);
        self.tracks.remove(profile_id);
        self.entities.remove(profile_id);
    }

    pub fn clear(&mut self) {
        self.health.clear();
        self.tracks.clear();
        self.entities.clear();
    }
}

/// One time bucket of log volume, split by severity.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistogramBucket {
    pub t: DateTime<Utc>,
    pub trace: u32,
    pub debug: u32,
    pub info: u32,
    pub warn: u32,
    pub error: u32,
    pub fatal: u32,
    pub total: u32,
    /// Counts per connection, so the same buckets can be stacked by severity
    /// ("how bad") or by source ("who is making the noise") without a second
    /// round trip.
    pub by_profile: HashMap<String, u32>,
}

impl HistogramBucket {
    fn empty(t: DateTime<Utc>) -> Self {
        HistogramBucket {
            t,
            trace: 0,
            debug: 0,
            info: 0,
            warn: 0,
            error: 0,
            fatal: 0,
            total: 0,
            by_profile: HashMap::new(),
        }
    }

    fn add(&mut self, event: &LogEvent) {
        let level = event.level;
        *self
            .by_profile
            .entry(event.profile_name.clone())
            .or_insert(0) += 1;
        match level {
            LogLevel::Trace => self.trace += 1,
            LogLevel::Debug => self.debug += 1,
            LogLevel::Info => self.info += 1,
            LogLevel::Warn => self.warn += 1,
            LogLevel::Error => self.error += 1,
            LogLevel::Fatal => self.fatal += 1,
        }
        self.total += 1;
    }
}

/// Bucket log events into a fixed number of equal time slots spanning
/// `[since, now]`.
///
/// Empty buckets are emitted rather than skipped: a gap in log volume is
/// information, and a chart that silently closes gaps misrepresents a service
/// that went quiet.
pub fn histogram(
    events: &[LogEvent],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    buckets: usize,
) -> Vec<HistogramBucket> {
    let buckets = buckets.clamp(1, 500);
    let span = (until - since).num_milliseconds().max(1);
    let width = (span as f64 / buckets as f64).max(1.0);

    let mut out: Vec<HistogramBucket> = (0..buckets)
        .map(|i| {
            HistogramBucket::empty(
                since + Duration::milliseconds((i as f64 * width).round() as i64),
            )
        })
        .collect();

    for event in events {
        if event.timestamp < since || event.timestamp > until {
            continue;
        }
        let offset = (event.timestamp - since).num_milliseconds() as f64;
        // The final instant maps to the last bucket rather than one past the end.
        let index = ((offset / width) as usize).min(buckets - 1);
        out[index].add(event);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Health, Metric, MetricUnit, ServiceKind};

    fn snapshot(cpu: f64, at: DateTime<Utc>) -> HealthSnapshot {
        HealthSnapshot {
            profile_id: "p1".into(),
            profile_name: "docker".into(),
            kind: ServiceKind::Docker,
            health: Health::Healthy,
            summary: String::new(),
            metrics: vec![Metric::new("cpu", "CPU", cpu, MetricUnit::Percent).warn_above(85.0)],
            sections: Vec::new(),
            checked_at: at,
            latency_ms: 1,
            error: None,
        }
    }


    // ------------------------------------------------------------ health runs

    fn health_snapshot(health: Health, at: DateTime<Utc>) -> HealthSnapshot {
        let mut snap = snapshot(10.0, at);
        snap.health = health;
        snap
    }

    /// A steady poll produces one run per state, not one per sample.
    #[test]
    fn health_runs_collapse_steady_polling() {
        let start = Utc::now() - Duration::minutes(10);
        let mut store = HistoryStore::default();
        for i in 0..20 {
            let health = if i < 12 { Health::Healthy } else { Health::Down };
            store.record(&health_snapshot(health, start + Duration::seconds(i * 15)));
        }

        let runs = store.health_runs("p1", start, start + Duration::seconds(20 * 15));
        assert_eq!(runs.len(), 2, "expected two runs, got {runs:?}");
        assert_eq!(runs[0].health, Health::Healthy);
        assert_eq!(runs[1].health, Health::Down);
    }

    /// A window that opens before the first sample opens as unknown: nothing
    /// was observed then, and back-filling would invent an observation.
    #[test]
    fn health_runs_do_not_backfill_before_the_first_sample() {
        let start = Utc::now() - Duration::minutes(30);
        let mut store = HistoryStore::default();
        for i in 0..10 {
            store.record(&health_snapshot(
                Health::Healthy,
                start + Duration::minutes(20) + Duration::seconds(i * 15),
            ));
        }

        let runs = store.health_runs("p1", start, Utc::now());
        assert_eq!(runs[0].health, Health::Unknown);
        assert_eq!(runs[0].from, start);
    }

    /// A collector that stops reporting shows the silence. This is the one that
    /// matters: extending the last known state across a gap would claim an
    /// observation nobody made.
    #[test]
    fn health_runs_report_a_gap_as_unknown() {
        let start = Utc::now() - Duration::minutes(60);
        let mut store = HistoryStore::default();
        for i in 0..10 {
            store.record(&health_snapshot(Health::Healthy, start + Duration::seconds(i * 15)));
        }
        // then nothing for twenty minutes
        let now = start + Duration::minutes(20);

        let runs = store.health_runs("p1", start, now);
        let last = runs.last().expect("a run");
        assert_eq!(last.health, Health::Unknown, "runs were {runs:?}");
        assert_eq!(last.to, now, "the final run must close at now, not the last sample");
        assert!(
            runs.iter().any(|r| r.health == Health::Healthy),
            "the observed period should survive: {runs:?}",
        );
    }

    /// A connection the store has never seen is unknown for the whole window,
    /// rather than absent from the result.
    #[test]
    fn health_runs_for_an_unseen_profile_are_unknown() {
        let store = HistoryStore::default();
        let now = Utc::now();
        let runs = store.health_runs("nope", now - Duration::minutes(60), now);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].health, Health::Unknown);
    }

    /// Cadence is per connection: a five-second poll must not be judged against
    /// a fifteen-second one, or every fast collector looks full of holes.
    #[test]
    fn health_runs_infer_cadence_per_connection() {
        let start = Utc::now() - Duration::minutes(10);
        let mut store = HistoryStore::default();
        for i in 0..40 {
            store.record(&health_snapshot(Health::Healthy, start + Duration::seconds(i * 5)));
        }

        let runs = store.health_runs("p1", start, start + Duration::seconds(40 * 5));
        assert_eq!(runs.len(), 1, "a steady 5s poll is not a gap: {runs:?}");
        assert_eq!(runs[0].health, Health::Healthy);
    }

    // ------------------------------------------------------- capacity ranking

    /// `count` samples 30s apart, ramping linearly from `from` to `to`. The
    /// span clears the forecaster's fifteen-minute floor at count >= 32.
    fn ramp(store: &mut HistoryStore, profile: &str, from: f64, to: f64, count: i64) {
        let start = Utc::now() - Duration::seconds(count * 30);
        for i in 0..count {
            let progress = i as f64 / (count - 1) as f64;
            let mut snap = snapshot(from + (to - from) * progress, start + Duration::seconds(i * 30));
            snap.profile_id = profile.to_string();
            store.record(&snap);
        }
    }

    /// The whole reason this ranking exists: a metric at 40% climbing fast is
    /// a bigger problem than one parked at 80%, and sorting by fill gets that
    /// exactly backwards.
    #[test]
    fn a_fast_climb_outranks_a_fuller_metric_that_is_not_moving() {
        let mut store = HistoryStore::default();
        ramp(&mut store, "climbing", 10.0, 40.0, 40);
        ramp(&mut store, "parked", 80.0, 80.0, 40);

        let risks = store.capacity_risks(10);
        assert_eq!(risks.len(), 2);
        assert_eq!(risks[0].profile_id, "climbing");
        assert!(risks[0].trend.unwrap().seconds_to_limit.is_some());
        assert_eq!(risks[1].profile_id, "parked");
        assert!(
            risks[1].trend.unwrap().seconds_to_limit.is_none(),
            "a flat metric never reaches its limit",
        );
    }

    /// Component series are the point of the estate-wide walk — an NFS export
    /// or one container's volume is where the actionable capacity answer
    /// lives, and no connection-level metric can name it.
    #[test]
    fn tracked_components_are_ranked_alongside_connection_metrics() {
        let mut store = HistoryStore::default();
        let start = Utc::now() - Duration::seconds(40 * 30);
        for i in 0..40 {
            let cpu = 10.0 + (i as f64) * 1.8;
            store.record(&with_containers(&["api"], cpu, start + Duration::seconds(i * 30), true));
        }

        let risks = store.capacity_risks(10);
        let component = risks
            .iter()
            .find(|r| r.component.is_some())
            .expect("the tracked container should be ranked");
        let at = component.component.as_ref().unwrap();
        assert_eq!(at.section, "containers");
        assert_eq!(at.row, "api");
        assert_eq!(component.key, "cpu");
        assert_eq!(component.limit, 85.0);
    }

    /// An untracked section has no series behind it, so it cannot be forecast
    /// and must not appear as though it had been.
    #[test]
    fn untracked_components_are_absent() {
        let mut store = HistoryStore::default();
        let start = Utc::now() - Duration::seconds(40 * 30);
        for i in 0..40 {
            let cpu = 10.0 + (i as f64) * 1.8;
            store.record(&with_containers(&["api"], cpu, start + Duration::seconds(i * 30), false));
        }

        assert!(store.capacity_risks(10).iter().all(|r| r.component.is_none()));
    }

    /// A metric with no declared threshold has no headroom to report, and
    /// inventing one would be a guess dressed as a measurement.
    #[test]
    fn a_metric_without_a_limit_has_no_capacity_answer() {
        let mut store = HistoryStore::default();
        let start = Utc::now() - Duration::seconds(40 * 30);
        for i in 0..40 {
            let mut snap = snapshot(10.0, start + Duration::seconds(i * 30));
            snap.metrics = vec![Metric::new("conns", "Connections", i as f64, MetricUnit::Count)];
            store.record(&snap);
        }

        assert!(store.capacity_risks(10).is_empty());
    }

    /// Being over the threshold is a state to report, not a value to clamp —
    /// and it is already there, not arriving at some point in the past.
    #[test]
    fn a_metric_past_its_limit_reports_no_time_left() {
        let mut store = HistoryStore::default();
        ramp(&mut store, "over", 80.0, 95.0, 40);

        let risks = store.capacity_risks(10);
        assert_eq!(risks[0].used, 95.0 / 85.0);
        assert_eq!(risks[0].trend.unwrap().seconds_to_limit, Some(0.0));
    }

    #[test]
    fn the_ranking_is_bounded_by_take() {
        let mut store = HistoryStore::default();
        for i in 0..5 {
            ramp(&mut store, &format!("p{i}"), 10.0, 40.0 + i as f64, 40);
        }
        assert_eq!(store.capacity_risks(3).len(), 3);
    }

    fn event(level: LogLevel, at: DateTime<Utc>) -> LogEvent {
        LogEvent::new("p1", "docker", ServiceKind::Docker, "api", level, "m".into(), at)
    }

    /// A snapshot carrying one section of containers, each with a CPU gauge.
    fn with_containers(
        names: &[&str],
        cpu: f64,
        at: DateTime<Utc>,
        tracked: bool,
    ) -> HealthSnapshot {
        use crate::model::{CellValue, ColumnSpec, Row, Section};

        let rows: Vec<Row> = names
            .iter()
            .map(|name| {
                let mut cells = std::collections::HashMap::new();
                cells.insert(
                    "cpu".to_string(),
                    CellValue::Gauge {
                        value: cpu,
                        max: Some(100.0),
                        warn_above: Some(85.0),
                    },
                );
                // Text cells must be ignored by the recorder.
                cells.insert("state".to_string(), CellValue::text("running"));
                Row {
                    id: (*name).to_string(),
                    name: (*name).to_string(),
                    health: Health::Healthy,
                    summary: String::new(),
                    cells,
                    actions: vec![],
                    extra: serde_json::Value::Null,
                }
            })
            .collect();

        let mut section = Section::new(
            "containers",
            "Containers",
            vec![
                ColumnSpec::text("state", "State"),
                ColumnSpec::gauge("cpu", "CPU", MetricUnit::Percent),
            ],
        )
        .rows(rows);
        if tracked {
            section = section.tracked();
        }

        let mut snapshot = snapshot(1.0, at);
        snapshot.sections = vec![section];
        snapshot
    }

    #[test]
    fn untracked_sections_record_no_component_history() {
        let mut store = HistoryStore::default();
        store.record(&with_containers(&["api"], 10.0, Utc::now(), false));

        assert_eq!(store.entity_count("p1"), 0);
        assert!(store.entity_series("p1", "containers", "api", None).is_empty());
    }

    #[test]
    fn tracked_sections_record_numeric_cells_only() {
        let mut store = HistoryStore::default();
        let now = Utc::now();
        store.record(&with_containers(&["api"], 10.0, now - Duration::seconds(15), true));
        store.record(&with_containers(&["api"], 20.0, now, true));

        // The text `state` column must not have produced a series.
        assert_eq!(store.entity_count("p1"), 1);

        let series = store.entity_series("p1", "containers", "api", None);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].key, "cpu");
        assert_eq!(series[0].warn_above, Some(85.0));
        assert_eq!(series[0].points.len(), 2);
        assert_eq!(series[0].latest, 20.0);
    }

    #[test]
    fn a_component_that_stops_appearing_is_evicted() {
        let mut store = HistoryStore::default();
        let now = Utc::now();
        store.record(&with_containers(&["api", "worker"], 10.0, now, true));
        assert_eq!(store.entity_count("p1"), 2);

        // `worker` is gone, but not yet for long enough.
        for i in 1..EVICT_AFTER_MISSES {
            store.record(&with_containers(&["api"], 10.0, now + Duration::seconds(i as i64), true));
        }
        assert_eq!(store.entity_count("p1"), 2, "evicted too eagerly");

        store.record(&with_containers(
            &["api"],
            10.0,
            now + Duration::seconds(EVICT_AFTER_MISSES as i64),
            true,
        ));
        assert_eq!(store.entity_count("p1"), 1);
        assert!(store.entity_series("p1", "containers", "worker", None).is_empty());
        // The survivor keeps its history across the whole run.
        assert!(!store.entity_series("p1", "containers", "api", None).is_empty());
    }

    #[test]
    fn admission_stops_at_the_ceiling() {
        let mut store = HistoryStore::default();
        let names: Vec<String> = (0..MAX_TRACKED_ENTITIES + 50)
            .map(|i| format!("c{i}"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();

        store.record(&with_containers(&refs, 10.0, Utc::now(), true));

        assert_eq!(store.entity_count("p1"), MAX_TRACKED_ENTITIES);
    }

    #[test]
    fn an_error_snapshot_does_not_evict_everything() {
        let mut store = HistoryStore::default();
        let now = Utc::now();
        store.record(&with_containers(&["api"], 10.0, now, true));
        assert_eq!(store.entity_count("p1"), 1);

        // A failed collection reports no sections at all; that is a gap in
        // observation, not evidence the container is gone.
        store.record(&snapshot(1.0, now + Duration::seconds(15)));
        assert_eq!(store.entity_count("p1"), 1);
    }

    #[test]
    fn records_points_and_carries_metric_metadata() {
        let mut store = HistoryStore::default();
        let now = Utc::now();
        store.record(&snapshot(10.0, now - Duration::seconds(30)));
        store.record(&snapshot(20.0, now));

        let series = store.series("p1", None);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].key, "cpu");
        assert_eq!(series[0].label, "CPU");
        assert_eq!(series[0].warn_above, Some(85.0));
        assert_eq!(series[0].points.len(), 2);
        assert_eq!(series[0].latest, 20.0);
        assert_eq!(series[0].min, 10.0);
        assert_eq!(series[0].max, 20.0);
        assert_eq!(series[0].change_pct, Some(100.0));
    }

    #[test]
    fn a_single_sample_reports_no_trend() {
        let mut store = HistoryStore::default();
        store.record(&snapshot(10.0, Utc::now()));
        assert_eq!(store.series("p1", None)[0].change_pct, None);
    }

    #[test]
    fn a_zero_baseline_reports_no_percentage_change() {
        let mut store = HistoryStore::default();
        let now = Utc::now();
        store.record(&snapshot(0.0, now - Duration::seconds(10)));
        store.record(&snapshot(5.0, now));
        // 0 → 5 is not "infinite percent"; the delta is simply not expressible.
        assert_eq!(store.series("p1", None)[0].change_pct, None);
    }

    #[test]
    fn history_is_bounded() {
        let mut store = HistoryStore::default();
        let now = Utc::now();
        for i in 0..(MAX_POINTS + 200) {
            store.record(&snapshot(i as f64, now + Duration::seconds(i as i64)));
        }
        let series = store.series("p1", None);
        assert_eq!(series[0].points.len(), MAX_POINTS);
        // The newest samples are the ones kept.
        assert_eq!(series[0].latest, (MAX_POINTS + 199) as f64);
    }

    #[test]
    fn series_can_be_windowed() {
        let mut store = HistoryStore::default();
        let now = Utc::now();
        store.record(&snapshot(1.0, now - Duration::hours(2)));
        store.record(&snapshot(2.0, now));
        let recent = store.series("p1", Some(now - Duration::minutes(30)));
        assert_eq!(recent[0].points.len(), 1);
    }

    #[test]
    fn unknown_profiles_yield_nothing_rather_than_panicking() {
        assert!(HistoryStore::default().series("nope", None).is_empty());
    }

    #[test]
    fn histogram_places_events_in_the_right_buckets() {
        let since = Utc::now() - Duration::minutes(10);
        let until = Utc::now();
        let events = vec![
            event(LogLevel::Error, since + Duration::seconds(1)),
            event(LogLevel::Info, since + Duration::minutes(5)),
            event(LogLevel::Warn, until - Duration::seconds(1)),
        ];

        let buckets = histogram(&events, since, until, 10);
        assert_eq!(buckets.len(), 10, "empty buckets are kept, not skipped");
        assert_eq!(buckets.iter().map(|b| b.total).sum::<u32>(), 3);
        assert_eq!(buckets[0].error, 1);
        assert_eq!(buckets[5].info, 1);
        assert_eq!(buckets.last().unwrap().warn, 1);
    }

    #[test]
    fn histogram_ignores_events_outside_the_window() {
        let since = Utc::now() - Duration::minutes(5);
        let until = Utc::now();
        let events = vec![
            event(LogLevel::Error, since - Duration::hours(1)),
            event(LogLevel::Error, until + Duration::hours(1)),
        ];
        assert_eq!(
            histogram(&events, since, until, 10)
                .iter()
                .map(|b| b.total)
                .sum::<u32>(),
            0
        );
    }

    #[test]
    fn the_final_instant_lands_in_the_last_bucket() {
        let since = Utc::now() - Duration::minutes(1);
        let until = Utc::now();
        // Without the clamp this would index one past the end and panic.
        let buckets = histogram(&[event(LogLevel::Info, until)], since, until, 4);
        assert_eq!(buckets[3].total, 1);
    }
}
