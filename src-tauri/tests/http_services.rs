//! Prometheus, RabbitMQ and Nginx against real servers.
//!
//! All three parse a payload whose exact shape is version-dependent, and all
//! three degrade rather than error on a missing field — so a wrong key reads as
//! a permanent zero, not a failure. These assert the values actually arrived.
//!
//! Skips cleanly when nothing is reachable. To run them:
//!
//! ```bash
//! docker run -d --name im-prom -p 59090:9090 prom/prometheus:latest
//! docker run -d --name im-rabbit -p 55672:15672 rabbitmq:3-management
//! docker run -d --name im-nginx -p 58080:80 \
//!     -v "$PWD/nginx-status.conf:/etc/nginx/conf.d/default.conf:ro" nginx:alpine
//! ```
//!
//! The nginx one needs a config exposing `stub_status` — it is off by default.

use inframonitoring_lib::collectors::{build_collector, validate, Collector};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::model::{Health, HealthSnapshot, ServiceKind};

use chrono::{Duration, Utc};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROM_URL: &str = "http://127.0.0.1:59090";
const RABBIT_URL: &str = "http://127.0.0.1:55672";
const NGINX_URL: &str = "http://127.0.0.1:58080/nginx_status";

fn profile(kind: ServiceKind, endpoint: &str, username: Option<&str>) -> ConnectionProfile {
    ConnectionProfile {
        id: "http-test".to_string(),
        name: "svc".to_string(),
        kind,
        endpoint: endpoint.to_string(),
        username: username.map(str::to_string),
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

async fn live(
    kind: ServiceKind,
    url: &str,
    username: Option<&str>,
    secret: Option<&str>,
) -> Option<(std::sync::Arc<dyn Collector>, HealthSnapshot)> {
    inframonitoring_lib::install_crypto_provider();
    let collector = build_collector(&profile(kind, url, username), secret.map(str::to_string))
        .await
        .ok()?;
    let snapshot = collector.collect().await.ok()?;
    snapshot.error.is_none().then_some((collector, snapshot))
}

#[tokio::test]
async fn prometheus_reports_targets_and_rules() {
    let Some((collector, snapshot)) = live(ServiceKind::Prometheus, PROM_URL, None, None).await
    else {
        eprintln!("skipping: no Prometheus on {PROM_URL}");
        return;
    };

    assert_eq!(snapshot.kind, ServiceKind::Prometheus);
    validate::assert_renderable(&snapshot);

    // A default Prometheus scrapes itself, so there is at least one target and
    // it should be up.
    assert!(
        metric(&snapshot, "targets_total") >= 1.0,
        "Prometheus scrapes itself by default"
    );
    assert!(
        metric(&snapshot, "targets_up") >= 1.0,
        "its own target should be up: {}",
        snapshot.summary
    );
    // Head series only appears once it has scraped at least once.
    assert!(metric(&snapshot, "series") >= 0.0);

    for id in ["alerts", "targets"] {
        assert!(
            snapshot.sections.iter().any(|s| s.id == id),
            "missing section `{id}`"
        );
    }

    let logs = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 5)
        .await
        .expect("fetch_logs");
    assert!(logs.len() <= 5);
}

#[tokio::test]
async fn rabbitmq_reports_broker_state() {
    // The image ships the guest/guest account, usable from localhost.
    let Some((collector, snapshot)) =
        live(ServiceKind::Rabbitmq, RABBIT_URL, Some("guest"), Some("guest")).await
    else {
        eprintln!("skipping: no RabbitMQ management API on {RABBIT_URL}");
        return;
    };

    assert_eq!(snapshot.kind, ServiceKind::Rabbitmq);
    validate::assert_renderable(&snapshot);

    // A fresh broker has no queues, which is healthy — not an absence of data.
    assert_eq!(
        snapshot.health,
        Health::Healthy,
        "an idle broker should be healthy: {}",
        snapshot.summary
    );
    assert_eq!(
        metric(&snapshot, "queues_stuck"),
        0.0,
        "no queues means none stuck"
    );

    // /api/nodes must have answered — this is the call most likely to fail on
    // a permissions problem, and it would silently yield an empty section.
    let nodes = snapshot
        .sections
        .iter()
        .find(|s| s.id == "nodes")
        .expect("nodes section");
    assert!(
        !nodes.rows.is_empty(),
        "a running broker reports at least one node"
    );

    // The broker section proves /api/overview parsed, version and all.
    let broker = snapshot
        .sections
        .iter()
        .find(|s| s.id == "broker")
        .expect("broker section");
    assert_eq!(broker.rows.len(), 1);
    assert!(
        broker.rows[0].summary.starts_with("RabbitMQ "),
        "version should be reported: {}",
        broker.rows[0].summary
    );

    let logs = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 5)
        .await
        .expect("fetch_logs");
    assert!(logs.len() <= 5);
}

#[tokio::test]
async fn nginx_parses_stub_status_and_rates() {
    let Some((collector, first)) = live(ServiceKind::Nginx, NGINX_URL, None, None).await else {
        eprintln!("skipping: no nginx stub_status on {NGINX_URL}");
        return;
    };

    assert_eq!(first.kind, ServiceKind::Nginx);
    validate::assert_renderable(&first);
    assert_eq!(
        first.health,
        Health::Healthy,
        "an idle nginx should be healthy: {}",
        first.summary
    );

    // We are ourselves a connection, so active is at least one and requests
    // must have been counted.
    assert!(metric(&first, "active") >= 1.0);
    assert!(metric(&first, "requests_total") >= 1.0);
    assert_eq!(metric(&first, "dropped"), 0.0, "an idle nginx drops nothing");

    // Rates need two samples, so the first poll must not invent one.
    assert!(
        first.metrics.iter().all(|m| m.key != "request_rate"),
        "a rate cannot exist before a second sample"
    );

    // Poll again: now there is a previous sample and a rate should appear.
    let second = collector.collect().await.expect("second collect");
    assert!(
        second.metrics.iter().any(|m| m.key == "request_rate"),
        "second poll should produce a request rate"
    );

    // stub_status has no log stream, and must say so by returning nothing
    // rather than fabricating lines.
    let logs = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 10)
        .await
        .expect("fetch_logs");
    assert!(logs.is_empty(), "stub_status exposes counters, not messages");
}

/// Pointing at the site root is the usual misconfiguration, and it must report
/// the real cause rather than a snapshot full of zeroes.
///
/// This one serves its own page rather than reusing the container the rest of
/// the file needs. It used to point at port 58080 and skip "when the server is
/// not up" — but the guard tested for a zero latency and no error, which is
/// not what an unreachable host produces. With nothing listening, the
/// collector correctly reported a connection failure and the test asserted
/// that a connection failure should mention `stub_status`. Serving the wrong
/// body deliberately is the only way to actually exercise the wrong body.
#[tokio::test]
async fn nginx_rejects_a_page_that_is_not_stub_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><body>Welcome to nginx!</body></html>"),
        )
        .mount(&server)
        .await;

    let collector = build_collector(&profile(ServiceKind::Nginx, &server.uri(), None), None)
        .await
        .expect("nginx collector builds");
    let snapshot = collector.collect().await.expect("returns a snapshot either way");

    let error = snapshot
        .error
        .expect("an HTML page is not stub_status output, so this must not be a healthy snapshot");
    assert!(
        error.contains("stub_status"),
        "the error should name the real cause: {error}"
    );
}
