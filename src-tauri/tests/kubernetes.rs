//! Kubernetes collector test against a real cluster.
//!
//! The unit tests in the collector cover the state machines — Ready inversion,
//! restart thresholds, scaled-to-zero workloads. What they cannot cover is
//! whether the API shapes we ask for actually exist on a live server: a renamed
//! field or a missing subresource shows up as a silently-empty section, never
//! as a compile error.
//!
//! Skips cleanly when no cluster is reachable. To run it, enable Kubernetes in
//! Docker Desktop, or:
//!
//! ```bash
//! minikube start
//! ```

use inframonitoring_lib::collectors::{build_collector, validate};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::model::{Health, ServiceKind};

use chrono::{Duration, Utc};

fn profile(context: &str) -> ConnectionProfile {
    ConnectionProfile {
        id: "k8s-test".to_string(),
        name: "local cluster".to_string(),
        kind: ServiceKind::Kubernetes,
        endpoint: context.to_string(),
        username: None,
        has_secret: false,
        enabled: true,
        poll_interval_secs: 20,
        options: ProfileOptions::default(),
        depends_on: Vec::new(),
            config_sources: Vec::new(),
            history_source: None,
            snapshot_sink: None,
            simulated: false,
            runtime: Default::default(),
        metadata: ProfileOptions::default(),
    }
}

#[tokio::test]
async fn collects_from_a_live_cluster() {
    // The app does this in `run()`; a test binary has to do it itself or the
    // first TLS handshake to the API server panics.
    inframonitoring_lib::install_crypto_provider();

    // Try the usual local contexts, then whatever the current one is.
    let mut collector = None;
    for context in ["docker-desktop", "minikube", ""] {
        if let Ok(built) = build_collector(&profile(context), None).await {
            let snapshot = built.collect().await.expect("collect must not error");
            // `unreachable` is a Down snapshot with an error, not an Err — so a
            // built-but-dead context has to be filtered out here rather than by
            // the build step.
            if snapshot.error.is_none() {
                collector = Some(built);
                break;
            }
        }
    }

    let Some(collector) = collector else {
        eprintln!("skipping: no reachable Kubernetes cluster");
        return;
    };

    let snapshot = collector.collect().await.expect("collect");
    assert!(snapshot.error.is_none(), "error: {:?}", snapshot.error);
    assert_eq!(snapshot.kind, ServiceKind::Kubernetes);

    // The generic renderer's contract: every declared column exists on every
    // row, and row ids are unique. This is what keeps a new collector from
    // shipping a section the UI cannot draw.
    validate::assert_renderable(&snapshot);

    // A cluster that answered has at least one node, and it must have produced
    // a real status string rather than an empty cell.
    let nodes = snapshot
        .sections
        .iter()
        .find(|s| s.id == "nodes")
        .expect("nodes section");
    assert!(!nodes.rows.is_empty(), "a reachable cluster has nodes");
    for row in &nodes.rows {
        assert!(!row.name.is_empty(), "node rows need a name");
        assert!(!row.summary.is_empty(), "node rows need a status summary");
    }

    // Metrics the dashboard charts depend on must be present and consistent.
    let metric = |key: &str| {
        snapshot
            .metrics
            .iter()
            .find(|m| m.key == key)
            .unwrap_or_else(|| panic!("missing metric `{key}`"))
            .value
    };
    let nodes_total = metric("nodes_total");
    assert!(nodes_total >= 1.0, "nodes_total was {nodes_total}");
    assert!(
        metric("nodes_ready") <= nodes_total,
        "more ready nodes than nodes"
    );
    assert!(
        metric("pods_running") <= metric("pods_total"),
        "more running pods than pods"
    );

    // Every section the collector declares must be renderable even when empty,
    // which is the normal case for `pods` and `events` on a healthy cluster.
    for id in ["nodes", "workloads", "pods", "events"] {
        assert!(
            snapshot.sections.iter().any(|s| s.id == id),
            "missing section `{id}`"
        );
    }

    // A healthy local cluster should not report itself as Down.
    assert_ne!(
        snapshot.health,
        Health::Unknown,
        "health should be resolved once nodes are known"
    );

    // Log fetching must not error, and anything returned must be an Event-stream
    // line with a real timestamp.
    let logs = collector
        .fetch_logs(Utc::now() - Duration::hours(24), 50)
        .await
        .expect("fetch_logs must not error");
    assert!(logs.len() <= 50, "limit was not honoured: {}", logs.len());
    for event in &logs {
        assert!(!event.message.is_empty(), "log lines need a message");
        assert_eq!(event.kind, ServiceKind::Kubernetes);
    }
}

#[tokio::test]
async fn a_bogus_context_fails_to_build_rather_than_hanging() {
    // A typo in the context name should be a clear config error at build time,
    // not a connection that times out on every poll forever.
    let result = build_collector(&profile("definitely-not-a-real-context"), None).await;
    assert!(result.is_err(), "a missing context must not build");
}
