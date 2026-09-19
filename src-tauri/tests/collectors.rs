//! End-to-end collector tests against a mock HTTP server.
//!
//! These exercise the parts most likely to break silently in production: real
//! request construction, real JSON parsing of the shapes Jenkins and
//! Elasticsearch actually return, and the mapping from those payloads onto
//! health, metrics, and log events.

use inframonitoring_lib::collectors::{build_collector, validate, DetailQuery};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::model::{CellValue, Health, HealthSnapshot, LogLevel, Row, ServiceKind};

/// Rows of one named section, after asserting the whole snapshot is renderable
/// by the generic table — the invariant that keeps a new service type working
/// with no frontend code.
fn section<'a>(snapshot: &'a HealthSnapshot, id: &str) -> &'a [Row] {
    validate::assert_renderable(snapshot);
    &snapshot
        .sections
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| {
            panic!(
                "no `{id}` section; got {:?}",
                snapshot.sections.iter().map(|s| &s.id).collect::<Vec<_>>()
            )
        })
        .rows
}

fn row<'a>(rows: &'a [Row], name: &str) -> &'a Row {
    rows.iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no row named `{name}`"))
}

/// A cell's text, for asserting a value landed in its own typed column rather
/// than being concatenated into a shared string.
fn cell_text(row: &Row, key: &str) -> String {
    match row.cells.get(key) {
        Some(CellValue::Text { value }) => value.clone(),
        Some(CellValue::Number { value }) => value.to_string(),
        Some(other) => format!("{other:?}"),
        None => panic!("row `{}` has no `{key}` cell", row.name),
    }
}

fn cell_number(row: &Row, key: &str) -> f64 {
    match row.cells.get(key) {
        Some(CellValue::Number { value }) => *value,
        Some(CellValue::Gauge { value, .. }) => *value,
        Some(CellValue::Duration { value }) => *value,
        other => panic!("row `{}` `{key}` is not numeric: {other:?}", row.name),
    }
}

use chrono::{Duration, Utc};
use serde_json::json;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile(kind: ServiceKind, endpoint: String, options: ProfileOptions) -> ConnectionProfile {
    ConnectionProfile {
        id: "test-profile".to_string(),
        name: "test".to_string(),
        kind,
        endpoint,
        username: Some("monitor".to_string()),
        has_secret: false,
        enabled: true,
        poll_interval_secs: 10,
        options,
        depends_on: Vec::new(),
            config_sources: Vec::new(),
            history_source: None,
            snapshot_sink: None,
            runtime: Default::default(),
        metadata: ProfileOptions::default(),
    }
}

fn metric(snapshot: &inframonitoring_lib::model::HealthSnapshot, key: &str) -> f64 {
    snapshot
        .metrics
        .iter()
        .find(|m| m.key == key)
        .unwrap_or_else(|| panic!("no `{key}` metric in {:?}", snapshot.metrics))
        .value
}

// ------------------------------------------------------------------ Jenkins

async fn jenkins_server(jobs: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jobs))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/queue/api/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                { "why": "Waiting for next available executor", "stuck": false,
                  "task": { "name": "deploy" } },
                { "why": "Blocked by upstream lock", "stuck": true,
                  "task": { "name": "release" } }
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/computer/api/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "busyExecutors": 2,
            "totalExecutors": 4,
            "computer": [
                { "displayName": "built-in", "offline": false, "idle": false, "numExecutors": 2 },
                { "displayName": "agent-1", "offline": true, "idle": true, "numExecutors": 2,
                  "offlineCauseReason": "Disk space is too low" }
            ]
        })))
        .mount(&server)
        .await;

    server
}

#[tokio::test]
async fn jenkins_reports_failing_jobs_offline_agents_and_a_stuck_queue() {
    let server = jenkins_server(json!({
        "jobs": [
            { "name": "api-build", "color": "blue", "buildable": true,
              "lastBuild": { "number": 42, "result": "SUCCESS", "building": false,
                             "timestamp": 1_700_000_000_000i64, "duration": 45_000 } },
            { "name": "web-build", "color": "red", "buildable": true,
              "lastBuild": { "number": 7, "result": "FAILURE", "building": false,
                             "timestamp": 1_700_000_000_000i64, "duration": 12_000 } },
            { "name": "flaky", "color": "yellow", "buildable": true,
              "lastBuild": { "number": 3, "result": "UNSTABLE", "building": false,
                             "timestamp": 1_700_000_000_000i64, "duration": 9_000 } }
        ]
    }))
    .await;

    let collector = build_collector(
        &profile(ServiceKind::Jenkins, server.uri(), ProfileOptions::default()),
        Some("token".to_string()),
    )
    .await
    .expect("collector builds");

    let snapshot = collector.collect().await.expect("collect succeeds");

    // Jenkins itself is up, so a failing job degrades rather than downs it.
    assert_eq!(snapshot.health, Health::Degraded);
    assert_eq!(metric(&snapshot, "jobs"), 3.0);
    assert_eq!(metric(&snapshot, "failing"), 1.0);
    assert_eq!(metric(&snapshot, "unstable"), 1.0);
    assert_eq!(metric(&snapshot, "queue"), 2.0);
    assert_eq!(metric(&snapshot, "stuck"), 1.0);
    assert_eq!(metric(&snapshot, "nodes_offline"), 1.0);

    // Jobs, agents, and queue items are three different kinds of thing and now
    // live in three different sections rather than one flattened list.
    let jobs = section(&snapshot, "jobs");
    let failing = row(jobs, "web-build");
    assert_eq!(failing.health, Health::Down);
    assert_eq!(cell_text(failing, "result"), "FAILURE");
    assert_eq!(cell_number(failing, "build"), 7.0);
    // Duration is seconds in its own typed column, not a formatted string.
    assert_eq!(cell_number(failing, "duration"), 12.0);

    let agent = row(section(&snapshot, "agents"), "agent-1");
    assert_eq!(agent.health, Health::Down);
    assert_eq!(cell_text(agent, "status"), "offline");
    assert_eq!(cell_text(agent, "reason"), "Disk space is too low");

    // The queue reason is the whole point of surfacing queued items.
    let queued = row(section(&snapshot, "queue"), "release");
    assert_eq!(cell_text(queued, "why"), "Blocked by upstream lock");
    assert_eq!(cell_text(queued, "stuck"), "stuck");
}

#[tokio::test]
async fn jenkins_flattens_folder_jobs_and_requests_nested_console_paths() {
    let server = jenkins_server(json!({
        "jobs": [
            { "name": "team", "jobs": [
                { "name": "api", "color": "red", "buildable": true,
                  "lastBuild": { "number": 9, "result": "FAILURE", "building": false,
                                 "timestamp": 1_700_000_000_000i64, "duration": 1_000 } }
            ]}
        ]
    }))
    .await;

    // A job inside a folder lives at /job/team/job/api/... — getting this wrong
    // is a silent 404 rather than an obvious failure.
    Mock::given(method("GET"))
        .and(path("/job/team/job/api/lastBuild/consoleText"))
        .respond_with(ResponseTemplate::new(200).set_body_string("line one\nline two\nBUILD FAILED"))
        .mount(&server)
        .await;

    let collector = build_collector(
        &profile(ServiceKind::Jenkins, server.uri(), ProfileOptions::default()),
        Some("token".to_string()),
    )
    .await
    .unwrap();

    let snapshot = collector.collect().await.unwrap();
    assert!(
        section(&snapshot, "jobs").iter().any(|r| r.name == "team/api"),
        "folder jobs are flattened to folder/job"
    );

    let detail = collector
        .detail(&DetailQuery::JenkinsBuildLog {
            job: "team/api".to_string(),
            build: None,
            tail_lines: Some(2),
        })
        .await
        .expect("console fetch succeeds");

    assert_eq!(detail["totalLines"], 3);
    assert_eq!(detail["console"], "line two\nBUILD FAILED");
}

#[tokio::test]
async fn jenkins_reports_an_unreachable_controller_as_down_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/json"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let collector = build_collector(
        &profile(ServiceKind::Jenkins, server.uri(), ProfileOptions::default()),
        None,
    )
    .await
    .unwrap();

    // A dashboard needs a red tile here, not a vanished connection.
    let snapshot = collector.collect().await.expect("returns a snapshot, not Err");
    assert_eq!(snapshot.health, Health::Down);
    assert!(snapshot.error.is_some());
}

// ------------------------------------------------------------ Elasticsearch

async fn elasticsearch_server(status: &str) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/_cluster/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "cluster_name": "logs-prod",
            "status": status,
            "number_of_nodes": 3,
            "number_of_data_nodes": 2,
            "active_shards": 40,
            "relocating_shards": 0,
            "initializing_shards": 1,
            "unassigned_shards": 4,
            "active_shards_percent_as_number": 88.9
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/_nodes/stats.*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "nodes": {
                "node-a": {
                    "name": "es-data-1",
                    "roles": ["data", "master"],
                    "jvm": { "mem": { "heap_used_percent": 93.0 } },
                    "os": { "cpu": { "percent": 40.0 } },
                    "fs": { "total": { "total_in_bytes": 1000, "available_in_bytes": 500 } }
                }
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/_cat/indices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "index": "logs-2024.05", "health": "yellow", "status": "open",
              "docs.count": "1500", "store.size": "2048", "pri": "1", "rep": "1" },
            { "index": ".internal-state", "health": "green", "status": "open",
              "docs.count": "5", "store.size": "100", "pri": "1", "rep": "0" }
        ])))
        .mount(&server)
        .await;

    server
}

#[tokio::test]
async fn elasticsearch_maps_cluster_colour_and_flags_a_hot_node() {
    let server = elasticsearch_server("yellow").await;

    let collector = build_collector(
        &profile(ServiceKind::Elasticsearch, server.uri(), ProfileOptions::default()),
        Some("pw".to_string()),
    )
    .await
    .unwrap();

    let snapshot = collector.collect().await.unwrap();

    assert_eq!(snapshot.health, Health::Degraded, "yellow cluster is degraded");
    assert_eq!(metric(&snapshot, "unassigned_shards"), 4.0);
    assert_eq!(metric(&snapshot, "max_heap"), 93.0);

    let node = row(section(&snapshot, "nodes"), "es-data-1");
    assert_eq!(node.health, Health::Degraded, "93% heap is degraded");
    assert_eq!(cell_number(node, "heap"), 93.0);
    assert_eq!(cell_number(node, "cpu"), 40.0);

    let indices = section(&snapshot, "indices");
    // Internal indices are noise on an operator dashboard.
    assert!(
        indices.iter().all(|r| !r.name.contains(".internal-state")),
        "dot-prefixed indices are filtered out"
    );
    let index = row(indices, "logs-2024.05");
    // Docs, size, primaries and replicas each get their own typed column
    // instead of being concatenated into one detail string.
    assert_eq!(cell_number(index, "docs"), 1500.0);
    assert_eq!(cell_number(index, "size"), 2048.0);
    assert_eq!(cell_number(index, "primaries"), 1.0);
    assert_eq!(cell_number(index, "replicas"), 1.0);
}

#[tokio::test]
async fn elasticsearch_red_cluster_is_down() {
    let server = elasticsearch_server("red").await;
    let collector = build_collector(
        &profile(ServiceKind::Elasticsearch, server.uri(), ProfileOptions::default()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(collector.collect().await.unwrap().health, Health::Down);
}

#[tokio::test]
async fn elasticsearch_tails_logs_and_normalizes_ecs_documents() {
    let server = elasticsearch_server("green").await;

    Mock::given(method("POST"))
        .and(path("/logs-*/_search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "hits": { "hits": [
                { "_source": {
                    "@timestamp": "2030-01-01T10:00:01Z",
                    "log": { "level": "ERROR" },
                    "message": "connection pool exhausted",
                    "service": { "name": "checkout-api" }
                }},
                { "_source": {
                    "@timestamp": "2030-01-01T10:00:00Z",
                    "message": "started",
                    "service": { "name": "checkout-api" }
                }}
            ]}
        })))
        .mount(&server)
        .await;

    let mut options = ProfileOptions::default();
    options.set("logIndex", serde_json::json!("logs-*"));
    let collector = build_collector(
        &profile(ServiceKind::Elasticsearch, server.uri(), options),
        None,
    )
    .await
    .unwrap();

    let events = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 50)
        .await
        .expect("log search succeeds");

    assert_eq!(events.len(), 2);
    // Returned oldest-first regardless of the ES sort order.
    assert!(events[0].timestamp < events[1].timestamp);
    assert_eq!(events[1].level, LogLevel::Error, "nested log.level is read");
    assert_eq!(events[1].message, "connection pool exhausted");
    assert_eq!(events[1].source, "checkout-api", "nested service.name is read");
    // No explicit level on the second doc, so it falls back to inference.
    assert_eq!(events[0].level, LogLevel::Info);
}

#[tokio::test]
async fn elasticsearch_without_a_log_index_tails_nothing() {
    let server = elasticsearch_server("green").await;
    let collector = build_collector(
        &profile(ServiceKind::Elasticsearch, server.uri(), ProfileOptions::default()),
        None,
    )
    .await
    .unwrap();

    let events = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 50)
        .await
        .unwrap();
    assert!(events.is_empty(), "cluster health only, no document tailing");
}

// ------------------------------------------------------------------- Docker

#[tokio::test]
async fn docker_collects_against_a_live_daemon_when_one_is_available() {
    let collector = match build_collector(
        &profile(ServiceKind::Docker, String::new(), ProfileOptions::default()),
        None,
    )
    .await
    {
        Ok(collector) => collector,
        Err(_) => {
            eprintln!("skipped: no Docker socket on this machine");
            return;
        }
    };

    let snapshot = collector.collect().await.expect("returns a snapshot either way");

    // `error` is set only when the daemon itself was unreachable. Health alone
    // is not the signal — a healthy daemon hosting a crashed container reports
    // Down, which is exactly the behaviour under test.
    if snapshot.error.is_some() {
        eprintln!("skipped: Docker daemon is not running");
        return;
    }

    assert_eq!(snapshot.kind, ServiceKind::Docker);
    assert!(
        snapshot.metrics.iter().any(|m| m.key == "containers"),
        "container count is always reported"
    );
    // Every running container must be accounted for as a component.
    let running = metric(&snapshot, "running") as usize;
    let containers = section(&snapshot, "containers");
    assert!(containers.len() >= running);

    eprintln!("docker: {}", snapshot.summary);
    for row in containers {
        eprintln!(
            "  [{}] {} — {}",
            row.health.label(),
            row.name,
            row.summary
        );
    }

    // A container that exited non-zero is down; a running one whose health
    // check is failing is degraded, not down. These are the two mappings most
    // likely to be wrong, so verify them whenever such a container exists.
    for component in containers {
        let detail = &component.summary;
        if detail.contains("Exited (0)") {
            assert_eq!(component.health, Health::Unknown, "a clean exit is not a fault");
        } else if detail.starts_with("Exited (")
            && ["second", "minute", "hour"].iter().any(|u| detail.contains(u))
        {
            assert_eq!(component.health, Health::Down, "{detail}");
        } else if detail.starts_with("Exited (") {
            assert_eq!(
                component.health,
                Health::Unknown,
                "a long-dead container is history, not a live fault: {detail}"
            );
        } else if detail.contains("(unhealthy)") {
            assert_eq!(
                component.health,
                Health::Degraded,
                "a failing health check degrades, it does not down: {detail}"
            );
        }
    }

    let logs = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 50)
        .await
        .expect("log tailing works against a live daemon");
    eprintln!("docker: collected {} log lines", logs.len());
    // Watermarking must never hand back lines older than the cutoff.
    let cutoff = Utc::now() - Duration::hours(1);
    assert!(logs.iter().all(|l| l.timestamp >= cutoff));
}

// ----------------------------------------------------------------- Gigapipe

/// A gigapipe whose process is up. `rows` is the `sent_rows{service="logs"}`
/// counter value, so a test can poll twice and watch the rate derive.
fn gigapipe_metrics(rows: u64, bytes: u64, parse_errors: u64) -> String {
    format!(
        "# HELP sent_rows The total number of rows sent\n\
         # TYPE sent_rows counter\n\
         sent_rows{{service=\"logs\"}} {rows}\n\
         sent_rows{{service=\"traces\"}} 40\n\
         sent_bytes{{service=\"logs\"}} {bytes}\n\
         sent_bytes{{service=\"traces\"}} 900\n\
         json_parse_errors_count {parse_errors}\n\
         connection_reset_by_peer_count 0\n\
         go_goroutines 61\n\
         process_resident_memory_bytes 157286400\n"
    )
}

/// A gigapipe that answers `/ready` and serves every query API.
async fn gigapipe_server(serving: bool) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ready"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ready"))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gigapipe_metrics(1000, 90_000, 0)))
        .mount(&server)
        .await;

    // The whole point of probing per signal: these are what a dead ClickHouse
    // takes down while `/ready` keeps saying yes.
    let status = if serving { 200 } else { 500 };
    for api in ["/loki/api/v1/labels", "/api/v1/labels", "/api/echo"] {
        Mock::given(method("GET"))
            .and(path(api))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "status": "success", "data": ["service_name", "level"]
            })))
            .mount(&server)
            .await;
    }

    server
}

fn gigapipe_profile(server: &MockServer, options: ProfileOptions) -> ConnectionProfile {
    profile(ServiceKind::Gigapipe, server.uri(), options)
}

#[tokio::test]
async fn gigapipe_serving_every_signal_is_healthy() {
    let server = gigapipe_server(true).await;
    let collector = build_collector(&gigapipe_profile(&server, ProfileOptions::default()), None)
        .await
        .expect("collector builds");

    let snapshot = collector.collect().await.expect("collect succeeds");

    assert_eq!(snapshot.health, Health::Healthy);
    assert_eq!(metric(&snapshot, "signals_up"), 3.0);

    let signals = section(&snapshot, "signals");
    assert_eq!(signals.len(), 3);
    assert_eq!(cell_text(row(signals, "logs"), "api"), "/loki/api/v1/labels");

    // Two `service` labels on the counter, so two pipelines.
    let ingest = section(&snapshot, "ingest");
    assert_eq!(ingest.len(), 2);
    assert_eq!(cell_number(row(ingest, "logs"), "rows"), 1000.0);
    assert_eq!(cell_number(row(ingest, "traces"), "bytes"), 900.0);
}

/// The Pyroscope selector, in the five-part form `ParseTypeId` expects.
const PROFILE_SELECTOR: &str = "process_cpu:cpu:nanoseconds:cpu:nanoseconds{service_name=\"api\"}";

fn with_profile_query() -> ProfileOptions {
    let mut options = ProfileOptions::default();
    options.set("profileQuery", json!(PROFILE_SELECTOR));
    options
}

#[tokio::test]
async fn gigapipe_without_a_profile_selector_grows_no_profiles_row() {
    // Declared, never inferred. A warehouse holding no profiles — or one
    // whose operator has not said which profile matters — must not sprout an
    // empty table and a screen behind it.
    let server = gigapipe_server(true).await;
    let collector = build_collector(&gigapipe_profile(&server, ProfileOptions::default()), None)
        .await
        .expect("collector builds");

    let snapshot = collector.collect().await.expect("collect succeeds");
    assert!(section(&snapshot, "profiles").is_empty());
    assert_eq!(metric(&snapshot, "signals_up"), 3.0);
}

#[tokio::test]
async fn gigapipe_counts_a_declared_profile_as_a_signal_it_serves() {
    let server = gigapipe_server(true).await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "flamebearer": { "names": ["total"], "levels": [[0, 10, 0, 0]], "numTicks": 10, "maxSelf": 0 },
            "metadata": { "format": "single", "units": "samples", "sampleRate": 100, "name": "cpu" },
        })))
        .mount(&server)
        .await;

    let collector = build_collector(&gigapipe_profile(&server, with_profile_query()), None)
        .await
        .expect("collector builds");

    let snapshot = collector.collect().await.expect("collect succeeds");
    assert_eq!(snapshot.health, Health::Healthy);
    // Four now, not three: a declared profile is a query API like the rest.
    assert_eq!(metric(&snapshot, "signals_up"), 4.0);

    let profiles = section(&snapshot, "profiles");
    assert_eq!(profiles.len(), 1);
    assert_eq!(cell_text(row(profiles, PROFILE_SELECTOR), "selector"), PROFILE_SELECTOR);
}

#[tokio::test]
async fn gigapipe_that_cannot_serve_its_declared_profile_is_degraded_by_it() {
    // The whole reason the probe exists rather than the row being static. A
    // store whose other three APIs answer while `/pyroscope/render` does not
    // is a store that has lost a signal, and it has to say so — the same way
    // losing LogQL does.
    let server = gigapipe_server(true).await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .respond_with(ResponseTemplate::new(500).set_body_string("no such profile type"))
        .mount(&server)
        .await;

    let collector = build_collector(&gigapipe_profile(&server, with_profile_query()), None)
        .await
        .expect("collector builds");

    let snapshot = collector.collect().await.expect("collect succeeds");
    assert_eq!(
        snapshot.health,
        Health::Degraded,
        "a signal went down and the connection did not notice: {}",
        snapshot.summary
    );
    assert_eq!(metric(&snapshot, "signals_up"), 3.0);
    assert_eq!(row(section(&snapshot, "profiles"), PROFILE_SELECTOR).health, Health::Down);
}

#[tokio::test]
async fn gigapipe_ready_with_a_dead_backend_is_down_and_says_why() {
    // The failure this collector exists for. `/ready` answers from the
    // process, so a store whose ClickHouse has gone away looks perfectly
    // healthy from the outside while every query fails.
    let server = gigapipe_server(false).await;
    let collector = build_collector(&gigapipe_profile(&server, ProfileOptions::default()), None)
        .await
        .expect("collector builds");

    let snapshot = collector.collect().await.expect("collect succeeds");

    assert_eq!(snapshot.health, Health::Down);
    assert_eq!(metric(&snapshot, "signals_up"), 0.0);
    assert!(
        snapshot.summary.contains("ClickHouse"),
        "the summary must name the backend, not just say down: {}",
        snapshot.summary
    );
    // And it is a Down snapshot, not a collector error — the trait's contract.
    assert!(snapshot.error.is_none());
}

#[tokio::test]
async fn gigapipe_that_does_not_answer_is_down_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ready"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let collector = build_collector(&gigapipe_profile(&server, ProfileOptions::default()), None)
        .await
        .expect("collector builds");

    let snapshot = collector.collect().await.expect("an unreachable target is not an error");
    assert_eq!(snapshot.health, Health::Down);
    assert!(snapshot.error.is_some(), "the reason must survive to the UI");
}

#[tokio::test]
async fn gigapipe_derives_an_ingest_rate_only_once_it_has_two_readings() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/ready"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ready"))
        .mount(&server)
        .await;
    for api in ["/loki/api/v1/labels", "/api/v1/labels", "/api/echo"] {
        Mock::given(method("GET"))
            .and(path(api))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "success"})))
            .mount(&server)
            .await;
    }

    // First poll sees 1000 rows, second sees 1600. Mounted most-specific
    // first: wiremock retires the bounded mock once it has served its one.
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gigapipe_metrics(1000, 90_000, 0)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(gigapipe_metrics(1600, 140_000, 0)))
        .mount(&server)
        .await;

    let collector = build_collector(&gigapipe_profile(&server, ProfileOptions::default()), None)
        .await
        .expect("collector builds");

    let first = collector.collect().await.expect("first poll");
    assert!(
        !first.metrics.iter().any(|m| m.key == "ingest_rows_per_sec"),
        "one reading of a counter is not a rate, and inventing one would draw \
         a spike on the chart the moment root started"
    );
    assert!(matches!(
        row(section(&first, "ingest"), "logs").cells.get("rate"),
        Some(CellValue::Null) | None
    ));

    let second = collector.collect().await.expect("second poll");
    assert!(
        metric(&second, "ingest_rows_per_sec") > 0.0,
        "600 rows arrived between the two polls"
    );
    assert!(cell_number(row(section(&second, "ingest"), "logs"), "rate") > 0.0);
}

#[tokio::test]
async fn gigapipe_tails_application_logs_out_of_the_warehouse() {
    // A gigapipe connection with a LogQL selector is not reporting on
    // gigapipe. It is carrying whatever was shipped *into* it — which is how
    // root reads OTLP-delivered logs without owning a receiver.
    let server = gigapipe_server(true).await;

    let now = Utc::now();
    let nanos = |offset: i64| (now.timestamp_nanos_opt().unwrap() + offset).to_string();

    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": {
                "resultType": "streams",
                "result": [
                    {
                        "stream": {
                            "service_name": "checkout-api",
                            "detected_level": "error",
                            "host": "10.0.0.4"
                        },
                        "values": [
                            [nanos(-2_000_000_000), "could not reach the payments service"],
                            [nanos(-1_000_000_000), "retrying in 5s"]
                        ]
                    }
                ]
            }
        })))
        .mount(&server)
        .await;

    let mut options = ProfileOptions::default();
    options.set("logQuery", json!("{service_name=\"checkout-api\"}"));

    let collector = build_collector(&gigapipe_profile(&server, options), None)
        .await
        .expect("collector builds");

    let logs = collector
        .fetch_logs(now - Duration::hours(1), 50)
        .await
        .expect("the LogQL query succeeds");

    assert_eq!(logs.len(), 2);
    // The shipper's own identity, not gigapipe's.
    assert_eq!(logs[0].source, "checkout-api");
    // The shipper's declared level beats inferring one from the text.
    assert_eq!(logs[0].level, LogLevel::Error);
    assert_eq!(logs[1].level, LogLevel::Error);
    // Oldest first, like every other collector.
    assert!(logs[0].timestamp < logs[1].timestamp);
}

#[tokio::test]
async fn a_bad_logql_expression_surfaces_the_reason_gigapipe_gave() {
    let server = gigapipe_server(true).await;

    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "status": "error",
            "error": "parse error at line 1, col 1: syntax error: unexpected IDENTIFIER"
        })))
        .mount(&server)
        .await;

    let mut options = ProfileOptions::default();
    options.set("logQuery", json!("not a selector"));

    let collector = build_collector(&gigapipe_profile(&server, options), None)
        .await
        .expect("collector builds");

    let error = collector
        .fetch_logs(Utc::now() - Duration::hours(1), 50)
        .await
        .expect_err("a bad expression is an error, not an empty result");

    assert!(
        error.to_string().contains("syntax error"),
        "the operator needs gigapipe's own message to fix the query: {error}"
    );
}

/// The JSON shape gigapipe's `/api/traces/{id}` actually writes: one flat span
/// list under the pre-1.0 `instrumentationLibrarySpans` key, each span
/// carrying its own `serviceName`, and `startTimeUnixNano` as a **number**.
fn gigapipe_trace_document(base_nanos: i64) -> serde_json::Value {
    json!({
        "resourceSpans": [{
            "resource": { "attributes": [
                { "key": "collector", "value": { "stringValue": "qryn" } }
            ]},
            "instrumentationLibrarySpans": [{ "spans": [
                {
                    "traceID": "abc123", "traceId": "abc123",
                    "spanID": "aaaa", "spanId": "aaaa",
                    "name": "GET /checkout",
                    "serviceName": "gateway",
                    "startTimeUnixNano": base_nanos,
                    "endTimeUnixNano": base_nanos + 500_000_000i64,
                    "attributes": [
                        { "key": "http.method", "value": { "stringValue": "GET" } }
                    ],
                    "events": []
                },
                {
                    "traceID": "abc123", "traceId": "abc123",
                    "spanID": "bbbb", "spanId": "bbbb",
                    "parentSpanId": "aaaa",
                    "name": "SELECT orders",
                    "serviceName": "postgres",
                    "startTimeUnixNano": base_nanos + 50_000_000i64,
                    "endTimeUnixNano": base_nanos + 480_000_000i64,
                    "attributes": [],
                    "events": [
                        { "timeUnixNano": base_nanos + 60_000_000i64, "name": "query.start" }
                    ],
                    "status": { "code": 2, "message": "statement timeout" }
                }
            ]}]
        }]
    })
}

#[tokio::test]
async fn gigapipe_lists_traces_matching_the_operators_traceql() {
    let server = gigapipe_server(true).await;
    let now_nanos = Utc::now().timestamp_nanos_opt().unwrap();

    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "traces": [
                {
                    "traceID": "abc123",
                    "rootServiceName": "gateway",
                    "rootTraceName": "GET /checkout",
                    // The TraceQL route sends this as a string.
                    "startTimeUnixNano": now_nanos.to_string(),
                    "durationMs": 3400
                },
                {
                    "traceID": "def456",
                    "rootServiceName": "gateway",
                    "rootTraceName": "POST /pay",
                    // The tags route sends it as a number. Both must parse.
                    "startTimeUnixNano": now_nanos,
                    "durationMs": 120
                }
            ]
        })))
        .mount(&server)
        .await;

    let mut options = ProfileOptions::default();
    options.set("traceQuery", json!("{ duration > 100ms }"));
    options.set("traceSlowMs", json!(2000));

    let collector = build_collector(&gigapipe_profile(&server, options), None)
        .await
        .expect("collector builds");
    let snapshot = collector.collect().await.expect("collect succeeds");

    let traces = section(&snapshot, "traces");
    assert_eq!(traces.len(), 2);

    let slow = row(traces, "GET /checkout");
    assert_eq!(cell_number(slow, "duration"), 3400.0);
    assert_eq!(cell_text(slow, "service"), "gateway");
    assert_eq!(
        slow.health,
        Health::Degraded,
        "3.4s is past the declared 2s threshold"
    );

    // Under the threshold: listed, not judged.
    assert_eq!(row(traces, "POST /pay").health, Health::Unknown);

    assert_eq!(metric(&snapshot, "traces_slow"), 1.0);
    assert_eq!(snapshot.health, Health::Degraded);
}

#[tokio::test]
async fn gigapipe_without_a_threshold_lists_traces_without_judging_them() {
    // The operator's TraceQL already said what it wanted. Grading every row
    // again would raise an alert for exactly the traces they asked to see.
    let server = gigapipe_server(true).await;

    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "traces": [{
                "traceID": "abc123",
                "rootServiceName": "gateway",
                "rootTraceName": "GET /checkout",
                "startTimeUnixNano": Utc::now().timestamp_nanos_opt().unwrap(),
                "durationMs": 9000
            }]
        })))
        .mount(&server)
        .await;

    let mut options = ProfileOptions::default();
    options.set("traceQuery", json!("{ status = error }"));

    let collector = build_collector(&gigapipe_profile(&server, options), None)
        .await
        .expect("collector builds");
    let snapshot = collector.collect().await.expect("collect succeeds");

    assert_eq!(section(&snapshot, "traces")[0].health, Health::Unknown);
    assert_eq!(snapshot.health, Health::Healthy);
}

#[tokio::test]
async fn a_failing_trace_search_does_not_take_the_connection_down() {
    // A rejected TraceQL expression says something about the query, not about
    // whether the warehouse is healthy.
    let server = gigapipe_server(true).await;

    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "status": "error",
            "error": "traceql: unexpected token at position 4"
        })))
        .mount(&server)
        .await;

    let mut options = ProfileOptions::default();
    options.set("traceQuery", json!("bad query"));

    let collector = build_collector(&gigapipe_profile(&server, options), None)
        .await
        .expect("collector builds");
    let snapshot = collector.collect().await.expect("collect succeeds");

    assert_eq!(snapshot.health, Health::Healthy, "gigapipe itself is fine");
    let traces = snapshot
        .sections
        .iter()
        .find(|s| s.id == "traces")
        .expect("the section is still there");
    assert!(traces.rows.is_empty());
    assert!(
        traces.empty_text.contains("unexpected token"),
        "the operator needs gigapipe's own reason to fix the query: {:?}",
        traces.empty_text
    );
}

#[tokio::test]
async fn gigapipe_assembles_a_fetched_trace_into_a_tree() {
    let server = gigapipe_server(true).await;
    let base = Utc::now().timestamp_nanos_opt().unwrap() - 1_000_000_000;

    Mock::given(method("GET"))
        .and(path_regex(r"^/api/traces/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(gigapipe_trace_document(base)))
        .mount(&server)
        .await;

    let collector = build_collector(&gigapipe_profile(&server, ProfileOptions::default()), None)
        .await
        .expect("collector builds");

    let value = collector
        .detail(&DetailQuery::Trace {
            trace_id: "abc123".to_string(),
        })
        .await
        .expect("the trace comes back");

    let trace: inframonitoring_lib::traces::Trace =
        serde_json::from_value(value).expect("it is a Trace");

    assert_eq!(trace.span_count, 2);
    assert_eq!(trace.root_service, "gateway");
    assert_eq!(trace.error_count, 1);
    assert_eq!(trace.orphan_count, 0);

    // The tree, not the flat list the wire carried.
    assert_eq!(trace.roots.len(), 1);
    let root = &trace.roots[0];
    assert_eq!(root.span.name, "GET /checkout");
    assert_eq!(root.children.len(), 1);

    let child = &root.children[0];
    assert_eq!(child.span.service, "postgres");
    assert_eq!(child.span.status_message.as_deref(), Some("statement timeout"));
    assert_eq!(child.span.events.len(), 1);
    assert_eq!(child.span.events[0].name, "query.start");
    // 50ms after the trace started.
    assert!((child.offset_ms - 50.0).abs() < 1.0, "offset {}", child.offset_ms);

    // postgres held 430 of the root's 500ms, so the gateway's own time is 70.
    assert!((root.self_ms - 70.0).abs() < 1.0, "self {}", root.self_ms);
    assert_eq!(trace.services[0].service, "postgres");
}

#[tokio::test]
async fn a_trace_id_that_is_not_hex_is_refused_before_the_request() {
    // Gigapipe answers a non-hex id with a bare 500. Refusing it here is the
    // only place the operator can be told what is actually wrong.
    let server = gigapipe_server(true).await;
    let collector = build_collector(&gigapipe_profile(&server, ProfileOptions::default()), None)
        .await
        .expect("collector builds");

    let error = collector
        .detail(&DetailQuery::Trace {
            trace_id: "not a trace id".to_string(),
        })
        .await
        .expect_err("refused");

    assert!(error.to_string().contains("hexadecimal"), "{error}");
}

#[tokio::test]
async fn a_trace_outside_retention_says_so_rather_than_failing_opaquely() {
    let server = gigapipe_server(true).await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/api/traces/.+$"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not found"))
        .mount(&server)
        .await;

    let collector = build_collector(&gigapipe_profile(&server, ProfileOptions::default()), None)
        .await
        .expect("collector builds");

    let error = collector
        .detail(&DetailQuery::Trace {
            trace_id: "abc123".to_string(),
        })
        .await
        .expect_err("404 is an error here");

    assert!(error.to_string().contains("retention"), "{error}");
}
