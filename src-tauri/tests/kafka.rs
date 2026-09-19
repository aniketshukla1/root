//! Kafka collector against a real broker.
//!
//! Consumer lag is the part that cannot be unit tested: it depends on a
//! per-group offset fetch that only exists over the wire, and getting it wrong
//! is silent — a group that has never committed looks identical to one that is
//! fully caught up unless the Invalid offsets are filtered out.
//!
//! Skips cleanly when no broker is reachable. To set up the fixture:
//!
//! ```bash
//! docker run -d --name im-kafka -p 59092:9092 \
//!   -e KAFKA_NODE_ID=1 -e KAFKA_PROCESS_ROLES=broker,controller \
//!   -e KAFKA_LISTENERS='PLAINTEXT://:9092,INTERNAL://:9094,CONTROLLER://:9093' \
//!   -e KAFKA_ADVERTISED_LISTENERS='PLAINTEXT://127.0.0.1:59092,INTERNAL://localhost:9094' \
//!   -e KAFKA_INTER_BROKER_LISTENER_NAME=INTERNAL \
//!   -e KAFKA_CONTROLLER_LISTENER_NAMES=CONTROLLER \
//!   -e KAFKA_LISTENER_SECURITY_PROTOCOL_MAP='CONTROLLER:PLAINTEXT,PLAINTEXT:PLAINTEXT,INTERNAL:PLAINTEXT' \
//!   -e KAFKA_CONTROLLER_QUORUM_VOTERS='1@localhost:9093' \
//!   -e KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR=1 \
//!   -e KAFKA_TRANSACTION_STATE_LOG_REPLICATION_FACTOR=1 \
//!   -e KAFKA_TRANSACTION_STATE_LOG_MIN_ISR=1 \
//!   apache/kafka:3.9.0
//! ```
//!
//! The second listener exists because the in-container CLI cannot use the
//! host-facing advertised address.

use inframonitoring_lib::collectors::{build_collector, validate, Collector};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::model::{Health, HealthSnapshot, ServiceKind};

use chrono::{Duration, Utc};

const BOOTSTRAP: &str = "127.0.0.1:59092";

fn profile(endpoint: &str) -> ConnectionProfile {
    ConnectionProfile {
        id: "kafka-test".to_string(),
        name: "kafka".to_string(),
        kind: ServiceKind::Kafka,
        endpoint: endpoint.to_string(),
        username: None,
        has_secret: false,
        enabled: true,
        poll_interval_secs: 30,
        options: ProfileOptions::default(),
        depends_on: Vec::new(),
            config_sources: Vec::new(),
            history_source: None,
            snapshot_sink: None,
            runtime: Default::default(),
        metadata: ProfileOptions::default(),
    }
}

fn metric(snapshot: &HealthSnapshot, key: &str) -> f64 {
    snapshot
        .metrics
        .iter()
        .find(|m| m.key == key)
        .unwrap_or_else(|| panic!("missing metric `{key}`"))
        .value
}

async fn live() -> Option<(std::sync::Arc<dyn Collector>, HealthSnapshot)> {
    inframonitoring_lib::install_crypto_provider();
    let collector = build_collector(&profile(BOOTSTRAP), None).await.ok()?;
    let snapshot = collector.collect().await.ok()?;
    snapshot.error.is_none().then_some((collector, snapshot))
}

#[tokio::test]
async fn reports_brokers_topics_and_consumer_lag() {
    let Some((collector, snapshot)) = live().await else {
        eprintln!("skipping: no Kafka broker on {BOOTSTRAP}");
        return;
    };

    assert_eq!(snapshot.kind, ServiceKind::Kafka);
    validate::assert_renderable(&snapshot);

    assert!(metric(&snapshot, "brokers") >= 1.0, "one broker at minimum");
    assert_eq!(
        metric(&snapshot, "offline_partitions"),
        0.0,
        "a healthy single-node cluster has no offline partitions"
    );

    for id in ["groups", "topics", "brokers"] {
        assert!(
            snapshot.sections.iter().any(|s| s.id == id),
            "missing section `{id}`"
        );
    }

    // Internal topics must not be listed — __consumer_offsets exists on every
    // cluster that has ever had a consumer, and it is not the operator's topic.
    let topics = snapshot
        .sections
        .iter()
        .find(|s| s.id == "topics")
        .expect("topics section");
    assert!(
        topics.rows.iter().all(|r| !r.name.starts_with("__")),
        "internal topics should be filtered out: {:?}",
        topics.rows.iter().map(|r| &r.name).collect::<Vec<_>>()
    );

    let logs = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 10)
        .await
        .expect("fetch_logs");
    assert!(logs.len() <= 10);
}

/// The fixture leaves a `billing` group with committed offsets, real lag, and
/// nobody consuming. That combination is the one this collector treats as an
/// outage rather than a warning, so it is worth asserting end to end.
#[tokio::test]
async fn a_group_behind_with_no_members_is_reported_as_down() {
    let Some((_, snapshot)) = live().await else {
        eprintln!("skipping: no Kafka broker on {BOOTSTRAP}");
        return;
    };

    let groups = snapshot
        .sections
        .iter()
        .find(|s| s.id == "groups")
        .expect("groups section");

    let Some(billing) = groups.rows.iter().find(|r| r.name == "billing") else {
        eprintln!("skipping: the `billing` fixture group is not present");
        return;
    };

    assert_eq!(
        billing.health,
        Health::Down,
        "a group with lag and no members is a stopped pipeline: {}",
        billing.summary
    );
    assert!(
        billing.summary.contains("no members"),
        "the summary should say why: {}",
        billing.summary
    );

    // Lag must be a real number, not the whole topic. A group that committed
    // on one partition and never touched the others would report every
    // untouched partition as fully lagged if Invalid offsets were counted.
    assert!(
        metric(&snapshot, "lag") > 0.0,
        "the fixture group is genuinely behind"
    );
    assert!(
        metric(&snapshot, "lag") < 1000.0,
        "lag of {} suggests uncommitted partitions were counted as zero",
        metric(&snapshot, "lag")
    );

    assert_eq!(snapshot.health, Health::Down);
}

#[tokio::test]
async fn a_url_is_rejected_with_a_useful_message() {
    // bootstrap.servers is host:port pairs. librdkafka's own error for a URL
    // is opaque, so this is caught at build time instead.
    let result = build_collector(&profile("http://localhost:9092"), None).await;
    let error = result.err().expect("a URL must not build").to_string();
    assert!(
        error.contains("without a scheme"),
        "the error should explain the format: {error}"
    );
}

#[tokio::test]
async fn an_unreachable_broker_is_down_not_an_error() {
    inframonitoring_lib::install_crypto_provider();
    let collector = build_collector(&profile("127.0.0.1:1"), None)
        .await
        .expect("a well-formed address should build even if nothing answers");
    let snapshot = collector.collect().await.expect("collect must not error");
    assert!(
        snapshot.error.is_some(),
        "an unreachable broker should report an error on the snapshot"
    );
    assert_eq!(snapshot.health, Health::Down);
}
