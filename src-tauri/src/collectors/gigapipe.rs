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

pub struct GigapipeCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    /// LogQL selector whose matching lines join root's shared log stream.
    /// Empty means this connection reports on gigapipe and nothing else.
    log_query: String,
    log_limit: usize,
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
            .and_then(|p| (current.parse_errors > p.parse_errors).then(|| current.parse_errors - p.parse_errors))
            .unwrap_or(0.0);
        let new_resets = previous
            .as_ref()
            .and_then(|p| (current.conn_resets > p.conn_resets).then(|| current.conn_resets - p.conn_resets))
            .unwrap_or(0.0);

        *self.previous.lock().await = Some(current.clone());

        // -------------------------------------------------------- rollup
        let idle_breach = self.idle_ingest_minutes > 0 && idle_minutes >= self.idle_ingest_minutes;

        let health = if signals_down > 0 && signals_up == 0 {
            Health::Down
        } else if (signals_down > 0 && self.signals_degrade)
            || new_parse_errors > 0.0
            || new_resets > 0.0
            || idle_breach
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
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Gigapipe,
                    &source,
                    level_from_labels(&stream.stream, line),
                    line.clone(),
                    timestamp,
                ));
            }
        }

        out.sort_by_key(|event| event.timestamp);
        out.truncate(want);
        Ok(out)
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
