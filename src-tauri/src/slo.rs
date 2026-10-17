//! Service level objectives: how often a connection should be good, the
//! error budget that leaves, how fast it is being spent, and the alerts that
//! say so while there is still budget to save.
//!
//! An objective is measured over a rolling window from root's own record —
//! up or not, or a metric on the right side of a threshold. Burn rate is
//! the speed of spending: 1× spends exactly the budget over the window, 14.4×
//! spends 2% of a 30-day budget in an hour. Alerts follow the multi-window
//! rule of the Google SRE workbook: a long window says it is real, a short
//! one says it is still happening.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::history::{HealthRun, MetricSeries, Point};
use crate::model::{Alert, AlertSeverity, Health, HealthSnapshot, MetricUnit};
use crate::rules::Op;

/// The fingerprint prefix of every alert an objective raises.
pub const PREFIX: &str = "slo:";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slo {
    pub id: String,
    pub profile_id: String,
    pub name: String,
    /// Percent of the window that should be good: 99.9.
    pub objective: f64,
    pub window_days: u32,
    pub indicator: Indicator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Indicator {
    /// Good while the connection is not down.
    Up,
    /// Good while a metric is on the right side of a threshold: error rate
    /// below 1%, latency below 300ms.
    Metric { metric_key: String, op: Op, threshold: f64 },
}

pub fn validate(slo: &Slo) -> Result<()> {
    let refuse = |why: &str| Err(AppError::Config(why.to_string()));
    if slo.name.trim().is_empty() {
        return refuse("An objective needs a name.");
    }
    if !(slo.objective > 0.0 && slo.objective < 100.0) {
        return refuse("An objective is a percentage under 100 — 100% leaves no budget at all.");
    }
    if !(1..=90).contains(&slo.window_days) {
        return refuse("Measure over between one and ninety days.");
    }
    if let Indicator::Metric { metric_key, threshold, .. } = &slo.indicator {
        if metric_key.trim().is_empty() {
            return refuse("Pick the metric that says whether it is good.");
        }
        if !threshold.is_finite() {
            return refuse("The threshold must be a number.");
        }
    }
    Ok(())
}

/// A stretch of time and how much of it was good, 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub good: f64,
}

/// Health as good time: down is bad, not knowing is not counted at all.
pub fn from_runs(runs: &[HealthRun]) -> Vec<Observation> {
    runs.iter()
        .filter(|r| r.health != Health::Unknown && r.to > r.from)
        .map(|r| Observation { from: r.from, to: r.to, good: if r.health == Health::Down { 0.0 } else { 1.0 } })
        .collect()
}

/// Readings as good time: each one holds until the next. `good` reads a value
/// as a fraction good — 0 or 1 for a threshold, the share up for availability.
pub fn from_points(points: &[Point], good: impl Fn(f64) -> f64) -> Vec<Observation> {
    let gaps: Vec<Duration> = points.windows(2).map(|w| w[1].t - w[0].t).collect();
    let typical = {
        let mut sorted = gaps.clone();
        sorted.sort();
        sorted.get(sorted.len() / 2).copied().unwrap_or(Duration::minutes(1))
    };
    points
        .iter()
        .enumerate()
        .map(|(i, p)| Observation {
            from: p.t,
            // A gap far longer than usual is root not watching, not a reading
            // that held: count only a usual step of it.
            to: p.t + gaps.get(i).copied().filter(|g| *g <= typical * 3).unwrap_or(typical),
            good: good(p.v),
        })
        .collect()
}

/// Good and known seconds inside the window.
fn totals(observations: &[Observation], from: DateTime<Utc>, to: DateTime<Utc>) -> (f64, f64) {
    observations.iter().fold((0.0, 0.0), |(good, known), o| {
        let start = o.from.max(from);
        let end = o.to.min(to);
        if end <= start {
            return (good, known);
        }
        let secs = (end - start).num_milliseconds() as f64 / 1000.0;
        (good + secs * o.good, known + secs)
    })
}

/// The share of known time that was good; nothing when nothing is known.
pub fn sli(observations: &[Observation], from: DateTime<Utc>, to: DateTime<Utc>) -> Option<f64> {
    let (good, known) = totals(observations, from, to);
    (known > 0.0).then(|| good / known)
}

/// How fast the budget is going over the last `window`: 1 spends it exactly
/// over the objective's window.
pub fn burn(observations: &[Observation], now: DateTime<Utc>, window: Duration, objective: f64) -> Option<f64> {
    let allowed = 1.0 - objective / 100.0;
    sli(observations, now - window, now).map(|good| (1.0 - good) / allowed)
}

/// What is left of the budget, as a fraction of it, at `steps` moments
/// across the window: it starts whole and every bad second spends some.
pub fn budget(observations: &[Observation], from: DateTime<Utc>, to: DateTime<Utc>, objective: f64, steps: usize) -> Vec<Point> {
    let (_, known) = totals(observations, from, to);
    let allowed = (1.0 - objective / 100.0) * known;
    if allowed <= 0.0 {
        return Vec::new();
    }
    (0..=steps)
        .map(|i| {
            let at = from + (to - from) * i as i32 / steps as i32;
            let (good, seen) = totals(observations, from, at);
            Point { t: at, v: 1.0 - (seen - good) / allowed }
        })
        .collect()
}

/// The windows a burn alert reads, long then short, and the rate that fires it.
const FAST: (i64, i64, f64) = (60, 5, 14.4);
const SLOW: (i64, i64, f64) = (360, 30, 6.0);

/// The alert an objective raises now, if it is burning: fast pages, slow warns.
pub fn burn_alert(slo: &Slo, observations: &[Observation], snapshot: &HealthSnapshot, now: DateTime<Utc>) -> Option<Alert> {
    let over = |(long, short, rate): (i64, i64, f64)| {
        let long_burn = burn(observations, now, Duration::minutes(long), slo.objective)?;
        let short_burn = burn(observations, now, Duration::minutes(short), slo.objective)?;
        (long_burn >= rate && short_burn >= rate).then_some((long, long_burn))
    };
    let (severity, how, (minutes, rate)) = match (over(FAST), over(SLOW)) {
        (Some(fast), _) => (AlertSeverity::Critical, "fast", fast),
        (None, Some(slow)) => (AlertSeverity::Warning, "steadily", slow),
        (None, None) => return None,
    };
    let lasts = Duration::seconds((slo.window_days as f64 * 86_400.0 / rate) as i64);
    Some(Alert {
        id: uuid::Uuid::new_v4().to_string(),
        fingerprint: format!("{PREFIX}{}", slo.id),
        profile_id: snapshot.profile_id.clone(),
        profile_name: snapshot.profile_name.clone(),
        kind: snapshot.kind,
        severity,
        title: format!("{} is burning its error budget {how}", slo.name),
        detail: format!(
            "{rate:.1}× over the last {} — at this rate a whole {}-day budget lasts {}",
            if minutes >= 60 { format!("{}h", minutes / 60) } else { format!("{minutes}m") },
            slo.window_days,
            if lasts >= Duration::days(2) { format!("{} days", lasts.num_days()) } else { format!("{} hours", lasts.num_hours().max(1)) }
        ),
        component: None,
        first_seen: now,
        last_seen: now,
        active: true,
        acknowledged: false,
    })
}

/// Good or not, per reading, for an indicator.
pub fn judge(indicator: &Indicator) -> impl Fn(f64) -> f64 + '_ {
    move |value| match indicator {
        Indicator::Up => value.clamp(0.0, 1.0),
        Indicator::Metric { op, threshold, .. } => {
            let good = match op {
                Op::Above => value > *threshold,
                Op::Below => value < *threshold,
            };
            if good { 1.0 } else { 0.0 }
        }
    }
}

/// The recent record an objective reads for its burn: health from the ring,
/// or the metric's own recent series.
pub fn recent(slo: &Slo, runs: &[HealthRun], series: &[MetricSeries]) -> Vec<Observation> {
    match &slo.indicator {
        Indicator::Up => from_runs(runs),
        Indicator::Metric { metric_key, .. } => series
            .iter()
            .find(|s| &s.key == metric_key)
            .map(|s| from_points(&s.points, judge(&slo.indicator)))
            .unwrap_or_default(),
    }
}

/// This connection's objectives' burn alerts, from what the ring holds.
pub async fn for_snapshot(state: &crate::state::AppState, snapshot: &HealthSnapshot) -> Vec<Alert> {
    let slos: Vec<Slo> = state
        .config
        .read()
        .await
        .slos
        .iter()
        .filter(|s| s.profile_id == snapshot.profile_id)
        .cloned()
        .collect();
    if slos.is_empty() {
        return Vec::new();
    }
    let now = Utc::now();
    let since = now - Duration::hours(7);
    let (runs, series) = {
        let history = state.history.read().await;
        (history.health_runs(&snapshot.profile_id, since, now), history.series(&snapshot.profile_id, Some(since)))
    };
    slos.iter()
        .filter_map(|slo| burn_alert(slo, &recent(slo, &runs, &series), snapshot, now))
        .collect()
}

/// One objective, measured.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SloStatus {
    pub slo: Slo,
    /// Percent good over the window, from what root has of it.
    pub sli: Option<f64>,
    /// How much of the window root has a record for, 0 to 1.
    pub covered: f64,
    /// What is left of the budget, as a fraction of it; below zero is overspent.
    pub remaining: Option<f64>,
    /// Burn over the last hour, six hours and three days.
    pub burn_1h: Option<f64>,
    pub burn_6h: Option<f64>,
    pub burn_3d: Option<f64>,
    /// The budget left across the window, for its chart — as a percent.
    pub budget: Vec<Point>,
    pub unit: MetricUnit,
}

/// Every objective measured against the longest record root keeps: the
/// archive's rollups for the window, the ring for what it has not rolled up.
pub async fn status(state: &crate::state::AppState, profile_id: Option<&str>) -> Vec<SloStatus> {
    let slos: Vec<Slo> = state
        .config
        .read()
        .await
        .slos
        .iter()
        .filter(|s| profile_id.is_none_or(|id| s.profile_id == id))
        .cloned()
        .collect();
    let now = Utc::now();
    let mut out = Vec::new();
    for slo in slos {
        let from = now - Duration::days(slo.window_days as i64);
        let key = match &slo.indicator {
            Indicator::Up => crate::archive::UP_KEY.to_string(),
            Indicator::Metric { metric_key, .. } => metric_key.clone(),
        };
        let archived: Vec<Point> = match state.store.read().await.as_ref() {
            Some(store) => store
                .metric_buckets(&slo.profile_id, &key, from)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|b| Point { t: b.at, v: b.avg })
                .collect(),
            None => Vec::new(),
        };
        let (runs, series) = {
            let history = state.history.read().await;
            (history.health_runs(&slo.profile_id, from, now), history.series(&slo.profile_id, Some(from)))
        };
        let mut observations = from_points(&archived, judge(&slo.indicator));
        // The ring covers what the archive has not got: before it opened, or
        // without one at all.
        let archived_from = observations.first().map(|o| o.from).unwrap_or(now);
        observations.extend(recent(&slo, &runs, &series).into_iter().filter(|o| o.to <= archived_from));
        let (_, known) = totals(&observations, from, now);
        let window_secs = (now - from).num_seconds() as f64;
        let good = sli(&observations, from, now);
        out.push(SloStatus {
            sli: good.map(|g| g * 100.0),
            covered: (known / window_secs).clamp(0.0, 1.0),
            remaining: good.map(|g| 1.0 - (1.0 - g) / (1.0 - slo.objective / 100.0)),
            burn_1h: burn(&observations, now, Duration::hours(1), slo.objective),
            burn_6h: burn(&observations, now, Duration::hours(6), slo.objective),
            burn_3d: burn(&observations, now, Duration::days(3), slo.objective),
            budget: budget(&observations, from, now, slo.objective, 120)
                .into_iter()
                .map(|p| Point { t: p.t, v: p.v * 100.0 })
                .collect(),
            unit: MetricUnit::Percent,
            slo,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_000_000, 0).unwrap() + Duration::minutes(minutes)
    }

    fn run(health: Health, from: i64, to: i64) -> HealthRun {
        HealthRun { health, from: at(from), to: at(to) }
    }

    #[test]
    fn down_time_is_bad_and_unknown_time_is_not_counted() {
        let obs = from_runs(&[run(Health::Healthy, 0, 90), run(Health::Down, 90, 100), run(Health::Unknown, 100, 200)]);
        assert_eq!(sli(&obs, at(0), at(200)), Some(90.0 / 100.0));
        assert_eq!(sli(&obs, at(150), at(200)), None, "nothing known, nothing claimed");
    }

    #[test]
    fn burn_is_the_bad_share_over_the_allowed_share() {
        // Ten minutes down in the last hour against 99.9%: 1/6 bad over 0.001 allowed.
        let obs = from_runs(&[run(Health::Healthy, 0, 50), run(Health::Down, 50, 60)]);
        let rate = burn(&obs, at(60), Duration::hours(1), 99.9).unwrap();
        assert!((rate - (10.0 / 60.0) / 0.001).abs() < 1e-6, "{rate}");
    }

    #[test]
    fn a_fast_burn_pages_only_while_the_short_window_still_burns() {
        let slo = Slo {
            id: "s1".into(),
            profile_id: "p1".into(),
            name: "Checkout availability".into(),
            objective: 99.9,
            window_days: 30,
            indicator: Indicator::Up,
        };
        let mut snapshot = HealthSnapshot::unreachable("p1", "checkout", crate::model::ServiceKind::Docker, "x".into(), 0);
        snapshot.error = None;
        // Down for the last ten minutes of an hour: 1h burn 167×, 5m burn 1000×.
        let burning = from_runs(&[run(Health::Healthy, 0, 50), run(Health::Down, 50, 60)]);
        let alert = burn_alert(&slo, &burning, &snapshot, at(60)).expect("pages");
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert_eq!(alert.fingerprint, "slo:s1");
        assert!(alert.detail.starts_with("166.7× over the last 1h"), "{}", alert.detail);
        // Recovered five minutes ago: the last 5m is clean, so the page stops;
        // but 10 bad minutes in the last 30 is still (10/30)/0.001 = 333×, so
        // the slow rule keeps a warning up.
        let recovered = from_runs(&[run(Health::Healthy, 0, 45), run(Health::Down, 45, 55), run(Health::Healthy, 55, 60)]);
        assert_eq!(burn_alert(&slo, &recovered, &snapshot, at(60)).unwrap().severity, AlertSeverity::Warning);
        // Recovered for half an hour: neither rule holds.
        let calm = from_runs(&[run(Health::Healthy, 0, 20), run(Health::Down, 20, 25), run(Health::Healthy, 25, 60)]);
        assert!(burn_alert(&slo, &calm, &snapshot, at(60)).is_none());
    }

    #[test]
    fn the_budget_starts_whole_and_bad_time_spends_it() {
        // 99% over 100 minutes allows one bad minute; minutes 50 to 52 were bad.
        // Read at 0, 25, 50, 75 and 100: whole until the bad stretch starts,
        // then 1 - 2/1 once it has passed.
        let obs = from_runs(&[run(Health::Healthy, 0, 50), run(Health::Down, 50, 52), run(Health::Healthy, 52, 100)]);
        let left = budget(&obs, at(0), at(100), 99.0, 4);
        let spent = 1.0 - 2.0 / 1.0;
        assert_eq!(left.iter().map(|p| (p.v * 100.0).round() / 100.0).collect::<Vec<_>>(), vec![1.0, 1.0, 1.0, spent, spent]);
    }

    #[test]
    fn a_threshold_judges_each_reading_and_a_long_gap_is_not_held() {
        let points: Vec<Point> = [(0, 0.5), (1, 2.0), (2, 0.4), (30, 0.3)].iter().map(|(m, v)| Point { t: at(*m), v: *v }).collect();
        let indicator = Indicator::Metric { metric_key: "error_rate".into(), op: Op::Below, threshold: 1.0 };
        let obs = from_points(&points, judge(&indicator));
        assert_eq!(obs.iter().map(|o| o.good).collect::<Vec<_>>(), vec![1.0, 0.0, 1.0, 1.0]);
        assert_eq!(obs[2].to, at(3), "the 28-minute silence after it counts as one usual minute");
    }
}
