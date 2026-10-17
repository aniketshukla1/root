//! Everything the UI can ask the backend to do.
//!
//! These are plain async functions over `&Arc<AppState>`, reachable two ways:
//! `tauri_cmds.rs` wraps each one for the desktop app's IPC, and `rpc.rs`
//! dispatches the same set by name over HTTP. One implementation, because two
//! would drift the first week and in the direction nobody tests.

use std::sync::Arc;

use serde::Serialize;

use crate::agent::{self, AgentTurn};
use crate::config::{ConnectionProfile, LlmSettings};
use crate::error::{AppError, Result};
use crate::history::{self, HistogramBucket, MetricSeries};
use crate::incidents::{self, Incident};
use crate::llm::{build_provider, ChatMessage};
use crate::model::{Alert, DashboardState, Health, HealthSnapshot, LogEvent};
use crate::registry::{self, ServiceDescriptor};
use crate::secrets;
use crate::state::{AppState, LogFilter, PendingConfirmation};
use crate::team;
use crate::supervisor;


// ---------------------------------------------------------------- dashboard

pub async fn get_dashboard(state: &Arc<AppState>) -> Result<DashboardState> {
    Ok(state.dashboard().await)
}

pub async fn refresh_connection(state: &Arc<AppState>, profile_id: String) -> Result<HealthSnapshot> {
    supervisor::poll_once(state, &profile_id).await
}

pub async fn acknowledge_alert(state: &Arc<AppState>, alert_id: String) -> Result<Alert> {
    state.alerts.write().await.acknowledge(&alert_id)
}

/// Run a mutating action the operator clicked on a section row.
///
/// This shares `action_for` with the assistant so a button and a tool call
/// resolve to exactly the same operation. The gate differs by design: the agent
/// must ask, whereas a click *is* the operator's decision — the UI confirms
/// before calling here.
pub async fn run_row_action(
    state: &Arc<AppState>,
    profile_id: String,
    tool: String,
    args: serde_json::Value,
) -> Result<String> {
    let action = crate::agent::tools::action_for(&tool, &args)?;
    let result = state.collector(&profile_id).await?.run_action(&action).await?;
    // Reflect the change immediately rather than waiting for the next poll.
    let _ = supervisor::poll_once(state, &profile_id).await;
    Ok(result)
}

// --------------------------------------------------------------------- logs

pub async fn query_logs(state: &Arc<AppState>, filter: LogFilter) -> Result<Vec<LogEvent>> {
    Ok(state.logs.read().await.query(&filter))
}

pub async fn clear_logs(state: &Arc<AppState>) -> Result<()> {
    state.logs.write().await.clear();
    Ok(())
}

/// One clustered message template and everything needed to rank and draw it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPattern {
    /// Stable across refreshes for the same template, so a selection survives
    /// new lines arriving.
    pub id: String,
    pub template: String,
    pub count: usize,
    /// Highest severity seen in this cluster — what the row is coloured by.
    pub level: crate::model::LogLevel,
    pub error_count: usize,
    /// Which connections and components emit it, capped for display.
    pub profiles: Vec<String>,
    pub sources: Vec<String>,
    pub first_seen: chrono::DateTime<chrono::Utc>,
    pub last_seen: chrono::DateTime<chrono::Utc>,
    /// Per-bucket counts across the queried window, for the row's sparkline.
    pub buckets: Vec<usize>,
    /// A couple of real lines, since a template alone reads as abstract.
    pub samples: Vec<String>,
}

/// How many distinct values of a facet to name before giving up on listing.
const PATTERN_FACET_CAP: usize = 4;
const PATTERN_SAMPLE_CAP: usize = 3;
const PATTERN_SPARK_BUCKETS: usize = 24;

/// Cluster the log buffer into message templates.
///
/// This is the analytical half of the log view: a tail shows you the last 200
/// lines, which is whatever happened to be noisiest; this shows every distinct
/// *thing* that happened and how often, so a 3-line panic is as visible as a
/// 400-line connection storm.
///
/// Ranking is severity first, then volume. Pure volume ranking — what a log
/// aggregator usually does — buries the panic under the storm, which is the
/// opposite of what an operator needs.
pub async fn log_patterns(
    state: &Arc<AppState>,
    filter: LogFilter,
    minutes: Option<i64>,
) -> Result<Vec<LogPattern>> {
    let until = chrono::Utc::now();
    let since = until - chrono::Duration::minutes(minutes.unwrap_or(60).clamp(1, 1440));

    let mut filter = filter;
    filter.since = Some(since);
    // Counts must reflect the whole window, not one display page.
    filter.limit = Some(5000);
    // Selecting a pattern must not change which patterns exist.
    filter.pattern = None;

    let events = state.logs.read().await.query(&filter);

    let mut drain = crate::patterns::Drain::new(crate::patterns::DrainConfig::default());
    // Cluster id -> the events that landed in it.
    let mut members: std::collections::HashMap<usize, Vec<&LogEvent>> =
        std::collections::HashMap::new();
    for event in &events {
        if let Some(id) = drain.add(&event.message) {
            members.entry(id).or_default().push(event);
        }
    }

    let span_ms = (until - since).num_milliseconds().max(1) as f64;
    let mut out: Vec<LogPattern> = Vec::new();

    for (id, cluster) in drain.clusters().iter().enumerate() {
        let Some(group) = members.get(&id) else {
            continue;
        };
        if group.is_empty() {
            continue;
        }

        let mut buckets = vec![0usize; PATTERN_SPARK_BUCKETS];
        let mut level = crate::model::LogLevel::Trace;
        let mut error_count = 0usize;
        let mut first_seen = group[0].timestamp;
        let mut last_seen = group[0].timestamp;
        let mut profiles: Vec<String> = Vec::new();
        let mut sources: Vec<String> = Vec::new();
        let mut samples: Vec<String> = Vec::new();

        for event in group {
            level = level.max(event.level);
            if event.level >= crate::model::LogLevel::Error {
                error_count += 1;
            }
            first_seen = first_seen.min(event.timestamp);
            last_seen = last_seen.max(event.timestamp);

            let offset = (event.timestamp - since).num_milliseconds() as f64;
            let slot = ((offset / span_ms) * PATTERN_SPARK_BUCKETS as f64) as usize;
            buckets[slot.min(PATTERN_SPARK_BUCKETS - 1)] += 1;

            if !profiles.contains(&event.profile_name) && profiles.len() < PATTERN_FACET_CAP {
                profiles.push(event.profile_name.clone());
            }
            if !sources.contains(&event.source) && sources.len() < PATTERN_FACET_CAP {
                sources.push(event.source.clone());
            }
            if samples.len() < PATTERN_SAMPLE_CAP && !samples.contains(&event.message) {
                samples.push(event.message.clone());
            }
        }

        let template = cluster.template_string();
        out.push(LogPattern {
            id: template_id(&template),
            template,
            count: group.len(),
            level,
            error_count,
            profiles,
            sources,
            first_seen,
            last_seen,
            buckets,
            samples,
        });
    }

    out.sort_by(|a, b| b.level.cmp(&a.level).then(b.count.cmp(&a.count)));
    Ok(out)
}

/// A short stable id for a template string.
///
/// FNV-1a rather than a crypto hash: this only has to be deterministic and
/// collision-resistant enough to key a list in the UI.
fn template_id(template: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in template.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("p{hash:016x}")
}

/// Log volume by severity over a window, for the activity histogram.
///
/// Bucketing happens here rather than in the UI so it spans the whole server
/// buffer instead of only the slice the client happens to be holding.
pub async fn log_histogram(
    state: &Arc<AppState>,
    filter: LogFilter,
    minutes: Option<i64>,
    buckets: Option<usize>,
) -> Result<Vec<HistogramBucket>> {
    let until = chrono::Utc::now();
    let since = until - chrono::Duration::minutes(minutes.unwrap_or(60).clamp(1, 1440));

    let mut filter = filter;
    filter.since = Some(since);
    // Bucket counts must reflect the whole window, not a display page.
    filter.limit = Some(5000);

    let events = state.logs.read().await.query(&filter);
    Ok(history::histogram(&events, since, until, buckets.unwrap_or(60)))
}

// -------------------------------------------------------------- correlation

/// One connection's activity inside a correlation window.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrelationLane {
    pub profile_id: String,
    pub profile_name: String,
    pub kind: crate::model::ServiceKind,
    pub total: usize,
    pub error_count: usize,
    /// Newest first, capped — the lane is a summary, not a log view.
    pub events: Vec<LogEvent>,
    /// Metrics that moved meaningfully during the window.
    pub movements: Vec<MetricMovement>,
    /// Traces that spent time in *this* connection during the window.
    ///
    /// A trace is not one service's, which is the whole reason it explains
    /// anything: the spans of one request land on the lane of whichever
    /// connection each service maps to, so a slow checkout shows its database
    /// time on the database's lane at the same instant as its own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub traces: Vec<LaneTrace>,
}

/// One trace's presence on one lane.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneTrace {
    pub trace_id: String,
    /// What the whole request was, named by its root span.
    pub root_service: String,
    pub root_name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// End to end, across every service.
    pub duration_ms: f64,
    /// Time this connection's own service spent, excluding what it was waiting
    /// on downstream. The number that says whether this lane is the cause or a
    /// bystander.
    pub self_ms: f64,
    /// Spans this connection contributed.
    pub span_count: usize,
    pub error_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricMovement {
    pub key: String,
    pub label: String,
    pub unit: crate::model::MetricUnit,
    pub from: f64,
    pub to: f64,
    pub change_pct: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Correlation {
    pub center: chrono::DateTime<chrono::Utc>,
    pub from: chrono::DateTime<chrono::Utc>,
    pub to: chrono::DateTime<chrono::Utc>,
    pub lanes: Vec<CorrelationLane>,
    pub alerts: Vec<Alert>,
}

/// Ask every connection for traces in the window, and put each trace's time on
/// the lane of the service that spent it.
///
/// Two things make this worth the requests it costs.
///
/// A trace crosses services, so the interesting fact is never "this store holds
/// a trace" — it is *which* of the connections already on screen that trace was
/// inside, and for how long. `ServiceTime::self_ms` is the number that
/// separates a cause from a bystander: a span that took 900ms because its child
/// took 890 did not spend 900.
///
/// And nothing here knows what a gigapipe is. Every collector is asked; the ones
/// with no traces refuse and are skipped, so a collector that grows a trace
/// store later lands on these lanes without a line changing in this function.
async fn attach_traces(
    state: &Arc<AppState>,
    lanes: &mut [CorrelationLane],
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) {
    // Resolve a span's service name to a lane. Connections are matched by name
    // first, then by the runtime an operator declared for them — the same
    // declaration `runtime.rs` reconciles against root's own inference — so a
    // service called `checkout-api` finds the connection watching it even when
    // the connection is named something else.
    let runtimes: Vec<(usize, String, String)> = lanes
        .iter()
        .enumerate()
        .map(|(i, lane)| (i, lane.profile_name.to_lowercase(), lane.profile_id.clone()))
        .collect();
    let lane_for = |service: &str| -> Option<usize> {
        let service = service.trim().to_lowercase();
        if service.is_empty() {
            return None;
        }
        runtimes
            .iter()
            .find(|(_, name, id)| *name == service || *id == service)
            .map(|(i, _, _)| *i)
    };

    let mut found: Vec<(usize, LaneTrace)> = Vec::new();

    let lane_ids: Vec<String> = lanes.iter().map(|l| l.profile_id.clone()).collect();
    for (index, profile_id) in lane_ids.into_iter().enumerate() {
        let Ok(collector) = state.collector(&profile_id).await else {
            continue;
        };

        // A collector without traces refuses, and refusing is not a failure.
        let Ok(value) = collector
            .detail(&crate::collectors::DetailQuery::TracesInWindow {
                from,
                to,
                limit: TRACE_SEARCH_CAP,
            })
            .await
        else {
            continue;
        };
        let Ok(mut summaries) =
            serde_json::from_value::<Vec<crate::traces::TraceSummary>>(value)
        else {
            continue;
        };

        // Worst first, so the few that get assembled are the ones worth the
        // request: anything with errors, then the slowest.
        summaries.sort_by(|a, b| {
            b.error_count
                .unwrap_or(0)
                .cmp(&a.error_count.unwrap_or(0))
                .then_with(|| {
                    b.duration_ms
                        .partial_cmp(&a.duration_ms)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        summaries.truncate(TRACE_DETAIL_CAP);

        for summary in summaries {
            let Ok(assembled) = collector
                .detail(&crate::collectors::DetailQuery::Trace {
                    trace_id: summary.trace_id.clone(),
                })
                .await
            else {
                continue;
            };
            let Ok(trace) = serde_json::from_value::<crate::traces::Trace>(assembled) else {
                continue;
            };

            for service in &trace.services {
                // A service root does not recognise lands on the store's own
                // lane, which is where the trace was actually read from. It is
                // still evidence, and dropping it would hide a whole tier.
                let lane = lane_for(&service.service).unwrap_or(index);
                found.push((
                    lane,
                    LaneTrace {
                        trace_id: trace.trace_id.clone(),
                        root_service: trace.root_service.clone(),
                        root_name: trace.root_name.clone(),
                        started_at: trace.started_at,
                        duration_ms: trace.duration_ms,
                        self_ms: service.self_ms,
                        span_count: service.span_count,
                        error_count: service.error_count,
                    },
                ));
            }
        }
    }

    for (lane, trace) in found {
        lanes[lane].traces.push(trace);
    }
    for lane in lanes.iter_mut() {
        // Most time spent here first — the lane's own claim on the incident.
        lane.traces.sort_by(|a, b| {
            b.error_count
                .cmp(&a.error_count)
                .then_with(|| b.self_ms.partial_cmp(&a.self_ms).unwrap_or(std::cmp::Ordering::Equal))
        });
        lane.traces.truncate(LANE_TRACE_CAP);
    }
}

/// Lines per lane. Enough to see the shape; the log view has the rest.
const LANE_EVENT_CAP: usize = 12;
/// Traces searched per store inside the correlation window.
const TRACE_SEARCH_CAP: usize = 40;
/// Traces assembled in full. A search answers with summaries, and only an
/// assembled trace knows which services its time went to — so the worst few are
/// fetched and the rest are left as they came.
const TRACE_DETAIL_CAP: usize = 5;
/// Traces shown on any one lane.
const LANE_TRACE_CAP: usize = 6;
/// A metric has to move this much to be worth naming.
const MOVEMENT_THRESHOLD_PCT: f64 = 15.0;

/// What every system was doing around one moment.
///
/// This is the answer to "why did this build fail". Rather than interleaving
/// every service into one stream — which is what made the shared log view
/// unreadable — it groups by source and aligns on time, so the Docker OOM that
/// actually caused the Jenkins failure sits in its own lane at the same
/// instant, next to the database connection spike.
pub async fn correlate(
    state: &Arc<AppState>,
    at: chrono::DateTime<chrono::Utc>,
    window_secs: Option<i64>,
) -> Result<Correlation> {
    let half = chrono::Duration::seconds(window_secs.unwrap_or(180).clamp(10, 3600));
    let from = at - half;
    let to = at + half;

    // Every stream, including build console and diagnostics: during an
    // investigation the noisy sources are exactly the interesting ones.
    let events = state.logs.read().await.query(&LogFilter {
        since: Some(from),
        limit: Some(5000),
        ..Default::default()
    });

    let profiles: Vec<(String, String, crate::model::ServiceKind)> = state
        .config
        .read()
        .await
        .profiles
        .iter()
        .map(|p| (p.id.clone(), p.name.clone(), p.kind))
        .collect();

    let history = state.history.read().await;
    let mut lanes = Vec::new();

    for (profile_id, profile_name, kind) in profiles {
        let mut lane_events: Vec<LogEvent> = events
            .iter()
            .filter(|e| e.profile_id == profile_id && e.timestamp >= from && e.timestamp <= to)
            .cloned()
            .collect();
        lane_events.sort_by_key(|e| std::cmp::Reverse(e.timestamp));

        let error_count = lane_events
            .iter()
            .filter(|e| e.level >= crate::model::LogLevel::Error)
            .count();
        let total = lane_events.len();
        lane_events.truncate(LANE_EVENT_CAP);

        // Metric movement inside the window is often the real story even when
        // a service logged nothing at all.
        let movements: Vec<MetricMovement> = history
            .series(&profile_id, Some(from))
            .into_iter()
            .filter_map(|series| {
                let inside: Vec<_> = series.points.iter().filter(|p| p.t <= to).collect();
                let first = inside.first()?.v;
                let last = inside.last()?.v;
                if inside.len() < 2 || first.abs() < f64::EPSILON {
                    return None;
                }
                let change = (last - first) / first.abs() * 100.0;
                if change.abs() < MOVEMENT_THRESHOLD_PCT {
                    return None;
                }
                Some(MetricMovement {
                    key: series.key,
                    label: series.label,
                    unit: series.unit,
                    from: first,
                    to: last,
                    change_pct: change,
                })
            })
            .collect();

        // Emptiness is decided after traces are attached, not here: a service
        // that logged nothing and moved no metric can still have spent 900ms
        // inside a trace, and that is exactly the lane an operator needs to
        // see. Dropping it now would make traces decorative.
        lanes.push(CorrelationLane {
            profile_id,
            profile_name,
            kind,
            total,
            error_count,
            events: lane_events,
            movements,
            // Filled below, once every store has been asked.
            traces: Vec::new(),
        });
    }

    attach_traces(state, &mut lanes, from, to).await;

    // Now that every source has reported, drop the lanes that carry nothing.
    // "elasticsearch logged nothing" is evidence, but only for a connection
    // that was involved at all.
    lanes.retain(|lane| lane.total > 0 || !lane.movements.is_empty() || !lane.traces.is_empty());


    // Most likely culprit first: errors — from logs or from spans — then the
    // time a lane actually spent inside a trace, then volume and movement.
    // Self time ranks above volume deliberately: a service that logged twice
    // and held the request for 900ms is the cause, and one that logged two
    // hundred lines while waiting on it is not.
    let trace_errors = |lane: &CorrelationLane| -> usize {
        lane.traces.iter().map(|t| t.error_count).sum()
    };
    let self_ms = |lane: &CorrelationLane| -> f64 {
        lane.traces.iter().map(|t| t.self_ms).sum()
    };
    lanes.sort_by(|a, b| {
        (b.error_count + trace_errors(b))
            .cmp(&(a.error_count + trace_errors(a)))
            .then_with(|| {
                self_ms(b)
                    .partial_cmp(&self_ms(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.total.cmp(&a.total))
            .then_with(|| b.movements.len().cmp(&a.movements.len()))
    });

    let alerts = state
        .alerts
        .read()
        .await
        .all()
        .into_iter()
        .filter(|a| a.last_seen >= from && a.first_seen <= to)
        .collect();

    Ok(Correlation {
        center: at,
        from,
        to,
        lanes,
        alerts,
    })
}

/// Console output for one build, fetched on demand.
///
/// Build logs live with the build rather than in the shared stream — this is
/// what the Jenkins build-history strip opens.
pub async fn get_build_console(
    state: &Arc<AppState>,
    profile_id: String,
    job: String,
    build: Option<i64>,
    tail_lines: Option<usize>,
) -> Result<serde_json::Value> {
    state
        .collector(&profile_id)
        .await?
        .detail(&crate::collectors::DetailQuery::JenkinsBuildLog {
            job,
            build,
            tail_lines,
        })
        .await
}

/// One assembled trace, for the waterfall.
///
/// Returns the tree rather than the flat span list the backend hands over:
/// assembling it here means the orphan, cycle and duplicate cases are handled
/// once, in `traces.rs`, where they are tested — rather than once per
/// component that tries to render a trace.
pub async fn get_trace(
    state: &Arc<AppState>,
    profile_id: String,
    trace_id: String,
) -> Result<serde_json::Value> {
    state
        .collector(&profile_id)
        .await?
        .detail(&crate::collectors::DetailQuery::Trace { trace_id })
        .await
}

/// One flame graph, over a window.
///
/// `query` empty means the connection's own declared selector, so the screen
/// opens on something rather than on a blank text box. The window defaults to
/// the last hour: a profile is an aggregate over time, and a window measured
/// in seconds is a sample of a sample.
pub async fn get_profile(
    state: &Arc<AppState>,
    profile_id: String,
    query: Option<String>,
    minutes: Option<i64>,
) -> Result<serde_json::Value> {
    let minutes = minutes.unwrap_or(60).clamp(1, 60 * 24 * 7);
    let to = chrono::Utc::now();
    let from = to - chrono::Duration::minutes(minutes);

    state
        .collector(&profile_id)
        .await?
        .detail(&crate::collectors::DetailQuery::Profile {
            query: query.unwrap_or_default(),
            from,
            to,
        })
        .await
}

// ------------------------------------------------------------------ history

// ------------------------------------------------------------- deploy ingest

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestStatus {
    pub enabled: bool,
    pub port: u16,
    /// Whether a bearer token exists. The token itself is never returned here —
    /// only `rotate_ingest_token` ever hands it back, and only once.
    pub has_token: bool,
    /// Loopback by construction; shown so the UI need not rebuild the string.
    pub url: String,
    pub keychain_available: bool,
}

pub async fn get_ingest_settings(state: &Arc<AppState>) -> Result<IngestStatus> {
    let settings = state.config.read().await.ingest.clone();
    let has_token = crate::secrets::get_secret(crate::ingest::TOKEN_KEY)
        .await
        .unwrap_or(None)
        .is_some();

    Ok(IngestStatus {
        enabled: settings.enabled,
        port: settings.port,
        has_token,
        url: format!("http://127.0.0.1:{}/v1/deploys", settings.port),
        keychain_available: crate::secrets::store_available(),
    })
}

/// Enable, disable, or move the listener. Returns the resulting status.
///
/// Enabling with no token stored mints one, so a user never ends up with a
/// listener that cannot authenticate anybody.
pub async fn save_ingest_settings(
    state: &Arc<AppState>,
    enabled: bool,
    port: u16,
) -> Result<IngestStatus> {
    if enabled && port < 1024 {
        return Err(AppError::Rejected(
            "choose a port above 1023; privileged ports need elevation".into(),
        ));
    }
    // The reciprocal of the check in `save_mcp_settings`. Without it the two
    // listeners can be pointed at one port from this side, and the config is
    // persisted before the bind is attempted — so it fails on every boot
    // afterwards, not just once.
    if enabled && port == state.config.read().await.mcp.port {
        return Err(AppError::Rejected(
            "that is the MCP server's port; give deploy ingest one of its own".into(),
        ));
    }

    {
        let mut config = state.config.write().await;
        config.ingest.enabled = enabled;
        config.ingest.port = port;
    }
    state.save_config().await?;

    if enabled && crate::secrets::get_secret(crate::ingest::TOKEN_KEY).await?.is_none() {
        let token = uuid::Uuid::new_v4().simple().to_string();
        crate::secrets::set_secret(crate::ingest::TOKEN_KEY, &token).await?;
    }

    crate::ingest::restart(state).await?;
    get_ingest_settings(state).await
}

// ------------------------------------------------------------ notifications

pub async fn get_notifications(state: &Arc<AppState>) -> Result<crate::notify::NotificationsView> {
    Ok(crate::notify::view(state).await)
}

pub async fn save_notification_channel(
    state: &Arc<AppState>,
    channel: crate::notify::NotifyChannel,
    target: Option<String>,
) -> Result<crate::notify::NotificationsView> {
    crate::notify::save(state, channel, target).await
}

pub async fn delete_notification_channel(
    state: &Arc<AppState>,
    channel_id: String,
) -> Result<crate::notify::NotificationsView> {
    crate::notify::delete(state, &channel_id).await
}

/// Each active alert's standing with the channels that will hear of it.
pub async fn get_alert_notices(
    state: &Arc<AppState>,
) -> Result<std::collections::HashMap<String, Vec<crate::notify::Notice>>> {
    Ok(crate::notify::notices(state).await)
}

pub async fn test_notification_channel(
    state: &Arc<AppState>,
    channel_id: String,
) -> Result<crate::notify::Delivery> {
    crate::notify::test(state, &channel_id).await
}

// ---------------------------------------------------------------- live demo

/// What the live demo is doing: whether it runs, which story is playing,
/// and what comes next.
pub async fn get_demo_status(state: &Arc<AppState>) -> Result<crate::sim::DemoStatus> {
    Ok(crate::sim::status(&*state.config.read().await))
}

/// Start the simulated estate. `pause_others` switches the real connections
/// off until the demo ends, so its incidents are not mixed with real ones.
pub async fn start_demo(
    state: &Arc<AppState>,
    pause_others: bool,
) -> Result<crate::config::AppConfig> {
    crate::sim::start(state, pause_others).await
}

/// Remove the simulated estate and switch back on what it switched off.
pub async fn stop_demo(state: &Arc<AppState>) -> Result<crate::config::AppConfig> {
    crate::sim::stop(state).await
}

/// "Break something": start a story now, or straight after the one playing.
pub async fn trigger_demo_story(
    state: &Arc<AppState>,
    story: Option<String>,
) -> Result<crate::sim::DemoStatus> {
    crate::sim::trigger(state, story).await
}

/// Mint a new token and return it once. Any pipeline using the old one stops
/// working immediately, which is the point.
pub async fn rotate_ingest_token(state: &Arc<AppState>) -> Result<String> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    crate::secrets::set_secret(crate::ingest::TOKEN_KEY, &token).await?;
    crate::ingest::restart(state).await?;
    Ok(token)
}

// ----------------------------------------------------------------- prediction

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictionReport {
    pub findings: Vec<crate::advisor::Finding>,
    /// Per prediction, in the model's own words, why it said so.
    pub because: Vec<Vec<String>>,
    /// Everything the model was shown this run. One list, not one per
    /// prediction — they all came from the same read, and a reader checking
    /// one wants to see what else was on the table.
    pub grounds: Vec<String>,
    pub model: String,
    pub at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Run a prediction sweep now and return what it found.
///
/// Also raises the alerts, so asking for a sweep and waiting for the timer
/// produce the same result rather than two behaviours to reason about.
pub async fn run_prediction(state: &Arc<AppState>) -> Result<PredictionReport> {
    let predictions = crate::predict::sweep(state).await?;
    Ok(PredictionReport {
        model: predictions
            .first()
            .map(|p| p.model.clone())
            .unwrap_or_default(),
        at: predictions.first().map(|p| p.at),
        grounds: predictions
            .first()
            .map(|p| p.grounds.clone())
            .unwrap_or_default(),
        because: predictions.iter().map(|p| p.because.clone()).collect(),
        findings: predictions.into_iter().map(|p| p.finding).collect(),
    })
}

/// What root would show the model, without asking it anything.
///
/// Exists so an operator can see exactly what leaves the machine before
/// enabling a feature that sends it. "Trust us" is not a security posture, and
/// this is the same courtesy `redact.rs` extends to config.
pub async fn preview_prediction_evidence(state: &Arc<AppState>) -> Result<Vec<String>> {
    Ok(crate::predict::gather_evidence(state).await)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictionStatus {
    pub enabled: bool,
    pub interval_minutes: u64,
    /// Whether an assistant is configured at all. Without one this cannot run,
    /// and saying so is better than a toggle that silently does nothing.
    pub assistant_ready: bool,
}

pub async fn get_prediction_settings(state: &Arc<AppState>) -> Result<PredictionStatus> {
    let config = state.config.read().await;
    let settings = config.prediction.clone();
    let provider = config.llm.provider;
    drop(config);

    let assistant_ready = !provider.requires_api_key()
        || crate::secrets::get_secret(provider.secret_key())
            .await
            .unwrap_or(None)
            .is_some_and(|key| !key.is_empty());

    Ok(PredictionStatus {
        enabled: settings.enabled,
        interval_minutes: settings.interval_minutes,
        assistant_ready,
    })
}

pub async fn save_prediction_settings(
    state: &Arc<AppState>,
    enabled: bool,
    interval_minutes: u64,
) -> Result<PredictionStatus> {
    {
        let mut config = state.config.write().await;
        config.prediction.enabled = enabled;
        // A floor, because each sweep is a model call and a one-minute timer
        // against a metered API is a bill nobody agreed to.
        config.prediction.interval_minutes = interval_minutes.clamp(5, 1440);
    }
    state.save_config().await?;
    get_prediction_settings(state).await
}

// ---------------------------------------------------------------- MCP server

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub enabled: bool,
    pub port: u16,
    /// Whether a bearer token exists. The token itself is never returned here —
    /// only `rotate_mcp_token` ever hands it back, and only once.
    pub has_token: bool,
    /// Loopback by construction; shown so the UI need not rebuild the string.
    pub url: String,
    pub keychain_available: bool,
    /// What a client would be able to call. Shown in Settings because the
    /// honest answer to "what does enabling this give away" is a list, not a
    /// sentence — and because it is read-only regardless of the assistant's
    /// own authority setting.
    pub tools: Vec<String>,
}

pub async fn get_mcp_settings(state: &Arc<AppState>) -> Result<McpStatus> {
    let settings = state.config.read().await.mcp.clone();
    let has_token = crate::secrets::get_secret(crate::mcp::http::TOKEN_KEY)
        .await
        .unwrap_or(None)
        .is_some();

    Ok(McpStatus {
        enabled: settings.enabled,
        port: settings.port,
        has_token,
        url: format!(
            "http://127.0.0.1:{}{}",
            settings.port,
            crate::mcp::http::ENDPOINT
        ),
        keychain_available: crate::secrets::store_available(),
        // The server's own list, not a second derivation of it — the panel
        // must not promise a tool the listener withholds.
        tools: crate::mcp::http::served_tool_names(),
    })
}

/// Enable, disable, or move the MCP listener. Returns the resulting status.
///
/// Enabling with no token stored mints one, so a user never ends up with a
/// listener that cannot authenticate anybody.
pub async fn save_mcp_settings(
    state: &Arc<AppState>,
    enabled: bool,
    port: u16,
) -> Result<McpStatus> {
    if enabled && port < 1024 {
        return Err(AppError::Rejected(
            "choose a port above 1023; privileged ports need elevation".into(),
        ));
    }
    if enabled && port == state.config.read().await.ingest.port {
        return Err(AppError::Rejected(
            "that is the deploy-ingest port; give the MCP server one of its own".into(),
        ));
    }

    {
        let mut config = state.config.write().await;
        config.mcp.enabled = enabled;
        config.mcp.port = port;
    }
    state.save_config().await?;

    if enabled
        && crate::secrets::get_secret(crate::mcp::http::TOKEN_KEY)
            .await?
            .is_none()
    {
        let token = uuid::Uuid::new_v4().simple().to_string();
        crate::secrets::set_secret(crate::mcp::http::TOKEN_KEY, &token).await?;
    }

    crate::mcp::http::restart(state).await?;
    get_mcp_settings(state).await
}

/// Mint a new token and return it once. Any client using the old one stops
/// working immediately, which is the point.
pub async fn rotate_mcp_token(state: &Arc<AppState>) -> Result<String> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    crate::secrets::set_secret(crate::mcp::http::TOKEN_KEY, &token).await?;
    crate::mcp::http::restart(state).await?;
    Ok(token)
}

/// Deploys and other changes, newest first.
///
/// Omit `profile_id` for the whole estate — the correlation view wants every
/// connection's changes in one window, while a service dashboard wants its own.
pub async fn get_change_events(
    state: &Arc<AppState>,
    profile_id: Option<String>,
    minutes: Option<i64>,
    limit: Option<usize>,
) -> Result<Vec<crate::model::ChangeEvent>> {
    let since = minutes.map(|m| chrono::Utc::now() - chrono::Duration::minutes(m.clamp(1, 1440)));
    Ok(state
        .changes
        .read()
        .await
        .query(profile_id.as_deref(), since, limit.unwrap_or(200).min(500)))
}

/// How far back a store-backed read may be asked to reach, in minutes.
///
/// Ninety days. The raw samples expire at thirty (`RAW_METRIC_DAYS`), but the
/// hourly rollup has no retention policy and outlives them, so the honest
/// ceiling is the rollup's rather than the raw table's. At one point an hour a
/// ninety-day window is about 2,160 points per series, which a chart can draw.
///
/// Reads with nothing but the ring buffer behind them keep the old
/// twenty-four-hour ceiling. Widening those would only promise a window they
/// cannot fill, which is worse than refusing it: the chart draws short and
/// nothing says why.
const STORE_MAX_MINUTES: i64 = 90 * 24 * 60;

/// Rolling metric history for one connection.
pub async fn get_metric_history(
    state: &Arc<AppState>,
    profile_id: String,
    minutes: Option<i64>,
) -> Result<Vec<MetricSeries>> {
    let since =
        minutes.map(|m| chrono::Utc::now() - chrono::Duration::minutes(m.clamp(1, STORE_MAX_MINUTES)));
    let mut series = state.history.read().await.series(&profile_id, since);

    // The ring buffer holds a few hours by design. When the question reaches
    // further back than that, the rest comes off disk — see `backfill` for why
    // the two are spliced rather than concatenated.
    if let (Some(since), Some(store)) = (since, state.store.read().await.as_ref()) {
        backfill(store, &profile_id, None, since, &mut series).await;
    }
    Ok(series)
}

/// Extend each series backwards with what the database kept.
///
/// The buffer is the live layer and stays authoritative for everything it
/// covers: its samples are raw, at the poll interval, and that is what a
/// recent chart should draw. The store's hourly rollup is coarser, and it
/// exists to answer the question the buffer cannot — what did this look like
/// before the last three hours.
///
/// Only points strictly older than the buffer's earliest sample are taken. The
/// two sources overlap by construction — the store has been recording the same
/// hours the buffer is holding — and concatenating them would draw the last
/// three hours twice, at two different resolutions, with the seam somewhere in
/// the middle of the chart.
///
/// Best-effort, like every other read of this store: a database that is not
/// answering costs the long tail of the chart, not the chart.
async fn backfill(
    store: &crate::store::Store,
    profile_id: &str,
    // A component's (section, row), or `None` for the connection's own metrics.
    entity: Option<(&str, &str)>,
    since: chrono::DateTime<chrono::Utc>,
    series: &mut [MetricSeries],
) {
    for one in series.iter_mut() {
        let Some(earliest) = one.points.first().map(|p| p.t) else {
            continue;
        };
        // The buffer already reaches the whole window. Nothing to add, and
        // asking the database would be a query for rows that get discarded.
        if earliest <= since {
            continue;
        }

        let older = match entity {
            None => store.metric_buckets(profile_id, &one.key, since).await,
            Some((section, row)) => store.entity_buckets(profile_id, section, row, &one.key, since).await,
        };
        let older = match older {
            Ok(rows) => rows,
            Err(e) => {
                tracing::debug!(metric = %one.key, "history backfill failed: {e}");
                continue;
            }
        };

        let buckets: Vec<_> = older.into_iter().filter(|h| h.at < earliest).collect();
        if buckets.is_empty() {
            continue;
        }

        // min and max come from each bucket's own extremes, not from the
        // averages that are about to become the points. An hour holding one
        // 9.9GB spike and eleven quiet samples averages to something
        // unremarkable, and a series whose max was taken from that average
        // would report that the breach never happened. The plotted point is
        // still the average — that is what a rollup is — but the range the
        // chart scales to, and any threshold read off it, comes from what was
        // actually measured.
        one.min = buckets.iter().map(|h| h.min).fold(one.min, f64::min);
        one.max = buckets.iter().map(|h| h.max).fold(one.max, f64::max);

        let mut prefix: Vec<crate::history::Point> = buckets
            .into_iter()
            .map(|h| crate::history::Point { t: h.at, v: h.avg })
            .collect();
        prefix.extend(std::mem::take(&mut one.points));
        one.points = prefix;
    }
}

/// Every time a condition has fired on one connection, newest first.
///
/// The question a ring buffer can never answer, and the reason the alert
/// lifecycle is on disk: is this the first time, or the ninth time this month?
pub async fn get_alert_history(
    state: &Arc<AppState>,
    profile_id: String,
    days: Option<i64>,
) -> Result<Vec<crate::store::PastAlert>> {
    let Some(store) = state.store.read().await.as_ref().cloned() else {
        // No database configured. An empty list rather than an error: the
        // screen asking this should say "nothing kept" and not "failed".
        return Ok(Vec::new());
    };
    let since = chrono::Utc::now() - chrono::Duration::days(days.unwrap_or(30).clamp(1, 3650));
    store.alert_history(&profile_id, since, 200).await
}

/// Conditions that keep firing, worst first.
///
/// One alert that fired forty times and forty that fired once look identical
/// in a list ordered by recency, and they are completely different problems.
pub async fn get_flapping(
    state: &Arc<AppState>,
    days: Option<i64>,
) -> Result<Vec<FlappingCondition>> {
    let Some(store) = state.store.read().await.as_ref().cloned() else {
        return Ok(Vec::new());
    };
    let since = chrono::Utc::now() - chrono::Duration::days(days.unwrap_or(7).clamp(1, 365));
    Ok(store
        .alert_frequency(since)
        .await?
        .into_iter()
        .map(|(profile_name, title, times)| FlappingCondition {
            profile_name,
            title,
            times,
        })
        .collect())
}

/// How much the database is holding, and whether Timescale is looking after it.
///
/// Shown in Settings because a store nobody can see the size of is a store
/// somebody eventually finds full.
pub async fn get_history_stats(state: &Arc<AppState>) -> Result<HistoryStats> {
    let Some(store) = state.store.read().await.as_ref().cloned() else {
        return Ok(HistoryStats::default());
    };
    Ok(HistoryStats {
        configured: true,
        timescale: store.has_timescale(),
        backend: Some(store.backend().to_string()),
        bytes: store.bytes(),
        rows: store.counts().await.unwrap_or_default(),
    })
}

/// Every change recorded for one connection, or for the estate, newest first.
///
/// Separate from [`get_change_events`] rather than spliced into it, for the
/// same reason [`get_alert_history`] is separate from the live alert list: a
/// stored row is a record and a live row is a working object. They carry
/// different types because they answer different questions, and a function
/// that returned a mix of the two would make every caller check which it got.
pub async fn get_change_history(
    state: &Arc<AppState>,
    profile_id: Option<String>,
    days: Option<i64>,
) -> Result<Vec<crate::store::PastChange>> {
    let Some(store) = state.store.read().await.as_ref().cloned() else {
        return Ok(Vec::new());
    };
    let since = chrono::Utc::now() - chrono::Duration::days(days.unwrap_or(30).clamp(1, 3650));
    store.changes_since(profile_id.as_deref(), since, 500).await
}

/// One condition and how often it has fired.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlappingCondition {
    pub profile_name: String,
    pub title: String,
    pub times: i64,
}

/// What the history store is holding.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStats {
    /// False only when no store would open at all: the local file is the
    /// default now, so history is kept unless something went wrong.
    pub configured: bool,
    /// Whether the rollup, compression and expiry are the database's job.
    pub timescale: bool,
    /// `postgres` or `local`; absent when nothing is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// The local file's size on disk. Postgres keeps its own counters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub rows: std::collections::HashMap<String, i64>,
}

/// The declared dependency graph, resolved both ways.
///
/// `roots` answers the question the graph exists for: of everything currently
/// failing, which failures are not explained by another failure. It is a
/// narrowing, never a verdict — an undeclared edge is invisible here, so a
/// root may just be a connection nobody finished configuring.
pub async fn get_topology(state: &Arc<AppState>) -> Result<TopologyView> {
    let profiles = state.config.read().await.profiles.clone();
    let graph = crate::topology::Topology::build(&profiles);

    let dashboard = state.dashboard().await;

    // Onset comes from the oldest still-firing alert on a connection, falling
    // back to the moment it was last checked. Without it the graph would
    // credit a dependency with a failure that started after its own.
    let failing: Vec<(&str, chrono::DateTime<chrono::Utc>)> = dashboard
        .snapshots
        .iter()
        .filter(|s| matches!(s.health, Health::Down | Health::Degraded | Health::Unknown))
        .map(|s| {
            let onset = dashboard
                .alerts
                .iter()
                .filter(|a| a.active && a.profile_id == s.profile_id)
                .map(|a| a.first_seen)
                .min()
                .unwrap_or(s.checked_at);
            (s.profile_id.as_str(), onset)
        })
        .collect();

    Ok(TopologyView {
        depends_on: profiles
            .iter()
            .map(|p| (p.id.clone(), graph.depends_on(&p.id).to_vec()))
            .collect(),
        dependents: profiles
            .iter()
            .map(|p| (p.id.clone(), graph.dependents(&p.id).to_vec()))
            .collect(),
        roots: graph
            .roots_with_onset(&failing, chrono::Duration::seconds(30))
            .into_iter()
            .map(str::to_string)
            .collect(),
    })
}

// ------------------------------------------------------------------- team

/// Local mode is not an error state, so it is a value.
///
/// `serverUrl` absent means root is doing what it does by default: reading a
/// config file on this machine and answering to nobody. Everything below is
/// additive.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamState {
    pub server_url: Option<String>,
    pub account: Option<crate::config::Account>,
    /// A token exists on this machine. Not proof the server still honours it.
    pub has_session: bool,
}

pub async fn get_team(state: &Arc<AppState>) -> Result<TeamState> {
    let team = state.config.read().await.team.clone();
    Ok(TeamState {
        server_url: team.server_url,
        account: team.account,
        has_session: team::has_session().await,
    })
}

/// Point this machine at a workspace server, or clear it to go back to local.
///
/// Clearing signs out too. Leaving a session behind for a server nobody is
/// talking to any more is a token sitting in a keychain for no reason.
pub async fn save_team_server(state: &Arc<AppState>, server_url: Option<String>) -> Result<TeamState> {
    let normalized = match server_url.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(url) => Some(team::normalize_server(url)?),
    };

    if normalized.is_none() {
        let previous = state.config.read().await.team.server_url.clone();
        team::sign_out(previous.as_deref()).await?;
    }

    {
        let mut config = state.config.write().await;
        config.team.server_url = normalized;
        if config.team.server_url.is_none() {
            config.team.account = None;
        }
    }
    state.save_config().await?;
    get_team(state).await
}

/// The password is used to get a token and is not retained anywhere.
pub async fn sign_in(state: &Arc<AppState>, email: String, password: String) -> Result<TeamState> {
    let server = state
        .config
        .read()
        .await
        .team
        .server_url
        .clone()
        .ok_or_else(|| AppError::Config("no workspace server is configured".into()))?;

    let account = team::sign_in(&server, &email, &password).await?;
    {
        let mut config = state.config.write().await;
        config.team.account = Some(account);
    }
    state.save_config().await?;
    get_team(state).await
}

pub async fn sign_out(state: &Arc<AppState>) -> Result<TeamState> {
    let server = state.config.read().await.team.server_url.clone();
    team::sign_out(server.as_deref()).await?;
    {
        let mut config = state.config.write().await;
        config.team.account = None;
    }
    state.save_config().await?;
    get_team(state).await
}

/// Who else is in the workspace. Errors in local mode rather than returning an
/// empty list — nobody is not the same fact as no workspace.
pub async fn get_workspace(state: &Arc<AppState>) -> Result<team::Workspace> {
    let server = state
        .config
        .read()
        .await
        .team
        .server_url
        .clone()
        .ok_or_else(|| AppError::Config("no workspace server is configured".into()))?;
    team::workspace(&server).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyView {
    pub depends_on: std::collections::HashMap<String, Vec<String>>,
    pub dependents: std::collections::HashMap<String, Vec<String>>,
    /// Failing connections not explained by another failing connection.
    pub roots: Vec<String>,
}

/// The alert stream, read as the small number of things actually happening.
///
/// This is the same evidence `get_dashboard` already returns — active alerts,
/// current snapshots, the declared graph — grouped rather than listed. It is
/// derived on every call rather than stored, because an incident is a claim
/// about the present: the moment an alert clears, the grouping that named it
/// should stop existing too.
///
/// Ordering is the reading order: worst first, then earliest, so the top of
/// the list is where to look. Incidents that are entirely acknowledged sink
/// below the ones that are not, whatever their severity — an acknowledged
/// outage is someone else's open tab.
pub async fn get_incidents(state: &Arc<AppState>) -> Result<Vec<Incident>> {
    let profiles = state.config.read().await.profiles.clone();
    let graph = crate::topology::Topology::build(&profiles);
    let dashboard = state.dashboard().await;

    Ok(incidents::correlate(
        &dashboard.alerts,
        &dashboard.snapshots,
        &graph,
    ))
}

/// State runs for every connection across one window — the lane view.
///
/// Returned for all connections at once because that is how it is read: down a
/// column, to see what everything was doing at a single moment. A connection
/// with no samples in the window still gets a run, marked unknown, rather than
/// being omitted — a lane that is missing and a lane that is silent are
/// different facts.
pub async fn get_health_runs(
    state: &Arc<AppState>,
    minutes: Option<i64>,
) -> Result<std::collections::HashMap<String, Vec<crate::history::HealthRun>>> {
    let now = chrono::Utc::now();
    let since = now - chrono::Duration::minutes(minutes.unwrap_or(60).clamp(1, 1440));

    let profiles = state.config.read().await.profiles.clone();
    let history = state.history.read().await;

    Ok(profiles
        .iter()
        .map(|profile| {
            (
                profile.id.clone(),
                history.health_runs(&profile.id, since, now),
            )
        })
        .collect())
}

/// Rolling history for one component — a container, a cluster node, a job.
///
/// Empty for sections a collector did not mark `tracked`; those are readable
/// live from the snapshot but have no series behind them, by design.
pub async fn get_component_history(
    state: &Arc<AppState>,
    profile_id: String,
    section_id: String,
    row_id: String,
    minutes: Option<i64>,
) -> Result<Vec<MetricSeries>> {
    let since =
        minutes.map(|m| chrono::Utc::now() - chrono::Duration::minutes(m.clamp(1, STORE_MAX_MINUTES)));
    let mut series = state
        .history
        .read()
        .await
        .entity_series(&profile_id, &section_id, &row_id, since);
    // Past the ring, from disk — the same splice the connection's own charts use.
    if let (Some(since), Some(store)) = (since, state.store.read().await.as_ref()) {
        backfill(store, &profile_id, Some((&section_id, &row_id)), since, &mut series).await;
    }
    Ok(series)
}

/// Every piece of configuration root can see for one connection.
///
/// Both provenances together: the files an operator declared, and whatever the
/// target itself can be asked for. They are listed side by side deliberately —
/// a file and a live value disagreeing is the commonest config bug there is,
/// and it is invisible unless you can see both at once.
///
/// Bodies are redacted before they reach this function. There is no variant of
/// this command that returns the original bytes.
pub async fn get_config_documents(
    state: &Arc<AppState>,
    profile_id: String,
) -> Result<Vec<crate::configsource::ConfigDocument>> {
    let profile = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .cloned()
            .ok_or_else(|| crate::error::AppError::Collector("no such connection".into()))?
    };

    let mut documents = Vec::new();
    for source in &profile.config_sources {
        documents.extend(crate::configsource::read_source(&profile.id, source).await);
    }

    // A target that cannot be asked, or is currently down, must not fail the
    // whole read — the declared files are still worth showing.
    if let Ok(collector) = state.collector(&profile.id).await {
        match collector.config().await {
            Ok(live) => documents.extend(live),
            Err(e) => tracing::debug!(profile = %profile.id, "live config unavailable: {e}"),
        }
    }

    Ok(documents)
}

/// What config and telemetry say when read together.
///
/// Omit `profile_id` for the whole estate. Deterministic rules, not the
/// assistant: every finding names the setting, the metric and the projection
/// behind it, so the reasoning can be checked rather than trusted.
pub async fn get_findings(
    state: &Arc<AppState>,
    profile_id: Option<String>,
) -> Result<Vec<crate::advisor::Finding>> {
    let profiles: Vec<crate::config::ConnectionProfile> = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .filter(|p| p.enabled)
            .filter(|p| profile_id.as_ref().is_none_or(|id| &p.id == id))
            .cloned()
            .collect()
    };

    let mut out = Vec::new();
    for profile in &profiles {
        let Some(snapshot) = state.snapshots.read().await.get(&profile.id).cloned() else {
            continue;
        };

        let mut documents = Vec::new();
        for source in &profile.config_sources {
            documents.extend(crate::configsource::read_source(&profile.id, source).await);
        }
        if let Ok(collector) = state.collector(&profile.id).await {
            if let Ok(live) = collector.config().await {
                documents.extend(live);
            }
        }
        if documents.is_empty() {
            continue;
        }

        let series = state.history.read().await.series(&profile.id, None);
        out.extend(crate::advisor::findings(&snapshot, &documents, &series));
    }

    // What the assistant reported, alongside what the rules found.
    //
    // Together rather than in a separate list, because they are answers to the
    // same question and the person rating them is deciding the same thing.
    // Keeping the model's claims on their own screen would make them optional
    // reading, and a claim nobody rates is a claim that never becomes a rule.
    {
        let claims = state.claims.read().await;
        for profile in &profiles {
            out.extend(claims.for_profile(&profile.id));
        }
    }

    out.sort_by_key(|f| std::cmp::Reverse(f.severity));
    Ok(out)
}

// ------------------------------------------------------- long-range history

/// Metric history from the estate's own Prometheus, over a window root's ring
/// buffer cannot reach.
///
/// Returns the same `MetricSeries` shape as the in-memory store so every chart
/// and every forecast reads it without knowing where it came from. The trend
/// is recomputed over the long window, which is the entire point: a volume
/// filling in three weeks is invisible to a few hours of samples, and root
/// currently refuses that projection honestly but uselessly.
pub async fn get_long_history(
    state: &Arc<AppState>,
    profile_id: String,
    hours: Option<i64>,
) -> Result<Vec<crate::history::MetricSeries>> {
    let hours = hours.unwrap_or(24 * 14).clamp(1, 24 * 365);
    let end = chrono::Utc::now();
    let start = end - chrono::Duration::hours(hours);
    long_history(state, &profile_id, start, end).await
}

/// Build long-range series for one profile, or nothing if it declares no source.
async fn long_history(
    state: &std::sync::Arc<crate::state::AppState>,
    profile_id: &str,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<crate::history::MetricSeries>> {
    let (source, server) = {
        let config = state.config.read().await;
        let profile = config
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or_else(|| crate::error::AppError::Collector("no such connection".into()))?;

        let Some(source) = profile.history_source.clone().filter(|s| s.is_configured()) else {
            return Ok(Vec::new());
        };
        let server = config
            .profiles
            .iter()
            .find(|p| p.id == source.prometheus_profile_id)
            .cloned()
            .ok_or_else(|| {
                crate::error::AppError::Collector(
                    "this connection points at a history source that is no longer configured"
                        .into(),
                )
            })?;
        (source, server)
    };

    // The metric's label, unit and threshold come from the live snapshot, not
    // from Prometheus: the series is the same measure root already knows about
    // and re-deriving a unit from a PromQL expression would be guesswork.
    let snapshot = state.snapshots.read().await.get(profile_id).cloned();

    let auth = crate::promquery::Auth {
        username: server.username.clone(),
        secret: crate::secrets::get_secret(&server.secret_key())
            .await
            .unwrap_or(None),
    };
    let client = crate::collectors::http_client(
        server.options.bool("insecureTls"),
        std::time::Duration::from_secs(30),
    )?;

    let mut out = Vec::new();
    for query in &source.queries {
        let points =
            crate::promquery::range(&client, &server.endpoint, &auth, &query.expr, start, end)
                .await?;
        if points.is_empty() {
            continue;
        }

        let known = snapshot
            .as_ref()
            .and_then(|s| s.metrics.iter().find(|m| m.key == query.metric_key));
        let warn_above = known.and_then(|m| m.warn_above);
        let values: Vec<f64> = points.iter().map(|p| p.v).collect();

        out.push(crate::history::MetricSeries {
            key: query.metric_key.clone(),
            label: known
                .map(|m| m.label.clone())
                .unwrap_or_else(|| query.metric_key.clone()),
            unit: known.map(|m| m.unit).unwrap_or(crate::model::MetricUnit::Count),
            warn_above,
            change_pct: None,
            min: values.iter().copied().fold(f64::INFINITY, f64::min),
            max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            latest: *values.last().unwrap_or(&0.0),
            trend: crate::forecast::trend(&points, warn_above),
            points,
        });
    }

    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// Drop the assistant's claims for one connection.
///
/// For when a conversation went somewhere unhelpful and left findings nobody
/// wants to rate. Clearing is not the same as rating something "not a
/// problem" — that is a judgement worth recording, and this is a wastebasket.
pub async fn clear_claims(state: &Arc<AppState>, profile_id: String) -> Result<()> {
    let mut claims = state.claims.write().await;
    claims.clear(&profile_id);
    Ok(())
}

// ------------------------------------------------------- runtime reconciling

/// One connection's declared runtime, checked against what root can see.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCheck {
    pub profile_id: String,
    pub profile_name: String,
    pub declared: crate::runtime::Runtime,
    pub discrepancies: Vec<crate::runtime::Discrepancy>,
}

/// Everything root and its operators disagree about, plus the gaps.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReport {
    /// Only connections with something to say. A declaration root agrees with
    /// is not news, and listing every one of them would bury the few that are
    /// wrong.
    pub checks: Vec<RuntimeCheck>,
    /// Containers and pods that look like services with nothing watching them.
    pub uncovered: Vec<crate::runtime::Uncovered>,
    /// How many connections have declared a runtime at all — the denominator
    /// for everything above, and the honest caveat when it is small.
    pub declared: usize,
    pub total: usize,
}

/// Reconcile every declared runtime against the live estate.
pub async fn get_runtime_report(state: &Arc<AppState>) -> Result<RuntimeReport> {
    let profiles: Vec<crate::config::ConnectionProfile> = {
        let config = state.config.read().await;
        config.profiles.iter().filter(|p| p.enabled).cloned().collect()
    };
    let snapshots = state.snapshots.read().await.clone();

    let mut checks = Vec::new();
    let mut uncovered = Vec::new();

    // Hosts first: each one's components are read once and reused for every
    // connection declared against it.
    for host in profiles.iter().filter(|p| {
        matches!(
            p.kind,
            crate::model::ServiceKind::Docker
                | crate::model::ServiceKind::Podman
                | crate::model::ServiceKind::Kubernetes
        )
    }) {
        let Some(snapshot) = snapshots.get(&host.id) else {
            continue;
        };
        let components = components_of(snapshot);

        // Which connections say they run here.
        let declared_here: Vec<&crate::config::ConnectionProfile> = profiles
            .iter()
            .filter(|p| p.runtime.host_profile_id.as_deref() == Some(host.id.as_str()))
            .collect();

        for profile in &declared_here {
            let found = crate::runtime::reconcile(
                &profile.id,
                &profile.runtime,
                true,
                &host.name,
                &components,
            );
            if !found.is_empty() {
                checks.push(RuntimeCheck {
                    profile_id: profile.id.clone(),
                    profile_name: profile.name.clone(),
                    declared: profile.runtime.clone(),
                    discrepancies: found,
                });
            }
        }

        let declared_names: Vec<String> = declared_here
            .iter()
            .filter_map(|p| p.runtime.name.clone())
            .collect();
        uncovered.extend(crate::runtime::uncovered(
            &host.id,
            &host.name,
            &components,
            &declared_names,
        ));
    }

    // Declarations pointing at a host that is gone, or that name nothing —
    // neither of which the loop above can see, because it walks hosts.
    let host_ids: std::collections::HashSet<&str> =
        profiles.iter().map(|p| p.id.as_str()).collect();
    for profile in profiles.iter().filter(|p| p.runtime.is_declared()) {
        let names_a_live_host = profile
            .runtime
            .host_profile_id
            .as_deref()
            .is_some_and(|id| host_ids.contains(id) && snapshots.contains_key(id));
        if names_a_live_host {
            continue;
        }
        let found = crate::runtime::reconcile(
            &profile.id,
            &profile.runtime,
            profile
                .runtime
                .host_profile_id
                .as_deref()
                .is_some_and(|id| host_ids.contains(id)),
            "",
            &[],
        );
        if !found.is_empty() {
            checks.push(RuntimeCheck {
                profile_id: profile.id.clone(),
                profile_name: profile.name.clone(),
                declared: profile.runtime.clone(),
                discrepancies: found,
            });
        }
    }

    Ok(RuntimeReport {
        checks,
        uncovered,
        declared: profiles.iter().filter(|p| p.runtime.is_declared()).count(),
        total: profiles.len(),
    })
}

/// A host snapshot's components, in the shape reconciliation wants.
fn components_of(snapshot: &crate::model::HealthSnapshot) -> Vec<crate::runtime::Component<'_>> {
    snapshot
        .sections
        .iter()
        .filter(|s| s.id == "containers" || s.id == "pods")
        .flat_map(|section| {
            section.rows.iter().map(|row| {
                // The identification the collector recorded, read back out of
                // the cell it renders from — one source of truth rather than
                // running the matcher twice and risking two answers.
                let kind_text = row.cells.get("kind").and_then(|c| match c {
                    crate::model::CellValue::Text { value } => Some(value.as_str()),
                    _ => None,
                });
                let serving = kind_text.is_some_and(|t| t != "—" && !t.contains("not serving"));
                let identified = kind_text
                    .filter(|t| *t != "—")
                    .and_then(|t| crate::registry::kind_from_label(t.trim_end_matches(" (not serving)")));

                crate::runtime::Component {
                    name: row.name.as_str(),
                    identified,
                    serving,
                    watched_by: row
                        .extra
                        .get("watchedBy")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty()),
                }
            })
        })
        .collect()
}

// ---------------------------------------------------------- scenario library

/// Run the built-in scenarios against the rules as they stand.
///
/// Answers the question a red build cannot: after a threshold was widened or a
/// rule reworded, is the product better or worse than it was? Pure computation
/// over embedded fixtures — it touches no infrastructure and needs nothing
/// configured, so it works on a fresh install with no connections at all.
pub async fn run_scenarios() -> Result<crate::scenarios::SuiteResult> {
    let library = crate::scenarios::library()
        .map_err(crate::error::AppError::Collector)?;
    Ok(crate::scenarios::run_all(&library))
}

/// The scenarios themselves, for reading rather than running.
pub async fn list_scenarios() -> Result<Vec<crate::scenarios::Scenario>> {
    crate::scenarios::library().map_err(crate::error::AppError::Collector)
}

// ------------------------------------------------------------------ backtest

/// Replay the current rules across a connection's stored history.
///
/// Uses the long-range source where one is declared, and root's own in-memory
/// ring otherwise — which is usually too short to say anything, and the result
/// says so rather than reporting a quiet window as a clean one.
///
/// Scored against the incidents somebody has already judged: the useful output
/// is not "these rules fire" but "these rules would have fired before the
/// thing we recorded as a miss".
pub async fn run_backtest(
    state: &Arc<AppState>,
    profile_id: String,
    days: Option<i64>,
    step_minutes: Option<i64>,
) -> Result<crate::backtest::Replay> {
    let days = days.unwrap_or(14).clamp(1, 365);
    let end = chrono::Utc::now();
    let start = end - chrono::Duration::days(days);

    let snapshot = state
        .snapshots
        .read()
        .await
        .get(&profile_id)
        .cloned()
        .ok_or_else(|| {
            crate::error::AppError::Collector(
                "that connection has not been polled yet, so there is no metric metadata to                  replay against"
                    .into(),
            )
        })?;

    let documents = collect_config(state, &profile_id).await?;

    // Prefer the estate's own long history. The in-memory ring is the fallback
    // and is rarely long enough to fit anything — which the caveats say.
    let series = match long_history(state, &profile_id, start, end).await {
        Ok(series) if !series.is_empty() => series,
        Ok(_) => state.history.read().await.series(&profile_id, Some(start)),
        Err(e) => {
            tracing::debug!(profile = %profile_id, "long history unavailable: {e}");
            state.history.read().await.series(&profile_id, Some(start))
        }
    };

    // Only the incidents this connection was actually part of. Scoring a Redis
    // replay against a Jenkins outage would be noise in both directions.
    let labels: Vec<_> = match state.journal().await {
        Ok(journal) => journal
            .incident_labels()
            .await
            .unwrap_or_default()
            .into_iter()
            .collect(),
        Err(_) => Vec::new(),
    };

    let step = chrono::Duration::minutes(step_minutes.unwrap_or(60).clamp(5, 1440));

    Ok(crate::backtest::replay(
        &snapshot,
        &documents,
        &series,
        &labels,
        step,
        crate::backtest::DEFAULT_TRAILING,
    ))
}

/// Declared files and live config for one profile, shared by the readers that
/// need both.
async fn collect_config(
    state: &std::sync::Arc<crate::state::AppState>,
    profile_id: &str,
) -> Result<Vec<crate::configsource::ConfigDocument>> {
    let sources = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .map(|p| p.config_sources.clone())
            .unwrap_or_default()
    };

    let mut documents = Vec::new();
    for source in &sources {
        documents.extend(crate::configsource::read_source(profile_id, source).await);
    }
    if let Ok(collector) = state.collector(profile_id).await {
        if let Ok(live) = collector.config().await {
            documents.extend(live);
        }
    }
    Ok(documents)
}

// ------------------------------------------------------ the judgement record

/// Record what an operator thought of a finding.
///
/// Always an insert. Changing a rating writes a second row and leaves the
/// first — the history of a mind changing is itself a signal, and a score that
/// can be edited cannot answer "did root get better".
pub async fn record_verdict(
    state: &Arc<AppState>,
    verdict: crate::journal::Verdict,
) -> Result<i64> {
    state.journal().await?.record_verdict(&verdict).await
}

/// Record whether root called an incident before it happened.
///
/// The half of the measurement that ratings on findings cannot produce: if
/// root never mentioned the thing that broke, there was no card to rate.
pub async fn record_recall(state: &Arc<AppState>, recall: crate::journal::Recall) -> Result<i64> {
    state.journal().await?.record_recall(&recall).await
}

/// The latest judgement per finding, so the UI can show what is already rated.
pub async fn get_verdicts(
    state: &Arc<AppState>,
    limit: Option<i64>,
) -> Result<Vec<crate::journal::RecordedVerdict>> {
    state.journal().await?.verdicts(limit.unwrap_or(500)).await
}

/// Incidents already judged, so nobody is asked the same question twice.
pub async fn get_judged_incidents(state: &Arc<AppState>) -> Result<Vec<String>> {
    state.journal().await?.judged_incidents().await
}

/// Precision by provenance and by rule, and recall from the incident side.
pub async fn get_scorecard(state: &Arc<AppState>) -> Result<crate::journal::Scorecard> {
    state.journal().await?.scorecard().await
}

/// What in the estate is heading for a limit, and when it gets there.
///
/// One call rather than one per connection per component, because the ranking
/// needs every series at once to be a ranking at all — and the alternative is
/// shipping the whole history store to the client to sort six rows.
pub async fn get_capacity_risks(
    state: &Arc<AppState>,
    take: Option<usize>,
) -> Result<Vec<crate::history::CapacityRisk>> {
    let mut risks = state
        .history
        .read()
        .await
        .capacity_risks(take.unwrap_or(8).clamp(1, 100));

    // Re-fit anything whose connection declares a long-range source.
    //
    // The in-memory ring holds hours, and a projection fitted over hours can
    // only see problems that arrive in hours. That is why an estate full of
    // slowly-filling disks reads as "unclear" everywhere: not because the
    // answer does not exist, but because root was looking through a window
    // too narrow to contain it. Where weeks of history are available, the
    // trend is refitted over weeks and the row is re-sorted on the result.
    let end = chrono::Utc::now();
    let start = end - chrono::Duration::days(30);
    let profiles: Vec<String> = {
        let mut ids: Vec<String> = risks.iter().map(|r| r.profile_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    };

    for profile_id in profiles {
        // A Prometheus that is unreachable, or a query that has gone stale,
        // must not empty the panel. The short-window answer is worse but it is
        // still an answer, and losing the whole capacity view because one
        // exporter moved would be a poor trade.
        let long = match long_history(state, &profile_id, start, end).await {
            Ok(series) => series,
            Err(e) => {
                tracing::debug!(profile = %profile_id, "long history unavailable: {e}");
                continue;
            }
        };
        if long.is_empty() {
            continue;
        }

        for risk in risks.iter_mut().filter(|r| r.profile_id == profile_id) {
            // Connection-level metrics only. A component series is keyed by
            // section and row, and a PromQL mapping declared per metric key
            // cannot say which container it meant.
            if risk.component.is_some() {
                continue;
            }
            if let Some(series) = long.iter().find(|s| s.key == risk.key) {
                risk.trend = series.trend;
            }
        }
    }

    risks.sort_by(|a, b| {
        match (
            a.trend.as_ref().and_then(|t| t.seconds_to_limit),
            b.trend.as_ref().and_then(|t| t.seconds_to_limit),
        ) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.used.total_cmp(&a.used),
        }
    });

    Ok(risks)
}

/// History for every connection at once, so the dashboard can draw a sparkline
/// on each card without a request per card.
pub async fn get_all_metric_history(
    state: &Arc<AppState>,
    minutes: Option<i64>,
) -> Result<std::collections::HashMap<String, Vec<MetricSeries>>> {
    let since = minutes
        .map(|m| chrono::Utc::now() - chrono::Duration::minutes(m.clamp(1, 1440)));
    let profiles: Vec<String> = state
        .config
        .read()
        .await
        .profiles
        .iter()
        .map(|p| p.id.clone())
        .collect();

    let history = state.history.read().await;
    Ok(profiles
        .into_iter()
        .map(|id| {
            let series = history.series(&id, since);
            (id, series)
        })
        .collect())
}

// ----------------------------------------------------------------- registry

/// Every service type this build supports, with the field specs the settings
/// form renders from. The frontend enumerates nothing itself.
pub async fn get_service_catalog() -> Result<Vec<ServiceDescriptor>> {
    Ok(registry::catalog())
}

// ----------------------------------------------------------------- settings

pub async fn get_config(state: &Arc<AppState>) -> Result<crate::config::AppConfig> {
    Ok(state.config.read().await.clone())
}

pub async fn secret_store_available() -> Result<bool> {
    Ok(secrets::store_available())
}

/// Create or update a connection. `secret` is written to the OS keychain and
/// never persisted to the config file. Passing `None` leaves any existing
/// credential in place, so editing a profile does not silently clear it.
pub async fn save_profile(
    state: &Arc<AppState>,
    mut profile: ConnectionProfile,
    secret: Option<String>,
) -> Result<crate::config::AppConfig> {
    if profile.name.trim().is_empty() {
        return Err(AppError::Config("the connection needs a name".to_string()));
    }
    if profile.id.trim().is_empty() {
        profile.id = uuid::Uuid::new_v4().to_string();
    }

    match secret {
        Some(value) if value.is_empty() => {
            secrets::delete_secret(&profile.secret_key()).await?;
            profile.has_secret = false;
        }
        Some(value) => {
            secrets::set_secret(&profile.secret_key(), &value).await?;
            profile.has_secret = true;
        }
        None => {
            profile.has_secret = secrets::get_secret(&profile.secret_key())
                .await
                .ok()
                .flatten()
                .is_some();
        }
    }

    {
        let mut config = state.config.write().await;
        match config.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile.clone(),
            None => config.profiles.push(profile.clone()),
        }
    }
    state.save_config().await?;

    // Restart just this connection so the change takes effect immediately.
    if profile.enabled {
        supervisor::start_profile(Arc::clone(state), profile).await;
    } else {
        supervisor::stop_profile(state, &profile.id).await;
    }

    Ok(state.config.read().await.clone())
}

pub async fn delete_profile(
    state: &Arc<AppState>,
    profile_id: String,
) -> Result<crate::config::AppConfig> {
    supervisor::stop_profile(state, &profile_id).await;

    let secret_key = {
        let mut config = state.config.write().await;
        let index = config
            .profiles
            .iter()
            .position(|p| p.id == profile_id)
            .ok_or_else(|| AppError::UnknownProfile(profile_id.clone()))?;
        config.profiles.remove(index).secret_key()
    };

    // Best effort: a stale keychain entry is untidy but not fatal.
    let _ = secrets::delete_secret(&secret_key).await;

    state.logs.write().await.drop_profile(&profile_id);
    state.alerts.write().await.drop_profile(&profile_id);
    state.save_config().await?;

    Ok(state.config.read().await.clone())
}

/// Try a profile's settings without saving it, so a bad URL or credential is
/// caught before it lands in the connection list.
pub async fn test_profile(
    profile: ConnectionProfile,
    secret: Option<String>,
) -> Result<HealthSnapshot> {
    let secret = match secret {
        Some(value) if !value.is_empty() => Some(value),
        // Editing an existing profile without retyping the credential.
        _ => secrets::get_secret(&profile.secret_key()).await.ok().flatten(),
    };
    let collector = crate::collectors::build_collector(&profile, secret).await?;
    collector.collect().await
}

/// Write one snapshot to a connection's declared warehouse, now, and hand
/// back the exact line that was sent.
///
/// The value is the line, not the success. Snapshot write-out is best-effort
/// by design — a failure is logged and swallowed so a warehouse outage never
/// becomes an opinion about the service being polled — which means the poll
/// loop cannot tell an operator whether it is working. This can, and showing
/// the payload means they can go look for exactly that measurement rather
/// than trusting that something arrived.
///
/// Takes the last snapshot root already has rather than polling: the point is
/// to test the write, and a collect here would fold two failures into one
/// message.
pub async fn test_snapshot_write(state: &Arc<AppState>, profile_id: String) -> Result<String> {
    let snapshot = state
        .snapshots
        .read()
        .await
        .get(&profile_id)
        .cloned()
        .ok_or_else(|| {
            crate::error::AppError::Collector(
                "this connection has not reported yet, so there is nothing to write. \
                 Save it, wait for one poll, then try again."
                    .into(),
            )
        })?;

    let sink = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .and_then(|p| p.snapshot_sink.clone())
            .filter(|s| s.is_configured())
            .ok_or_else(|| {
                crate::error::AppError::Collector(
                    "this connection is not writing snapshots anywhere. \
                     Pick a warehouse first."
                        .into(),
                )
            })?
    };

    let line = crate::influx::encode(&snapshot, &sink).ok_or_else(|| {
        crate::error::AppError::Collector(
            "this snapshot has nothing to write — no metrics and no timestamp.".into(),
        )
    })?;

    crate::influx::write_snapshot(state, &snapshot).await?;
    Ok(line)
}

// ---------------------------------------------------------------- assistant

pub async fn save_llm_settings(
    state: &Arc<AppState>,
    settings: LlmSettings,
    api_key: Option<String>,
) -> Result<crate::config::AppConfig> {
    let mut settings = settings;
    // No model named means the provider's own default — so a caller that
    // only picks a provider (onboarding, pasting a key) never has to keep a
    // second copy of the defaults that could drift from this one.
    if settings.model.trim().is_empty() {
        settings.model = settings.provider.default_model().to_string();
    }

    match api_key {
        Some(value) if value.is_empty() => {
            secrets::delete_secret(settings.provider.secret_key()).await?;
            settings.has_api_key = false;
        }
        Some(value) => {
            secrets::set_secret(settings.provider.secret_key(), &value).await?;
            settings.has_api_key = true;
        }
        None => {
            settings.has_api_key = secrets::get_secret(settings.provider.secret_key())
                .await
                .ok()
                .flatten()
                .is_some();
        }
    }

    state.config.write().await.llm = settings;
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

/// Model ids available for the configured provider and key.
pub async fn list_models(state: &Arc<AppState>) -> Result<Vec<String>> {
    let settings = state.config.read().await.llm.clone();
    let api_key = secrets::get_secret(settings.provider.secret_key()).await?;
    build_provider(&settings, api_key)?.list_models().await
}

/// Save a dashboard, or replace the one with the same id.
pub async fn save_dashboard(
    state: &Arc<AppState>,
    mut dashboard: crate::config::Dashboard,
) -> Result<crate::config::AppConfig> {
    if dashboard.title.trim().is_empty() {
        return Err(AppError::Config("the dashboard needs a title".to_string()));
    }
    if dashboard.panels.len() > crate::config::MAX_PANELS {
        return Err(AppError::Config(format!(
            "a dashboard holds at most {} panels",
            crate::config::MAX_PANELS
        )));
    }
    for panel in &mut dashboard.panels {
        panel.w = panel.w.clamp(1, 12);
        panel.h = panel.h.clamp(1, 6);
    }
    dashboard.updated_at = Some(chrono::Utc::now());
    {
        let mut config = state.config.write().await;
        match config.dashboards.iter_mut().find(|d| d.id == dashboard.id) {
            Some(existing) => *existing = dashboard,
            None => config.dashboards.push(dashboard),
        }
    }
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

pub async fn delete_dashboard(state: &Arc<AppState>, dashboard_id: String) -> Result<crate::config::AppConfig> {
    state.config.write().await.dashboards.retain(|d| d.id != dashboard_id);
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

/// Insert or replace an alert rule by id. An edited rule starts its "for"
/// wait again: the old condition holding says nothing about the new one.
pub async fn save_alert_rule(
    state: &Arc<AppState>,
    rule: crate::rules::AlertRule,
) -> Result<crate::config::AppConfig> {
    crate::rules::validate_rule(&rule)?;
    {
        let mut config = state.config.write().await;
        if !config.profiles.iter().any(|p| p.id == rule.condition.profile_id()) {
            return Err(AppError::Config("That connection no longer exists.".into()));
        }
        state.rule_pending.lock().await.forget(&rule.id);
        match config.alert_rules.iter_mut().find(|r| r.id == rule.id) {
            Some(slot) => *slot = rule,
            None => config.alert_rules.push(rule),
        }
    }
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

pub async fn delete_alert_rule(state: &Arc<AppState>, rule_id: String) -> Result<crate::config::AppConfig> {
    state.config.write().await.alert_rules.retain(|r| r.id != rule_id);
    state.rule_pending.lock().await.forget(&rule_id);
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

/// What a rule would have done over the last week, before anyone saves it.
pub async fn preview_alert_rule(
    state: &Arc<AppState>,
    rule: crate::rules::AlertRule,
) -> Result<crate::rules::RulePreview> {
    crate::rules::preview(state, &rule, chrono::Utc::now()).await
}

/// Incident records, newest first, brought up to date first.
pub async fn get_incident_log(state: &Arc<AppState>, limit: Option<usize>) -> Result<Vec<crate::incident_log::IncidentRecord>> {
    crate::incident_log::sync_now(state).await;
    Ok(state.incident_log.read().await.recent(limit.unwrap_or(100).clamp(1, 500)))
}

/// An owner, a status, a note or a public update, entered on the timeline.
/// `author` comes from the session on a server, never from the caller.
pub async fn update_incident(
    state: &Arc<AppState>,
    incident_id: String,
    change: crate::incident_log::Change,
    author: Option<String>,
) -> Result<crate::incident_log::IncidentRecord> {
    let mut log = state.incident_log.write().await;
    let record = log.apply(&incident_id, change, author, chrono::Utc::now())?;
    log.save(&crate::incident_log::log_path(&state.config_path))?;
    Ok(record)
}

/// A blameless postmortem draft from the record and nothing else.
pub async fn draft_postmortem(state: &Arc<AppState>, incident_id: String) -> Result<crate::incident_log::IncidentRecord> {
    let names: std::collections::HashMap<String, String> =
        state.config.read().await.profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
    let record = state
        .incident_log
        .read()
        .await
        .get(&incident_id)
        .cloned()
        .ok_or_else(|| AppError::Config("No such incident.".into()))?;
    let grounds = crate::incident_log::postmortem_grounds(&record, &names);
    let settings = state.config.read().await.llm.clone();
    let api_key = secrets::get_secret(settings.provider.secret_key()).await?;
    let provider = build_provider(&settings, api_key)?;
    let response = provider
        .complete(crate::llm::CompletionRequest {
            model: settings.model.clone(),
            system: crate::incident_log::POSTMORTEM_SYSTEM.to_string(),
            messages: vec![ChatMessage::user(grounds.join("\n"))],
            tools: Vec::new(),
            max_tokens: 1400,
            temperature: 0.2,
        })
        .await?;
    let text = response.text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::Llm("the model returned nothing".to_string()));
    }
    let mut log = state.incident_log.write().await;
    let record = log.set_postmortem(&incident_id, text, chrono::Utc::now())?;
    log.save(&crate::incident_log::log_path(&state.config_path))?;
    Ok(record)
}

/// The status page as it reads now, enabled or not — the app previews it.
pub async fn get_status_page(state: &Arc<AppState>) -> Result<crate::incident_log::StatusPageView> {
    let (settings, profiles) = {
        let config = state.config.read().await;
        (config.status_page.clone(), config.profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect::<Vec<_>>())
    };
    let components: Vec<(String, String, Health)> = {
        let snapshots = state.snapshots.read().await;
        profiles
            .into_iter()
            .map(|(id, name)| {
                let health = snapshots.get(&id).map(|s| s.health).unwrap_or(Health::Unknown);
                (id, name, health)
            })
            .collect()
    };
    let records = state.incident_log.read().await.recent(500);
    Ok(crate::incident_log::status_view(&settings, &components, &records, chrono::Utc::now()))
}

pub async fn save_status_page(
    state: &Arc<AppState>,
    settings: crate::incident_log::StatusPageSettings,
) -> Result<crate::config::AppConfig> {
    state.config.write().await.status_page = settings;
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

/// Insert or replace an objective by id.
pub async fn save_slo(state: &Arc<AppState>, slo: crate::slo::Slo) -> Result<crate::config::AppConfig> {
    crate::slo::validate(&slo)?;
    {
        let mut config = state.config.write().await;
        if !config.profiles.iter().any(|p| p.id == slo.profile_id) {
            return Err(AppError::Config("That connection no longer exists.".into()));
        }
        match config.slos.iter_mut().find(|s| s.id == slo.id) {
            Some(slot) => *slot = slo,
            None => config.slos.push(slo),
        }
    }
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

pub async fn delete_slo(state: &Arc<AppState>, slo_id: String) -> Result<crate::config::AppConfig> {
    state.config.write().await.slos.retain(|s| s.id != slo_id);
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

/// Each objective measured: how good, how much budget is left, how fast it burns.
pub async fn get_slo_status(state: &Arc<AppState>, profile_id: Option<String>) -> Result<Vec<crate::slo::SloStatus>> {
    Ok(crate::slo::status(state, profile_id.as_deref()).await)
}

/// Insert or replace a silence by id; silences already over are dropped.
pub async fn save_silence(state: &Arc<AppState>, silence: crate::rules::Silence) -> Result<crate::config::AppConfig> {
    let now = chrono::Utc::now();
    crate::rules::validate_silence(&silence, now)?;
    {
        let mut config = state.config.write().await;
        config.silences.retain(|s| s.ends_at > now && s.id != silence.id);
        config.silences.push(silence);
    }
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

pub async fn delete_silence(state: &Arc<AppState>, silence_id: String) -> Result<crate::config::AppConfig> {
    state.config.write().await.silences.retain(|s| s.id != silence_id);
    state.save_config().await?;
    Ok(state.config.read().await.clone())
}

/// How many series one query draws: the categorical palette's length. More
/// lines than colours is a query that wants a `by`, and Explore says so.
const QUERY_SERIES_CAP: usize = 8;
/// Lines one LogQL query brings back to Explore, newest first.
const EXPLORE_LINE_CAP: usize = 1000;
/// Traces one window lists in Explore.
const EXPLORE_TRACE_CAP: usize = 200;
/// Names one autocomplete list carries; the editor filters as you type.
const SUGGEST_CAP: usize = 5000;

async fn connection(state: &Arc<AppState>, profile_id: &str) -> Result<ConnectionProfile> {
    state
        .config
        .read()
        .await
        .profiles
        .iter()
        .find(|p| p.id == profile_id)
        .cloned()
        .ok_or_else(|| AppError::Collector("no such connection".into()))
}

/// The client and credentials for PromQL against a connection that speaks
/// it: Prometheus, or Gigapipe, which serves the same API at its root.
async fn promql_client(server: &ConnectionProfile) -> Result<(reqwest::Client, crate::promquery::Auth)> {
    use crate::model::ServiceKind;
    if !matches!(server.kind, ServiceKind::Prometheus | ServiceKind::Gigapipe) {
        return Err(AppError::Collector(format!(
            "{} is not a Prometheus or Gigapipe connection, so it cannot run PromQL",
            server.name
        )));
    }
    let auth = crate::promquery::Auth {
        username: server.username.clone().filter(|u| !u.is_empty()),
        secret: secrets::get_secret(&server.secret_key()).await.unwrap_or(None),
    };
    let client = crate::collectors::http_client(server.options.insecure_tls(), std::time::Duration::from_secs(30))?;
    Ok((client, auth))
}

/// Labelled series as charts take them — the first `cap`, with how many had
/// data. PromQL carries no unit; the panel or the person supplies one.
fn query_series(found: Vec<(String, Vec<crate::history::Point>)>, expr: &str, cap: usize) -> (Vec<MetricSeries>, usize) {
    let found: Vec<_> = found.into_iter().filter(|(_, points)| !points.is_empty()).collect();
    let matched = found.len();
    let series = found
        .into_iter()
        .take(cap)
        .map(|(labels, points)| {
            let values: Vec<f64> = points.iter().map(|p| p.v).collect();
            MetricSeries {
                label: if labels == "{}" { expr.to_string() } else { labels.clone() },
                key: labels,
                unit: crate::model::MetricUnit::Count,
                warn_above: None,
                change_pct: None,
                min: values.iter().copied().fold(f64::INFINITY, f64::min),
                max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                latest: *values.last().unwrap_or(&0.0),
                trend: None,
                points,
            }
        })
        .collect();
    (series, matched)
}

/// Series a panel draws when it carries PromQL, each labelled by its label
/// set: up to the palette's eight for lines, more for the panels whose rows
/// are the point — a histogram's buckets, a top-N table.
pub async fn query_prometheus(
    state: &Arc<AppState>,
    profile_id: String,
    expr: String,
    minutes: Option<i64>,
    max_series: Option<usize>,
) -> Result<Vec<MetricSeries>> {
    let server = connection(state, &profile_id).await?;
    let (client, auth) = promql_client(&server).await?;
    let end = chrono::Utc::now();
    let start = end - chrono::Duration::minutes(minutes.unwrap_or(60).clamp(1, STORE_MAX_MINUTES));
    let found = crate::promquery::range_all(&client, &server.endpoint, &auth, &expr, start, end).await?;
    Ok(query_series(found, &expr, max_series.unwrap_or(QUERY_SERIES_CAP).clamp(1, 60)).0)
}

/// Calls between two connections, as traces saw them: how many in the
/// sample, how many failed, and how long they took on average.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedEdge {
    pub from: String,
    pub to: String,
    pub calls: u64,
    pub errors: u64,
    pub mean_ms: f64,
}

/// Who calls whom, from traces; the declared dependencies are the topology's.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceMap {
    pub edges: Vec<ObservedEdge>,
    /// Traces read to build the edges.
    pub traces: usize,
    /// Services the traces name that no connection does, so the map can say
    /// what it cannot place.
    pub unknown: Vec<String>,
}

/// Traces assembled per tracing connection for the map: enough to see who
/// calls whom, few enough to answer in a second or two.
const MAP_TRACE_CAP: usize = 20;

/// The calls traces show between connections over the last `minutes`. A
/// service is placed on the connection with its name or id; the rest are
/// listed as unknown rather than guessed at.
pub async fn get_service_map(state: &Arc<AppState>, minutes: Option<i64>) -> Result<ServiceMap> {
    let to = chrono::Utc::now();
    let from = to - chrono::Duration::minutes(minutes.unwrap_or(30).clamp(5, 24 * 60));
    let profiles: Vec<(String, String)> = state
        .config
        .read()
        .await
        .profiles
        .iter()
        .filter(|p| p.enabled)
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();
    let place = |service: &str| {
        profiles
            .iter()
            .find(|(id, name)| name.eq_ignore_ascii_case(service) || id.eq_ignore_ascii_case(service))
            .map(|(id, _)| id.clone())
    };

    let mut seen: std::collections::HashMap<(String, String), (u64, u64, f64)> = std::collections::HashMap::new();
    let mut unknown = std::collections::BTreeSet::new();
    let mut read = 0;
    for (id, _) in &profiles {
        let Ok(collector) = state.collector(id).await else {
            continue;
        };
        // A collector without traces refuses, and refusing is not a failure.
        let Ok(found) = collector
            .detail(&crate::collectors::DetailQuery::TracesInWindow { from, to, limit: TRACE_SEARCH_CAP })
            .await
        else {
            continue;
        };
        let Ok(summaries) = serde_json::from_value::<Vec<crate::traces::TraceSummary>>(found) else {
            continue;
        };
        for summary in summaries.into_iter().take(MAP_TRACE_CAP) {
            let Ok(assembled) = collector
                .detail(&crate::collectors::DetailQuery::Trace { trace_id: summary.trace_id })
                .await
            else {
                continue;
            };
            let Ok(trace) = serde_json::from_value::<crate::traces::Trace>(assembled) else {
                continue;
            };
            read += 1;
            for call in crate::traces::calls(&trace) {
                match (place(&call.from), place(&call.to)) {
                    (Some(a), Some(b)) if a != b => {
                        let edge = seen.entry((a, b)).or_default();
                        edge.0 += 1;
                        edge.1 += u64::from(call.error);
                        edge.2 += call.ms;
                    }
                    (a, b) => {
                        if a.is_none() {
                            unknown.insert(call.from);
                        }
                        if b.is_none() {
                            unknown.insert(call.to);
                        }
                    }
                }
            }
        }
    }

    let mut edges: Vec<ObservedEdge> = seen
        .into_iter()
        .map(|((from, to), (calls, errors, total))| ObservedEdge { from, to, calls, errors, mean_ms: total / calls as f64 })
        .collect();
    edges.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.from.cmp(&b.from)));
    Ok(ServiceMap { edges, traces: read, unknown: unknown.into_iter().collect() })
}

/// What Explore asked one connection, and the answer: series for PromQL or
/// a LogQL metric query, lines for a LogQL stream selector, traces for a
/// trace search. `matched` counts every series, so "8 of 23" can be said.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExploreResult {
    pub series: Vec<MetricSeries>,
    pub matched: usize,
    pub lines: Vec<LogEvent>,
    pub traces: Vec<crate::traces::TraceSummary>,
}

/// One query from Explore over an absolute window, in milliseconds since the
/// epoch. PromQL runs on Prometheus and Gigapipe; LogQL and the trace list
/// on Gigapipe.
pub async fn explore(
    state: &Arc<AppState>,
    profile_id: String,
    language: String,
    query: String,
    start_ms: i64,
    end_ms: i64,
) -> Result<ExploreResult> {
    use crate::collectors::{DetailQuery, GigapipeRead};
    let (Some(start), Some(end)) = (
        chrono::DateTime::from_timestamp_millis(start_ms),
        chrono::DateTime::from_timestamp_millis(end_ms),
    ) else {
        return Err(AppError::Collector("that window is not a time range".into()));
    };
    if end <= start {
        return Err(AppError::Collector("the window ends before it starts".into()));
    }
    let server = connection(state, &profile_id).await?;
    if language == "promql" {
        let (client, auth) = promql_client(&server).await?;
        let found = crate::promquery::range_all(&client, &server.endpoint, &auth, &query, start, end).await?;
        let (series, matched) = query_series(found, &query, QUERY_SERIES_CAP);
        return Ok(ExploreResult { series, matched, ..Default::default() });
    }
    // An OTLP receiver keeps the traces sent to it, and only those.
    let received_traces = server.kind == crate::model::ServiceKind::Otlp && language == "traces";
    if server.kind != crate::model::ServiceKind::Gigapipe && !received_traces {
        return Err(AppError::Collector(format!(
            "{} keeps no logs or traces root can query: LogQL and traces need a Gigapipe connection",
            server.name
        )));
    }
    let collector = state.collector(&profile_id).await?;
    match language.as_str() {
        "logql" => {
            let nanos = |t: chrono::DateTime<chrono::Utc>| t.timestamp_nanos_opt().unwrap_or(0).to_string();
            let response = collector
                .detail(&DetailQuery::GigapipeRead(GigapipeRead::LokiQuery {
                    query: query.clone(),
                    start: Some(nanos(start)),
                    end: Some(nanos(end)),
                    limit: Some(EXPLORE_LINE_CAP.to_string()),
                }))
                .await?;
            if response["data"]["resultType"] == "streams" {
                let mut lines = crate::collectors::gigapipe::lines_in(&response, &server.id, &server.name);
                // Newest first, as anyone reading a tail expects.
                lines.sort_by_key(|line| std::cmp::Reverse(line.timestamp));
                lines.truncate(EXPLORE_LINE_CAP);
                Ok(ExploreResult { lines, ..Default::default() })
            } else {
                let (series, matched) = query_series(crate::promquery::series_in(response)?, &query, QUERY_SERIES_CAP);
                Ok(ExploreResult { series, matched, ..Default::default() })
            }
        }
        "traces" => {
            let response = collector
                .detail(&DetailQuery::TracesInWindow { from: start, to: end, limit: EXPLORE_TRACE_CAP })
                .await?;
            let traces = serde_json::from_value(response).map_err(|e| {
                AppError::Collector(format!("the trace search answered in a shape root cannot read: {e}"))
            })?;
            Ok(ExploreResult { traces, ..Default::default() })
        }
        other => Err(AppError::Collector(format!(
            "Explore speaks promql, logql and traces, not `{other}`"
        ))),
    }
}

/// Names for Explore's autocomplete: label names when `label` is absent,
/// else that label's values — `__name__` gives PromQL's metric names.
pub async fn explore_suggest(
    state: &Arc<AppState>,
    profile_id: String,
    language: String,
    label: Option<String>,
) -> Result<Vec<String>> {
    use crate::collectors::{DetailQuery, GigapipeRead};
    let server = connection(state, &profile_id).await?;
    let mut names = if language == "logql" {
        if server.kind != crate::model::ServiceKind::Gigapipe {
            return Err(AppError::Collector(format!("{} has no LogQL labels to list", server.name)));
        }
        let read = match label {
            None => GigapipeRead::LokiLabels { start: None, end: None },
            Some(label) => GigapipeRead::LokiLabelValues { label, start: None, end: None },
        };
        let response = state.collector(&profile_id).await?.detail(&DetailQuery::GigapipeRead(read)).await?;
        serde_json::from_value::<Vec<String>>(response["data"].clone()).unwrap_or_default()
    } else {
        let (client, auth) = promql_client(&server).await?;
        crate::promquery::label_values(&client, &server.endpoint, &auth, label.as_deref()).await?
    };
    names.sort();
    names.dedup();
    names.truncate(SUGGEST_CAP);
    Ok(names)
}

/// The models of an Ollama running at its default address, for onboarding to
/// offer in one click. `None` when nothing answers within a second and a half
/// — the common case, and not an error: most machines have no Ollama.
pub async fn detect_ollama() -> Result<Option<Vec<String>>> {
    let settings = crate::config::LlmSettings {
        provider: crate::config::LlmProviderKind::Ollama,
        model: crate::config::LlmProviderKind::Ollama.default_model().to_string(),
        ..Default::default()
    };
    let provider = build_provider(&settings, None)?;
    match tokio::time::timeout(std::time::Duration::from_millis(1500), provider.list_models()).await {
        Ok(Ok(models)) => Ok(Some(models)),
        _ => Ok(None),
    }
}

pub async fn send_message(
    state: &Arc<AppState>,
    conversation_id: String,
    message: String,
) -> Result<AgentTurn> {
    if message.trim().is_empty() {
        return Err(AppError::Llm("the message is empty".to_string()));
    }
    agent::run_turn(state.events().await, Arc::clone(state), conversation_id, message).await
}

/// A short model reading of one failing component, for the matrix tooltip.
///
/// Deliberately not `send_message`. That runs the agent loop: a system prompt
/// listing every tool, several round trips while it queries the estate, and a
/// conversation that grows. None of it is affordable on a hover, and none of it
/// is needed — everything worth saying about one row is already in this
/// process. So this builds the facts itself and makes exactly one call with no
/// tools and a small budget.
///
/// Healthy rows are refused rather than answered. A model asked what is wrong
/// with something that is fine will find something, and a tooltip that
/// speculates over 357 working components is worse than no tooltip at all.
pub async fn explain_component(
    state: &Arc<AppState>,
    profile_id: String,
    section_id: String,
    row_id: String,
) -> Result<crate::state::ComponentReading> {
    let (snapshot, section, row) = {
        let snapshots = state.snapshots.read().await;
        let snapshot = snapshots
            .get(&profile_id)
            .ok_or_else(|| AppError::Collector("no snapshot for that connection".into()))?;
        let section = snapshot
            .sections
            .iter()
            .find(|s| s.id == section_id)
            .ok_or_else(|| AppError::Collector("no such section".into()))?;
        let row = section
            .rows
            .iter()
            .find(|r| r.id == row_id)
            .ok_or_else(|| AppError::Collector("no such component".into()))?;
        (snapshot.clone(), section.clone(), row.clone())
    };

    if row.health == Health::Healthy {
        return Err(AppError::Llm(
            "this component is healthy — there is nothing to explain".to_string(),
        ));
    }

    let alert = state
        .alerts
        .read()
        .await
        .all()
        .into_iter()
        .find(|a| {
            a.profile_id == profile_id
                && a.active
                && a.component.as_deref().is_some_and(|c| c == row.name)
        });

    // The fingerprint is the state, not the clock: the same broken container
    // asked about twice is one call, and a container that breaks differently is
    // a new one.
    let fingerprint = format!(
        "{}|{}|{}|{}",
        row.health.label(),
        row.summary,
        alert.as_ref().map(|a| a.id.as_str()).unwrap_or(""),
        alert
            .as_ref()
            .map(|a| a.first_seen.to_rfc3339())
            .unwrap_or_default(),
    );
    let key = format!("{profile_id}:{section_id}:{row_id}");

    if let Some(cached) = state.readings.read().await.get(&key) {
        if cached.fingerprint == fingerprint {
            return Ok(cached.reading.clone());
        }
    }

    let grounds = component_grounds(state, &snapshot, &section, &row, alert.as_ref()).await;

    let (settings, _mode) = {
        let config = state.config.read().await;
        (config.llm.clone(), config.llm.agent_mode)
    };
    let api_key = secrets::get_secret(settings.provider.secret_key()).await?;
    let provider = build_provider(&settings, api_key)?;

    let response = provider
        .complete(crate::llm::CompletionRequest {
            model: settings.model.clone(),
            system: COMPONENT_READING_SYSTEM.to_string(),
            messages: vec![ChatMessage::user(grounds.join("\n"))],
            tools: Vec::new(),
            max_tokens: 160,
            temperature: 0.1,
        })
        .await?;

    let text = response.text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::Llm("the model returned nothing".to_string()));
    }

    let reading = crate::state::ComponentReading {
        text,
        model: settings.model.clone(),
        grounds,
        read_at: chrono::Utc::now(),
    };

    state.readings.write().await.insert(
        key,
        crate::state::CachedReading {
            fingerprint,
            reading: reading.clone(),
        },
    );

    Ok(reading)
}

/// What happened on one connection between two moments: the chart's
/// "✦ Explain this", asked of a window someone dragged across.
///
/// Built the way `explain_component` is — the facts root is already holding,
/// one completion, no tools — because an agent loop would spend its round
/// trips rediscovering what this process can read straight out of memory.
/// Cached on the facts rather than the clock: the same window over the same
/// evidence is one call, however many times it is asked.
pub async fn explain_window(
    state: &Arc<AppState>,
    profile_id: String,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Result<crate::state::ComponentReading> {
    if to <= from {
        return Err(AppError::Llm("drag across a chart to choose a window first".to_string()));
    }
    let grounds = window_grounds(state, &profile_id, from, to).await?;
    let fingerprint = grounds.join("\n");
    let key = format!("window:{profile_id}:{}:{}", from.timestamp(), to.timestamp());
    if let Some(cached) = state.readings.read().await.get(&key) {
        if cached.fingerprint == fingerprint {
            return Ok(cached.reading.clone());
        }
    }

    let settings = state.config.read().await.llm.clone();
    let api_key = secrets::get_secret(settings.provider.secret_key()).await?;
    let provider = build_provider(&settings, api_key)?;
    let response = provider
        .complete(crate::llm::CompletionRequest {
            model: settings.model.clone(),
            system: WINDOW_READING_SYSTEM.to_string(),
            messages: vec![ChatMessage::user(grounds.join("\n"))],
            tools: Vec::new(),
            max_tokens: 220,
            temperature: 0.1,
        })
        .await?;

    let text = response.text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::Llm("the model returned nothing".to_string()));
    }
    let reading = crate::state::ComponentReading {
        text,
        model: settings.model.clone(),
        grounds,
        read_at: chrono::Utc::now(),
    };
    state.readings.write().await.insert(
        key,
        crate::state::CachedReading { fingerprint, reading: reading.clone() },
    );
    Ok(reading)
}

const WINDOW_READING_SYSTEM: &str = "\
You are explaining what happened on ONE connection of an infrastructure estate \
during a window an operator has just selected on a chart.

Answer in one to three short sentences, under 70 words, no preamble, no lists. \
Lead with the most likely cause and name the facts that support it — a change, \
an alert, a log message, a metric's movement — with their times. If something \
elsewhere in the estate moved at the same moment, say whether it looks like \
cause or consequence.

Use only the facts given. If they do not explain the movement, say so plainly \
and name the one thing worth checking next: \"nothing recorded explains this\" \
is a useful answer and a guess is not.";

/// The facts about one window, in the order an operator would read them out:
/// what the connection is, what its metrics did against the stretch just
/// before, what fired, what changed, what it logged, and what moved elsewhere.
async fn window_grounds(
    state: &Arc<AppState>,
    profile_id: &str,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<String>> {
    use chrono::Duration;
    let hm = |t: chrono::DateTime<chrono::Utc>| t.format("%H:%M:%S").to_string();
    let span = to - from;
    let baseline_from = from - span;
    let mut out = Vec::new();

    let snapshot = state
        .snapshots
        .read()
        .await
        .get(profile_id)
        .cloned()
        .ok_or_else(|| AppError::Collector("no snapshot for that connection".into()))?;
    out.push(format!(
        "connection: {} — now {}: {}",
        snapshot.profile_name,
        snapshot.health.label(),
        snapshot.summary
    ));
    out.push(format!(
        "window: {} to {} UTC ({} min)",
        hm(from),
        hm(to),
        span.num_minutes().max(1)
    ));

    // Metrics: the window against the same length of time just before it.
    let reach = (chrono::Utc::now() - baseline_from).num_minutes() + 1;
    let series = get_metric_history(state, profile_id.to_string(), Some(reach.max(1))).await?;
    for s in &series {
        let inside: Vec<&crate::history::Point> =
            s.points.iter().filter(|p| p.t >= from && p.t <= to).collect();
        let (Some(first), Some(last)) = (inside.first(), inside.last()) else {
            continue;
        };
        let peak = inside
            .iter()
            .copied()
            .max_by(|a, b| a.v.partial_cmp(&b.v).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(first);
        let low = inside.iter().map(|p| p.v).fold(f64::INFINITY, f64::min);
        let before: Vec<f64> = s
            .points
            .iter()
            .filter(|p| p.t >= baseline_from && p.t < from)
            .map(|p| p.v)
            .collect();
        let baseline = if before.is_empty() {
            String::new()
        } else {
            format!(
                "; the {} min before averaged {}",
                span.num_minutes().max(1),
                unit_value(before.iter().sum::<f64>() / before.len() as f64, s.unit)
            )
        };
        out.push(format!(
            "metric {}: {} → {} (low {}, peak {} at {}){baseline}",
            s.label,
            unit_value(first.v, s.unit),
            unit_value(last.v, s.unit),
            unit_value(low, s.unit),
            unit_value(peak.v, s.unit),
            hm(peak.t),
        ));
    }

    // Alerts on this connection that overlapped the window, then anything that
    // started elsewhere around the same time.
    let alerts = state.alerts.read().await.all();
    for a in alerts
        .iter()
        .filter(|a| a.profile_id == profile_id && a.first_seen <= to && a.last_seen >= from - Duration::minutes(5))
        .take(6)
    {
        out.push(format!(
            "alert {} {}: \"{}\"{}",
            hm(a.first_seen),
            format!("{:?}", a.severity).to_lowercase(),
            a.title,
            if a.active { " — still firing" } else { "" }
        ));
    }
    for a in alerts
        .iter()
        .filter(|a| a.profile_id != profile_id && a.first_seen <= to && a.first_seen >= from - Duration::minutes(10))
        .take(5)
    {
        out.push(format!("elsewhere {} {}: \"{}\"", hm(a.first_seen), a.profile_name, a.title));
    }

    // Changes in the half hour leading in: a deploy just before a spike is the
    // first thing anyone asks about.
    let changes = state
        .changes
        .read()
        .await
        .query(Some(profile_id), Some(from - Duration::minutes(30)), 20);
    for c in changes.iter().filter(|c| c.at <= to).take(6) {
        out.push(format!("change {} {}", hm(c.at), c.summary));
    }

    // Logs at warn and above, counted, with the messages that repeated most.
    let filter = crate::state::LogFilter {
        profile_ids: Some(vec![profile_id.to_string()]),
        min_level: Some(crate::model::LogLevel::Warn),
        since: Some(from),
        limit: Some(5000),
        ..Default::default()
    };
    let lines: Vec<crate::model::LogEvent> = state
        .logs
        .read()
        .await
        .query(&filter)
        .into_iter()
        .filter(|e| e.timestamp <= to)
        .collect();
    if !lines.is_empty() {
        let errors = lines
            .iter()
            .filter(|e| e.level >= crate::model::LogLevel::Error)
            .count();
        out.push(format!(
            "logs: {} lines at warn or above in the window, {} of them errors",
            lines.len(),
            errors
        ));
        for (message, count) in repeated(&lines, 3) {
            out.push(format!("log ×{count}: {message}"));
        }
    }

    Ok(out)
}

/// The messages that repeat most, with numbers and ids folded so that
/// "timeout after 3001ms" and "timeout after 2987ms" count as one.
fn repeated(lines: &[crate::model::LogEvent], take: usize) -> Vec<(String, usize)> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for line in lines {
        *counts.entry(fold_message(&line.message)).or_default() += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(take);
    ranked
}

/// A message with its numbers folded, so a line that differs only by a
/// latency, a port or an address is recognised as the same line.
///
/// Each whole number becomes one `#`, not each digit: folding digit by digit
/// made `10.0.3.9` and `10.0.3.29` two different messages, so one recurring
/// failure was counted as several smaller ones depending on how many digits
/// its address happened to have.
fn fold_message(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(110));
    let mut in_number = false;
    for c in message.chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('#');
            }
            in_number = true;
        } else {
            out.push(c);
            in_number = false;
        }
        if out.len() >= 110 {
            break;
        }
    }
    out
}

/// A value the way the chart labels it, so the model quotes what the operator sees.
fn unit_value(v: f64, unit: crate::model::MetricUnit) -> String {
    use crate::model::MetricUnit as U;
    match unit {
        U::Percent => format!("{v:.0}%"),
        U::Millis => format!("{v:.0} ms"),
        U::Seconds => format!("{v:.1} s"),
        U::PerSecond => format!("{v:.1}/s"),
        U::Count => format!("{v:.0}"),
        U::Bytes => {
            let gb = v / 1e9;
            if gb >= 1.0 {
                format!("{gb:.1} GB")
            } else {
                format!("{:.0} MB", v / 1e6)
            }
        }
        U::BytesPerSecond => format!("{}/s", unit_value(v, U::Bytes)),
    }
}

/// What a handful of selected log lines mean: the log view's
/// "✦ Explain these lines".
///
/// The lines come from the client because it is holding exactly what was
/// selected; the buffer may already have rotated some of them out. The rest
/// of the facts are read here: the state of each connection the lines came
/// from, its alerts and recent changes, how often each message has repeated
/// in the last hour — which is what separates a new failure from a chronic
/// one — and errors elsewhere in the same couple of minutes.
pub async fn explain_lines(
    state: &Arc<AppState>,
    lines: Vec<crate::model::LogEvent>,
) -> Result<crate::state::ComponentReading> {
    if lines.is_empty() {
        return Err(AppError::Llm("select at least one line first".to_string()));
    }
    let lines: Vec<_> = lines.into_iter().take(LINES_CAP).collect();
    let grounds = lines_grounds(state, &lines).await;
    let fingerprint = grounds.join("\n");
    let mut ids: Vec<&str> = lines.iter().map(|l| l.id.as_str()).collect();
    ids.sort_unstable();
    let key = format!("lines:{}", ids.join(","));
    if let Some(cached) = state.readings.read().await.get(&key) {
        if cached.fingerprint == fingerprint {
            return Ok(cached.reading.clone());
        }
    }

    let settings = state.config.read().await.llm.clone();
    let api_key = secrets::get_secret(settings.provider.secret_key()).await?;
    let provider = build_provider(&settings, api_key)?;
    let response = provider
        .complete(crate::llm::CompletionRequest {
            model: settings.model.clone(),
            system: LINES_READING_SYSTEM.to_string(),
            messages: vec![ChatMessage::user(grounds.join("\n"))],
            tools: Vec::new(),
            max_tokens: 240,
            temperature: 0.1,
        })
        .await?;

    let text = response.text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::Llm("the model returned nothing".to_string()));
    }
    let reading = crate::state::ComponentReading {
        text,
        model: settings.model.clone(),
        grounds,
        read_at: chrono::Utc::now(),
    };
    state.readings.write().await.insert(
        key,
        crate::state::CachedReading { fingerprint, reading: reading.clone() },
    );
    Ok(reading)
}

/// More than this is a search, not a selection.
const LINES_CAP: usize = 40;

const LINES_READING_SYSTEM: &str = "\
You are reading log lines an operator has just selected in an infrastructure \
monitoring tool.

Answer in two or three short sentences, under 80 words, no preamble, no lists. \
Say what the lines mean in plain words, the most likely cause with the facts \
that support it, and whether this is new or recurring — use the recurrence \
counts. If errors elsewhere moved at the same moment, say whether they look \
like cause or consequence.

Use only the facts given. If they do not explain the lines, say so plainly \
and name the one thing worth checking next.";

async fn lines_grounds(state: &Arc<AppState>, lines: &[crate::model::LogEvent]) -> Vec<String> {
    use chrono::Duration;
    let hm = |t: chrono::DateTime<chrono::Utc>| t.format("%H:%M:%S").to_string();
    let earliest = lines.iter().map(|l| l.timestamp).min().unwrap_or_else(chrono::Utc::now);
    let latest = lines.iter().map(|l| l.timestamp).max().unwrap_or_else(chrono::Utc::now);
    let mut out = vec![format!(
        "selected: {} line{} from {} to {} UTC",
        lines.len(),
        if lines.len() == 1 { "" } else { "s" },
        hm(earliest),
        hm(latest)
    )];
    for l in lines {
        let message: String = l.message.chars().take(300).collect();
        out.push(format!(
            "line {} {} [{} / {}] {}",
            hm(l.timestamp),
            format!("{:?}", l.level).to_uppercase(),
            l.profile_name,
            l.source,
            message
        ));
    }

    // The connections the lines came from: their state, alerts and changes.
    let mut connections: Vec<&str> = lines.iter().map(|l| l.profile_id.as_str()).collect();
    connections.sort_unstable();
    connections.dedup();
    // Read what is needed and let go, rather than hold the snapshot lock
    // while waiting on the change store's.
    let states: Vec<Option<String>> = {
        let snapshots = state.snapshots.read().await;
        connections
            .iter()
            .map(|id| {
                snapshots.get(*id).map(|s| {
                    format!("connection {}: now {}: {}", s.profile_name, s.health.label(), s.summary)
                })
            })
            .collect()
    };
    let alerts = state.alerts.read().await.all();
    for (id, line) in connections.iter().zip(states) {
        if let Some(line) = line {
            out.push(line);
        }
        for a in alerts.iter().filter(|a| a.profile_id == *id && a.active).take(4) {
            out.push(format!(
                "alert {} {}: \"{}\"",
                hm(a.first_seen),
                format!("{:?}", a.severity).to_lowercase(),
                a.title
            ));
        }
        let changes = state
            .changes
            .read()
            .await
            .query(Some(id), Some(earliest - Duration::minutes(30)), 10);
        for c in changes.iter().filter(|c| c.at <= latest).take(4) {
            out.push(format!("change {} {}", hm(c.at), c.summary));
        }
    }

    // Recurrence: how often each selected message has come up in the last
    // hour on the same connections, and since when.
    let hour = crate::state::LogFilter {
        profile_ids: Some(connections.iter().map(|c| c.to_string()).collect()),
        since: Some(latest - Duration::hours(1)),
        limit: Some(20_000),
        ..Default::default()
    };
    let pool = state.logs.read().await.query(&hour);
    let mut seen = std::collections::HashSet::new();
    for l in lines {
        let folded = fold_message(&l.message);
        if !seen.insert(folded.clone()) || seen.len() > 3 {
            continue;
        }
        let matching: Vec<&crate::model::LogEvent> =
            pool.iter().filter(|e| fold_message(&e.message) == folded).collect();
        if let Some(first) = matching.iter().map(|e| e.timestamp).min() {
            out.push(format!(
                "recurrence: \"{folded}\" ×{} in the last hour, first at {}",
                matching.len(),
                hm(first)
            ));
        }
    }

    // Errors elsewhere around the same moment.
    let near = crate::state::LogFilter {
        min_level: Some(crate::model::LogLevel::Error),
        since: Some(earliest - Duration::minutes(2)),
        limit: Some(2_000),
        ..Default::default()
    };
    let elsewhere: Vec<crate::model::LogEvent> = state
        .logs
        .read()
        .await
        .query(&near)
        .into_iter()
        .filter(|e| e.timestamp <= latest + Duration::minutes(2) && !connections.contains(&e.profile_id.as_str()))
        .take(5)
        .collect();
    for e in &elsewhere {
        let message: String = e.message.chars().take(160).collect();
        out.push(format!("elsewhere {} [{}] {}", hm(e.timestamp), e.profile_name, message));
    }

    out
}

/// One alert's likely root cause, in a sentence — the note "✦ Explain" writes
/// into the alert's own row.
///
/// The facts are the alert itself and the window around its onset: the ten
/// minutes before it fired, when a cause shows up, and the first few minutes
/// after, when its consequences do — read exactly the way a chart window is,
/// so a note and an "Explain this" over the same stretch agree.
pub async fn explain_alert(
    state: &Arc<AppState>,
    alert_id: String,
) -> Result<crate::state::ComponentReading> {
    let alert = state
        .alerts
        .read()
        .await
        .all()
        .into_iter()
        .find(|a| a.id == alert_id)
        .ok_or_else(|| AppError::Llm("that alert is no longer tracked".to_string()))?;
    let grounds = alert_grounds(state, &alert).await?;
    let fingerprint = grounds.join("\n");
    let key = format!("alert:{}:{}", alert.fingerprint, alert.first_seen.timestamp());
    if let Some(cached) = state.readings.read().await.get(&key) {
        if cached.fingerprint == fingerprint {
            return Ok(cached.reading.clone());
        }
    }

    let settings = state.config.read().await.llm.clone();
    let api_key = secrets::get_secret(settings.provider.secret_key()).await?;
    let provider = build_provider(&settings, api_key)?;
    let response = provider
        .complete(crate::llm::CompletionRequest {
            model: settings.model.clone(),
            system: ALERT_NOTE_SYSTEM.to_string(),
            messages: vec![ChatMessage::user(grounds.join("\n"))],
            tools: Vec::new(),
            max_tokens: 120,
            temperature: 0.1,
        })
        .await?;

    let text = response.text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::Llm("the model returned nothing".to_string()));
    }
    let reading = crate::state::ComponentReading {
        text,
        model: settings.model.clone(),
        grounds,
        read_at: chrono::Utc::now(),
    };
    state.readings.write().await.insert(
        key,
        crate::state::CachedReading { fingerprint, reading: reading.clone() },
    );
    Ok(reading)
}

const ALERT_NOTE_SYSTEM: &str = "\
You are writing a one-line note into an alert's row in an infrastructure \
monitoring tool, for an operator scanning a list of alerts.

One sentence, under 30 words, no preamble. Name the most likely root cause and \
the single fact that points to it — a change, a dependency, a log message, a \
metric's movement — with its time. Do not restate the alert's title. If the \
facts do not point to a cause, write \"No cause in the evidence\" and name the \
one thing to check first.";

/// The alert, then the window around its onset.
async fn alert_grounds(
    state: &Arc<AppState>,
    alert: &crate::model::Alert,
) -> Result<Vec<String>> {
    use chrono::Duration;
    let hm = |t: chrono::DateTime<chrono::Utc>| t.format("%H:%M:%S").to_string();
    let mut out = vec![format!(
        "the alert: {} \"{}\" on {}{} — first seen {}, {}",
        format!("{:?}", alert.severity).to_lowercase(),
        alert.title,
        alert.profile_name,
        alert
            .component
            .as_deref()
            .map(|c| format!(" / {c}"))
            .unwrap_or_default(),
        hm(alert.first_seen),
        if alert.active { "still firing" } else { "resolved" }
    )];
    if !alert.detail.trim().is_empty() {
        out.push(format!("detail: {}", alert.detail.trim()));
    }
    let to = (alert.first_seen + Duration::minutes(5)).min(chrono::Utc::now());
    let from = alert.first_seen - Duration::minutes(10);
    out.extend(window_grounds(state, &alert.profile_id, from, to.max(from + Duration::minutes(1))).await?);
    Ok(out)
}

/// Why one incident happened, by the model, on evidence root already has.
///
/// Deliberately not `send_message`, for the same reason `explain_component` is
/// not: the agent loop would spend several round trips rediscovering facts this
/// process is already holding. One call, no tools, a structured answer.
///
/// Cached on the incident's own state rather than on the clock. Re-opening the
/// same incident is free; an incident that grew a member, recovered one, or was
/// acknowledged is a different question and is asked again.
pub async fn explain_incident(
    state: &Arc<AppState>,
    incident_id: String,
    refresh: Option<bool>,
) -> Result<crate::rca::RootCause> {
    let incident = {
        let profiles = state.config.read().await.profiles.clone();
        let graph = crate::topology::Topology::build(&profiles);
        let dashboard = state.dashboard().await;
        incidents::correlate(&dashboard.alerts, &dashboard.snapshots, &graph)
            .into_iter()
            .find(|i| i.id == incident_id)
            .ok_or_else(|| {
                crate::error::AppError::Collector(
                    "that incident has resolved or regrouped — there is nothing to analyse"
                        .into(),
                )
            })?
    };

    // The state, not the clock. `last_seen` moves every poll the incident is
    // still true, so it is deliberately absent: including it would re-bill
    // every ten seconds for an answer that has not changed.
    let fingerprint = format!(
        "{}|{}|{}|{}",
        incident.id,
        incident.alert_ids.len(),
        incident.acknowledged,
        incident.started.to_rfc3339(),
    );

    if !refresh.unwrap_or(false) {
        if let Some(cached) = state.analyses.read().await.get(&incident_id) {
            if cached.fingerprint == fingerprint {
                return Ok(cached.cause.clone());
            }
        }
    }

    let cause = crate::rca::analyse(state, &incident).await?;
    state.analyses.write().await.insert(
        incident_id,
        crate::state::CachedAnalysis {
            fingerprint,
            cause: cause.clone(),
        },
    );
    Ok(cause)
}

/// The instruction is mostly a list of refusals, because the failure mode of a
/// two-sentence summary is confident invention.
const COMPONENT_READING_SYSTEM: &str = "\
You are reading the state of ONE component of an infrastructure estate for an \
operator who is hovering over it and has not yet decided whether to open it.

Answer in at most two sentences, under 40 words, no preamble. Do not restate \
the component's name, health, or any number you were given — the operator can \
already see those. Say what the evidence indicates and, if it supports one, the \
single most likely cause. If it does not, say which of the facts below to check \
first and why.

Use only the facts given. If they are thin, say so plainly rather than \
speculating: \"nothing here explains it\" is a useful answer and a guess is not.";

/// The facts, in the order an operator would read them out.
async fn component_grounds(
    state: &AppState,
    snapshot: &HealthSnapshot,
    section: &crate::model::Section,
    row: &crate::model::Row,
    alert: Option<&Alert>,
) -> Vec<String> {
    let mut grounds = vec![
        format!(
            "connection: {} ({})",
            snapshot.profile_name,
            snapshot.kind.label()
        ),
        format!("component: {} · {}", row.name, section.label),
        format!("health: {}", row.health.label()),
    ];

    if !row.summary.is_empty() {
        grounds.push(format!("collector says: {}", row.summary));
    }

    for column in &section.columns {
        if column.primary {
            continue;
        }
        if let Some(value) = row.cells.get(&column.key) {
            if let Some(text) = cell_text(value) {
                grounds.push(format!("{}: {}", column.label, text));
            }
        }
    }

    if let Some(alert) = alert {
        grounds.push(format!(
            "alert: {} — {} ({}, firing since {})",
            alert.title,
            alert.detail,
            format!("{:?}", alert.severity).to_lowercase(),
            alert.first_seen.to_rfc3339(),
        ));
    }

    // A deploy inside the window is the single most useful fact there is, and
    // the one an operator most often has not checked yet.
    let changes = state
        .changes
        .read()
        .await
        .query(Some(&snapshot.profile_id), None, 3);
    for change in changes {
        grounds.push(format!(
            "change: {} {} on {} at {}",
            change.component,
            match change.from.as_deref() {
                Some(from) => format!("{from} → {}", change.to),
                None => change.to.clone(),
            },
            change.profile_name,
            change.at.to_rfc3339(),
        ));
    }

    let lines = state.logs.read().await.query(&LogFilter {
        profile_ids: Some(vec![snapshot.profile_id.clone()]),
        search: Some(row.name.clone()),
        limit: Some(6),
        ..Default::default()
    });
    for line in lines {
        grounds.push(format!(
            "log {} [{}] {}",
            line.timestamp.to_rfc3339(),
            format!("{:?}", line.level).to_lowercase(),
            line.message,
        ));
    }

    grounds
}

/// The same formatting the table uses, flattened to text for a prompt.
fn cell_text(value: &crate::model::CellValue) -> Option<String> {
    use crate::model::CellValue;
    match value {
        CellValue::Null => None,
        CellValue::Text { value } => (!value.is_empty()).then(|| value.clone()),
        CellValue::Number { value } | CellValue::Gauge { value, .. } => Some(format!("{value}")),
        CellValue::Duration { value } => Some(format!("{value}s")),
        CellValue::Timestamp { value } => Some(value.to_rfc3339()),
        CellValue::Tags { value } => {
            (!value.is_empty()).then(|| value.join(", "))
        }
        CellValue::Status { value } => Some(value.label().to_string()),
    }
}

pub async fn resolve_confirmation(
    state: &Arc<AppState>,
    call_id: String,
    approved: bool,
) -> Result<()> {
    agent::resolve_confirmation(state, &call_id, approved).await
}

/// Approval requests still waiting — used to restore the dialog if the UI
/// reloads while the assistant is mid-turn.
pub async fn pending_confirmations(state: &Arc<AppState>) -> Result<Vec<PendingConfirmation>> {
    let mut pending: Vec<PendingConfirmation> =
        state.pending_meta.read().await.values().cloned().collect();
    pending.sort_by_key(|p| p.requested_at);
    Ok(pending)
}

pub async fn get_conversation(
    state: &Arc<AppState>,
    conversation_id: String,
) -> Result<Vec<ChatMessage>> {
    Ok(state
        .conversations
        .read()
        .await
        .get(&conversation_id)
        .cloned()
        .unwrap_or_default())
}

pub async fn clear_conversation(state: &Arc<AppState>, conversation_id: String) -> Result<()> {
    state.conversations.write().await.remove(&conversation_id);
    Ok(())
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use crate::model::{
        Alert, AlertSeverity, ChangeEvent, ChangeKind, ChangeSource, Health, HealthSnapshot,
        LogEvent, LogLevel, LogStream, Metric, MetricUnit, ServiceKind,
    };
    use chrono::{Duration, Utc};

    fn snapshot(cpu: f64, at: chrono::DateTime<Utc>) -> HealthSnapshot {
        HealthSnapshot {
            profile_id: "p1".into(),
            profile_name: "orders-api".into(),
            kind: ServiceKind::Docker,
            health: Health::Down,
            summary: "3 of 8 containers exited".into(),
            metrics: vec![Metric::new("cpu", "CPU", cpu, MetricUnit::Percent)],
            sections: Vec::new(),
            checked_at: at,
            latency_ms: 40,
            error: None,
        }
    }

    #[tokio::test]
    async fn a_window_carries_the_facts_that_would_explain_it() {
        let state = Arc::new(AppState::new(
            crate::config::AppConfig::default(),
            std::path::PathBuf::from("/tmp/root-window-test.json"),
        ));
        let now = Utc::now();
        // The store stamps an alert with the moment it is raised, which is
        // after `now`; the window runs a few seconds past it so the alert is
        // inside, as it would be on a chart that is still live.
        let (from, to) = (now - Duration::minutes(10), now + Duration::seconds(5));

        state.snapshots.write().await.insert("p1".into(), snapshot(94.0, now));
        {
            let mut history = state.history.write().await;
            // A quiet twenty minutes, then a climb through the window.
            for i in 0..10 {
                history.record(&snapshot(40.0, now - Duration::minutes(20) + Duration::minutes(i)));
            }
            for i in 0..10 {
                history.record(&snapshot(40.0 + i as f64 * 6.0, from + Duration::seconds(30) + Duration::minutes(i)));
            }
        }
        state.alerts.write().await.reconcile(
            "p1",
            vec![Alert {
                id: "a1".into(),
                fingerprint: "f1".into(),
                profile_id: "p1".into(),
                profile_name: "orders-api".into(),
                kind: ServiceKind::Docker,
                severity: AlertSeverity::Critical,
                title: "3 containers exited with code 137".into(),
                detail: String::new(),
                component: Some("worker".into()),
                first_seen: now - Duration::minutes(6),
                last_seen: now,
                active: true,
                acknowledged: false,
            }],
        );
        state.changes.write().await.record(vec![ChangeEvent {
            id: "c1".into(),
            profile_id: "p1".into(),
            profile_name: "orders-api".into(),
            kind: ServiceKind::Docker,
            change: ChangeKind::Deployed,
            source: ChangeSource::Observed,
            section: "containers".into(),
            component: "worker".into(),
            from: Some("2.14.2".into()),
            to: "2.14.3".into(),
            at: from - Duration::minutes(4),
            summary: "worker 2.14.2 → 2.14.3".into(),
        }]);
        state.logs.write().await.push_many((0..5).map(|i| LogEvent {
            id: format!("l{i}"),
            profile_id: "p1".into(),
            profile_name: "orders-api".into(),
            kind: ServiceKind::Docker,
            source: "worker".into(),
            level: LogLevel::Error,
            message: format!("OOMKilled after {}ms", 3000 + i),
            timestamp: now - Duration::minutes(5),
            stream: LogStream::Runtime,
        }));

        let facts = window_grounds(&state, "p1", from, to).await.unwrap().join("\n");

        assert!(facts.contains("connection: orders-api — now down"), "{facts}");
        assert!(facts.contains("metric CPU: 40% → 94% (low 40%, peak 94%"), "{facts}");
        assert!(facts.contains("the 10 min before averaged 40%"), "the window is read against what came before it: {facts}");
        assert!(facts.contains("alert") && facts.contains("code 137") && facts.contains("still firing"), "{facts}");
        assert!(facts.contains("change") && facts.contains("2.14.3"), "a deploy in the lead-in is evidence: {facts}");
        assert!(facts.contains("logs: 5 lines at warn or above in the window, 5 of them errors"), "{facts}");
        assert!(facts.contains("log ×5: OOMKilled after #ms"), "numbers fold, so five timeouts are one message: {facts}");
    }

    #[tokio::test]
    async fn an_empty_or_backwards_window_is_refused_before_anything_is_asked() {
        let state = Arc::new(AppState::new(
            crate::config::AppConfig::default(),
            std::path::PathBuf::from("/tmp/root-window-test.json"),
        ));
        let now = Utc::now();
        let err = explain_window(&state, "p1".into(), now, now - Duration::minutes(1))
            .await
            .expect_err("a backwards window has nothing in it");
        assert!(err.to_string().contains("drag across a chart"), "{err}");
    }

    #[tokio::test]
    async fn selected_lines_carry_their_recurrence_and_what_failed_elsewhere() {
        let state = Arc::new(AppState::new(
            crate::config::AppConfig::default(),
            std::path::PathBuf::from("/tmp/root-lines-test.json"),
        ));
        let now = Utc::now();
        let line = |id: &str, profile: &str, name: &str, level: LogLevel, message: String, at| LogEvent {
            id: id.into(),
            profile_id: profile.into(),
            profile_name: name.into(),
            kind: ServiceKind::Docker,
            source: "worker".into(),
            level,
            message,
            timestamp: at,
            stream: LogStream::Runtime,
        };
        state.snapshots.write().await.insert("p1".into(), snapshot(90.0, now));
        // Thirty of the same failure over the last half hour, then the two selected.
        state.logs.write().await.push_many((0..30).map(|i| {
            line(&format!("old{i}"), "p1", "orders-api", LogLevel::Error,
                 format!("connection reset by 10.0.3.{i}"), now - Duration::minutes(30) + Duration::minutes(i))
        }));
        state.logs.write().await.push_many([line(
            "other", "p2", "payments-db", LogLevel::Error, "replication lag 42s".into(), now - Duration::seconds(40),
        )]);
        let selected = vec![
            line("s1", "p1", "orders-api", LogLevel::Error, "connection reset by 10.0.3.9".into(), now - Duration::seconds(30)),
            line("s2", "p1", "orders-api", LogLevel::Warn, "retrying in 500ms".into(), now - Duration::seconds(29)),
        ];

        let facts = lines_grounds(&state, &selected).await.join("\n");

        assert!(facts.contains("selected: 2 lines"), "{facts}");
        assert!(facts.contains("line ") && facts.contains("ERROR [orders-api / worker] connection reset by 10.0.3.9"), "{facts}");
        assert!(facts.contains("connection orders-api: now down"), "{facts}");
        assert!(
            facts.contains("recurrence: \"connection reset by #.#.#.#\" ×30 in the last hour"),
            "a selected line is counted against the hour before it, numbers folded: {facts}"
        );
        assert!(facts.contains("elsewhere") && facts.contains("payments-db") && facts.contains("replication lag"), "{facts}");
    }

    #[tokio::test]
    async fn an_alert_note_is_read_from_the_alert_and_the_minutes_before_it() {
        let state = Arc::new(AppState::new(
            crate::config::AppConfig::default(),
            std::path::PathBuf::from("/tmp/root-alert-test.json"),
        ));
        let now = Utc::now();
        state.snapshots.write().await.insert("p1".into(), snapshot(94.0, now));
        let raised = state.alerts.write().await.reconcile(
            "p1",
            vec![Alert {
                id: "a1".into(),
                fingerprint: "f1".into(),
                profile_id: "p1".into(),
                profile_name: "orders-api".into(),
                kind: ServiceKind::Docker,
                severity: AlertSeverity::Critical,
                title: "3 containers exited with code 137".into(),
                detail: "worker-3 OOMKilled".into(),
                component: Some("worker".into()),
                first_seen: now,
                last_seen: now,
                active: true,
                acknowledged: false,
            }],
        );
        let alert = raised.into_iter().next().expect("raised");
        state.changes.write().await.record(vec![ChangeEvent {
            id: "c1".into(),
            profile_id: "p1".into(),
            profile_name: "orders-api".into(),
            kind: ServiceKind::Docker,
            change: ChangeKind::Deployed,
            source: ChangeSource::Observed,
            section: "containers".into(),
            component: "worker".into(),
            from: Some("2.14.2".into()),
            to: "2.14.3".into(),
            at: alert.first_seen - Duration::minutes(4),
            summary: "worker 2.14.2 → 2.14.3".into(),
        }]);

        let facts = alert_grounds(&state, &alert).await.unwrap();

        assert!(facts[0].starts_with("the alert: critical \"3 containers exited with code 137\" on orders-api / worker"), "the alert leads: {facts:?}");
        assert!(facts.iter().any(|f| f == "detail: worker-3 OOMKilled"), "{facts:?}");
        assert!(facts.iter().any(|f| f.starts_with("change ") && f.contains("2.14.3")), "the deploy four minutes before is in the window: {facts:?}");
    }

    #[tokio::test]
    async fn an_alert_that_has_gone_is_said_to_have_gone() {
        let state = Arc::new(AppState::new(
            crate::config::AppConfig::default(),
            std::path::PathBuf::from("/tmp/root-alert-test.json"),
        ));
        let err = explain_alert(&state, "nope".into()).await.expect_err("unknown alert");
        assert!(err.to_string().contains("no longer tracked"), "{err}");
    }
}

#[cfg(test)]
mod dashboard_tests {
    use super::*;

    #[tokio::test]
    async fn a_rule_and_its_silence_save_and_delete_in_the_shape_the_ui_sends() {
        let dir = std::env::temp_dir().join(format!("root-rules-{}", uuid::Uuid::new_v4()));
        let mut config = crate::config::AppConfig::default();
        config.profiles.push(
            serde_json::from_value(serde_json::json!({
                "id": "cache", "name": "session-cache", "kind": "redis", "endpoint": "redis://localhost:6379"
            }))
            .unwrap(),
        );
        let state = Arc::new(AppState::new(config, dir.join("config.json")));
        let rule: crate::rules::AlertRule = serde_json::from_value(serde_json::json!({
            "id": "r1", "name": "Hit rate low", "severity": "warning", "labels": { "team": "platform" },
            "condition": { "source": "metric", "profileId": "cache", "metricKey": "hit", "op": "below", "threshold": 95.0 },
            "forSecs": 300
        }))
        .unwrap();
        assert_eq!(save_alert_rule(&state, rule.clone()).await.unwrap().alert_rules.len(), 1);
        assert_eq!(save_alert_rule(&state, rule.clone()).await.unwrap().alert_rules.len(), 1, "same id replaces");

        let now = chrono::Utc::now();
        let silence: crate::rules::Silence = serde_json::from_value(serde_json::json!({
            "id": "s1", "ruleId": "r1", "startsAt": now, "endsAt": now + chrono::Duration::hours(1), "comment": "fix deploying"
        }))
        .unwrap();
        assert_eq!(save_silence(&state, silence).await.unwrap().silences.len(), 1);

        let orphan: crate::rules::AlertRule = serde_json::from_value(serde_json::json!({
            "id": "r2", "name": "Gone", "severity": "critical",
            "condition": { "source": "log", "profileId": "gone", "pattern": "panic", "windowSecs": 300, "op": "above", "threshold": 0.0 }
        }))
        .unwrap();
        assert!(save_alert_rule(&state, orphan).await.is_err(), "a rule on a connection that is not there");

        assert!(delete_alert_rule(&state, "r1".into()).await.unwrap().alert_rules.is_empty());
        assert!(delete_silence(&state, "s1".into()).await.unwrap().silences.is_empty());
    }

    #[tokio::test]
    async fn explore_refuses_what_a_connection_cannot_answer() {
        let dir = std::env::temp_dir().join(format!("root-explore-{}", uuid::Uuid::new_v4()));
        let mut config = crate::config::AppConfig::default();
        config.profiles.push(
            serde_json::from_value(serde_json::json!({
                "id": "cache", "name": "session-cache", "kind": "redis", "endpoint": "redis://localhost:6379"
            }))
            .unwrap(),
        );
        let state = Arc::new(AppState::new(config, dir.join("config.json")));
        let now = chrono::Utc::now().timestamp_millis();

        let backwards = explore(&state, "cache".into(), "promql".into(), "up".into(), now, now - 1).await;
        assert!(backwards.unwrap_err().to_string().contains("ends before it starts"));
        let promql = explore(&state, "cache".into(), "promql".into(), "up".into(), now - 60_000, now).await;
        assert!(promql.unwrap_err().to_string().contains("cannot run PromQL"));
        let logql = explore(&state, "cache".into(), "logql".into(), "{app=\"x\"}".into(), now - 60_000, now).await;
        assert!(logql.unwrap_err().to_string().contains("need a Gigapipe connection"));
        let missing = explore_suggest(&state, "nope".into(), "promql".into(), None).await;
        assert!(missing.unwrap_err().to_string().contains("no such connection"));
    }

    fn dashboard(title: &str, width: u8) -> crate::config::Dashboard {
        serde_json::from_value(serde_json::json!({
            "id": "d1",
            "title": title,
            "panels": [
                { "id": "p1", "title": "CPU", "kind": "timeseries", "w": width, "h": 2,
                  "query": { "source": "metric", "profileId": "$service", "metricKey": "cpu" } },
                { "id": "p2", "title": "Requests", "kind": "timeseries", "w": 6, "h": 2,
                  "query": { "source": "promql", "profileId": "prom", "expr": "sum(rate(http_requests_total[5m]))" } },
                { "id": "p3", "title": "Health", "kind": "state", "w": 6, "h": 2,
                  "query": { "source": "health", "profileIds": [] } }
            ],
            "variables": { "service": "orders-api" }
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn a_dashboard_saves_replaces_and_deletes() {
        let dir = std::env::temp_dir().join(format!("root-dash-{}", uuid::Uuid::new_v4()));
        let state = Arc::new(AppState::new(crate::config::AppConfig::default(), dir.join("config.json")));

        let config = save_dashboard(&state, dashboard("Checkout", 40)).await.unwrap();
        assert_eq!(config.dashboards.len(), 1);
        assert_eq!(config.dashboards[0].panels[0].w, 12, "a width past the grid is clamped to it");
        assert!(config.dashboards[0].updated_at.is_some());

        let config = save_dashboard(&state, dashboard("Checkout API", 6)).await.unwrap();
        assert_eq!(config.dashboards.len(), 1, "the same id replaces, it does not duplicate");
        assert_eq!(config.dashboards[0].title, "Checkout API");

        assert!(save_dashboard(&state, dashboard("  ", 6)).await.is_err(), "a title is required");

        let config = delete_dashboard(&state, "d1".into()).await.unwrap();
        assert!(config.dashboards.is_empty());
    }
}
