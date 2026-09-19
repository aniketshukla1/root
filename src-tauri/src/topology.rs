//! The declared dependency graph, and what can honestly be concluded from it.
//!
//! root watches connections, not the network between them, so it cannot
//! discover topology and does not try. Every edge here was asserted by whoever
//! configured the profile — the same standing as a reported deploy rather than
//! an observed one — which is why nothing in this module ever claims causality
//! on its own. It narrows a set of candidates; the caller still needs evidence.
//!
//! Only `depends_on` is stored. Dependents are derived, because "what does this
//! need?" is a question an operator can answer and "what needs this?" is one
//! they usually cannot.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Duration, Utc};

use crate::config::ConnectionProfile;

/// Adjacency in both directions, built once per read.
#[derive(Debug, Default)]
pub struct Topology {
    /// profile id → what it needs
    depends_on: HashMap<String, Vec<String>>,
    /// profile id → what needs it
    dependents: HashMap<String, Vec<String>>,
}

impl Topology {
    /// Edges to unknown profiles and self-edges are dropped rather than
    /// rejected: deleting a connection must not invalidate another's config,
    /// and a profile cannot be its own dependency however it was typed in.
    pub fn build(profiles: &[ConnectionProfile]) -> Self {
        let known: HashSet<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
        let mut topology = Topology::default();

        for profile in profiles {
            let mut seen = HashSet::new();
            let edges: Vec<String> = profile
                .depends_on
                .iter()
                .filter(|id| id.as_str() != profile.id)
                .filter(|id| known.contains(id.as_str()))
                .filter(|id| seen.insert(id.as_str()))
                .cloned()
                .collect();

            for target in &edges {
                topology
                    .dependents
                    .entry(target.clone())
                    .or_default()
                    .push(profile.id.clone());
            }
            topology.depends_on.insert(profile.id.clone(), edges);
        }

        topology
    }

    pub fn depends_on(&self, id: &str) -> &[String] {
        self.depends_on.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn dependents(&self, id: &str) -> &[String] {
        self.dependents.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Everything that would be affected if `id` failed, transitively.
    ///
    /// Breadth-first with a visited set, so a cycle costs one extra visit
    /// rather than hanging. `id` itself is never included.
    pub fn impact(&self, id: &str) -> Vec<String> {
        let mut seen: HashSet<&str> = HashSet::from([id]);
        let mut queue: VecDeque<&str> = VecDeque::from([id]);
        let mut out = Vec::new();

        while let Some(current) = queue.pop_front() {
            for dependent in self.dependents(current) {
                if seen.insert(dependent.as_str()) {
                    out.push(dependent.clone());
                    queue.push_back(dependent.as_str());
                }
            }
        }
        out
    }

    /// Of a failing set, which are not explained by anything else failing —
    /// using the clock as well as the graph.
    ///
    /// Topology alone is not enough, and trusting it alone is actively
    /// dangerous. If orders-api depends on payments-db and both are failing,
    /// the graph says the database explains the api. But if the database
    /// turned two minutes LATER, it cannot have caused what happened before
    /// it: what is far more likely is the reverse — a restart storm in the api
    /// hammering reconnections at the database it depends on.
    ///
    /// So a dependency only explains a dependent when it failed first, within
    /// `tolerance` for clock skew and poll jitter. An edge pointing the other
    /// way in time is left alone, and both ends stay roots for a human to
    /// judge.
    pub fn roots_with_onset<'a>(
        &self,
        failing: &[(&'a str, DateTime<Utc>)],
        tolerance: Duration,
    ) -> Vec<&'a str> {
        let onset: HashMap<&str, DateTime<Utc>> = failing.iter().copied().collect();

        let roots: Vec<&str> = failing
            .iter()
            .filter(|(id, started)| {
                !self.depends_on(id).iter().any(|dep| {
                    onset
                        .get(dep.as_str())
                        .is_some_and(|dep_started| *dep_started <= *started + tolerance)
                })
            })
            .map(|(id, _)| *id)
            .collect();

        if roots.is_empty() {
            failing.iter().map(|(id, _)| *id).collect()
        } else {
            roots
        }
    }

    /// Of a failing set, which are not explained by anything else failing.
    ///
    /// Graph only — use [`Topology::roots_with_onset`] wherever onset times are
    /// available, which is everywhere that matters.
    ///
    /// This is the whole point of declaring dependencies: when orders-api and
    /// payments-db are both unhealthy and orders-api depends on payments-db,
    /// the database is the one worth looking at first. A member with no failing
    /// dependency is a root; everything else is downstream of one.
    ///
    /// Two honest limits. A cycle has no root, so every member of it is
    /// returned rather than none — a wrong answer that is visible beats a
    /// confident empty one. And an undeclared dependency is invisible here, so
    /// a root may simply be a connection nobody finished configuring.
    pub fn roots_of<'a>(&self, failing: &[&'a str]) -> Vec<&'a str> {
        let set: HashSet<&str> = failing.iter().copied().collect();

        let roots: Vec<&str> = failing
            .iter()
            .copied()
            .filter(|id| {
                !self
                    .depends_on(id)
                    .iter()
                    .any(|dep| set.contains(dep.as_str()))
            })
            .collect();

        if roots.is_empty() { failing.to_vec() } else { roots }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            snapshot_sink: None,
            runtime: Default::default(),
            metadata: Default::default(),
        }
    }

    /// The stored direction is dependencies; dependents come back reversed.
    #[test]
    fn dependents_are_derived_from_declared_dependencies() {
        let t = Topology::build(&[
            profile("web", &["api"]),
            profile("api", &["db"]),
            profile("db", &[]),
        ]);

        assert_eq!(t.depends_on("web"), ["api"]);
        assert_eq!(t.dependents("db"), ["api"]);
        assert_eq!(t.dependents("api"), ["web"]);
        assert!(t.dependents("web").is_empty());
    }

    /// Deleting a connection must not invalidate another's config.
    #[test]
    fn edges_to_unknown_profiles_are_dropped() {
        let t = Topology::build(&[profile("web", &["api", "deleted-last-week"])]);
        assert!(t.depends_on("web").is_empty(), "api is not a known profile either");

        let t = Topology::build(&[profile("web", &["api"]), profile("api", &[])]);
        assert_eq!(t.depends_on("web"), ["api"]);
    }

    #[test]
    fn self_edges_and_duplicates_are_dropped() {
        let t = Topology::build(&[profile("web", &["web", "api", "api"]), profile("api", &[])]);
        assert_eq!(t.depends_on("web"), ["api"]);
    }

    #[test]
    fn impact_is_transitive_and_excludes_the_origin() {
        let t = Topology::build(&[
            profile("web", &["api"]),
            profile("api", &["db"]),
            profile("worker", &["db"]),
            profile("db", &[]),
            profile("unrelated", &[]),
        ]);

        let mut impact = t.impact("db");
        impact.sort();
        assert_eq!(impact, ["api", "web", "worker"]);
        assert!(t.impact("web").is_empty());
    }

    /// A cycle costs one extra visit, not a hang.
    #[test]
    fn impact_terminates_on_a_cycle() {
        let t = Topology::build(&[profile("a", &["b"]), profile("b", &["a"])]);
        assert_eq!(t.impact("a"), ["b"]);
    }

    /// The point of the graph: of everything failing, what is not explained by
    /// something else failing.
    #[test]
    fn roots_exclude_members_downstream_of_another_failure() {
        let t = Topology::build(&[
            profile("web", &["api"]),
            profile("api", &["db"]),
            profile("db", &[]),
        ]);

        assert_eq!(t.roots_of(&["web", "api", "db"]), ["db"]);
        // The database is healthy this time, so the api is the root.
        assert_eq!(t.roots_of(&["web", "api"]), ["api"]);
    }


    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + minute * 60, 0).unwrap()
    }

    /// The case the graph gets wrong on its own. orders-api depends on
    /// payments-db, so topology says the database explains the api — but the
    /// database turned two minutes later, so it cannot have.
    #[test]
    fn a_dependency_that_failed_later_does_not_explain_its_dependent() {
        let t = Topology::build(&[
            profile("orders-api", &["payments-db"]),
            profile("payments-db", &[]),
        ]);

        // Graph alone: the database is the only root, and the api is explained.
        assert_eq!(t.roots_of(&["orders-api", "payments-db"]), ["payments-db"]);

        // With the clock: the api turned first, so nothing explains it, and the
        // database is not credited with a failure that predates it.
        let mut roots = t.roots_with_onset(
            &[("orders-api", at(4)), ("payments-db", at(6))],
            Duration::seconds(30),
        );
        roots.sort();
        assert_eq!(roots, ["orders-api", "payments-db"]);
    }

    /// The ordinary case still works: a dependency that failed first explains
    /// what depends on it.
    #[test]
    fn a_dependency_that_failed_first_still_explains_its_dependent() {
        let t = Topology::build(&[
            profile("orders-api", &["payments-db"]),
            profile("payments-db", &[]),
        ]);

        let roots = t.roots_with_onset(
            &[("payments-db", at(2)), ("orders-api", at(5))],
            Duration::seconds(30),
        );
        assert_eq!(roots, ["payments-db"]);
    }

    /// Two connections that turn together are one event, not a race decided by
    /// poll jitter.
    #[test]
    fn simultaneous_onsets_are_within_tolerance() {
        let t = Topology::build(&[profile("web", &["api"]), profile("api", &[])]);

        let roots = t.roots_with_onset(
            &[("api", at(5)), ("web", at(5))],
            Duration::seconds(30),
        );
        assert_eq!(roots, ["api"], "the dependency still explains the dependent");
    }

    #[test]
    fn unrelated_failures_are_all_roots() {
        let t = Topology::build(&[profile("a", &[]), profile("b", &[])]);
        let mut roots = t.roots_of(&["a", "b"]);
        roots.sort();
        assert_eq!(roots, ["a", "b"]);
    }

    /// A cycle has no root. Returning every member is a visible wrong answer;
    /// returning none would silently drop the incident.
    #[test]
    fn a_failing_cycle_returns_every_member_rather_than_none() {
        let t = Topology::build(&[profile("a", &["b"]), profile("b", &["a"])]);
        let mut roots = t.roots_of(&["a", "b"]);
        roots.sort();
        assert_eq!(roots, ["a", "b"]);
    }
}
