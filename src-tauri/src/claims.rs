//! What the assistant says it found.
//!
//! The rules produce findings and the journal scores them. Until now the
//! assistant produced prose, which cannot be scored at all — so the scorecard
//! had a column for the model that would have stayed empty forever, and the
//! loop this whole measurement exists to run never started.
//!
//! That loop is the point. A rule only finds what somebody thought to encode.
//! The model reaches further and is sometimes confidently wrong. The set of
//! things it found that no rule covers, **and that a human confirmed were
//! real**, is the queue of rules worth writing next — and each one that gets
//! written moves a class of problem from the half that has to be trusted to
//! the half that can be checked.
//!
//! So the assistant reports findings through a tool rather than in a sentence.
//! Structured, attributable, and rateable beside the rules on the same screen.
//!
//! ## Crediting the model for a rule's work
//!
//! When the assistant restates something a rule already found — which it will,
//! constantly, because it can see the rule's output — that must not count as a
//! model win. `ModelCitingRule` is a third category for exactly this, and it is
//! established two ways, because either alone leaks:
//!
//! The model declares it. Cheap, honest most of the time, and unreliable
//! precisely when the model has half-forgotten where an idea came from.
//!
//! The backend checks. A claim naming the same setting or metric as a live
//! rule finding is the same claim however it was worded, and that is a
//! comparison the model does not get a vote on.
//!
//! ## Why these do not persist
//!
//! A claim is what the assistant thought at one moment about a condition that
//! may already be over, so it is held in memory and expires — like incidents,
//! which are derived fresh, and unlike verdicts, which are the record.
//!
//! Rating one writes a verdict, and a verdict snapshots the whole finding. So
//! the judgement survives even though the claim does not, which is the right
//! way round: the thing worth keeping forever is what somebody decided, not
//! what the model said before they decided it.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use crate::advisor::{Evidence, Finding, Provenance, Severity, Suggestion};

/// Claims retained per connection. Generous for one conversation, bounded so a
/// chatty session cannot grow the store without limit.
const MAX_PER_PROFILE: usize = 40;

/// How long a claim stays rateable.
///
/// Long enough to still be there after lunch, short enough that a claim about
/// a condition that resolved last week is not sitting on the screen inviting a
/// verdict nobody can now check.
const EXPIRES_AFTER: Duration = Duration::hours(12);

/// What the assistant reported, as it arrived.
#[derive(Debug, Clone)]
pub struct Claim {
    pub finding: Finding,
    pub at: DateTime<Utc>,
    /// The conversation it came from, so a reader can go back to the reasoning.
    pub conversation_id: Option<String>,
}

/// Claims the assistant has made, by connection.
#[derive(Debug, Default)]
pub struct ClaimStore {
    by_profile: HashMap<String, Vec<Claim>>,
}

impl ClaimStore {
    /// Record a claim, replacing any earlier one that is plainly the same.
    ///
    /// Replacement rather than accumulation because the assistant re-reports
    /// as a conversation goes on, and three copies of one observation would
    /// let a single claim be rated three times and outweigh everything else in
    /// the score.
    pub fn record(&mut self, profile_id: &str, claim: Claim) {
        let claims = self.by_profile.entry(profile_id.to_string()).or_default();
        claims.retain(|existing| existing.finding.id != claim.finding.id);
        claims.push(claim);

        // Oldest out first.
        if claims.len() > MAX_PER_PROFILE {
            let excess = claims.len() - MAX_PER_PROFILE;
            claims.drain(0..excess);
        }
    }

    /// Live claims for one connection, newest first.
    pub fn for_profile(&self, profile_id: &str) -> Vec<Finding> {
        let cutoff = Utc::now() - EXPIRES_AFTER;
        let mut live: Vec<&Claim> = self
            .by_profile
            .get(profile_id)
            .map(|claims| claims.iter().filter(|c| c.at >= cutoff).collect())
            .unwrap_or_default();
        live.sort_by_key(|c| std::cmp::Reverse(c.at));
        live.into_iter().map(|c| c.finding.clone()).collect()
    }

    /// Drop what has expired. Called on record so the store does not need a
    /// timer of its own.
    pub fn sweep(&mut self) {
        let cutoff = Utc::now() - EXPIRES_AFTER;
        for claims in self.by_profile.values_mut() {
            claims.retain(|c| c.at >= cutoff);
        }
        self.by_profile.retain(|_, claims| !claims.is_empty());
    }

    /// Forget one connection's claims.
    pub fn clear(&mut self, profile_id: &str) {
        self.by_profile.remove(profile_id);
    }

    pub fn len(&self, profile_id: &str) -> usize {
        self.for_profile(profile_id).len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_profile.values().all(|c| c.is_empty())
    }
}

/// What the assistant hands in when it reports something.
pub struct Reported {
    pub title: String,
    pub detail: String,
    pub severity: Severity,
    /// Free-text facts the assistant is resting on. Kept as prose rather than
    /// forced into `Evidence::Setting` — the assistant reasons from log lines
    /// and timings as well as settings, and making it fabricate a structured
    /// shape it does not have would produce tidier evidence and less true
    /// evidence.
    pub evidence: Vec<String>,
    pub suggestion: Option<Suggestion>,
    /// The rule the assistant believes it is restating, if any.
    pub restates_rule: Option<String>,
}

/// Turn a report into a finding, deciding its provenance.
///
/// `rule_findings` are the live rule findings for the same connection — the
/// backstop that stops the model being credited for a rule's work when it
/// forgets to say so.
pub fn attribute(
    profile_id: &str,
    profile_name: &str,
    model: &str,
    reported: Reported,
    rule_findings: &[Finding],
) -> Finding {
    let cited = reported
        .restates_rule
        .clone()
        .filter(|rule| !rule.trim().is_empty())
        .or_else(|| overlapping_rule(&reported, rule_findings));

    let provenance = match &cited {
        Some(rule) => Provenance::ModelCitingRule {
            model: model.to_string(),
            rule: rule.clone(),
        },
        None => Provenance::Model {
            model: model.to_string(),
        },
    };

    Finding {
        // Stable for the same claim on the same connection, so re-reporting
        // replaces rather than duplicates, and a verdict filed against it
        // still matches after the assistant says it again.
        id: format!("{profile_id}:model:{}", slug(&reported.title)),
        provenance,
        rule: cited.unwrap_or_else(|| "model".to_string()),
        profile_id: profile_id.to_string(),
        profile_name: profile_name.to_string(),
        severity: reported.severity,
        title: reported.title,
        detail: reported.detail,
        evidence: reported
            .evidence
            .into_iter()
            .map(|text| Evidence::Setting {
                key: "observed".to_string(),
                value: text,
                source: "assistant".to_string(),
            })
            .collect(),
        suggestion: reported.suggestion,
    }
}

/// Whether a claim is really a rule's finding in different words.
///
/// Compares what each one rests on rather than how it is phrased: a claim that
/// names the same setting or the same metric as a live rule finding is about
/// the same thing, whatever wording it arrived in. Titles are useless for this
/// — the assistant will always phrase it differently, which is most of why it
/// is asked to declare the rule itself.
fn overlapping_rule(reported: &Reported, rule_findings: &[Finding]) -> Option<String> {
    let text = format!("{} {}", reported.title, reported.detail).to_ascii_lowercase();

    rule_findings
        .iter()
        .find(|finding| {
            finding.evidence.iter().any(|evidence| {
                let key = match evidence {
                    Evidence::Setting { key, .. } => key,
                    Evidence::Metric { key, .. } => key,
                    Evidence::Projection { key, .. } => key,
                };
                // Short keys like `cpu` would match half of everything; a
                // claim has to name something specific to count as the same.
                key.len() >= 6 && text.contains(&key.to_ascii_lowercase())
            })
        })
        .map(|finding| finding.rule.clone())
}

fn slug(title: &str) -> String {
    title
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MetricUnit;

    fn reported(title: &str) -> Reported {
        Reported {
            title: title.to_string(),
            detail: "because of a thing".to_string(),
            severity: Severity::Soon,
            evidence: vec!["a log line".to_string()],
            suggestion: None,
            restates_rule: None,
        }
    }

    fn rule_finding(rule: &str, evidence: Vec<Evidence>) -> Finding {
        Finding {
            id: "p1:x".into(),
            provenance: Provenance::Rule { id: rule.into() },
            rule: rule.into(),
            profile_id: "p1".into(),
            profile_name: "session-cache".into(),
            severity: Severity::Now,
            title: "a rule said something".into(),
            detail: String::new(),
            evidence,
            suggestion: None,
        }
    }

    // -------------------------------------------------------- attribution

    /// Something no rule covers is the model's own — and this is the category
    /// the whole measurement exists to grow.
    #[test]
    fn a_novel_claim_is_attributed_to_the_model() {
        let finding = attribute("p1", "session-cache", "claude-opus-5", reported("Replication lag is climbing on the secondary"), &[]);

        assert!(matches!(finding.provenance, Provenance::Model { .. }));
        assert_eq!(finding.rule, "model");
    }

    /// Counting a restatement as a model win credits the model for the rule's
    /// work, and the rules would look worse every time the assistant agreed
    /// with them.
    #[test]
    fn a_declared_restatement_is_its_own_category() {
        let mut report = reported("Redis will stop accepting writes soon");
        report.restates_rule = Some("redis.maxmemory-noeviction".into());

        let finding = attribute("p1", "session-cache", "claude-opus-5", report, &[]);
        match finding.provenance {
            Provenance::ModelCitingRule { rule, .. } => {
                assert_eq!(rule, "redis.maxmemory-noeviction")
            }
            other => panic!("{other:?}"),
        }
    }

    /// The backstop. The model forgets where an idea came from precisely when
    /// it matters, so the backend compares what each rests on.
    #[test]
    fn an_undeclared_restatement_is_caught_by_shared_evidence() {
        let rules = vec![rule_finding(
            "redis.maxmemory-noeviction",
            vec![Evidence::Setting {
                key: "maxmemory-policy".into(),
                value: "noeviction".into(),
                source: "CONFIG GET *".into(),
            }],
        )];

        let mut report = reported("This cache will refuse writes when it fills");
        report.detail = "maxmemory-policy is set to noeviction, so it will not evict".into();

        let finding = attribute("p1", "session-cache", "claude-opus-5", report, &rules);
        assert!(
            matches!(finding.provenance, Provenance::ModelCitingRule { .. }),
            "{:?}",
            finding.provenance
        );
    }

    /// A short key like `cpu` appears in half of everything. Matching on one
    /// would file every model claim as a restatement and empty the category
    /// the loop depends on.
    #[test]
    fn a_short_evidence_key_does_not_swallow_every_claim() {
        let rules = vec![rule_finding(
            "redis.something",
            vec![Evidence::Metric {
                key: "cpu".into(),
                label: "CPU".into(),
                value: 1.0,
                unit: MetricUnit::Percent,
                limit: None,
            }],
        )];

        let mut report = reported("The cpu governor is set to powersave");
        report.detail = "cpu scaling is limiting throughput".into();

        let finding = attribute("p1", "session-cache", "claude-opus-5", report, &rules);
        assert!(
            matches!(finding.provenance, Provenance::Model { .. }),
            "a claim merely mentioning cpu is not a restatement: {:?}",
            finding.provenance
        );
    }

    /// Ids must be stable so re-reporting replaces, and so a verdict filed
    /// earlier still matches the claim when the assistant says it again.
    #[test]
    fn the_same_claim_gets_the_same_id_twice() {
        let a = attribute("p1", "n", "m", reported("Replication lag is climbing"), &[]);
        let b = attribute("p1", "n", "m", reported("Replication lag is climbing"), &[]);
        assert_eq!(a.id, b.id);

        let other = attribute("p1", "n", "m", reported("Something else entirely"), &[]);
        assert_ne!(a.id, other.id);
    }

    #[test]
    fn ids_are_scoped_to_their_connection() {
        let a = attribute("p1", "n", "m", reported("Replication lag"), &[]);
        let b = attribute("p2", "n", "m", reported("Replication lag"), &[]);
        assert_ne!(a.id, b.id);
    }

    // -------------------------------------------------------------- store

    fn claim(id_title: &str, at: DateTime<Utc>) -> Claim {
        Claim {
            finding: attribute("p1", "n", "m", reported(id_title), &[]),
            at,
            conversation_id: None,
        }
    }

    /// The assistant re-reports as a conversation goes on. Three copies of one
    /// observation would let a single claim be rated three times.
    #[test]
    fn re_reporting_replaces_rather_than_accumulating() {
        let mut store = ClaimStore::default();
        store.record("p1", claim("Replication lag is climbing", Utc::now()));
        store.record("p1", claim("Replication lag is climbing", Utc::now()));

        assert_eq!(store.for_profile("p1").len(), 1);
    }

    /// A claim about a condition that ended last week should not be sitting on
    /// screen inviting a verdict nobody can check.
    #[test]
    fn a_stale_claim_stops_being_rateable() {
        let mut store = ClaimStore::default();
        store.record("p1", claim("Old news", Utc::now() - Duration::hours(13)));
        store.record("p1", claim("Current", Utc::now()));

        let live = store.for_profile("p1");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].title, "Current");
    }

    #[test]
    fn the_store_is_bounded_per_connection() {
        let mut store = ClaimStore::default();
        for i in 0..(MAX_PER_PROFILE + 10) {
            store.record("p1", claim(&format!("Claim number {i}"), Utc::now()));
        }
        assert_eq!(store.for_profile("p1").len(), MAX_PER_PROFILE);
    }

    #[test]
    fn claims_do_not_leak_between_connections() {
        let mut store = ClaimStore::default();
        store.record("p1", claim("One", Utc::now()));
        store.record("p2", claim("Two", Utc::now()));

        assert_eq!(store.for_profile("p1").len(), 1);
        assert_eq!(store.for_profile("p1")[0].title, "One");
        assert_eq!(store.for_profile("p2").len(), 1);
    }

    #[test]
    fn sweeping_drops_expired_claims_and_empty_connections() {
        let mut store = ClaimStore::default();
        store.record("p1", claim("Old", Utc::now() - Duration::hours(13)));
        store.sweep();
        assert!(store.is_empty());
    }
}
