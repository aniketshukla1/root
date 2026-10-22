//! The flame graph read, pinned to the wire.
//!
//! Two things here can only be got wrong against a server.
//!
//! The first is the timestamps. `/pyroscope/render` parses `from` and `until`
//! as an integer and divides by a thousand — they are **milliseconds**. Send
//! seconds and the store answers an empty profile for a window in 1970, with
//! a 200 and no error anywhere. Nothing downstream can tell that from a
//! service that was genuinely idle, so it is asserted on the URL.
//!
//! The second is that all three parameters are required, and a missing one is
//! a 400 whose body names it. That message is the only part an operator can
//! act on, so it has to survive into root's error.
//!
//! The decoding itself is covered by unit tests in `profiles.rs`; what is
//! checked here is that a body shaped the way gigapipe shapes one arrives at
//! the decoder intact.
//!
//! No live server is needed and none of these skip.

use inframonitoring_lib::collectors::{build_collector, Collector, DetailQuery};
use inframonitoring_lib::config::{ConnectionProfile, ProfileOptions};
use inframonitoring_lib::model::ServiceKind;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SELECTOR: &str = "process_cpu:cpu:nanoseconds:cpu:nanoseconds{service_name=\"checkout\"}";

async fn collector(endpoint: String, declared_query: Option<&str>) -> std::sync::Arc<dyn Collector> {
    let mut options = ProfileOptions::default();
    if let Some(q) = declared_query {
        options.set("profileQuery", json!(q));
    }
    let profile = ConnectionProfile {
        id: "p9".into(),
        name: "warehouse".into(),
        kind: ServiceKind::Gigapipe,
        endpoint,
        username: None,
        has_secret: false,
        enabled: true,
        poll_interval_secs: 30,
        options,
        depends_on: Vec::new(),
        config_sources: Vec::new(),
        log_sources: Vec::new(),
        history_source: None,
        snapshot_sink: None,
        simulated: false,
        runtime: Default::default(),
        metadata: Default::default(),
    };
    build_collector(&profile, None).await.expect("a collector")
}

/// total(100) → [serve(0..60) self 20, flush(60..100) self 40],
/// serve → [parse(10..50) self 40]. Offsets on the wire are deltas.
fn flamebearer() -> serde_json::Value {
    json!({
        "flamebearer": {
            "names": ["total", "serve", "flush", "parse"],
            "levels": [
                [0, 100, 0, 0],
                [0, 60, 20, 1, 0, 40, 40, 2],
                [10, 40, 40, 3],
            ],
            "numTicks": 100,
            "maxSelf": 40,
        },
        "metadata": {
            "format": "single",
            "units": "samples",
            "sampleRate": 1000000000,
            "name": "cpu",
            "spyName": "gospy",
        },
        // gigapipe's render path leaves this null. Root must not need it.
        "timeline": null,
    })
}

async fn profile(server: &MockServer, query: &str) -> serde_json::Value {
    let from = chrono::Utc::now() - chrono::Duration::minutes(60);
    let to = chrono::Utc::now();
    collector(server.uri(), Some(SELECTOR))
        .await
        .detail(&DetailQuery::Profile {
            query: query.to_string(),
            from,
            to,
        })
        .await
        .expect("a profile")
}

#[tokio::test]
async fn the_window_is_sent_in_milliseconds_and_not_seconds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .respond_with(ResponseTemplate::new(200).set_body_json(flamebearer()))
        .expect(1)
        .mount(&server)
        .await;

    let to = chrono::Utc::now();
    let from = to - chrono::Duration::minutes(60);
    collector(server.uri(), None)
        .await
        .detail(&DetailQuery::Profile {
            query: SELECTOR.into(),
            from,
            to,
        })
        .await
        .expect("a profile");

    let url = &server.received_requests().await.unwrap()[0].url;
    let sent: std::collections::HashMap<_, _> = url.query_pairs().collect();
    let from_param: i64 = sent["from"].parse().expect("an integer");
    let until_param: i64 = sent["until"].parse().expect("an integer");

    assert_eq!(from_param, from.timestamp_millis());
    assert_eq!(until_param, to.timestamp_millis());
    // The window is an hour. In milliseconds that is 3,600,000; in seconds it
    // would be 3,600 — which the store would read as an hour in 1970.
    assert_eq!(until_param - from_param, 3_600_000);
}

#[tokio::test]
async fn the_selector_and_a_node_cap_travel_with_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .and(query_param("query", SELECTOR))
        .and(query_param("maxNodes", "8192"))
        .respond_with(ResponseTemplate::new(200).set_body_json(flamebearer()))
        .expect(1)
        .mount(&server)
        .await;

    profile(&server, SELECTOR).await;
}

#[tokio::test]
async fn an_empty_query_falls_back_to_the_one_the_connection_declared() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .and(query_param("query", SELECTOR))
        .respond_with(ResponseTemplate::new(200).set_body_json(flamebearer()))
        .expect(1)
        .mount(&server)
        .await;

    profile(&server, "").await;
}

#[tokio::test]
async fn a_connection_that_declares_nothing_says_so_rather_than_guessing_a_type() {
    // No request at all: there is no default profile type to invent, and
    // picking one would draw a graph of something nobody asked about.
    let server = MockServer::start().await;
    let error = collector(server.uri(), None)
        .await
        .detail(&DetailQuery::Profile {
            query: String::new(),
            from: chrono::Utc::now() - chrono::Duration::minutes(60),
            to: chrono::Utc::now(),
        })
        .await
        .expect_err("nothing to ask for");
    assert!(error.to_string().contains("declares no profile query"), "{error}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_frames_come_back_positioned_and_ranked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .respond_with(ResponseTemplate::new(200).set_body_json(flamebearer()))
        .mount(&server)
        .await;

    let value = profile(&server, SELECTOR).await;
    let frames = value["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 4);

    let at = |name: &str| {
        frames
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("no frame named {name}"))
            .clone()
    };
    // The wire says flush begins 0 after serve ends. Absolute, that is 60.
    assert_eq!(at("flush")["offset"], 60);
    assert_eq!(at("parse")["offset"], 10);
    assert_eq!(at("parse")["depth"], 2);

    assert_eq!(value["total"], 100);
    assert_eq!(value["unit"], "samples");
    assert_eq!(value["name"], "cpu");
    assert_eq!(value["spy"], "gospy");
    assert_eq!(value["hot"][0]["name"], "flush");
    assert_eq!(value["hot"][0]["own"], 40);
}

#[tokio::test]
async fn a_store_holding_nothing_answers_an_empty_profile_rather_than_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "flamebearer": { "names": [], "levels": [], "numTicks": 0, "maxSelf": 0 },
            "metadata": { "format": "single", "units": "samples", "sampleRate": 100, "name": "cpu" },
        })))
        .mount(&server)
        .await;

    let value = profile(&server, SELECTOR).await;
    assert_eq!(value["frames"].as_array().unwrap().len(), 0);
    assert_eq!(value["total"], 0);
}

#[tokio::test]
async fn the_reason_a_render_was_refused_survives_into_the_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .respond_with(
            ResponseTemplate::new(400).set_body_string("Missing required parameter: query"),
        )
        .mount(&server)
        .await;

    let error = collector(server.uri(), Some(SELECTOR))
        .await
        .detail(&DetailQuery::Profile {
            query: SELECTOR.into(),
            from: chrono::Utc::now() - chrono::Duration::minutes(60),
            to: chrono::Utc::now(),
        })
        .await
        .expect_err("a 400");
    let message = error.to_string();
    assert!(message.contains("400"), "{message}");
    assert!(message.contains("Missing required parameter"), "{message}");
}

#[tokio::test]
async fn a_body_that_is_not_a_flame_graph_says_that_rather_than_drawing_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pyroscope/render"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "success" })))
        .mount(&server)
        .await;

    let error = collector(server.uri(), Some(SELECTOR))
        .await
        .detail(&DetailQuery::Profile {
            query: SELECTOR.into(),
            from: chrono::Utc::now() - chrono::Duration::minutes(60),
            to: chrono::Utc::now(),
        })
        .await
        .expect_err("not a flame graph");
    assert!(
        error.to_string().contains("not a flame graph"),
        "{error}"
    );
}

#[tokio::test]
async fn a_collector_without_profiles_refuses_the_query_instead_of_answering_it() {
    // Every collector is asked this; only the ones that hold profiles answer.
    // A Docker host returning an empty graph would be a lie about the estate.
    let profile = ConnectionProfile {
        id: "p1".into(),
        name: "prod-docker".into(),
        kind: ServiceKind::HttpEndpoint,
        endpoint: "https://status.example".into(),
        username: None,
        has_secret: false,
        enabled: true,
        poll_interval_secs: 30,
        options: ProfileOptions::default(),
        depends_on: Vec::new(),
        config_sources: Vec::new(),
        log_sources: Vec::new(),
        history_source: None,
        snapshot_sink: None,
        simulated: false,
        runtime: Default::default(),
        metadata: Default::default(),
    };
    let error = build_collector(&profile, None)
        .await
        .expect("a collector")
        .detail(&DetailQuery::Profile {
            query: SELECTOR.into(),
            from: chrono::Utc::now() - chrono::Duration::minutes(60),
            to: chrono::Utc::now(),
        })
        .await
        .expect_err("no profiles here");
    assert!(
        error.to_string().contains("not supported"),
        "{error}"
    );
}
