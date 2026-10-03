//! The simulated company: a small online shop and the platform under it.
//!
//! Thirteen connections across eleven collector kinds, wired with declared
//! dependencies the way a real estate would be, so incident grouping, blast
//! radius and the propagation chain all have a graph to work over. Every
//! connection is an ordinary profile of an ordinary kind — the screens, the
//! rules and the incident engine cannot tell it from a real one, which is the
//! point: the demo shows what root does, not a mock-up of it.

use crate::config::{ConnectionProfile, ProfileOptions};
use crate::model::ServiceKind;

/// One connection in the demo estate.
pub struct Member {
    /// Stable short name. The profile id is `sim-<key>`.
    pub key: &'static str,
    pub name: &'static str,
    pub kind: ServiceKind,
    /// What the connection form would hold. Never dialled.
    pub endpoint: &'static str,
    pub depends_on: &'static [&'static str],
    pub team: &'static str,
    pub criticality: &'static str,
    pub poll_secs: u64,
    /// Collector options that change how the real collector would read it.
    pub options: &'static [(&'static str, &'static str)],
}

pub const ESTATE: &[Member] = &[
    Member {
        key: "shop",
        name: "shop.acme.dev",
        kind: ServiceKind::HttpEndpoint,
        endpoint: "https://shop.acme.dev/",
        depends_on: &["edge-lb"],
        team: "storefront",
        criticality: "tier1",
        poll_secs: 10,
        options: &[("slowMillis", "1000")],
    },
    Member {
        key: "checkout",
        name: "checkout-api",
        kind: ServiceKind::HttpEndpoint,
        endpoint: "https://api.acme.dev/checkout/healthz",
        depends_on: &["prod-docker", "orders-db", "session-cache"],
        team: "payments",
        criticality: "tier1",
        poll_secs: 10,
        options: &[("slowMillis", "1000")],
    },
    Member {
        key: "edge-lb",
        name: "edge-lb",
        kind: ServiceKind::Haproxy,
        endpoint: "http://edge-lb.acme.internal:8404/stats;csv",
        depends_on: &["prod-docker"],
        team: "platform",
        criticality: "tier1",
        poll_secs: 10,
        options: &[],
    },
    Member {
        key: "prod-docker",
        name: "prod-docker",
        kind: ServiceKind::Docker,
        endpoint: "tcp://app-1.acme.internal:2376",
        depends_on: &["orders-db", "session-cache", "events-kafka", "jobs-rabbit", "search-es"],
        team: "platform",
        criticality: "tier1",
        poll_secs: 10,
        options: &[],
    },
    Member {
        key: "orders-db",
        name: "orders-db",
        kind: ServiceKind::Postgres,
        endpoint: "postgres://orders-db.acme.internal:5432/orders",
        depends_on: &[],
        team: "payments",
        criticality: "tier1",
        poll_secs: 10,
        options: &[],
    },
    Member {
        key: "session-cache",
        name: "session-cache",
        kind: ServiceKind::Redis,
        endpoint: "redis://session-cache.acme.internal:6379",
        depends_on: &[],
        team: "storefront",
        criticality: "tier2",
        poll_secs: 10,
        options: &[],
    },
    Member {
        key: "events-kafka",
        name: "events-kafka",
        kind: ServiceKind::Kafka,
        endpoint: "kafka-1.acme.internal:9092,kafka-2.acme.internal:9092,kafka-3.acme.internal:9092",
        depends_on: &[],
        team: "data",
        criticality: "tier1",
        poll_secs: 10,
        options: &[],
    },
    Member {
        key: "jobs-rabbit",
        name: "jobs-rabbit",
        kind: ServiceKind::Rabbitmq,
        endpoint: "http://mq.acme.internal:15672",
        depends_on: &[],
        team: "platform",
        criticality: "tier2",
        poll_secs: 15,
        options: &[],
    },
    Member {
        key: "search-es",
        name: "search-es",
        kind: ServiceKind::Elasticsearch,
        endpoint: "https://search.acme.internal:9200",
        depends_on: &[],
        team: "storefront",
        criticality: "tier2",
        poll_secs: 10,
        options: &[],
    },
    Member {
        key: "data-k8s",
        name: "data-k8s",
        kind: ServiceKind::Kubernetes,
        endpoint: "arn:aws:eks:eu-west-1:000000000000:cluster/data",
        depends_on: &["events-kafka"],
        team: "data",
        criticality: "tier2",
        poll_secs: 10,
        options: &[],
    },
    Member {
        key: "ci",
        name: "ci.acme.dev",
        kind: ServiceKind::Jenkins,
        endpoint: "https://ci.acme.dev",
        depends_on: &[],
        team: "platform",
        criticality: "tier3",
        poll_secs: 10,
        options: &[],
    },
    Member {
        key: "batch-1",
        name: "batch-1",
        kind: ServiceKind::LinuxHost,
        endpoint: "http://batch-1.acme.internal:9100",
        depends_on: &["orders-db"],
        team: "data",
        criticality: "tier3",
        poll_secs: 15,
        options: &[],
    },
    Member {
        key: "telemetry",
        name: "telemetry",
        kind: ServiceKind::Gigapipe,
        endpoint: "http://telemetry.acme.internal:3100",
        depends_on: &[],
        team: "platform",
        criticality: "tier2",
        poll_secs: 15,
        options: &[
            ("traceQuery", "{ resource.service.name =~ \".+\" }"),
            (
                "profileQuery",
                "process_cpu:cpu:nanoseconds:cpu:nanoseconds{service_name=\"checkout-api\"}",
            ),
        ],
    },
];

/// Profile ids carry a prefix so a simulated connection can never collide
/// with a real one, and so one can be recognised at a glance in a log line.
pub const ID_PREFIX: &str = "sim-";

pub fn profile_id(key: &str) -> String {
    format!("{ID_PREFIX}{key}")
}

pub fn member(profile_id: &str) -> Option<&'static Member> {
    let key = profile_id.strip_prefix(ID_PREFIX)?;
    ESTATE.iter().find(|m| m.key == key)
}

/// The estate as connection profiles, ready to add to the config.
pub fn profiles() -> Vec<ConnectionProfile> {
    ESTATE
        .iter()
        .map(|m| {
            let mut options = ProfileOptions::default();
            for (key, value) in m.options {
                options.set(key, serde_json::Value::String((*value).to_string()));
            }
            let mut metadata = ProfileOptions::default();
            metadata.set(
                "operations",
                serde_json::json!({
                    "team": m.team,
                    "owner": format!("{} on-call", m.team),
                    "contact": format!("#{}-alerts", m.team),
                    "environment": "production",
                    "criticality": m.criticality,
                    "runbookUrl": format!("https://runbooks.acme.dev/{}", m.key),
                }),
            );
            ConnectionProfile {
                id: profile_id(m.key),
                name: m.name.to_string(),
                kind: m.kind,
                endpoint: m.endpoint.to_string(),
                username: None,
                has_secret: false,
                enabled: true,
                poll_interval_secs: m.poll_secs,
                options,
                depends_on: m.depends_on.iter().map(|k| profile_id(k)).collect(),
                metadata,
                config_sources: Vec::new(),
                history_source: None,
                runtime: Default::default(),
                snapshot_sink: None,
                simulated: true,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dependency_names_a_member() {
        for m in ESTATE {
            for dep in m.depends_on {
                assert!(
                    ESTATE.iter().any(|other| other.key == *dep),
                    "{} depends on unknown {dep}",
                    m.key
                );
            }
        }
    }

    #[test]
    fn profiles_round_trip_to_members() {
        for profile in profiles() {
            assert!(profile.simulated);
            let m = member(&profile.id).expect("member");
            assert_eq!(m.name, profile.name);
        }
    }
}
