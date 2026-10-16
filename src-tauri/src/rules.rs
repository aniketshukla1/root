//! Alert rules people write, and silences.
//!
//! The snapshot engine raises what collectors already judge — a breached
//! threshold, a failed component. A rule is the operator's own judgement:
//! "checkout's error rate above 2% for five minutes", or "`payment declined`
//! more than ten times in five minutes on orders-api". A rule runs on its
//! connection's poll, against the snapshot just collected and the lines the
//! log buffer holds, and its alert goes into the same store as every other —
//! so it resolves, reopens and notifies the same way.
//!
//! A silence mutes alerts without hiding them: they still open, resolve and
//! appear, marked, and no channel is told. A silence that starts later is a
//! maintenance window. Silences are applied when alerts are read, because they
//! begin and end on the clock, not on a poll.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::history::{MetricSeries, Point};
use crate::model::{Alert, AlertSeverity, HealthSnapshot, LogEvent, MetricUnit};

/// The fingerprint prefix of every alert a rule raises.
pub const PREFIX: &str = "rule:";

/// How far back a preview replays a rule.
const PREVIEW_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertRule {
    pub id: String,
    pub name: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub severity: AlertSeverity,
    /// Free-form pairs — team, service, runbook — shown with the alert.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub condition: Condition,
    /// How long the condition must hold before the alert fires. Zero fires on
    /// the first poll that sees it.
    #[serde(default)]
    pub for_secs: u64,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Condition {
    /// A metric root collects on one connection, against a threshold.
    Metric {
        profile_id: String,
        metric_key: String,
        op: Op,
        threshold: f64,
    },
    /// Lines on one connection matching a pattern, counted over a sliding
    /// window. A pattern between slashes is a regular expression; anything
    /// else is a case-insensitive substring.
    Log {
        profile_id: String,
        pattern: String,
        window_secs: u64,
        op: Op,
        threshold: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Above,
    Below,
}

impl Op {
    fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            Op::Above => value > threshold,
            Op::Below => value < threshold,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Op::Above => "above",
            Op::Below => "below",
        }
    }
}

impl Condition {
    pub fn profile_id(&self) -> &str {
        match self {
            Condition::Metric { profile_id, .. } | Condition::Log { profile_id, .. } => profile_id,
        }
    }

    fn op_threshold(&self) -> (Op, f64) {
        match self {
            Condition::Metric { op, threshold, .. } | Condition::Log { op, threshold, .. } => (*op, *threshold),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Silence {
    pub id: String,
    /// What it mutes. Every field that is set must match, and at least one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default)]
    pub comment: String,
}

impl Silence {
    pub fn covers(&self, alert: &Alert, now: DateTime<Utc>) -> bool {
        self.starts_at <= now
            && now < self.ends_at
            && self.rule_id.as_ref().is_none_or(|id| alert.fingerprint == format!("{PREFIX}{id}"))
            && self.profile_id.as_ref().is_none_or(|id| &alert.profile_id == id)
            && self.fingerprint.as_ref().is_none_or(|f| &alert.fingerprint == f)
    }
}

/// When each silenced alert's silence ends, keyed by fingerprint.
pub fn silenced(alerts: &[Alert], silences: &[Silence], now: DateTime<Utc>) -> HashMap<String, DateTime<Utc>> {
    alerts
        .iter()
        .filter_map(|alert| {
            silences
                .iter()
                .filter(|s| s.covers(alert, now))
                .map(|s| s.ends_at)
                .max()
                .map(|until| (alert.fingerprint.clone(), until))
        })
        .collect()
}

// ------------------------------------------------------------- validation

pub fn validate_rule(rule: &AlertRule) -> Result<()> {
    let refuse = |why: &str| Err(AppError::Config(why.to_string()));
    if rule.name.trim().is_empty() {
        return refuse("A rule needs a name.");
    }
    if rule.for_secs > 86_400 {
        return refuse("A rule can wait at most a day before it fires.");
    }
    if !rule.condition.op_threshold().1.is_finite() {
        return refuse("The threshold must be a number.");
    }
    match &rule.condition {
        Condition::Metric { metric_key, .. } if metric_key.trim().is_empty() => {
            refuse("Pick the metric this rule watches.")
        }
        Condition::Log { pattern, window_secs, .. } => {
            if pattern.trim().is_empty() {
                return refuse("A log rule needs a pattern to count.");
            }
            if !(60..=86_400).contains(window_secs) {
                return refuse("Count lines over a window between a minute and a day.");
            }
            Matcher::new(pattern).map(|_| ())
        }
        Condition::Metric { .. } => Ok(()),
    }
}

pub fn validate_silence(silence: &Silence, now: DateTime<Utc>) -> Result<()> {
    let refuse = |why: &str| Err(AppError::Config(why.to_string()));
    if silence.rule_id.is_none() && silence.profile_id.is_none() && silence.fingerprint.is_none() {
        return refuse("A silence needs something to silence: a rule, a connection or one alert.");
    }
    if silence.ends_at <= silence.starts_at {
        return refuse("A silence has to end after it starts.");
    }
    if silence.ends_at <= now {
        return refuse("That silence would already be over.");
    }
    if silence.ends_at - silence.starts_at > Duration::days(30) {
        // Longer than a month is not a silence; it is a rule to change.
        return refuse("A silence can last at most 30 days.");
    }
    Ok(())
}

// ------------------------------------------------------------ evaluation

enum Matcher {
    Substring(String),
    Regex(regex::Regex),
}

impl Matcher {
    fn new(pattern: &str) -> Result<Self> {
        let pattern = pattern.trim();
        if pattern.len() > 2 && pattern.starts_with('/') && pattern.ends_with('/') {
            return regex::Regex::new(&pattern[1..pattern.len() - 1])
                .map(Matcher::Regex)
                .map_err(|e| AppError::Config(format!("That pattern is not a regular expression root can read: {e}")));
        }
        Ok(Matcher::Substring(pattern.to_lowercase()))
    }

    fn matches(&self, line: &str) -> bool {
        match self {
            Matcher::Substring(needle) => line.to_lowercase().contains(needle),
            Matcher::Regex(re) => re.is_match(line),
        }
    }
}

/// The value a rule reads on this poll: the metric, or the number of matching
/// lines in the window. `None` when there is nothing to read.
fn reading(rule: &AlertRule, snapshot: &HealthSnapshot, logs: &[LogEvent], now: DateTime<Utc>) -> Option<f64> {
    match &rule.condition {
        Condition::Metric { metric_key, .. } => {
            snapshot.metrics.iter().find(|m| &m.key == metric_key).map(|m| m.value)
        }
        Condition::Log { profile_id, pattern, window_secs, .. } => {
            let matcher = Matcher::new(pattern).ok()?;
            let since = now - Duration::seconds(*window_secs as i64);
            let count = logs
                .iter()
                .filter(|l| &l.profile_id == profile_id && l.timestamp > since && l.timestamp <= now)
                .filter(|l| matcher.matches(&l.message))
                .count();
            Some(count as f64)
        }
    }
}

/// Since when each rule's condition has held without a break; the alert waits
/// `for_secs` of it.
#[derive(Default)]
pub struct Pending(HashMap<String, DateTime<Utc>>);

impl Pending {
    /// A rule edited or deleted starts its wait again.
    pub fn forget(&mut self, rule_id: &str) {
        self.0.remove(rule_id);
    }
}

/// The alerts this connection's rules raise on this poll.
pub fn evaluate(
    rules: &[AlertRule],
    snapshot: &HealthSnapshot,
    logs: &[LogEvent],
    pending: &mut Pending,
    now: DateTime<Utc>,
) -> Vec<Alert> {
    let mut alerts = Vec::new();
    for rule in rules.iter().filter(|r| r.enabled && r.condition.profile_id() == snapshot.profile_id) {
        let (op, threshold) = rule.condition.op_threshold();
        let Some(value) = reading(rule, snapshot, logs, now).filter(|v| op.holds(*v, threshold)) else {
            pending.forget(&rule.id);
            continue;
        };
        let since = *pending.0.entry(rule.id.clone()).or_insert(now);
        if now - since < Duration::seconds(rule.for_secs as i64) {
            continue;
        }
        alerts.push(Alert {
            id: uuid::Uuid::new_v4().to_string(),
            fingerprint: format!("{PREFIX}{}", rule.id),
            profile_id: snapshot.profile_id.clone(),
            profile_name: snapshot.profile_name.clone(),
            kind: snapshot.kind,
            severity: rule.severity,
            title: rule.name.clone(),
            detail: describe(rule, snapshot, value),
            component: None,
            first_seen: since,
            last_seen: now,
            active: true,
            acknowledged: false,
        });
    }
    alerts
}

/// The alerts this connection's rules raise, against the snapshot just
/// collected and the lines the buffer holds for it.
pub async fn for_snapshot(state: &crate::state::AppState, snapshot: &HealthSnapshot) -> Vec<Alert> {
    let rules: Vec<AlertRule> = state
        .config
        .read()
        .await
        .alert_rules
        .iter()
        .filter(|r| r.enabled && r.condition.profile_id() == snapshot.profile_id)
        .cloned()
        .collect();
    if rules.is_empty() {
        return Vec::new();
    }
    let logs: Vec<LogEvent> = if rules.iter().any(|r| matches!(r.condition, Condition::Log { .. })) {
        state.logs.read().await.iter().filter(|l| l.profile_id == snapshot.profile_id).cloned().collect()
    } else {
        Vec::new()
    };
    let mut pending = state.rule_pending.lock().await;
    evaluate(&rules, snapshot, &logs, &mut pending, Utc::now())
}

fn describe(rule: &AlertRule, snapshot: &HealthSnapshot, value: f64) -> String {
    let (op, threshold) = rule.condition.op_threshold();
    let held = if rule.for_secs > 0 { format!(" for {}", span(rule.for_secs)) } else { String::new() };
    match &rule.condition {
        Condition::Metric { metric_key, .. } => {
            let (label, unit) = snapshot
                .metrics
                .iter()
                .find(|m| &m.key == metric_key)
                .map(|m| (m.label.clone(), m.unit))
                .unwrap_or((metric_key.clone(), MetricUnit::Count));
            format!(
                "{label} is {} — {} {}{held}",
                crate::state::format_metric(value, unit),
                op.word(),
                crate::state::format_metric(threshold, unit)
            )
        }
        Condition::Log { pattern, window_secs, .. } => format!(
            "{} line{} matching “{pattern}” in the last {} — {} {threshold}{held}",
            value,
            if value == 1.0 { "" } else { "s" },
            span(*window_secs),
            op.word()
        ),
    }
}

/// `90` → `1m 30s`, `300` → `5m`, `7200` → `2h`, `607800` → `7d 50m`.
fn span(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    let parts: Vec<String> = [(d, "d"), (h, "h"), (m, "m"), (s, "s")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect();
    if parts.is_empty() { "0s".into() } else { parts.join(" ") }
}

// --------------------------------------------------------------- preview

/// One stretch a rule would have fired for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Firing {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub peak: f64,
    /// Still firing at the end of the history.
    pub ongoing: bool,
}

/// What a rule would have done over the history root has.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePreview {
    /// What the condition was read from: the metric, or matching lines per window.
    pub series: Option<MetricSeries>,
    pub firings: Vec<Firing>,
    /// In words: how often, for how long, over how much history.
    pub summary: String,
}

/// The stretches where `points` held the condition long enough to fire —
/// each starting at the first reading that had held `for_secs`, as a poll
/// would have, and ending at the first reading that no longer held.
pub fn firings(points: &[Point], op: Op, threshold: f64, for_secs: u64) -> Vec<Firing> {
    let wait = Duration::seconds(for_secs as i64);
    let mut out = Vec::new();
    let mut since: Option<DateTime<Utc>> = None;
    let mut open: Option<Firing> = None;
    for p in points {
        if !op.holds(p.v, threshold) {
            since = None;
            if let Some(mut firing) = open.take() {
                firing.end = p.t;
                out.push(firing);
            }
            continue;
        }
        let start = *since.get_or_insert(p.t);
        match open.as_mut() {
            Some(firing) => {
                firing.end = p.t;
                firing.peak = if op == Op::Above { firing.peak.max(p.v) } else { firing.peak.min(p.v) };
            }
            None if p.t - start >= wait => {
                open = Some(Firing { start: p.t, end: p.t, peak: p.v, ongoing: false });
            }
            None => {}
        }
    }
    if let Some(mut firing) = open {
        firing.ongoing = true;
        out.push(firing);
    }
    out
}

/// Matching lines counted over the window, once a minute across the lines held.
fn line_counts(stamps: &mut [DateTime<Utc>], window: Duration, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Point> {
    stamps.sort();
    let mut points = Vec::new();
    let (mut lo, mut hi) = (0, 0);
    let mut t = from;
    while t <= to {
        while hi < stamps.len() && stamps[hi] <= t {
            hi += 1;
        }
        while lo < hi && stamps[lo] <= t - window {
            lo += 1;
        }
        points.push(Point { t, v: (hi - lo) as f64 });
        t += Duration::minutes(1);
    }
    points
}

/// Replay a rule over the last week of the history root keeps — the metric's
/// archive, or the lines the log buffer still holds — and say what it would
/// have done.
pub async fn preview(state: &crate::state::AppState, rule: &AlertRule, now: DateTime<Utc>) -> Result<RulePreview> {
    validate_rule(rule)?;
    let (op, threshold) = rule.condition.op_threshold();
    let name = state
        .config
        .read()
        .await
        .profiles
        .iter()
        .find(|p| p.id == rule.condition.profile_id())
        .map(|p| p.name.clone())
        .ok_or_else(|| AppError::Collector("no such connection".into()))?;

    let (label, unit, points) = match &rule.condition {
        Condition::Metric { profile_id, metric_key, .. } => {
            // A minute short of the week keeps the archive on its one-minute rollups.
            let since = now - Duration::days(PREVIEW_DAYS) + Duration::minutes(1);
            let mut points: Vec<Point> = match state.store.read().await.as_ref() {
                Some(store) => store
                    .metric_buckets(profile_id, metric_key, since)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|b| Point { t: b.at, v: b.avg })
                    .collect(),
                None => Vec::new(),
            };
            // No archive, or nothing in it yet: what the in-memory ring holds.
            let recent = state.history.read().await.series(profile_id, Some(since));
            let known = recent.iter().find(|s| &s.key == metric_key);
            if points.is_empty() {
                points = known.map(|s| s.points.clone()).unwrap_or_default();
            }
            let label = known.map(|s| s.label.clone()).unwrap_or_else(|| metric_key.clone());
            let unit = known.map(|s| s.unit).unwrap_or(MetricUnit::Count);
            (label, unit, points)
        }
        Condition::Log { profile_id, pattern, window_secs, .. } => {
            let matcher = Matcher::new(pattern)?;
            let (mut stamps, oldest) = {
                let logs = state.logs.read().await;
                let mine: Vec<&LogEvent> = logs.iter().filter(|l| &l.profile_id == profile_id).collect();
                let oldest = mine.iter().map(|l| l.timestamp).min();
                let stamps: Vec<DateTime<Utc>> =
                    mine.iter().filter(|l| matcher.matches(&l.message)).map(|l| l.timestamp).collect();
                (stamps, oldest)
            };
            let from = oldest.unwrap_or(now).max(now - Duration::days(PREVIEW_DAYS));
            let points = if oldest.is_some() {
                line_counts(&mut stamps, Duration::seconds(*window_secs as i64), from, now)
            } else {
                Vec::new()
            };
            (format!("Lines matching “{pattern}” per {}", span(*window_secs)), MetricUnit::Count, points)
        }
    };

    if points.len() < 2 {
        return Ok(RulePreview {
            series: None,
            firings: Vec::new(),
            summary: format!("No history for {label} on {name} yet, so there is nothing to replay."),
        });
    }

    let found = firings(&points, op, threshold, rule.for_secs);
    let covered = points[points.len() - 1].t - points[0].t;
    let over = if covered >= Duration::days(2) {
        format!("{} days", covered.num_days())
    } else {
        format!("{} hours", covered.num_hours().max(1))
    };
    let total: Duration = found.iter().map(|f| f.end - f.start).sum();
    let summary = match found.len() {
        0 => format!("Would not have fired in the last {over}."),
        n => format!(
            "Would have fired {n} time{} in the last {over}, for {} in all{}.",
            if n == 1 { "" } else { "s" },
            span(total.num_seconds().max(60) as u64 / 60 * 60),
            if found.last().is_some_and(|f| f.ongoing) { ", and would be firing now" } else { "" }
        ),
    };

    let values: Vec<f64> = points.iter().map(|p| p.v).collect();
    Ok(RulePreview {
        series: Some(MetricSeries {
            key: rule.id.clone(),
            label,
            unit,
            // Above-threshold rules get the limit drawn; the chart has no floor to draw.
            warn_above: (op == Op::Above).then_some(threshold),
            change_pct: None,
            min: values.iter().copied().fold(f64::INFINITY, f64::min),
            max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            latest: *values.last().unwrap_or(&0.0),
            trend: None,
            points,
        }),
        firings: found,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Health, Metric, ServiceKind};

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_000_000, 0).unwrap() + Duration::minutes(minute)
    }

    fn points(values: &[f64]) -> Vec<Point> {
        values.iter().enumerate().map(|(i, v)| Point { t: at(i as i64), v: *v }).collect()
    }

    fn rule(condition: Condition, for_secs: u64) -> AlertRule {
        AlertRule {
            id: "r1".into(),
            name: "Checkout errors".into(),
            enabled: true,
            severity: AlertSeverity::Critical,
            labels: BTreeMap::new(),
            condition,
            for_secs,
        }
    }

    fn snapshot(value: f64) -> HealthSnapshot {
        let mut snapshot = HealthSnapshot::unreachable("p1", "checkout", ServiceKind::Docker, "x".into(), 0);
        snapshot.error = None;
        snapshot.health = Health::Healthy;
        snapshot.metrics = vec![Metric::new("error_rate", "Error rate", value, MetricUnit::Percent)];
        snapshot
    }

    fn metric_rule(for_secs: u64) -> AlertRule {
        rule(
            Condition::Metric { profile_id: "p1".into(), metric_key: "error_rate".into(), op: Op::Above, threshold: 2.0 },
            for_secs,
        )
    }

    #[test]
    fn a_rule_fires_only_after_holding_for_its_duration() {
        let rules = [metric_rule(300)];
        let mut pending = Pending::default();
        assert!(evaluate(&rules, &snapshot(5.0), &[], &mut pending, at(0)).is_empty(), "pending, not firing");
        assert!(evaluate(&rules, &snapshot(5.0), &[], &mut pending, at(4)).is_empty());
        let fired = evaluate(&rules, &snapshot(5.0), &[], &mut pending, at(5));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].fingerprint, "rule:r1");
        assert_eq!(fired[0].detail, "Error rate is 5.0% — above 2.0% for 5m");
        // A break restarts the wait.
        assert!(evaluate(&rules, &snapshot(1.0), &[], &mut pending, at(6)).is_empty());
        assert!(evaluate(&rules, &snapshot(5.0), &[], &mut pending, at(7)).is_empty());
    }

    #[test]
    fn a_log_rule_counts_matching_lines_in_its_window() {
        let rules = [rule(
            Condition::Log { profile_id: "p1".into(), pattern: "declined".into(), window_secs: 300, op: Op::Above, threshold: 2.0 },
            0,
        )];
        let line = |minute: i64, message: &str| {
            LogEvent::new("p1", "checkout", ServiceKind::Docker, "api", crate::model::LogLevel::Error, message.into(), at(minute))
        };
        let logs = vec![
            line(1, "payment DECLINED: card_expired"),
            line(2, "payment declined: insufficient funds"),
            line(3, "order placed"),
            line(4, "payment declined: fraud"),
        ];
        let mut pending = Pending::default();
        let fired = evaluate(&rules, &snapshot(0.0), &logs, &mut pending, at(5));
        assert_eq!(fired.len(), 1, "three matches, case-insensitively, is above two");
        assert!(fired[0].detail.starts_with("3 lines matching “declined” in the last 5m"), "{}", fired[0].detail);
        assert!(evaluate(&rules, &snapshot(0.0), &logs, &mut pending, at(9)).is_empty(), "two have left the window");
    }

    #[test]
    fn firings_start_when_a_poll_would_have_fired_and_end_when_it_stops_holding() {
        let held = points(&[1.0, 5.0, 6.0, 7.0, 1.0, 5.0, 9.0]);
        assert_eq!(
            firings(&held, Op::Above, 2.0, 120),
            vec![
                Firing { start: at(3), end: at(4), peak: 7.0, ongoing: false },
                // Minutes 5 and 6 held for one minute only: not long enough.
            ]
        );
        let now = firings(&held, Op::Above, 2.0, 0);
        assert_eq!(now.len(), 2);
        assert!(now[1].ongoing && now[1].peak == 9.0);
    }

    #[test]
    fn silences_match_on_every_field_they_set_and_only_while_they_run() {
        let alert = evaluate(&[metric_rule(0)], &snapshot(5.0), &[], &mut Pending::default(), at(0)).remove(0);
        let silence = |rule: Option<&str>, profile: Option<&str>| Silence {
            id: "s1".into(),
            rule_id: rule.map(String::from),
            profile_id: profile.map(String::from),
            fingerprint: None,
            starts_at: at(0),
            ends_at: at(60),
            comment: String::new(),
        };
        assert!(silence(Some("r1"), None).covers(&alert, at(1)));
        assert!(silence(Some("r1"), Some("p1")).covers(&alert, at(1)));
        assert!(!silence(Some("r1"), Some("p2")).covers(&alert, at(1)), "both fields must match");
        assert!(!silence(Some("r1"), None).covers(&alert, at(60)), "over at its end");
        assert!(!silence(None, Some("p1")).covers(&alert, at(-1)), "not yet started: a maintenance window ahead");
        assert!(validate_silence(&silence(None, None), at(0)).is_err(), "a silence of everything is refused");
    }

    #[test]
    fn a_pattern_between_slashes_is_a_regex_and_a_broken_one_is_refused() {
        assert!(Matcher::new("/5\\d\\d/").unwrap().matches("upstream 502"));
        assert!(!Matcher::new("/5\\d\\d/").unwrap().matches("upstream 404"));
        let broken = rule(
            Condition::Log { profile_id: "p1".into(), pattern: "/(unclosed/".into(), window_secs: 300, op: Op::Above, threshold: 1.0 },
            0,
        );
        assert!(validate_rule(&broken).is_err());
    }

    #[test]
    fn spans_read_in_the_largest_units_that_fit() {
        assert_eq!(span(90), "1m 30s");
        assert_eq!(span(300), "5m");
        assert_eq!(span(607_800), "7d 50m");
    }

    #[test]
    fn line_counts_slide_over_the_window() {
        let mut stamps = vec![at(0), at(1), at(1), at(5)];
        let counts = line_counts(&mut stamps, Duration::minutes(2), at(0), at(5));
        // (t - 2m, t]: at minute 2 the line from minute 0 has already left.
        assert_eq!(counts.iter().map(|p| p.v).collect::<Vec<_>>(), vec![1.0, 3.0, 2.0, 0.0, 0.0, 1.0]);
    }
}
