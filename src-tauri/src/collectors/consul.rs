//! Consul monitor, via the HTTP API.
//!
//! Three reads: `/v1/status/leader` for whether the cluster can accept writes
//! at all, `/v1/status/peers` for the size of the raft set, and
//! `/v1/health/state/any` for every check Consul knows about.
//!
//! The check list needs care, because Consul reports one failure many times.
//!
//! Every agent runs a built-in `serfHealth` check. When an agent dies — the
//! process, the host, the network path — Consul fails `serfHealth` for that
//! node and marks *every service registered on it* critical too. A host with
//! twenty services produces twenty-one critical checks from one event. Read
//! naively that is twenty service outages; it is one dead agent, and the
//! services on it may be running perfectly.
//!
//! So node-level and service-level failures are counted apart, and a critical
//! service check on a node whose agent is already down is attributed to the
//! agent rather than counted again. It is the same discipline the incident
//! correlator applies to alerts, at the level of one connection.
//!
//! And a cluster with no leader is not degraded, it is down: reads may still
//! be served from a stale follower, but nothing can be registered,
//! deregistered, or written to the KV store until an election completes.

use std::collections::BTreeSet;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// The check every Consul agent runs on itself. Its failure means the agent is
/// gone, not that a service is unhealthy.
const SERF_HEALTH: &str = "serfHealth";

pub struct ConsulCollector {
    client: reqwest::Client,
    base: String,
    token: Option<String>,
    profile_id: String,
    profile_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Check {
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub check_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub service_name: String,
    #[serde(default)]
    pub output: String,
}

impl Check {
    pub fn health(&self) -> Health {
        match self.status.as_str() {
            "passing" => Health::Healthy,
            "warning" => Health::Degraded,
            "critical" => Health::Down,
            // Consul has only these three, so anything else is a version this
            // build does not know rather than something to assume about.
            _ => Health::Unknown,
        }
    }

    /// The agent's own check, as opposed to one about a registered service.
    pub fn is_node_check(&self) -> bool {
        self.check_id == SERF_HEALTH || self.service_name.is_empty()
    }
}

/// Split failing checks into the agents that are gone and the services that
/// are genuinely failing on agents that are still alive.
///
/// A service check on a node whose `serfHealth` is already critical is not
/// counted: Consul marks it critical as a consequence of the agent being
/// unreachable, and counting it again would report one dead host as twenty
/// broken services.
pub(crate) fn attribute(checks: &[Check]) -> (BTreeSet<String>, Vec<&Check>) {
    let dead_nodes: BTreeSet<String> = checks
        .iter()
        .filter(|c| c.check_id == SERF_HEALTH && c.health() == Health::Down)
        .map(|c| c.node.clone())
        .collect();

    let failing_services: Vec<&Check> = checks
        .iter()
        .filter(|c| !c.is_node_check())
        .filter(|c| matches!(c.health(), Health::Down | Health::Degraded))
        .filter(|c| !dead_nodes.contains(&c.node))
        .collect();

    (dead_nodes, failing_services)
}

impl ConsulCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim().trim_end_matches('/');
        if raw.is_empty() {
            return Err(AppError::Config(
                "Consul connections need the HTTP API address, e.g. http://consul.internal:8500"
                    .into(),
            ));
        }
        let base = raw.trim_end_matches("/v1").to_string();

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        Ok(ConsulCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            base,
            token: secret,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> std::result::Result<T, String> {
        let url = format!("{}{path}", self.base);
        let mut request = self.client.get(&url);
        if let Some(token) = &self.token {
            // Consul's own header. A bearer token would be ignored.
            request = request.header("X-Consul-Token", token);
        }
        let response = request.send().await.map_err(|e| format!("{url}: {e}"))?;
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(format!(
                "{url} returned 403 — the ACL token needs `node:read` and `service:read`"
            ));
        }
        if !response.status().is_success() {
            return Err(format!("{url} returned {}", response.status()));
        }
        response
            .json::<T>()
            .await
            .map_err(|e| format!("{url} sent a body root could not read: {e}"))
    }
}

#[async_trait]
impl Collector for ConsulCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Consul
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        // The leader endpoint returns a bare JSON string, and an empty one is
        // the answer — not a failure to parse.
        let leader: String = match self.get("/v1/status/leader").await {
            Ok(leader) => leader,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Consul,
                    message,
                    elapsed_ms(start),
                ));
            }
        };

        let peers: Vec<String> = self.get("/v1/status/peers").await.unwrap_or_default();
        let checks: Vec<Check> = self.get("/v1/health/state/any").await.unwrap_or_default();
        let latency = elapsed_ms(start);

        let (dead_nodes, failing_services) = attribute(&checks);
        let leaderless = leader.trim().is_empty();

        let critical_services = failing_services
            .iter()
            .filter(|c| c.health() == Health::Down)
            .count();

        let health = if leaderless || !dead_nodes.is_empty() {
            Health::Down
        } else if !failing_services.is_empty() {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if leaderless {
            // Worth spelling out: reads keep working, which is exactly why
            // this is easy to miss until a registration fails.
            "no raft leader — reads may still answer, but nothing can be written".to_string()
        } else if !dead_nodes.is_empty() {
            format!(
                "{} unreachable: {}",
                if dead_nodes.len() == 1 { "agent" } else { "agents" },
                dead_nodes.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        } else if critical_services > 0 {
            format!(
                "{critical_services} service check{} failing",
                if critical_services == 1 { "" } else { "s" }
            )
        } else if !failing_services.is_empty() {
            format!("{} service checks warning", failing_services.len())
        } else {
            format!(
                "leader {leader} · {} peers · {} checks passing",
                peers.len(),
                checks.len()
            )
        };

        let metrics = vec![
            Metric::new("peers", "Raft peers", peers.len() as f64, MetricUnit::Count),
            Metric::new(
                "agents_down",
                "Agents unreachable",
                dead_nodes.len() as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new(
                "services_failing",
                "Service checks failing",
                failing_services.len() as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new("checks", "Checks total", checks.len() as f64, MetricUnit::Count),
        ];

        let mut sections = Vec::new();

        if !dead_nodes.is_empty() {
            sections.push(
                Section::new(
                    "agents",
                    "Unreachable agents",
                    vec![
                        ColumnSpec::text("node", "Node").primary(),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::metric("services", "Services on it", MetricUnit::Count),
                    ],
                )
                .rows(
                    dead_nodes
                        .iter()
                        .map(|node| {
                            let count = checks
                                .iter()
                                .filter(|c| &c.node == node && !c.is_node_check())
                                .count();
                            Row::new(format!("node:{node}"), node.clone(), Health::Down)
                                .summary(format!(
                                    "serfHealth failing — the {count} service check{} on this \
                                     node are a consequence, not separate failures",
                                    if count == 1 { "" } else { "s" }
                                ))
                                .cell("node", CellValue::text(node.clone()))
                                .cell("state", CellValue::status(Health::Down))
                                .cell("services", CellValue::number(count as f64))
                        })
                        .collect(),
                )
                .priority(5),
            );
        }

        sections.push(
            Section::new(
                "checks",
                "Failing checks",
                vec![
                    ColumnSpec::text("check", "Check").primary(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::text("service", "Service"),
                    ColumnSpec::text("node", "Node"),
                    ColumnSpec::text("output", "Output"),
                ],
            )
            .rows(
                failing_services
                    .iter()
                    .map(|check| {
                        Row::new(
                            format!("check:{}:{}", check.node, check.check_id),
                            check.name.clone(),
                            check.health(),
                        )
                        .summary(check.output.lines().next().unwrap_or_default())
                        .cell("check", CellValue::text(check.name.clone()))
                        .cell("state", CellValue::status(check.health()))
                        .cell("service", CellValue::text(check.service_name.clone()))
                        .cell("node", CellValue::text(check.node.clone()))
                        .cell(
                            "output",
                            CellValue::text(
                                check.output.lines().next().unwrap_or_default().to_string(),
                            ),
                        )
                    })
                    .collect(),
            )
            .empty_text("Every service check is passing.")
            .priority(10),
        );

        sections.push(
            Section::new(
                "raft",
                "Raft",
                vec![
                    ColumnSpec::text("peer", "Peer").primary(),
                    ColumnSpec::text("role", "Role"),
                ],
            )
            .rows(
                peers
                    .iter()
                    .map(|peer| {
                        let is_leader = peer == leader.trim();
                        Row::new(
                            format!("peer:{peer}"),
                            peer.clone(),
                            if leaderless { Health::Down } else { Health::Healthy },
                        )
                        .cell("peer", CellValue::text(peer.clone()))
                        .cell(
                            "role",
                            CellValue::text(if is_leader { "leader" } else { "follower" }),
                        )
                    })
                    .collect(),
            )
            .empty_text("Consul reported no raft peers.")
            .priority(20),
        );

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Consul,
            health,
            summary,
            metrics,
            sections,
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// Consul's own log is on the agent's host. The API serves state, not a
    /// journal.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(node: &str, check_id: &str, service: &str, status: &str) -> Check {
        Check {
            node: node.into(),
            check_id: check_id.into(),
            name: check_id.into(),
            status: status.into(),
            service_name: service.into(),
            output: String::new(),
        }
    }

    #[test]
    fn parses_the_check_payload() {
        let checks: Vec<Check> = serde_json::from_str(
            r#"[{"Node":"web-1","CheckID":"service:api","Name":"api health",
                 "Status":"critical","ServiceName":"api","Output":"connection refused"}]"#,
        )
        .expect("checks");
        assert_eq!(checks[0].node, "web-1");
        assert_eq!(checks[0].health(), Health::Down);
        assert!(!checks[0].is_node_check());
    }

    /// The whole point of the module. One agent dying produces twenty-one
    /// critical checks, and only one thing is wrong.
    #[test]
    fn a_dead_agent_is_one_failure_not_twenty() {
        let mut checks = vec![check("web-1", SERF_HEALTH, "", "critical")];
        for i in 0..20 {
            checks.push(check("web-1", &format!("service:svc-{i}"), &format!("svc-{i}"), "critical"));
        }

        let (dead, failing) = attribute(&checks);
        assert_eq!(dead.len(), 1);
        assert!(dead.contains("web-1"));
        assert!(
            failing.is_empty(),
            "services on an unreachable agent are a consequence, not separate failures",
        );
    }

    /// And a service genuinely failing on a live agent is still counted.
    #[test]
    fn a_failing_service_on_a_live_agent_is_counted() {
        let checks = vec![
            check("web-1", SERF_HEALTH, "", "passing"),
            check("web-1", "service:api", "api", "critical"),
        ];
        let (dead, failing) = attribute(&checks);
        assert!(dead.is_empty());
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].service_name, "api");
    }

    /// A dead agent on one node must not excuse a real failure on another.
    #[test]
    fn a_dead_agent_does_not_hide_failures_elsewhere() {
        let checks = vec![
            check("web-1", SERF_HEALTH, "", "critical"),
            check("web-1", "service:api", "api", "critical"),
            check("web-2", SERF_HEALTH, "", "passing"),
            check("web-2", "service:api", "api", "critical"),
        ];
        let (dead, failing) = attribute(&checks);
        assert_eq!(dead.len(), 1);
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].node, "web-2");
    }

    #[test]
    fn warnings_are_counted_as_failing_but_not_as_critical() {
        let checks = vec![check("web-1", "service:api", "api", "warning")];
        let (_, failing) = attribute(&checks);
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].health(), Health::Degraded);
    }

    /// A node-level check that is not serfHealth still belongs to the node.
    #[test]
    fn a_check_with_no_service_is_a_node_check() {
        assert!(check("web-1", "disk-space", "", "critical").is_node_check());
    }

    #[test]
    fn an_unrecognised_status_is_unknown_rather_than_assumed_passing() {
        assert_eq!(check("web-1", "c", "s", "maintenance").health(), Health::Unknown);
    }

    /// An empty leader string is the answer, not a parse failure.
    #[test]
    fn an_empty_leader_is_a_leaderless_cluster() {
        let leader: String = serde_json::from_str(r#""""#).expect("leader");
        assert!(leader.trim().is_empty());
    }
}
