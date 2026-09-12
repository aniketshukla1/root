//! The whole chain against real, deliberately-misconfigured services.
//!
//! Every other test here proves a piece in isolation: the collector parses,
//! the rule fires on a fixture, the forecaster refuses a sawtooth. None of
//! them proves the pieces meet — that a real Redis, read by root's own
//! collector, produces config and metrics that root's own rules then turn into
//! the finding they were written for.
//!
//! That is what this does, against `demo/compose.yml` — an estate configured
//! wrongly on purpose, in the specific ways the rules exist to catch:
//!
//! ```text
//! docker compose -f demo/compose.yml up -d
//! cargo test --test demo_estate -- --ignored --nocapture
//! ```
//!
//! Ignored by default because it needs that estate running. Skipping rather
//! than failing when a service is absent would be worse here than elsewhere:
//! the entire point is that these assertions ran against something real, and a
//! silent skip would let the suite go green having proved nothing.

use chrono::Utc;
use inframonitoring_lib::advisor;
use inframonitoring_lib::collectors::{build_collector, Collector};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::forecast;
use inframonitoring_lib::history::{MetricSeries, Point};
use inframonitoring_lib::model::{Health, HealthSnapshot, ServiceKind};

/// Credentials from `demo/compose.yml`. Literal, fake, and committed on
/// purpose — the estate is disposable and nothing here is a secret.
const DEMO_SECRET: &str = "demo-not-a-real-password";

const CACHE: &str = "redis://localhost:6399";
const CACHE_HEALTHY: &str = "redis://localhost:6398";
const DB: &str = "postgres://postgres@localhost:5442/orders";
const PROXY: &str = "http://localhost:8088/stub_status";

fn profile(id: &str, kind: ServiceKind, endpoint: &str) -> ConnectionProfile {
    ConnectionProfile {
        id: id.to_string(),
        name: id.to_string(),
        kind,
        endpoint: endpoint.to_string(),
        username: None,
        has_secret: true,
        enabled: true,
        poll_interval_secs: 15,
        options: ProfileOptions::default(),
        depends_on: Vec::new(),
        config_sources: Vec::new(),
        history_source: None,
        metadata: ProfileOptions::default(),
    }
}

async fn connect(
    id: &str,
    kind: ServiceKind,
    endpoint: &str,
    secret: Option<&str>,
) -> Option<(std::sync::Arc<dyn Collector>, HealthSnapshot)> {
    let collector = build_collector(&profile(id, kind, endpoint), secret.map(str::to_string))
        .await
        .ok()?;
    let snapshot = collector.collect().await.ok()?;
    Some((collector, snapshot))
}

/// A synthetic climb ending at the value just measured.
///
/// The demo's fillers produce a real trend, but only after running for an
/// hour, and a test that needs an hour of warm-up is a test nobody runs. So
/// the *shape* is synthesised and the *endpoint* is the reading that was
/// actually taken — which is the half that matters here. Whether root can fit
/// a line is settled by `forecast`'s own tests; what is unproven until now is
/// whether a rule fires on a real collector's real output.
fn climb_to(series_key: &str, latest: f64, warn_above: Option<f64>, unit: inframonitoring_lib::model::MetricUnit) -> MetricSeries {
    let count = 60;
    let start = Utc::now() - chrono::Duration::minutes(count);
    let from = latest * 0.35;

    let points: Vec<Point> = (0..count)
        .map(|i| Point {
            t: start + chrono::Duration::minutes(i),
            v: from + (latest - from) * (i as f64 / (count - 1) as f64),
        })
        .collect();

    MetricSeries {
        key: series_key.to_string(),
        label: series_key.to_string(),
        unit,
        warn_above,
        change_pct: None,
        min: from,
        max: latest,
        latest,
        trend: forecast::trend(&points, warn_above),
        points,
    }
}

// ------------------------------------------------------------------- caching

/// The misconfigured cache, end to end: collector reads it, `CONFIG GET *`
/// returns the real settings, and the rule turns the pair into the finding it
/// was written for.
#[tokio::test]
#[ignore]
async fn the_misconfigured_cache_produces_the_finding_it_was_built_to_produce() {
    let (collector, snapshot) = connect("demo-cache", ServiceKind::Redis, CACHE, Some(DEMO_SECRET))
        .await
        .expect("the demo cache should be up — `docker compose -f demo/compose.yml up -d`");

    // Health is deliberately not asserted. This instance is climbing toward a
    // ceiling and will read healthy, degraded or worse depending on how long
    // the filler has been running — pinning it would make the test a clock.
    // What must hold at every point on that climb is the finding.
    println!("demo cache health: {:?} — {}", snapshot.health, snapshot.summary);

    // The config half, read off a running server rather than a fixture.
    let documents = collector.config().await.expect("CONFIG GET *");
    let settings = advisor::Settings::from_documents(&documents);
    assert_eq!(
        settings.get("maxmemory-policy"),
        Some("noeviction"),
        "the compose file sets this; if it has drifted the demo is not testing what it claims"
    );
    let ceiling: f64 = settings
        .get("maxmemory")
        .and_then(|v| v.parse().ok())
        .expect("a ceiling");
    assert_eq!(ceiling, 256.0 * 1024.0 * 1024.0);

    // The telemetry half, off the same server.
    let used = snapshot
        .metrics
        .iter()
        .find(|m| m.key == "used_memory")
        .map(|m| m.value)
        .expect("a memory metric");
    println!(
        "demo cache: {:.1} MB of {:.0} MB, policy {}",
        used / 1024.0 / 1024.0,
        ceiling / 1024.0 / 1024.0,
        settings.get("maxmemory-policy").unwrap_or("?"),
    );

    // Both halves together.
    let series = vec![climb_to(
        "used_memory",
        used,
        Some(ceiling),
        inframonitoring_lib::model::MetricUnit::Bytes,
    )];
    let found = advisor::findings(&snapshot, &documents, &series);
    for finding in &found {
        println!("  [{:?}] {} — {}", finding.severity, finding.rule, finding.title);
    }

    assert!(
        found.iter().any(|f| f.rule.contains("maxmemory-noeviction")),
        "a filling cache with noeviction must produce the write-failure finding: {found:#?}"
    );

    // The suggestion has to name the setting an operator would actually change.
    let finding = found
        .iter()
        .find(|f| f.rule.contains("maxmemory-noeviction"))
        .unwrap();
    let suggestion = finding.suggestion.as_ref().expect("something to change");
    assert_eq!(suggestion.setting, "maxmemory-policy");
    assert!(suggestion.apply.as_ref().unwrap().contains("allkeys-lru"));
}

/// The other half of the estate, and the more important assertion. A demo
/// where everything is flagged teaches the same wrong lesson as one where
/// nothing is: root has to stay quiet about a cache that is set up correctly.
#[tokio::test]
#[ignore]
async fn the_healthy_cache_produces_nothing() {
    let (collector, snapshot) = connect(
        "demo-cache-healthy",
        ServiceKind::Redis,
        CACHE_HEALTHY,
        Some(DEMO_SECRET),
    )
    .await
    .expect("the healthy demo cache should be up");

    let documents = collector.config().await.expect("CONFIG GET *");
    let settings = advisor::Settings::from_documents(&documents);
    assert_eq!(settings.get("maxmemory-policy"), Some("allkeys-lru"));

    let used = snapshot
        .metrics
        .iter()
        .find(|m| m.key == "used_memory")
        .map(|m| m.value)
        .expect("a memory metric");
    let ceiling: f64 = settings.get("maxmemory").and_then(|v| v.parse().ok()).unwrap();

    // Flat and far from the ceiling, which is what this instance actually is.
    let flat = MetricSeries {
        key: "memory".into(),
        label: "Memory".into(),
        unit: inframonitoring_lib::model::MetricUnit::Bytes,
        warn_above: Some(ceiling),
        change_pct: None,
        min: used,
        max: used,
        latest: used,
        trend: None,
        points: (0..60)
            .map(|i| Point {
                t: Utc::now() - chrono::Duration::minutes(60 - i),
                v: used,
            })
            .collect(),
    };

    let found = advisor::findings(&snapshot, &documents, &[flat]);
    assert!(
        found.is_empty(),
        "a correctly-sized cache must produce nothing: {found:#?}"
    );
}

// ------------------------------------------------------------------ database

#[tokio::test]
#[ignore]
async fn the_database_reports_its_real_connection_ceiling() {
    let (collector, snapshot) = connect("demo-db", ServiceKind::Postgres, DB, Some(DEMO_SECRET))
        .await
        .expect("the demo database should be up");

    assert_eq!(snapshot.health, Health::Healthy, "{}", snapshot.summary);

    // `pg_settings` filtered to non-defaults — the values somebody chose,
    // which is the set that explains behaviour.
    let documents = collector.config().await.expect("pg_settings");
    let settings = advisor::Settings::from_documents(&documents);
    // By its bare name: a rule should not have to know that this collector
    // reports settings with their provenance attached.
    let max: f64 = settings
        .get("max_connections")
        .and_then(|v| v.parse().ok())
        .expect("max_connections");
    assert_eq!(max, 25.0, "the compose file sets this");

    let current = snapshot
        .metrics
        .iter()
        .find(|m| m.key == "connections")
        .map(|m| m.value)
        .expect("a connections metric");
    println!("demo db: {current:.0} of {max:.0} connections");

    let series = vec![climb_to(
        "connections",
        current,
        Some(max),
        inframonitoring_lib::model::MetricUnit::Count,
    )];
    let found = advisor::findings(&snapshot, &documents, &series);
    for finding in &found {
        println!("  [{:?}] {} — {}", finding.severity, finding.rule, finding.title);
    }

    assert!(
        found.iter().any(|f| f.rule.contains("max-connections")),
        "a climbing connection count against 25 slots must be reported: {found:#?}"
    );
    // Postgres, so the remedy must be the Postgres one.
    let finding = found.iter().find(|f| f.rule.contains("max-connections")).unwrap();
    assert!(finding.detail.contains("PgBouncer"), "{}", finding.detail);
}

// --------------------------------------------------------------------- proxy

/// nginx only, because this is the collector most often pointed at a server
/// where nobody enabled `stub_status` — and the demo proves the happy path
/// actually parses.
#[tokio::test]
#[ignore]
async fn the_proxy_reports_real_request_counts() {
    let (_, snapshot) = connect("demo-proxy", ServiceKind::Nginx, PROXY, None)
        .await
        .expect("the demo proxy should be up");

    assert_eq!(snapshot.health, Health::Healthy, "{}", snapshot.summary);
    let requests = snapshot
        .metrics
        .iter()
        .find(|m| m.key.contains("request"))
        .map(|m| m.value)
        .unwrap_or(0.0);
    println!("demo proxy: {} — {requests} requests", snapshot.summary);
    assert!(requests > 0.0, "the traffic generator should have produced some");
}

// -------------------------------------------------------------------- docker

/// The estate seen as containers, which is where the limit rules live. The
/// demo deliberately leaves the misconfigured cache unlimited.
#[tokio::test]
#[ignore]
async fn the_containers_show_which_have_no_memory_limit() {
    let Some((collector, snapshot)) = connect("demo-docker", ServiceKind::Docker, "", None).await
    else {
        eprintln!("skipping: no reachable Docker socket");
        return;
    };

    let documents = collector.config().await.expect("container limits");
    let found = advisor::findings(&snapshot, &documents, &[]);
    for finding in &found {
        println!("  [{:?}] {} — {}", finding.severity, finding.rule, finding.title);
    }

    let unlimited = found
        .iter()
        .find(|f| f.rule.contains("container-no-memory-limit"))
        .expect("the demo runs containers without limits on purpose");
    assert!(
        unlimited.detail.contains("root-demo-cache"),
        "the unlimited demo cache should be named: {}",
        unlimited.detail
    );
}
