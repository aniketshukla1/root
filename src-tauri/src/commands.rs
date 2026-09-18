//! Tauri command surface — everything the UI can ask the backend to do.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};

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

type Shared<'a> = State<'a, Arc<AppState>>;

// ---------------------------------------------------------------- dashboard

#[tauri::command]
pub async fn get_dashboard(state: Shared<'_>) -> Result<DashboardState> {
    Ok(state.dashboard().await)
}

#[tauri::command]
pub async fn refresh_connection(state: Shared<'_>, profile_id: String) -> Result<HealthSnapshot> {
    supervisor::poll_once(&state, &profile_id).await
}

#[tauri::command]
pub async fn acknowledge_alert(state: Shared<'_>, alert_id: String) -> Result<Alert> {
    state.alerts.write().await.acknowledge(&alert_id)
}

/// Run a mutating action the operator clicked on a section row.
///
/// This shares `action_for` with the assistant so a button and a tool call
/// resolve to exactly the same operation. The gate differs by design: the agent
/// must ask, whereas a click *is* the operator's decision — the UI confirms
/// before calling here.
#[tauri::command]
pub async fn run_row_action(
    state: Shared<'_>,
    profile_id: String,
    tool: String,
    args: serde_json::Value,
) -> Result<String> {
    let action = crate::agent::tools::action_for(&tool, &args)?;
    let result = state.collector(&profile_id).await?.run_action(&action).await?;
    // Reflect the change immediately rather than waiting for the next poll.
    let _ = supervisor::poll_once(&state, &profile_id).await;
    Ok(result)
}

// --------------------------------------------------------------------- logs

#[tauri::command]
pub async fn query_logs(state: Shared<'_>, filter: LogFilter) -> Result<Vec<LogEvent>> {
    Ok(state.logs.read().await.query(&filter))
}

#[tauri::command]
pub async fn clear_logs(state: Shared<'_>) -> Result<()> {
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
#[tauri::command]
pub async fn log_patterns(
    state: Shared<'_>,
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
#[tauri::command]
pub async fn log_histogram(
    state: Shared<'_>,
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

/// Lines per lane. Enough to see the shape; the log view has the rest.
const LANE_EVENT_CAP: usize = 12;
/// A metric has to move this much to be worth naming.
const MOVEMENT_THRESHOLD_PCT: f64 = 15.0;

/// What every system was doing around one moment.
///
/// This is the answer to "why did this build fail". Rather than interleaving
/// every service into one stream — which is what made the shared log view
/// unreadable — it groups by source and aligns on time, so the Docker OOM that
/// actually caused the Jenkins failure sits in its own lane at the same
/// instant, next to the database connection spike.
#[tauri::command]
pub async fn correlate(
    state: Shared<'_>,
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

        // A lane with nothing in it is still worth showing — "elasticsearch
        // logged nothing" is evidence too. But drop it when the connection
        // contributed neither events nor movement anywhere in the window.
        if total == 0 && movements.is_empty() {
            continue;
        }

        lanes.push(CorrelationLane {
            profile_id,
            profile_name,
            kind,
            total,
            error_count,
            events: lane_events,
            movements,
        });
    }

    // Most likely culprit first: errors, then volume, then movement.
    lanes.sort_by(|a, b| {
        b.error_count
            .cmp(&a.error_count)
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
#[tauri::command]
pub async fn get_build_console(
    state: Shared<'_>,
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
#[tauri::command]
pub async fn get_trace(
    state: Shared<'_>,
    profile_id: String,
    trace_id: String,
) -> Result<serde_json::Value> {
    state
        .collector(&profile_id)
        .await?
        .detail(&crate::collectors::DetailQuery::Trace { trace_id })
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

#[tauri::command]
pub async fn get_ingest_settings(state: Shared<'_>) -> Result<IngestStatus> {
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
#[tauri::command]
pub async fn save_ingest_settings(
    app: tauri::AppHandle,
    state: Shared<'_>,
    enabled: bool,
    port: u16,
) -> Result<IngestStatus> {
    if enabled && port < 1024 {
        return Err(AppError::Rejected(
            "choose a port above 1023; privileged ports need elevation".into(),
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

    crate::ingest::restart(&state, &app).await?;
    get_ingest_settings(state).await
}

/// Mint a new token and return it once. Any pipeline using the old one stops
/// working immediately, which is the point.
#[tauri::command]
pub async fn rotate_ingest_token(app: tauri::AppHandle, state: Shared<'_>) -> Result<String> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    crate::secrets::set_secret(crate::ingest::TOKEN_KEY, &token).await?;
    crate::ingest::restart(&state, &app).await?;
    Ok(token)
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

#[tauri::command]
pub async fn get_mcp_settings(state: Shared<'_>) -> Result<McpStatus> {
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
        tools: crate::agent::tools::catalog(crate::config::AgentMode::ReadOnly)
            .into_iter()
            .filter(|t| !t.mutating)
            .map(|t| t.spec.name)
            .collect(),
    })
}

/// Enable, disable, or move the MCP listener. Returns the resulting status.
///
/// Enabling with no token stored mints one, so a user never ends up with a
/// listener that cannot authenticate anybody.
#[tauri::command]
pub async fn save_mcp_settings(
    state: Shared<'_>,
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

    crate::mcp::http::restart(&state).await?;
    get_mcp_settings(state).await
}

/// Mint a new token and return it once. Any client using the old one stops
/// working immediately, which is the point.
#[tauri::command]
pub async fn rotate_mcp_token(state: Shared<'_>) -> Result<String> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    crate::secrets::set_secret(crate::mcp::http::TOKEN_KEY, &token).await?;
    crate::mcp::http::restart(&state).await?;
    Ok(token)
}

/// Deploys and other changes, newest first.
///
/// Omit `profile_id` for the whole estate — the correlation view wants every
/// connection's changes in one window, while a service dashboard wants its own.
#[tauri::command]
pub async fn get_change_events(
    state: Shared<'_>,
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

/// Rolling metric history for one connection.
#[tauri::command]
pub async fn get_metric_history(
    state: Shared<'_>,
    profile_id: String,
    minutes: Option<i64>,
) -> Result<Vec<MetricSeries>> {
    let since = minutes
        .map(|m| chrono::Utc::now() - chrono::Duration::minutes(m.clamp(1, 1440)));
    Ok(state.history.read().await.series(&profile_id, since))
}

/// The declared dependency graph, resolved both ways.
///
/// `roots` answers the question the graph exists for: of everything currently
/// failing, which failures are not explained by another failure. It is a
/// narrowing, never a verdict — an undeclared edge is invisible here, so a
/// root may just be a connection nobody finished configuring.
#[tauri::command]
pub async fn get_topology(state: Shared<'_>) -> Result<TopologyView> {
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

#[tauri::command]
pub async fn get_team(state: Shared<'_>) -> Result<TeamState> {
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
#[tauri::command]
pub async fn save_team_server(state: Shared<'_>, server_url: Option<String>) -> Result<TeamState> {
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
#[tauri::command]
pub async fn sign_in(state: Shared<'_>, email: String, password: String) -> Result<TeamState> {
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

#[tauri::command]
pub async fn sign_out(state: Shared<'_>) -> Result<TeamState> {
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
#[tauri::command]
pub async fn get_workspace(state: Shared<'_>) -> Result<team::Workspace> {
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
#[tauri::command]
pub async fn get_incidents(state: Shared<'_>) -> Result<Vec<Incident>> {
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
#[tauri::command]
pub async fn get_health_runs(
    state: Shared<'_>,
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
#[tauri::command]
pub async fn get_component_history(
    state: Shared<'_>,
    profile_id: String,
    section_id: String,
    row_id: String,
    minutes: Option<i64>,
) -> Result<Vec<MetricSeries>> {
    let since = minutes.map(|m| chrono::Utc::now() - chrono::Duration::minutes(m.clamp(1, 1440)));
    Ok(state
        .history
        .read()
        .await
        .entity_series(&profile_id, &section_id, &row_id, since))
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
#[tauri::command]
pub async fn get_config_documents(
    state: Shared<'_>,
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
#[tauri::command]
pub async fn get_findings(
    state: Shared<'_>,
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
#[tauri::command]
pub async fn get_long_history(
    state: Shared<'_>,
    profile_id: String,
    hours: Option<i64>,
) -> Result<Vec<crate::history::MetricSeries>> {
    let hours = hours.unwrap_or(24 * 14).clamp(1, 24 * 365);
    let end = chrono::Utc::now();
    let start = end - chrono::Duration::hours(hours);
    long_history(&state, &profile_id, start, end).await
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
#[tauri::command]
pub async fn clear_claims(state: Shared<'_>, profile_id: String) -> Result<()> {
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
#[tauri::command]
pub async fn get_runtime_report(state: Shared<'_>) -> Result<RuntimeReport> {
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
#[tauri::command]
pub async fn run_scenarios() -> Result<crate::scenarios::SuiteResult> {
    let library = crate::scenarios::library()
        .map_err(crate::error::AppError::Collector)?;
    Ok(crate::scenarios::run_all(&library))
}

/// The scenarios themselves, for reading rather than running.
#[tauri::command]
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
#[tauri::command]
pub async fn run_backtest(
    state: Shared<'_>,
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

    let documents = collect_config(&state, &profile_id).await?;

    // Prefer the estate's own long history. The in-memory ring is the fallback
    // and is rarely long enough to fit anything — which the caveats say.
    let series = match long_history(&state, &profile_id, start, end).await {
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
#[tauri::command]
pub async fn record_verdict(
    state: Shared<'_>,
    verdict: crate::journal::Verdict,
) -> Result<i64> {
    state.journal().await?.record_verdict(&verdict).await
}

/// Record whether root called an incident before it happened.
///
/// The half of the measurement that ratings on findings cannot produce: if
/// root never mentioned the thing that broke, there was no card to rate.
#[tauri::command]
pub async fn record_recall(state: Shared<'_>, recall: crate::journal::Recall) -> Result<i64> {
    state.journal().await?.record_recall(&recall).await
}

/// The latest judgement per finding, so the UI can show what is already rated.
#[tauri::command]
pub async fn get_verdicts(
    state: Shared<'_>,
    limit: Option<i64>,
) -> Result<Vec<crate::journal::RecordedVerdict>> {
    state.journal().await?.verdicts(limit.unwrap_or(500)).await
}

/// Incidents already judged, so nobody is asked the same question twice.
#[tauri::command]
pub async fn get_judged_incidents(state: Shared<'_>) -> Result<Vec<String>> {
    state.journal().await?.judged_incidents().await
}

/// Precision by provenance and by rule, and recall from the incident side.
#[tauri::command]
pub async fn get_scorecard(state: Shared<'_>) -> Result<crate::journal::Scorecard> {
    state.journal().await?.scorecard().await
}

/// What in the estate is heading for a limit, and when it gets there.
///
/// One call rather than one per connection per component, because the ranking
/// needs every series at once to be a ranking at all — and the alternative is
/// shipping the whole history store to the client to sort six rows.
#[tauri::command]
pub async fn get_capacity_risks(
    state: Shared<'_>,
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
        let long = match long_history(&state, &profile_id, start, end).await {
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
#[tauri::command]
pub async fn get_all_metric_history(
    state: Shared<'_>,
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
#[tauri::command]
pub async fn get_service_catalog() -> Result<Vec<ServiceDescriptor>> {
    Ok(registry::catalog())
}

// ----------------------------------------------------------------- settings

#[tauri::command]
pub async fn get_config(state: Shared<'_>) -> Result<crate::config::AppConfig> {
    Ok(state.config.read().await.clone())
}

#[tauri::command]
pub async fn secret_store_available() -> Result<bool> {
    Ok(secrets::store_available())
}

/// Create or update a connection. `secret` is written to the OS keychain and
/// never persisted to the config file. Passing `None` leaves any existing
/// credential in place, so editing a profile does not silently clear it.
#[tauri::command]
pub async fn save_profile(
    app: AppHandle,
    state: Shared<'_>,
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
        supervisor::start_profile(app, Arc::clone(&state), profile).await;
    } else {
        supervisor::stop_profile(&state, &profile.id).await;
    }

    Ok(state.config.read().await.clone())
}

#[tauri::command]
pub async fn delete_profile(
    state: Shared<'_>,
    profile_id: String,
) -> Result<crate::config::AppConfig> {
    supervisor::stop_profile(&state, &profile_id).await;

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
#[tauri::command]
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

// ---------------------------------------------------------------- assistant

#[tauri::command]
pub async fn save_llm_settings(
    state: Shared<'_>,
    settings: LlmSettings,
    api_key: Option<String>,
) -> Result<crate::config::AppConfig> {
    let mut settings = settings;

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
#[tauri::command]
pub async fn list_models(state: Shared<'_>) -> Result<Vec<String>> {
    let settings = state.config.read().await.llm.clone();
    let api_key = secrets::get_secret(settings.provider.secret_key()).await?;
    build_provider(&settings, api_key)?.list_models().await
}

#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    state: Shared<'_>,
    conversation_id: String,
    message: String,
) -> Result<AgentTurn> {
    if message.trim().is_empty() {
        return Err(AppError::Llm("the message is empty".to_string()));
    }
    agent::run_turn(app, Arc::clone(&state), conversation_id, message).await
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
#[tauri::command]
pub async fn explain_component(
    state: Shared<'_>,
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

    let grounds = component_grounds(&state, &snapshot, &section, &row, alert.as_ref()).await;

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

#[tauri::command]
pub async fn resolve_confirmation(
    state: Shared<'_>,
    call_id: String,
    approved: bool,
) -> Result<()> {
    agent::resolve_confirmation(&state, &call_id, approved).await
}

/// Approval requests still waiting — used to restore the dialog if the UI
/// reloads while the assistant is mid-turn.
#[tauri::command]
pub async fn pending_confirmations(state: Shared<'_>) -> Result<Vec<PendingConfirmation>> {
    let mut pending: Vec<PendingConfirmation> =
        state.pending_meta.read().await.values().cloned().collect();
    pending.sort_by_key(|p| p.requested_at);
    Ok(pending)
}

#[tauri::command]
pub async fn get_conversation(
    state: Shared<'_>,
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

#[tauri::command]
pub async fn clear_conversation(state: Shared<'_>, conversation_id: String) -> Result<()> {
    state.conversations.write().await.remove(&conversation_id);
    Ok(())
}
