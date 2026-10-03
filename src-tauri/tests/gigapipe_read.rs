//! The nine gigapipe read APIs, pinned to the wire.
//!
//! These exist because the whole surface is URLs. Every one of these calls is
//! a path and a query string, and getting either subtly wrong — `/label`
//! versus `/labels`, a missing `/json` suffix, a parameter sent empty rather
//! than omitted — produces an empty result and no error at all. A store that
//! answers "no labels" looks exactly like a client asking the wrong question.
//!
//! So each tool gets a mock that asserts the exact method, path and parameters
//! it must send, and fails the test if anything else arrives. Wiremock fails a
//! mock whose `expect` is unmet on drop, so an endpoint that was never called
//! is a failure rather than a silent pass.
//!
//! No live server is needed and none of these skip.

use inframonitoring_lib::collectors::{Collector, DetailQuery, GigapipeRead};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::model::ServiceKind;
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn collector(endpoint: String) -> Box<dyn Collector> {
    let profile = ConnectionProfile {
        id: "gp".to_string(),
        name: "warehouse".to_string(),
        kind: ServiceKind::Gigapipe,
        endpoint,
        username: None,
        has_secret: false,
        enabled: true,
        poll_interval_secs: 30,
        options: ProfileOptions::default(),
        depends_on: Vec::new(),
        config_sources: Vec::new(),
        history_source: None,
        snapshot_sink: None,
        simulated: false,
        runtime: Default::default(),
        metadata: ProfileOptions::default(),
    };
    Box::new(
        inframonitoring_lib::collectors::gigapipe::GigapipeCollector::new(&profile, None)
            .expect("the collector should build from a plain http endpoint"),
    )
}

async fn read(server: &MockServer, query: GigapipeRead) -> serde_json::Value {
    collector(server.uri())
        .detail(&DetailQuery::GigapipeRead(query))
        .await
        .expect("the read should succeed against the mock")
}

/// The `{status, data}` envelope every one of these APIs answers with.
fn ok(data: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "status": "success", "data": data }))
}

// ------------------------------------------------------------- Prometheus

#[tokio::test]
async fn a_promql_range_query_sends_query_start_end_and_step() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query_range"))
        .and(query_param("query", "rate(http_requests_total[5m])"))
        .and(query_param("start", "2024-01-01T00:00:00Z"))
        .and(query_param("end", "2024-01-01T01:00:00Z"))
        .and(query_param("step", "1m"))
        .respond_with(ok(json!({ "resultType": "matrix", "result": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(
        &server,
        GigapipeRead::PrometheusQuery {
            query: "rate(http_requests_total[5m])".to_string(),
            start: Some("2024-01-01T00:00:00Z".to_string()),
            end: Some("2024-01-01T01:00:00Z".to_string()),
            step: Some("1m".to_string()),
        },
    )
    .await;

    assert_eq!(value["status"], "success");
    assert_eq!(value["data"]["resultType"], "matrix");
}

/// An unset parameter must be absent, not present and empty. Sending
/// `start=` is a different request from sending no `start`, and only the
/// latter lets the store apply its own default.
#[tokio::test]
async fn omitted_parameters_are_left_off_the_url_entirely() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query_range"))
        .and(query_param("query", "up"))
        .and(query_param_is_missing("start"))
        .and(query_param_is_missing("end"))
        .and(query_param_is_missing("step"))
        .respond_with(ok(json!({ "resultType": "vector", "result": [] })))
        .expect(1)
        .mount(&server)
        .await;

    read(
        &server,
        GigapipeRead::PrometheusQuery {
            query: "up".to_string(),
            start: None,
            // Whitespace is the same as absent: a model that fills a field it
            // was told is optional should not change the request.
            end: Some("   ".to_string()),
            step: None,
        },
    )
    .await;
}

#[tokio::test]
async fn prometheus_labels_reads_the_plural_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/labels"))
        .respond_with(ok(json!(["__name__", "instance", "job"])))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(
        &server,
        GigapipeRead::PrometheusLabels {
            start: None,
            end: None,
        },
    )
    .await;
    assert_eq!(value["data"][1], "instance");
}

#[tokio::test]
async fn prometheus_label_values_puts_the_label_in_the_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/label/instance/values"))
        .and(query_param("start", "1704067200"))
        .respond_with(ok(json!(["web-1:9100"])))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(
        &server,
        GigapipeRead::PrometheusLabelValues {
            label: "instance".to_string(),
            start: Some("1704067200".to_string()),
            end: None,
        },
    )
    .await;
    assert_eq!(value["data"][0], "web-1:9100");
}

// -------------------------------------------------------------------- Loki

#[tokio::test]
async fn a_logql_query_sends_query_and_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .and(query_param("query", "{job=\"varlogs\"}"))
        .and(query_param("limit", "100"))
        .respond_with(ok(json!({ "resultType": "streams", "result": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(
        &server,
        GigapipeRead::LokiQuery {
            query: "{job=\"varlogs\"}".to_string(),
            start: None,
            end: None,
            limit: Some("100".to_string()),
        },
    )
    .await;
    assert_eq!(value["data"]["resultType"], "streams");
}

/// Loki's label list is `/label`, singular, while Prometheus' is `/labels`.
/// They sit one character apart on the same server and mixing them up returns
/// a 404 that reads like an empty store.
#[tokio::test]
async fn loki_labels_reads_the_singular_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/label"))
        .respond_with(ok(json!(["job", "namespace"])))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(
        &server,
        GigapipeRead::LokiLabels {
            start: None,
            end: None,
        },
    )
    .await;
    assert_eq!(value["data"][0], "job");
}

#[tokio::test]
async fn loki_label_values_puts_the_label_in_the_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/label/job/values"))
        .respond_with(ok(json!(["varlogs"])))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(
        &server,
        GigapipeRead::LokiLabelValues {
            label: "job".to_string(),
            start: None,
            end: None,
        },
    )
    .await;
    assert_eq!(value["data"][0], "varlogs");
}

// ------------------------------------------------------------------- Tempo

/// The trace read is `/api/traces/{id}/json`. Without the suffix the store
/// answers protobuf, which is not what anything here can read.
#[tokio::test]
async fn a_trace_is_read_from_the_json_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/traces/1234567890abcdef/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "batches": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(
        &server,
        GigapipeRead::TempoTrace {
            trace_id: "1234567890abcdef".to_string(),
        },
    )
    .await;
    assert!(value["batches"].is_array());
}

#[tokio::test]
async fn tempo_tags_takes_no_parameters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tagNames": ["service.name", "http.status_code"]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(&server, GigapipeRead::TempoTags).await;
    assert_eq!(value["tagNames"][0], "service.name");
}

/// A tag name is caller-supplied text that lands in a URL path, so it is
/// percent-encoded. `service.name` must survive intact — `.` is unreserved —
/// while a slash must not be allowed to address a different resource.
#[tokio::test]
async fn a_tag_name_reaches_the_path_encoded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search/tag/service.name/values"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "tagValues": ["checkout"] })))
        .expect(1)
        .mount(&server)
        .await;

    let value = read(
        &server,
        GigapipeRead::TempoTagValues {
            tag: "service.name".to_string(),
        },
    )
    .await;
    assert_eq!(value["tagValues"][0], "checkout");
}

#[tokio::test]
async fn a_slash_in_a_tag_cannot_escape_its_path_segment() {
    let server = MockServer::start().await;
    // The encoded form is what must arrive. If the slash went through raw the
    // request would address /api/search/tag/a/b/values, which this mock does
    // not answer, and the read would fail instead of silently hitting it.
    Mock::given(method("GET"))
        .and(path("/api/search/tag/a%2Fb/values"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "tagValues": [] })))
        .expect(1)
        .mount(&server)
        .await;

    read(
        &server,
        GigapipeRead::TempoTagValues {
            tag: "a/b".to_string(),
        },
    )
    .await;
}

// ------------------------------------------------------------------ Errors

/// Gigapipe puts the reason a query was rejected in the body. Reducing that to
/// a status code throws away the only part an operator can act on, so the
/// message has to carry it.
#[tokio::test]
async fn a_rejected_query_surfaces_the_stores_own_reason() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .respond_with(
            ResponseTemplate::new(400).set_body_string("parse error at line 1: unexpected }"),
        )
        .mount(&server)
        .await;

    let error = collector(server.uri())
        .detail(&DetailQuery::GigapipeRead(GigapipeRead::LokiQuery {
            query: "{".to_string(),
            start: None,
            end: None,
            limit: None,
        }))
        .await
        .expect_err("a 400 must be an error, not an empty result");

    let text = error.to_string();
    assert!(text.contains("400"), "status missing from: {text}");
    assert!(text.contains("parse error"), "reason missing from: {text}");
}

/// A non-gigapipe collector must refuse these rather than pretending, so the
/// assistant gets told it aimed at the wrong connection.
#[tokio::test]
async fn a_store_read_against_a_non_gigapipe_connection_is_refused() {
    let server = MockServer::start().await;
    let profile = ConnectionProfile {
        id: "nginx".to_string(),
        name: "proxy".to_string(),
        kind: ServiceKind::Nginx,
        endpoint: server.uri(),
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
    };
    let collector = inframonitoring_lib::collectors::nginx::NginxCollector::new(&profile, None)
        .expect("the nginx collector should build");

    collector
        .detail(&DetailQuery::GigapipeRead(GigapipeRead::TempoTags))
        .await
        .expect_err("a store read is not something nginx can answer");
}
