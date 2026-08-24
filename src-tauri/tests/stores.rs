//! Redis and MongoDB collectors against real servers.
//!
//! Both parse text or BSON whose shape varies by server version, and both
//! degrade rather than error when something is missing — which means a wrong
//! field name shows up as a permanently-zero metric, not a failure. So these
//! run against real servers and assert the values actually arrived.
//!
//! Skips cleanly when nothing is reachable. To run them:
//!
//! ```bash
//! docker run -d --name im-redis -p 56379:6379 redis:7-alpine
//! docker run -d --name im-mongo -p 57017:27017 mongo:7
//! ```

use inframonitoring_lib::collectors::{build_collector, validate, Collector};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::model::{Health, HealthSnapshot, ServiceKind};

use chrono::{Duration, Utc};

const REDIS_URL: &str = "redis://127.0.0.1:56379";
const MONGO_URL: &str = "mongodb://127.0.0.1:57017";

fn profile(kind: ServiceKind, endpoint: &str) -> ConnectionProfile {
    ConnectionProfile {
        id: "store-test".to_string(),
        name: "store".to_string(),
        kind,
        endpoint: endpoint.to_string(),
        username: None,
        has_secret: false,
        enabled: true,
        poll_interval_secs: 15,
        options: {
            let mut options = ProfileOptions::default();
            // Everything currently running counts as "long", so the
            // long-operation path is exercised rather than skipped.
            options.set("slowOpMillis", serde_json::json!(1));
            options
        },
        metadata: ProfileOptions::default(),
    }
}

fn metric(snapshot: &HealthSnapshot, key: &str) -> f64 {
    snapshot
        .metrics
        .iter()
        .find(|m| m.key == key)
        .unwrap_or_else(|| {
            panic!(
                "missing metric `{key}`; present: {:?}",
                snapshot.metrics.iter().map(|m| &m.key).collect::<Vec<_>>()
            )
        })
        .value
}

/// Build and collect, or `None` when the server is not reachable.
async fn snapshot(kind: ServiceKind, url: &str) -> Option<(std::sync::Arc<dyn Collector>, HealthSnapshot)> {
    inframonitoring_lib::install_crypto_provider();
    let collector = build_collector(&profile(kind, url), None).await.ok()?;
    let snapshot = collector.collect().await.ok()?;
    // Unreachable is a Down snapshot carrying an error, not an Err.
    snapshot.error.is_none().then_some((collector, snapshot))
}

#[tokio::test]
async fn redis_reports_real_values() {
    let Some((collector, snapshot)) = snapshot(ServiceKind::Redis, REDIS_URL).await else {
        eprintln!("skipping: no Redis on {REDIS_URL}");
        return;
    };

    assert_eq!(snapshot.kind, ServiceKind::Redis);
    validate::assert_renderable(&snapshot);

    // An idle local Redis is healthy — if this reports otherwise, a threshold
    // is miscalibrated.
    assert_eq!(
        snapshot.health,
        Health::Healthy,
        "idle Redis should be healthy: {}",
        snapshot.summary
    );

    // These come straight from INFO. A renamed key would silently read zero,
    // which is exactly what this catches.
    assert!(
        metric(&snapshot, "used_memory") > 0.0,
        "used_memory should be non-zero on a live server"
    );
    assert!(
        metric(&snapshot, "clients") >= 1.0,
        "we are ourselves a connected client"
    );
    // Fragmentation defaults to 1.0 when absent; a real server reports > 0.
    assert!(metric(&snapshot, "fragmentation") > 0.0);

    // The server section must have a row with a real role.
    let server = snapshot
        .sections
        .iter()
        .find(|s| s.id == "server")
        .expect("server section");
    assert_eq!(server.rows.len(), 1);
    assert!(
        !server.rows[0].summary.is_empty(),
        "server row needs a summary"
    );

    for id in ["server", "memory", "slowlog", "keyspace"] {
        assert!(
            snapshot.sections.iter().any(|s| s.id == id),
            "missing section `{id}`"
        );
    }

    // Log fetching must not error and must respect the limit.
    let logs = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 10)
        .await
        .expect("fetch_logs");
    assert!(logs.len() <= 10);
}

#[tokio::test]
async fn redis_rejects_a_bad_url_at_build_time() {
    let result = build_collector(&profile(ServiceKind::Redis, "not a url"), None).await;
    assert!(result.is_err(), "a malformed URL must not build");
}

#[tokio::test]
async fn mongodb_reports_real_values() {
    let Some((collector, snapshot)) = snapshot(ServiceKind::Mongodb, MONGO_URL).await else {
        eprintln!("skipping: no MongoDB on {MONGO_URL}");
        return;
    };

    assert_eq!(snapshot.kind, ServiceKind::Mongodb);
    validate::assert_renderable(&snapshot);

    assert!(
        metric(&snapshot, "connections") >= 1.0,
        "we are ourselves a connection"
    );
    assert!(
        metric(&snapshot, "uptime") > 0.0,
        "a running server has uptime"
    );
    assert!(
        metric(&snapshot, "resident") > 0.0,
        "resident memory should be non-zero"
    );

    // A standalone server answers serverStatus but errors on
    // replSetGetStatus. That must produce an empty section, not a failure —
    // the single most likely way this collector could wrongly report an
    // outage.
    let replication = snapshot
        .sections
        .iter()
        .find(|s| s.id == "replication")
        .expect("replication section");
    assert!(
        replication.rows.is_empty(),
        "a standalone server has no replica members"
    );
    assert!(
        snapshot.error.is_none(),
        "a standalone must not be reported as failing"
    );

    // listDatabases should find the built-in databases.
    let databases = snapshot
        .sections
        .iter()
        .find(|s| s.id == "databases")
        .expect("databases section");
    assert!(
        !databases.rows.is_empty(),
        "admin/config/local should be listed"
    );

    let logs = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 10)
        .await
        .expect("fetch_logs");
    assert!(logs.len() <= 10);
}

#[tokio::test]
async fn mongodb_rejects_a_bad_connection_string() {
    let result = build_collector(&profile(ServiceKind::Mongodb, "http://wrong-scheme"), None).await;
    assert!(result.is_err(), "a non-mongodb URL must not build");
}
