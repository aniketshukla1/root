//! Config read against telemetry, producing findings somebody can act on.
//!
//! Neither half is enough on its own, and that is the entire point of this
//! module.
//!
//! Telemetry alone says a Redis is at 1.9 GB and climbing. That is a fact with
//! no action attached: it might be fine forever, or it might be four hours
//! from an outage, and the metric cannot tell you which. Config alone says
//! `maxmemory 2gb` and `maxmemory-policy noeviction`. Also fine — that is a
//! perfectly reasonable configuration for an instance that never fills.
//!
//! Together they say: this instance stops accepting writes in about four
//! hours, here is the setting that decides it, here is the number to change.
//!
//! ## Why these are rules and not prompts
//!
//! The assistant could be handed the config and the metrics and asked to spot
//! this. It would, often. "Often" is the problem: a recommendation that is
//! usually right and occasionally invents a setting that does not exist is
//! worse than no recommendation, because the first time somebody runs one and
//! it breaks something, every later one gets ignored too.
//!
//! A rule states its evidence — the setting, its value, where it came from,
//! the metric, the fit behind the projection — and every part of that is
//! checkable by the person reading it. The assistant is better used to explain
//! a finding and weigh it against everything else it knows about the estate,
//! which is what it is actually good at.
//!
//! ## Findings are never applied
//!
//! A suggestion carries the command that would apply it, as text, for a human
//! to run. Nothing here writes config. An app that edits `postgresql.conf` on
//! the strength of a projection is a new and worse kind of outage, and one
//! with a blast radius of the whole estate rather than a single container.
//!
//! ## On the redacted body
//!
//! Rules read the same redacted text every other consumer gets. There is no
//! privileged path to the original, and there does not need to be: `maxmemory`,
//! `max_connections` and `restartPolicy` are settings, not credentials, and a
//! rule that needed to read a password would be a rule worth deleting.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::configsource::{ConfigDocument, Origin};
use crate::forecast::Trend;
use crate::history::MetricSeries;
use crate::model::{HealthSnapshot, MetricUnit, ServiceKind};

/// How much attention a finding deserves, and on what timescale.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Worth knowing. No deadline.
    Advisory,
    /// There is a projection, and it lands inside a week.
    Soon,
    /// Already true, or lands within hours.
    Now,
}

/// A fact a finding rests on. Rendered beside the finding so the reasoning can
/// be checked rather than believed.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Evidence {
    Setting {
        key: String,
        value: String,
        /// Which document it came from, and whether that was a file or the
        /// running process. A file and a live value disagreeing is itself
        /// usually the finding.
        source: String,
    },
    Metric {
        key: String,
        label: String,
        value: f64,
        unit: MetricUnit,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<f64>,
    },
    Projection {
        key: String,
        seconds_to_limit: f64,
        fit: f64,
        window_secs: f64,
    },
}

/// What to change, and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub setting: String,
    pub current: String,
    pub proposed: String,
    /// The command that would apply it. Text for a human to run — nothing in
    /// this app executes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply: Option<String>,
}

/// Where a finding came from.
///
/// Rules and the model fail differently — a rule never invents a setting but
/// only finds what somebody encoded; the model reaches further and can be
/// confidently wrong — so a score that mixes them describes neither. Kept as
/// a string rather than an enum so a rule id (`redis.maxmemory-noeviction`)
/// travels with it and a per-rule score is a `GROUP BY`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provenance {
    /// A deterministic rule, named.
    Rule { id: String },
    /// The assistant, unprompted by any rule.
    Model { model: String },
    /// The assistant restating a rule's finding. Scored apart from both,
    /// because counting it as a model win would credit the model for the
    /// rule's work.
    ModelCitingRule { model: String, rule: String },
}

impl Provenance {
    /// The column value: `rule:<id>`, `model:<name>`, `model+rule:<id>`.
    pub fn tag(&self) -> String {
        match self {
            Provenance::Rule { id } => format!("rule:{id}"),
            Provenance::Model { .. } => "model".to_string(),
            Provenance::ModelCitingRule { rule, .. } => format!("model+rule:{rule}"),
        }
    }

    /// The coarse bucket a scorecard groups by.
    pub fn family(&self) -> &'static str {
        match self {
            Provenance::Rule { .. } => "rule",
            Provenance::Model { .. } => "model",
            Provenance::ModelCitingRule { .. } => "model+rule",
        }
    }
}

/// One thing wrong, or about to be.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    /// Where this came from — a rule, the assistant, or the assistant
    /// restating a rule.
    ///
    /// Carried on the finding rather than attached when it is rated, so a
    /// verdict cannot be filed against the wrong engine. The three fail
    /// differently enough that a score mixing them describes none of them.
    pub provenance: Provenance,
    /// Which rule produced this, as a stable name.
    ///
    /// Carried so a verdict can be attributed to one rule rather than to
    /// "the rules": "rules are 80% precise" names nothing to fix, and "this
    /// one rule is 30% precise" names the thing. Stable across rewording —
    /// it is the rule's identity, not its current text.
    pub rule: String,
    pub profile_id: String,
    pub profile_name: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub evidence: Vec<Evidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<Suggestion>,
}

/// Settings flattened out of every document for one connection.
///
/// Live JSON documents and ini-style files both reduce to the same thing — a
/// key with a value and a note of where it came from — and a rule should not
/// have to care which shape it arrived in.
#[derive(Debug, Default)]
pub struct Settings {
    values: HashMap<String, Entry>,
}

/// One setting, keyed for lookup but remembered as it was written.
///
/// Lookup normalises away the `-`/`_` split that the same setting takes in
/// different formats, and that normalised form is unreadable — a finding that
/// said "maxmemorypolicy" would look like a typo in something that is asking
/// to be trusted about exact values.
#[derive(Debug, Clone)]
struct Entry {
    key: String,
    value: String,
    source: String,
}

impl Settings {
    pub fn from_documents(documents: &[ConfigDocument]) -> Self {
        let mut values = HashMap::new();
        for doc in documents {
            let Some(body) = &doc.body else { continue };
            let source = match &doc.origin {
                Origin::File { path } => path.clone(),
                Origin::Live { via } => via.clone(),
            };

            if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                flatten_json(&json, String::new(), &source, &mut values);
            } else {
                flatten_ini(body, &source, &mut values);
            }
        }
        Settings { values }
    }

    /// Case-insensitive, and tolerant of the `_`/`-`/`.` split that the same
    /// setting takes in different formats: `maxmemory-policy` in redis.conf is
    /// `maxmemory_policy` in half the tooling that talks about it.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(&normalise(key)).map(|e| e.value.as_str())
    }

    pub fn source_of(&self, key: &str) -> Option<&str> {
        self.values.get(&normalise(key)).map(|e| e.source.as_str())
    }

    pub fn evidence(&self, key: &str) -> Option<Evidence> {
        let entry = self.values.get(&normalise(key))?;
        Some(Evidence::Setting {
            key: entry.key.clone(),
            value: entry.value.clone(),
            source: entry.source.clone(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

fn normalise(key: &str) -> String {
    key.to_ascii_lowercase().replace(['-', '_'], "")
}

fn flatten_json(
    value: &serde_json::Value,
    prefix: String,
    source: &str,
    out: &mut HashMap<String, Entry>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_json(v, path, source, out);
            }
        }
        serde_json::Value::Null => {}
        other => {
            let rendered = match other {
                serde_json::Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            // The full dotted path AND the leaf, so a rule can ask for
            // `maxmemory` without knowing which document nested it where.
            out.insert(
                normalise(&prefix),
                Entry {
                    key: prefix.clone(),
                    value: rendered.clone(),
                    source: source.to_string(),
                },
            );
            if let Some(leaf) = prefix.rsplit('.').next() {
                out.entry(normalise(leaf)).or_insert(Entry {
                    key: leaf.to_string(),
                    value: rendered,
                    source: source.to_string(),
                });
            }
        }
    }
}

fn flatten_ini(body: &str, source: &str, out: &mut HashMap<String, Entry>) {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let line = line.trim_end_matches(';');
        let (key, value) = match line.split_once([' ', '=', ':', '\t']) {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        if key.is_empty() || value.is_empty() {
            continue;
        }
        out.entry(normalise(key)).or_insert(Entry {
            key: key.to_string(),
            value: value.trim_matches(['"', '\'']).to_string(),
            source: source.to_string(),
        });
    }
}

/// Everything one connection's config and telemetry say together.
pub fn findings(
    snapshot: &HealthSnapshot,
    documents: &[ConfigDocument],
    series: &[MetricSeries],
) -> Vec<Finding> {
    let settings = Settings::from_documents(documents);
    if settings.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();

    // Runs for every kind, because it is not about any of them: a file that
    // disagrees with the running process is the same bug on Redis, Postgres
    // and nginx, and it is the one nothing else can see.
    drift_rule(snapshot, documents, &mut out);

    match snapshot.kind {
        ServiceKind::Redis => redis_rules(snapshot, &settings, series, &mut out),
        ServiceKind::Postgres | ServiceKind::Mysql => {
            database_rules(snapshot, &settings, series, &mut out)
        }
        ServiceKind::Docker | ServiceKind::Podman => {
            container_rules(snapshot, documents, &mut out)
        }
        ServiceKind::Kubernetes => workload_rules(snapshot, documents, &mut out),
        _ => {}
    }

    out.sort_by(|a, b| b.severity.cmp(&a.severity));
    out
}

// ------------------------------------------------------------------- helpers

fn metric<'a>(snapshot: &'a HealthSnapshot, key: &str) -> Option<&'a crate::model::Metric> {
    snapshot.metrics.iter().find(|m| m.key == key)
}

fn trend_for<'a>(series: &'a [MetricSeries], key: &str) -> Option<&'a Trend> {
    series
        .iter()
        .find(|s| s.key == key)
        .and_then(|s| s.trend.as_ref())
}

/// Whether a projection is worth putting a deadline on, and how urgent.
fn urgency(seconds: f64) -> Severity {
    if seconds < 4.0 * 3600.0 {
        Severity::Now
    } else if seconds < 7.0 * 86_400.0 {
        Severity::Soon
    } else {
        Severity::Advisory
    }
}

fn horizon(seconds: f64) -> String {
    if seconds <= 0.0 {
        return "already".to_string();
    }
    let hours = seconds / 3600.0;
    if hours < 1.0 {
        format!("about {} minutes", (seconds / 60.0).round().max(1.0))
    } else if hours < 48.0 {
        format!("about {} hours", hours.round())
    } else {
        format!("about {} days", (hours / 24.0).round())
    }
}

/// Sizes as config writes them: `2gb`, `512mb`, `1073741824`.
fn parse_size(value: &str) -> Option<f64> {
    let v = value.trim().to_ascii_lowercase();
    if v.is_empty() {
        return None;
    }
    let (digits, multiplier) = match () {
        _ if v.ends_with("kb") || v.ends_with('k') => (v.trim_end_matches(['k', 'b']), 1024.0),
        _ if v.ends_with("mb") || v.ends_with('m') => {
            (v.trim_end_matches(['m', 'b']), 1024.0 * 1024.0)
        }
        _ if v.ends_with("gb") || v.ends_with('g') => {
            (v.trim_end_matches(['g', 'b']), 1024.0 * 1024.0 * 1024.0)
        }
        _ if v.ends_with("tb") || v.ends_with('t') => {
            (v.trim_end_matches(['t', 'b']), 1024f64.powi(4))
        }
        _ => (v.as_str(), 1.0),
    };
    digits.trim().parse::<f64>().ok().map(|n| n * multiplier)
}

fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn finding(
    snapshot: &HealthSnapshot,
    slug: &str,
    severity: Severity,
    title: String,
    detail: String,
    evidence: Vec<Evidence>,
    suggestion: Option<Suggestion>,
) -> Finding {
    let rule = format!("{}.{slug}", snapshot.kind.label().to_ascii_lowercase());
    Finding {
        id: format!("{}:{slug}", snapshot.profile_id),
        provenance: Provenance::Rule { id: rule.clone() },
        rule,
        profile_id: snapshot.profile_id.clone(),
        profile_name: snapshot.profile_name.clone(),
        severity,
        title,
        detail,
        evidence,
        suggestion,
    }
}

// --------------------------------------------------------------------- drift

/// Settings whose file value and running value disagree.
///
/// The commonest configuration bug there is, and the one a monitor cannot see
/// on its own: somebody edited the file, the change was correct, and nothing
/// reloaded. Every subsequent conversation about that service is then held
/// against a document that does not describe it — the file says the ceiling is
/// 4 GB, the process is enforcing 2 GB, and the person reading the file cannot
/// work out why it keeps filling.
///
/// It also cuts the other way, which is worse: a restart will silently adopt
/// whatever the file says, so a drifted service is carrying a scheduled change
/// nobody remembers making.
fn drift_rule(snapshot: &HealthSnapshot, documents: &[ConfigDocument], out: &mut Vec<Finding>) {
    let files: Vec<&ConfigDocument> = documents
        .iter()
        .filter(|d| matches!(d.origin, Origin::File { .. }))
        .collect();
    let live: Vec<&ConfigDocument> = documents
        .iter()
        .filter(|d| matches!(d.origin, Origin::Live { .. }))
        .collect();
    if files.is_empty() || live.is_empty() {
        return;
    }

    let from_file = Settings::from_documents(&files.into_iter().cloned().collect::<Vec<_>>());
    let from_live = Settings::from_documents(&live.into_iter().cloned().collect::<Vec<_>>());

    let mut drifted: Vec<(String, String, String)> = Vec::new();
    for (key, on_disk) in &from_file.values {
        let Some(running) = from_live.values.get(key) else {
            continue;
        };
        if same_value(&on_disk.value, &running.value) {
            continue;
        }
        // A redacted value is equal to every other redacted value and to
        // nothing else. Reporting one as drift would be reporting the
        // redaction rather than the setting.
        if on_disk.value.contains('«') || running.value.contains('«') {
            continue;
        }
        // The file's spelling: it is the one a person will go and edit.
        drifted.push((
            on_disk.key.clone(),
            on_disk.value.clone(),
            running.value.clone(),
        ));
    }

    if drifted.is_empty() {
        return;
    }
    drifted.sort();

    let evidence = drifted
        .iter()
        .take(8)
        .flat_map(|(key, file_value, live_value)| {
            [
                Evidence::Setting {
                    key: key.clone(),
                    value: file_value.clone(),
                    source: "on disk".to_string(),
                },
                Evidence::Setting {
                    key: key.clone(),
                    value: live_value.clone(),
                    source: "running".to_string(),
                },
            ]
        })
        .collect();

    let names = drifted
        .iter()
        .take(4)
        .map(|(k, _, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    out.push(finding(
        snapshot,
        "config-drift",
        Severity::Soon,
        format!(
            "{} setting{} on disk {} not what is running",
            drifted.len(),
            if drifted.len() == 1 { "" } else { "s" },
            if drifted.len() == 1 { "is" } else { "are" },
        ),
        format!(
            "{names} differ between the file and the running process. Usually this means an \
             edit that nothing reloaded — the change is correct and has never taken effect, \
             and anyone reading the file is reasoning about a service that does not match \
             it. It also means a restart will adopt the file's values without anyone \
             deciding to, so this is a change already scheduled for the next time this \
             process comes up."
        ),
        evidence,
        Some(Suggestion {
            setting: "reload".to_string(),
            current: "file and process disagree".to_string(),
            proposed: "reload to adopt the file, or correct the file to match the process"
                .to_string(),
            apply: None,
        }),
    ));
}

/// Whether two written forms of a setting mean the same thing.
///
/// `2147483648` and `2gb` are the same ceiling written by two tools, and a
/// rule that called that drift would fire on every Redis in existence.
fn same_value(a: &str, b: &str) -> bool {
    if a.trim().eq_ignore_ascii_case(b.trim()) {
        return true;
    }
    match (parse_size(a), parse_size(b)) {
        (Some(x), Some(y)) => (x - y).abs() < f64::EPSILON,
        _ => false,
    }
}

// --------------------------------------------------------------------- redis

fn redis_rules(
    snapshot: &HealthSnapshot,
    settings: &Settings,
    series: &[MetricSeries],
    out: &mut Vec<Finding>,
) {
    let policy = settings.get("maxmemory-policy").unwrap_or("noeviction");
    let configured = settings.get("maxmemory").and_then(parse_size);
    let used = metric(snapshot, "memory").map(|m| m.value);

    // No ceiling at all. The instance will grow until the host has no memory
    // left, and then the kernel picks what dies — which on a shared host is
    // frequently not Redis.
    if configured == Some(0.0) {
        out.push(finding(
            snapshot,
            "maxmemory-unset",
            Severity::Advisory,
            "No memory ceiling is set".to_string(),
            "`maxmemory 0` means unbounded. This instance will keep allocating until the \
             host runs out, at which point the kernel's OOM killer chooses a process — \
             and it does not have to choose this one. A ceiling turns a host-wide \
             incident into a single-service one."
                .to_string(),
            settings.evidence("maxmemory").into_iter().collect(),
            Some(Suggestion {
                setting: "maxmemory".to_string(),
                current: "0 (unbounded)".to_string(),
                proposed: "roughly 70% of the memory this host can spare".to_string(),
                apply: Some("redis-cli CONFIG SET maxmemory <bytes>".to_string()),
            }),
        ));
        return;
    }

    let (Some(limit), Some(used)) = (configured, used) else {
        return;
    };
    if limit <= 0.0 {
        return;
    }

    // The finding this module exists for. The policy decides whether filling
    // up is a degradation or an outage, and the default is the outage.
    let refuses_writes = policy == "noeviction";
    let trend = trend_for(series, "memory");
    let seconds = trend.and_then(|t| t.seconds_to_ceiling(used, limit));

    if let Some(seconds) = seconds {
        let severity = urgency(seconds);
        let mut evidence = vec![Evidence::Metric {
            key: "memory".to_string(),
            label: "Memory".to_string(),
            value: used,
            unit: MetricUnit::Bytes,
            limit: Some(limit),
        }];
        evidence.extend(settings.evidence("maxmemory"));
        evidence.extend(settings.evidence("maxmemory-policy"));
        if let Some(t) = trend {
            evidence.push(Evidence::Projection {
                key: "memory".to_string(),
                seconds_to_limit: seconds,
                fit: t.fit,
                window_secs: t.window_secs,
            });
        }

        let when = horizon(seconds);
        out.push(if refuses_writes {
            finding(
                snapshot,
                "maxmemory-noeviction",
                severity,
                format!("Writes will start failing in {when}"),
                format!(
                    "Memory is at {} of a {} ceiling and rising. `maxmemory-policy` is \
                     `noeviction`, so when it arrives Redis does not evict anything — it \
                     returns OOM on every write and keeps serving reads. Applications \
                     that treat Redis as a cache usually survive that badly, because a \
                     failed write looks like a bug rather than a full cache.",
                    human_bytes(used),
                    human_bytes(limit),
                ),
                evidence,
                Some(Suggestion {
                    setting: "maxmemory-policy".to_string(),
                    current: policy.to_string(),
                    proposed: "allkeys-lru, if this instance is a cache".to_string(),
                    apply: Some(
                        "redis-cli CONFIG SET maxmemory-policy allkeys-lru   # cache \
                         semantics: evict rather than refuse\n\
                         redis-cli CONFIG SET maxmemory <larger>            # or raise \
                         the ceiling, if the data is not disposable"
                            .to_string(),
                    ),
                }),
            )
        } else {
            finding(
                snapshot,
                "maxmemory-eviction",
                match severity {
                    // Eviction is the designed behaviour, not an incident. It
                    // is still worth knowing that the working set has outgrown
                    // the instance, because the hit rate goes with it.
                    Severity::Now => Severity::Soon,
                    other => other,
                },
                format!("Eviction starts in {when}"),
                format!(
                    "Memory reaches its {} ceiling in {when} at the current rate. \
                     `maxmemory-policy` is `{policy}`, so Redis will evict rather than \
                     fail — the service stays up and the hit rate falls. Worth sizing up \
                     if this instance is load-bearing.",
                    human_bytes(limit),
                ),
                evidence,
                Some(Suggestion {
                    setting: "maxmemory".to_string(),
                    current: human_bytes(limit),
                    proposed: human_bytes(limit * 1.5),
                    apply: Some(format!(
                        "redis-cli CONFIG SET maxmemory {}",
                        (limit * 1.5) as u64
                    )),
                }),
            )
        });
    } else if used >= limit * 0.9 && refuses_writes {
        out.push(finding(
            snapshot,
            "maxmemory-noeviction-near",
            Severity::Soon,
            "Close to the ceiling, and configured to refuse writes".to_string(),
            format!(
                "Memory is at {} of {}. There is not enough of a trend to say when it \
                 arrives, but `maxmemory-policy noeviction` means arriving is an outage \
                 rather than a slowdown.",
                human_bytes(used),
                human_bytes(limit),
            ),
            settings
                .evidence("maxmemory-policy")
                .into_iter()
                .collect(),
            None,
        ));
    }
}

// ------------------------------------------------------------------ database

fn database_rules(
    snapshot: &HealthSnapshot,
    settings: &Settings,
    series: &[MetricSeries],
    out: &mut Vec<Finding>,
) {
    let Some(max) = settings
        .get("max_connections")
        .and_then(|v| v.trim().parse::<f64>().ok())
    else {
        return;
    };
    let Some(current) = metric(snapshot, "connections").map(|m| m.value) else {
        return;
    };

    let trend = trend_for(series, "connections");
    let seconds = trend.and_then(|t| t.seconds_to_ceiling(current, max));

    // Exhausting the pool is not a slow degradation. It is the point at which
    // new sessions are refused, including the ones an operator opens to find
    // out what is happening.
    if let Some(seconds) = seconds {
        let mut evidence = vec![Evidence::Metric {
            key: "connections".to_string(),
            label: "Connections".to_string(),
            value: current,
            unit: MetricUnit::Count,
            limit: Some(max),
        }];
        evidence.extend(settings.evidence("max_connections"));
        if let Some(t) = trend {
            evidence.push(Evidence::Projection {
                key: "connections".to_string(),
                seconds_to_limit: seconds,
                fit: t.fit,
                window_secs: t.window_secs,
            });
        }

        let source = settings.source_of("max_connections").unwrap_or("config");
        // The pooler differs by engine, and naming the wrong one is how a
        // suggestion gets dismissed along with every other suggestion.
        let pooler = match snapshot.kind {
            ServiceKind::Mysql => "ProxySQL",
            _ => "PgBouncer",
        };
        out.push(finding(
            snapshot,
            "max-connections",
            urgency(seconds),
            format!("Connection slots run out in {}", horizon(seconds)),
            format!(
                "{current:.0} of {max:.0} slots are in use and the count is climbing. When \
                 it arrives, new connections are refused — including the psql session \
                 somebody opens to find out why. Raising `max_connections` costs memory \
                 per slot; {pooler} in front costs none, and is usually the right answer \
                 when the climb is an application leaking sessions rather than real load. \
                 Current value read from {source}."
            ),
            evidence,
            Some(Suggestion {
                setting: "max_connections".to_string(),
                current: format!("{max:.0}"),
                proposed: format!("{:.0}, or put {pooler} in front", (max * 1.5).ceil()),
                apply: Some(match snapshot.kind {
                    ServiceKind::Mysql => {
                        "SET GLOBAL max_connections = <n>;   -- until restart\n\
                         # and max_connections = <n> in my.cnf to survive one"
                            .to_string()
                    }
                    _ => "ALTER SYSTEM SET max_connections = <n>;  -- needs a restart"
                        .to_string(),
                }),
            }),
        ));
    }
}

// ---------------------------------------------------------------- containers

fn container_rules(
    snapshot: &HealthSnapshot,
    documents: &[ConfigDocument],
    out: &mut Vec<Finding>,
) {
    let Some(body) = documents
        .iter()
        .find(|d| matches!(&d.origin, Origin::Live { via } if via.contains("HostConfig")))
        .and_then(|d| d.body.as_deref())
    else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let Some(containers) = parsed.as_object() else {
        return;
    };

    let unlimited: Vec<&String> = containers
        .iter()
        .filter(|(_, v)| {
            // Absent and zero both mean no limit; bollard reports one or the
            // other depending on engine version.
            v.get("memoryLimitBytes")
                .and_then(|m| m.as_i64())
                .unwrap_or(0)
                == 0
        })
        .map(|(k, _)| k)
        .collect();

    if !unlimited.is_empty() {
        let names = unlimited
            .iter()
            .take(6)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let more = unlimited.len().saturating_sub(6);
        out.push(finding(
            snapshot,
            "container-no-memory-limit",
            Severity::Advisory,
            format!(
                "{} container{} can use all the host's memory",
                unlimited.len(),
                if unlimited.len() == 1 { "" } else { "s" }
            ),
            format!(
                "{names}{} run with no memory limit. A leak in any one of them is a \
                 host-wide event rather than a container-wide one: the kernel's OOM \
                 killer picks by score, and it regularly picks a healthy neighbour over \
                 the process that is actually growing. A limit turns that into a restart \
                 of the thing at fault.",
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            ),
            vec![Evidence::Setting {
                key: "HostConfig.Memory".to_string(),
                value: "0 (unlimited)".to_string(),
                source: "inspect (HostConfig)".to_string(),
            }],
            Some(Suggestion {
                setting: "memory limit".to_string(),
                current: "unlimited".to_string(),
                proposed: "a ceiling per container".to_string(),
                apply: Some(
                    "docker update --memory 2g --memory-swap 2g <container>   # or set \
                     mem_limit in the compose file"
                        .to_string(),
                ),
            }),
        ));
    }

    // A container that has already proved it can die, configured not to come
    // back. This is only worth saying because it is invisible until 3am.
    let no_restart: Vec<&String> = containers
        .iter()
        .filter(|(_, v)| {
            v.get("restartPolicy")
                .and_then(|p| p.as_str())
                .is_some_and(|p| p.eq_ignore_ascii_case("\"\"") || p.to_lowercase().contains("no"))
        })
        .map(|(k, _)| k)
        .collect();

    if !no_restart.is_empty() {
        out.push(finding(
            snapshot,
            "container-no-restart-policy",
            Severity::Advisory,
            format!(
                "{} container{} will not come back after a crash",
                no_restart.len(),
                if no_restart.len() == 1 { "" } else { "s" }
            ),
            format!(
                "{} {} no restart policy. If the process exits — OOM, a panic, a bad \
                 deploy — it stays down until somebody notices and starts it by hand.",
                no_restart
                    .iter()
                    .take(6)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if no_restart.len() == 1 { "has" } else { "have" },
            ),
            vec![Evidence::Setting {
                key: "HostConfig.RestartPolicy".to_string(),
                value: "no".to_string(),
                source: "inspect (HostConfig)".to_string(),
            }],
            Some(Suggestion {
                setting: "restart policy".to_string(),
                current: "no".to_string(),
                proposed: "unless-stopped".to_string(),
                apply: Some("docker update --restart unless-stopped <container>".to_string()),
            }),
        ));
    }
}

// ----------------------------------------------------------------- workloads

fn workload_rules(
    snapshot: &HealthSnapshot,
    documents: &[ConfigDocument],
    out: &mut Vec<Finding>,
) {
    let Some(body) = documents
        .iter()
        .find(|d| matches!(&d.origin, Origin::Live { via } if via.contains("apps/v1")))
        .and_then(|d| d.body.as_deref())
    else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    let Some(workloads) = parsed.as_object() else {
        return;
    };

    let mut single: Vec<&str> = Vec::new();
    let mut unlimited: Vec<String> = Vec::new();
    let mut unrequested: Vec<String> = Vec::new();

    for (name, spec) in workloads {
        if spec.get("replicas").and_then(|r| r.as_i64()) == Some(1) {
            single.push(name);
        }
        let Some(containers) = spec.get("containers").and_then(|c| c.as_object()) else {
            continue;
        };
        for (container, detail) in containers {
            let at = |side: &str, key: &str| {
                detail
                    .get(side)
                    .and_then(|s| s.get(key))
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            };
            if at("limits", "memory").is_none() {
                unlimited.push(format!("{name}/{container}"));
            }
            if at("requests", "memory").is_none() && at("requests", "cpu").is_none() {
                unrequested.push(format!("{name}/{container}"));
            }
        }
    }

    // Requests are what the scheduler places on. A container with none is
    // BestEffort, which is a queue position, not a resource decision.
    if !unrequested.is_empty() {
        out.push(finding(
            snapshot,
            "workload-besteffort",
            Severity::Soon,
            format!(
                "{} container{} scheduled as BestEffort",
                unrequested.len(),
                if unrequested.len() == 1 { "" } else { "s" }
            ),
            format!(
                "{} {} neither CPU nor memory requests. Kubernetes places them wherever \
                 there is room and evicts them first when a node comes under pressure, \
                 regardless of how important they are. This is the setting that decides \
                 whether a node running short takes out a batch job or your checkout \
                 service.",
                join_with_overflow(&unrequested, 6),
                if unrequested.len() == 1 { "has" } else { "have" },
            ),
            vec![Evidence::Setting {
                key: "resources.requests".to_string(),
                value: "unset".to_string(),
                source: "apps/v1 specs".to_string(),
            }],
            Some(Suggestion {
                setting: "resources.requests".to_string(),
                current: "unset".to_string(),
                proposed: "requests matching observed steady-state usage".to_string(),
                apply: Some(
                    "kubectl set resources deployment/<name> --requests=cpu=100m,memory=256Mi"
                        .to_string(),
                ),
            }),
        ));
    }

    if !unlimited.is_empty() {
        out.push(finding(
            snapshot,
            "workload-no-memory-limit",
            Severity::Advisory,
            format!(
                "{} container{} without a memory limit",
                unlimited.len(),
                if unlimited.len() == 1 { "" } else { "s" }
            ),
            format!(
                "{} can grow until the node runs out, taking unrelated pods with them. \
                 A limit converts that into a single OOMKilled container that the \
                 controller restarts.",
                join_with_overflow(&unlimited, 6),
            ),
            vec![Evidence::Setting {
                key: "resources.limits.memory".to_string(),
                value: "unset".to_string(),
                source: "apps/v1 specs".to_string(),
            }],
            Some(Suggestion {
                setting: "resources.limits.memory".to_string(),
                current: "unset".to_string(),
                proposed: "a headroom multiple of observed peak".to_string(),
                apply: Some(
                    "kubectl set resources deployment/<name> --limits=memory=512Mi".to_string(),
                ),
            }),
        ));
    }

    if !single.is_empty() {
        let owned: Vec<String> = single.iter().map(|s| s.to_string()).collect();
        out.push(finding(
            snapshot,
            "workload-single-replica",
            Severity::Advisory,
            format!(
                "{} workload{} running a single replica",
                single.len(),
                if single.len() == 1 { "" } else { "s" }
            ),
            format!(
                "{} have one replica, so a node drain, an eviction or a rolling update \
                 is a full outage of that workload rather than a reduction in capacity. \
                 Fine for something genuinely singleton; worth knowing when it is not.",
                join_with_overflow(&owned, 6),
            ),
            vec![Evidence::Setting {
                key: "replicas".to_string(),
                value: "1".to_string(),
                source: "apps/v1 specs".to_string(),
            }],
            Some(Suggestion {
                setting: "replicas".to_string(),
                current: "1".to_string(),
                proposed: "2 or more, with a PodDisruptionBudget".to_string(),
                apply: Some("kubectl scale deployment/<name> --replicas=2".to_string()),
            }),
        ));
    }
}

fn join_with_overflow(items: &[String], take: usize) -> String {
    let shown = items
        .iter()
        .take(take)
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match items.len().saturating_sub(take) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configsource::ConfigDocument;
    use crate::model::{Health, Metric};
    use chrono::Utc;

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

    /// A rising series with a strong fit, spanning enough clock time that the
    /// forecaster will speak.
    fn rising(key: &str, from: f64, to: f64, warn_above: Option<f64>) -> MetricSeries {
        let count = 40;
        let start = Utc::now() - chrono::Duration::seconds(count * 30);
        let points: Vec<crate::history::Point> = (0..count)
            .map(|i| crate::history::Point {
                t: start + chrono::Duration::seconds(i * 30),
                v: from + (to - from) * (i as f64 / (count - 1) as f64),
            })
            .collect();
        MetricSeries {
            key: key.to_string(),
            label: key.to_string(),
            unit: MetricUnit::Bytes,
            warn_above,
            change_pct: None,
            min: from,
            max: to,
            latest: to,
            trend: crate::forecast::trend(&points, warn_above),
            points,
        }
    }

    // ------------------------------------------------------------- settings

    #[test]
    fn settings_read_the_same_key_out_of_either_format() {
        let json = ConfigDocument::live(
            "p1",
            "live",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": "2147483648" }),
        );
        let ini = ConfigDocument::live_text("p1", "file", "redis.conf", "maxmemory-policy noeviction\n");

        let settings = Settings::from_documents(&[json, ini]);
        assert_eq!(settings.get("maxmemory"), Some("2147483648"));
        assert_eq!(settings.get("maxmemory-policy"), Some("noeviction"));
        // The separator a caller happens to use must not matter.
        assert_eq!(settings.get("maxmemory_policy"), Some("noeviction"));
    }

    #[test]
    fn a_nested_live_value_is_reachable_by_its_leaf() {
        let doc = ConfigDocument::live(
            "p1",
            "live",
            "pg_settings",
            &serde_json::json!({ "max_connections": { "value": "100", "source": "configuration file" } }),
        );
        let settings = Settings::from_documents(&[doc]);
        assert_eq!(settings.get("max_connections.value"), Some("100"));
    }

    #[test]
    fn sizes_parse_the_way_config_writes_them() {
        assert_eq!(parse_size("2gb"), Some(2.0 * 1024.0 * 1024.0 * 1024.0));
        assert_eq!(parse_size("512mb"), Some(512.0 * 1024.0 * 1024.0));
        assert_eq!(parse_size("1073741824"), Some(1_073_741_824.0));
        assert_eq!(parse_size("0"), Some(0.0));
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("auto"), None);
    }

    // ---------------------------------------------------------------- drift

    /// The bug nothing else can see: the file is right, and nothing reloaded.
    #[test]
    fn a_file_that_disagrees_with_the_running_process_is_a_finding() {
        let snap = snapshot(ServiceKind::Redis, vec![]);
        let live = ConfigDocument::live(
            "p1",
            "Running configuration",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": "2147483648", "maxmemory-policy": "noeviction" }),
        );
        let file = ConfigDocument::live_text(
            "p1",
            "redis.conf",
            "/etc/redis/redis.conf",
            "maxmemory 4gb\nmaxmemory-policy allkeys-lru\n",
        );
        // live_text marks its origin Live; the rule needs a File origin.
        let file = as_file(file, "/etc/redis/redis.conf");

        let found = findings(&snap, &[live, file], &[]);
        let drift = found
            .iter()
            .find(|f| f.id.ends_with("config-drift"))
            .expect("drift should be reported");
        assert!(drift.title.contains("2 settings"), "{}", drift.title);
        // Both sides, so the reader can see which is which.
        assert_eq!(drift.evidence.len(), 4);
        // The file's own spelling, not the normalised lookup key — a finding
        // that said "maxmemorypolicy" would read as a typo.
        assert!(
            drift.detail.contains("maxmemory-policy"),
            "{}",
            drift.detail
        );
    }

    /// `2147483648` and `2gb` are the same ceiling. A rule that called this
    /// drift would fire on every Redis there is, and be switched off.
    #[test]
    fn the_same_size_written_two_ways_is_not_drift() {
        let snap = snapshot(ServiceKind::Redis, vec![]);
        let live = ConfigDocument::live(
            "p1",
            "Running configuration",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": "2147483648" }),
        );
        let file = as_file(
            ConfigDocument::live_text("p1", "redis.conf", "/etc/redis/redis.conf", "maxmemory 2gb\n"),
            "/etc/redis/redis.conf",
        );

        let found = findings(&snap, &[live, file], &[]);
        assert!(
            !found.iter().any(|f| f.id.ends_with("config-drift")),
            "{found:#?}"
        );
    }

    /// A redacted value equals every other redacted value and nothing else.
    /// Reporting one as drift reports the redaction, not the setting.
    #[test]
    fn a_redacted_value_is_never_reported_as_drift() {
        let snap = snapshot(ServiceKind::Redis, vec![]);
        let live = ConfigDocument::live(
            "p1",
            "Running configuration",
            "CONFIG GET *",
            &serde_json::json!({ "requirepass": "hunter2" }),
        );
        let file = as_file(
            ConfigDocument::live_text(
                "p1",
                "redis.conf",
                "/etc/redis/redis.conf",
                "requirepass somethingelse\n",
            ),
            "/etc/redis/redis.conf",
        );

        let found = findings(&snap, &[live, file], &[]);
        assert!(
            !found.iter().any(|f| f.id.ends_with("config-drift")),
            "{found:#?}"
        );
    }

    #[test]
    fn drift_needs_both_sides_to_compare() {
        let snap = snapshot(ServiceKind::Redis, vec![]);
        let file = as_file(
            ConfigDocument::live_text("p1", "redis.conf", "/etc/redis/redis.conf", "maxmemory 4gb\n"),
            "/etc/redis/redis.conf",
        );
        assert!(findings(&snap, &[file], &[]).is_empty());
    }

    fn as_file(mut doc: ConfigDocument, path: &str) -> ConfigDocument {
        doc.origin = crate::configsource::Origin::File {
            path: path.to_string(),
        };
        doc
    }

    // ---------------------------------------------------------------- redis

    /// The finding the module exists for: neither half says this alone.
    #[test]
    fn a_filling_redis_that_refuses_writes_is_an_outage_with_a_date() {
        let two_gb = 2.0 * 1024.0 * 1024.0 * 1024.0;
        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new(
                "memory",
                "Memory",
                two_gb * 0.9,
                MetricUnit::Bytes,
            )],
        );
        let config = ConfigDocument::live(
            "p1",
            "live",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": "2gb", "maxmemory-policy": "noeviction" }),
        );
        let series = vec![rising("memory", two_gb * 0.5, two_gb * 0.9, Some(two_gb))];

        let found = findings(&snap, &[config], &series);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].title.contains("Writes will start failing"), "{}", found[0].title);
        assert_eq!(found[0].severity, Severity::Now);

        let suggestion = found[0].suggestion.as_ref().expect("something to change");
        assert_eq!(suggestion.setting, "maxmemory-policy");
        assert!(suggestion.apply.as_ref().unwrap().contains("allkeys-lru"));

        // Every claim is checkable.
        assert!(found[0]
            .evidence
            .iter()
            .any(|e| matches!(e, Evidence::Projection { .. })));
        assert!(found[0]
            .evidence
            .iter()
            .any(|e| matches!(e, Evidence::Setting { key, .. } if key == "maxmemory-policy")));
    }

    /// The same fill with an eviction policy is a capacity note, not an
    /// outage. Reporting them identically would be the thing that teaches
    /// people to ignore both.
    #[test]
    fn the_same_fill_with_an_eviction_policy_is_not_an_outage() {
        let two_gb = 2.0 * 1024.0 * 1024.0 * 1024.0;
        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", two_gb * 0.9, MetricUnit::Bytes)],
        );
        let config = ConfigDocument::live(
            "p1",
            "live",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": "2gb", "maxmemory-policy": "allkeys-lru" }),
        );
        let series = vec![rising("memory", two_gb * 0.5, two_gb * 0.9, Some(two_gb))];

        let found = findings(&snap, &[config], &series);
        assert_eq!(found.len(), 1);
        assert!(found[0].title.contains("Eviction starts"), "{}", found[0].title);
        assert_ne!(found[0].severity, Severity::Now);
    }

    #[test]
    fn an_unbounded_redis_is_reported_whatever_the_metrics_say() {
        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 1000.0, MetricUnit::Bytes)],
        );
        let config = ConfigDocument::live(
            "p1",
            "live",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": "0", "maxmemory-policy": "noeviction" }),
        );

        let found = findings(&snap, &[config], &[]);
        assert_eq!(found.len(), 1);
        assert!(found[0].title.contains("No memory ceiling"));
    }

    /// Config with nothing moving behind it is not a finding. A rule that
    /// fires on config alone is a linter, and a linter that shouts at a
    /// healthy estate gets turned off.
    #[test]
    fn a_healthy_redis_well_inside_its_ceiling_produces_nothing() {
        let two_gb = 2.0 * 1024.0 * 1024.0 * 1024.0;
        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", two_gb * 0.2, MetricUnit::Bytes)],
        );
        let config = ConfigDocument::live(
            "p1",
            "live",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": "2gb", "maxmemory-policy": "noeviction" }),
        );
        // Flat: a fit exists, but it points nowhere.
        let series = vec![rising("memory", two_gb * 0.2, two_gb * 0.2, Some(two_gb))];

        assert!(findings(&snap, &[config], &series).is_empty());
    }

    #[test]
    fn no_config_means_no_findings_rather_than_guesses() {
        let snap = snapshot(
            ServiceKind::Redis,
            vec![Metric::new("memory", "Memory", 1e9, MetricUnit::Bytes)],
        );
        assert!(findings(&snap, &[], &[]).is_empty());
    }

    // ------------------------------------------------------------- database

    #[test]
    fn connection_slots_running_out_names_the_pooler_as_the_alternative() {
        let snap = snapshot(
            ServiceKind::Postgres,
            vec![Metric::new("connections", "Connections", 90.0, MetricUnit::Count)],
        );
        let config = ConfigDocument::live(
            "p1",
            "live",
            "pg_settings (non-default)",
            &serde_json::json!({ "max_connections": "100" }),
        );
        let mut series = rising("connections", 40.0, 90.0, Some(100.0));
        series.unit = MetricUnit::Count;

        let found = findings(&snap, &[config], &[series]);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].title.contains("Connection slots run out"));
        assert!(found[0].detail.contains("PgBouncer"));
        assert!(found[0].suggestion.as_ref().unwrap().apply.as_ref().unwrap().contains("ALTER SYSTEM"));
    }

    /// The same rule on MySQL must not recommend a Postgres pooler or
    /// Postgres syntax. One wrong suggestion costs the credibility of all of
    /// them.
    #[test]
    fn the_same_rule_on_mysql_names_mysql_tooling() {
        let snap = snapshot(
            ServiceKind::Mysql,
            vec![Metric::new("connections", "Connections", 90.0, MetricUnit::Count)],
        );
        let config = ConfigDocument::live(
            "p1",
            "live",
            "SHOW GLOBAL VARIABLES",
            &serde_json::json!({ "max_connections": "100" }),
        );
        let mut series = rising("connections", 40.0, 90.0, Some(100.0));
        series.unit = MetricUnit::Count;

        let found = findings(&snap, &[config], &[series]);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].detail.contains("ProxySQL"), "{}", found[0].detail);
        assert!(!found[0].detail.contains("PgBouncer"));
        let apply = found[0].suggestion.as_ref().unwrap().apply.as_ref().unwrap();
        assert!(apply.contains("SET GLOBAL"), "{apply}");
        assert!(!apply.contains("ALTER SYSTEM"), "{apply}");
    }

    // ------------------------------------------------------------ containers

    #[test]
    fn containers_with_no_memory_limit_are_a_host_wide_risk() {
        let snap = snapshot(ServiceKind::Docker, vec![]);
        let config = ConfigDocument::live(
            "p1",
            "live",
            "inspect (HostConfig)",
            &serde_json::json!({
                "api":    { "memoryLimitBytes": 0, "restartPolicy": "UnlessStopped" },
                "worker": { "memoryLimitBytes": 2147483648i64, "restartPolicy": "UnlessStopped" },
            }),
        );

        let found = findings(&snap, &[config], &[]);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].title.starts_with("1 container"));
        assert!(found[0].detail.contains("api"));
        assert!(!found[0].detail.contains("worker"), "the limited one is fine");
    }

    // ------------------------------------------------------------- workloads

    #[test]
    fn a_besteffort_workload_is_ranked_above_a_missing_limit() {
        let snap = snapshot(ServiceKind::Kubernetes, vec![]);
        let config = ConfigDocument::live(
            "p1",
            "live",
            "apps/v1 specs",
            &serde_json::json!({
                "Deployment/checkout": {
                    "replicas": 3,
                    "containers": {
                        "web": { "requests": { "cpu": null, "memory": null },
                                 "limits":   { "cpu": null, "memory": null } }
                    }
                }
            }),
        );

        let found = findings(&snap, &[config], &[]);
        let titles: Vec<&str> = found.iter().map(|f| f.title.as_str()).collect();
        assert!(titles.iter().any(|t| t.contains("BestEffort")), "{titles:?}");
        assert!(titles.iter().any(|t| t.contains("without a memory limit")), "{titles:?}");
        // Severity orders the list: the eviction risk outranks the note.
        assert_eq!(found[0].severity, Severity::Soon);
    }

    #[test]
    fn a_fully_specified_workload_produces_nothing() {
        let snap = snapshot(ServiceKind::Kubernetes, vec![]);
        let config = ConfigDocument::live(
            "p1",
            "live",
            "apps/v1 specs",
            &serde_json::json!({
                "Deployment/checkout": {
                    "replicas": 3,
                    "containers": {
                        "web": { "requests": { "cpu": "100m", "memory": "256Mi" },
                                 "limits":   { "cpu": "500m", "memory": "512Mi" } }
                    }
                }
            }),
        );
        assert!(findings(&snap, &[config], &[]).is_empty());
    }
}
