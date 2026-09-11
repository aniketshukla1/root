//! Long-range history against a Prometheus HTTP API.
//!
//! Two layers. The wiremock tests pin the wire contract and always run. The
//! `live_*` tests at the bottom talk to a real Prometheus and are `#[ignore]`d
//! so a checkout without one still passes — run them with:
//!
//! ```text
//! PROMETHEUS_URL=http://localhost:9090 cargo test --test promquery -- --ignored --nocapture
//! ```
//!
//! Both layers exist because they catch different things. Wiremock proves the
//! parser handles the shapes Prometheus documents. A real server proves those
//! are the shapes it actually sends — which is where the quoted-integer
//! encoding came from, and it is not obvious from the docs.

use chrono::{Duration, Utc};
use inframonitoring_lib::promquery::{range, step_for, Auth};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}

const MATRIX: &str = r#"{
  "status":"success",
  "data":{
    "resultType":"matrix",
    "result":[{
      "metric":{"__name__":"redis_memory_used_bytes","instance":"10.0.0.2:9121"},
      "values":[[1789095229,"1073741824"],[1789095289,"1181116006"],[1789095349,"1288490188"]]
    }]
  }
}"#;

#[tokio::test]
async fn a_range_query_returns_points_in_order() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query_range"))
        .and(query_param("query", "redis_memory_used_bytes"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MATRIX, "application/json"))
        .mount(&server)
        .await;

    let end = Utc::now();
    let points = range(
        &client(),
        &server.uri(),
        &Auth::default(),
        "redis_memory_used_bytes",
        end - Duration::hours(1),
        end,
    )
    .await
    .expect("a range");

    assert_eq!(points.len(), 3);
    assert_eq!(points[0].v, 1_073_741_824.0);
    assert_eq!(points[2].v, 1_288_490_188.0);
    assert!(points[0].t < points[2].t);
}

/// The mapping is per metric key, so a query matching several series has no
/// single number to forecast. Taking the first would produce a confident
/// projection for some other host.
#[tokio::test]
async fn an_ambiguous_query_is_refused_and_names_what_it_matched() {
    let body = r#"{
      "status":"success",
      "data":{"resultType":"matrix","result":[
        {"metric":{"__name__":"m","instance":"10.0.0.2:9121"},"values":[[1,"1"]]},
        {"metric":{"__name__":"m","instance":"10.0.0.3:9121"},"values":[[1,"2"]]}
      ]}
    }"#;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    let end = Utc::now();
    let error = range(
        &client(),
        &server.uri(),
        &Auth::default(),
        "m",
        end - Duration::hours(1),
        end,
    )
    .await
    .expect_err("two series is not an answer");

    let text = error.to_string();
    assert!(text.contains("2 series"), "{text}");
    assert!(text.contains("10.0.0.2:9121"), "names the labels: {text}");
    assert!(text.contains("10.0.0.3:9121"), "{text}");
}

/// Prometheus puts the useful part of a 400 in the body. The status line says
/// only that something was wrong; the body says which token.
#[tokio::test]
async fn a_bad_expression_surfaces_prometheus_own_message() {
    let body = r#"{"status":"error","errorType":"bad_data","error":"1:8: parse error: unexpected \"{\""}"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    let end = Utc::now();
    let error = range(
        &client(),
        &server.uri(),
        &Auth::default(),
        "nonsense{",
        end - Duration::hours(1),
        end,
    )
    .await
    .expect_err("a parse error");

    assert!(error.to_string().contains("parse error"), "{error}");
}

/// An endpoint that answers but is not Prometheus — a dashboard, a proxy, a
/// login page — must say so rather than reporting "no data".
#[tokio::test]
async fn a_non_prometheus_endpoint_is_named_as_the_problem() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<html>hello</html>", "text/html"))
        .mount(&server)
        .await;

    let end = Utc::now();
    let error = range(
        &client(),
        &server.uri(),
        &Auth::default(),
        "up",
        end - Duration::hours(1),
        end,
    )
    .await
    .expect_err("html is not a response");

    assert!(error.to_string().contains("not a Prometheus response"), "{error}");
}

/// A metric that exists but has no samples in the window is empty, not an
/// error: a disk added yesterday has no history from last month, and that is
/// a fact rather than a fault.
#[tokio::test]
async fn an_empty_result_is_not_an_error() {
    let body = r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(&server)
        .await;

    let end = Utc::now();
    let points = range(
        &client(),
        &server.uri(),
        &Auth::default(),
        "up",
        end - Duration::hours(1),
        end,
    )
    .await
    .expect("empty is a result");
    assert!(points.is_empty());
}

/// A month at fifteen seconds is 172,800 points and a rejected query. The step
/// must widen with the range before the request is sent.
#[tokio::test]
async fn a_long_range_widens_its_step_before_asking() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("step", step_for(30 * 86_400).to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MATRIX, "application/json"))
        .mount(&server)
        .await;

    let end = Utc::now();
    // Mounted with the step as a matcher: a wrong step gets no mock and fails.
    range(
        &client(),
        &server.uri(),
        &Auth::default(),
        "redis_memory_used_bytes",
        end - Duration::days(30),
        end,
    )
    .await
    .expect("a month should be askable");
}

// ---------------------------------------------------------------------- live

/// Against a real server. Ignored by default; see the module note.
#[tokio::test]
#[ignore]
async fn live_reads_a_real_range() {
    let base = std::env::var("PROMETHEUS_URL").expect("set PROMETHEUS_URL");
    let end = Utc::now();

    let points = range(
        &client(),
        &base,
        &Auth::default(),
        // Every Prometheus scrapes itself, so this exists on any of them.
        "go_memstats_heap_inuse_bytes{job=\"prometheus\"}",
        end - Duration::hours(2),
        end,
    )
    .await
    .expect("a real range");

    assert!(!points.is_empty(), "a live server should have samples");
    assert!(points.iter().all(|p| p.v.is_finite()));
    assert!(
        points.windows(2).all(|w| w[0].t <= w[1].t),
        "samples must come back in time order"
    );
    println!(
        "live: {} points, {} → {}, first {:.0}, last {:.0}",
        points.len(),
        points[0].t,
        points[points.len() - 1].t,
        points[0].v,
        points[points.len() - 1].v,
    );
}

/// The whole reason this module exists: a window the in-memory ring cannot
/// reach, producing a fit the short window could not.
#[tokio::test]
#[ignore]
async fn live_a_long_window_produces_a_trend() {
    let base = std::env::var("PROMETHEUS_URL").expect("set PROMETHEUS_URL");
    let end = Utc::now();

    let points = range(
        &client(),
        &base,
        &Auth::default(),
        "prometheus_tsdb_head_series",
        end - Duration::days(7),
        end,
    )
    .await
    .expect("a week");

    assert!(!points.is_empty());
    let span = (points[points.len() - 1].t - points[0].t).num_seconds();
    println!("live: {} points spanning {}h", points.len(), span / 3600);

    // Far past root's own ring buffer, which is the claim being tested.
    assert!(span > 6 * 3600, "expected a window longer than the ring holds");

    let trend = inframonitoring_lib::forecast::trend(&points, None);
    println!("live trend: {trend:?}");
    assert!(trend.is_some(), "a week of samples should fit something");
}

/// A real server's own rejection, not a mocked one.
#[tokio::test]
#[ignore]
async fn live_rejects_a_query_matching_many_series() {
    let base = std::env::var("PROMETHEUS_URL").expect("set PROMETHEUS_URL");
    let end = Utc::now();

    // `up` is one series per scrape target, so any real server has several.
    let result = range(
        &client(),
        &base,
        &Auth::default(),
        "up",
        end - Duration::hours(1),
        end,
    )
    .await;

    match result {
        Err(e) => {
            println!("live refusal: {e}");
            assert!(e.to_string().contains("series"), "{e}");
        }
        Ok(points) => {
            // A server scraping exactly one target is legitimately unambiguous.
            println!("this server has a single `up` series: {} points", points.len());
        }
    }
}
