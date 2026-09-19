//! Snapshot write-out: root's own judgement, pushed to a warehouse.
//!
//! Everything else root does is a read. `promquery.rs` asks a Prometheus or a
//! gigapipe what a number did; `history.rs` keeps a few hours in a ring buffer
//! and says plainly that it is not persisted. Quit root and the derived
//! health, the latency, and every metric it computed are gone. The raw numbers
//! survive wherever they came from — but root's *reading* of them does not,
//! and that reading is the only thing in the estate that nothing else
//! produces.
//!
//! This module writes that reading out.
//!
//! ## Why line protocol
//!
//! Gigapipe accepts OTLP at `/v1/metrics`, Datadog at `/api/v2/series`, and
//! Influx line protocol at `/influx/api/v2/write`. The stored rows are the
//! same either way. Line protocol is plain text, so emitting it is `format!`;
//! OTLP would mean protobuf and gRPC and Prometheus remote-write would mean
//! protobuf plus snappy. Zero new crates against two substantial ones, for an
//! identical payload.
//!
//! The path is InfluxDB's own, but this targets gigapipe specifically: it
//! authenticates with basic auth and sends no `org` or `bucket`, which a real
//! InfluxDB 2.x requires along with token auth. Pointing this at InfluxDB
//! itself is a different connection type, not a query-parameter away.
//!
//! ## Opt-in, per connection, off by default
//!
//! root writes nothing anywhere except its own config and journal. A tool that
//! only reads cannot corrupt what it is watching, and that property is worth
//! more than the convenience of defaulting this on. Writing snapshots into a
//! shared warehouse makes root a *producer* of observability data — rows that
//! someone else's dashboard will eventually depend on — which is a different
//! product with a different set of obligations.
//!
//! So a connection opts in by naming a warehouse, one connection at a time,
//! and an estate that never opts in behaves exactly as it did before.
//!
//! ## Best-effort, always
//!
//! A warehouse that is down must never hold up a poll cycle or make a healthy
//! service look unhealthy. The write is therefore fire-and-forget with its own
//! timeout, and its failure is logged rather than surfaced as connection
//! state. The alternative — a service marked degraded because the place root
//! files its opinions is unreachable — would be root reporting on itself and
//! calling it infrastructure.
//!
//! ## Whose numbers these are
//!
//! Fields carrying root's own verdict are prefixed `root_`, so a query can
//! tell them apart from the collector's metrics, which keep their own keys. A
//! value root could not compute is omitted rather than written as zero: a gap
//! in a series reads as "not measured", where a zero reads as "measured, and
//! it was nothing".

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::model::{Health, HealthSnapshot, ServiceKind};
use crate::promquery::Auth;

/// Gigapipe's line-protocol ingest path.
const WRITE_PATH: &str = "/influx/api/v2/write";

/// Ceiling on a rejection body quoted back in an error. Enough for a real
/// message, short of a warehouse returning an HTML error page in full.
const MAX_ERROR_BODY: usize = 400;

/// Measurement used when a connection has no usable name.
const FALLBACK_MEASUREMENT: &str = "root";

/// Where a connection's snapshots are written, when anywhere.
///
/// Shaped like [`crate::promquery::HistorySource`] on purpose: opt-in by
/// presence, pointing at another profile by id rather than carrying a second
/// copy of its endpoint and credentials. Deleting the warehouse profile then
/// stops the writes instead of leaving a stale URL behind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSink {
    /// The connection that receives them, as a profile id.
    pub warehouse_profile_id: String,
    /// Measurement name. Empty means the connection's own name is used.
    ///
    /// A rename then starts a new measurement, which is why the profile id
    /// rides along as a tag: that is the identifier which survives one.
    #[serde(default)]
    pub measurement: String,
}

impl SnapshotSink {
    pub fn is_configured(&self) -> bool {
        !self.warehouse_profile_id.trim().is_empty()
    }

    /// Measurement for a snapshot, falling back to the connection's name.
    pub fn measurement_for(&self, snapshot: &HealthSnapshot) -> String {
        let chosen = if self.measurement.trim().is_empty() {
            snapshot.profile_name.trim()
        } else {
            self.measurement.trim()
        };
        if chosen.is_empty() {
            FALLBACK_MEASUREMENT.to_string()
        } else {
            chosen.to_string()
        }
    }
}

/// Health as a number, so a warehouse can graph it.
///
/// Ordered by severity rather than by declaration, so `max()` over a window is
/// the worst the connection was and a rising line means a worsening service.
/// The label is not also written as a tag: a tag whose value changes creates a
/// new series every time health does, which is the one shape a time-series
/// store handles badly.
fn health_code(health: Health) -> i64 {
    match health {
        Health::Unknown => 0,
        Health::Healthy => 1,
        Health::Degraded => 2,
        Health::Down => 3,
    }
}

/// The kind's wire name, as serde already writes it everywhere else.
///
/// Derived rather than matched so a new collector cannot be added without its
/// tag — there are twenty-eight kinds and a second exhaustive match over them
/// would drift the first time someone forgot this file existed.
fn kind_tag(kind: ServiceKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Escape a measurement name: commas and spaces only.
fn escape_measurement(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(c, ',' | ' ') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape a tag key, a tag value, or a field key: commas, equals, spaces.
fn escape_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(c, ',' | '=' | ' ') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape a string field value, which is quoted rather than bare.
fn escape_string_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    for c in raw.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Line protocol has no spelling for NaN or infinity, and a receiver rejects
/// the whole batch over one of them. A metric that is not a finite number was
/// not measured, so it is left out.
fn float_field(value: f64) -> Option<String> {
    value.is_finite().then(|| format!("{value}"))
}

/// One snapshot as one line of Influx line protocol, or nothing.
///
/// Returns `None` when there is nothing to say: a point with no fields is
/// rejected by every receiver, and a snapshot without a usable timestamp
/// cannot be placed on a timeline.
pub fn encode(snapshot: &HealthSnapshot, sink: &SnapshotSink) -> Option<String> {
    let timestamp = snapshot.checked_at.timestamp_nanos_opt()?;

    let mut fields: Vec<String> = Vec::with_capacity(snapshot.metrics.len() + 3);

    // root's own verdict first, so a line is readable left to right.
    fields.push(format!("root_health={}i", health_code(snapshot.health)));
    fields.push(format!("root_latency_ms={}i", snapshot.latency_ms));
    if let Some(error) = snapshot.error.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        fields.push(format!("root_error=\"{}\"", escape_string_value(error)));
    }

    // Then the collector's numbers, under the keys the rest of root uses for
    // them — a series named `memory` here and `memory` in a forecast is the
    // same measure, and renaming it at the boundary would break that.
    for metric in &snapshot.metrics {
        let key = metric.key.trim();
        if key.is_empty() {
            continue;
        }
        let Some(value) = float_field(metric.value) else {
            continue;
        };
        fields.push(format!("{}={}", escape_key(key), value));
    }

    // Only ever false if a snapshot arrived with no timestamp-bearing verdict
    // at all, which the two pushes above make impossible — kept because the
    // receiver's rule is about the line, not about how it was built.
    if fields.is_empty() {
        return None;
    }

    let measurement = escape_measurement(&sink.measurement_for(snapshot));
    let profile = escape_key(snapshot.profile_id.trim());
    let kind = escape_key(&kind_tag(snapshot.kind));

    // An empty tag value is silently dropped by the receiver rather than
    // rejected, which would lose the one identifier that survives a rename.
    let mut tags = String::new();
    if !profile.is_empty() {
        tags.push_str(&format!(",profile={profile}"));
    }
    if !kind.is_empty() {
        tags.push_str(&format!(",kind={kind}"));
    }

    Some(format!(
        "{measurement}{tags} {} {timestamp}",
        fields.join(",")
    ))
}

/// POST a line-protocol body to a gigapipe.
///
/// No `precision` parameter: nanoseconds is line protocol's documented default
/// and sending a parameter a receiver might validate buys nothing over sending
/// timestamps in the unit it already assumes.
pub async fn write(
    client: &reqwest::Client,
    base: &str,
    auth: &Auth,
    body: String,
) -> Result<()> {
    let url = format!("{}{WRITE_PATH}", base.trim_end_matches('/'));
    let request = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body);

    let request = match (&auth.username, &auth.secret) {
        (Some(user), secret) => request.basic_auth(user, secret.as_ref()),
        (None, Some(token)) => request.bearer_auth(token),
        (None, None) => request,
    };

    let response = request
        .send()
        .await
        .map_err(|e| AppError::Collector(format!("{base} did not accept the write: {e}")))?;

    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    // The status line alone does not distinguish a malformed line from a
    // wrong endpoint, and both are operator errors worth telling apart.
    let detail = response.text().await.unwrap_or_default();
    let detail = detail.trim();
    let detail: String = if detail.is_empty() {
        String::new()
    } else {
        format!(" — {}", detail.chars().take(MAX_ERROR_BODY).collect::<String>())
    };
    Err(AppError::Collector(format!(
        "{base} refused the snapshot write with {status}{detail}"
    )))
}

/// Resolve a connection's sink and write one snapshot to it.
///
/// `Ok(false)` means the connection did not opt in, which is the answer for
/// almost every connection and is not a failure. Everything is re-resolved per
/// write rather than captured when the poller starts: repointing or
/// re-crendentialing the *warehouse* restarts only the warehouse's own poller,
/// so anything cached here would keep writing at the old address. A keychain
/// read every poll interval is the cheaper side of that trade, and it happens
/// only for connections that opted in.
pub async fn write_snapshot(state: &crate::state::AppState, snapshot: &HealthSnapshot) -> Result<bool> {
    let (sink, warehouse) = {
        let config = state.config.read().await;
        let Some(profile) = config.profiles.iter().find(|p| p.id == snapshot.profile_id) else {
            return Ok(false);
        };
        let Some(sink) = profile.snapshot_sink.clone().filter(|s| s.is_configured()) else {
            return Ok(false);
        };
        let warehouse = config
            .profiles
            .iter()
            .find(|p| p.id == sink.warehouse_profile_id)
            .cloned()
            .ok_or_else(|| {
                AppError::Collector(
                    "this connection writes snapshots to a warehouse that is no longer configured"
                        .to_string(),
                )
            })?;
        (sink, warehouse)
    };

    let Some(line) = encode(snapshot, &sink) else {
        return Ok(false);
    };

    let auth = Auth {
        username: warehouse.username.clone(),
        secret: crate::secrets::get_secret(&warehouse.secret_key())
            .await
            .unwrap_or(None),
    };
    let client = crate::collectors::http_client(
        warehouse.options.bool("insecureTls"),
        std::time::Duration::from_secs(10),
    )?;

    write(&client, &warehouse.endpoint, &auth, line).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Metric, MetricUnit};
    use chrono::{TimeZone, Utc};

    fn snapshot() -> HealthSnapshot {
        HealthSnapshot {
            profile_id: "p1".into(),
            profile_name: "prod-docker".into(),
            kind: ServiceKind::Docker,
            health: Health::Degraded,
            summary: "2 of 9 containers unhealthy".into(),
            metrics: vec![
                Metric::new("memory", "Memory", 9.4e9, MetricUnit::Bytes),
                Metric::new("cpu", "CPU", 41.5, MetricUnit::Percent),
            ],
            sections: Vec::new(),
            checked_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
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

    #[test]
    fn writes_verdict_tags_and_metrics_on_one_line() {
        let line = encode(&snapshot(), &sink()).expect("a line");
        assert_eq!(
            line,
            concat!(
                "prod-docker,profile=p1,kind=docker ",
                "root_health=2i,root_latency_ms=42i,memory=9400000000,cpu=41.5 ",
                "1700000000000000000"
            )
        );
    }

    #[test]
    fn health_rises_with_severity() {
        assert!(health_code(Health::Down) > health_code(Health::Degraded));
        assert!(health_code(Health::Degraded) > health_code(Health::Healthy));
        assert!(health_code(Health::Healthy) > health_code(Health::Unknown));
    }

    #[test]
    fn an_explicit_measurement_wins_over_the_connection_name() {
        let sink = SnapshotSink {
            warehouse_profile_id: "p9".into(),
            measurement: "estate".into(),
        };
        let line = encode(&snapshot(), &sink).unwrap();
        assert!(line.starts_with("estate,profile=p1"), "{line}");
    }

    #[test]
    fn a_nameless_connection_still_writes() {
        let mut snap = snapshot();
        snap.profile_name = "   ".into();
        let line = encode(&snap, &sink()).unwrap();
        assert!(line.starts_with("root,profile=p1"), "{line}");
    }

    #[test]
    fn spaces_and_commas_are_escaped_everywhere_they_can_appear() {
        let mut snap = snapshot();
        snap.profile_name = "prod docker, eu".into();
        snap.metrics = vec![Metric::new("disk free", "Disk", 1.0, MetricUnit::Bytes)];
        let line = encode(&snap, &sink()).unwrap();
        assert!(line.starts_with(r"prod\ docker\,\ eu,profile=p1"), "{line}");
        assert!(line.contains(r"disk\ free=1"), "{line}");
    }

    #[test]
    fn an_unreachable_snapshot_carries_its_error_as_a_quoted_field() {
        let snap = HealthSnapshot::unreachable(
            "p1",
            "prod-docker",
            ServiceKind::Docker,
            r#"connection refused on "localhost:2375""#.into(),
            10,
        );
        let line = encode(&snap, &sink()).unwrap();
        assert!(line.contains("root_health=3i"), "{line}");
        assert!(
            line.contains(r#"root_error="connection refused on \"localhost:2375\"""#),
            "{line}"
        );
    }

    #[test]
    fn a_metric_that_is_not_a_finite_number_is_left_out_rather_than_zeroed() {
        let mut snap = snapshot();
        snap.metrics = vec![
            Metric::new("ratio", "Ratio", f64::NAN, MetricUnit::Count),
            Metric::new("rate", "Rate", f64::INFINITY, MetricUnit::Count),
            Metric::new("real", "Real", 3.0, MetricUnit::Count),
        ];
        let line = encode(&snap, &sink()).unwrap();
        assert!(!line.contains("ratio"), "{line}");
        assert!(!line.contains("rate="), "{line}");
        assert!(line.contains("real=3"), "{line}");
    }

    #[test]
    fn a_connection_with_no_metrics_still_reports_root_s_verdict() {
        let mut snap = snapshot();
        snap.metrics.clear();
        let line = encode(&snap, &sink()).unwrap();
        assert!(line.contains("root_health=2i"), "{line}");
        assert!(line.contains("root_latency_ms=42i"), "{line}");
    }

    #[test]
    fn every_kind_has_a_tag_and_none_of_them_is_unknown() {
        for kind in crate::registry::catalog().iter().map(|d| d.kind) {
            let tag = kind_tag(kind);
            assert!(!tag.is_empty(), "{kind:?} has no wire name");
            assert_ne!(tag, "unknown", "{kind:?} did not serialize");
            assert_eq!(tag, escape_key(&tag), "{kind:?} needs escaping: {tag}");
        }
    }

    #[test]
    fn a_sink_without_a_warehouse_is_not_configured() {
        assert!(!SnapshotSink::default().is_configured());
        assert!(!SnapshotSink {
            warehouse_profile_id: "  ".into(),
            measurement: "estate".into(),
        }
        .is_configured());
        assert!(sink().is_configured());
    }
}
