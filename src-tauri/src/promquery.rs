//! Long-range history, read from a Prometheus the operator already runs.
//!
//! root's own history is a ring buffer holding a few hours — deliberately, so
//! there is no retention policy to manage. That is fine for a chart and wrong
//! for a forecast. `forecast.rs` refuses to project on a window shorter than
//! fifteen minutes and a few hours is not much better: a volume that fills in
//! three weeks is invisible to it, and the refusal is honest but useless. The
//! answer exists, root just cannot see far enough back to find it.
//!
//! Most estates already have somewhere that does: a Prometheus with weeks of
//! retention. This reads from it.
//!
//! ## Anything that speaks the same API
//!
//! Nothing below is Prometheus-specific beyond the endpoint shape. Gigapipe
//! serves PromQL at the same `/api/v1/query_range`, so a gigapipe connection
//! is a history source here with no code change — which matters, because a
//! ClickHouse-backed store holds years where a Prometheus holds weeks.
//!
//! ## A source, not a store
//!
//! Prometheus is read here and never written. root's own judgement record
//! lives in `journal.rs`, in a local file, for reasons that module explains —
//! the short version being that Prometheus stores `float64` samples against
//! label sets, expires them by design, and is itself monitored infrastructure.
//! This module only asks it for the one thing it is excellent at: what a
//! number did over a long stretch of time.
//!
//! ## The mapping is declared
//!
//! root's `memory` metric and Prometheus' `redis_memory_used_bytes` are not
//! the same series and nothing can infer that they correspond. So an operator
//! writes the PromQL, per metric, on the connection — the same stance as
//! `depends_on` and config sources. A guess here would silently forecast the
//! wrong number, which is worse than forecasting nothing.
//!
//! ## Refusing an ambiguous answer
//!
//! A query that returns more than one series has not been narrowed enough —
//! `redis_memory_used_bytes` without an instance selector matches every Redis
//! being scraped. Picking the first would produce a forecast for some other
//! host, correct-looking and wrong, so this returns an error naming the label
//! sets it found instead.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::history::Point;

/// Prometheus refuses a query resolving to more than 11,000 points. The step
/// is derived from the range so a long window widens the step rather than
/// being rejected.
const MAX_POINTS: i64 = 11_000;

/// Finest step worth asking for. Below this a long window returns far more
/// detail than a trend line needs, at real cost to the server being queried.
const MIN_STEP_SECS: i64 = 15;

/// Series a single query may return before it is called ambiguous.
const MAX_SERIES: usize = 1;

/// Where a connection's long-range history lives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HistorySource {
    /// The connection that holds it, as a profile id. A Prometheus or a
    /// Gigapipe — the field name is the original one, kept so existing
    /// configs keep resolving.
    pub prometheus_profile_id: String,
    /// How root's metric keys map onto PromQL. Declared, never inferred.
    #[serde(default)]
    pub queries: Vec<MetricQuery>,
}

impl HistorySource {
    pub fn is_configured(&self) -> bool {
        !self.prometheus_profile_id.trim().is_empty() && !self.queries.is_empty()
    }

    pub fn expr_for(&self, metric_key: &str) -> Option<&str> {
        self.queries
            .iter()
            .find(|q| q.metric_key == metric_key)
            .map(|q| q.expr.as_str())
    }
}

/// One metric key, and the PromQL that produces it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricQuery {
    /// root's own metric key, as it appears in a snapshot — `memory`, `cpu`.
    pub metric_key: String,
    /// A PromQL expression that resolves to exactly one series.
    pub expr: String,
}

/// Credentials for the Prometheus being queried.
#[derive(Debug, Clone, Default)]
pub struct Auth {
    pub username: Option<String>,
    pub secret: Option<String>,
}

/// The step that keeps a range under Prometheus' point ceiling.
///
/// Returned in seconds. A month at fifteen seconds would be 172,800 points and
/// a rejected query; this widens until it fits, which is exactly the right
/// trade for a trend line — a fit does not get better from more samples of the
/// same slope.
pub fn step_for(range_secs: i64) -> i64 {
    let minimum = (range_secs / MAX_POINTS) + 1;
    minimum.max(MIN_STEP_SECS)
}

/// Run a range query and return the raw matrix.
async fn matrix(
    client: &reqwest::Client,
    base: &str,
    auth: &Auth,
    expr: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<SeriesResult>> {
    let range_secs = (end - start).num_seconds().max(1);
    let step = step_for(range_secs);

    let url = format!("{}/api/v1/query_range", base.trim_end_matches('/'));
    let request = client.get(&url).query(&[
        ("query", expr.to_string()),
        ("start", start.timestamp().to_string()),
        ("end", end.timestamp().to_string()),
        ("step", step.to_string()),
    ]);

    let request = match (&auth.username, &auth.secret) {
        (Some(user), secret) => request.basic_auth(user, secret.as_ref()),
        (None, Some(token)) => request.bearer_auth(token),
        (None, None) => request,
    };

    let response = request
        .send()
        .await
        .map_err(|e| AppError::Collector(format!("{base} did not answer: {e}")))?;

    // Prometheus returns a JSON body with the error message on a 400, which is
    // far more useful than the status line — a bad PromQL expression says
    // exactly what is wrong with it.
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AppError::Collector(format!("{base} response unreadable: {e}")))?;

    let parsed: ApiResponse = serde_json::from_str(&body).map_err(|_| {
        AppError::Collector(format!(
            "{base} returned {status} and something that is not a Prometheus response — \
             check the endpoint points at Prometheus itself and not a proxy or a dashboard"
        ))
    })?;

    if parsed.status != "success" {
        return Err(AppError::Collector(
            parsed
                .error
                .unwrap_or_else(|| format!("query failed with {status}")),
        ));
    }

    let data = parsed
        .data
        .ok_or_else(|| AppError::Collector("no data in the response".to_string()))?;

    Ok(data.result)
}

/// Fetch one metric's history over `[start, end]`.
pub async fn range(
    client: &reqwest::Client,
    base: &str,
    auth: &Auth,
    expr: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<Point>> {
    let result = matrix(client, base, auth, expr, start, end).await?;
    if result.is_empty() {
        return Ok(Vec::new());
    }
    if result.len() > MAX_SERIES {
        // Naming the label sets is the whole value of this error: it shows the
        // operator which dimension they forgot to pin.
        let found = result
            .iter()
            .take(4)
            .map(|s| render_labels(&s.metric))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::Collector(format!(
            "that query matches {} series, so there is no single number to forecast — \
             narrow it with a label selector. Found: {found}",
            result.len()
        )));
    }
    Ok(result[0].values.iter().filter_map(|sample| sample.point()).collect())
}

/// Every series a query matches, each with its label set, up to `cap` — what
/// a dashboard panel or Explore draws, where many lines is the point.
pub async fn range_all(
    client: &reqwest::Client,
    base: &str,
    auth: &Auth,
    expr: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    cap: usize,
) -> Result<Vec<(String, Vec<Point>)>> {
    let result = matrix(client, base, auth, expr, start, end).await?;
    Ok(result
        .iter()
        .take(cap)
        .map(|s| {
            (
                render_labels(&s.metric),
                s.values.iter().filter_map(|sample| sample.point()).collect(),
            )
        })
        .collect())
}

fn render_labels(labels: &std::collections::HashMap<String, String>) -> String {
    let mut pairs: Vec<String> = labels
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__")
        .map(|(k, v)| format!("{k}=\"{v}\""))
        .collect();
    pairs.sort();
    format!("{{{}}}", pairs.join(","))
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    status: String,
    data: Option<RangeData>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RangeData {
    #[serde(default)]
    result: Vec<SeriesResult>,
}

#[derive(Debug, Deserialize)]
struct SeriesResult {
    #[serde(default)]
    metric: std::collections::HashMap<String, String>,
    #[serde(default)]
    values: Vec<Sample>,
}

/// `[1789095229, "32309248"]` — a float timestamp and a **string** value.
///
/// The quoting is not incidental: Prometheus sends values as strings so a
/// 64-bit integer survives a JSON parser that would otherwise round it to a
/// double. Reading it as a number is the same class of bug as reading
/// ClickHouse's quoted integers as numbers, and it fails the same way — the
/// deserialiser rejects the whole response and the feature reports "no data"
/// on a server that is answering perfectly.
#[derive(Debug, Deserialize)]
struct Sample(f64, String);

impl Sample {
    fn point(&self) -> Option<Point> {
        // `NaN`, `+Inf` and `-Inf` all arrive as strings that Rust's float
        // parser accepts happily. A gap in a series is not a reading, and
        // letting one through puts a non-finite value into a least-squares fit
        // where it poisons every coefficient.
        let value: f64 = self.1.parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        let secs = self.0.trunc() as i64;
        let nanos = ((self.0 - self.0.trunc()) * 1e9).round() as u32;
        Some(Point {
            t: DateTime::from_timestamp(secs, nanos)?,
            v: value,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t: f64, v: &str) -> Sample {
        Sample(t, v.to_string())
    }

    #[test]
    fn a_quoted_integer_is_read_as_a_number() {
        // The real shape, taken from a live server.
        let point = sample(1_789_095_229.0, "32309248").point().unwrap();
        assert_eq!(point.v, 32_309_248.0);
        assert_eq!(point.t.timestamp(), 1_789_095_229);
    }

    #[test]
    fn a_fractional_timestamp_keeps_its_sub_second_part() {
        let point = sample(1_789_095_229.5, "1").point().unwrap();
        assert_eq!(point.t.timestamp(), 1_789_095_229);
        assert_eq!(point.t.timestamp_subsec_millis(), 500);
    }

    /// A gap is not a reading. Rust parses these happily, and one of them in a
    /// least-squares fit poisons every coefficient.
    #[test]
    fn non_finite_values_are_dropped_rather_than_parsed() {
        for raw in ["NaN", "+Inf", "-Inf", "inf"] {
            assert!(sample(1.0, raw).point().is_none(), "{raw} survived");
        }
    }

    #[test]
    fn a_value_that_is_not_a_number_is_dropped() {
        assert!(sample(1.0, "").point().is_none());
        assert!(sample(1.0, "null").point().is_none());
    }

    // ------------------------------------------------------------------ step

    #[test]
    fn the_step_keeps_a_long_range_under_the_point_ceiling() {
        for days in [1, 7, 30, 90, 365] {
            let range = days * 86_400;
            let step = step_for(range);
            assert!(
                range / step <= MAX_POINTS,
                "{days}d at step {step} is {} points",
                range / step
            );
        }
    }

    #[test]
    fn a_short_range_is_not_asked_for_at_absurd_resolution() {
        assert_eq!(step_for(600), MIN_STEP_SECS);
        assert_eq!(step_for(3600), MIN_STEP_SECS);
    }

    // --------------------------------------------------------------- parsing

    #[test]
    fn a_matrix_response_parses_into_points() {
        let body = r#"{
            "status":"success",
            "data":{"resultType":"matrix","result":[
                {"metric":{"__name__":"x","instance":"a:9090"},
                 "values":[[1789095229,"1"],[1789095289,"2"],[1789095349,"NaN"]]}
            ]}
        }"#;
        let parsed: ApiResponse = serde_json::from_str(body).unwrap();
        let points: Vec<Point> = parsed.data.unwrap().result[0]
            .values
            .iter()
            .filter_map(Sample::point)
            .collect();
        assert_eq!(points.len(), 2, "the NaN is a gap, not a reading");
        assert_eq!(points[0].v, 1.0);
        assert_eq!(points[1].v, 2.0);
    }

    #[test]
    fn labels_render_without_the_name_so_the_difference_is_visible() {
        let labels = std::collections::HashMap::from([
            ("__name__".to_string(), "redis_memory_used_bytes".to_string()),
            ("instance".to_string(), "10.0.0.2:9121".to_string()),
            ("job".to_string(), "redis".to_string()),
        ]);
        assert_eq!(
            render_labels(&labels),
            r#"{instance="10.0.0.2:9121",job="redis"}"#
        );
    }

    // -------------------------------------------------------------- declared

    #[test]
    fn a_source_needs_both_a_server_and_a_mapping_to_be_usable() {
        let empty = HistorySource::default();
        assert!(!empty.is_configured());

        let no_queries = HistorySource {
            prometheus_profile_id: "prom".into(),
            queries: vec![],
        };
        assert!(!no_queries.is_configured());

        let ready = HistorySource {
            prometheus_profile_id: "prom".into(),
            queries: vec![MetricQuery {
                metric_key: "memory".into(),
                expr: "redis_memory_used_bytes{instance=\"a\"}".into(),
            }],
        };
        assert!(ready.is_configured());
        assert_eq!(
            ready.expr_for("memory"),
            Some("redis_memory_used_bytes{instance=\"a\"}")
        );
        assert_eq!(ready.expr_for("cpu"), None);
    }
}
