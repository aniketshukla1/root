//! The estate as one graph: every system root knows of, how each is connected
//! to the others, and how root knows it.
//!
//! Nothing here is stored. The graph is assembled on each read from what root
//! already holds — connections and their declarations, the containers and pods
//! their hosts report, the calls traces show — so it is never staler than those
//! are. Every link says how it is known, because the four ways deserve
//! different trust:
//!
//! - **declared**: an operator said so (a dependency, a runtime, a file);
//! - **reported**: a connection's own answer (a host listing its containers);
//! - **observed**: seen happening (calls in traces);
//! - **inferred**: root's own matching (an image and a published port).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::commands::ObservedEdge;
use crate::config::ConnectionProfile;
use crate::files::Location;
use crate::model::{CellValue, Health, HealthSnapshot, ServiceKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A connection root watches.
    Service,
    /// What a host reports running: a container, a pod.
    Component,
    /// Something traces show being called, or calling, that no connection watches.
    External,
    /// Who owns a connection, from its operations details.
    Team,
    Environment,
    /// A declared configuration or log file, or a journal.
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// Needs the other to work.
    DependsOn,
    /// Sends it requests.
    Calls,
    /// Runs on that host, or as that container or pod.
    RunsOn,
    /// A container or pod of that host.
    PartOf,
    OwnedBy,
    InEnvironment,
    /// Its configuration lives in that file.
    ConfiguredBy,
    /// It writes its logs there.
    LogsTo,
    /// The file is read on that system.
    LocatedOn,
    /// Its long-range history is kept by that Prometheus.
    HistoryIn,
}

/// How a link is known, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Declared,
    Reported,
    Observed,
    Inferred,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// `service:<id>`, `component:<host>/<section>/<row>`, `external:<name>`,
    /// `team:<name>`, `environment:<name>`, `file:<id>/<source>`.
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    /// What a service or an identified component is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_kind: Option<ServiceKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<Health>,
    /// Short facts worth showing beside it: owner, criticality, path, where.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub facts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub relation: Relation,
    pub provenance: Provenance,
    /// "as payments-db-1, which the host does not report"; "also inferred:
    /// matched by image and port".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What traces saw on a call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traffic: Option<Traffic>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Traffic {
    pub calls: u64,
    pub errors: u64,
    pub mean_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstateGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub generated_at: DateTime<Utc>,
}

/// Node ids, one place, so a link and its ends cannot disagree on spelling.
pub fn service_id(profile_id: &str) -> String {
    format!("service:{profile_id}")
}
/// With its section, so a screen can open the component's own page.
fn component_id(host_profile_id: &str, section_id: &str, row_id: &str) -> String {
    format!("component:{host_profile_id}/{section_id}/{row_id}")
}

#[derive(Default)]
struct Builder {
    nodes: BTreeMap<String, Node>,
    /// (from, to, relation) → the edge, strongest provenance kept.
    edges: BTreeMap<(String, String, Relation), Edge>,
}

impl Builder {
    fn node(&mut self, id: String, kind: NodeKind, label: &str) -> &mut Node {
        self.nodes.entry(id.clone()).or_insert_with(|| Node {
            id,
            kind,
            label: label.to_string(),
            service_kind: None,
            health: None,
            facts: BTreeMap::new(),
        })
    }

    /// A link known two ways is one link: the stronger provenance is kept and
    /// the other is noted with its own detail, so a runtime both declared and
    /// matched by root reads "also inferred: matched by image and port".
    fn edge(&mut self, from: &str, to: &str, relation: Relation, provenance: Provenance, detail: Option<String>) {
        if from == to {
            return;
        }
        let key = (from.to_string(), to.to_string(), relation);
        match self.edges.get_mut(&key) {
            Some(existing) if existing.provenance == provenance => {}
            Some(existing) => {
                let (strong, strong_detail, weak, weak_detail) = if provenance < existing.provenance {
                    (provenance, detail, existing.provenance, existing.detail.take())
                } else {
                    (existing.provenance, existing.detail.take(), provenance, detail)
                };
                let also = match weak_detail {
                    Some(d) => format!("also {}: {d}", label_of(weak)),
                    None => format!("also {}", label_of(weak)),
                };
                existing.provenance = strong;
                existing.detail = Some(match strong_detail {
                    Some(d) => format!("{d}; {also}"),
                    None => also,
                });
            }
            None => {
                let edge = Edge { from: from.to_string(), to: to.to_string(), relation, provenance, detail, traffic: None };
                self.edges.insert(key, edge);
            }
        }
    }
}

fn label_of(provenance: Provenance) -> &'static str {
    match provenance {
        Provenance::Declared => "declared",
        Provenance::Reported => "reported",
        Provenance::Observed => "observed",
        Provenance::Inferred => "inferred",
    }
}

fn text(cell: Option<&CellValue>) -> Option<&str> {
    match cell {
        Some(CellValue::Text { value }) => Some(value.as_str()).filter(|v| *v != "—" && !v.is_empty()),
        _ => None,
    }
}

/// One graph from everything root holds. `calls` and `external` are the
/// service map's observed edges, between connections and across root's edge.
pub fn build(
    profiles: &[ConnectionProfile],
    snapshots: &HashMap<String, HealthSnapshot>,
    calls: &[ObservedEdge],
    external: &[ObservedEdge],
) -> EstateGraph {
    let mut g = Builder::default();
    let known: HashMap<&str, &ConnectionProfile> = profiles.iter().map(|p| (p.id.as_str(), p)).collect();

    // Connections, with their health and what their owners said about them.
    for profile in profiles {
        let id = service_id(&profile.id);
        let node = g.node(id.clone(), NodeKind::Service, &profile.name);
        node.service_kind = Some(profile.kind);
        node.health = Some(snapshots.get(&profile.id).map(|s| s.health).unwrap_or(Health::Unknown));
        let operations = profile.metadata.get("operations");
        let fact = |key: &str| operations.and_then(|o| o.get(key)).and_then(|v| v.as_str()).filter(|v| !v.is_empty());
        for key in ["owner", "contact", "criticality"] {
            if let Some(value) = fact(key) {
                node.facts.insert(key.to_string(), value.to_string());
            }
        }
        if let Some(team) = fact("team").map(str::to_string) {
            let team_id = format!("team:{team}");
            g.node(team_id.clone(), NodeKind::Team, &team);
            g.edge(&id, &team_id, Relation::OwnedBy, Provenance::Declared, None);
        }
        if let Some(environment) = fact("environment").map(str::to_string) {
            let env_id = format!("environment:{environment}");
            g.node(env_id.clone(), NodeKind::Environment, &environment);
            g.edge(&id, &env_id, Relation::InEnvironment, Provenance::Declared, None);
        }
    }

    // What each host reports running, and which connection root matched to it.
    for profile in profiles {
        let Some(snapshot) = snapshots.get(&profile.id) else { continue };
        for section in snapshot.sections.iter().filter(|s| s.id == "containers" || s.id == "pods") {
            for row in &section.rows {
                let id = component_id(&profile.id, &section.id, &row.id);
                let node = g.node(id.clone(), NodeKind::Component, &row.name);
                node.health = Some(row.health);
                node.service_kind = text(row.cells.get("kind"))
                    .and_then(|t| crate::registry::kind_from_label(t.trim_end_matches(" (not serving)")));
                if let Some(image) = text(row.cells.get("image")) {
                    node.facts.insert("image".into(), image.to_string());
                }
                g.edge(&id, &service_id(&profile.id), Relation::PartOf, Provenance::Reported, None);
                let watched_by = row.extra.get("watchedBy").and_then(|v| v.as_str()).filter(|w| known.contains_key(w));
                if let Some(watcher) = watched_by {
                    g.edge(&service_id(watcher), &id, Relation::RunsOn, Provenance::Inferred, Some("matched by image and port".into()));
                }
            }
        }
    }

    // Find the component a host reports by name, for declarations that name one.
    let component_named = |g: &Builder, host: &str, name: &str| -> Option<String> {
        let prefix = format!("component:{host}/");
        g.nodes
            .values()
            .find(|n| n.kind == NodeKind::Component && n.id.starts_with(&prefix) && (n.label == name || n.id.ends_with(&format!("/{name}"))))
            .map(|n| n.id.clone())
    };

    for profile in profiles {
        let id = service_id(&profile.id);

        for target in &profile.depends_on {
            if known.contains_key(target.as_str()) {
                g.edge(&id, &service_id(target), Relation::DependsOn, Provenance::Declared, None);
            }
        }

        // Where it runs, as declared: the component itself when the host
        // reports one by that name, else the host.
        if let Some(host) = profile.runtime.host_profile_id.as_deref().filter(|h| known.contains_key(h)) {
            let name = profile.runtime.name.as_deref().unwrap_or("");
            match component_named(&g, host, name).filter(|_| !name.is_empty()) {
                Some(component) => g.edge(&id, &component, Relation::RunsOn, Provenance::Declared, None),
                None => {
                    let detail = (!name.is_empty()).then(|| format!("as {name}, which the host does not report"));
                    g.edge(&id, &service_id(host), Relation::RunsOn, Provenance::Declared, detail)
                }
            }
        }

        if let Some(history) = &profile.history_source {
            if known.contains_key(history.prometheus_profile_id.as_str()) {
                g.edge(&id, &service_id(&history.prometheus_profile_id), Relation::HistoryIn, Provenance::Declared, None);
            }
        }

        // Declared files, and the system each is read on.
        let config = profile.config_sources.iter().map(|s| (&s.id, s.label.as_deref(), s.path.as_str(), &s.location, false, Relation::ConfiguredBy));
        let logs = profile.log_sources.iter().map(|s| (&s.id, s.label.as_deref(), s.path.as_str(), &s.location, s.journal, Relation::LogsTo));
        for (source_id, label, path, location, journal, relation) in config.chain(logs).collect::<Vec<_>>() {
            let file_id = format!("file:{}/{source_id}", profile.id);
            let shown = label.map(str::to_string).unwrap_or_else(|| if journal { format!("journal: {path}") } else { path.to_string() });
            let node = g.node(file_id.clone(), NodeKind::File, &shown);
            node.facts.insert("path".into(), path.to_string());
            if journal {
                node.facts.insert("journal".into(), "systemd".into());
            }
            g.edge(&id, &file_id, relation, Provenance::Declared, None);
            let place = match location {
                Location::Local => None,
                Location::Container { host_profile_id, container } => {
                    component_named(&g, host_profile_id, container).or_else(|| known.contains_key(host_profile_id.as_str()).then(|| service_id(host_profile_id)))
                }
                Location::Ssh { server_profile_id } => known.contains_key(server_profile_id.as_str()).then(|| service_id(server_profile_id)),
                Location::Pod { cluster_profile_id, namespace, pod, .. } => component_named(&g, cluster_profile_id, &format!("pod:{namespace}/{pod}"))
                    .or_else(|| known.contains_key(cluster_profile_id.as_str()).then(|| service_id(cluster_profile_id))),
            };
            match place {
                Some(place) => g.edge(&file_id, &place, Relation::LocatedOn, Provenance::Declared, None),
                None => {
                    g.node(file_id.clone(), NodeKind::File, &shown).facts.insert("on".into(), "this machine".into());
                }
            }
        }
    }

    // Calls seen in traces, inside the estate and across its edge.
    let ended = |g: &mut Builder, name: &str| -> String {
        if known.contains_key(name) {
            return service_id(name);
        }
        let id = format!("external:{name}");
        g.node(id.clone(), NodeKind::External, name);
        id
    };
    for call in calls.iter().chain(external) {
        let (from, to) = (ended(&mut g, &call.from), ended(&mut g, &call.to));
        g.edge(&from, &to, Relation::Calls, Provenance::Observed, None);
        if let Some(edge) = g.edges.get_mut(&(from, to, Relation::Calls)) {
            edge.traffic = Some(Traffic { calls: call.calls, errors: call.errors, mean_ms: call.mean_ms });
        }
    }

    EstateGraph {
        nodes: g.nodes.into_values().collect(),
        edges: g.edges.into_values().collect(),
        generated_at: Utc::now(),
    }
}

impl EstateGraph {
    /// The nodes `keep` admits and the links between them.
    fn only(&self, keep: impl Fn(&Node) -> bool) -> EstateGraph {
        let nodes: Vec<Node> = self.nodes.iter().filter(|n| keep(n)).cloned().collect();
        let ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        let edges = self.edges.iter().filter(|e| ids.contains(e.from.as_str()) && ids.contains(e.to.as_str())).cloned().collect();
        EstateGraph { nodes, edges, generated_at: self.generated_at }
    }

    /// Everything within `depth` links of `focus`, in either direction.
    pub fn around(&self, focus: &str, depth: usize) -> EstateGraph {
        let mut reached = BTreeSet::from([focus.to_string()]);
        for _ in 0..depth {
            let next: Vec<String> = self
                .edges
                .iter()
                .filter_map(|e| match (reached.contains(&e.from), reached.contains(&e.to)) {
                    (true, false) => Some(e.to.clone()),
                    (false, true) => Some(e.from.clone()),
                    _ => None,
                })
                .collect();
            reached.extend(next);
        }
        self.only(|n| reached.contains(&n.id))
    }

    /// The systems, who owns them and how they connect, without the
    /// containers, pods and files that can run to thousands.
    pub fn systems(&self) -> EstateGraph {
        self.only(|n| !matches!(n.kind, NodeKind::Component | NodeKind::File))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Row, Section};

    fn profile(value: serde_json::Value) -> ConnectionProfile {
        serde_json::from_value(value).unwrap()
    }

    fn estate() -> (Vec<ConnectionProfile>, HashMap<String, HealthSnapshot>) {
        let profiles = vec![
            profile(serde_json::json!({
                "id": "orders", "name": "orders-api", "kind": "http_endpoint", "endpoint": "http://orders", "enabled": true, "options": {},
                "dependsOn": ["payments", "ghost"],
                "metadata": { "operations": { "team": "payments", "environment": "production", "owner": "Priya" } },
                "logSources": [{ "id": "l1", "path": "/var/log/orders", "include": "*.log", "location": { "kind": "ssh", "serverProfileId": "web1" } }]
            })),
            profile(serde_json::json!({
                "id": "payments", "name": "payments-db", "kind": "postgres", "endpoint": "postgres://db", "enabled": true, "options": {},
                "runtime": { "kind": "container", "hostProfileId": "docker", "name": "payments-db-1" },
                "metadata": { "operations": { "team": "payments" } },
                "configSources": [{ "id": "c1", "path": "/etc/postgresql", "location": { "kind": "container", "hostProfileId": "docker", "container": "payments-db-1" } }]
            })),
            profile(serde_json::json!({ "id": "docker", "name": "docker-prod", "kind": "docker", "endpoint": "", "enabled": true, "options": {} })),
            profile(serde_json::json!({ "id": "web1", "name": "web-1", "kind": "ssh", "endpoint": "web-1", "username": "reader", "enabled": true, "options": {} })),
        ];
        let mut containers = Section::new("containers", "Containers", Vec::new());
        let mut row = Row::new("abc123", "payments-db-1", Health::Healthy).cell("kind", CellValue::text("PostgreSQL"));
        row.extra = serde_json::json!({ "watchedBy": "payments" });
        containers.rows = vec![row];
        let snapshot = HealthSnapshot {
            profile_id: "docker".into(),
            profile_name: "docker-prod".into(),
            kind: ServiceKind::Docker,
            health: Health::Healthy,
            summary: String::new(),
            metrics: Vec::new(),
            sections: vec![containers],
            checked_at: Utc::now(),
            latency_ms: 1,
            error: None,
        };
        (profiles, HashMap::from([("docker".to_string(), snapshot)]))
    }

    fn edge<'a>(graph: &'a EstateGraph, from: &str, to: &str, relation: Relation) -> Option<&'a Edge> {
        graph.edges.iter().find(|e| e.from == from && e.to == to && e.relation == relation)
    }

    #[test]
    fn every_connection_and_what_it_says_about_itself_becomes_the_graph() {
        let (profiles, snapshots) = estate();
        let graph = build(&profiles, &snapshots, &[], &[]);

        let declared = edge(&graph, "service:orders", "service:payments", Relation::DependsOn).unwrap();
        assert_eq!(declared.provenance, Provenance::Declared);
        assert!(graph.edges.iter().all(|e| !e.to.contains("ghost")), "a dependency on no connection is dropped");
        assert!(edge(&graph, "service:orders", "team:payments", Relation::OwnedBy).is_some());
        assert!(edge(&graph, "service:payments", "team:payments", Relation::OwnedBy).is_some(), "one team node, shared");
        assert_eq!(graph.nodes.iter().filter(|n| n.kind == NodeKind::Team).count(), 1);
        assert!(edge(&graph, "service:orders", "environment:production", Relation::InEnvironment).is_some());
        let orders = graph.nodes.iter().find(|n| n.id == "service:orders").unwrap();
        assert_eq!(orders.facts.get("owner").map(String::as_str), Some("Priya"));
        assert_eq!(orders.health, Some(Health::Unknown), "no snapshot yet");
    }

    #[test]
    fn a_runtime_declared_and_matched_by_root_is_one_link_known_both_ways() {
        let (profiles, snapshots) = estate();
        let graph = build(&profiles, &snapshots, &[], &[]);

        let component = "component:docker/containers/abc123";
        assert_eq!(edge(&graph, component, "service:docker", Relation::PartOf).unwrap().provenance, Provenance::Reported);
        let runs = edge(&graph, "service:payments", component, Relation::RunsOn).unwrap();
        assert_eq!(runs.provenance, Provenance::Declared, "the stronger evidence is kept");
        assert_eq!(runs.detail.as_deref(), Some("also inferred: matched by image and port"), "each detail stays with how it was known");
        let node = graph.nodes.iter().find(|n| n.id == component).unwrap();
        assert_eq!(node.service_kind, Some(ServiceKind::Postgres));
    }

    #[test]
    fn declared_files_lead_to_the_system_they_are_read_on() {
        let (profiles, snapshots) = estate();
        let graph = build(&profiles, &snapshots, &[], &[]);

        assert!(edge(&graph, "service:orders", "file:orders/l1", Relation::LogsTo).is_some());
        assert!(edge(&graph, "file:orders/l1", "service:web1", Relation::LocatedOn).is_some(), "logs read over SSH lead to that server");
        assert!(edge(&graph, "service:payments", "file:payments/c1", Relation::ConfiguredBy).is_some());
        assert!(
            edge(&graph, "file:payments/c1", "component:docker/containers/abc123", Relation::LocatedOn).is_some(),
            "config inside a container leads to that container"
        );
    }

    #[test]
    fn calls_seen_in_traces_reach_past_the_estate() {
        let (profiles, snapshots) = estate();
        let call = |from: &str, to: &str| ObservedEdge { from: from.into(), to: to.into(), calls: 120, errors: 3, mean_ms: 42.4 };
        let graph = build(&profiles, &snapshots, &[call("orders", "payments")], &[call("orders", "stripe-api")]);

        let inside = edge(&graph, "service:orders", "service:payments", Relation::Calls).unwrap();
        assert_eq!(inside.provenance, Provenance::Observed);
        assert_eq!(inside.traffic.map(|t| (t.calls, t.errors)), Some((120, 3)));
        assert!(edge(&graph, "service:orders", "external:stripe-api", Relation::Calls).is_some());
        assert_eq!(graph.nodes.iter().find(|n| n.id == "external:stripe-api").map(|n| n.kind), Some(NodeKind::External));
    }

    #[test]
    fn a_question_about_one_system_gets_its_neighbourhood_and_the_whole_gets_the_systems() {
        let (profiles, snapshots) = estate();
        let graph = build(&profiles, &snapshots, &[], &[]);

        let near = graph.around("service:payments", 1);
        let ids: BTreeSet<&str> = near.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains("service:orders"), "who depends on it, followed backwards");
        assert!(ids.contains("component:docker/containers/abc123") && ids.contains("file:payments/c1"));
        assert!(!ids.contains("service:docker"), "two links away, through its container");
        assert!(graph.around("service:payments", 2).nodes.iter().any(|n| n.id == "service:docker"));
        assert!(near.edges.iter().all(|e| ids.contains(e.from.as_str()) && ids.contains(e.to.as_str())));

        let systems = graph.systems();
        assert!(systems.nodes.iter().all(|n| !matches!(n.kind, NodeKind::Component | NodeKind::File)));
        assert!(systems.edges.iter().any(|e| e.relation == Relation::DependsOn));
    }
}
