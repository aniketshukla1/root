//! Correlating alerts into incidents.
//!
//! An operator does not have four problems when orders-api OOMs and three
//! things that talk to it start complaining — they have one, and a list that
//! says four is a list that hides the shape of the failure. This module is the
//! part that turns the alert stream into the small number of things actually
//! happening.
//!
//! Three properties are deliberate, and each one costs something:
//!
//! It runs without a model. Grouping is derivation from what the collectors
//! already record — onset times, the declared graph, observed changes — so an
//! estate with no LLM configured still gets the whole benefit, and a model that
//! is wrong can only mis-describe an incident, never invent one.
//!
//! It says why. Every incident carries the reasons its members were grouped, so
//! a wrong grouping is arguable rather than mysterious.
//!
//! It prefers under-merging. Two incidents shown as two costs a second glance;
//! two shown as one costs a wrong diagnosis, and the merged story is confident
//! enough to be believed. Where the evidence is thin the members stay apart.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::model::{Alert, AlertSeverity, Health, HealthSnapshot};
use crate::topology::Topology;

/// Clock skew and poll jitter between two connections reporting the same event.
const ONSET_TOLERANCE: Duration = Duration::seconds(30);

/// How close together onsets must be to look like one event rather than
/// coincidence, and how many it takes before that is worth suggesting.
const STORM_WINDOW: Duration = Duration::seconds(90);
const STORM_MIN_CONNECTIONS: usize = 4;

/// How long after a connection fails its own dependencies may degrade under
/// the pressure and still be the same incident.
///
/// Bounded, unlike the forward direction. A database that has been down for
/// three hours still explains a service that just fell over — causation
/// upstream does not expire. Back-pressure does: a retry storm shows up in
/// seconds to minutes, so a dependency that degrades half an hour later is
/// more likely a second problem than an echo of the first.
const PRESSURE_WINDOW: Duration = Duration::minutes(15);

/// Why a set of alerts is being shown as one thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GroupingReason {
    /// Several conditions on one connection. Never in doubt.
    SameConnection { profile_id: String },
    /// A declared dependency that failed first, so it can explain the other.
    DependencyFailedFirst { root: String, dependent: String },
    /// A connection failed and something it depends on degraded shortly after
    /// — a retry storm reaching the database rather than the database causing
    /// the outage. Real propagation, but along the edge backwards, so it never
    /// grants high confidence on its own.
    DependentPressuredIt { origin: String, pressured: String },
    /// Many things turning at once, with nothing else in common. The weakest
    /// reason here, and the one most worth overruling.
    ///
    /// `count` is the number of INDEPENDENT failures that coincided, not the
    /// number of connections: a declared chain that failed for a known reason
    /// enters the storm as one participant. Four connections that already
    /// explain each other are one coincidence, not four.
    SimultaneousOnset { count: usize, window_secs: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    /// Stable while the originating condition persists — see `incident_id`.
    pub id: String,
    pub title: String,
    pub severity: AlertSeverity,
    pub health: Health,
    pub started: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// Every connection involved, worst and earliest first.
    pub profile_ids: Vec<String>,
    pub alert_ids: Vec<String>,
    /// Members not explained by another member failing.
    pub roots: Vec<String>,
    /// The connection the title is about — the worst, earliest alert among the
    /// roots. Carried rather than left for the UI to re-derive, because a UI
    /// that guesses picks a different one and then names one connection while
    /// offering to open another.
    pub lead_profile_id: String,
    /// True only when every alert in it has been acknowledged.
    pub acknowledged: bool,
    pub confidence: Confidence,
    pub grouping: Vec<GroupingReason>,
}

/// Disjoint sets over connection ids.
struct Merge {
    parent: HashMap<String, String>,
}

impl Merge {
    fn new<'a>(ids: impl Iterator<Item = &'a str>) -> Self {
        Merge {
            parent: ids.map(|id| (id.to_string(), id.to_string())).collect(),
        }
    }

    fn find(&mut self, id: &str) -> String {
        let mut current = id.to_string();
        while let Some(next) = self.parent.get(&current) {
            if next == &current {
                return current;
            }
            current = next.clone();
        }
        current
    }

    fn union(&mut self, a: &str, b: &str) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent.insert(ra, rb);
        }
    }
}

/// When a connection's trouble began: the oldest still-firing alert on it.
fn onsets(alerts: &[Alert]) -> HashMap<&str, DateTime<Utc>> {
    let mut out: HashMap<&str, DateTime<Utc>> = HashMap::new();
    for alert in alerts.iter().filter(|a| a.active) {
        out.entry(alert.profile_id.as_str())
            .and_modify(|t| *t = (*t).min(alert.first_seen))
            .or_insert(alert.first_seen);
    }
    out
}

/// Group the active alerts into the incidents actually happening.
pub fn correlate(
    alerts: &[Alert],
    snapshots: &[HealthSnapshot],
    topology: &Topology,
) -> Vec<Incident> {
    let active: Vec<&Alert> = alerts.iter().filter(|a| a.active).collect();
    if active.is_empty() {
        return Vec::new();
    }

    let onset = onsets(alerts);
    let mut merge = Merge::new(onset.keys().copied());
    let mut reasons: Vec<GroupingReason> = Vec::new();

    // Several conditions on one connection are one incident about that
    // connection. This needs no rule beyond keying by profile.
    for (profile_id, _) in onset.iter() {
        if active.iter().filter(|a| a.profile_id == *profile_id).count() > 1 {
            reasons.push(GroupingReason::SameConnection {
                profile_id: (*profile_id).to_string(),
            });
        }
    }

    // A declared edge binds two connections when either end failed first, but
    // the two directions are not the same claim and are not recorded as one.
    //
    // Forward — the dependency failed first — is causation, and it does not
    // expire: a database down since breakfast still explains a service that
    // fell over at noon.
    //
    // Backward — the dependent failed first and its dependency degraded soon
    // after — is back-pressure. An operator whose service is OOM-looping and
    // whose database is now lagging has ONE problem, and a correlator that
    // insisted otherwise would be splitting the exact case this module opens
    // by describing. It is bounded by PRESSURE_WINDOW and never grants high
    // confidence, because the edge is being read against its direction.
    for (dependent, started) in onset.iter() {
        for dep in topology.depends_on(dependent) {
            let Some(dep_started) = onset.get(dep.as_str()) else {
                continue;
            };
            if *dep_started <= *started + ONSET_TOLERANCE {
                merge.union(dependent, dep);
                reasons.push(GroupingReason::DependencyFailedFirst {
                    root: dep.clone(),
                    dependent: (*dependent).to_string(),
                });
            } else if *dep_started <= *started + PRESSURE_WINDOW {
                merge.union(dependent, dep);
                reasons.push(GroupingReason::DependentPressuredIt {
                    origin: (*dependent).to_string(),
                    pressured: dep.clone(),
                });
            }
        }
    }

    // Many things turning together, with nothing declared to explain it, is
    // worth suggesting as one event — a partition, a shared host, a
    // certificate nobody tracked.
    //
    // It runs over the groups the declared edges have already formed, not over
    // raw connections, so a chain that failed for a known reason enters the
    // storm as one participant rather than inflating the count. Each group is
    // represented by its earliest onset, which is when that group's trouble
    // began.
    let storm = storm_cluster(&group_onsets(&mut merge, &onset));
    if let Some(members) = &storm {
        let first = members[0].as_str();
        for member in members.iter().skip(1) {
            merge.union(first, member);
        }
        reasons.push(GroupingReason::SimultaneousOnset {
            count: members.len(),
            window_secs: STORM_WINDOW.num_seconds(),
        });
    }

    // Collect the connections into their groups.
    let mut groups: HashMap<String, BTreeSet<String>> = HashMap::new();
    let keys: Vec<&str> = onset.keys().copied().collect();
    for id in keys {
        let root = merge.find(id);
        groups.entry(root).or_default().insert(id.to_string());
    }

    let mut incidents: Vec<Incident> = groups
        .into_values()
        .map(|members| build(&members, &active, snapshots, topology, &onset, &reasons, storm.is_some()))
        .collect();

    // Worst and newest first; the caller ranks for display.
    incidents.sort_by(|a, b| {
        a.acknowledged
            .cmp(&b.acknowledged)
            .then(severity_rank(a.severity).cmp(&severity_rank(b.severity)))
            .then(b.profile_ids.len().cmp(&a.profile_ids.len()))
            .then(a.started.cmp(&b.started))
    });
    incidents
}

/// One representative per current group, carrying that group's earliest onset.
///
/// Storms are counted in groups rather than connections so a declared chain
/// that failed for a known reason contributes one participant, not four. Four
/// connections that already explain each other are not four coincidences.
fn group_onsets<'a>(
    merge: &mut Merge,
    onset: &HashMap<&'a str, DateTime<Utc>>,
) -> HashMap<&'a str, DateTime<Utc>> {
    let mut earliest: HashMap<String, (&'a str, DateTime<Utc>)> = HashMap::new();
    for (id, started) in onset.iter() {
        let root = merge.find(id);
        earliest
            .entry(root)
            .and_modify(|slot| {
                if *started < slot.1 {
                    *slot = (id, *started);
                }
            })
            .or_insert((id, *started));
    }
    earliest.into_values().collect()
}

/// The largest set of groups whose onsets all fall inside one window. Returns
/// None unless it is big enough to be more likely than coincidence.
fn storm_cluster(onset: &HashMap<&str, DateTime<Utc>>) -> Option<Vec<String>> {
    let mut by_time: Vec<(&str, DateTime<Utc>)> = onset.iter().map(|(k, v)| (*k, *v)).collect();
    by_time.sort_by_key(|(_, t)| *t);

    let mut best: Vec<String> = Vec::new();
    for (i, (_, start)) in by_time.iter().enumerate() {
        let window: Vec<String> = by_time[i..]
            .iter()
            .take_while(|(_, t)| *t - *start <= STORM_WINDOW)
            .map(|(id, _)| (*id).to_string())
            .collect();
        if window.len() > best.len() {
            best = window;
        }
    }

    (best.len() >= STORM_MIN_CONNECTIONS).then_some(best)
}

fn severity_rank(severity: AlertSeverity) -> u8 {
    match severity {
        AlertSeverity::Critical => 0,
        AlertSeverity::Warning => 1,
        AlertSeverity::Info => 2,
    }
}

fn build(
    members: &BTreeSet<String>,
    active: &[&Alert],
    snapshots: &[HealthSnapshot],
    topology: &Topology,
    onset: &HashMap<&str, DateTime<Utc>>,
    all_reasons: &[GroupingReason],
    stormed: bool,
) -> Incident {
    let mine: Vec<&&Alert> = active
        .iter()
        .filter(|a| members.contains(&a.profile_id))
        .collect();

    let severity = mine
        .iter()
        .map(|a| a.severity)
        .min_by_key(|s| severity_rank(*s))
        .unwrap_or(AlertSeverity::Warning);

    let health = members
        .iter()
        .filter_map(|id| snapshots.iter().find(|s| &s.profile_id == id))
        .map(|s| s.health)
        .max()
        .unwrap_or(Health::Degraded);

    let started = mine.iter().map(|a| a.first_seen).min().unwrap_or_else(Utc::now);
    let last_seen = mine.iter().map(|a| a.last_seen).max().unwrap_or(started);

    let with_onset: Vec<(&str, DateTime<Utc>)> = members
        .iter()
        .filter_map(|id| onset.get(id.as_str()).map(|t| (id.as_str(), *t)))
        .collect();
    let mut roots: Vec<String> = topology
        .roots_with_onset(&with_onset, ONSET_TOLERANCE)
        .into_iter()
        .map(str::to_string)
        .collect();

    // Only the reasons that actually bind THESE members.
    let grouping: Vec<GroupingReason> = all_reasons
        .iter()
        .filter(|reason| match reason {
            GroupingReason::SameConnection { profile_id } => members.contains(profile_id),
            GroupingReason::DependencyFailedFirst { root, dependent } => {
                members.contains(root) && members.contains(dependent)
            }
            GroupingReason::DependentPressuredIt { origin, pressured } => {
                members.contains(origin) && members.contains(pressured)
            }
            GroupingReason::SimultaneousOnset { .. } => stormed && members.len() > 1,
        })
        .cloned()
        .collect();

    // Reading an edge backwards leaves both ends looking like origins: the
    // graph cannot separate a service from the database it is hammering,
    // because neither is downstream of a failure in the declared sense. When
    // declared edges are what bind the incident, the clock settles it — the
    // one that turned first is where to look. Topology narrows, the clock
    // arbitrates, and neither is a verdict.
    let bound_by_edges = grouping.iter().any(|r| {
        matches!(
            r,
            GroupingReason::DependencyFailedFirst { .. }
                | GroupingReason::DependentPressuredIt { .. }
        )
    });
    if bound_by_edges && roots.len() > 1 {
        if let Some(first) = roots
            .iter()
            .filter_map(|id| onset.get(id.as_str()).copied())
            .min()
        {
            roots.retain(|id| {
                onset
                    .get(id.as_str())
                    .is_some_and(|t| *t <= first + ONSET_TOLERANCE)
            });
        }
    }

    // Back-pressure alone is a weaker claim than a dependency that failed
    // first, so it caps confidence even when the roots came out clean.
    let only_pressure = bound_by_edges
        && !grouping
            .iter()
            .any(|r| matches!(r, GroupingReason::DependencyFailedFirst { .. }));

    // Confidence is about the GROUPING, not the diagnosis. One root with a
    // dependency edge behind it is a claim worth making; anything partly held
    // together by coincident timing is not.
    //
    // Partly, not only. An incident that merged on a declared edge AND then
    // absorbed a storm is not a confident claim with an asterisk — the storm
    // put connections in it that nothing explains, so the whole grouping is
    // now the thing to check before acting on it as one.
    let held_partly_by_timing = grouping
        .iter()
        .any(|r| matches!(r, GroupingReason::SimultaneousOnset { .. }))
        && members.len() > 1;
    let confidence = if held_partly_by_timing {
        Confidence::Low
    } else if members.len() == 1 {
        Confidence::High
    } else if only_pressure {
        Confidence::Medium
    } else if roots.len() == 1 {
        Confidence::High
    } else {
        Confidence::Medium
    };

    let lead = mine
        .iter()
        .filter(|a| roots.contains(&a.profile_id))
        .min_by_key(|a| (severity_rank(a.severity), a.first_seen))
        .or_else(|| mine.iter().min_by_key(|a| a.first_seen));

    let title = match lead {
        Some(alert) if members.len() > 1 => {
            format!("{} — {}", alert.profile_name, alert.title)
        }
        Some(alert) => alert.title.clone(),
        None => "Unattributed incident".into(),
    };

    let mut profile_ids: Vec<String> = members.iter().cloned().collect();
    profile_ids.sort_by_key(|id| onset.get(id.as_str()).copied().unwrap_or(started));

    Incident {
        id: incident_id(lead.map(|a| a.fingerprint.as_str()), started),
        lead_profile_id: lead
            .map(|a| a.profile_id.clone())
            .or_else(|| members.iter().next().cloned())
            .unwrap_or_default(),
        title,
        severity,
        health,
        started,
        last_seen,
        profile_ids,
        alert_ids: mine.iter().map(|a| a.id.clone()).collect(),
        roots,
        acknowledged: mine.iter().all(|a| a.acknowledged),
        confidence,
        grouping,
    }
}

/// Keyed on the originating condition, which is already stable across polls —
/// alerts are keyed by condition, so a problem that persists keeps its
/// fingerprint. New members joining an incident therefore do not re-key it.
///
/// The limit: if the originating condition recovers while downstream ones
/// persist, the incident is re-keyed and the UI sees a new one. That is
/// survivable and honest; keeping the old key would mean naming an incident
/// after something that is no longer wrong.
fn incident_id(fingerprint: Option<&str>, started: DateTime<Utc>) -> String {
    match fingerprint {
        Some(fp) => format!("inc:{fp}"),
        None => format!("inc:@{}", started.timestamp()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConnectionProfile;
    use crate::model::ServiceKind;

    fn profile(id: &str, depends_on: &[&str]) -> ConnectionProfile {
        ConnectionProfile {
            id: id.into(),
            name: id.into(),
            kind: ServiceKind::Docker,
            endpoint: String::new(),
            username: None,
            has_secret: false,
            enabled: true,
            poll_interval_secs: 15,
            options: Default::default(),
            depends_on: depends_on.iter().map(|s| (*s).into()).collect(),
            config_sources: Vec::new(),
            history_source: None,
            runtime: Default::default(),
            metadata: Default::default(),
        }
    }

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + minute * 60, 0).unwrap()
    }

    fn alert(id: &str, profile: &str, severity: AlertSeverity, minute: i64) -> Alert {
        Alert {
            id: id.into(),
            fingerprint: format!("{profile}/{id}"),
            profile_id: profile.into(),
            profile_name: profile.into(),
            kind: ServiceKind::Docker,
            severity,
            title: format!("{id} fired"),
            detail: String::new(),
            component: None,
            first_seen: at(minute),
            last_seen: at(minute + 1),
            active: true,
            acknowledged: false,
        }
    }

    fn snapshot(profile: &str, health: Health) -> HealthSnapshot {
        HealthSnapshot {
            profile_id: profile.into(),
            profile_name: profile.into(),
            kind: ServiceKind::Docker,
            health,
            summary: String::new(),
            metrics: Vec::new(),
            sections: Vec::new(),
            checked_at: at(10),
            latency_ms: 1,
            error: None,
        }
    }

    #[test]
    fn no_active_alerts_is_no_incidents() {
        let topology = Topology::build(&[profile("a", &[])]);
        assert!(correlate(&[], &[], &topology).is_empty());
    }

    /// Several conditions on one connection are one incident about it.
    #[test]
    fn alerts_on_one_connection_become_one_incident() {
        let topology = Topology::build(&[profile("orders-api", &[])]);
        let alerts = vec![
            alert("oom", "orders-api", AlertSeverity::Critical, 4),
            alert("errors", "orders-api", AlertSeverity::Warning, 6),
        ];

        let incidents = correlate(&alerts, &[snapshot("orders-api", Health::Down)], &topology);
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].alert_ids.len(), 2);
        assert_eq!(incidents[0].severity, AlertSeverity::Critical, "worst member wins");
        assert_eq!(incidents[0].started, at(4), "earliest member");
        assert!(incidents[0]
            .grouping
            .iter()
            .any(|r| matches!(r, GroupingReason::SameConnection { .. })));
    }

    /// The case the whole module exists for: a dependency that failed first
    /// pulls its dependent into one incident.
    #[test]
    fn a_dependency_that_failed_first_merges_with_its_dependent() {
        let topology = Topology::build(&[
            profile("orders-api", &["payments-db"]),
            profile("payments-db", &[]),
        ]);
        let alerts = vec![
            alert("lag", "payments-db", AlertSeverity::Critical, 2),
            alert("errors", "orders-api", AlertSeverity::Warning, 5),
        ];
        let snapshots = vec![
            snapshot("payments-db", Health::Down),
            snapshot("orders-api", Health::Degraded),
        ];

        let incidents = correlate(&alerts, &snapshots, &topology);
        assert_eq!(incidents.len(), 1, "one incident, not two: {incidents:#?}");
        assert_eq!(incidents[0].roots, ["payments-db"]);
        assert_eq!(incidents[0].confidence, Confidence::High);
        assert!(incidents[0]
            .grouping
            .iter()
            .any(|r| matches!(r, GroupingReason::DependencyFailedFirst { .. })));
    }

    /// And the case that makes it safe: the same edge, the other way in time.
    /// payments-db turned AFTER orders-api, so it cannot explain it — these
    /// stay two incidents rather than one blaming the database.
    #[test]
    fn a_dependency_that_degrades_soon_after_is_back_pressure_not_a_second_problem() {
        let topology = Topology::build(&[
            profile("orders-api", &["payments-db"]),
            profile("payments-db", &[]),
        ]);
        let alerts = vec![
            alert("oom", "orders-api", AlertSeverity::Critical, 4),
            alert("lag", "payments-db", AlertSeverity::Warning, 6),
        ];
        let snapshots = vec![
            snapshot("orders-api", Health::Down),
            snapshot("payments-db", Health::Degraded),
        ];

        let incidents = correlate(&alerts, &snapshots, &topology);
        assert_eq!(incidents.len(), 1, "one problem, not two: {incidents:#?}");

        let incident = &incidents[0];
        assert_eq!(
            incident.grouping,
            vec![GroupingReason::DependentPressuredIt {
                origin: "orders-api".into(),
                pressured: "payments-db".into(),
            }],
            "the edge was read backwards and must say so",
        );
        // The graph cannot separate them — neither is downstream of a declared
        // failure — so the clock does, and it names the one that turned first.
        assert_eq!(incident.roots, vec!["orders-api".to_string()]);
        assert_eq!(
            incident.confidence,
            Confidence::Medium,
            "back-pressure alone never earns high confidence",
        );
    }

    /// The bound that keeps back-pressure from becoming a licence to merge.
    #[test]
    fn a_dependency_that_degrades_much_later_is_a_second_problem() {
        let topology = Topology::build(&[
            profile("orders-api", &["payments-db"]),
            profile("payments-db", &[]),
        ]);
        let alerts = vec![
            alert("oom", "orders-api", AlertSeverity::Critical, 4),
            // Well past PRESSURE_WINDOW: a retry storm reaches a database in
            // seconds, so half an hour later is its own event.
            alert("lag", "payments-db", AlertSeverity::Warning, 40),
        ];
        let snapshots = vec![
            snapshot("orders-api", Health::Down),
            snapshot("payments-db", Health::Degraded),
        ];

        let incidents = correlate(&alerts, &snapshots, &topology);
        assert_eq!(
            incidents.len(),
            2,
            "too far apart to be one story: {incidents:#?}",
        );
    }

    /// Forward causation still outranks back-pressure, and still says so.
    #[test]
    fn a_dependency_that_failed_first_is_still_causation_not_pressure() {
        let topology = Topology::build(&[
            profile("orders-api", &["payments-db"]),
            profile("payments-db", &[]),
        ]);
        let alerts = vec![
            alert("lag", "payments-db", AlertSeverity::Critical, 4),
            alert("errors", "orders-api", AlertSeverity::Critical, 9),
        ];
        let snapshots = vec![
            snapshot("orders-api", Health::Down),
            snapshot("payments-db", Health::Down),
        ];

        let incidents = correlate(&alerts, &snapshots, &topology);
        assert_eq!(incidents.len(), 1);
        assert_eq!(
            incidents[0].grouping,
            vec![GroupingReason::DependencyFailedFirst {
                root: "payments-db".into(),
                dependent: "orders-api".into(),
            }],
        );
        assert_eq!(incidents[0].roots, vec!["payments-db".to_string()]);
        assert_eq!(incidents[0].confidence, Confidence::High);
    }

    /// Unrelated failures stay unrelated. Under-merging is the safe direction.
    #[test]
    fn unrelated_connections_stay_separate() {
        let topology = Topology::build(&[profile("a", &[]), profile("b", &[])]);
        let alerts = vec![
            alert("x", "a", AlertSeverity::Critical, 4),
            alert("y", "b", AlertSeverity::Warning, 40),
        ];

        let incidents = correlate(&alerts, &[], &topology);
        assert_eq!(incidents.len(), 2);
    }

    /// Many connections turning together with no declared edge is suggested as
    /// one event — but marked Low, because timing alone is the weakest reason
    /// here and the operator has to be able to overrule it.
    #[test]
    fn a_simultaneous_storm_is_grouped_but_held_at_low_confidence() {
        let profiles: Vec<ConnectionProfile> =
            ["a", "b", "c", "d", "e"].iter().map(|id| profile(id, &[])).collect();
        let topology = Topology::build(&profiles);
        let alerts: Vec<Alert> = ["a", "b", "c", "d", "e"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let mut a = alert("down", id, AlertSeverity::Critical, 10);
                a.first_seen = at(10) + Duration::seconds(i as i64 * 15);
                a
            })
            .collect();

        let incidents = correlate(&alerts, &[], &topology);
        assert_eq!(incidents.len(), 1, "five at once is one event: {incidents:#?}");
        assert_eq!(incidents[0].confidence, Confidence::Low);
        assert!(incidents[0]
            .grouping
            .iter()
            .any(|r| matches!(r, GroupingReason::SimultaneousOnset { .. })));
    }

    /// A declared chain that failed for a known reason is one participant in a
    /// storm, not four coincidences. Four connections that already explain each
    /// other must not, on their own, look like a partition.
    #[test]
    fn a_declared_chain_counts_once_and_does_not_make_a_storm_alone() {
        // d -> c -> b -> a, failing in that order, all inside the window.
        let topology = Topology::build(&[
            profile("a", &[]),
            profile("b", &["a"]),
            profile("c", &["b"]),
            profile("d", &["c"]),
        ]);
        let alerts: Vec<Alert> = ["a", "b", "c", "d"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let mut a = alert("down", id, AlertSeverity::Critical, 10);
                a.first_seen = at(10) + Duration::seconds(i as i64 * 15);
                a
            })
            .collect();

        let incidents = correlate(&alerts, &[], &topology);
        assert_eq!(incidents.len(), 1);
        assert!(
            !incidents[0]
                .grouping
                .iter()
                .any(|r| matches!(r, GroupingReason::SimultaneousOnset { .. })),
            "one explained chain is not a storm: {:#?}",
            incidents[0].grouping,
        );
        assert_eq!(
            incidents[0].confidence,
            Confidence::High,
            "a fully explained chain keeps its confidence",
        );
    }

    /// And when a chain IS swept up with unrelated connections, the whole
    /// incident drops to low — the storm put members in it that nothing
    /// explains, so the grouping as a whole is now the thing to check.
    #[test]
    fn a_chain_swept_into_a_storm_loses_its_confidence() {
        let topology = Topology::build(&[
            profile("a", &[]),
            profile("b", &["a"]),
            profile("x", &[]),
            profile("y", &[]),
            profile("z", &[]),
        ]);
        let alerts: Vec<Alert> = ["a", "b", "x", "y", "z"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let mut a = alert("down", id, AlertSeverity::Critical, 10);
                a.first_seen = at(10) + Duration::seconds(i as i64 * 15);
                a
            })
            .collect();

        let incidents = correlate(&alerts, &[], &topology);
        assert_eq!(incidents.len(), 1, "{incidents:#?}");
        assert_eq!(incidents[0].profile_ids.len(), 5);
        assert_eq!(incidents[0].confidence, Confidence::Low);
    }

    /// Three is under the threshold — coincidence is likelier than a partition,
    /// and over-merging costs a wrong diagnosis.
    #[test]
    fn too_few_simultaneous_failures_are_not_called_a_storm() {
        let profiles: Vec<ConnectionProfile> =
            ["a", "b", "c"].iter().map(|id| profile(id, &[])).collect();
        let topology = Topology::build(&profiles);
        let alerts: Vec<Alert> = ["a", "b", "c"]
            .iter()
            .map(|id| alert("down", id, AlertSeverity::Critical, 10))
            .collect();

        assert_eq!(correlate(&alerts, &[], &topology).len(), 3);
    }

    /// An incident keeps its id as members join, so acknowledgement and the
    /// UI's selection survive a poll.
    #[test]
    fn the_id_is_stable_as_an_incident_grows() {
        let topology = Topology::build(&[
            profile("orders-api", &["payments-db"]),
            profile("payments-db", &[]),
        ]);
        let first = vec![alert("lag", "payments-db", AlertSeverity::Critical, 2)];
        let later = vec![
            alert("lag", "payments-db", AlertSeverity::Critical, 2),
            alert("errors", "orders-api", AlertSeverity::Warning, 5),
        ];

        let a = correlate(&first, &[], &topology);
        let b = correlate(&later, &[], &topology);
        assert_eq!(a[0].id, b[0].id, "adding a member must not re-key the incident");
        assert_eq!(b[0].profile_ids.len(), 2);
    }

    /// An incident is only acknowledged when every alert in it is.
    #[test]
    fn acknowledgement_needs_every_member() {
        let topology = Topology::build(&[profile("a", &[])]);
        let mut alerts = vec![
            alert("x", "a", AlertSeverity::Critical, 4),
            alert("y", "a", AlertSeverity::Warning, 5),
        ];
        alerts[0].acknowledged = true;

        assert!(!correlate(&alerts, &[], &topology)[0].acknowledged);
        alerts[1].acknowledged = true;
        assert!(correlate(&alerts, &[], &topology)[0].acknowledged);
    }

    /// Resolved alerts are not incidents.
    #[test]
    fn inactive_alerts_are_ignored() {
        let topology = Topology::build(&[profile("a", &[])]);
        let mut alerts = vec![alert("x", "a", AlertSeverity::Critical, 4)];
        alerts[0].active = false;
        assert!(correlate(&alerts, &[], &topology).is_empty());
    }
}
