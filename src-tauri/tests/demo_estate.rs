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
        // AWS reads this as the access key id; every other collector here
        // authenticates with the secret alone and ignores it.
        username: matches!(kind, ServiceKind::Aws).then(|| "test".to_string()),
        has_secret: true,
        enabled: true,
        poll_interval_secs: 15,
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

// ----------------------------------------------------------------------- aws

const AWS: &str = "http://localhost:4566";

/// Root's AWS collector against a running emulator.
///
/// The point is not that Floci works — it is that the collector reads a real
/// AWS API surface, over a real signed request, and produces the components
/// and configuration the rules will later need. A collector that only ever
/// ran against a mocked HTTP response would not have proved that SigV4, path
/// style addressing or the SDK's error shapes are right.
#[tokio::test]
#[ignore]
async fn aws_reports_buckets_and_queues_from_a_live_emulator() {
    let (_collector, snapshot) = connect("demo-aws", ServiceKind::Aws, AWS, Some("test"))
        .await
        .expect("the AWS emulator should be up — `docker compose -f demo/compose.yml up -d`");

    println!("demo aws: {:?} — {}", snapshot.health, snapshot.summary);
    for metric in &snapshot.metrics {
        println!("  metric {} = {}", metric.key, metric.value);
    }

    let buckets = snapshot
        .sections
        .iter()
        .find(|s| s.id == "buckets")
        .expect("a buckets section");
    let queues = snapshot
        .sections
        .iter()
        .find(|s| s.id == "queues")
        .expect("a queues section");

    for row in &buckets.rows {
        println!("  bucket {} — {}", row.name, row.summary);
    }
    for row in &queues.rows {
        println!("  queue {} — {}", row.name, row.summary);
    }

    assert!(
        buckets.rows.iter().any(|r| r.name == "root-demo-uploads"),
        "the seeder creates this bucket"
    );
    assert!(
        queues.rows.iter().any(|r| r.name == "root-demo-jobs"),
        "the seeder creates this queue"
    );

    // The seeder writes objects continuously, so this must be non-zero — a
    // zero here means the walk is not actually reading the bucket.
    let stored = snapshot
        .metrics
        .iter()
        .find(|m| m.key == "stored_bytes")
        .map(|m| m.value)
        .expect("a stored bytes metric");
    assert!(stored > 0.0, "the seeder has been uploading objects");

    // Both sections are tracked, which is what gives each bucket and queue its
    // own series — the level at which a capacity answer is actionable.
    assert!(buckets.tracked, "per-bucket growth is the useful series");
    assert!(queues.tracked);
}

/// The config half. Lifecycle rules and dead-letter targets are facts no
/// metric shows, and they are what turn a growing bucket or a deep queue from
/// a number into a decision.
#[tokio::test]
#[ignore]
async fn aws_config_carries_the_facts_no_metric_shows() {
    let (collector, _snapshot) = connect("demo-aws", ServiceKind::Aws, AWS, Some("test"))
        .await
        .expect("the AWS emulator should be up");

    let documents = collector.config().await.expect("s3/sqs attributes");
    let body = documents[0].body.as_deref().expect("a body");
    println!("{body}");

    let parsed: serde_json::Value = serde_json::from_str(body).expect("json");

    // The seeder creates buckets with no lifecycle configuration, which is the
    // default and the commonest real state.
    let rules = parsed["buckets"]["root-demo-uploads"]["lifecycleRules"]
        .as_u64()
        .expect("a rule count");
    assert_eq!(rules, 0, "nothing will ever delete from this bucket");

    // And a queue with no redrive policy, which is the other default that
    // bites: a poison message retries forever.
    let has_dlq = parsed["queues"]["root-demo-jobs"]["hasDeadLetterQueue"]
        .as_bool()
        .expect("a dlq flag");
    assert!(!has_dlq, "the seeder does not attach the DLQ, on purpose");
}

/// The rules on top of the AWS collector, against live resources.
#[tokio::test]
#[ignore]
async fn aws_findings_name_the_cost_and_the_missing_consumer() {
    let (collector, snapshot) = connect("demo-aws", ServiceKind::Aws, AWS, Some("test"))
        .await
        .expect("the AWS emulator should be up");

    let documents = collector.config().await.expect("s3/sqs attributes");
    let stored = snapshot
        .metrics
        .iter()
        .find(|m| m.key == "stored_bytes")
        .map(|m| m.value)
        .unwrap_or(0.0);

    let series = vec![climb_to(
        "stored_bytes",
        stored,
        None,
        inframonitoring_lib::model::MetricUnit::Bytes,
    )];

    let found = advisor::findings(&snapshot, &documents, &series);
    for finding in &found {
        println!("  [{:?}] {} — {}", finding.severity, finding.rule, finding.title);
        println!("      {}", finding.detail);
    }

    assert!(
        found.iter().any(|f| f.rule.contains("bucket-no-lifecycle")),
        "buckets with no lifecycle policy grow for ever: {found:#?}"
    );
    assert!(
        found.iter().any(|f| f.rule.contains("queue-unattended")),
        "the seeder fills a queue nothing consumes: {found:#?}"
    );
    assert!(
        found.iter().any(|f| f.rule.contains("queue-no-dlq")),
        "the seeder's queue has no redrive policy: {found:#?}"
    );

    // The growth rate is what makes the lifecycle finding actionable without a
    // declared ceiling.
    let lifecycle = found
        .iter()
        .find(|f| f.rule.contains("bucket-no-lifecycle"))
        .unwrap();
    assert!(
        lifecycle.detail.contains("a day"),
        "the rate should be stated: {}",
        lifecycle.detail
    );
}

/// The last mile: root's own config file, root's own keychain, every demo
/// connection polled the way the running app polls them.
///
/// Every test above builds a profile by hand. This one proves the estate is
/// actually *registered* — that the connections exist in root's configuration,
/// that their credentials resolve out of the keychain, and that each one comes
/// back healthy. Machine-specific by nature, so it skips rather than fails
/// when root has never been run here.
#[tokio::test]
#[ignore]
async fn every_demo_connection_is_registered_and_reachable() {
    use inframonitoring_lib::{config, secrets};

    let Some(base) = dirs_config() else {
        eprintln!("skipping: no config directory for this platform");
        return;
    };
    let path = config::config_path(&base);
    if !path.exists() {
        eprintln!("skipping: root has no config at {}", path.display());
        return;
    }

    let loaded = config::AppConfig::load(&path).expect("root's config should parse");
    let demo: Vec<_> = loaded
        .profiles
        .iter()
        .filter(|p| p.id.starts_with("demo-"))
        .collect();

    assert!(
        !demo.is_empty(),
        "no demo connections registered in {}",
        path.display()
    );

    let mut failures: Vec<String> = Vec::new();
    for profile in &demo {
        // Reported rather than swallowed: a credential that cannot be read
        // and a credential that was never set produce the same "unreachable",
        // and they are fixed in completely different places.
        let secret = match secrets::get_secret(&profile.secret_key()).await {
            Ok(secret) => {
                println!(
                    "  {:<20} secret: {}",
                    profile.name,
                    if secret.is_some() { "read" } else { "not set" }
                );
                secret
            }
            Err(e) => {
                failures.push(format!("{}: keychain — {e}", profile.name));
                None
            }
        };
        match build_collector(profile, secret).await {
            Ok(collector) => match collector.collect().await {
                Ok(snapshot) => {
                    println!(
                        "  {:<20} {:?}  {}",
                        profile.name, snapshot.health, snapshot.summary
                    );
                    if snapshot.health == Health::Down {
                        failures.push(format!("{}: {}", profile.name, snapshot.summary));
                    }
                }
                Err(e) => failures.push(format!("{}: collect failed — {e}", profile.name)),
            },
            Err(e) => failures.push(format!("{}: could not build — {e}", profile.name)),
        }
    }

    assert!(failures.is_empty(), "connections root cannot reach:\n  {}", failures.join("\n  "));
}

/// Where the running app keeps its configuration.
fn dirs_config() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let base = std::path::PathBuf::from(home);
    #[cfg(target_os = "macos")]
    let dir = base.join("Library/Application Support/com.root.observability");
    #[cfg(not(target_os = "macos"))]
    let dir = base.join(".config/com.root.observability");
    Some(dir)
}

// ------------------------------------------------------------- what things are

/// The bug this was reported as: in a containerised estate, everything read as
/// "a container". The demo runs a database, two caches, a proxy and a queue —
/// and the list said nothing about which was which.
#[tokio::test]
#[ignore]
async fn containers_are_identified_by_what_they_run() {
    let Some((_, snapshot)) = connect("demo-docker", ServiceKind::Docker, "", None).await else {
        eprintln!("skipping: no reachable Docker socket");
        return;
    };

    let containers = snapshot
        .sections
        .iter()
        .find(|s| s.id == "containers")
        .expect("a containers section");

    let kind_of = |name: &str| -> Option<String> {
        containers
            .rows
            .iter()
            .find(|r| r.name == name)
            .and_then(|r| r.cells.get("kind"))
            .and_then(|c| match c {
                inframonitoring_lib::model::CellValue::Text { value } => Some(value.clone()),
                _ => None,
            })
    };

    for name in ["root-demo-db", "root-demo-cache", "root-demo-proxy", "root-demo-queue"] {
        println!("  {name:<28} {}", kind_of(name).unwrap_or_else(|| "?".into()));
    }
    println!(
        "  {:<28} {}",
        "root-demo-db-churner",
        kind_of("root-demo-db-churner").unwrap_or_else(|| "?".into())
    );

    assert_eq!(kind_of("root-demo-db").as_deref(), Some("PostgreSQL"));
    assert_eq!(kind_of("root-demo-cache").as_deref(), Some("Redis"));
    assert_eq!(kind_of("root-demo-proxy").as_deref(), Some("Nginx"));
    assert_eq!(kind_of("root-demo-queue").as_deref(), Some("RabbitMQ"));

    // The case that makes the port check worth having: same image as the real
    // database, publishes nothing, runs psql in a loop. Calling this a
    // PostgreSQL would put a database in the inventory that does not exist.
    assert_eq!(
        kind_of("root-demo-db-churner").as_deref(),
        Some("PostgreSQL (not serving)"),
        "the churner carries the image but is not a database"
    );

    // An image root does not recognise says nothing rather than guessing.
    assert_eq!(kind_of("root-demo-proxy-traffic").as_deref(), Some("—"));
}

/// The other half of the same bug: the demo's Postgres appears twice — as a
/// container on the Docker host, and as the `demo-db` connection pointed at
/// it. Root showed them as unrelated, so an operator could not tell which
/// container rows were already covered properly.
///
/// Needs the demo connections registered in root's own config, so it skips
/// when they are not.
#[tokio::test]
#[ignore]
async fn a_container_is_linked_to_the_connection_that_watches_it() {
    use inframonitoring_lib::{config, identify};

    let Some(base) = dirs_config() else { return };
    let path = config::config_path(&base);
    if !path.exists() {
        eprintln!("skipping: root has no config here");
        return;
    }
    let loaded = config::AppConfig::load(&path).expect("root's config should parse");

    let Some((_, snapshot)) = connect("demo-docker", ServiceKind::Docker, "", None).await else {
        eprintln!("skipping: no reachable Docker socket");
        return;
    };
    let containers = snapshot
        .sections
        .iter()
        .find(|s| s.id == "containers")
        .expect("a containers section");

    // The supervisor does this join on every poll; done here directly because
    // a test does not run the supervisor.
    let connections: Vec<(String, String)> = loaded
        .profiles
        .iter()
        .filter(|p| p.enabled && p.kind != ServiceKind::Docker)
        .map(|p| (p.id.clone(), p.endpoint.clone()))
        .collect();

    let watcher_of = |name: &str| -> Option<String> {
        let row = containers.rows.iter().find(|r| r.name == name)?;
        let ports: Vec<u16> = row
            .extra
            .get("hostPorts")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_u64())
            .map(|p| p as u16)
            .collect();
        identify::monitored_by(
            &ports,
            connections.iter().map(|(id, e)| (id.as_str(), e.as_str())),
        )
    };

    for name in ["root-demo-db", "root-demo-cache", "root-demo-proxy"] {
        println!("  {name:<22} watched by {:?}", watcher_of(name));
    }

    assert_eq!(watcher_of("root-demo-db").as_deref(), Some("demo-db"));
    assert_eq!(watcher_of("root-demo-cache").as_deref(), Some("demo-cache"));

    // Publishes nothing, so nothing can be pointed at it — correctly unlinked
    // rather than guessed at from its image.
    assert_eq!(watcher_of("root-demo-db-churner"), None);
}

/// A declared runtime, checked against what root can actually see.
///
/// root reads a container's image and published port to work out what it is,
/// and nothing checks that answer — so a wrong one is wrong quietly. A
/// declaration gives it something to be wrong against, and this is the case
/// the whole feature exists for: `demo-proxy` is declared as
/// `root-demo-nginx`, and the real container is `root-demo-proxy`.
#[tokio::test]
#[ignore]
async fn a_wrong_declaration_is_caught_against_the_live_estate() {
    use inframonitoring_lib::{config, runtime};

    let Some(base) = dirs_config() else { return };
    let path = config::config_path(&base);
    if !path.exists() {
        eprintln!("skipping: root has no config here");
        return;
    }
    let loaded = config::AppConfig::load(&path).expect("root's config should parse");

    let Some((_, snapshot)) = connect("demo-docker", ServiceKind::Docker, "", None).await else {
        eprintln!("skipping: no reachable Docker socket");
        return;
    };
    let containers = snapshot
        .sections
        .iter()
        .find(|s| s.id == "containers")
        .expect("a containers section");

    let components: Vec<runtime::Component<'_>> = containers
        .rows
        .iter()
        .map(|row| {
            let kind_text = row.cells.get("kind").and_then(|c| match c {
                inframonitoring_lib::model::CellValue::Text { value } => Some(value.as_str()),
                _ => None,
            });
            runtime::Component {
                name: row.name.as_str(),
                identified: kind_text
                    .filter(|t| *t != "—")
                    .and_then(|t| {
                        inframonitoring_lib::registry::kind_from_label(
                            t.trim_end_matches(" (not serving)"),
                        )
                    }),
                serving: kind_text.is_some_and(|t| t != "—" && !t.contains("not serving")),
                watched_by: None,
            }
        })
        .collect();

    let mut reported = 0;
    for profile in loaded.profiles.iter().filter(|p| p.runtime.is_declared()) {
        let found = runtime::reconcile(
            &profile.id,
            &profile.runtime,
            true,
            "demo-docker",
            &components,
        );
        for d in &found {
            println!("  {:<20} {}", profile.name, d.describe());
            reported += 1;
        }
        if found.is_empty() && profile.runtime.kind.is_hosted() {
            println!("  {:<20} agrees", profile.name);
        }

        // The planted error, and the only one that must be reported.
        if profile.id == "demo-proxy" {
            assert_eq!(
                found,
                vec![runtime::Discrepancy::NotFound {
                    host: "demo-docker".into(),
                    name: "root-demo-nginx".into(),
                }],
                "a declaration naming a container that does not exist must be caught"
            );
        }
        // The correct ones must stay quiet, or the check is just noise.
        if profile.id == "demo-db" || profile.id == "demo-cache" {
            assert!(found.is_empty(), "{}: {found:#?}", profile.name);
        }
    }

    assert_eq!(reported, 1, "exactly the planted error, nothing else");
}
