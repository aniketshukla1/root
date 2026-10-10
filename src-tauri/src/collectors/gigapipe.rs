//! Gigapipe — the observability warehouse, watched and read.
//!
//! Gigapipe is the one connection that is two things at once. It is a service
//! root monitors like any other, and it is a place root can read *from*: a
//! ClickHouse-backed store holding logs, metrics, traces and profiles that
//! arrived over OpenTelemetry, Loki, Influx, Datadog or Elastic protocols.
//!
//! ## Why the read path matters here
//!
//! root's own history is a ring buffer holding a few hours and its log buffer
//! is in memory only — deliberately, so there is no retention policy to
//! manage. The cost is that root cannot answer a question about last Tuesday.
//! Gigapipe can, and it speaks two query languages root already has a client
//! for: PromQL at `/api/v1/query_range`, which `promquery.rs` already talks to
//! unchanged, and LogQL at `/loki/api/v1/query_range`, which `fetch_logs`
//! below uses to pull real application logs into root's shared stream.
//!
//! So a gigapipe connection with `logQuery` set is not reporting on gigapipe.
//! It is reporting on whatever ships *into* gigapipe — which is the whole
//! point, and the reason root does not need an OTLP receiver of its own.
//!
//! ## Health is not readiness
//!
//! `/ready` answers from the process. A ClickHouse that has gone away leaves
//! gigapipe answering `/ready` happily while every query fails, which is the
//! exact failure this collector exists to catch. So the read APIs are probed
//! per signal — logs, metrics, traces — and a store that cannot serve is
//! degraded even while its process is fine.
//!
//! ## Counters, not gauges
//!
//! `/metrics` exposes `sent_rows` and `sent_bytes` as monotonic counters, so
//! the useful number — rows per second, and which signal stopped arriving —
//! has to be derived between polls. A counter that went *down* means the
//! process restarted; that interval is dropped rather than reported as a
//! negative rate.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

use super::promtext::{parse_prometheus, Sample};
use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::configsource::ConfigDocument;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, Section, ServiceKind,
};
use crate::traces::{Span, SpanEvent, SpanStatus, Trace, TraceSummary};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Read APIs probed each cycle, as (signal, path). One cheap request each.
///
/// Profiles are absent on purpose: `/pyroscope/render` needs a query, a from
/// and an until, and a probe that has to invent all three is testing its own
/// parameters rather than the store.
const SIGNAL_PROBES: &[(&str, &str)] = &[
    ("logs", "/loki/api/v1/labels"),
    ("metrics", "/api/v1/labels"),
    ("traces", "/api/echo"),
];

/// Ingest pipelines listed before the table is truncated.
const MAX_INGEST_ROWS: usize = 40;

/// Frames the store is asked to collapse a profile into.
///
/// Pyroscope's own default. Past this the store folds the tail into a single
/// `other` node, which is the right trade for a picture: a rectangle one
/// pixel wide is not readable and the ranking beside the graph already sums
/// what the tail was doing.
const MAX_PROFILE_NODES: usize = 8192;

/// The most a single read API answer may be before it is refused.
///
/// Generous for any real query and far below what an unbounded selector can
/// produce. Without it the body, the parsed value and the rendered string are
/// all live at once, so the process peaks at several times the response size.
const MAX_READ_BYTES: usize = 8 * 1024 * 1024;

pub struct GigapipeCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    /// LogQL selector whose matching lines join root's shared log stream.
    /// Empty means this connection reports on gigapipe and nothing else.
    log_query: String,
    log_limit: usize,
    /// TraceQL selector listing traces worth looking at. Empty means traces
    /// are not listed at all — a warehouse holding none should not grow an
    /// empty table on every dashboard.
    trace_query: String,
    trace_limit: usize,
    /// Milliseconds past which a listed trace is called slow. Zero means the
    /// listing carries no verdict, which is the honest default: the operator's
    /// TraceQL already said what it wanted, and re-judging it here would raise
    /// an alert for every row they deliberately asked for.
    trace_slow_ms: f64,
    /// How far back the trace search looks on each poll.
    trace_lookback_minutes: i64,
    /// Default Pyroscope selector for the flame graph — a profile type and a
    /// label set, as the store spells it. Empty means this connection offers
    /// no profiles and the screen says so rather than guessing a type.
    profile_query: String,
    /// Whether an unreachable read API degrades the connection.
    signals_degrade: bool,
    /// Minutes without an ingested row before the connection is degraded.
    /// Zero disables it — a fresh warehouse is idle, not broken.
    idle_ingest_minutes: u64,
    profile_id: String,
    profile_name: String,
    /// Last poll's counters, for deriving rates.
    previous: Mutex<Option<Counters>>,
    /// When a row was last seen to arrive, for idleness.
    last_ingest: Mutex<Option<DateTime<Utc>>>,
    /// Signal reachability as of the last poll, so a transition is logged once
    /// rather than every cycle.
    signal_state: Mutex<BTreeMap<String, bool>>,
    /// Last build version seen, so an upgrade is an event.
    version: Mutex<Option<String>>,
}

/// The counter values one poll observed.
#[derive(Debug, Clone, Default)]
struct Counters {
    at: DateTime<Utc>,
    /// Rows written, per `service` label — the ingest pipeline's own name.
    rows: BTreeMap<String, f64>,
    bytes: BTreeMap<String, f64>,
    parse_errors: f64,
    conn_resets: f64,
}

impl Counters {
    fn total_rows(&self) -> f64 {
        self.rows.values().sum()
    }
}

/// Prometheus' `{status, data}` envelope, shared by the Loki and Prometheus
/// APIs gigapipe serves.
#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    status: String,
    data: Option<T>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamsData {
    #[serde(default)]
    result: Vec<StreamResult>,
}

#[derive(Debug, Deserialize)]
struct StreamResult {
    #[serde(default)]
    stream: BTreeMap<String, String>,
    /// `[["<nanosecond timestamp as string>", "<line>"], ...]`
    #[serde(default)]
    values: Vec<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct BuildInfo {
    #[serde(default)]
    version: String,
}

/// A nanosecond timestamp, which gigapipe sends two different ways.
///
/// `TraceResponse.startTimeUnixNano` is an `int64` and serializes as a JSON
/// number; `TraceInfo.startTimeUnixNano` is a `string` and serializes quoted.
/// Which one comes back depends on whether the search used TraceQL or tags.
/// Accepting only one shape would make trace listing work right up until the
/// operator wrote a TraceQL expression, and then stop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Nanos(i64);

impl Nanos {
    fn to_utc(self) -> Option<DateTime<Utc>> {
        Utc.timestamp_opt(self.0 / 1_000_000_000, (self.0 % 1_000_000_000) as u32)
            .single()
    }
}

impl<'de> serde::Deserialize<'de> for Nanos {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Number(n) => Ok(Nanos(n.as_i64().unwrap_or(0))),
            serde_json::Value::String(s) => Ok(Nanos(s.trim().parse().unwrap_or(0))),
            _ => Ok(Nanos(0)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TraceSearchData {
    #[serde(default)]
    traces: Vec<TraceHit>,
}

/// One row of `/api/search`.
///
/// No serde `alias` on `traceID`: gigapipe's trace document emits both
/// `traceID` and `traceId` for the same value, and a field matched twice is a
/// duplicate-field error, not a fallback. Declaring the one spelling every
/// producer emits means the other is ignored, which is what is wanted.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceHit {
    #[serde(rename = "traceID", default)]
    trace_id: String,
    #[serde(default)]
    root_service_name: String,
    #[serde(default)]
    root_trace_name: String,
    #[serde(default)]
    start_time_unix_nano: Nanos,
    #[serde(default)]
    duration_ms: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceDocument {
    #[serde(default)]
    resource_spans: Vec<ResourceSpans>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceSpans {
    /// gigapipe emits the pre-1.0 OTLP name. Every current producer emits
    /// `scopeSpans`, so both are accepted and the same parser works against a
    /// real Tempo without a second code path.
    #[serde(default, alias = "scopeSpans")]
    instrumentation_library_spans: Vec<ScopeSpans>,
}

#[derive(Debug, Deserialize)]
struct ScopeSpans {
    #[serde(default)]
    spans: Vec<WireSpan>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSpan {
    #[serde(rename = "spanID", default)]
    span_id: String,
    #[serde(default)]
    parent_span_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    service_name: String,
    #[serde(default)]
    start_time_unix_nano: Nanos,
    #[serde(default)]
    end_time_unix_nano: Nanos,
    #[serde(default)]
    attributes: Vec<WireAttribute>,
    #[serde(default)]
    events: Vec<WireEvent>,
    #[serde(default)]
    status: Option<WireStatus>,
}

#[derive(Debug, Deserialize)]
struct WireAttribute {
    #[serde(default)]
    key: String,
    #[serde(default)]
    value: WireAttributeValue,
}

/// Every attribute value arrives already coerced to a string — gigapipe
/// stringifies ints, bools, doubles and bytes on the way out, so there is no
/// type to preserve here even if root wanted one.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireAttributeValue {
    #[serde(default)]
    string_value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEvent {
    #[serde(default)]
    time_unix_nano: Nanos,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct WireStatus {
    #[serde(default)]
    code: serde_json::Value,
    #[serde(default)]
    message: String,
}

impl WireSpan {
    /// The span's own service, from whichever place the producer put it.
    ///
    /// gigapipe sets `serviceName` and also injects `service.name` into the
    /// attributes; a stock OTLP exporter puts it only in the resource, which
    /// this document shape flattens away. Falling through both keeps a span
    /// attributable instead of landing it under an empty service name, which
    /// would silently merge every such span into one row of the breakdown.
    fn service(&self, attributes: &BTreeMap<String, String>) -> String {
        if !self.service_name.is_empty() {
            return self.service_name.clone();
        }
        attributes
            .get("service.name")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn into_span(self) -> Option<Span> {
        if self.span_id.is_empty() {
            return None;
        }
        let started_at = self.start_time_unix_nano.to_utc()?;
        let ended_at = self.end_time_unix_nano.to_utc().unwrap_or(started_at);

        let attributes: BTreeMap<String, String> = self
            .attributes
            .iter()
            .filter(|a| !a.key.is_empty())
            .map(|a| (a.key.clone(), a.value.string_value.clone()))
            .collect();

        let events = self
            .events
            .iter()
            .filter_map(|e| {
                Some(SpanEvent {
                    at: e.time_unix_nano.to_utc()?,
                    name: e.name.clone(),
                })
            })
            .collect();

        let (status, status_message) = match &self.status {
            Some(status) => (
                SpanStatus::from_wire(&status.code),
                (!status.message.is_empty()).then(|| status.message.clone()),
            ),
            None => (SpanStatus::Unset, None),
        };

        let service = self.service(&attributes);
        // A negative duration means the two clocks disagree about which came
        // first. Zero is wrong but harmless; a negative bar cannot be drawn.
        let duration_ms =
            ((ended_at - started_at).num_microseconds().unwrap_or(0) as f64 / 1000.0).max(0.0);

        Some(Span {
            span_id: self.span_id,
            parent_span_id: (!self.parent_span_id.is_empty()).then_some(self.parent_span_id),
            name: if self.name.is_empty() {
                "(unnamed)".to_string()
            } else {
                self.name
            },
            service,
            started_at,
            duration_ms,
            status,
            status_message,
            attributes,
            events,
        })
    }
}

/// Trace ids are hex. Sending anything else earns a 500 from gigapipe with no
/// explanation, so it is refused here where the reason can be given.
/// The leading part of an error body, for a message an operator will read.
///
/// Gigapipe answers a rejected query with the parse error in the body, which is
/// the useful half; it can also answer with an HTML error page, which is not.
/// Taking the first line and capping it keeps the first case intact without
/// pasting a document into the conversation.
fn stream_lines(data: &StreamsData, profile_id: &str, profile_name: &str) -> Vec<LogEvent> {
    let mut out = Vec::new();
    for stream in &data.result {
        let source = source_from_labels(&stream.stream);
        for entry in &stream.values {
            // `[timestamp, line]`. Anything else is a payload shape this
            // does not understand, and guessing at it would put a
            // mislabelled line in the operator's stream.
            let (Some(raw_ts), Some(line)) = (entry.first(), entry.get(1)) else {
                continue;
            };
            let Some(timestamp) = parse_nanos(raw_ts) else {
                continue;
            };
            out.push(LogEvent::new(
                profile_id,
                profile_name,
                ServiceKind::Gigapipe,
                &source,
                level_from_labels(&stream.stream, line),
                line.clone(),
                timestamp,
            ));
        }
    }
    out
}

/// The lines in a LogQL answer fetched through the read API — Explore's —
/// in whatever order the store sent them. A metric query has none; its
/// matrix is for `promquery::series_in`.
pub fn lines_in(response: &serde_json::Value, profile_id: &str, profile_name: &str) -> Vec<LogEvent> {
    serde_json::from_value::<StreamsData>(response["data"].clone())
        .map(|data| stream_lines(&data, profile_id, profile_name))
        .unwrap_or_default()
}

fn first_line(body: &str, limit: usize) -> String {
    let line = body.trim().lines().next().unwrap_or_default().trim();
    if line.is_empty() {
        return "no reason given".to_string();
    }
    match line.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

fn is_hex_trace_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_hexdigit())
}

impl GigapipeCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let base = profile.endpoint.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err(AppError::Config(
                "Gigapipe connections need a URL, e.g. http://gigapipe.internal:3100".to_string(),
            ));
        }
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(AppError::Config(format!(
                "`{base}` must start with http:// or https://"
            )));
        }

        Ok(GigapipeCollector {
            client: http_client(profile.options.insecure_tls(), REQUEST_TIMEOUT)?,
            base,
            username: profile.username.clone().filter(|u| !u.is_empty()),
            secret,
            log_query: profile
                .options
                .string("logQuery")
                .unwrap_or_default()
                .trim()
                .to_string(),
            log_limit: profile.options.log_limit(),
            trace_query: profile
                .options
                .string("traceQuery")
                .unwrap_or_default()
                .trim()
                .to_string(),
            trace_limit: profile.options.u64("traceLimit").unwrap_or(20).clamp(1, 200) as usize,
            trace_slow_ms: profile.options.u64("traceSlowMs").unwrap_or(0) as f64,
            trace_lookback_minutes: profile
                .options
                .u64("traceLookbackMinutes")
                .unwrap_or(15)
                .clamp(1, 1440) as i64,
            profile_query: profile
                .options
                .string("profileQuery")
                .unwrap_or_default()
                .trim()
                .to_string(),
            signals_degrade: profile.options.get("signalsDegrade").is_none_or(|v| {
                // Absent means on: a store that cannot answer is the failure
                // this collector is for.
                v.as_bool().unwrap_or(true)
            }),
            idle_ingest_minutes: profile.options.u64("idleIngestMinutes").unwrap_or(0),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            previous: Mutex::new(None),
            last_ingest: Mutex::new(None),
            signal_state: Mutex::new(BTreeMap::new()),
            version: Mutex::new(None),
        })
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (&self.username, &self.secret) {
            (Some(user), secret) => req.basic_auth(user, secret.as_ref()),
            (None, Some(token)) => req.bearer_auth(token),
            (None, None) => req,
        }
    }

    /// GET a path, returning the body only on a 2xx.
    async fn get_text(&self, path: &str) -> Option<String> {
        let url = format!("{}{path}", self.base);
        let response = self.authed(self.client.get(&url)).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.text().await.ok()
    }

    /// GET one of the store's read APIs and hand back the JSON it answered
    /// with, or an error that says what went wrong.
    ///
    /// A failure here is reported rather than swallowed, unlike [`get_text`],
    /// because the caller is an operator asking a question and "nothing came
    /// back" is not an answer. Gigapipe puts the reason a query was rejected
    /// in the body — a malformed LogQL selector comes back as a 400 naming the
    /// parse error — so the body is carried into the message instead of being
    /// reduced to a status code.
    ///
    /// [`get_text`]: Self::get_text
    async fn read_api(&self, path: &str, params: &[(&str, String)]) -> Result<serde_json::Value> {
        let url = format!("{}{path}", self.base);
        let mut request = self.authed(self.client.get(&url));
        if !params.is_empty() {
            request = request.query(params);
        }

        let response = request
            .send()
            .await
            .map_err(|e| AppError::Collector(format!("{} did not answer {path}: {e}", self.base)))?;

        let status = response.status();

        // Read with a ceiling rather than `.text()`, which has none. A broad
        // PromQL or LogQL selector can answer with tens of megabytes, and the
        // body, the parsed value and the rendered string would all be live at
        // once. `limit` and `step` are the caller's hints; this is the bound
        // that holds when they are omitted.
        let mut body = Vec::new();
        let mut stream = response;
        while let Some(chunk) = stream.chunk().await.map_err(|e| {
            AppError::Collector(format!("{} response unreadable: {e}", self.base))
        })? {
            if body.len() + chunk.len() > MAX_READ_BYTES {
                return Err(AppError::Collector(format!(
                    "{path} answered with more than {} MiB; narrow the query, or set \
                     `limit` or a coarser `step`",
                    MAX_READ_BYTES / (1024 * 1024)
                )));
            }
            body.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&body).into_owned();

        if !status.is_success() {
            return Err(AppError::Collector(format!(
                "{path} returned {status}: {}",
                first_line(&body, 400)
            )));
        }

        serde_json::from_str(&body).map_err(|e| {
            AppError::Collector(format!(
                "{path} returned {status} and something that is not JSON: {e}"
            ))
        })
    }

    /// Run one of the named read APIs. The match is the whole security
    /// boundary: every path is a literal here, and the only caller-supplied
    /// text that reaches a URL path is percent-encoded first.
    async fn read(&self, read: &super::GigapipeRead) -> Result<serde_json::Value> {
        use super::GigapipeRead as R;

        /// Query parameters the caller left unset are omitted rather than sent
        /// empty, because an empty `start` is not the same request as no
        /// `start` — the store applies its own default only for the latter.
        fn optional(pairs: &[(&'static str, &Option<String>)]) -> Vec<(&'static str, String)> {
            pairs
                .iter()
                .filter_map(|(key, value)| {
                    let value = value.as_ref()?.trim();
                    (!value.is_empty()).then(|| (*key, value.to_string()))
                })
                .collect()
        }

        match read {
            R::PrometheusQuery {
                query,
                start,
                end,
                step,
            } => {
                let mut params = vec![("query", query.clone())];
                params.extend(optional(&[("start", start), ("end", end), ("step", step)]));
                self.read_api("/api/v1/query_range", &params).await
            }
            R::PrometheusLabels { start, end } => {
                let params = optional(&[("start", start), ("end", end)]);
                self.read_api("/api/v1/labels", &params).await
            }
            R::PrometheusLabelValues { label, start, end } => {
                let params = optional(&[("start", start), ("end", end)]);
                let path = format!("/api/v1/label/{}/values", super::path_segment(label));
                self.read_api(&path, &params).await
            }
            R::LokiQuery {
                query,
                start,
                end,
                limit,
            } => {
                let mut params = vec![("query", query.clone())];
                params.extend(optional(&[
                    ("start", start),
                    ("end", end),
                    ("limit", limit),
                ]));
                self.read_api("/loki/api/v1/query_range", &params).await
            }
            R::LokiLabels { start, end } => {
                let params = optional(&[("start", start), ("end", end)]);
                self.read_api("/loki/api/v1/label", &params).await
            }
            R::LokiLabelValues { label, start, end } => {
                let params = optional(&[("start", start), ("end", end)]);
                let path = format!("/loki/api/v1/label/{}/values", super::path_segment(label));
                self.read_api(&path, &params).await
            }
            R::TempoTrace { trace_id } => {
                let path = format!("/api/traces/{}/json", super::path_segment(trace_id));
                self.read_api(&path, &[]).await
            }
            R::TempoTags => self.read_api("/api/search/tags", &[]).await,
            R::TempoTagValues { tag } => {
                let path = format!("/api/search/tag/{}/values", super::path_segment(tag));
                self.read_api(&path, &[]).await
            }
        }
    }

    /// Time one request and report whether it answered. The latency is root's
    /// own measurement, which is the number an operator actually cares about:
    /// how long this store takes to answer *from here*.
    async fn probe(&self, path: &str) -> (bool, u64) {
        let start = Instant::now();
        let url = format!("{}{path}", self.base);
        let ok = self
            .authed(self.client.get(&url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        (ok, elapsed_ms(start))
    }

    /// Probe the render endpoint with the connection's declared selector.
    ///
    /// The other three signals are probed with a parameterless path, which is
    /// why `/pyroscope/render` was left out of `SIGNAL_PROBES` — a probe that
    /// has to invent a query, a from and an until is testing its own
    /// parameters. Once a selector is *declared*, none of the three are
    /// invented, and the probe becomes as honest as the other three.
    ///
    /// A one-minute window, because the question is whether the store answers
    /// rather than what it holds.
    async fn probe_profiles(&self) -> (bool, u64) {
        let start = Instant::now();
        let to = Utc::now();
        let from = to - chrono::Duration::minutes(1);
        let url = format!("{}/pyroscope/render", self.base);
        let ok = self
            .authed(self.client.get(&url))
            .query(&[
                ("query", self.profile_query.clone()),
                ("from", from.timestamp_millis().to_string()),
                ("until", to.timestamp_millis().to_string()),
            ])
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        (ok, elapsed_ms(start))
    }

    /// The running version. Two paths because gigapipe registers the shared
    /// one and the reader registers a Prometheus-compatible alias; which
    /// answers depends on how the modules were deployed.
    async fn build_version(&self) -> Option<String> {
        for path in ["/api/v1/status/buildinfo", "/api/status/buildinfo"] {
            let Some(body) = self.get_text(path).await else {
                continue;
            };
            // The Prometheus-compatible route wraps it; the shared one does not.
            if let Ok(envelope) = serde_json::from_str::<ApiEnvelope<BuildInfo>>(&body) {
                if let Some(info) = envelope.data {
                    if !info.version.is_empty() {
                        return Some(info.version);
                    }
                }
            }
            if let Ok(info) = serde_json::from_str::<BuildInfo>(&body) {
                if !info.version.is_empty() {
                    return Some(info.version);
                }
            }
        }
        None
    }

    /// Pull the counters this collector reasons about out of `/metrics`.
    fn read_counters(samples: &[Sample], at: DateTime<Utc>) -> Counters {
        let mut counters = Counters {
            at,
            ..Default::default()
        };
        for sample in samples {
            match sample.name.as_str() {
                "sent_rows" => {
                    let service = sample
                        .labels
                        .get("service")
                        .cloned()
                        .unwrap_or_else(|| "unlabelled".to_string());
                    *counters.rows.entry(service).or_insert(0.0) += sample.value;
                }
                "sent_bytes" => {
                    let service = sample
                        .labels
                        .get("service")
                        .cloned()
                        .unwrap_or_else(|| "unlabelled".to_string());
                    *counters.bytes.entry(service).or_insert(0.0) += sample.value;
                }
                "json_parse_errors_count" => counters.parse_errors = sample.value,
                "connection_reset_by_peer_count" => counters.conn_resets = sample.value,
                _ => {}
            }
        }
        counters
    }

    /// List traces matching the operator's TraceQL, newest first.
    ///
    /// `start` and `end` are epoch **seconds** here, unlike the LogQL route
    /// which wants nanoseconds. That is gigapipe's contract, not a choice.
    async fn search_traces(&self) -> Result<Vec<TraceSummary>> {
        let end = Utc::now();
        let start = end - chrono::Duration::minutes(self.trace_lookback_minutes);
        self.search_traces_between(start, end, self.trace_limit).await
    }

    /// The same search over an arbitrary window.
    ///
    /// One flame graph, over a window.
    ///
    /// `/pyroscope/render` is a GET, and its three required parameters are
    /// `query`, `from` and `until`. The two timestamps are **milliseconds**:
    /// gigapipe parses them as an integer and divides by a thousand, so
    /// sending seconds would silently ask for a window in 1970 and answer
    /// with an empty profile rather than an error. That is the one detail in
    /// this call worth stating twice.
    ///
    /// The response is the flamebearer, which `crate::profiles` decodes.
    async fn fetch_profile(
        &self,
        query: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<crate::profiles::Profile> {
        // The caller's selector wins; the connection's declared one is the
        // fallback, so the screen opens on something without asking.
        let selector = if query.trim().is_empty() {
            self.profile_query.as_str()
        } else {
            query.trim()
        };
        if selector.is_empty() {
            return Err(AppError::Collector(
                "this connection declares no profile query. Set one on the connection, \
                 or type a selector — a profile type and a label set, such as \
                 `process_cpu:cpu:nanoseconds:cpu:nanoseconds{service_name=\"checkout\"}`."
                    .to_string(),
            ));
        }

        let value = self
            .read_api(
                "/pyroscope/render",
                &[
                    ("query", selector.to_string()),
                    ("from", from.timestamp_millis().to_string()),
                    ("until", to.timestamp_millis().to_string()),
                    ("maxNodes", MAX_PROFILE_NODES.to_string()),
                ],
            )
            .await?;

        let raw: crate::profiles::Flamebearer = serde_json::from_value(value).map_err(|e| {
            AppError::Collector(format!(
                "/pyroscope/render answered with something that is not a flame graph: {e}"
            ))
        })?;
        crate::profiles::decode(&raw)
    }

    /// The polling path asks for "the last N minutes"; a correlation asks for
    /// the window the operator clicked. One implementation, two callers, so a
    /// trace that shows on the dashboard and a trace that shows on a lane are
    /// found the same way.
    async fn search_traces_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<TraceSummary>> {

        let url = format!("{}/api/search", self.base);
        let response = self
            .authed(self.client.get(&url))
            .query(&[
                ("q", self.trace_query.clone()),
                ("start", start.timestamp().to_string()),
                ("end", end.timestamp().to_string()),
                ("limit", limit.to_string()),
            ])
            .send()
            .await
            .map_err(|e| {
                AppError::Collector(format!("{} did not answer the trace search: {e}", self.base))
            })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AppError::Collector(format!("{} response unreadable: {e}", self.base)))?;

        // The status is checked before the body is parsed. A rejected TraceQL
        // expression comes back as `{"status":"error","error":"..."}`, which
        // deserializes perfectly well into a search result holding no traces —
        // so parsing first would report "no traces matched" for what is really
        // a broken query, and the operator would never see the reason.
        if !status.is_success() {
            if let Ok(envelope) = serde_json::from_str::<ApiEnvelope<serde_json::Value>>(&body) {
                if let Some(error) = envelope.error {
                    return Err(AppError::Collector(error));
                }
            }
            return Err(AppError::Collector(format!(
                "{} returned {status} for the trace search",
                self.base
            )));
        }

        let Ok(data) = serde_json::from_str::<TraceSearchData>(&body) else {
            return Err(AppError::Collector(format!(
                "{} returned {status} and something that is not a trace search response",
                self.base
            )));
        };

        Ok(data
            .traces
            .into_iter()
            .filter(|hit| !hit.trace_id.is_empty())
            .map(|hit| TraceSummary {
                trace_id: hit.trace_id,
                root_service: hit.root_service_name,
                root_name: hit.root_trace_name,
                started_at: hit.start_time_unix_nano.to_utc().unwrap_or(end),
                duration_ms: hit.duration_ms,
                // The search result carries no error count. Reporting zero
                // would read as "checked, none found" rather than "not known".
                error_count: None,
            })
            .collect())
    }

    /// One trace, assembled into a tree.
    async fn fetch_trace(&self, trace_id: &str) -> Result<Trace> {
        let trace_id = trace_id.trim();
        if !is_hex_trace_id(trace_id) {
            return Err(AppError::Collector(format!(
                "`{trace_id}` is not a trace id — they are hexadecimal, at most 32 characters"
            )));
        }

        let end = Utc::now();
        let start = end - chrono::Duration::minutes(self.trace_lookback_minutes.max(60));

        let url = format!("{}/api/traces/{trace_id}", self.base);
        let response = self
            .authed(self.client.get(&url))
            .query(&[
                ("start", start.timestamp().to_string()),
                ("end", end.timestamp().to_string()),
            ])
            .send()
            .await
            .map_err(|e| {
                AppError::Collector(format!("{} did not answer for that trace: {e}", self.base))
            })?;

        // Gigapipe answers 404 for a trace it holds no spans for, which is a
        // different thing from a failure and deserves a different sentence.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::Collector(format!(
                "no spans for trace {trace_id} — it may have fallen outside retention"
            )));
        }

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AppError::Collector(format!("{} response unreadable: {e}", self.base)))?;

        let document: TraceDocument = serde_json::from_str(&body).map_err(|_| {
            AppError::Collector(format!(
                "{} returned {status} and something that is not a trace document",
                self.base
            ))
        })?;

        let spans: Vec<Span> = document
            .resource_spans
            .into_iter()
            .flat_map(|resource| resource.instrumentation_library_spans)
            .flat_map(|scope| scope.spans)
            .filter_map(|wire| wire.into_span())
            .collect();

        if spans.is_empty() {
            return Err(AppError::Collector(format!(
                "trace {trace_id} came back with no readable spans"
            )));
        }

        Ok(Trace::assemble(trace_id, spans))
    }
}

/// Per-second rate between two counter readings.
///
/// `None` when there is no previous reading, when no time has passed, or when
/// the counter went backwards — the last of which means the process restarted,
/// and a restart is not a negative ingest rate.
fn rate(current: f64, previous: Option<f64>, seconds: f64) -> Option<f64> {
    let previous = previous?;
    if seconds <= 0.0 || current < previous {
        return None;
    }
    Some((current - previous) / seconds)
}

/// The best name for a log line's origin, from whatever labels it carries.
///
/// OpenTelemetry sets `service_name`; Loki-native shippers usually set `job`.
/// Falling back to the rendered label set keeps a line attributable even when
/// it uses neither.
fn source_from_labels(labels: &BTreeMap<String, String>) -> String {
    for key in ["service_name", "service", "job", "app", "container", "host"] {
        if let Some(value) = labels.get(key) {
            if !value.is_empty() {
                return value.clone();
            }
        }
    }
    let rendered: Vec<String> = labels
        .iter()
        .take(2)
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if rendered.is_empty() {
        "gigapipe".to_string()
    } else {
        rendered.join(" ")
    }
}

/// A level the shipper already decided, if it declared one.
fn level_from_labels(labels: &BTreeMap<String, String>, line: &str) -> LogLevel {
    for key in ["level", "severity", "detected_level", "severity_text"] {
        if let Some(value) = labels.get(key) {
            match value.trim().to_ascii_lowercase().as_str() {
                "trace" => return LogLevel::Trace,
                "debug" => return LogLevel::Debug,
                "info" | "information" => return LogLevel::Info,
                "warn" | "warning" => return LogLevel::Warn,
                "error" | "err" => return LogLevel::Error,
                "fatal" | "critical" | "crit" => return LogLevel::Fatal,
                _ => {}
            }
        }
    }
    LogLevel::infer(line)
}

/// Loki timestamps are nanoseconds since the epoch, as a string.
fn parse_nanos(raw: &str) -> Option<DateTime<Utc>> {
    let nanos: i64 = raw.trim().parse().ok()?;
    Utc.timestamp_opt(nanos / 1_000_000_000, (nanos % 1_000_000_000) as u32)
        .single()
}

#[async_trait]
impl Collector for GigapipeCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Gigapipe
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        // `/ready` is the cheapest true/false there is, and it is the contract
        // gigapipe publishes for exactly this question.
        let Some(_ready) = self.get_text("/ready").await else {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Gigapipe,
                format!("{} did not answer /ready", self.base),
                elapsed_ms(start),
            ));
        };
        let latency = elapsed_ms(start);
        let now = Utc::now();

        let samples = self
            .get_text("/metrics")
            .await
            .map(|body| parse_prometheus(&body))
            .unwrap_or_default();
        let current = Self::read_counters(&samples, now);

        // ------------------------------------------------------- signals
        let mut signal_rows = Vec::new();
        let mut signals_up = 0usize;
        let mut signals_down = 0usize;
        let mut probed = BTreeMap::new();

        if !SIGNAL_PROBES.is_empty() {
            for (signal, path) in SIGNAL_PROBES {
                let (ok, took) = self.probe(path).await;
                probed.insert((*signal).to_string(), ok);
                if ok {
                    signals_up += 1;
                } else {
                    signals_down += 1;
                }
                signal_rows.push(
                    Row::new(
                        format!("signal:{signal}"),
                        *signal,
                        if ok { Health::Healthy } else { Health::Down },
                    )
                    .summary(if ok {
                        format!("answering in {took} ms")
                    } else {
                        format!("{path} did not answer")
                    })
                    .cell("signal", CellValue::text(*signal))
                    .cell("api", CellValue::text(*path))
                    .cell(
                        "state",
                        CellValue::status(if ok { Health::Healthy } else { Health::Down }),
                    )
                    .cell("latency", CellValue::number(took as f64)),
                );
            }
        }
        *self.signal_state.lock().await = probed;

        // ------------------------------------------------------ profiles
        //
        // Only when a selector is declared. A store holding no profiles — or
        // one whose operator has not said which profile matters — should not
        // grow an empty table and a screen behind it.
        let profile_rows = if self.profile_query.is_empty() {
            Vec::new()
        } else {
            let (ok, took) = self.probe_profiles().await;
            if ok {
                signals_up += 1;
            } else {
                signals_down += 1;
            }
            vec![Row::new(
                "profile:declared",
                &self.profile_query,
                if ok { Health::Healthy } else { Health::Down },
            )
            .summary(if ok {
                format!("answering in {took} ms")
            } else {
                "/pyroscope/render did not answer".to_string()
            })
            .cell("selector", CellValue::text(&self.profile_query))
            .cell(
                "state",
                CellValue::status(if ok { Health::Healthy } else { Health::Down }),
            )
            .cell("latency", CellValue::number(took as f64))]
        };

        // -------------------------------------------------------- ingest
        let previous = self.previous.lock().await.clone();
        let elapsed = previous
            .as_ref()
            .map(|p| (now - p.at).num_milliseconds() as f64 / 1000.0)
            .unwrap_or(0.0);

        let mut ingest_rows = Vec::new();
        let mut total_rate = 0.0;
        let mut rate_known = false;

        let mut services: Vec<&String> = current.rows.keys().collect();
        services.sort();
        for service in services.into_iter().take(MAX_INGEST_ROWS) {
            let rows_now = current.rows.get(service).copied().unwrap_or(0.0);
            let bytes_now = current.bytes.get(service).copied().unwrap_or(0.0);
            let rows_per_sec = rate(
                rows_now,
                previous.as_ref().and_then(|p| p.rows.get(service).copied()),
                elapsed,
            );
            if let Some(value) = rows_per_sec {
                total_rate += value;
                rate_known = true;
            }

            ingest_rows.push(
                Row::new(format!("ingest:{service}"), service, Health::Healthy)
                    .summary(match rows_per_sec {
                        Some(value) => format!("{value:.1} rows/s"),
                        None => "rate not known yet".to_string(),
                    })
                    .cell("pipeline", CellValue::text(service))
                    .cell(
                        "rate",
                        match rows_per_sec {
                            Some(value) => CellValue::number(value),
                            None => CellValue::Null,
                        },
                    )
                    .cell("rows", CellValue::number(rows_now))
                    .cell("bytes", CellValue::number(bytes_now)),
            );
        }

        // Idleness is measured from when root first saw this store, not from
        // the counter's own start: a warehouse that was idle long before root
        // was launched has not just gone quiet.
        let mut last_ingest = self.last_ingest.lock().await;
        let ingest_moved = previous
            .as_ref()
            .is_some_and(|p| current.total_rows() > p.total_rows());
        if ingest_moved || last_ingest.is_none() {
            *last_ingest = Some(now);
        }
        let idle_minutes = last_ingest
            .map(|at| (now - at).num_minutes().max(0) as u64)
            .unwrap_or(0);
        drop(last_ingest);

        let new_parse_errors = previous
            .as_ref()
            .and_then(|p| {
                (current.parse_errors > p.parse_errors)
                    .then_some(current.parse_errors - p.parse_errors)
            })
            .unwrap_or(0.0);
        let new_resets = previous
            .as_ref()
            .and_then(|p| {
                (current.conn_resets > p.conn_resets)
                    .then_some(current.conn_resets - p.conn_resets)
            })
            .unwrap_or(0.0);

        *self.previous.lock().await = Some(current.clone());

        // -------------------------------------------------------- traces
        //
        // A failing trace search must not take the connection down: it means
        // the operator's TraceQL is wrong, or the trace store is empty, and
        // neither says anything about whether gigapipe is healthy. The reason
        // goes in the section's empty text where they will actually see it.
        let (trace_rows, trace_error, slow_traces) = if self.trace_query.is_empty() {
            (Vec::new(), None, 0usize)
        } else {
            match self.search_traces().await {
                Ok(found) => {
                    let mut slow = 0usize;
                    let rows = found
                        .iter()
                        .map(|trace| {
                            let is_slow =
                                self.trace_slow_ms > 0.0 && trace.duration_ms > self.trace_slow_ms;
                            if is_slow {
                                slow += 1;
                            }
                            let label = if trace.root_name.is_empty() {
                                trace.trace_id.clone()
                            } else {
                                trace.root_name.clone()
                            };
                            Row::new(
                                format!("trace:{}", trace.trace_id),
                                &label,
                                // Without a declared threshold a listing
                                // carries no verdict: the operator's TraceQL
                                // already said what it wanted, and grading it
                                // again here would alert on every row they
                                // deliberately asked for.
                                if is_slow { Health::Degraded } else { Health::Unknown },
                            )
                            .summary(format!("{:.0} ms", trace.duration_ms))
                            .cell("trace", CellValue::text(&label))
                            .cell("service", CellValue::text(&trace.root_service))
                            .cell("duration", CellValue::number(trace.duration_ms))
                            .cell("started", CellValue::timestamp(trace.started_at))
                            .cell("id", CellValue::text(&trace.trace_id))
                        })
                        .collect();
                    (rows, None, slow)
                }
                Err(error) => (Vec::new(), Some(error.to_string()), 0usize),
            }
        };

        // -------------------------------------------------------- rollup
        let idle_breach = self.idle_ingest_minutes > 0 && idle_minutes >= self.idle_ingest_minutes;

        let health = if signals_down > 0 && signals_up == 0 {
            Health::Down
        } else if (signals_down > 0 && self.signals_degrade)
            || new_parse_errors > 0.0
            || new_resets > 0.0
            || idle_breach
            || slow_traces > 0
        {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if signals_down > 0 && signals_up == 0 {
            "ready, but no query API is answering — check the ClickHouse backend".to_string()
        } else if signals_down > 0 {
            format!(
                "{signals_down} of {} query APIs not answering",
                signals_up + signals_down
            )
        } else if new_resets > 0.0 {
            format!("{new_resets:.0} backend connections reset since the last check")
        } else if new_parse_errors > 0.0 {
            format!("{new_parse_errors:.0} payloads rejected as malformed")
        } else if idle_breach {
            format!("nothing ingested for {idle_minutes} minutes")
        } else if slow_traces > 0 {
            format!(
                "{slow_traces} trace{} over {:.0} ms",
                if slow_traces == 1 { "" } else { "s" },
                self.trace_slow_ms
            )
        } else if rate_known {
            format!(
                "{total_rate:.0} rows/s across {} pipeline{}",
                ingest_rows.len(),
                if ingest_rows.len() == 1 { "" } else { "s" }
            )
        } else {
            format!("ready · {} pipelines", ingest_rows.len())
        };

        let mut metrics = vec![
            Metric::new(
                "signals_up",
                "Query APIs up",
                signals_up as f64,
                MetricUnit::Count,
            ),
            Metric::new(
                "parse_errors",
                "Rejected payloads",
                current.parse_errors,
                MetricUnit::Count,
            ),
            Metric::new(
                "conn_resets",
                "Backend resets",
                current.conn_resets,
                MetricUnit::Count,
            ),
            Metric::new(
                "ingest_bytes",
                "Bytes written",
                current.bytes.values().sum(),
                MetricUnit::Bytes,
            ),
        ];
        if rate_known {
            metrics.insert(
                0,
                Metric::new(
                    "ingest_rows_per_sec",
                    "Ingest rate",
                    total_rate,
                    MetricUnit::PerSecond,
                ),
            );
        }
        if !self.trace_query.is_empty() {
            metrics.push(Metric::new(
                "traces_listed",
                "Traces matched",
                trace_rows.len() as f64,
                MetricUnit::Count,
            ));
            if self.trace_slow_ms > 0.0 {
                // Only warn when a threshold was declared. Without one there
                // is no such thing as a slow trace here, and a metric that
                // always breaches trains an operator to ignore it.
                metrics.push(
                    Metric::new(
                        "traces_slow",
                        "Slow traces",
                        slow_traces as f64,
                        MetricUnit::Count,
                    )
                    .warn_above(0.0),
                );
            }
        }
        if let Some(memory) = super::promtext::gauge(&samples, "process_resident_memory_bytes") {
            metrics.push(Metric::new(
                "memory",
                "Resident memory",
                memory,
                MetricUnit::Bytes,
            ));
        }
        if let Some(goroutines) = super::promtext::gauge(&samples, "go_goroutines") {
            metrics.push(Metric::new(
                "goroutines",
                "Goroutines",
                goroutines,
                MetricUnit::Count,
            ));
        }

        let version = self.build_version().await;
        let mut sections = vec![
            Section::new(
                "signals",
                "Query APIs",
                vec![
                    ColumnSpec::text("signal", "Signal").primary(),
                    ColumnSpec::text("api", "Endpoint"),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::metric("latency", "Answered in", MetricUnit::Millis),
                ],
            )
            .rows(signal_rows)
            .priority(5)
            .sorted_by_health()
            .empty_text("No query APIs probed."),
            Section::new(
                "profiles",
                "Profiles",
                vec![
                    // Marked, so the cell opens the flame graph. The frontend
                    // learns nothing about gigapipe from this.
                    ColumnSpec::text("selector", "Profile").primary().profile(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::metric("latency", "Answered in", MetricUnit::Millis),
                ],
            )
            .rows(profile_rows)
            .priority(6)
            .empty_text("No profile selector declared on this connection."),
            Section::new(
                "ingest",
                "Ingest pipelines",
                vec![
                    ColumnSpec::text("pipeline", "Pipeline").primary(),
                    ColumnSpec::metric("rate", "Rows/s", MetricUnit::PerSecond),
                    ColumnSpec::metric("rows", "Rows written", MetricUnit::Count),
                    ColumnSpec::metric("bytes", "Bytes written", MetricUnit::Bytes),
                ],
            )
            .rows(ingest_rows)
            // Per-pipeline history: which signal stopped arriving is the
            // question, and it only has an answer if each one has its own line.
            .tracked()
            .priority(10)
            .empty_text("Nothing has been ingested since this instance started."),
        ];

        if !self.trace_query.is_empty() {
            let empty_text = match &trace_error {
                Some(reason) => format!("Trace search failed: {reason}"),
                None => "No traces matched in the search window.".to_string(),
            };
            sections.push(
                Section::new(
                    "traces",
                    "Traces",
                    vec![
                        ColumnSpec::text("trace", "Trace").primary(),
                        ColumnSpec::text("service", "Root service"),
                        ColumnSpec::metric("duration", "Duration", MetricUnit::Millis),
                        ColumnSpec::timestamp("started", "Started"),
                        ColumnSpec::code("id", "Trace id").trace(),
                    ],
                )
                .rows(trace_rows)
                .priority(8)
                .empty_text(&empty_text),
            );
        }

        if let Some(version) = &version {
            sections.push(
                Section::new(
                    "build",
                    "Build",
                    vec![
                        ColumnSpec::text("component", "Component").primary(),
                        ColumnSpec::text("version", "Version").revision(),
                    ],
                )
                .rows(vec![Row::new("build:gigapipe", "gigapipe", Health::Healthy)
                    .summary(version.clone())
                    .cell("component", CellValue::text("gigapipe"))
                    .cell("version", CellValue::text(version))])
                .priority(60)
                .empty_text("Version not reported."),
            );
        }

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Gigapipe,
            health,
            summary,
            metrics,
            sections,
            checked_at: now,
            latency_ms: latency,
            error: None,
        })
    }

    /// Two streams, and they are not the same thing.
    ///
    /// Without `logQuery` this reports on gigapipe: an `Event` line when a
    /// query API changes state or the build version moves. With it, the lines
    /// are whatever matched the LogQL selector — other people's applications,
    /// pulled out of the warehouse into root's shared stream as `Runtime`.
    async fn fetch_logs(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let mut out = Vec::new();

        // ---------------------------------------------------- transitions
        let previous_version = self.version.lock().await.clone();
        if let Some(version) = self.build_version().await {
            let mut held = self.version.lock().await;
            match held.as_ref() {
                Some(old) if old != &version => out.push(
                    LogEvent::new(
                        &self.profile_id,
                        &self.profile_name,
                        ServiceKind::Gigapipe,
                        "gigapipe",
                        LogLevel::Info,
                        format!("gigapipe upgraded from {old} to {version}"),
                        Utc::now(),
                    )
                    .stream(LogStream::Event),
                ),
                None if previous_version.is_none() => {}
                _ => {}
            }
            *held = Some(version);
        }

        // ------------------------------------------------------- LogQL
        if self.log_query.is_empty() {
            return Ok(out);
        }

        let end = Utc::now();
        // A `since` at or after now would ask for an empty window; the
        // watermark is the caller's, so it is clamped rather than trusted.
        let start = if since >= end {
            end - chrono::Duration::seconds(1)
        } else {
            since
        };
        let want = limit.min(self.log_limit);

        let url = format!("{}/loki/api/v1/query_range", self.base);
        let request = self.authed(self.client.get(&url)).query(&[
            ("query", self.log_query.clone()),
            (
                "start",
                (start.timestamp_nanos_opt().unwrap_or(0) + 1).to_string(),
            ),
            ("end", end.timestamp_nanos_opt().unwrap_or(0).to_string()),
            ("limit", want.to_string()),
            // Oldest first, matching every other collector's contract.
            ("direction", "forward".to_string()),
        ]);

        let response = request.send().await.map_err(|e| {
            AppError::Collector(format!("{} did not answer the LogQL query: {e}", self.base))
        })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AppError::Collector(format!("{} response unreadable: {e}", self.base)))?;

        // A bad LogQL expression comes back as a 400 with the reason in the
        // body, which is the only useful thing to show an operator.
        let parsed: ApiEnvelope<StreamsData> = serde_json::from_str(&body).map_err(|_| {
            AppError::Collector(format!(
                "{} returned {status} and something that is not a LogQL response — \
                 check the URL points at gigapipe itself and not a proxy",
                self.base
            ))
        })?;
        if parsed.status != "success" {
            return Err(AppError::Collector(
                parsed
                    .error
                    .unwrap_or_else(|| format!("the LogQL query failed with {status}")),
            ));
        }

        let Some(data) = parsed.data else {
            return Ok(out);
        };

        out.extend(stream_lines(&data, &self.profile_id, &self.profile_name));
        out.sort_by_key(|event| event.timestamp);
        out.truncate(want);
        Ok(out)
    }

    /// The only drill-down this collector offers, and the only one it needs:
    /// a trace is the one thing here that a table cannot show.
    async fn detail(&self, query: &super::DetailQuery) -> Result<serde_json::Value> {
        match query {
            super::DetailQuery::Trace { trace_id } => {
                let trace = self.fetch_trace(trace_id).await?;
                serde_json::to_value(trace).map_err(|e| {
                    AppError::Collector(format!("could not serialize the trace: {e}"))
                })
            }
            super::DetailQuery::GigapipeRead(read) => self.read(read).await,
            super::DetailQuery::TracesInWindow { from, to, limit } => {
                // No TraceQL selector means this connection lists no traces at
                // all, which is the honest empty answer rather than an error:
                // the store is reachable, the operator just has not said what
                // to look for.
                if self.trace_query.is_empty() {
                    return Ok(serde_json::Value::Array(Vec::new()));
                }
                let traces = self.search_traces_between(*from, *to, *limit).await?;
                serde_json::to_value(traces).map_err(|e| {
                    AppError::Collector(format!("could not serialize the traces: {e}"))
                })
            }
            super::DetailQuery::Profile { query, from, to } => {
                let profile = self.fetch_profile(query, *from, *to).await?;
                serde_json::to_value(profile).map_err(|e| {
                    AppError::Collector(format!("could not serialize the profile: {e}"))
                })
            }
            _ => Err(AppError::UnsupportedAction(
                ServiceKind::Gigapipe.label().to_string(),
            )),
        }
    }

    async fn config(&self) -> Result<Vec<ConfigDocument>> {
        let Some(body) = self.get_text("/config").await else {
            return Ok(Vec::new());
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            return Ok(vec![ConfigDocument::live_text(
                &self.profile_id,
                "gigapipe configuration",
                "GET /config",
                &body,
            )]);
        };
        Ok(vec![ConfigDocument::live(
            &self.profile_id,
            "gigapipe configuration",
            "GET /config",
            &value,
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_metrics() -> &'static str {
        "\
# HELP sent_rows The total number of rows sent
# TYPE sent_rows counter
sent_rows{service=\"logs\"} 1000
sent_rows{service=\"metrics\"} 500
sent_bytes{service=\"logs\"} 90000
sent_bytes{service=\"metrics\"} 4000
json_parse_errors_count 2
connection_reset_by_peer_count 0
go_goroutines 42
process_resident_memory_bytes 104857600
"
    }

    #[test]
    fn counters_are_read_per_pipeline() {
        let samples = parse_prometheus(sample_metrics());
        let counters = super::GigapipeCollector::read_counters(&samples, Utc::now());

        assert_eq!(counters.rows.get("logs"), Some(&1000.0));
        assert_eq!(counters.rows.get("metrics"), Some(&500.0));
        assert_eq!(counters.bytes.get("logs"), Some(&90000.0));
        assert_eq!(counters.parse_errors, 2.0);
        assert_eq!(counters.conn_resets, 0.0);
        assert_eq!(counters.total_rows(), 1500.0);
    }

    #[test]
    fn rate_needs_a_previous_reading() {
        assert_eq!(rate(100.0, None, 10.0), None);
    }

    #[test]
    fn rate_is_per_second() {
        assert_eq!(rate(100.0, Some(50.0), 10.0), Some(5.0));
    }

    #[test]
    fn a_counter_reset_reports_no_rate() {
        // gigapipe restarted: its counters start again at zero. Reporting
        // (0 - 1000) / 10 would draw a large negative spike on the ingest
        // chart and read as data loss, which is the opposite of what happened.
        assert_eq!(rate(0.0, Some(1000.0), 10.0), None);
    }

    #[test]
    fn a_zero_interval_reports_no_rate() {
        assert_eq!(rate(100.0, Some(50.0), 0.0), None);
    }

    #[test]
    fn otlp_service_name_wins_over_a_rendered_label_set() {
        let mut labels = BTreeMap::new();
        labels.insert("service_name".to_string(), "checkout-api".to_string());
        labels.insert("host".to_string(), "10.0.0.4".to_string());
        assert_eq!(source_from_labels(&labels), "checkout-api");
    }

    #[test]
    fn a_line_with_no_identifying_label_is_still_attributable() {
        let mut labels = BTreeMap::new();
        labels.insert("region".to_string(), "eu-west-1".to_string());
        assert_eq!(source_from_labels(&labels), "region=eu-west-1");
    }

    #[test]
    fn a_declared_level_beats_inference() {
        let mut labels = BTreeMap::new();
        labels.insert("detected_level".to_string(), "warn".to_string());
        // The line itself says nothing about severity; the shipper does.
        assert_eq!(level_from_labels(&labels, "cache miss for key 91"), LogLevel::Warn);
    }

    #[test]
    fn an_undeclared_level_falls_back_to_the_line() {
        let labels = BTreeMap::new();
        assert_eq!(
            level_from_labels(&labels, "ERROR could not reach the database"),
            LogLevel::Error
        );
    }

    #[test]
    fn loki_nanosecond_timestamps_parse() {
        // 2026-01-01T00:00:00Z in nanoseconds.
        let parsed = parse_nanos("1767225600000000000").expect("should parse");
        assert_eq!(parsed.timestamp(), 1_767_225_600);
    }

    #[test]
    fn a_malformed_timestamp_is_refused_rather_than_defaulted() {
        // Defaulting to the epoch would file the line at 1970 and hide it at
        // the far end of every time range.
        assert!(parse_nanos("not-a-timestamp").is_none());
    }
}
