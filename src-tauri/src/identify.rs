//! What a container is actually running.
//!
//! root's model is one connection, one system. That holds for a database on a
//! host and stops holding the moment everything is containerised: a Docker
//! host is twenty systems of a dozen kinds, and reporting them as twenty
//! containers is technically true and useless. An operator looking at that
//! list cannot see that three of them are databases and one is the cache
//! everything depends on.
//!
//! ## Reading, not guessing
//!
//! The estate's topology is declared rather than inferred, deliberately —
//! root watches connections, not the network between them, so it cannot know
//! what depends on what. This is a different thing. An image name is the
//! container's own statement about what it runs, and reading `postgres:16` and
//! concluding "Postgres" is not a guess in the way inferring a dependency
//! would be.
//!
//! It can still be wrong, so it is always reported with the fragment that
//! matched, and it never overrides anything an operator has said.
//!
//! ## The image is not enough
//!
//! Here is the case that makes the point, and it is sitting in root's own demo
//! estate. Two containers run `postgres:16-alpine`:
//!
//! ```text
//! root-demo-db          5432/tcp → published on 5442
//! root-demo-db-churner  5432/tcp → null
//! ```
//!
//! One is a database. The other uses the same image to run `psql` in a loop.
//! On the image alone they are identical, and calling the churner a Postgres
//! would put a phantom database in the inventory — the kind of confident error
//! that makes an operator stop trusting the column.
//!
//! An image says what software is *present*. Publishing the port that software
//! serves on says it is *serving*. So identification has two levels, and the
//! difference between them is reported rather than flattened.

use crate::model::ServiceKind;

/// A container's apparent identity, with the evidence for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub kind: ServiceKind,
    /// True when the container publishes the port this kind serves on.
    ///
    /// The difference between a database and a container that happens to have
    /// the database's client tools in it.
    pub serving: bool,
    /// The image fragment that matched, so a wrong answer can be seen to be
    /// wrong without reading this file.
    pub matched: &'static str,
}

/// Image fragments and what they mean, with the port that kind listens on.
///
/// Matched against the image's final path segment, so `bitnami/redis`,
/// `docker.io/library/redis` and `redis` all land on the same entry. Ordered
/// longest-first at match time, because `timescaledb` must win over a
/// hypothetical `db` and `cp-kafka` over `kafka`.
const IMAGES: &[(&str, ServiceKind, u16)] = &[
    // Databases. Postgres has the most forks of anything here, and every one
    // of them is a Postgres as far as root's collector is concerned.
    ("postgres", ServiceKind::Postgres, 5432),
    ("postgresql", ServiceKind::Postgres, 5432),
    ("timescaledb", ServiceKind::Postgres, 5432),
    ("pgvector", ServiceKind::Postgres, 5432),
    ("citus", ServiceKind::Postgres, 5432),
    ("mysql", ServiceKind::Mysql, 3306),
    ("mariadb", ServiceKind::Mysql, 3306),
    ("percona", ServiceKind::Mysql, 3306),
    ("mongo", ServiceKind::Mongodb, 27017),
    ("mongodb", ServiceKind::Mongodb, 27017),
    ("clickhouse", ServiceKind::Clickhouse, 8123),
    ("clickhouse-server", ServiceKind::Clickhouse, 8123),
    // Caching.
    ("redis", ServiceKind::Redis, 6379),
    ("valkey", ServiceKind::Redis, 6379),
    ("redis-stack", ServiceKind::Redis, 6379),
    ("memcached", ServiceKind::Memcached, 11211),
    // Proxies and web servers.
    ("nginx", ServiceKind::Nginx, 80),
    ("openresty", ServiceKind::Nginx, 80),
    ("httpd", ServiceKind::Apache, 80),
    ("haproxy", ServiceKind::Haproxy, 80),
    ("traefik", ServiceKind::Traefik, 80),
    ("tomcat", ServiceKind::Tomcat, 8080),
    // Messaging.
    ("rabbitmq", ServiceKind::Rabbitmq, 5672),
    ("kafka", ServiceKind::Kafka, 9092),
    ("cp-kafka", ServiceKind::Kafka, 9092),
    ("redpanda", ServiceKind::Kafka, 9092),
    ("nats", ServiceKind::Nats, 4222),
    // Coordination.
    ("zookeeper", ServiceKind::Zookeeper, 2181),
    ("cp-zookeeper", ServiceKind::Zookeeper, 2181),
    ("consul", ServiceKind::Consul, 8500),
    ("etcd", ServiceKind::Etcd, 2379),
    // Search and observability.
    ("elasticsearch", ServiceKind::Elasticsearch, 9200),
    ("opensearch", ServiceKind::Elasticsearch, 9200),
    ("prometheus", ServiceKind::Prometheus, 9090),
    ("jenkins", ServiceKind::Jenkins, 8080),
    // Cloud emulators, which serve everything on one port.
    ("localstack", ServiceKind::Aws, 4566),
    ("floci", ServiceKind::Aws, 4566),
];

/// Suffixes that mean a companion process rather than the thing itself.
///
/// `redis-exporter` scrapes a Redis and is not one; `postgres-backup` talks to
/// a database and is not one. Without this the inventory fills with phantom
/// servers, each of which an operator has to work out is not real.
/// Matched against the **image** name, not the container name. A container
/// called `db-churner` running `postgres:16-alpine` is not caught here and
/// does not need to be — the published-port check is what separates it from a
/// real database, and it is the stronger signal anyway.
const COMPANIONS: &[&str] = &[
    "-exporter",
    "_exporter",
    "-backup",
    "-cli",
    "-client",
    "-init",
    "-migrate",
    "-migration",
];

/// What an image says it runs, if anything recognisable.
///
/// `published` is the set of container-side ports the container publishes to
/// the host — the discriminator between running the software and merely
/// carrying it.
pub fn identify(image: &str, published: &[u16]) -> Option<Identity> {
    let name = base_name(image)?;

    if COMPANIONS.iter().any(|suffix| name.ends_with(suffix)) {
        return None;
    }

    // Longest match first, so `timescaledb` is not read as something shorter
    // it happens to contain and `cp-kafka` beats `kafka`.
    let mut candidates: Vec<&(&str, ServiceKind, u16)> = IMAGES
        .iter()
        .filter(|(fragment, _, _)| name == *fragment || name.starts_with(&format!("{fragment}-")))
        .collect();
    candidates.sort_by_key(|(fragment, _, _)| std::cmp::Reverse(fragment.len()));

    let (matched, kind, port) = candidates.first()?;
    Some(Identity {
        kind: *kind,
        serving: published.contains(port),
        matched,
    })
}

/// The image's final path segment, without registry, tag or digest.
///
/// `public.ecr.aws/docker/library/postgres:16-alpine` → `postgres`
fn base_name(image: &str) -> Option<String> {
    let image = image.trim();
    if image.is_empty() {
        return None;
    }

    // Digest first: a tag cannot follow one, and `@sha256:` contains a colon
    // that the tag split would otherwise take.
    let without_digest = image.split('@').next()?;
    let last_segment = without_digest.rsplit('/').next()?;
    // A tag, but not a port in a registry host — that only appears before a
    // slash, which this segment is already past.
    let without_tag = last_segment.split(':').next()?;

    let name = without_tag.trim().to_ascii_lowercase();
    (!name.is_empty()).then_some(name)
}

/// The connection watching this container properly, if one is configured.
///
/// Matched on published host port rather than on anything about the image: a
/// connection pointed at `localhost:5442` and a container publishing 5442 are
/// the same service, and that is a fact rather than a resemblance. It is also
/// the only signal that survives an image root does not recognise.
///
/// Only loopback and wildcard endpoints are considered. A connection to
/// `db.prod.internal:5432` and a local container publishing 5432 are not the
/// same thing, and saying they were would be worse than saying nothing.
pub fn monitored_by<'a>(
    host_ports: &[u16],
    connections: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<String> {
    for (id, endpoint) in connections {
        let Some((host, port)) = endpoint_host_port(endpoint) else {
            continue;
        };
        if !is_local(&host) {
            continue;
        }
        if host_ports.contains(&port) {
            return Some(id.to_string());
        }
    }
    None
}

/// Host and port out of the endpoint forms root's profiles actually use.
fn endpoint_host_port(endpoint: &str) -> Option<(String, u16)> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return None;
    }

    // Strip a scheme and any credentials, both of which contain characters the
    // naive split would trip over.
    let after_scheme = endpoint.rsplit("://").next()?;
    let authority = after_scheme.rsplit('@').next()?;
    let authority = authority.split(['/', '?']).next()?;

    // IPv6 literals are bracketed, so the last colon is the port separator
    // only outside the brackets.
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    Some((host.trim_matches(['[', ']']).to_string(), port))
}

fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "::")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------ the image

    #[test]
    fn common_images_are_recognised_whatever_registry_they_came_from() {
        for image in [
            "postgres:16-alpine",
            "postgres",
            "library/postgres:16",
            "docker.io/library/postgres:16-alpine",
            "public.ecr.aws/docker/library/postgres:16",
            "postgres@sha256:abc123",
        ] {
            let identity = identify(image, &[5432]).unwrap_or_else(|| panic!("{image}"));
            assert_eq!(identity.kind, ServiceKind::Postgres, "{image}");
        }
    }

    /// Every fork of Postgres is a Postgres as far as the collector is
    /// concerned, and an operator running Timescale does not want it filed
    /// under "unknown".
    #[test]
    fn forks_land_on_the_thing_they_are_a_fork_of() {
        for (image, kind) in [
            ("timescale/timescaledb:latest-pg14", ServiceKind::Postgres),
            ("pgvector/pgvector:pg16", ServiceKind::Postgres),
            ("mariadb:11", ServiceKind::Mysql),
            ("valkey/valkey:8", ServiceKind::Redis),
            ("redpanda-data/redpanda:latest", ServiceKind::Kafka),
            ("opensearchproject/opensearch:2", ServiceKind::Elasticsearch),
        ] {
            let identity = identify(image, &[]).unwrap_or_else(|| panic!("{image}"));
            assert_eq!(identity.kind, kind, "{image}");
        }
    }

    /// A companion process is not the thing it talks to. Without this the
    /// inventory fills with phantom servers.
    #[test]
    fn companions_are_not_mistaken_for_the_service_they_serve() {
        for image in [
            "oliver006/redis-exporter:latest",
            "prometheuscommunity/postgres-exporter",
            "mycompany/postgres-backup:1.0",
            "redis-cli",
            "kafka-init",
        ] {
            assert_eq!(identify(image, &[]), None, "{image} is not a server");
        }
    }

    #[test]
    fn an_unrecognised_image_says_nothing_rather_than_guessing() {
        for image in ["alpine:3", "mycompany/orders-api:2.14.3", "busybox", ""] {
            assert_eq!(identify(image, &[]), None, "{image}");
        }
    }

    /// The case from root's own demo estate: two containers, one image, and
    /// only one of them is a database.
    #[test]
    fn publishing_the_port_is_what_separates_serving_from_merely_carrying() {
        let serving = identify("postgres:16-alpine", &[5432]).unwrap();
        let carrying = identify("postgres:16-alpine", &[]).unwrap();

        assert!(serving.serving, "publishes 5432, so it is a database");
        assert!(
            !carrying.serving,
            "same image, publishes nothing — this is the churner running psql"
        );
        // Both are still identified: the image is real information even when
        // the container is not serving.
        assert_eq!(serving.kind, carrying.kind);
    }

    #[test]
    fn the_matched_fragment_travels_so_a_wrong_answer_is_visible() {
        let identity = identify("timescale/timescaledb:latest-pg14", &[]).unwrap();
        assert_eq!(identity.matched, "timescaledb");
    }

    /// `cp-kafka` must not be read as `kafka` with a stray prefix, and
    /// `timescaledb` must beat anything shorter it contains.
    #[test]
    fn the_longest_matching_fragment_wins() {
        assert_eq!(
            identify("confluentinc/cp-kafka:7.5.0", &[]).unwrap().matched,
            "cp-kafka"
        );
        assert_eq!(
            identify("timescale/timescaledb-ha:pg16", &[]).unwrap().matched,
            "timescaledb"
        );
    }

    // ------------------------------------------------------------- the link

    #[test]
    fn a_container_is_linked_to_the_connection_pointed_at_its_published_port() {
        let connections = vec![
            ("demo-cache", "redis://localhost:6399"),
            ("demo-db", "postgres://postgres@localhost:5442/orders"),
        ];
        assert_eq!(
            monitored_by(&[5442], connections.clone()),
            Some("demo-db".to_string())
        );
        assert_eq!(
            monitored_by(&[6399], connections.clone()),
            Some("demo-cache".to_string())
        );
        assert_eq!(monitored_by(&[9999], connections), None);
    }

    /// A remote database and a local container on the same port number are not
    /// the same service, and claiming they were would be worse than silence.
    #[test]
    fn a_remote_endpoint_is_never_matched_to_a_local_container() {
        let connections = vec![("prod-db", "postgres://user@db.prod.internal:5432/orders")];
        assert_eq!(monitored_by(&[5432], connections), None);
    }

    #[test]
    fn endpoints_parse_in_the_shapes_profiles_actually_use() {
        for (endpoint, expected) in [
            ("redis://localhost:6399", Some(("localhost".into(), 6399))),
            (
                "postgres://postgres:pw@127.0.0.1:5442/orders",
                Some(("127.0.0.1".into(), 5442)),
            ),
            ("http://localhost:8088/stub_status", Some(("localhost".into(), 8088))),
            ("localhost:9090", Some(("localhost".into(), 9090))),
            ("http://[::1]:8080/x", Some(("::1".into(), 8080))),
            // No port to match on.
            ("https://example.com/health", None),
            ("", None),
        ] {
            assert_eq!(endpoint_host_port(endpoint), expected, "{endpoint}");
        }
    }

    #[test]
    fn a_connection_with_no_port_is_skipped_rather_than_matched_loosely() {
        let connections = vec![("api", "https://example.com/health"), ("db", "redis://localhost:6399")];
        assert_eq!(monitored_by(&[6399], connections), Some("db".to_string()));
    }
}
