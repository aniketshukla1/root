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
