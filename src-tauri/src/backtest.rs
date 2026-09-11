//! Running today's rules over yesterday's data.
//!
//! The judgement record answers "is root any good" from ratings, and ratings
//! arrive slowly. A single estate produces a handful of findings a week and an
//! incident worth rating maybe monthly; waiting for a sample large enough to
//! gate a decision on would stall the roadmap for a year.
//!
//! There is already a large labelled dataset sitting there. The estate's
//! Prometheus holds weeks of what every metric actually did, and the journal
//! holds which incidents happened and whether root called them. Replaying the
//! current rules across that history answers, for free and immediately, the
//! question that matters most: **would the rules as they stand now have caught
//! the thing we recorded as a miss?**
//!
//! ## The one thing this cannot do
//!
//! Config has no history. Prometheus stores numbers, not `redis.conf`, and
//! root reads configuration live. So a replay uses **today's configuration
//! with yesterday's metrics**, and every result carries that caveat.
//!
//! This is not a detail to bury. If `maxmemory` was raised from 2 GB to 8 GB
//! last Tuesday, a replay over last month will confidently report that nothing
//! was ever close to the ceiling — because it is measuring last month's usage
//! against this month's limit. The further back the window reaches, the more
//! likely that is, and the honest framing of a replay result is "what these
//! rules would say about this history, given how things are set up today".
//!
//! ## Trailing windows, because root had one
//!
//! At any past instant root held a ring buffer of recent samples, not every
//! sample since the beginning. Fitting a trend at time T over *all* history up
//! to T would flatter the replay badly: it would find slow trends that the
//! running product could never have seen, and every projection would look
//! better than the real one. Each evaluation therefore uses a trailing window
//! sized like the buffer root actually keeps.
//!
//! ## Episodes, not firings
//!
//! "This rule would have fired 400 times" means nothing — it is the step size
//! restated. Consecutive firings coalesce into stretches, and what gets
//! reported is when a rule started saying something and when it stopped.
//!
//! Two numbers come out of that, and they measure opposite failures:
//!
//! **Lead time** — how long before an incident the rule started firing. The
//! single most valuable figure a backtest produces: six hours of warning is a
//! product, six seconds is a log line.
//!
//! **Duty cycle** — what fraction of the window the rule spent firing. A rule
//! that is correct and firing eighty per cent of the time is not a warning,
//! it is wallpaper, and nobody will see the one time it matters.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::advisor::{self, Severity};
use crate::configsource::ConfigDocument;
use crate::history::{MetricSeries, Point};
use crate::journal::Called;
use crate::model::HealthSnapshot;

/// How much recent history each past evaluation may see.
///
/// Matched to the order of what root's own ring buffer holds. Larger would
/// invent foresight the product never had.
pub const DEFAULT_TRAILING: Duration = Duration::hours(6);

/// How often to evaluate. Hourly is fine for anything a capacity rule can say;
/// finer steps multiply cost without changing an episode's boundaries much.
pub const DEFAULT_STEP: Duration = Duration::hours(1);

/// Evaluations one replay may run, so a fine step over a long window cannot
/// wedge the app. At the default hourly step this is about seven months —
/// well past the retention of any Prometheus it would be reading from.
const MAX_EVALUATIONS: usize = 5_000;

/// Something the reader must know before believing a number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Caveat {
    /// Always present. See the module note.
    ConfigIsCurrent,
    /// The window has fewer samples than a trend needs, so projection-based
    /// rules could not fire regardless of what the data did.
    ThinHistory { samples: usize },
    /// No long-range source, so this replayed only what root happens to hold
    /// in memory right now.
    NoHistorySource,
}

/// One stretch during which a rule would have been saying something.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub rule: String,
    /// The title at the episode's most severe moment — the wording an operator
    /// would actually have been shown.
    pub title: String,
    pub severity: Severity,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Evaluations inside it, so a one-sample blip is distinguishable from a
    /// sustained condition.
    pub evaluations: usize,
}

impl Episode {
    pub fn seconds(&self) -> f64 {
        (self.to - self.from).num_seconds().max(0) as f64
    }
}

/// How a rule behaved across the whole window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleBehaviour {
    pub rule: String,
    pub episodes: usize,
    /// Seconds spent firing, summed.
    pub firing_secs: f64,
    /// Fraction of the window spent firing. Above roughly a third, a rule has
    /// stopped being a warning and become part of the background.
    pub duty_cycle: f64,
    pub worst: Severity,
}

/// Whether the replayed rules would have preceded a known incident.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub incident_id: String,
    pub headline: String,
    pub started_at: DateTime<Utc>,
    /// What a human recorded at the time — the label this is scored against.
    pub recorded: Called,
    /// The rule that would have fired first before it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// Seconds of warning the replay would have given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_secs: Option<f64>,
    /// A miss the current rules would now catch. The strongest signal a rule
    /// change actually worked, and the reason this module exists.
    pub newly_caught: bool,
}

/// What replaying one connection's history produced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Replay {
    pub profile_id: String,
    pub profile_name: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub evaluations: usize,
    pub samples: usize,
    pub episodes: Vec<Episode>,
    pub behaviour: Vec<RuleBehaviour>,
    pub coverage: Vec<Coverage>,
    pub caveats: Vec<Caveat>,
}

impl Replay {
    pub fn window_secs(&self) -> f64 {
        (self.to - self.from).num_seconds().max(1) as f64
    }
}

/// How far before an incident a firing counts as having warned about it.
///
/// A rule counts as having warned if it was saying something at any point in
/// this window, not only if it *started* inside it. That distinction was got
/// wrong first: a rule that had been firing for forty hours and was still
/// firing when the thing broke was being scored as silent, which is the
/// opposite of what happened.
///
/// Beyond the window a rule was firing about something else that happened to
/// precede this, and crediting that would inflate every score. A day is
/// generous for a capacity problem and far too generous for a crash loop; it
/// errs toward crediting the rule, because silently discarding real warnings
/// is the worse error when the whole point is finding rules that work.
///
/// The lead time that comes back is honest rather than clipped — a forty-hour
/// warning is reported as forty hours, and the duty cycle beside it is what
/// says whether anybody would have still been reading it.
const WARNING_COUNTS_WITHIN: Duration = Duration::hours(24);

/// Replay the current rules across a window of history.
///
/// `series` is the long-range history, typically from the estate's own
/// Prometheus. `snapshot` supplies each metric's label, unit and threshold —
/// its *values* are ignored and substituted from history at each instant.
pub fn replay(
    snapshot: &HealthSnapshot,
    documents: &[ConfigDocument],
    series: &[MetricSeries],
    incidents: &[(String, String, DateTime<Utc>, Called)],
    step: Duration,
    trailing: Duration,
) -> Replay {
    let mut caveats = vec![Caveat::ConfigIsCurrent];

    let samples: usize = series.iter().map(|s| s.points.len()).sum();
    let (from, to) = match bounds(series) {
        Some(bounds) => bounds,
        None => {
            caveats.push(Caveat::NoHistorySource);
            let now = Utc::now();
            return Replay {
                profile_id: snapshot.profile_id.clone(),
                profile_name: snapshot.profile_name.clone(),
                from: now,
                to: now,
                evaluations: 0,
                samples: 0,
                episodes: Vec::new(),
                behaviour: Vec::new(),
                coverage: Vec::new(),
                caveats,
            };
        }
    };

    if samples < 32 {
        caveats.push(Caveat::ThinHistory { samples });
    }

    // ------------------------------------------------------------ evaluate
    let step_secs = step.num_seconds().max(60);
    let mut open: Vec<Episode> = Vec::new();
    let mut closed: Vec<Episode> = Vec::new();
    let mut evaluations = 0usize;

    let mut at = from + trailing;
    while at <= to && evaluations < MAX_EVALUATIONS {
        evaluations += 1;

        let past = window(series, at - trailing, at);
        let moment = snapshot_at(snapshot, &past);
        let found = advisor::findings(&moment, documents, &past);

        // Rules firing now, by name. A rule can produce at most one finding
        // per connection per evaluation, so the name identifies the episode.
        let firing: Vec<(String, String, Severity)> = found
            .iter()
            .map(|f| (f.rule.clone(), f.title.clone(), f.severity))
            .collect();

        // Extend or open.
        for (rule, title, severity) in &firing {
            match open.iter_mut().find(|e| &e.rule == rule) {
                Some(episode) => {
                    episode.to = at;
                    episode.evaluations += 1;
                    // The wording shown at the worst moment, not the first.
                    if *severity > episode.severity {
                        episode.severity = *severity;
                        episode.title = title.clone();
                    }
                }
                None => open.push(Episode {
                    rule: rule.clone(),
                    title: title.clone(),
                    severity: *severity,
                    from: at,
                    to: at,
                    evaluations: 1,
                }),
            }
        }

        // Close anything that stopped.
        let still: Vec<String> = firing.into_iter().map(|(r, _, _)| r).collect();
        let (ended, continuing): (Vec<Episode>, Vec<Episode>) =
            open.into_iter().partition(|e| !still.contains(&e.rule));
        closed.extend(ended);
        open = continuing;

        at += Duration::seconds(step_secs);
    }
    closed.extend(open);
    closed.sort_by_key(|e| e.from);

    let behaviour = summarise(&closed, (to - from).num_seconds().max(1) as f64);
    let coverage = cover(&closed, incidents);

    Replay {
        profile_id: snapshot.profile_id.clone(),
        profile_name: snapshot.profile_name.clone(),
        from,
        to,
        evaluations,
        samples,
        episodes: closed,
        behaviour,
        coverage,
        caveats,
    }
}

/// Earliest and latest instant covered by any series.
fn bounds(series: &[MetricSeries]) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let mut from: Option<DateTime<Utc>> = None;
    let mut to: Option<DateTime<Utc>> = None;
    for s in series {
        if let (Some(first), Some(last)) = (s.points.first(), s.points.last()) {
            from = Some(from.map_or(first.t, |f: DateTime<Utc>| f.min(first.t)));
            to = Some(to.map_or(last.t, |t: DateTime<Utc>| t.max(last.t)));
        }
    }
    Some((from?, to?))
}

/// Each series clipped to `(start, end]`, with its summary statistics redone
/// over the clip.
///
/// The trend is refitted here rather than carried over: the whole question is
/// what a fit over *that* window would have said, and reusing the full-history
/// trend would answer a different one.
fn window(series: &[MetricSeries], start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<MetricSeries> {
    series
        .iter()
        .filter_map(|s| {
            let points: Vec<Point> = s
                .points
                .iter()
                .filter(|p| p.t > start && p.t <= end)
                .copied()
                .collect();
            if points.is_empty() {
                return None;
            }

            let values: Vec<f64> = points.iter().map(|p| p.v).collect();
            Some(MetricSeries {
                key: s.key.clone(),
                label: s.label.clone(),
                unit: s.unit,
                warn_above: s.warn_above,
                change_pct: None,
                min: values.iter().copied().fold(f64::INFINITY, f64::min),
                max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                latest: *values.last().unwrap_or(&0.0),
                trend: crate::forecast::trend(&points, s.warn_above),
                points,
            })
        })
        .collect()
}

/// The snapshot as it would have read at that instant.
///
/// Metadata — labels, units, thresholds — comes from the live snapshot because
/// none of it is in Prometheus and re-deriving a unit from a PromQL expression
/// would be guesswork. Only the values move.
fn snapshot_at(snapshot: &HealthSnapshot, past: &[MetricSeries]) -> HealthSnapshot {
    let mut moment = snapshot.clone();
    for metric in &mut moment.metrics {
        if let Some(series) = past.iter().find(|s| s.key == metric.key) {
            metric.value = series.latest;
        }
    }
    // Sections are live inventory with no history behind them. Left as they
    // are: the rules that read them (container limits, workload sizing) are
    // config rules, and config is already flagged as current rather than
    // historical.
    moment.checked_at = past
        .iter()
        .filter_map(|s| s.points.last().map(|p| p.t))
        .max()
        .unwrap_or(moment.checked_at);
    moment
}

fn summarise(episodes: &[Episode], window_secs: f64) -> Vec<RuleBehaviour> {
    let mut out: Vec<RuleBehaviour> = Vec::new();
    for episode in episodes {
        match out.iter_mut().find(|b| b.rule == episode.rule) {
            Some(behaviour) => {
                behaviour.episodes += 1;
                behaviour.firing_secs += episode.seconds();
                if episode.severity > behaviour.worst {
                    behaviour.worst = episode.severity;
                }
            }
            None => out.push(RuleBehaviour {
                rule: episode.rule.clone(),
                episodes: 1,
                firing_secs: episode.seconds(),
                duty_cycle: 0.0,
                worst: episode.severity,
            }),
        }
    }
    for behaviour in &mut out {
        behaviour.duty_cycle = (behaviour.firing_secs / window_secs).clamp(0.0, 1.0);
    }
    // Noisiest first: the list is read to find what to fix.
    out.sort_by(|a, b| b.duty_cycle.total_cmp(&a.duty_cycle));
    out
}

/// Which known incidents the replayed rules would have preceded.
fn cover(
    episodes: &[Episode],
    incidents: &[(String, String, DateTime<Utc>, Called)],
) -> Vec<Coverage> {
    incidents
        .iter()
        .map(|(id, headline, started_at, recorded)| {
            // Interval overlap, not a start-time test: an episode counts if it
            // was firing at any point between the window opening and the thing
            // breaking. Requiring it to have *begun* inside the window scores a
            // rule that had been shouting for two days as having said nothing.
            let window_opens = *started_at - WARNING_COUNTS_WITHIN;
            let warned = episodes
                .iter()
                .filter(|e| e.from <= *started_at && e.to >= window_opens)
                .min_by_key(|e| e.from);

            let lead_secs = warned.map(|e| (*started_at - e.from).num_seconds().max(0) as f64);

            Coverage {
                incident_id: id.clone(),
                headline: headline.clone(),
                started_at: *started_at,
                recorded: *recorded,
                rule: warned.map(|e| e.rule.clone()),
                lead_secs,
                // The finding this module exists for: a recorded miss that the
                // rules as they stand now would have caught.
                newly_caught: warned.is_some() && *recorded == Called::No,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configsource::ConfigDocument;
    use crate::model::{Health, Metric, MetricUnit, ServiceKind};

    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    fn snapshot(kind: ServiceKind, metrics: Vec<Metric>) -> HealthSnapshot {
        HealthSnapshot {
            profile_id: "p1".into(),
            profile_name: "session-cache".into(),
            kind,
            health: Health::Healthy,
            summary: String::new(),
            metrics,
            sections: Vec::new(),
            checked_at: Utc::now(),
            latency_ms: 1,
            error: None,
        }
    }

    fn redis_config(maxmemory: &str, policy: &str) -> ConfigDocument {
        ConfigDocument::live(
            "p1",
            "Running configuration",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": maxmemory, "maxmemory-policy": policy }),
        )
    }

    /// A series shaped by a function of elapsed hours, sampled every minute.
    fn series(
        key: &str,
        warn_above: Option<f64>,
        hours: i64,
        end: DateTime<Utc>,
        shape: impl Fn(f64) -> f64,
    ) -> MetricSeries {
        let start = end - Duration::hours(hours);
        let points: Vec<Point> = (0..hours * 60)
            .map(|i| {
                let elapsed = i as f64 / 60.0;
                Point {
                    t: start + Duration::minutes(i),
                    v: shape(elapsed),
                }
            })
            .collect();
        let values: Vec<f64> = points.iter().map(|p| p.v).collect();

        MetricSeries {
            key: key.to_string(),
            label: key.to_string(),
            unit: MetricUnit::Bytes,
            warn_above,
            change_pct: None,
            min: values.iter().copied().fold(f64::INFINITY, f64::min),
            max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            latest: *values.last().unwrap(),
            trend: None,
            points,
        }
    }

    // ------------------------------------------------------------ episodes

    /// A memory series that climbs into its ceiling should produce one
    /// sustained episode, not one per evaluation.
    #[test]
    fn consecutive_firings_collapse_into_one_episode() {
        let end = Utc::now();
        // Flat for a day, then climbing hard toward 2 GB.
        let memory = series("memory", Some(2.0 * GB), 48, end, |h| {
            if h < 24.0 {
                0.5 * GB
            } else {
                0.5 * GB + (h - 24.0) / 24.0 * 1.4 * GB
            }
        });

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[memory],
            &[],
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        let noeviction: Vec<&Episode> = out
            .episodes
            .iter()
            .filter(|e| e.rule.contains("maxmemory-noeviction"))
            .collect();

        assert_eq!(noeviction.len(), 1, "one stretch, not one per hour: {:#?}", out.episodes);
        assert!(
            noeviction[0].evaluations > 3,
            "a sustained condition, not a blip: {:?}",
            noeviction[0]
        );
        // It must begin in the climbing half, not during the flat day.
        assert!(noeviction[0].from > end - Duration::hours(25));
    }

    /// A metric that never approaches its limit must produce nothing. A
    /// backtest that fires on a healthy month would be measuring itself.
    #[test]
    fn a_quiet_history_produces_no_episodes() {
        let end = Utc::now();
        let memory = series("memory", Some(8.0 * GB), 48, end, |_| 0.4 * GB);

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("8gb", "noeviction")],
            &[memory],
            &[],
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        assert!(out.episodes.is_empty(), "{:#?}", out.episodes);
        assert!(out.evaluations > 0, "it did actually run");
    }

    /// The number that says a rule is wallpaper rather than a warning.
    #[test]
    fn a_rule_that_never_stops_firing_reports_a_high_duty_cycle() {
        let end = Utc::now();
        // Over its limit for the whole window: the rule is correct and useless.
        let memory = series("memory", Some(2.0 * GB), 48, end, |_| 2.5 * GB);

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[memory],
            &[],
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        let behaviour = out
            .behaviour
            .iter()
            .find(|b| b.rule.contains("maxmemory"))
            .expect("it fired");
        assert!(
            behaviour.duty_cycle > 0.7,
            "firing almost continuously: {:?}",
            behaviour
        );
    }

    // ------------------------------------------------------------ coverage

    /// The whole reason this module exists: a miss somebody recorded, that the
    /// rules as they stand now would have seen coming.
    #[test]
    fn a_recorded_miss_that_the_current_rules_would_catch_is_flagged() {
        let end = Utc::now();
        let memory = series("memory", Some(2.0 * GB), 48, end, |h| {
            0.4 * GB + h / 48.0 * 1.5 * GB
        });

        // Something broke six hours ago, and nobody was warned at the time.
        let broke_at = end - Duration::hours(6);
        let incidents = vec![(
            "i1".to_string(),
            "session-cache stopped accepting writes".to_string(),
            broke_at,
            Called::No,
        )];

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[memory],
            &incidents,
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        assert_eq!(out.coverage.len(), 1);
        let coverage = &out.coverage[0];
        assert!(coverage.newly_caught, "{coverage:#?}");
        assert!(coverage.rule.is_some());
        assert!(
            coverage.lead_secs.is_some_and(|s| s > 0.0),
            "some warning, not zero: {coverage:#?}"
        );
    }

    /// An incident root already called is covered, but it is not *newly*
    /// caught — counting it would inflate the one number this is for.
    #[test]
    fn an_incident_already_called_is_not_counted_as_newly_caught() {
        let end = Utc::now();
        let memory = series("memory", Some(2.0 * GB), 48, end, |h| {
            0.4 * GB + h / 48.0 * 1.5 * GB
        });
        let incidents = vec![(
            "i1".to_string(),
            "x".to_string(),
            end - Duration::hours(6),
            Called::Yes,
        )];

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[memory],
            &incidents,
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        assert!(out.coverage[0].rule.is_some(), "it did fire");
        assert!(!out.coverage[0].newly_caught, "already known, not new");
    }

    /// The case the overlap test exists for: a rule that had been firing for
    /// a long stretch and was still firing when the thing broke. Scoring that
    /// as silence is the opposite of what happened.
    #[test]
    fn a_rule_still_firing_when_it_broke_counts_as_having_warned() {
        let end = Utc::now();
        // Climbing from the very start, so the rule opens early and never
        // closes — its episode begins well outside the 24h warning window.
        let memory = series("memory", Some(2.0 * GB), 72, end, |h| {
            0.4 * GB + h / 72.0 * 1.5 * GB
        });
        let broke_at = end - Duration::hours(2);
        let incidents = vec![("i1".to_string(), "x".to_string(), broke_at, Called::No)];

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[memory],
            &incidents,
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        let coverage = &out.coverage[0];
        assert!(coverage.rule.is_some(), "it was firing at the time: {coverage:#?}");
        assert!(coverage.newly_caught);
        // Honest rather than clipped: the duty cycle beside it is what says
        // whether anybody would still have been reading it.
        assert!(
            coverage.lead_secs.is_some_and(|s| s > 24.0 * 3600.0),
            "a long warning is reported as long: {coverage:#?}"
        );
    }

    /// A rule firing long before an unrelated incident is not a warning about
    /// it. Crediting that would inflate every score.
    #[test]
    fn a_firing_far_outside_the_warning_window_is_not_a_catch() {
        let end = Utc::now();
        let memory = series("memory", Some(2.0 * GB), 48, end, |h| {
            0.4 * GB + h / 48.0 * 1.5 * GB
        });
        // The incident is two days after anything the replay could have said.
        let incidents = vec![(
            "i1".to_string(),
            "x".to_string(),
            end + Duration::hours(48),
            Called::No,
        )];

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[memory],
            &incidents,
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        assert!(out.coverage[0].rule.is_none(), "{:#?}", out.coverage[0]);
        assert!(!out.coverage[0].newly_caught);
    }

    // ------------------------------------------------------------- honesty

    /// Fitting at time T over everything up to T would find trends the running
    /// product could never have seen. The trailing window is what stops the
    /// replay flattering itself, so it has to actually be applied.
    #[test]
    fn each_evaluation_sees_only_a_trailing_window() {
        let end = Utc::now();
        // Utterly flat for two days, then one hour of steep climb. With a 6h
        // trailing window the early evaluations can see nothing but flatness.
        let memory = series("memory", Some(2.0 * GB), 48, end, |h| {
            if h < 47.0 {
                0.5 * GB
            } else {
                0.5 * GB + (h - 47.0) * 1.4 * GB
            }
        });

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[memory],
            &[],
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        // Nothing may fire in the first day: there is nothing there to fire on.
        let early = out
            .episodes
            .iter()
            .filter(|e| e.from < end - Duration::hours(12))
            .count();
        assert_eq!(early, 0, "fired on data it could not have had: {:#?}", out.episodes);
    }

    /// Config has no history. Every result says so, always, without being
    /// asked — it is the difference between a replay and a recording.
    #[test]
    fn every_replay_admits_that_config_is_current() {
        let end = Utc::now();
        let memory = series("memory", Some(2.0 * GB), 48, end, |_| 0.4 * GB);
        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "allkeys-lru")],
            &[memory],
            &[],
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );
        assert!(out.caveats.contains(&Caveat::ConfigIsCurrent));
    }

    #[test]
    fn no_history_is_reported_rather_than_returning_an_empty_success() {
        let snap = snapshot(ServiceKind::Redis, vec![]);
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[],
            &[],
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );
        assert!(out.caveats.contains(&Caveat::NoHistorySource));
        assert_eq!(out.evaluations, 0);
        assert!(out.episodes.is_empty());
    }

    /// A window too short for a trend means the projection rules could not
    /// have fired whatever the data did — which is a different statement from
    /// "nothing was wrong", and must not be read as the second.
    #[test]
    fn a_thin_window_is_flagged_rather_than_reported_as_clean() {
        let end = Utc::now();
        let thin = MetricSeries {
            key: "memory".into(),
            label: "Memory".into(),
            unit: MetricUnit::Bytes,
            warn_above: Some(2.0 * GB),
            change_pct: None,
            min: 0.0,
            max: 0.0,
            latest: 1.9 * GB,
            trend: None,
            points: (0..4)
                .map(|i| Point {
                    t: end - Duration::minutes(4 - i),
                    v: 1.9 * GB,
                })
                .collect(),
        };

        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[thin],
            &[],
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );
        assert!(
            out.caveats
                .iter()
                .any(|c| matches!(c, Caveat::ThinHistory { .. })),
            "{:#?}",
            out.caveats
        );
    }

    /// A year at a fine step must not wedge the app.
    #[test]
    fn evaluations_are_bounded() {
        let end = Utc::now();
        let long = series("memory", Some(2.0 * GB), 24 * 30, end, |_| 0.4 * GB);
        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[long],
            &[],
            Duration::minutes(1),
            DEFAULT_TRAILING,
        );
        assert!(out.evaluations <= MAX_EVALUATIONS);
    }

    /// The wording an operator would have been shown is the one from the worst
    /// moment, not the first — a stretch that started as advisory and became
    /// urgent should read as the urgent thing.
    #[test]
    fn an_episode_carries_its_most_severe_wording() {
        let end = Utc::now();
        let memory = series("memory", Some(2.0 * GB), 48, end, |h| {
            0.4 * GB + h / 48.0 * 1.7 * GB
        });
        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 0.0, MetricUnit::Bytes)],
        );
        let out = replay(
            &snap,
            &[redis_config("2gb", "noeviction")],
            &[memory],
            &[],
            DEFAULT_STEP,
            DEFAULT_TRAILING,
        );

        let episode = out
            .episodes
            .iter()
            .find(|e| e.rule.contains("maxmemory"))
            .expect("it fired");
        assert_eq!(episode.severity, Severity::Now, "{episode:#?}");
    }
}
