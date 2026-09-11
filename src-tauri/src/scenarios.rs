//! Known situations with known right answers.
//!
//! The backtest asks what the rules would have said about one estate's real
//! history. This asks something narrower and more useful for keeping them
//! honest: given a situation we already agree on, do they still say the right
//! thing?
//!
//! ## What this is for, given the unit tests already exist
//!
//! `advisor.rs` has tests, and they overlap with this on purpose. They are not
//! the same instrument.
//!
//! A unit test fails a build. It is the right tool for "this rule's arithmetic
//! is correct" and a bad tool for the question that actually comes up: somebody
//! has widened a threshold, or reworded a rule, or swapped a model — is the
//! product **better or worse than it was**? A red build answers that with a
//! stack trace for one case. A suite that reports 18/20 with the two names
//! that regressed answers it directly, and can be run by somebody who is not
//! going to read Rust.
//!
//! The second difference is where they come from. A unit test is written when
//! a rule is written. A scenario should be written when a rule *fails* — every
//! recorded miss in the journal, with its note saying what would have shown
//! it, is a scenario waiting to be added. That is how the library earns its
//! keep over time, and why scenarios are data rather than code.
//!
//! ## Both halves, always
//!
//! Every scenario declares what should fire **and** what should stay silent.
//! A library that only checks for detection drifts steadily noisier, because
//! every fix for a miss is free and every new false positive is invisible.
//! `Spurious` and `Missed` are reported apart for the same reason precision
//! and recall are.
//!
//! ## What it does not cover
//!
//! The rules only. Running these against the assistant would need a provider
//! configured and would cost a request per scenario per run, which is a
//! different kind of thing to wire up — the result type carries an `engine`
//! so that can slot in later without rewriting what is here. Today, `engine`
//! is always `rules`.

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::advisor::{self, Severity};
use crate::configsource::ConfigDocument;
use crate::history::{MetricSeries, Point};
use crate::model::{Health, HealthSnapshot, Metric, MetricUnit, ServiceKind};

/// The library that ships with the product.
///
/// Embedded rather than read from disk so the baseline travels with the rules
/// it is a baseline for: a scenario set that can drift out of step with the
/// code it grades is not a regression suite.
const BUILT_IN: &str = include_str!("scenarios/library.json");

/// Samples a generated series holds. Enough to clear the forecaster's floor
/// with room to spare, so a scenario tests a rule rather than the gate.
const SAMPLES: i64 = 240;

/// How a generated metric moves across the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Shape {
    /// Climbs from `from` to `to` with realistic scatter around the line.
    ///
    /// Deliberately not a perfect line. A scenario built on one has an R² of
    /// exactly 1.0, which means it cannot notice the fit gate moving — this
    /// library passed unchanged with the gate raised from 0.25 to 0.9, a
    /// change that would have blinded the real product, because no scenario in
    /// it had a fit a real metric could fail. Nothing in nature is that
    /// straight, and testing against one is testing a fantasy.
    Linear,
    /// A perfectly straight line, for the rare case that is the point.
    CleanLinear,
    /// A real climb buried in real noise — a fit around 0.4.
    ///
    /// The scenario that brackets the forecaster's fit gate from below. A
    /// library whose climbing cases all fit at 0.95 cannot tell whether the
    /// gate is at 0.25 or 0.9, and both of those are products: one warns about
    /// messy metrics, the other only about immaculate ones. Something has to
    /// sit in between or the threshold is untested in either direction.
    NoisyClimb,
    /// Held at `to` throughout.
    Flat,
    /// Alternating between `from` and `to` every sample. Has a slope and
    /// almost no fit — the case the forecaster exists to refuse.
    Sawtooth,
    /// At `from` for most of the window, then `to`. A change with no runway,
    /// which no straight line can usefully extrapolate.
    Step,
    /// At `from` until the last tenth, then climbing hard to `to`.
    LateClimb,
}

/// One metric a scenario sets up.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesSpec {
    pub key: String,
    #[serde(default)]
    pub label: Option<String>,
    pub unit: MetricUnit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warn_above: Option<f64>,
    pub from: f64,
    pub to: f64,
    pub shape: Shape,
    /// Window length. Long enough by default that a trend is fittable.
    #[serde(default = "default_hours")]
    pub hours: i64,
}

fn default_hours() -> i64 {
    24
}

fn default_via() -> String {
    "scenario (live)".to_string()
}

/// What a scenario expects the rules to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expectation {
    /// Rules that must fire. A suffix match, so a scenario survives a rule
    /// being renamed from `caching.x` to `redis.x` — the identity is the
    /// behaviour, not the namespace.
    #[serde(default)]
    pub fires: Vec<ExpectedRule>,
    /// Rules that must stay silent. Without these the library drifts noisier
    /// with every fix.
    #[serde(default)]
    pub silent: Vec<String>,
    /// True when nothing at all should fire. Stronger than listing every rule
    /// in `silent`, and the only way to catch a brand new rule that is wrong.
    #[serde(default)]
    pub nothing_fires: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedRule {
    pub rule: String,
    /// Absent when the scenario does not care how urgent it reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

/// One situation, and what should be said about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scenario {
    pub id: String,
    pub title: String,
    /// Why this case matters — carried so a failure explains itself rather
    /// than being a name somebody has to go and look up.
    pub why: String,
    pub kind: ServiceKind,
    /// Config as the target would report it.
    #[serde(default)]
    pub config: serde_json::Value,
    /// The `via` string the document carries.
    ///
    /// Some rules locate their document by a substring of this — the container
    /// rules look for `HostConfig`, the workload rules for `apps/v1` — because
    /// those payloads have no other marker saying what they are. That coupling
    /// is brittle and worth removing, and until it is, a scenario has to
    /// reproduce the wording a collector actually emits or the rule will not
    /// find its own input. Declaring it here is also simply more faithful: a
    /// scenario should look like the real thing.
    #[serde(default = "default_via")]
    pub config_via: String,
    /// A second document with a File origin, for drift scenarios.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_on_disk: Option<serde_json::Value>,
    #[serde(default)]
    pub series: Vec<SeriesSpec>,
    pub expect: Expectation,
}

/// Why a scenario did not pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Failure {
    /// An expected rule did not fire. The dangerous regression.
    Missed { rule: String },
    /// A rule fired that should not have. The noise regression.
    Spurious { rule: String, title: String },
    /// Right rule, wrong urgency. Not a miss, but a finding that reads as a
    /// planning note when it is an outage is close to one.
    WrongSeverity {
        rule: String,
        expected: Severity,
        actual: Severity,
    },
}

/// What produced the findings being graded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Engine {
    Rules,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub id: String,
    pub title: String,
    pub why: String,
    pub kind: ServiceKind,
    pub engine: Engine,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Failure>,
    /// Everything that fired, so a failure can be read without re-running.
    pub fired: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteResult {
    pub total: usize,
    pub passed: usize,
    /// Counted apart, because a suite that is drifting noisier and one that is
    /// drifting blinder need different responses.
    pub missed: usize,
    pub spurious: usize,
    pub wrong_severity: usize,
    pub outcomes: Vec<Outcome>,
}

/// The scenarios that ship with the product.
pub fn library() -> Result<Vec<Scenario>, String> {
    serde_json::from_str(BUILT_IN).map_err(|e| format!("built-in scenario library is invalid: {e}"))
}

/// Run every scenario and report what happened.
pub fn run_all(scenarios: &[Scenario]) -> SuiteResult {
    let outcomes: Vec<Outcome> = scenarios.iter().map(run).collect();

    SuiteResult {
        total: outcomes.len(),
        passed: outcomes.iter().filter(|o| o.passed).count(),
        missed: count(&outcomes, |f| matches!(f, Failure::Missed { .. })),
        spurious: count(&outcomes, |f| matches!(f, Failure::Spurious { .. })),
        wrong_severity: count(&outcomes, |f| matches!(f, Failure::WrongSeverity { .. })),
        outcomes,
    }
}

fn count(outcomes: &[Outcome], matches: impl Fn(&Failure) -> bool) -> usize {
    outcomes
        .iter()
        .flat_map(|o| &o.failures)
        .filter(|f| matches(f))
        .count()
}

/// Run one scenario.
pub fn run(scenario: &Scenario) -> Outcome {
    let end = Utc::now();
    let series: Vec<MetricSeries> = scenario
        .series
        .iter()
        .map(|spec| build_series(spec, end))
        .collect();

    let snapshot = HealthSnapshot {
        profile_id: format!("scenario:{}", scenario.id),
        profile_name: scenario.title.clone(),
        kind: scenario.kind,
        health: Health::Healthy,
        summary: String::new(),
        metrics: scenario
            .series
            .iter()
            .zip(&series)
            .map(|(spec, built)| Metric {
                key: spec.key.clone(),
                label: spec.label.clone().unwrap_or_else(|| spec.key.clone()),
                value: built.latest,
                unit: spec.unit,
                warn_above: spec.warn_above,
            })
            .collect(),
        sections: Vec::new(),
        checked_at: end,
        latency_ms: 1,
        error: None,
    };

    let mut documents = Vec::new();
    if !scenario.config.is_null() {
        documents.push(ConfigDocument::live(
            &snapshot.profile_id,
            "Running configuration",
            &scenario.config_via,
            &scenario.config,
        ));
    }
    if let Some(on_disk) = &scenario.config_on_disk {
        let mut doc = ConfigDocument::live(
            &snapshot.profile_id,
            "scenario.conf",
            "scenario (file)",
            on_disk,
        );
        // `live()` marks its origin Live; the drift rule needs a File to
        // compare against, and a scenario expressing drift has to produce one.
        doc.origin = crate::configsource::Origin::File {
            path: "/etc/scenario.conf".to_string(),
        };
        documents.push(doc);
    }

    let found = advisor::findings(&snapshot, &documents, &series);
    let fired: Vec<String> = found.iter().map(|f| f.rule.clone()).collect();
    let mut failures = Vec::new();

    // ---- what should have fired
    for expected in &scenario.expect.fires {
        match found.iter().find(|f| rule_matches(&f.rule, &expected.rule)) {
            None => failures.push(Failure::Missed {
                rule: expected.rule.clone(),
            }),
            Some(finding) => {
                if let Some(severity) = expected.severity {
                    if finding.severity != severity {
                        failures.push(Failure::WrongSeverity {
                            rule: expected.rule.clone(),
                            expected: severity,
                            actual: finding.severity,
                        });
                    }
                }
            }
        }
    }

    // ---- what should not have
    for forbidden in &scenario.expect.silent {
        if let Some(finding) = found.iter().find(|f| rule_matches(&f.rule, forbidden)) {
            failures.push(Failure::Spurious {
                rule: finding.rule.clone(),
                title: finding.title.clone(),
            });
        }
    }

    // The strongest form, and the only one that catches a new rule that is
    // wrong: nothing at all may fire.
    if scenario.expect.nothing_fires {
        for finding in &found {
            failures.push(Failure::Spurious {
                rule: finding.rule.clone(),
                title: finding.title.clone(),
            });
        }
    }

    Outcome {
        id: scenario.id.clone(),
        title: scenario.title.clone(),
        why: scenario.why.clone(),
        kind: scenario.kind,
        engine: Engine::Rules,
        passed: failures.is_empty(),
        failures,
        fired,
    }
}

/// Whether a fired rule satisfies an expectation.
///
/// Suffix match, so a scenario survives a rule moving namespace — the identity
/// being asserted is the behaviour (`maxmemory-noeviction`), not where it
/// currently lives. An exact match would make every rename look like a
/// regression, and a library that cries wolf on renames gets ignored.
fn rule_matches(fired: &str, expected: &str) -> bool {
    fired == expected || fired.ends_with(&format!(".{expected}"))
}

/// How far a `Linear` series strays from its own line, as a fraction of the
/// climb.
///
/// Tuned so the fit lands around 0.9 — messy enough that a gate raised much
/// past the real one starts rejecting it, clean enough that the rule under
/// test is what is being measured rather than the gate.
const SCATTER: f64 = 0.06;

/// Scatter for `NoisyClimb`.
///
/// Chosen by measurement rather than derivation. The closed form for uniform
/// noise predicted 0.6 and produced a fit of 0.15 — below the gate, so the
/// scenario failed for being too noisy rather than testing anything. `wobble`
/// is not uniformly distributed and the arithmetic that assumed it was is not
/// worth repairing when the value can simply be measured; the bracketing test
/// pins the result, which is the part that matters.
const SCATTER_HEAVY: f64 = 0.28;

/// Deterministic scatter in `[-1, 1]`.
///
/// Not a random number generator: a scenario that produced a different series
/// on each run could pass and fail for reasons nobody could reproduce, which
/// is the one thing a regression suite may not do.
fn wobble(i: i64) -> f64 {
    let x = (i as f64) * 12.9898;
    let noise = (x.sin() * 43758.5453).fract();
    noise * 2.0 - 1.0
}

fn build_series(spec: &SeriesSpec, end: chrono::DateTime<Utc>) -> MetricSeries {
    let hours = spec.hours.max(1);
    let start = end - Duration::hours(hours);
    let step_ms = (hours * 3_600_000) / SAMPLES;

    let points: Vec<Point> = (0..SAMPLES)
        .map(|i| {
            let progress = i as f64 / (SAMPLES - 1) as f64;
            let value = match spec.shape {
                Shape::CleanLinear => spec.from + (spec.to - spec.from) * progress,
                Shape::Linear => {
                    let line = spec.from + (spec.to - spec.from) * progress;
                    line + wobble(i) * (spec.to - spec.from).abs() * SCATTER
                }
                Shape::NoisyClimb => {
                    let line = spec.from + (spec.to - spec.from) * progress;
                    line + wobble(i) * (spec.to - spec.from).abs() * SCATTER_HEAVY
                }
                Shape::Flat => spec.to,
                Shape::Sawtooth => {
                    if i % 2 == 0 {
                        spec.from
                    } else {
                        spec.to
                    }
                }
                Shape::Step => {
                    if progress < 0.8 {
                        spec.from
                    } else {
                        spec.to
                    }
                }
                Shape::LateClimb => {
                    if progress < 0.9 {
                        spec.from
                    } else {
                        spec.from + (spec.to - spec.from) * ((progress - 0.9) / 0.1)
                    }
                }
            };
            Point {
                t: start + Duration::milliseconds(i * step_ms),
                v: value,
            }
        })
        .collect();

    let values: Vec<f64> = points.iter().map(|p| p.v).collect();
    MetricSeries {
        key: spec.key.clone(),
        label: spec.label.clone().unwrap_or_else(|| spec.key.clone()),
        unit: spec.unit,
        warn_above: spec.warn_above,
        change_pct: None,
        min: values.iter().copied().fold(f64::INFINITY, f64::min),
        max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        latest: *values.last().unwrap_or(&0.0),
        trend: crate::forecast::trend(&points, spec.warn_above),
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library must parse. A malformed scenario file would otherwise be
    /// discovered by an operator pressing a button.
    #[test]
    fn the_built_in_library_parses() {
        let scenarios = library().expect("valid library");
        assert!(scenarios.len() >= 10, "got {}", scenarios.len());
    }

    /// Ids are how a failure is referred to and how a scenario is added to
    /// without being duplicated.
    #[test]
    fn every_scenario_has_a_unique_id_and_says_why_it_matters() {
        let scenarios = library().unwrap();
        let mut ids: Vec<&str> = scenarios.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate scenario id");

        for scenario in &scenarios {
            assert!(!scenario.why.trim().is_empty(), "{} has no why", scenario.id);
            assert!(
                scenario.why.len() > 40,
                "{} explains itself too thinly to be useful in a failure report",
                scenario.id
            );
        }
    }

    /// The whole library must pass against the rules as they stand. A shipped
    /// baseline that does not is not a baseline.
    #[test]
    fn the_library_passes_against_the_current_rules() {
        let result = run_all(&library().unwrap());
        assert_eq!(
            result.passed, result.total,
            "regressions: {:#?}",
            result
                .outcomes
                .iter()
                .filter(|o| !o.passed)
                .collect::<Vec<_>>()
        );
    }

    /// Both halves are covered, or the library drifts noisier with every fix
    /// for a miss.
    #[test]
    fn the_library_checks_silence_as_well_as_detection() {
        let scenarios = library().unwrap();
        let quiet = scenarios
            .iter()
            .filter(|s| s.expect.nothing_fires || !s.expect.silent.is_empty())
            .count();
        assert!(
            quiet >= 4,
            "only {quiet} scenarios assert silence — a detection-only library gets noisy"
        );
    }

    // ------------------------------------------------------- the harness

    fn redis_filling() -> Scenario {
        Scenario {
            id: "t".into(),
            title: "t".into(),
            why: "a test fixture that exists only to exercise the harness itself".into(),
            kind: ServiceKind::Redis,
            config: serde_json::json!({
                "maxmemory": "2147483648",
                "maxmemory-policy": "noeviction",
            }),
            config_via: default_via(),
            config_on_disk: None,
            series: vec![SeriesSpec {
                key: "memory".into(),
                label: Some("Memory".into()),
                unit: MetricUnit::Bytes,
                warn_above: Some(2_147_483_648.0),
                from: 500_000_000.0,
                to: 1_950_000_000.0,
                shape: Shape::Linear,
                hours: 24,
            }],
            expect: Expectation {
                fires: vec![ExpectedRule {
                    rule: "maxmemory-noeviction".into(),
                    severity: None,
                }],
                silent: vec![],
                nothing_fires: false,
            },
        }
    }

    #[test]
    fn a_satisfied_expectation_passes() {
        let outcome = run(&redis_filling());
        assert!(outcome.passed, "{outcome:#?}");
        assert!(outcome.fired.iter().any(|r| r.contains("maxmemory")));
    }

    /// The dangerous regression: a rule that used to catch this stops.
    #[test]
    fn a_rule_that_does_not_fire_is_reported_as_missed() {
        let mut scenario = redis_filling();
        scenario.expect.fires = vec![ExpectedRule {
            rule: "a-rule-that-does-not-exist".into(),
            severity: None,
        }];

        let outcome = run(&scenario);
        assert!(!outcome.passed);
        assert!(matches!(outcome.failures[0], Failure::Missed { .. }));
    }

    /// The noise regression, which a detection-only library never sees.
    #[test]
    fn a_rule_that_should_have_stayed_quiet_is_reported_as_spurious() {
        let mut scenario = redis_filling();
        scenario.expect.silent = vec!["maxmemory-noeviction".into()];

        let outcome = run(&scenario);
        assert!(!outcome.passed);
        assert!(matches!(outcome.failures[0], Failure::Spurious { .. }));
    }

    /// A finding that reads as a planning note when it is an outage is nearly
    /// a miss, and is reported apart from one.
    #[test]
    fn firing_at_the_wrong_urgency_is_its_own_failure() {
        let mut scenario = redis_filling();
        scenario.expect.fires = vec![ExpectedRule {
            rule: "maxmemory-noeviction".into(),
            severity: Some(Severity::Advisory),
        }];

        let outcome = run(&scenario);
        assert!(!outcome.passed);
        assert!(matches!(
            outcome.failures[0],
            Failure::WrongSeverity { .. }
        ));
    }

    /// The only form that catches a brand new rule that is wrong.
    #[test]
    fn nothing_fires_catches_a_rule_no_scenario_named() {
        let mut scenario = redis_filling();
        scenario.expect.fires = vec![];
        scenario.expect.nothing_fires = true;

        let outcome = run(&scenario);
        assert!(!outcome.passed, "something did fire");
        assert!(outcome
            .failures
            .iter()
            .all(|f| matches!(f, Failure::Spurious { .. })));
    }

    /// A library that cried wolf on every rename would be ignored.
    #[test]
    fn a_renamed_namespace_does_not_read_as_a_regression() {
        assert!(rule_matches("caching.maxmemory-noeviction", "maxmemory-noeviction"));
        assert!(rule_matches("redis.maxmemory-noeviction", "maxmemory-noeviction"));
        assert!(rule_matches("redis.maxmemory-noeviction", "redis.maxmemory-noeviction"));
        // But a genuinely different rule still fails.
        assert!(!rule_matches("redis.maxmemory-unset", "maxmemory-noeviction"));
        // And a prefix is not a suffix.
        assert!(!rule_matches("redis.maxmemory-noeviction-near", "maxmemory-noeviction"));
    }

    // --------------------------------------------------------------- shapes

    /// The shape the forecaster exists to refuse. If a sawtooth fitted, every
    /// "should stay quiet" scenario built on one would be testing nothing.
    #[test]
    fn a_sawtooth_produces_a_fit_too_poor_to_project() {
        let series = build_series(
            &SeriesSpec {
                key: "cpu".into(),
                label: None,
                unit: MetricUnit::Percent,
                warn_above: Some(90.0),
                from: 20.0,
                to: 80.0,
                shape: Shape::Sawtooth,
                hours: 24,
            },
            Utc::now(),
        );
        let trend = series.trend.expect("a trend exists");
        assert!(trend.fit < 0.25, "fit was {}", trend.fit);
        assert_eq!(trend.seconds_to_limit, None);
    }

    /// The gap this library had: every climbing scenario fitted perfectly, so
    /// none of them could notice the fit gate moving. A realistic climb sits
    /// near the gate, not at 1.0.
    #[test]
    fn a_linear_climb_has_a_fit_a_real_metric_could_have() {
        let series = build_series(
            &SeriesSpec {
                key: "memory".into(),
                label: None,
                unit: MetricUnit::Bytes,
                warn_above: Some(100.0),
                from: 10.0,
                to: 90.0,
                shape: Shape::Linear,
                hours: 24,
            },
            Utc::now(),
        );
        let trend = series.trend.expect("a trend");
        assert!(
            trend.fit > 0.8 && trend.fit < 0.99,
            "a real climb is neither perfect nor noise: fit was {}",
            trend.fit
        );
        assert!(trend.seconds_to_limit.is_some());
    }

    /// The three points that bracket the fit gate. Without the middle one the
    /// library cannot tell a gate at 0.25 from a gate at 0.9.
    #[test]
    fn the_shapes_bracket_the_fit_gate() {
        let fit = |shape: Shape| {
            build_series(
                &SeriesSpec {
                    key: "m".into(),
                    label: None,
                    unit: MetricUnit::Bytes,
                    warn_above: Some(100.0),
                    from: 10.0,
                    to: 90.0,
                    shape,
                    hours: 24,
                },
                Utc::now(),
            )
            .trend
            .expect("a trend")
            .fit
        };

        let clean = fit(Shape::Linear);
        let noisy = fit(Shape::NoisyClimb);
        let refused = fit(Shape::Sawtooth);

        assert!(clean > 0.9, "a tidy climb should fit well: {clean}");
        assert!(
            noisy > 0.25 && noisy < 0.7,
            "the middle case must sit between the gate and a tightened one: {noisy}"
        );
        assert!(refused < 0.25, "a sawtooth must fail the gate: {refused}");
    }

    /// The same series every run. A suite that produced different data each
    /// time could pass and fail for reasons nobody could reproduce.
    #[test]
    fn generated_series_are_identical_between_runs() {
        let spec = SeriesSpec {
            key: "m".into(),
            label: None,
            unit: MetricUnit::Bytes,
            warn_above: Some(100.0),
            from: 10.0,
            to: 90.0,
            shape: Shape::Linear,
            hours: 24,
        };
        let now = Utc::now();
        let a = build_series(&spec, now);
        let b = build_series(&spec, now);
        assert_eq!(
            a.points.iter().map(|p| p.v).collect::<Vec<_>>(),
            b.points.iter().map(|p| p.v).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_clean_linear_climb_produces_a_fit_good_enough_to_project() {
        let series = build_series(
            &SeriesSpec {
                key: "memory".into(),
                label: None,
                unit: MetricUnit::Bytes,
                warn_above: Some(100.0),
                from: 10.0,
                to: 90.0,
                shape: Shape::CleanLinear,
                hours: 24,
            },
            Utc::now(),
        );
        let trend = series.trend.expect("a trend");
        assert!(trend.fit > 0.99);
        assert!(trend.seconds_to_limit.is_some());
    }

    /// A change with no runway. A straight line through it has a slope and
    /// says nothing useful, which is the point of having the shape.
    #[test]
    fn a_step_is_not_mistaken_for_a_trend() {
        let series = build_series(
            &SeriesSpec {
                key: "memory".into(),
                label: None,
                unit: MetricUnit::Bytes,
                warn_above: Some(100.0),
                from: 10.0,
                to: 40.0,
                shape: Shape::Step,
                hours: 24,
            },
            Utc::now(),
        );
        let trend = series.trend.expect("a trend");
        // It fits a line poorly enough that the projection is not trusted, or
        // projects so far out it is meaningless — either is acceptable; what
        // matters is that it does not read as an imminent breach.
        let soon = trend
            .seconds_to_limit
            .is_some_and(|s| s < 24.0 * 3600.0);
        assert!(!soon, "a step read as an imminent breach: {trend:?}");
    }

    #[test]
    fn a_flat_series_has_no_destination() {
        let series = build_series(
            &SeriesSpec {
                key: "memory".into(),
                label: None,
                unit: MetricUnit::Bytes,
                warn_above: Some(100.0),
                from: 0.0,
                to: 20.0,
                shape: Shape::Flat,
                hours: 24,
            },
            Utc::now(),
        );
        assert_eq!(series.latest, 20.0);
        assert_eq!(series.trend.unwrap().seconds_to_limit, None);
    }

    #[test]
    fn generated_series_span_the_window_they_declare() {
        let series = build_series(
            &SeriesSpec {
                key: "m".into(),
                label: None,
                unit: MetricUnit::Count,
                warn_above: None,
                from: 0.0,
                to: 1.0,
                shape: Shape::Linear,
                hours: 48,
            },
            Utc::now(),
        );
        let span = (series.points.last().unwrap().t - series.points[0].t).num_hours();
        assert!((span - 48).abs() <= 1, "spanned {span}h");
        assert_eq!(series.points.len(), SAMPLES as usize);
    }
}
