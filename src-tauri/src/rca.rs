//! Root cause analysis for one incident, by the model, on root's evidence.
//!
//! `incidents.rs` already answers part of this without a model: it groups
//! alerts into an incident, names which members nothing else explains, and
//! says with what confidence and on what grounds. That answer is deterministic
//! and it is the one root trusts. What it cannot do is say *why* — it knows
//! that the database failed before the API did, not that a deploy changed a
//! pool size forty seconds earlier.
//!
//! This asks the model that question, and only that question.
//!
//! ## Root's own answer goes in the prompt
//!
//! The model is shown what `incidents.rs` concluded and asked to confirm or
//! overturn it, and to say which. A model handed the same alerts with no prior
//! will reliably produce a fluent story; a model asked to disagree with a
//! stated position has to find a reason. The disagreements are the output
//! worth reading — they are either the model seeing something the rules cannot
//! encode, or the model being wrong in a way somebody can check, and both are
//! more useful than agreement.
//!
//! ## Ordering, not correlation
//!
//! Every alert is timestamped as an offset from the incident's own onset
//! rather than as a wall clock. "+0s, +14s, +2m" is a causal sequence;
//! three RFC3339 strings are three facts a reader has to subtract. The same
//! goes for deploys, which are given relative to onset and *before* the alerts,
//! because a change that landed ninety seconds earlier is the most common
//! answer in this file and the one most often missed.
//!
//! ## An explanation that cannot be wrong is not one
//!
//! The model must name what would disprove its account. This is the field that
//! makes the rest of it checkable: "a deploy caused it" with nothing that could
//! falsify it is a sentence, not a finding. A refusal — low confidence and a
//! list of what to go and look at — is a good answer and expected often.
//!
//! ## It is a claim, and it is scored
//!
//! The result is recorded in `claims.rs` like any other model output, so it
//! reaches the Scorecard and gets rated beside the rules. An RCA that reads
//! well and is wrong should cost the model its precision, exactly as a bad
//! prediction does.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::incidents::{Confidence, Incident};
use crate::llm::{build_provider, ChatMessage, CompletionRequest};
use crate::state::AppState;

/// How far before onset a change is still worth showing.
///
/// Thirty minutes because a deploy that broke something usually breaks it
/// within one poll cycle or one request cycle, and a window wide enough to
/// catch yesterday's release is wide enough to offer a coincidence for every
/// incident.
const CHANGE_LOOKBACK_MINUTES: i64 = 30;

/// How far back log lines are read from, relative to onset.
const LOG_LOOKBACK_MINUTES: i64 = 15;

/// Log templates shown. Past this the tail is single occurrences, which is
/// noise at the resolution this question is asked at.
const MAX_TEMPLATES: usize = 12;

/// Alerts listed before the sequence is truncated. An incident with more
/// members than this is a storm, and the first twenty are the ones with
/// ordering information in them.
const MAX_ALERTS: usize = 20;

/// Budget for the answer. Larger than a component reading and smaller than a
/// conversation: this is one structured object, not a discussion.
const MAX_TOKENS: u32 = 1400;

/// What the model concluded about one incident.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootCause {
    pub incident_id: String,
    /// One sentence naming the cause, not the symptom.
    pub summary: String,
    /// The model's own confidence, which is not root's.
    pub confidence: Confidence,
    /// The causal sequence, earliest first. Each step is one clause.
    pub chain: Vec<String>,
    /// Evidence lines the model said it relied on, quoted from what it was given.
    pub because: Vec<String>,
    /// What to check to confirm this, in the order worth checking it.
    pub confirm: Vec<String>,
    /// What would show this account to be wrong.
    pub disconfirm: String,
    /// Whether it agrees with root's own grouping about which member is the root.
    pub agrees: bool,
    /// Why not, when it does not. The most interesting field in the struct.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dissent: Option<String>,
    pub model: String,
    /// Every fact the model was given, in the order it was given them.
    pub grounds: Vec<String>,
    pub at: DateTime<Utc>,
}

/// The answer as the model returns it, before it is checked.
#[derive(Debug, Deserialize)]
struct Raw {
    summary: String,
    #[serde(default)]
    confidence: String,
    #[serde(default)]
    chain: Vec<String>,
    #[serde(default)]
    because: Vec<String>,
    #[serde(default)]
    confirm: Vec<String>,
    #[serde(default)]
    disconfirm: String,
    #[serde(default = "default_true")]
    agrees: bool,
    #[serde(default)]
    dissent: String,
}

fn default_true() -> bool {
    true
}

const SYSTEM: &str = "\
You are finding the cause of ONE incident in an operator's infrastructure, from \
evidence that tool already collected. You are not guessing; every fact you may use \
is below.

The tool has already grouped these alerts and stated which member it believes is the \
root, with what confidence and why. Your job is to explain WHY it happened, and to \
say whether you agree with that grouping.

Answer with one JSON object and nothing else:
  summary     string  one sentence naming the cause, not the symptom. Under 140 chars.
  confidence  string  one of: high, medium, low
  chain       array of strings, earliest first, each one step of the causal sequence
  because     array of strings, each quoting one evidence line you relied on
  confirm     array of strings, what to check to confirm this, most decisive first
  disconfirm  string  what would show this account is wrong
  agrees      boolean whether you agree with the root the tool named
  dissent     string  when you do not agree, which member you think is the root and why

Rules you must follow:
- Order is your strongest evidence. A thing that started after another thing cannot \
have caused it.
- A deploy shortly before onset is the most common cause and the most commonly missed. \
If one is listed, say explicitly whether it explains this or not.
- Quote only evidence lines that appear below. Never invent a number, a name, a file \
or a log line.
- If the evidence does not support a single cause, say so: confidence `low`, a summary \
that names the ambiguity, and a `confirm` list of what would settle it. That is a good \
answer and is expected often.
- `disconfirm` may never be empty. An account nothing could disprove is not an account.
- Do not restate the alerts. The operator can already see them.";

/// Build the evidence for one incident, in the order the model reads it.
pub async fn grounds_for(state: &Arc<AppState>, incident: &Incident) -> Vec<String> {
    let mut lines = Vec::new();
    let onset = incident.started;

    // --- What root already concluded, so the model has something to disagree with.
    lines.push("WHAT THIS TOOL ALREADY CONCLUDED".to_string());
    lines.push(format!("- incident: {}", incident.title));
    lines.push(format!(
        "- severity: {:?}, confidence: {:?}",
        incident.severity, incident.confidence
    ));
    for reason in &incident.grouping {
        lines.push(format!("- grouped because: {}", describe(reason)));
    }
    let names = names_of(state).await;
    let lead = names
        .get(&incident.lead_profile_id)
        .cloned()
        .unwrap_or_else(|| incident.lead_profile_id.clone());
    lines.push(format!("- the root it named: {lead}"));
    if !incident.roots.is_empty() {
        let roots: Vec<String> = incident
            .roots
            .iter()
            .map(|id| names.get(id).cloned().unwrap_or_else(|| id.clone()))
            .collect();
        lines.push(format!(
            "- members nothing else explains: {}",
            roots.join(", ")
        ));
    }

    // --- Deploys first, and before the alerts, because that is the answer
    //     often enough that burying it under the symptoms is a mistake.
    let changes = {
        let store = state.changes.read().await;
        let since = onset - Duration::minutes(CHANGE_LOOKBACK_MINUTES);
        let mut found: Vec<crate::model::ChangeEvent> = incident
            .profile_ids
            .iter()
            .flat_map(|id| store.query(Some(id), Some(since), 20))
            .filter(|c| c.at <= incident.last_seen)
            .collect();
        found.sort_by_key(|c| c.at);
        found
    };
    if !changes.is_empty() {
        lines.push(String::new());
        lines.push("CHANGES, RELATIVE TO ONSET".to_string());
        for change in &changes {
            lines.push(format!(
                "- {} {}: {}",
                offset(change.at, onset),
                change.profile_name,
                change.summary
            ));
        }
    }

    // --- The sequence. This is the part with causality in it.
    lines.push(String::new());
    lines.push("WHAT FAILED, IN ORDER, RELATIVE TO ONSET".to_string());
    {
        let store = state.alerts.read().await;
        let mut members: Vec<crate::model::Alert> = store
            .all()
            .into_iter()
            .filter(|a| incident.alert_ids.contains(&a.id))
            .collect();
        members.sort_by_key(|a| a.first_seen);
        for alert in members.iter().take(MAX_ALERTS) {
            lines.push(format!(
                "- {} [{:?}] {}{}: {} — {}",
                offset(alert.first_seen, onset),
                alert.severity,
                alert.profile_name,
                alert
                    .component
                    .as_deref()
                    .map(|c| format!(" · {c}"))
                    .unwrap_or_default(),
                alert.title,
                alert.detail
            ));
        }
        if members.len() > MAX_ALERTS {
            lines.push(format!("- … and {} more", members.len() - MAX_ALERTS));
        }
    }

    // --- What each involved connection says about itself right now.
    {
        let snapshots = state.snapshots.read().await;
        let mut said = Vec::new();
        for id in &incident.profile_ids {
            if let Some(snapshot) = snapshots.get(id) {
                let metrics: Vec<String> = snapshot
                    .metrics
                    .iter()
                    .filter(|m| m.is_breaching() || m.warn_above.is_some())
                    .take(6)
                    .map(|m| {
                        format!(
                            "{}={:.2}{}",
                            m.key,
                            m.value,
                            m.warn_above
                                .map(|t| format!(" of {t:.2}"))
                                .unwrap_or_default()
                        )
                    })
                    .collect();
                said.push(format!(
                    "- {} [{}]: {}{}",
                    snapshot.profile_name,
                    snapshot.health.label(),
                    snapshot.summary,
                    if metrics.is_empty() {
                        String::new()
                    } else {
                        format!(" | {}", metrics.join(", "))
                    }
                ));
            }
        }
        if !said.is_empty() {
            lines.push(String::new());
            lines.push("WHAT EACH ONE REPORTS NOW".to_string());
            lines.extend(said);
        }
    }

    // --- Declared dependencies, so "A needs B" is available as a reason
    //     rather than being inferred from names.
    {
        let profiles = state.config.read().await.profiles.clone();
        let mut edges = Vec::new();
        for profile in &profiles {
            if !incident.profile_ids.contains(&profile.id) {
                continue;
            }
            for needed in &profile.depends_on {
                if let Some(name) = names.get(needed) {
                    edges.push(format!("- {} needs {}", profile.name, name));
                }
            }
        }
        if !edges.is_empty() {
            lines.push(String::new());
            lines.push("DECLARED DEPENDENCIES".to_string());
            lines.extend(edges);
        }
    }

    // --- Log shape across the window, clustered so a thousand lines of one
    //     thing arrive as one line with counts rather than as a thousand.
    let templates = log_templates(state, incident, onset).await;
    if !templates.is_empty() {
        lines.push(String::new());
        lines.push("LOG TEMPLATES ACROSS THE WINDOW (8 buckets, onset to now)".to_string());
        lines.extend(templates);
    }

    // --- Findings the rules already made about these connections, so the
    //     model adds to them rather than rediscovering them.
    {
        let claims = state.claims.read().await;
        let mut found = Vec::new();
        for id in &incident.profile_ids {
            for finding in claims.for_profile(id).iter().take(4) {
                found.push(format!(
                    "- {}: {}",
                    names.get(id).cloned().unwrap_or_else(|| id.clone()),
                    finding.title
                ));
            }
        }
        if !found.is_empty() {
            lines.push(String::new());
            lines.push("WHAT WAS ALREADY KNOWN ABOUT THESE CONNECTIONS".to_string());
            lines.extend(found);
        }
    }

    lines
}

/// Ask the model, check the answer, and record it as a claim.
pub async fn analyse(state: &Arc<AppState>, incident: &Incident) -> Result<RootCause> {
    let grounds = grounds_for(state, incident).await;

    let settings = state.config.read().await.llm.clone();
    let api_key = crate::secrets::get_secret(settings.provider.secret_key()).await?;
    let provider = build_provider(&settings, api_key)?;

    let response = provider
        .complete(CompletionRequest {
            model: settings.model.clone(),
            system: SYSTEM.to_string(),
            messages: vec![ChatMessage::user(grounds.join("\n"))],
            tools: Vec::new(),
            max_tokens: MAX_TOKENS,
            temperature: 0.0,
        })
        .await?;

    let raw = parse(&response.text)?;
    let cause = validate(raw, incident, grounds, &settings.model)?;

    record(state, incident, &cause).await;
    Ok(cause)
}

/// Pull the JSON object out of the answer.
///
/// Same tolerance as `predict::parse` and for the same reason: models wrap
/// JSON in prose and in fences often enough that refusing on the first stray
/// character would fail for a reason nobody cares about.
fn parse(text: &str) -> Result<Raw> {
    let trimmed = text.trim();
    let body = match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if end > start => &trimmed[start..=end],
        _ => {
            return Err(AppError::Llm(format!(
                "the model did not answer with a JSON object: {}",
                trimmed.chars().take(200).collect::<String>()
            )))
        }
    };

    serde_json::from_str::<Raw>(body)
        .map_err(|e| AppError::Llm(format!("could not read the model's analysis: {e}")))
}

/// Check the answer says something, and refuse it when it does not.
fn validate(
    raw: Raw,
    incident: &Incident,
    grounds: Vec<String>,
    model: &str,
) -> Result<RootCause> {
    let summary = raw.summary.trim().to_string();
    if summary.is_empty() {
        return Err(AppError::Llm(
            "the model returned an analysis with no conclusion in it".to_string(),
        ));
    }

    // The falsifiability rule, enforced rather than requested. A model that
    // skipped this field produced a story, and a story is what this module
    // exists to avoid shipping.
    let disconfirm = raw.disconfirm.trim().to_string();
    if disconfirm.is_empty() {
        return Err(AppError::Llm(
            "the model did not say what would disprove its account, so there is no way \
             to check it. Ask again."
                .to_string(),
        ));
    }

    let dissent = raw.dissent.trim().to_string();
    Ok(RootCause {
        incident_id: incident.id.clone(),
        summary,
        confidence: match raw.confidence.trim().to_ascii_lowercase().as_str() {
            "high" => Confidence::High,
            "low" => Confidence::Low,
            // An unreadable confidence is not high confidence.
            _ => Confidence::Medium,
        },
        chain: clean(raw.chain),
        because: clean(raw.because),
        confirm: clean(raw.confirm),
        disconfirm,
        agrees: raw.agrees,
        // A dissent on an answer that agrees is the model talking to itself.
        dissent: (!raw.agrees && !dissent.is_empty()).then_some(dissent),
        model: model.to_string(),
        grounds,
        at: Utc::now(),
    })
}

fn clean(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

/// File it where the rules' findings already are, so the Scorecard rates it.
///
/// An RCA that reads well and is wrong should cost the model its precision,
/// exactly as a bad prediction does. Filed against the connection root named
/// as the root, which is the connection somebody would open to check it.
async fn record(state: &Arc<AppState>, incident: &Incident, cause: &RootCause) {
    let profile_id = incident.lead_profile_id.clone();
    let profile_name = {
        let names = names_of(state).await;
        names
            .get(&profile_id)
            .cloned()
            .unwrap_or_else(|| profile_id.clone())
    };

    let finding = crate::advisor::Finding {
        id: format!("rca:{}", incident.id),
        // One rule name for every RCA, so the Scorecard can say how often this
        // path is right as a path — "the model is 60% precise" names nothing
        // to fix; "root-cause analysis is 60% precise" names this file.
        rule: "rca".to_string(),
        profile_id: profile_id.clone(),
        profile_name,
        provenance: crate::advisor::Provenance::Model {
            model: cause.model.clone(),
        },
        severity: crate::advisor::Severity::Now,
        title: cause.summary.clone(),
        detail: cause.chain.join(" → "),
        evidence: Vec::new(),
        suggestion: None,
    };

    state.claims.write().await.record(
        &profile_id,
        crate::claims::Claim {
            finding,
            at: cause.at,
            conversation_id: None,
        },
    );
}

/// Connection names by id, for turning ids in the incident into words.
async fn names_of(state: &Arc<AppState>) -> HashMap<String, String> {
    state
        .config
        .read()
        .await
        .profiles
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect()
}

/// A timestamp as a signed offset from onset.
///
/// The whole point of this module's prompt: `+14s` is a causal statement and
/// `2026-09-19T09:43:55Z` is arithmetic homework.
fn offset(at: DateTime<Utc>, onset: DateTime<Utc>) -> String {
    let secs = (at - onset).num_seconds();
    let sign = if secs < 0 { "-" } else { "+" };
    let abs = secs.abs();
    if abs < 60 {
        format!("{sign}{abs}s")
    } else if abs < 3600 {
        format!("{sign}{}m{:02}s", abs / 60, abs % 60)
    } else {
        format!("{sign}{}h{:02}m", abs / 3600, (abs % 3600) / 60)
    }
}

/// Root's own words for why these alerts were grouped.
fn describe(reason: &crate::incidents::GroupingReason) -> String {
    use crate::incidents::GroupingReason as R;
    match reason {
        R::SameConnection { .. } => "several conditions on one connection".to_string(),
        R::DependencyFailedFirst { root, dependent } => {
            format!("{dependent} declares it needs {root}, and {root} failed first")
        }
        R::DependentPressuredIt { origin, pressured } => format!(
            "{origin} failed and {pressured}, which it depends on, degraded shortly after \
             — pressure travelling backwards along the edge, not the cause"
        ),
        R::SimultaneousOnset { count, window_secs } => format!(
            "{count} independent failures within {window_secs}s and nothing else in common"
        ),
    }
}

/// Log lines across the window, clustered into templates with per-bucket counts.
async fn log_templates(
    state: &Arc<AppState>,
    incident: &Incident,
    onset: DateTime<Utc>,
) -> Vec<String> {
    let since = onset - Duration::minutes(LOG_LOOKBACK_MINUTES);
    let events: Vec<crate::model::LogEvent> = state
        .logs
        .read()
        .await
        .query(&crate::state::LogFilter {
            since: Some(since),
            limit: Some(5000),
            ..Default::default()
        })
        .into_iter()
        .filter(|e| incident.profile_ids.contains(&e.profile_id))
        .collect();
    if events.is_empty() {
        return Vec::new();
    }

    let until = Utc::now();
    let mut drain = crate::patterns::Drain::new(crate::patterns::DrainConfig::default());
    let mut members: HashMap<usize, Vec<&crate::model::LogEvent>> = HashMap::new();
    for event in &events {
        if let Some(id) = drain.add(&event.message) {
            members.entry(id).or_default().push(event);
        }
    }

    const BUCKETS: usize = 8;
    let span_ms = (until - since).num_milliseconds().max(1) as f64;
    let mut rows: Vec<(usize, String)> = Vec::new();

    for (id, cluster) in drain.clusters().iter().enumerate() {
        let Some(group) = members.get(&id) else {
            continue;
        };
        if group.is_empty() {
            continue;
        }
        let mut buckets = vec![0usize; BUCKETS];
        for event in group {
            let at = (event.timestamp - since).num_milliseconds().max(0) as f64;
            let slot = ((at / span_ms) * BUCKETS as f64) as usize;
            buckets[slot.min(BUCKETS - 1)] += 1;
        }
        let errors = group
            .iter()
            .filter(|e| e.level >= crate::model::LogLevel::Error)
            .count();
        rows.push((
            group.len(),
            format!(
                "- x{} ({errors} error) {} | buckets {:?}",
                group.len(),
                cluster.template_string(),
                buckets
            ),
        ));
    }

    // Busiest first: a template that fired four hundred times is the shape of
    // the window, and one that fired twice is a detail.
    rows.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
    rows.into_iter()
        .take(MAX_TEMPLATES)
        .map(|(_, line)| line)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incident() -> Incident {
        Incident {
            id: "i1".into(),
            title: "orders-db and checkout-api".into(),
            severity: crate::model::AlertSeverity::Critical,
            health: crate::model::Health::Down,
            started: Utc::now() - Duration::minutes(5),
            last_seen: Utc::now(),
            profile_ids: vec!["p4".into(), "p1".into()],
            alert_ids: vec!["a1".into()],
            roots: vec!["p4".into()],
            lead_profile_id: "p4".into(),
            acknowledged: false,
            confidence: Confidence::High,
            grouping: Vec::new(),
        }
    }

    fn raw() -> Raw {
        Raw {
            summary: "A deploy shrank the connection pool".into(),
            confidence: "high".into(),
            chain: vec!["pool size dropped to 5".into(), "checkout-api blocked".into()],
            because: vec!["- -90s orders-db: pool 25 → 5".into()],
            confirm: vec!["read max_connections on orders-db".into()],
            disconfirm: "the pool size is still 25".into(),
            agrees: true,
            dissent: String::new(),
        }
    }

    #[test]
    fn an_offset_reads_as_a_sequence_not_as_a_clock() {
        let onset = Utc::now();
        assert_eq!(offset(onset, onset), "+0s");
        assert_eq!(offset(onset + Duration::seconds(14), onset), "+14s");
        // A deploy before onset is the common case and must read as before.
        assert_eq!(offset(onset - Duration::seconds(90), onset), "-1m30s");
        assert_eq!(offset(onset + Duration::seconds(3725), onset), "+1h02m");
    }

    #[test]
    fn an_account_nothing_could_disprove_is_refused() {
        let mut r = raw();
        r.disconfirm = "   ".into();
        let error = validate(r, &incident(), Vec::new(), "m").expect_err("no falsifier");
        assert!(error.to_string().contains("disprove"), "{error}");
    }

    #[test]
    fn an_analysis_with_no_conclusion_is_refused() {
        let mut r = raw();
        r.summary = "".into();
        let error = validate(r, &incident(), Vec::new(), "m").expect_err("no summary");
        assert!(error.to_string().contains("no conclusion"), "{error}");
    }

    #[test]
    fn an_unreadable_confidence_is_not_read_as_high() {
        let mut r = raw();
        r.confidence = "extremely".into();
        let cause = validate(r, &incident(), Vec::new(), "m").unwrap();
        assert_eq!(cause.confidence, Confidence::Medium);
    }

    #[test]
    fn dissent_is_dropped_when_the_model_agrees() {
        // Otherwise the panel renders an objection beside an agreement, which
        // reads as the model arguing with itself.
        let mut r = raw();
        r.agrees = true;
        r.dissent = "actually it was checkout-api".into();
        let cause = validate(r, &incident(), Vec::new(), "m").unwrap();
        assert!(cause.dissent.is_none());
    }

    #[test]
    fn dissent_survives_when_the_model_disagrees() {
        let mut r = raw();
        r.agrees = false;
        r.dissent = "checkout-api retried hard enough to exhaust the pool".into();
        let cause = validate(r, &incident(), Vec::new(), "m").unwrap();
        assert_eq!(
            cause.dissent.as_deref(),
            Some("checkout-api retried hard enough to exhaust the pool")
        );
    }

    #[test]
    fn blank_list_entries_are_dropped_rather_than_rendered_as_empty_rows() {
        let mut r = raw();
        r.chain = vec!["  ".into(), "real step".into(), "".into()];
        let cause = validate(r, &incident(), Vec::new(), "m").unwrap();
        assert_eq!(cause.chain, vec!["real step"]);
    }

    #[test]
    fn a_fenced_answer_still_parses() {
        let text = "Here is the analysis:\n```json\n{\"summary\":\"x\",\"disconfirm\":\"y\"}\n```";
        let raw = parse(text).expect("fenced JSON");
        assert_eq!(raw.summary, "x");
    }

    #[test]
    fn prose_with_no_object_in_it_is_refused_by_name() {
        let error = parse("I could not determine a cause.").expect_err("no object");
        assert!(error.to_string().contains("did not answer"), "{error}");
    }

    #[test]
    fn the_grouping_reasons_all_render_as_sentences() {
        use crate::incidents::GroupingReason as R;
        for reason in [
            R::SameConnection {
                profile_id: "p1".into(),
            },
            R::DependencyFailedFirst {
                root: "orders-db".into(),
                dependent: "checkout-api".into(),
            },
            R::DependentPressuredIt {
                origin: "checkout-api".into(),
                pressured: "orders-db".into(),
            },
            R::SimultaneousOnset {
                count: 4,
                window_secs: 30,
            },
        ] {
            let text = describe(&reason);
            assert!(!text.is_empty(), "{reason:?} has no sentence");
            assert!(
                !text.contains("{}"),
                "{reason:?} rendered a placeholder: {text}"
            );
        }
    }
}
