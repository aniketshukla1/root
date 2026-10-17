//! OpenTelemetry, pushed: an OTLP/HTTP receiver as a connection.
//!
//! Every other connection is asked; this one is told. Services export OTLP to
//! it — protobuf or JSON, gzipped or not, at `/v1/traces`, `/v1/logs` and
//! `/v1/metrics` — and it turns what arrives into what root already shows:
//! log lines in the stream, spans in every trace view, and each service's
//! metrics as this connection's own, charted and archived like any other.
//! Counters arrive as running totals and are shown as rates; histograms as a
//! p95. It listens on loopback unless an address is given, and listening
//! beyond the machine needs a token.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use chrono::{DateTime, Duration, Utc};
use prost::Message;
use serde_json::Value;
use tokio::sync::Mutex;

use super::{Collector, DetailQuery};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, Metric, MetricUnit, Row, Section, ServiceKind};
use crate::traces::{Span, SpanStatus, Trace, TraceSummary};

/// The OTLP/HTTP default port.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:4318";
/// What one request may carry, before and after decompression. A batch from a
/// busy service is megabytes; a gzip that inflates past this is not a batch.
const MAX_BODY: usize = 16 << 20;
const MAX_INFLATED: u64 = 64 << 20;
/// Kept in memory between polls and for the trace views.
const MAX_SPANS: usize = 20_000;
const MAX_LINES: usize = 5_000;
const MAX_METRICS: usize = 200;

// ------------------------------------------------------------------ the wire

/// The part of the OTLP protobuf schema root reads, with the field numbers of
/// opentelemetry-proto v1. Anything else in a message is skipped by prost.
pub mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct KeyValue {
        #[prost(string, tag = "1")]
        pub key: String,
        #[prost(message, optional, tag = "2")]
        pub value: Option<AnyValue>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct AnyValue {
        #[prost(oneof = "any_value::Value", tags = "1, 2, 3, 4")]
        pub value: Option<any_value::Value>,
    }

    pub mod any_value {
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub enum Value {
            #[prost(string, tag = "1")]
            StringValue(String),
            #[prost(bool, tag = "2")]
            BoolValue(bool),
            #[prost(int64, tag = "3")]
            IntValue(i64),
            #[prost(double, tag = "4")]
            DoubleValue(f64),
        }
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Resource {
        #[prost(message, repeated, tag = "1")]
        pub attributes: Vec<KeyValue>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ExportTraceServiceRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_spans: Vec<ResourceSpans>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ResourceSpans {
        #[prost(message, optional, tag = "1")]
        pub resource: Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        pub scope_spans: Vec<ScopeSpans>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ScopeSpans {
        #[prost(message, repeated, tag = "2")]
        pub spans: Vec<Span>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Span {
        #[prost(bytes = "vec", tag = "1")]
        pub trace_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "2")]
        pub span_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "4")]
        pub parent_span_id: Vec<u8>,
        #[prost(string, tag = "5")]
        pub name: String,
        #[prost(fixed64, tag = "7")]
        pub start_time_unix_nano: u64,
        #[prost(fixed64, tag = "8")]
        pub end_time_unix_nano: u64,
        #[prost(message, repeated, tag = "9")]
        pub attributes: Vec<KeyValue>,
        #[prost(message, optional, tag = "15")]
        pub status: Option<Status>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Status {
        #[prost(string, tag = "2")]
        pub message: String,
        /// 0 unset, 1 ok, 2 error.
        #[prost(int32, tag = "3")]
        pub code: i32,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ExportLogsServiceRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_logs: Vec<ResourceLogs>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ResourceLogs {
        #[prost(message, optional, tag = "1")]
        pub resource: Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        pub scope_logs: Vec<ScopeLogs>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ScopeLogs {
        #[prost(message, repeated, tag = "2")]
        pub log_records: Vec<LogRecord>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct LogRecord {
        #[prost(fixed64, tag = "1")]
        pub time_unix_nano: u64,
        #[prost(int32, tag = "2")]
        pub severity_number: i32,
        #[prost(string, tag = "3")]
        pub severity_text: String,
        #[prost(message, optional, tag = "5")]
        pub body: Option<AnyValue>,
        #[prost(bytes = "vec", tag = "9")]
        pub trace_id: Vec<u8>,
        #[prost(fixed64, tag = "11")]
        pub observed_time_unix_nano: u64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ExportMetricsServiceRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_metrics: Vec<ResourceMetrics>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ResourceMetrics {
        #[prost(message, optional, tag = "1")]
        pub resource: Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        pub scope_metrics: Vec<ScopeMetrics>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ScopeMetrics {
        #[prost(message, repeated, tag = "2")]
        pub metrics: Vec<Metric>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Metric {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(string, tag = "3")]
        pub unit: String,
        #[prost(oneof = "metric::Data", tags = "5, 7, 9")]
        pub data: Option<metric::Data>,
    }

    pub mod metric {
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub enum Data {
            #[prost(message, tag = "5")]
            Gauge(super::Gauge),
            #[prost(message, tag = "7")]
            Sum(super::Sum),
            #[prost(message, tag = "9")]
            Histogram(super::Histogram),
        }
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Gauge {
        #[prost(message, repeated, tag = "1")]
        pub data_points: Vec<NumberDataPoint>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Sum {
        #[prost(message, repeated, tag = "1")]
        pub data_points: Vec<NumberDataPoint>,
        /// 1 delta, 2 cumulative.
        #[prost(int32, tag = "2")]
        pub aggregation_temporality: i32,
        #[prost(bool, tag = "3")]
        pub is_monotonic: bool,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Histogram {
        #[prost(message, repeated, tag = "1")]
        pub data_points: Vec<HistogramDataPoint>,
        #[prost(int32, tag = "2")]
        pub aggregation_temporality: i32,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct NumberDataPoint {
        #[prost(message, repeated, tag = "7")]
        pub attributes: Vec<KeyValue>,
        #[prost(fixed64, tag = "2")]
        pub start_time_unix_nano: u64,
        #[prost(fixed64, tag = "3")]
        pub time_unix_nano: u64,
        #[prost(oneof = "number_data_point::Value", tags = "4, 6")]
        pub value: Option<number_data_point::Value>,
    }

    pub mod number_data_point {
        #[derive(Clone, PartialEq, prost::Oneof)]
        pub enum Value {
            #[prost(double, tag = "4")]
            AsDouble(f64),
            #[prost(sfixed64, tag = "6")]
            AsInt(i64),
        }
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HistogramDataPoint {
        #[prost(message, repeated, tag = "9")]
        pub attributes: Vec<KeyValue>,
        #[prost(fixed64, tag = "2")]
        pub start_time_unix_nano: u64,
        #[prost(fixed64, tag = "3")]
        pub time_unix_nano: u64,
        #[prost(fixed64, tag = "4")]
        pub count: u64,
        #[prost(fixed64, repeated, tag = "6")]
        pub bucket_counts: Vec<u64>,
        #[prost(double, repeated, tag = "7")]
        pub explicit_bounds: Vec<f64>,
    }
}

// ----------------------------------------------------------- OTLP/JSON → proto

/// OTLP/JSON onto the same structs, so there is one path from the wire on:
/// camelCase names, ids in hex, 64-bit integers as strings or numbers.
mod json {
    use super::proto;
    use serde_json::Value;

    fn list<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
        v.get(key).and_then(Value::as_array).into_iter().flatten()
    }

    fn u64_of(v: Option<&Value>) -> u64 {
        match v {
            Some(Value::String(s)) => s.parse().unwrap_or(0),
            Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)).unwrap_or(0),
            _ => 0,
        }
    }

    fn str_of(v: Option<&Value>) -> String {
        v.and_then(Value::as_str).unwrap_or_default().to_string()
    }

    fn hex(v: Option<&Value>) -> Vec<u8> {
        let s = v.and_then(Value::as_str).unwrap_or_default();
        (0..s.len() / 2).filter_map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()).collect()
    }

    fn any(v: Option<&Value>) -> Option<proto::AnyValue> {
        use proto::any_value::Value as A;
        let v = v?;
        let value = if let Some(s) = v.get("stringValue").and_then(Value::as_str) {
            A::StringValue(s.to_string())
        } else if let Some(b) = v.get("boolValue").and_then(Value::as_bool) {
            A::BoolValue(b)
        } else if let Some(i) = v.get("intValue") {
            A::IntValue(match i {
                Value::String(s) => s.parse().unwrap_or(0),
                other => other.as_i64().unwrap_or(0),
            })
        } else {
            A::DoubleValue(v.get("doubleValue").and_then(Value::as_f64)?)
        };
        Some(proto::AnyValue { value: Some(value) })
    }

    fn attributes(v: &Value, key: &str) -> Vec<proto::KeyValue> {
        list(v, key).map(|kv| proto::KeyValue { key: str_of(kv.get("key")), value: any(kv.get("value")) }).collect()
    }

    fn resource(v: &Value) -> Option<proto::Resource> {
        v.get("resource").map(|r| proto::Resource { attributes: attributes(r, "attributes") })
    }

    pub fn traces(v: &Value) -> proto::ExportTraceServiceRequest {
        proto::ExportTraceServiceRequest {
            resource_spans: list(v, "resourceSpans")
                .map(|rs| proto::ResourceSpans {
                    resource: resource(rs),
                    scope_spans: list(rs, "scopeSpans")
                        .map(|ss| proto::ScopeSpans {
                            spans: list(ss, "spans")
                                .map(|s| proto::Span {
                                    trace_id: hex(s.get("traceId")),
                                    span_id: hex(s.get("spanId")),
                                    parent_span_id: hex(s.get("parentSpanId")),
                                    name: str_of(s.get("name")),
                                    start_time_unix_nano: u64_of(s.get("startTimeUnixNano")),
                                    end_time_unix_nano: u64_of(s.get("endTimeUnixNano")),
                                    attributes: attributes(s, "attributes"),
                                    status: s.get("status").map(|st| proto::Status {
                                        message: str_of(st.get("message")),
                                        code: u64_of(st.get("code")) as i32,
                                    }),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn logs(v: &Value) -> proto::ExportLogsServiceRequest {
        proto::ExportLogsServiceRequest {
            resource_logs: list(v, "resourceLogs")
                .map(|rl| proto::ResourceLogs {
                    resource: resource(rl),
                    scope_logs: list(rl, "scopeLogs")
                        .map(|sl| proto::ScopeLogs {
                            log_records: list(sl, "logRecords")
                                .map(|r| proto::LogRecord {
                                    time_unix_nano: u64_of(r.get("timeUnixNano")),
                                    severity_number: u64_of(r.get("severityNumber")) as i32,
                                    severity_text: str_of(r.get("severityText")),
                                    body: any(r.get("body")),
                                    trace_id: hex(r.get("traceId")),
                                    observed_time_unix_nano: u64_of(r.get("observedTimeUnixNano")),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn number_points(v: &Value) -> Vec<proto::NumberDataPoint> {
        use proto::number_data_point::Value as N;
        list(v, "dataPoints")
            .map(|p| proto::NumberDataPoint {
                attributes: attributes(p, "attributes"),
                start_time_unix_nano: u64_of(p.get("startTimeUnixNano")),
                time_unix_nano: u64_of(p.get("timeUnixNano")),
                value: if let Some(d) = p.get("asDouble").and_then(Value::as_f64) {
                    Some(N::AsDouble(d))
                } else {
                    p.get("asInt").map(|i| N::AsInt(match i {
                        Value::String(s) => s.parse().unwrap_or(0),
                        other => other.as_i64().unwrap_or(0),
                    }))
                },
            })
            .collect()
    }

    pub fn metrics(v: &Value) -> proto::ExportMetricsServiceRequest {
        use proto::metric::Data;
        proto::ExportMetricsServiceRequest {
            resource_metrics: list(v, "resourceMetrics")
                .map(|rm| proto::ResourceMetrics {
                    resource: resource(rm),
                    scope_metrics: list(rm, "scopeMetrics")
                        .map(|sm| proto::ScopeMetrics {
                            metrics: list(sm, "metrics")
                                .map(|m| proto::Metric {
                                    name: str_of(m.get("name")),
                                    unit: str_of(m.get("unit")),
                                    data: if let Some(g) = m.get("gauge") {
                                        Some(Data::Gauge(proto::Gauge { data_points: number_points(g) }))
                                    } else if let Some(s) = m.get("sum") {
                                        Some(Data::Sum(proto::Sum {
                                            data_points: number_points(s),
                                            aggregation_temporality: u64_of(s.get("aggregationTemporality")) as i32,
                                            is_monotonic: s.get("isMonotonic").and_then(Value::as_bool).unwrap_or(false),
                                        }))
                                    } else {
                                        m.get("histogram").map(|h| {
                                            Data::Histogram(proto::Histogram {
                                                data_points: list(h, "dataPoints")
                                                    .map(|p| proto::HistogramDataPoint {
                                                        attributes: attributes(p, "attributes"),
                                                        start_time_unix_nano: u64_of(p.get("startTimeUnixNano")),
                                                        time_unix_nano: u64_of(p.get("timeUnixNano")),
                                                        count: u64_of(p.get("count")),
                                                        bucket_counts: list(p, "bucketCounts").map(|c| u64_of(Some(c))).collect(),
                                                        explicit_bounds: list(p, "explicitBounds").filter_map(Value::as_f64).collect(),
                                                    })
                                                    .collect(),
                                                aggregation_temporality: u64_of(h.get("aggregationTemporality")) as i32,
                                            })
                                        })
                                    },
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

// ------------------------------------------------------------ what it keeps

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn at_nanos(nanos: u64) -> DateTime<Utc> {
    DateTime::from_timestamp((nanos / 1_000_000_000) as i64, (nanos % 1_000_000_000) as u32).unwrap_or_else(Utc::now)
}

fn text_of(value: &Option<proto::AnyValue>) -> String {
    use proto::any_value::Value as A;
    match value.as_ref().and_then(|v| v.value.as_ref()) {
        Some(A::StringValue(s)) => s.clone(),
        Some(A::BoolValue(b)) => b.to_string(),
        Some(A::IntValue(i)) => i.to_string(),
        Some(A::DoubleValue(d)) => d.to_string(),
        None => String::new(),
    }
}

fn service_of(resource: &Option<proto::Resource>) -> String {
    resource
        .as_ref()
        .and_then(|r| r.attributes.iter().find(|kv| kv.key == "service.name"))
        .map(|kv| text_of(&kv.value))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown_service".to_string())
}

/// Severity numbers in bands of four, as the OTLP log data model defines them.
fn level_of(number: i32, text: &str) -> LogLevel {
    match number {
        1..=4 => LogLevel::Trace,
        5..=8 => LogLevel::Debug,
        9..=12 => LogLevel::Info,
        13..=16 => LogLevel::Warn,
        17..=20 => LogLevel::Error,
        21..=24 => LogLevel::Fatal,
        _ => match text.to_ascii_lowercase().as_str() {
            "trace" => LogLevel::Trace,
            "debug" => LogLevel::Debug,
            "warn" | "warning" => LogLevel::Warn,
            "error" => LogLevel::Error,
            "fatal" | "critical" => LogLevel::Fatal,
            _ => LogLevel::Info,
        },
    }
}

/// OTLP's unit strings (UCUM) as root's units.
fn unit_of(unit: &str) -> MetricUnit {
    match unit {
        "ms" => MetricUnit::Millis,
        "s" => MetricUnit::Seconds,
        "By" => MetricUnit::Bytes,
        "By/s" => MetricUnit::BytesPerSecond,
        "%" => MetricUnit::Percent,
        _ => MetricUnit::Count,
    }
}

#[derive(Default, Clone, Copy)]
struct Tally {
    spans: u64,
    errors: u64,
    lines: u64,
    last: Option<DateTime<Utc>>,
}

struct Pushed {
    label: String,
    unit: MetricUnit,
    value: f64,
    at: DateTime<Utc>,
}

#[derive(Default)]
pub struct Received {
    traces: HashMap<String, Vec<Span>>,
    order: VecDeque<String>,
    spans: usize,
    lines: VecDeque<LogEvent>,
    metrics: HashMap<String, Pushed>,
    /// Last cumulative reading per counter series, for rates.
    counters: HashMap<String, (f64, DateTime<Utc>)>,
    /// Last cumulative bucket counts per histogram series, for deltas.
    buckets: HashMap<String, Vec<u64>>,
    tally: BTreeMap<String, Tally>,
    /// The tallies as of the last poll, so a poll reports what is new since.
    polled: BTreeMap<String, Tally>,
    polled_at: Option<DateTime<Utc>>,
}

impl Received {
    fn keep_span(&mut self, trace_id: String, span: Span) {
        if !self.traces.contains_key(&trace_id) {
            self.order.push_back(trace_id.clone());
        }
        self.traces.entry(trace_id).or_default().push(span);
        self.spans += 1;
        while self.spans > MAX_SPANS {
            let Some(oldest) = self.order.pop_front() else { break };
            self.spans -= self.traces.remove(&oldest).map(|s| s.len()).unwrap_or(0);
        }
    }

    pub fn take_traces(&mut self, request: proto::ExportTraceServiceRequest) {
        let now = Utc::now();
        for resource in request.resource_spans {
            let service = service_of(&resource.resource);
            for span in resource.scope_spans.into_iter().flat_map(|s| s.spans) {
                let status = match span.status.as_ref().map(|s| s.code) {
                    Some(2) => SpanStatus::Error,
                    Some(1) => SpanStatus::Ok,
                    _ => SpanStatus::Unset,
                };
                let tally = self.tally.entry(service.clone()).or_default();
                tally.spans += 1;
                tally.errors += u64::from(status == SpanStatus::Error);
                tally.last = Some(now);
                let started = at_nanos(span.start_time_unix_nano);
                let duration_ms = span.end_time_unix_nano.saturating_sub(span.start_time_unix_nano) as f64 / 1e6;
                self.keep_span(
                    hex_of(&span.trace_id),
                    Span {
                        span_id: hex_of(&span.span_id),
                        parent_span_id: (!span.parent_span_id.is_empty()).then(|| hex_of(&span.parent_span_id)),
                        name: span.name,
                        service: service.clone(),
                        started_at: started,
                        duration_ms,
                        status,
                        status_message: span.status.map(|s| s.message).filter(|m| !m.is_empty()),
                        attributes: span.attributes.iter().map(|kv| (kv.key.clone(), text_of(&kv.value))).collect(),
                        events: Vec::new(),
                    },
                );
            }
        }
    }

    pub fn take_logs(&mut self, request: proto::ExportLogsServiceRequest, profile_id: &str, profile_name: &str) {
        let now = Utc::now();
        for resource in request.resource_logs {
            let service = service_of(&resource.resource);
            for record in resource.scope_logs.into_iter().flat_map(|s| s.log_records) {
                let nanos = if record.time_unix_nano > 0 { record.time_unix_nano } else { record.observed_time_unix_nano };
                let mut message = text_of(&record.body);
                // The trace id rides in the line, where every view that links
                // a line to its trace already looks.
                if !record.trace_id.is_empty() && !message.contains("trace_id") {
                    message.push_str(&format!(" trace_id={}", hex_of(&record.trace_id)));
                }
                let tally = self.tally.entry(service.clone()).or_default();
                tally.lines += 1;
                tally.last = Some(now);
                self.lines.push_back(LogEvent::new(
                    profile_id,
                    profile_name,
                    ServiceKind::Otlp,
                    &service,
                    level_of(record.severity_number, &record.severity_text),
                    message,
                    if nanos > 0 { at_nanos(nanos) } else { now },
                ));
                if self.lines.len() > MAX_LINES {
                    self.lines.pop_front();
                }
            }
        }
    }

    fn put(&mut self, key: String, label: String, unit: MetricUnit, value: f64, at: DateTime<Utc>) {
        if !value.is_finite() {
            return;
        }
        if !self.metrics.contains_key(&key) && self.metrics.len() >= MAX_METRICS {
            // The stalest goes, so a service that stopped reporting yields
            // its place to one that has started.
            if let Some(stalest) = self.metrics.iter().min_by_key(|(_, p)| p.at).map(|(k, _)| k.clone()) {
                self.metrics.remove(&stalest);
            }
        }
        self.metrics.insert(key, Pushed { label, unit, value, at });
    }

    pub fn take_metrics(&mut self, request: proto::ExportMetricsServiceRequest) {
        use proto::metric::Data;
        use proto::number_data_point::Value as N;
        let now = Utc::now();
        let value_of = |p: &proto::NumberDataPoint| match p.value {
            Some(N::AsDouble(d)) => d,
            Some(N::AsInt(i)) => i as f64,
            None => f64::NAN,
        };
        let series_of = |attributes: &[proto::KeyValue]| {
            let mut pairs: Vec<String> = attributes.iter().map(|kv| format!("{}={}", kv.key, text_of(&kv.value))).collect();
            pairs.sort();
            pairs.join(",")
        };
        for resource in request.resource_metrics {
            let service = service_of(&resource.resource);
            self.tally.entry(service.clone()).or_default().last = Some(now);
            for metric in resource.scope_metrics.into_iter().flat_map(|s| s.metrics) {
                let key = format!("{service}/{}", metric.name);
                let label = format!("{service} · {}", metric.name);
                let unit = unit_of(&metric.unit);
                match metric.data {
                    Some(Data::Gauge(g)) => {
                        let values: Vec<f64> = g.data_points.iter().map(value_of).filter(|v| v.is_finite()).collect();
                        if !values.is_empty() {
                            self.put(key, label, unit, values.iter().sum::<f64>() / values.len() as f64, now);
                        }
                    }
                    // A counter: its rate, summed across every attribute set.
                    Some(Data::Sum(s)) if s.is_monotonic => {
                        let mut rate = 0.0;
                        let mut seen = false;
                        for p in &s.data_points {
                            let v = value_of(p);
                            let at = at_nanos(p.time_unix_nano);
                            if s.aggregation_temporality == 1 {
                                let span = (p.time_unix_nano.saturating_sub(p.start_time_unix_nano)) as f64 / 1e9;
                                if span > 0.0 {
                                    rate += v / span;
                                    seen = true;
                                }
                                continue;
                            }
                            let series = format!("{key}|{}", series_of(&p.attributes));
                            if let Some((before, then)) = self.counters.insert(series, (v, at)) {
                                let secs = (at - then).num_milliseconds() as f64 / 1000.0;
                                // A reset reads lower than before: no rate from it.
                                if secs > 0.0 && v >= before {
                                    rate += (v - before) / secs;
                                    seen = true;
                                }
                            }
                        }
                        if seen {
                            let unit = if unit == MetricUnit::Bytes { MetricUnit::BytesPerSecond } else { MetricUnit::PerSecond };
                            self.put(format!("{key}.rate"), format!("{label} rate"), unit, rate, now);
                        }
                    }
                    // Up and down — connections open, queue depth: a sum across attributes.
                    Some(Data::Sum(s)) => {
                        let values: Vec<f64> = s.data_points.iter().map(value_of).filter(|v| v.is_finite()).collect();
                        if !values.is_empty() {
                            self.put(key, label, unit, values.iter().sum(), now);
                        }
                    }
                    Some(Data::Histogram(h)) => self.take_histogram(&key, &label, unit, &h, now),
                    None => {}
                }
            }
        }
    }

    /// A histogram's p95 over what arrived since the last export, and its rate.
    fn take_histogram(&mut self, key: &str, label: &str, unit: MetricUnit, h: &proto::Histogram, now: DateTime<Utc>) {
        let mut merged: Vec<u64> = Vec::new();
        let mut bounds: Vec<f64> = Vec::new();
        for p in &h.data_points {
            if p.bucket_counts.is_empty() || (!bounds.is_empty() && p.explicit_bounds != bounds) {
                continue;
            }
            let delta: Vec<u64> = if h.aggregation_temporality == 1 {
                p.bucket_counts.clone()
            } else {
                let series = format!("{key}|{}", p.attributes.iter().map(|kv| format!("{}={}", kv.key, text_of(&kv.value))).collect::<Vec<_>>().join(","));
                let before = self.buckets.insert(series, p.bucket_counts.clone());
                match before {
                    Some(before) if before.len() == p.bucket_counts.len() && before.iter().zip(&p.bucket_counts).all(|(b, a)| a >= b) => {
                        p.bucket_counts.iter().zip(before).map(|(a, b)| a - b).collect()
                    }
                    // The first reading, or after a reset: nothing to compare yet.
                    _ => continue,
                }
            };
            if merged.is_empty() {
                merged = delta;
                bounds = p.explicit_bounds.clone();
            } else {
                merged.iter_mut().zip(delta).for_each(|(m, d)| *m += d);
            }
        }
        let total: u64 = merged.iter().sum();
        if total == 0 {
            return;
        }
        if let Some(p95) = quantile(0.95, &bounds, &merged) {
            self.put(format!("{key}.p95"), format!("{label} p95"), unit, p95, now);
        }
    }

    pub fn summaries(&self, from: DateTime<Utc>, to: DateTime<Utc>, limit: usize) -> Vec<TraceSummary> {
        let mut out: Vec<TraceSummary> = self
            .traces
            .iter()
            .filter_map(|(id, spans)| {
                let root = spans.iter().find(|s| s.parent_span_id.as_ref().is_none_or(|p| !spans.iter().any(|o| &o.span_id == p)))?;
                (root.started_at >= from && root.started_at <= to).then(|| TraceSummary {
                    trace_id: id.clone(),
                    root_service: root.service.clone(),
                    root_name: root.name.clone(),
                    started_at: root.started_at,
                    duration_ms: root.duration_ms,
                    error_count: Some(spans.iter().filter(|s| s.status == SpanStatus::Error).count()),
                })
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        out.truncate(limit);
        out
    }
}

/// The q-quantile of bucket counts with these upper bounds; the last bucket
/// is everything above the last bound and answers that bound.
fn quantile(q: f64, bounds: &[f64], counts: &[u64]) -> Option<f64> {
    let total: u64 = counts.iter().sum();
    if total == 0 || counts.len() != bounds.len() + 1 {
        return None;
    }
    let rank = q * total as f64;
    let mut below = 0u64;
    for (i, count) in counts.iter().enumerate() {
        if (below + count) as f64 >= rank {
            let Some(upper) = bounds.get(i) else {
                return bounds.last().copied();
            };
            let lower = if i == 0 { 0.0 } else { bounds[i - 1] };
            return Some(if *count == 0 { *upper } else { lower + (upper - lower) * ((rank - below as f64) / *count as f64) });
        }
        below += count;
    }
    bounds.last().copied()
}

// -------------------------------------------------------------- the listener

#[derive(Clone)]
struct Receiver {
    store: Arc<Mutex<Received>>,
    token: Option<String>,
    profile_id: String,
    profile_name: String,
}

/// The body as protobuf bytes or JSON, inflated when it says it is gzipped.
fn body_of(headers: &HeaderMap, body: &[u8]) -> std::result::Result<(Vec<u8>, bool), &'static str> {
    let gzipped = headers.get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()) == Some("gzip");
    let bytes = if gzipped {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(body)
            .take(MAX_INFLATED)
            .read_to_end(&mut out)
            .map_err(|_| "the body is not gzip")?;
        out
    } else {
        body.to_vec()
    };
    let json = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("application/json"));
    Ok((bytes, json))
}

/// The empty Export…ServiceResponse, in whichever encoding was sent.
fn accepted(json: bool) -> Response {
    if json {
        (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], "{}").into_response()
    } else {
        (StatusCode::OK, [(header::CONTENT_TYPE, "application/x-protobuf")], Vec::<u8>::new()).into_response()
    }
}

fn admitted(receiver: &Receiver, headers: &HeaderMap) -> bool {
    receiver.token.as_deref().is_none_or(|token| crate::local_listener::authorized(headers, token))
}

macro_rules! endpoint {
    ($name:ident, $kind:ident, $from_json:path, $take:ident $(, $extra:ident)*) => {
        async fn $name(State(r): State<Receiver>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
            if !admitted(&r, &headers) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let (bytes, json) = match body_of(&headers, &body) {
                Ok(parts) => parts,
                Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
            };
            let request = if json {
                match serde_json::from_slice::<Value>(&bytes) {
                    Ok(value) => $from_json(&value),
                    Err(_) => return (StatusCode::BAD_REQUEST, "the body is not JSON").into_response(),
                }
            } else {
                match proto::$kind::decode(bytes.as_slice()) {
                    Ok(request) => request,
                    Err(_) => return (StatusCode::BAD_REQUEST, "the body is not an OTLP protobuf").into_response(),
                }
            };
            r.store.lock().await.$take(request $(, &r.$extra)*);
            accepted(json)
        }
    };
}

endpoint!(traces, ExportTraceServiceRequest, json::traces, take_traces);
endpoint!(logs, ExportLogsServiceRequest, json::logs, take_logs, profile_id, profile_name);
endpoint!(metrics, ExportMetricsServiceRequest, json::metrics, take_metrics);

/// The task listening, stopped when the collector that owns it is dropped —
/// which is what frees the port when the connection is edited or removed.
struct Listening(tokio::task::JoinHandle<()>);

impl Drop for Listening {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn listen(address: SocketAddr, receiver: Receiver) -> Listening {
    Listening(tokio::spawn(async move {
        let app = Router::new()
            .route("/v1/traces", post(traces))
            .route("/v1/logs", post(logs))
            .route("/v1/metrics", post(metrics))
            .layer(DefaultBodyLimit::max(MAX_BODY))
            .with_state(receiver);
        // An edited connection's old listener may still hold the port for a
        // moment; try for a few seconds before saying so.
        for attempt in 0..20 {
            match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => {
                    if let Err(e) = axum::serve(listener, app).await {
                        tracing::warn!("OTLP receiver on {address} stopped: {e}");
                    }
                    return;
                }
                Err(e) if attempt == 19 => tracing::warn!("OTLP receiver could not listen on {address}: {e}"),
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            }
        }
    }))
}

// -------------------------------------------------------------- the collector

pub struct OtlpCollector {
    profile_id: String,
    profile_name: String,
    address: SocketAddr,
    store: Arc<Mutex<Received>>,
    _listening: Listening,
}

impl OtlpCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim();
        let address: SocketAddr = (if raw.is_empty() { DEFAULT_ADDRESS } else { raw })
            .parse()
            .map_err(|_| AppError::Config(format!("`{raw}` is not an address to listen on, such as {DEFAULT_ADDRESS}")))?;
        let token = secret.filter(|s| !s.is_empty());
        if !address.ip().is_loopback() && token.is_none() {
            return Err(AppError::Config(
                "Listening beyond this machine needs a token, so only your services can send to it.".into(),
            ));
        }
        let store = Arc::new(Mutex::new(Received::default()));
        let receiver = Receiver {
            store: Arc::clone(&store),
            token,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        };
        Ok(OtlpCollector {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            address,
            _listening: listen(address, receiver),
            store,
        })
    }
}

#[async_trait]
impl Collector for OtlpCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Otlp
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let now = Utc::now();
        let mut received = self.store.lock().await;
        let since = received.polled_at.unwrap_or(now);
        let minutes = ((now - since).num_milliseconds() as f64 / 60_000.0).max(1.0 / 60.0);
        let mut rows = Vec::new();
        let mut spans_total = 0.0;
        let mut lines_total = 0.0;
        for (service, tally) in &received.tally {
            let before = received.polled.get(service).copied().unwrap_or_default();
            let spans = (tally.spans - before.spans) as f64 / minutes;
            let errors = (tally.errors - before.errors) as f64 / minutes;
            let lines = (tally.lines - before.lines) as f64 / minutes;
            spans_total += spans;
            lines_total += lines;
            let quiet = tally.last.is_none_or(|t| now - t > Duration::minutes(5));
            let error_share = if spans > 0.0 { errors / spans * 100.0 } else { 0.0 };
            let health = if quiet {
                Health::Unknown
            } else if error_share >= 5.0 {
                Health::Degraded
            } else {
                Health::Healthy
            };
            rows.push(
                Row::new(service.clone(), service.clone(), health)
                    .summary(if quiet {
                        "nothing received for five minutes".to_string()
                    } else {
                        format!("{spans:.0} spans/min · {error_share:.1}% errors · {lines:.0} lines/min")
                    })
                    .cell("service", CellValue::text(service.clone()))
                    .cell("spans", CellValue::number(spans))
                    .cell("errors", CellValue::number(error_share))
                    .cell("lines", CellValue::number(lines))
                    .cell("last", tally.last.map(CellValue::timestamp).unwrap_or(CellValue::Null)),
            );
        }
        received.polled = received.tally.clone();
        received.polled_at = Some(now);

        let mut services = Section::new(
            "services",
            "Services sending",
            vec![
                ColumnSpec::text("service", "Service").primary(),
                ColumnSpec::metric("spans", "Spans/min", MetricUnit::Count),
                ColumnSpec::metric("errors", "Errors", MetricUnit::Percent),
                ColumnSpec::metric("lines", "Lines/min", MetricUnit::Count),
                ColumnSpec::timestamp("last", "Last data"),
            ],
        )
        .tracked()
        .empty_text("Nothing received yet.");
        services.rows = rows;

        let mut metrics = vec![
            Metric::new("spans_per_minute", "Spans per minute", spans_total, MetricUnit::Count),
            Metric::new("lines_per_minute", "Lines per minute", lines_total, MetricUnit::Count),
        ];
        let mut pushed: Vec<(&String, &Pushed)> = received.metrics.iter().collect();
        pushed.sort_by(|a, b| a.0.cmp(b.0));
        // What stopped arriving ten minutes ago is not a reading any more.
        metrics.extend(
            pushed
                .into_iter()
                .filter(|(_, p)| now - p.at < Duration::minutes(10))
                .map(|(key, p)| Metric::new(key, &p.label, p.value, p.unit)),
        );

        let heard = received.tally.values().filter_map(|t| t.last).max();
        let health = match heard {
            None => Health::Unknown,
            Some(t) if now - t > Duration::minutes(5) => Health::Unknown,
            _ if services.rows.iter().any(|r| r.health == Health::Degraded) => Health::Degraded,
            _ => Health::Healthy,
        };
        let summary = match heard {
            None => format!("Listening on {} — nothing received yet. Point an OTLP exporter at http://{}", self.address, self.address),
            Some(_) => format!(
                "{} service{} · {spans_total:.0} spans/min · {lines_total:.0} lines/min",
                services.rows.len(),
                if services.rows.len() == 1 { "" } else { "s" }
            ),
        };
        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Otlp,
            health,
            summary,
            metrics,
            sections: vec![services],
            checked_at: now,
            latency_ms: 0,
            error: None,
        })
    }

    async fn fetch_logs(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let received = self.store.lock().await;
        let mut lines: Vec<LogEvent> = received.lines.iter().filter(|l| l.timestamp > since).cloned().collect();
        lines.sort_by_key(|l| l.timestamp);
        lines.truncate(limit);
        Ok(lines)
    }

    async fn detail(&self, query: &DetailQuery) -> Result<serde_json::Value> {
        let received = self.store.lock().await;
        match query {
            DetailQuery::TracesInWindow { from, to, limit } => Ok(serde_json::to_value(received.summaries(*from, *to, *limit))?),
            DetailQuery::Trace { trace_id } => {
                let spans = received
                    .traces
                    .get(trace_id)
                    .cloned()
                    .ok_or_else(|| AppError::Collector(format!("trace {trace_id} is not among the ones received")))?;
                Ok(serde_json::to_value(Trace::assemble(trace_id, spans))?)
            }
            _ => Err(AppError::Collector("this connection receives OTLP and answers trace queries only".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(key: &str, value: &str) -> proto::KeyValue {
        proto::KeyValue {
            key: key.into(),
            value: Some(proto::AnyValue { value: Some(proto::any_value::Value::StringValue(value.into())) }),
        }
    }

    fn resource(service: &str) -> Option<proto::Resource> {
        Some(proto::Resource { attributes: vec![kv("service.name", service)] })
    }

    #[test]
    fn protobuf_spans_round_trip_into_a_trace() {
        let request = proto::ExportTraceServiceRequest {
            resource_spans: vec![proto::ResourceSpans {
                resource: resource("checkout"),
                scope_spans: vec![proto::ScopeSpans {
                    spans: vec![
                        proto::Span {
                            trace_id: vec![0xab; 16],
                            span_id: vec![1; 8],
                            parent_span_id: Vec::new(),
                            name: "POST /checkout".into(),
                            start_time_unix_nano: 1_789_000_000_000_000_000,
                            end_time_unix_nano: 1_789_000_000_250_000_000,
                            attributes: vec![kv("http.status_code", "502")],
                            status: Some(proto::Status { message: "upstream".into(), code: 2 }),
                        },
                        proto::Span {
                            trace_id: vec![0xab; 16],
                            span_id: vec![2; 8],
                            parent_span_id: vec![1; 8],
                            name: "charge".into(),
                            start_time_unix_nano: 1_789_000_000_010_000_000,
                            end_time_unix_nano: 1_789_000_000_200_000_000,
                            attributes: Vec::new(),
                            status: None,
                        },
                    ],
                }],
            }],
        };
        // Through the wire format, the way an SDK sends it.
        let decoded = proto::ExportTraceServiceRequest::decode(request.encode_to_vec().as_slice()).unwrap();
        let mut received = Received::default();
        received.take_traces(decoded);
        let id = "ab".repeat(16);
        let at = at_nanos(1_789_000_000_000_000_000);
        let found = received.summaries(at - Duration::minutes(1), at + Duration::minutes(1), 10);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].trace_id.as_str(), found[0].root_name.as_str(), found[0].error_count), (id.as_str(), "POST /checkout", Some(1)));
        assert!((found[0].duration_ms - 250.0).abs() < 1e-6);
        let trace = Trace::assemble(&id, received.traces[&id].clone());
        assert_eq!(trace.span_count, 2);
    }

    #[test]
    fn json_logs_read_like_protobuf_ones_with_the_trace_id_in_the_line() {
        let body = serde_json::json!({
            "resourceLogs": [{
                "resource": { "attributes": [{ "key": "service.name", "value": { "stringValue": "orders" } }] },
                "scopeLogs": [{ "logRecords": [{
                    "timeUnixNano": "1789000000000000000",
                    "severityNumber": 17,
                    "body": { "stringValue": "payment declined" },
                    "traceId": "4bf92f3577b34da6a3ce929d0e0e4736"
                }] }]
            }]
        });
        let mut received = Received::default();
        received.take_logs(json::logs(&body), "otel", "telemetry");
        let line = received.lines.front().unwrap();
        assert_eq!((line.source.as_str(), line.level), ("orders", LogLevel::Error));
        assert_eq!(line.message, "payment declined trace_id=4bf92f3577b34da6a3ce929d0e0e4736");
    }

    #[test]
    fn a_cumulative_counter_becomes_a_rate_and_a_reset_is_not_a_negative_one() {
        let sum = |value: i64, seconds: u64| proto::ExportMetricsServiceRequest {
            resource_metrics: vec![proto::ResourceMetrics {
                resource: resource("orders"),
                scope_metrics: vec![proto::ScopeMetrics {
                    metrics: vec![proto::Metric {
                        name: "http.server.requests".into(),
                        unit: "1".into(),
                        data: Some(proto::metric::Data::Sum(proto::Sum {
                            data_points: vec![proto::NumberDataPoint {
                                attributes: vec![kv("route", "/orders")],
                                start_time_unix_nano: 0,
                                time_unix_nano: seconds * 1_000_000_000,
                                value: Some(proto::number_data_point::Value::AsInt(value)),
                            }],
                            aggregation_temporality: 2,
                            is_monotonic: true,
                        })),
                    }],
                }],
            }],
        };
        let mut received = Received::default();
        received.take_metrics(sum(100, 1_789_000_000));
        assert!(received.metrics.is_empty(), "one reading is no rate");
        received.take_metrics(sum(700, 1_789_000_060));
        let rate = &received.metrics["orders/http.server.requests.rate"];
        assert_eq!((rate.value, rate.unit), ((700.0 - 100.0) / 60.0, MetricUnit::PerSecond));
        received.take_metrics(sum(5, 1_789_000_120));
        assert_eq!(received.metrics["orders/http.server.requests.rate"].value, (700.0 - 100.0) / 60.0, "a reset leaves the last rate");
    }

    #[test]
    fn a_histogram_reports_the_p95_of_what_arrived_since_the_last_export() {
        // Bounds 0.1, 0.5, 1 (seconds) and a last bucket above 1.
        let h = |counts: Vec<u64>| proto::Histogram {
            data_points: vec![proto::HistogramDataPoint {
                attributes: Vec::new(),
                start_time_unix_nano: 0,
                time_unix_nano: 0,
                count: counts.iter().sum(),
                bucket_counts: counts,
                explicit_bounds: vec![0.1, 0.5, 1.0],
            }],
            aggregation_temporality: 2,
        };
        let mut received = Received::default();
        received.take_histogram("api/latency", "api · latency", MetricUnit::Seconds, &h(vec![90, 10, 0, 0]), Utc::now());
        assert!(received.metrics.is_empty(), "cumulative: the first export is only a baseline");
        // Since then: 50 under 0.1s, 50 between 0.5s and 1s.
        received.take_histogram("api/latency", "api · latency", MetricUnit::Seconds, &h(vec![140, 10, 50, 0]), Utc::now());
        // Rank 95 of 100 falls 45 into the (0.5, 1] bucket of 50.
        let expected = 0.5 + (1.0 - 0.5) * (45.0 / 50.0);
        assert!((received.metrics["api/latency.p95"].value - expected).abs() < 1e-9);
    }

    #[tokio::test]
    async fn an_exporter_posting_protobuf_and_gzipped_json_is_heard() {
        use std::io::Write;
        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "otel", "name": "telemetry", "kind": "otlp", "endpoint": "127.0.0.1:43187"
        }))
        .unwrap();
        let collector = OtlpCollector::new(&profile, Some("t0ken".into())).unwrap();
        let client = reqwest::Client::new();
        let spans = proto::ExportTraceServiceRequest {
            resource_spans: vec![proto::ResourceSpans {
                resource: resource("checkout"),
                scope_spans: vec![proto::ScopeSpans {
                    spans: vec![proto::Span {
                        trace_id: vec![7; 16],
                        span_id: vec![1; 8],
                        parent_span_id: Vec::new(),
                        name: "GET /cart".into(),
                        start_time_unix_nano: 1,
                        end_time_unix_nano: 2,
                        attributes: Vec::new(),
                        status: None,
                    }],
                }],
            }],
        }
        .encode_to_vec();
        let post = |path: &str, token: Option<&str>| {
            let request = client.post(format!("http://127.0.0.1:43187{path}"));
            match token {
                Some(token) => request.bearer_auth(token),
                None => request,
            }
        };

        // The listener starts on its own task: give it a moment.
        let mut answered = None;
        for _ in 0..60 {
            match post("/v1/traces", Some("t0ken")).header("content-type", "application/x-protobuf").body(spans.clone()).send().await {
                Ok(response) => {
                    answered = Some(response.status());
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        assert_eq!(answered, Some(reqwest::StatusCode::OK));
        let refused = post("/v1/traces", None).header("content-type", "application/x-protobuf").body(spans).send().await.unwrap();
        assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);

        let logs = serde_json::json!({ "resourceLogs": [{
            "resource": { "attributes": [{ "key": "service.name", "value": { "stringValue": "orders" } }] },
            "scopeLogs": [{ "logRecords": [{ "severityText": "WARN", "body": { "stringValue": "slow charge" } }] }]
        }] });
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(logs.to_string().as_bytes()).unwrap();
        let sent = post("/v1/logs", Some("t0ken"))
            .header("content-type", "application/json")
            .header("content-encoding", "gzip")
            .body(gz.finish().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(sent.status(), reqwest::StatusCode::OK);
        assert_eq!(sent.text().await.unwrap(), "{}", "an empty ExportLogsServiceResponse, in JSON");

        let snapshot = collector.collect().await.unwrap();
        assert_eq!(snapshot.health, Health::Healthy);
        let mut services: Vec<&str> = snapshot.sections[0].rows.iter().map(|r| r.name.as_str()).collect();
        services.sort();
        assert_eq!(services, vec!["checkout", "orders"]);
        let lines = collector.fetch_logs(DateTime::<Utc>::MIN_UTC, 10).await.unwrap();
        assert_eq!((lines.len(), lines[0].level), (1, LogLevel::Warn));
    }

    #[test]
    fn listening_beyond_the_machine_needs_a_token() {
        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "otel", "name": "telemetry", "kind": "otlp", "endpoint": "0.0.0.0:4318"
        }))
        .unwrap();
        assert!(OtlpCollector::new(&profile, None).is_err());
    }
}
