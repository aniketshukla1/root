//! What is about to break, read by the model across everything root holds.
//!
//! `advisor.rs` predicts with rules and says why it does not use the model:
//! *"It would, often. 'Often' is the problem."* That argument is right about
//! rules and wrong as a reason to stop here, because it assumes the only two
//! options are a rule you can check and a model you must believe.
//!
//! There is a third, and it is the one this module is built on. A rule only
//! finds what somebody thought to encode, and the failures that actually cause
//! downtime rarely announce themselves in one metric: a connection pool
//! creeping up *while* a new error template appears in the logs *while* a
//! deploy went out an hour ago *while* the service downstream has been slow
//! since. No rule spans that, and a model reads all four at once. What makes it
//! usable is not trusting it — it is that every prediction arrives with the
//! evidence it was made from, is scored beside the rules, and can be wrong
//! out loud.
//!
//! ## Three things keep it honest
//!
//! **It only sees what root already knows.** Evidence is assembled here, in
//! process, from the snapshots, the metric ring, the log templates, the config
//! findings, the declared topology and the recent changes. There are no tools
//! and no second round trip, so a prediction cannot wander off and go looking.
//!
//! **The grounds travel with the prediction.** Every [`Finding`] this produces
//! carries the lines the model was shown. A prediction nobody can check is a
//! rumour with a timestamp, and this is the same obligation `explain_component`
//! already carries for a tooltip.
//!
//! **It is scored apart.** Findings are stamped [`Provenance::Model`], so the
//! journal rates them separately from the rules and the scorecard can answer
//! the only question that matters — is it any good — rather than averaging the
//! two engines into a number describing neither. When a model prediction proves
//! right often enough to be worth relying on, it has earned a rule.
//!
//! ## What it refuses to do
//!
//! Invent a connection. A prediction naming something not in the estate is
//! dropped rather than shown, because the failure mode of a confident wrong
//! name is an operator checking the wrong system during an incident.
//!
//! Run without evidence. An estate with nothing moving produces no call at all;
//! asking a model what might break when nothing is happening is how you
//! manufacture a plausible answer.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::advisor::{Finding, Provenance, Severity};
use crate::error::{AppError, Result};
use crate::llm::{build_provider, ChatMessage, CompletionRequest};
use crate::model::{Alert, AlertSeverity, Health};
use crate::state::AppState;

/// Fingerprints of prediction-derived alerts start with this, so the snapshot
/// engine and this one never resolve each other's work. See
/// [`crate::state::AlertStore::reconcile`], which resolves anything it did not
/// just see for a connection.
pub const ALERT_PREFIX: &str = "predict:";

/// How far back the evidence looks.
const WINDOW_MINUTES: i64 = 180;
/// Capacity risks named per estate. Sorted worst first before the cut.
const MAX_RISKS: usize = 12;
/// Log templates named. Enough to show a shift, short of pasting the buffer.
const MAX_PATTERNS: usize = 12;
/// Rule findings shown to the model, so it can build on them or disagree.
const MAX_FINDINGS: usize = 12;
/// Predictions accepted from one call.
const MAX_PREDICTIONS: usize = 8;
/// Evidence lines. A hard ceiling on the prompt, whatever the estate's size.
const MAX_EVIDENCE_LINES: usize = 220;

const SYSTEM: &str = "\
You are reading one operator's infrastructure and saying what is likely to fail next.

You are given only facts root already collected: health, metric trends against declared \
limits, log templates with per-bucket counts across the window, configuration findings \
its rules already produced, declared dependencies, recent deploys and active alerts.

Answer with a JSON array and nothing else. Each element:
  connection  string  exact `name` from the CONNECTIONS list, never invented
  severity    string  one of: now, soon, advisory
  title       string  under 80 chars, names the failure, not the symptom
  detail      string  2-3 sentences: what fails, roughly when, and why these \
facts say so. Name the numbers you used.
  because     array of strings, each quoting one evidence line you relied on

Rules you must follow:
- severity `now` means already failing or inside hours; `soon` means inside a week; \
`advisory` means real but no deadline.
- Predict only what the evidence supports. Do not restate a finding that is already \
listed under FINDINGS unless you are adding a consequence it does not state.
- A trend with a poor fit is noise, not a projection. Say nothing about it.
- If nothing here suggests a coming failure, answer with an empty array. An empty \
array is a good answer and is expected often.
- Never name a connection that is not in the list.";

/// One prediction as the model returns it, before it is checked.
#[derive(Debug, Deserialize)]
struct Raw {
    connection: String,
    severity: String,
    title: String,
    detail: String,
    #[serde(default)]
    because: Vec<String>,
}

/// What the model was shown and what it said.
#[derive(Debug, Clone)]
pub struct Prediction {
    pub finding: Finding,
    /// The connection's real kind, carried so the alert renders with the right
    /// badge and colour. `Finding` has no kind — it never needed one, because
    /// findings render inside a connection that already declares it, and an
    /// alert does not.
    pub kind: crate::model::ServiceKind,
    /// The evidence lines the model said it relied on, in its own words.
    ///
    /// Separate from `finding.evidence`, which is the rules' structured
    /// vocabulary: a sentence is not a setting, a metric or a projection, and
    /// dressing it as one would hide which engine made the claim.
    pub because: Vec<String>,
    /// Every evidence line handed to the model for this run, so a reader can
    /// check the reasoning rather than take it.
    pub grounds: Vec<String>,
    pub model: String,
    pub at: DateTime<Utc>,
}

/// Assemble what root knows into lines a model can read.
///
/// Returns an empty vector when there is nothing worth asking about — an
/// estate at rest produces no call.
pub async fn gather_evidence(state: &Arc<AppState>) -> Vec<String> {
    let now = Utc::now();
    let since = now - Duration::minutes(WINDOW_MINUTES);
    let mut lines = Vec::new();

    let profiles: Vec<(String, String, crate::model::ServiceKind)> = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .filter(|p| p.enabled)
            .map(|p| (p.id.clone(), p.name.clone(), p.kind))
            .collect()
    };
    if profiles.is_empty() {
        return lines;
    }

    // ------------------------------------------------------------ inventory
    lines.push("CONNECTIONS".to_string());
    {
        let snapshots = state.snapshots.read().await;
        for (id, name, kind) in &profiles {
            let (health, summary) = snapshots
                .get(id)
                .map(|s| (s.health, s.summary.clone()))
                .unwrap_or((Health::Unknown, "not yet polled".to_string()));
            lines.push(format!(
                "- {name} ({}) health={health:?} — {summary}",
                kind.label()
            ));
        }
    }

    // What depends on what, so a prediction can reason about blast radius.
    // Declared, never inferred — the same standing `topology.rs` gives it.
    {
        let config = state.config.read().await;
        let edges: Vec<String> = config
            .profiles
            .iter()
            .filter(|p| !p.depends_on.is_empty())
            .map(|p| format!("- {} depends on {}", p.name, p.depends_on.join(", ")))
            .collect();
        if !edges.is_empty() {
            lines.push(String::new());
            lines.push("DECLARED DEPENDENCIES".to_string());
            lines.extend(edges);
        }
    }

    // ------------------------------------------------------------ capacity
    let risks = state.history.read().await.capacity_risks(MAX_RISKS);
    if !risks.is_empty() {
        lines.push(String::new());
        lines.push("MEASURES APPROACHING A LIMIT".to_string());
        for risk in &risks {
            let name = profiles
                .iter()
                .find(|(id, _, _)| *id == risk.profile_id)
                .map(|(_, n, _)| n.as_str())
                .unwrap_or("unknown");
            // The fit travels with the slope on purpose: a projection from a
            // poor fit is the thing `forecast.rs` refuses to make, and the
            // model is told the number so it can refuse too.
            let trend = match &risk.trend {
                Some(t) => match t.seconds_to_limit {
                    Some(secs) => format!(
                        " trend: reaches limit in {:.1}h (fit {:.2}, {} samples)",
                        secs / 3600.0,
                        t.fit,
                        t.samples
                    ),
                    None => format!(" trend: not heading at the limit (fit {:.2})", t.fit),
                },
                None => String::new(),
            };
            lines.push(format!(
                "- {name}: {} at {:.0}% of limit ({:.2} of {:.2}){trend}",
                risk.label,
                risk.used * 100.0,
                risk.value,
                risk.limit
            ));
        }
    }

    // -------------------------------------------------------- log templates
    let patterns = log_templates(state, since, now).await;
    if !patterns.is_empty() {
        lines.push(String::new());
        lines.push(format!("LOG TEMPLATES, LAST {WINDOW_MINUTES} MINUTES").to_string());
        lines.push(
            "(buckets run oldest to newest across the window; a rising tail is a rate \
             that is climbing)"
                .to_string(),
        );
        lines.extend(patterns);
    }

    // ------------------------------------------------------- rule findings
    let findings = rule_findings(state).await;
    if !findings.is_empty() {
        lines.push(String::new());
        lines.push("FINDINGS ROOT'S RULES ALREADY MADE".to_string());
        for finding in findings.iter().take(MAX_FINDINGS) {
            lines.push(format!(
                "- [{:?}] {}: {} — {}",
                finding.severity, finding.profile_name, finding.title, finding.detail
            ));
        }
    }

    // -------------------------------------------------------------- alerts
    {
        let alerts = state.alerts.read().await;
        // `all()` is real alerts only now, so this can no longer feed the
        // model its own past predictions as though they were observations —
        // which was the point of the filter that used to sit here.
        let active: Vec<String> = alerts
            .all()
            .into_iter()
            .filter(|a| a.active)
            .map(|a| format!("- [{:?}] {}: {}", a.severity, a.profile_name, a.title))
            .collect();
        if !active.is_empty() {
            lines.push(String::new());
            lines.push("ACTIVE ALERTS".to_string());
            lines.extend(active);
        }
    }

    // ------------------------------------------------------------- changes
    {
        let changes = state.changes.read().await;
        let recent: Vec<String> = changes
            .query(None, Some(since), 20)
            .into_iter()
            .map(|c| {
                format!(
                    "- {} {} {} at {}",
                    c.profile_name,
                    c.component,
                    c.summary,
                    c.at.format("%H:%M")
                )
            })
            .collect();
        if !recent.is_empty() {
            lines.push(String::new());
            lines.push("DEPLOYS AND CHANGES IN THE WINDOW".to_string());
            lines.extend(recent);
        }
    }

    // Nothing but the inventory means nothing is happening. Asking a model
    // what might break in a quiet estate is how a plausible answer gets
    // manufactured, so there is no call.
    let substantive = lines.iter().any(|l| {
        l.starts_with("MEASURES")
            || l.starts_with("LOG TEMPLATES")
            || l.starts_with("FINDINGS")
            || l.starts_with("ACTIVE ALERTS")
            || l.starts_with("DEPLOYS")
    });
    if !substantive {
        return Vec::new();
    }

    lines.truncate(MAX_EVIDENCE_LINES);
    lines
}

/// What root's own rules already concluded, so the model can build on them
/// rather than rediscover them.
///
/// Shown deliberately: a prediction that restates a rule is worth less than one
/// that names a consequence the rule did not, and the system prompt says so.
/// Giving the model the rules' output is what lets it tell the difference.
async fn rule_findings(state: &Arc<AppState>) -> Vec<Finding> {
    let profiles: Vec<crate::config::ConnectionProfile> = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .filter(|p| p.enabled)
            .cloned()
            .collect()
    };

    let mut out = Vec::new();
    for profile in &profiles {
        let Some(snapshot) = state.snapshots.read().await.get(&profile.id).cloned() else {
            continue;
        };

        let mut documents = Vec::new();
        for source in &profile.config_sources {
            documents.extend(crate::configsource::read_source(&profile.id, source).await);
        }
        if let Ok(collector) = state.collector(&profile.id).await {
            if let Ok(live) = collector.config().await {
                documents.extend(live);
            }
        }
        if documents.is_empty() {
            continue;
        }

        let series = state.history.read().await.series(&profile.id, None);
        out.extend(crate::advisor::findings(&snapshot, &documents, &series));
    }
    out
}

/// Log templates across the estate, with the shape of their rate.
async fn log_templates(
    state: &Arc<AppState>,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Vec<String> {
    let events = state.logs.read().await.query(&crate::state::LogFilter {
        since: Some(since),
        limit: Some(5000),
        ..Default::default()
    });
    if events.is_empty() {
        return Vec::new();
    }

    let mut drain = crate::patterns::Drain::new(crate::patterns::DrainConfig::default());
    let mut members: std::collections::HashMap<usize, Vec<&crate::model::LogEvent>> =
        std::collections::HashMap::new();
    for event in &events {
        if let Some(id) = drain.add(&event.message) {
            members.entry(id).or_default().push(event);
        }
    }

    const BUCKETS: usize = 8;
    let span_ms = (until - since).num_milliseconds().max(1) as f64;
    let mut rows: Vec<(usize, String)> = Vec::new();

    for (id, cluster) in drain.clusters().iter().enumerate() {
        let Some(group) = members.get(&id) else { continue };
        if group.is_empty() {
            continue;
        }
        let mut buckets = vec![0usize; BUCKETS];
        for event in group {
            let offset = (event.timestamp - since).num_milliseconds().max(0) as f64;
            let slot = ((offset / span_ms) * BUCKETS as f64) as usize;
            buckets[slot.min(BUCKETS - 1)] += 1;
        }
        let errors = group
            .iter()
            .filter(|e| e.level >= crate::model::LogLevel::Error)
            .count();
        let worst = group
            .iter()
            .map(|e| e.level)
            .max()
            .unwrap_or(crate::model::LogLevel::Info);
        let mut who: Vec<&str> = group.iter().map(|e| e.profile_name.as_str()).collect();
        who.sort_unstable();
        who.dedup();
        who.truncate(3);

        rows.push((
            group.len(),
            format!(
                "- [{worst:?}] x{} ({} error) on {} | buckets {:?} | {}",
                group.len(),
                errors,
                who.join(", "),
                buckets,
                cluster.template_string()
            ),
        ));
    }

    // Busiest first, then cut: a template nobody saw twice is noise.
    rows.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
    rows.into_iter()
        .take(MAX_PATTERNS)
        .map(|(_, line)| line)
        .collect()
}

/// Ask the model what is about to fail, and keep only what checks out.
///
/// `Ok(vec![])` covers both "nothing worth asking about" and "the model saw
/// nothing coming" — neither is an error, and both are common.
pub async fn predict(state: &Arc<AppState>) -> Result<Vec<Prediction>> {
    let grounds = gather_evidence(state).await;
    if grounds.is_empty() {
        return Ok(Vec::new());
    }

    let settings = state.config.read().await.llm.clone();
    let api_key = crate::secrets::get_secret(settings.provider.secret_key()).await?;
    let provider = build_provider(&settings, api_key)?;

    let response = provider
        .complete(CompletionRequest {
            model: settings.model.clone(),
            system: SYSTEM.to_string(),
            messages: vec![ChatMessage::user(grounds.join("\n"))],
            tools: Vec::new(),
            // One call, no tools, and a budget that fits a handful of
            // predictions. This runs on a timer, so it has to stay cheap.
            max_tokens: 1200,
            temperature: 0.0,
        })
        .await?;

    let raw = parse(&response.text)?;
    Ok(validate(state, raw, grounds, &settings.model).await)
}

/// Pull the JSON array out of the answer.
///
/// Models wrap JSON in prose and in fences often enough that refusing on the
/// first stray character would make this fail for a reason nobody cares about.
fn parse(text: &str) -> Result<Vec<Raw>> {
    let trimmed = text.trim();
    let body = match (trimmed.find('['), trimmed.rfind(']')) {
        (Some(start), Some(end)) if end > start => &trimmed[start..=end],
        _ => {
            // No array at all. An empty answer is legitimate; anything else is
            // a model that did not follow the format.
            return if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                Ok(Vec::new())
            } else {
                Err(AppError::Llm(format!(
                    "the model did not answer with a JSON array: {}",
                    trimmed.chars().take(200).collect::<String>()
                )))
            };
        }
    };

    serde_json::from_str::<Vec<Raw>>(body)
        .map_err(|e| AppError::Llm(format!("could not read the model's predictions: {e}")))
}

/// Drop anything that does not name a real connection, and turn the rest into
/// findings stamped with the model that made them.
async fn validate(
    state: &Arc<AppState>,
    raw: Vec<Raw>,
    grounds: Vec<String>,
    model: &str,
) -> Vec<Prediction> {
    let profiles: Vec<(String, String, crate::model::ServiceKind)> = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .map(|p| (p.id.clone(), p.name.clone(), p.kind))
            .collect()
    };

    let at = Utc::now();
    let mut out = Vec::new();

    for item in raw.into_iter().take(MAX_PREDICTIONS) {
        let wanted = item.connection.trim().to_lowercase();
        // A prediction about a system that does not exist is worse than none:
        // it sends somebody to check the wrong thing during an incident.
        let Some((profile_id, profile_name, kind)) = profiles
            .iter()
            .find(|(id, name, _)| name.to_lowercase() == wanted || id.to_lowercase() == wanted)
            .cloned()
        else {
            tracing::debug!(connection = %item.connection, "prediction named no known connection");
            continue;
        };

        let title = item.title.trim();
        if title.is_empty() {
            continue;
        }

        let severity = match item.severity.trim().to_lowercase().as_str() {
            "now" => Severity::Now,
            "soon" => Severity::Soon,
            _ => Severity::Advisory,
        };

        let finding = Finding {
            // Stable on the connection and the claim, so the same prediction
            // across two runs is one row to rate rather than a new one each
            // time the timer fires.
            id: format!("predict:{profile_id}:{}", stable_slug(title)),
            provenance: Provenance::Model {
                model: model.to_string(),
            },
            rule: "prediction".to_string(),
            profile_id,
            profile_name,
            severity,
            title: title.to_string(),
            detail: item.detail.trim().to_string(),
            // `Evidence` is the rules' structured vocabulary — a setting, a
            // metric, a projection — and a sentence the model wrote is none of
            // those. Forging one would make a model's reason indistinguishable
            // from a rule's citation, which is exactly the confusion
            // `Provenance` exists to prevent. The reasons ride on the
            // prediction instead.
            evidence: Vec::new(),
            suggestion: None,
        };

        out.push(Prediction {
            finding,
            kind,
            because: item
                .because
                .into_iter()
                .filter(|line| !line.trim().is_empty())
                .map(|line| line.trim().to_string())
                .take(6)
                .collect(),
            grounds: grounds.clone(),
            model: model.to_string(),
            at,
        });
    }

    out
}

/// A short stable key from a title, so the same claim keeps one id.
fn stable_slug(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-")
}

/// Turn predictions into alerts, so they arrive rather than wait to be found.
///
/// This is the whole point of the feature. Root could already project a disk
/// filling and read a config that will refuse writes this afternoon, and both
/// sat on a screen until somebody opened it. A prediction that does not push is
/// a prediction nobody reads in time.
/// Whether an alert came from the prediction engine rather than from a
/// collector's own reading.
///
/// The single discriminator. A prediction is stored beside real alerts because
/// the lifecycle is genuinely shared — one that stops being true has to resolve
/// exactly as an alert does — but it is never *shown* beside one, and every
/// boundary that hands alerts outward splits on this and nothing else.
pub fn is_prediction(fingerprint: &str) -> bool {
    fingerprint.starts_with(ALERT_PREFIX)
}

pub fn alerts_from(predictions: &[Prediction]) -> Vec<Alert> {
    predictions
        .iter()
        .map(|prediction| {
            let finding = &prediction.finding;
            Alert {
                id: uuid::Uuid::new_v4().to_string(),
                // Prefixed so the snapshot engine never resolves these and
                // this never resolves the snapshot engine's.
                fingerprint: format!("{ALERT_PREFIX}{}", finding.id),
                profile_id: finding.profile_id.clone(),
                profile_name: finding.profile_name.clone(),
                kind: prediction.kind,
                severity: match finding.severity {
                    Severity::Now => AlertSeverity::Critical,
                    Severity::Soon => AlertSeverity::Warning,
                    Severity::Advisory => AlertSeverity::Info,
                },
                // The title is the failure, unqualified. It used to carry a
                // "Predicted: " prefix because this rode in the same list as
                // real alerts and an operator had to tell them apart by
                // reading. Nothing mixes them now — predictions leave the
                // backend on their own field and land in their own panel — so
                // the prefix would only repeat the heading above it.
                title: finding.title.clone(),
                detail: finding.detail.clone(),
                component: None,
                first_seen: prediction.at,
                last_seen: prediction.at,
                active: true,
                acknowledged: false,
            }
        })
        .collect()
}

/// Run one sweep and raise what it found.
///
/// Returns the predictions so a caller that asked for this directly can show
/// them; the alerts go out regardless, because arriving is the point.
///
/// Errors are the caller's to report. A sweep on a timer logs and moves on: a
/// provider that is briefly unreachable must not take the app down with it.
pub async fn sweep(state: &Arc<AppState>) -> Result<Vec<Prediction>> {
    let predictions = predict(state).await?;

    // Grouped by connection because that is how alerts resolve: a prediction
    // that stops being made should stop alerting, and only for the connection
    // it was about.
    let mut by_profile: std::collections::HashMap<String, Vec<Prediction>> =
        std::collections::HashMap::new();
    for prediction in &predictions {
        by_profile
            .entry(prediction.finding.profile_id.clone())
            .or_default()
            .push(prediction.clone());
    }

    // Every enabled connection is reconciled, not only the ones predicted
    // about. A connection that was predicted about last sweep and is fine now
    // needs its alert resolved, and that only happens if it is passed an empty
    // list rather than skipped.
    let profile_ids: Vec<String> = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .filter(|p| p.enabled)
            .map(|p| p.id.clone())
            .collect()
    };

    let mut alerts_store = state.alerts.write().await;
    for profile_id in profile_ids {
        let alerts = by_profile
            .get(&profile_id)
            .map(|group| alerts_from(group))
            .unwrap_or_default();
        alerts_store.reconcile_predictions(&profile_id, alerts);
    }
    // Persisted with `predicted = true`, so the split holds on disk as well
    // as in memory and no query over the alert history can count a forecast
    // as something that happened.
    let predictions_now = alerts_store.predictions();
    drop(alerts_store);

    if let Some(history) = state.store.read().await.as_ref() {
        for alert in &predictions_now {
            if let Err(e) = history.write_alert(alert, true).await {
                tracing::warn!("prediction write failed: {e}");
            }
        }
    }

    // The model's claims land where the rules' findings already are, so the
    // same screen rates both and a prediction can earn its way into being a
    // rule. `claims.rs` explains why that loop matters.
    let mut claims = state.claims.write().await;
    claims.sweep();
    for prediction in &predictions {
        claims.record(
            &prediction.finding.profile_id,
            crate::claims::Claim {
                finding: prediction.finding.clone(),
                at: prediction.at,
                conversation_id: None,
            },
        );
    }

    Ok(predictions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fenced_array_is_read() {
        let raw = parse(
            "Here is what I found:\n```json\n[{\"connection\":\"db\",\"severity\":\"now\",\
             \"title\":\"t\",\"detail\":\"d\",\"because\":[\"x\"]}]\n```",
        )
        .expect("a fenced array should parse");
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].connection, "db");
    }

    /// The expected answer most of the time, and it must not read as a fault.
    #[test]
    fn an_empty_array_is_a_valid_answer() {
        assert!(parse("[]").expect("empty is fine").is_empty());
        assert!(parse("").expect("nothing is fine").is_empty());
        assert!(parse("none").expect("none is fine").is_empty());
    }

    #[test]
    fn prose_with_no_array_is_an_error() {
        assert!(parse("I think the database might be unhappy.").is_err());
    }

    #[test]
    fn a_slug_is_stable_across_punctuation_and_case() {
        assert_eq!(
            stable_slug("Redis will refuse writes!"),
            stable_slug("redis WILL refuse  writes")
        );
    }

    #[test]
    fn a_slug_is_bounded() {
        let slug = stable_slug("one two three four five six seven eight nine ten");
        assert_eq!(slug.split('-').count(), 6);
    }

    /// Severity is the horizon, and it decides how loudly the alert lands.
    #[test]
    fn severity_maps_to_alert_severity() {
        let make = |severity| Prediction {
            kind: crate::model::ServiceKind::Postgres,
            finding: Finding {
                id: "predict:p:x".into(),
                provenance: Provenance::Model {
                    model: "m".into(),
                },
                rule: "prediction".into(),
                profile_id: "p".into(),
                profile_name: "p".into(),
                severity,
                title: "t".into(),
                detail: "d".into(),
                evidence: Vec::new(),
                suggestion: None,
            },
            because: Vec::new(),
            grounds: Vec::new(),
            model: "m".into(),
            at: Utc::now(),
        };

        assert_eq!(
            alerts_from(&[make(Severity::Now)])[0].severity,
            AlertSeverity::Critical
        );
        assert_eq!(
            alerts_from(&[make(Severity::Soon)])[0].severity,
            AlertSeverity::Warning
        );
        assert_eq!(
            alerts_from(&[make(Severity::Advisory)])[0].severity,
            AlertSeverity::Info
        );
    }

    /// The prefix is what stops the two alert engines resolving each other.
    #[test]
    fn prediction_alerts_carry_the_prefix() {
        let prediction = Prediction {
            kind: crate::model::ServiceKind::Postgres,
            finding: Finding {
                id: "predict:p:disk".into(),
                provenance: Provenance::Model { model: "m".into() },
                rule: "prediction".into(),
                profile_id: "p".into(),
                profile_name: "web".into(),
                severity: Severity::Soon,
                title: "Disk fills".into(),
                detail: "d".into(),
                evidence: Vec::new(),
                suggestion: None,
            },
            because: Vec::new(),
            grounds: Vec::new(),
            model: "m".into(),
            at: Utc::now(),
        };
        let alerts = alerts_from(&[prediction]);
        assert!(alerts[0].fingerprint.starts_with(ALERT_PREFIX));
        assert!(
            !alerts[0].title.starts_with("Predicted: "),
            "the panel says it is a forecast; the title should not say it twice"
        );
        assert!(is_prediction(&alerts[0].fingerprint));
    }
}
