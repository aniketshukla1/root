//! Snapshot write-out, pinned to the wire.
//!
//! The encoding is covered by unit tests in `influx.rs`; this covers the part
//! only a server can answer. Line protocol is a *body*, not a query string, so
//! a client that gets the path right and the body wrong produces a 204 from
//! some receivers and a silent drop from others — the failure mode is a
//! measurement that quietly never appears.
//!
//! So each test asserts what arrived at the server, byte for byte where that
//! matters, and the failure paths assert that a refused write stays a refused
//! write rather than becoming an opinion about the connection.
//!
//! No live server is needed and none of these skip.

use inframonitoring_lib::influx::{encode, write, SnapshotSink};
use inframonitoring_lib::model::{Health, HealthSnapshot, Metric, MetricUnit, ServiceKind};
use inframonitoring_lib::promquery::Auth;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn snapshot() -> HealthSnapshot {
    HealthSnapshot {
        profile_id: "p1".into(),
        profile_name: "prod-docker".into(),
        kind: ServiceKind::Docker,
        health: Health::Degraded,
        summary: "2 of 9 containers unhealthy".into(),
        metrics: vec![
            Metric::new("memory", "Memory", 9.4e9, MetricUnit::Bytes),
            Metric::new("containers_running", "Running", 7.0, MetricUnit::Count),
        ],
        sections: Vec::new(),
        checked_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        latency_ms: 42,
        error: None,
    }
}

fn sink() -> SnapshotSink {
    SnapshotSink {
        warehouse_profile_id: "p9".into(),
        measurement: String::new(),
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_snapshot_is_posted_as_line_protocol_to_the_influx_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .and(header("content-type", "text/plain; charset=utf-8"))
        .and(body_string_contains("prod-docker,profile=p1,kind=docker"))
        .and(body_string_contains("root_health=2i"))
        .and(body_string_contains("memory=9400000000"))
        .and(body_string_contains("1700000000000000000"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let line = encode(&snapshot(), &sink()).expect("a line");
    write(&client(), &server.uri(), &Auth::default(), line)
        .await
        .expect("the write");
}

#[tokio::test]
async fn the_body_is_exactly_one_line_with_no_trailing_newline_games() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let line = encode(&snapshot(), &sink()).unwrap();
    write(&client(), &server.uri(), &Auth::default(), line.clone())
        .await
        .unwrap();

    let received = &server.received_requests().await.unwrap()[0];
    let body = String::from_utf8(received.body.clone()).unwrap();
    assert_eq!(body, line);
    assert_eq!(body.lines().count(), 1, "{body}");
}

#[tokio::test]
async fn a_trailing_slash_on_the_endpoint_does_not_double_up_the_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let base = format!("{}/", server.uri());
    let line = encode(&snapshot(), &sink()).unwrap();
    write(&client(), &base, &Auth::default(), line).await.unwrap();
}

#[tokio::test]
async fn basic_auth_is_sent_when_the_warehouse_has_a_username() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        // "root:hunter2" base64-encoded. Asserted as the literal header so a
        // change to the scheme is a test failure rather than a 401 in the field.
        .and(header("authorization", "Basic cm9vdDpodW50ZXIy"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let auth = Auth {
        username: Some("root".into()),
        secret: Some("hunter2".into()),
    };
    let line = encode(&snapshot(), &sink()).unwrap();
    write(&client(), &server.uri(), &auth, line).await.unwrap();
}

#[tokio::test]
async fn a_secret_without_a_username_is_sent_as_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .and(header("authorization", "Bearer t0ken"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let auth = Auth {
        username: None,
        secret: Some("t0ken".into()),
    };
    let line = encode(&snapshot(), &sink()).unwrap();
    write(&client(), &server.uri(), &auth, line).await.unwrap();
}

#[tokio::test]
async fn an_unauthenticated_warehouse_gets_no_authorization_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let line = encode(&snapshot(), &sink()).unwrap();
    write(&client(), &server.uri(), &Auth::default(), line)
        .await
        .unwrap();

    let received = &server.received_requests().await.unwrap()[0];
    assert!(
        received.headers.get("authorization").is_none(),
        "an authorization header was sent for a warehouse with no credentials"
    );
}

#[tokio::test]
async fn a_two_hundred_is_accepted_as_well_as_a_two_oh_four() {
    // InfluxDB answers 204; gigapipe answers 200. Both are a stored write, and
    // treating one of them as a failure would log a warning every poll cycle
    // for a warehouse that is working perfectly.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let line = encode(&snapshot(), &sink()).unwrap();
    write(&client(), &server.uri(), &Auth::default(), line)
        .await
        .expect("200 is a success");
}

#[tokio::test]
async fn a_rejection_quotes_the_reason_the_warehouse_gave() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .respond_with(
            ResponseTemplate::new(400).set_body_string("unable to parse: missing field value"),
        )
        .mount(&server)
        .await;

    let line = encode(&snapshot(), &sink()).unwrap();
    let error = write(&client(), &server.uri(), &Auth::default(), line)
        .await
        .expect_err("a 400 is a failure");
    let message = error.to_string();
    assert!(message.contains("400"), "{message}");
    assert!(message.contains("unable to parse"), "{message}");
}

#[tokio::test]
async fn a_wall_of_html_in_a_rejection_is_truncated_rather_than_logged_whole() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .respond_with(ResponseTemplate::new(502).set_body_string("x".repeat(50_000)))
        .mount(&server)
        .await;

    let line = encode(&snapshot(), &sink()).unwrap();
    let message = write(&client(), &server.uri(), &Auth::default(), line)
        .await
        .expect_err("a 502 is a failure")
        .to_string();
    assert!(message.contains("502"), "{message}");
    assert!(message.len() < 700, "error was {} chars", message.len());
}

#[tokio::test]
async fn an_unreachable_warehouse_is_an_error_and_not_a_panic() {
    // Port 1 is reserved and never listening. The point is that the failure
    // arrives as a value the caller logs, because the caller is a poll loop.
    let line = encode(&snapshot(), &sink()).unwrap();
    let error = write(&client(), "http://127.0.0.1:1", &Auth::default(), line)
        .await
        .expect_err("nothing is listening");
    assert!(
        error.to_string().contains("did not accept the write"),
        "{error}"
    );
}

#[tokio::test]
async fn a_metric_key_with_a_space_survives_the_round_trip_unambiguously() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/influx/api/v2/write"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let mut snap = snapshot();
    snap.metrics = vec![Metric::new("disk free", "Disk free", 1024.0, MetricUnit::Bytes)];
    let line = encode(&snap, &sink()).unwrap();
    write(&client(), &server.uri(), &Auth::default(), line)
        .await
        .unwrap();

    let received: &Request = &server.received_requests().await.unwrap()[0];
    let body = String::from_utf8(received.body.clone()).unwrap();
    assert!(body.contains(r"disk\ free=1024"), "{body}");
    assert!(!body.contains("disk free"), "the space was left bare: {body}");
    // A bare space would end the field set early and the receiver would read
    // `free=1024` as the timestamp. The timestamp is still the last token.
    assert_eq!(
        body.rsplit(' ').next().unwrap(),
        "1700000000000000000",
        "{body}"
    );
}
