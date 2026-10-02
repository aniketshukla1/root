//! The Tauri command surface.
//!
//! Sixty-four one-line wrappers, and not one of them contains logic. The
//! implementations live in [`crate::commands`] and take `&Arc<AppState>`,
//! because the same sixty-four are also reachable over HTTP from
//! [`crate::rpc`] and two implementations of the same command would drift the
//! first week — silently, and in the direction nobody tests.
//!
//! What lives here is the one thing Tauri needs and axum does not: the
//! `#[tauri::command]` attribute, which generates the IPC glue and injects
//! `State`. That is the whole reason this file exists rather than the
//! attribute sitting on the implementations.
//!
//! Argument names matter and are not cosmetic. Tauri matches them against the
//! keys the frontend sends, so renaming one here breaks a call site that the
//! compiler cannot see.

#![allow(clippy::too_many_arguments)]

use std::sync::Arc;

use tauri::State;

use crate::agent::AgentTurn;
use crate::commands;
use crate::store as inframonitoring_store;
use crate::commands::{Correlation, IngestStatus, LogPattern, McpStatus, PredictionReport, PredictionStatus, RuntimeReport, TeamState, TopologyView};
use crate::config::{ConnectionProfile, LlmSettings};
use crate::error::Result;
use crate::llm::ChatMessage;
use crate::team;
use crate::history::{HistogramBucket, MetricSeries};
use crate::incidents::Incident;
use crate::model::{Alert, DashboardState, HealthSnapshot, LogEvent};
use crate::registry::ServiceDescriptor;
use crate::state::{AppState, LogFilter, PendingConfirmation};

type Shared<'a> = State<'a, Arc<AppState>>;

#[tauri::command]
pub async fn get_dashboard(state: Shared<'_>) -> Result<DashboardState> {
    commands::get_dashboard(&state).await
}

#[tauri::command]
pub async fn refresh_connection(state: Shared<'_>, profile_id: String) -> Result<HealthSnapshot> {
    commands::refresh_connection(&state, profile_id).await
}

#[tauri::command]
pub async fn acknowledge_alert(state: Shared<'_>, alert_id: String) -> Result<Alert> {
    commands::acknowledge_alert(&state, alert_id).await
}

#[tauri::command]
pub async fn run_row_action(state: Shared<'_>, profile_id: String, tool: String, args: serde_json::Value) -> Result<String> {
    commands::run_row_action(&state, profile_id, tool, args).await
}

#[tauri::command]
pub async fn query_logs(state: Shared<'_>, filter: LogFilter) -> Result<Vec<LogEvent>> {
    commands::query_logs(&state, filter).await
}

#[tauri::command]
pub async fn clear_logs(state: Shared<'_>) -> Result<()> {
    commands::clear_logs(&state).await
}

#[tauri::command]
pub async fn log_patterns(state: Shared<'_>, filter: LogFilter, minutes: Option<i64>) -> Result<Vec<LogPattern>> {
    commands::log_patterns(&state, filter, minutes).await
}

#[tauri::command]
pub async fn log_histogram(state: Shared<'_>, filter: LogFilter, minutes: Option<i64>, buckets: Option<usize>) -> Result<Vec<HistogramBucket>> {
    commands::log_histogram(&state, filter, minutes, buckets).await
}

#[tauri::command]
pub async fn correlate(state: Shared<'_>, at: chrono::DateTime<chrono::Utc>, window_secs: Option<i64>) -> Result<Correlation> {
    commands::correlate(&state, at, window_secs).await
}

#[tauri::command]
pub async fn get_build_console(state: Shared<'_>, profile_id: String, job: String, build: Option<i64>, tail_lines: Option<usize>) -> Result<serde_json::Value> {
    commands::get_build_console(&state, profile_id, job, build, tail_lines).await
}

#[tauri::command]
pub async fn get_trace(state: Shared<'_>, profile_id: String, trace_id: String) -> Result<serde_json::Value> {
    commands::get_trace(&state, profile_id, trace_id).await
}

#[tauri::command]
pub async fn get_profile(state: Shared<'_>, profile_id: String, query: Option<String>, minutes: Option<i64>) -> Result<serde_json::Value> {
    commands::get_profile(&state, profile_id, query, minutes).await
}

#[tauri::command]
pub async fn get_ingest_settings(state: Shared<'_>) -> Result<IngestStatus> {
    commands::get_ingest_settings(&state).await
}

#[tauri::command]
pub async fn save_ingest_settings(state: Shared<'_>, enabled: bool, port: u16) -> Result<IngestStatus> {
    commands::save_ingest_settings(&state, enabled, port).await
}

#[tauri::command]
pub async fn get_notifications(state: Shared<'_>) -> Result<crate::notify::NotificationsView> {
    commands::get_notifications(&state).await
}

#[tauri::command]
pub async fn save_notification_channel(
    state: Shared<'_>,
    channel: crate::notify::NotifyChannel,
    target: Option<String>,
) -> Result<crate::notify::NotificationsView> {
    commands::save_notification_channel(&state, channel, target).await
}

#[tauri::command]
pub async fn delete_notification_channel(
    state: Shared<'_>,
    channel_id: String,
) -> Result<crate::notify::NotificationsView> {
    commands::delete_notification_channel(&state, channel_id).await
}

#[tauri::command]
pub async fn test_notification_channel(
    state: Shared<'_>,
    channel_id: String,
) -> Result<crate::notify::Delivery> {
    commands::test_notification_channel(&state, channel_id).await
}

#[tauri::command]
pub async fn rotate_ingest_token(state: Shared<'_>) -> Result<String> {
    commands::rotate_ingest_token(&state).await
}

#[tauri::command]
pub async fn run_prediction(state: Shared<'_>) -> Result<PredictionReport> {
    commands::run_prediction(&state).await
}

#[tauri::command]
pub async fn preview_prediction_evidence(state: Shared<'_>) -> Result<Vec<String>> {
    commands::preview_prediction_evidence(&state).await
}

#[tauri::command]
pub async fn get_prediction_settings(state: Shared<'_>) -> Result<PredictionStatus> {
    commands::get_prediction_settings(&state).await
}

#[tauri::command]
pub async fn save_prediction_settings(state: Shared<'_>, enabled: bool, interval_minutes: u64) -> Result<PredictionStatus> {
    commands::save_prediction_settings(&state, enabled, interval_minutes).await
}

#[tauri::command]
pub async fn get_mcp_settings(state: Shared<'_>) -> Result<McpStatus> {
    commands::get_mcp_settings(&state).await
}

#[tauri::command]
pub async fn save_mcp_settings(state: Shared<'_>, enabled: bool, port: u16) -> Result<McpStatus> {
    commands::save_mcp_settings(&state, enabled, port).await
}

#[tauri::command]
pub async fn rotate_mcp_token(state: Shared<'_>) -> Result<String> {
    commands::rotate_mcp_token(&state).await
}

#[tauri::command]
pub async fn get_change_events(state: Shared<'_>, profile_id: Option<String>, minutes: Option<i64>, limit: Option<usize>) -> Result<Vec<crate::model::ChangeEvent>> {
    commands::get_change_events(&state, profile_id, minutes, limit).await
}

#[tauri::command]
pub async fn get_metric_history(state: Shared<'_>, profile_id: String, minutes: Option<i64>) -> Result<Vec<MetricSeries>> {
    commands::get_metric_history(&state, profile_id, minutes).await
}

#[tauri::command]
pub async fn get_alert_history(
    state: Shared<'_>,
    profile_id: String,
    days: Option<i64>,
) -> Result<Vec<inframonitoring_store::PastAlert>> {
    commands::get_alert_history(&state, profile_id, days).await
}

#[tauri::command]
pub async fn get_flapping(
    state: Shared<'_>,
    days: Option<i64>,
) -> Result<Vec<commands::FlappingCondition>> {
    commands::get_flapping(&state, days).await
}

#[tauri::command]
pub async fn get_change_history(
    state: Shared<'_>,
    profile_id: Option<String>,
    days: Option<i64>,
) -> Result<Vec<crate::store::PastChange>> {
    commands::get_change_history(&state, profile_id, days).await
}

#[tauri::command]
pub async fn get_history_stats(state: Shared<'_>) -> Result<commands::HistoryStats> {
    commands::get_history_stats(&state).await
}

#[tauri::command]
pub async fn get_topology(state: Shared<'_>) -> Result<TopologyView> {
    commands::get_topology(&state).await
}

#[tauri::command]
pub async fn get_team(state: Shared<'_>) -> Result<TeamState> {
    commands::get_team(&state).await
}

#[tauri::command]
pub async fn save_team_server(state: Shared<'_>, server_url: Option<String>) -> Result<TeamState> {
    commands::save_team_server(&state, server_url).await
}

#[tauri::command]
pub async fn sign_in(state: Shared<'_>, email: String, password: String) -> Result<TeamState> {
    commands::sign_in(&state, email, password).await
}

#[tauri::command]
pub async fn sign_out(state: Shared<'_>) -> Result<TeamState> {
    commands::sign_out(&state).await
}

#[tauri::command]
pub async fn get_workspace(state: Shared<'_>) -> Result<team::Workspace> {
    commands::get_workspace(&state).await
}

#[tauri::command]
pub async fn get_incidents(state: Shared<'_>) -> Result<Vec<Incident>> {
    commands::get_incidents(&state).await
}

#[tauri::command]
pub async fn get_health_runs(state: Shared<'_>, minutes: Option<i64>) -> Result<std::collections::HashMap<String, Vec<crate::history::HealthRun>>> {
    commands::get_health_runs(&state, minutes).await
}

#[tauri::command]
pub async fn get_component_history(state: Shared<'_>, profile_id: String, section_id: String, row_id: String, minutes: Option<i64>) -> Result<Vec<MetricSeries>> {
    commands::get_component_history(&state, profile_id, section_id, row_id, minutes).await
}

#[tauri::command]
pub async fn get_config_documents(state: Shared<'_>, profile_id: String) -> Result<Vec<crate::configsource::ConfigDocument>> {
    commands::get_config_documents(&state, profile_id).await
}

#[tauri::command]
pub async fn get_findings(state: Shared<'_>, profile_id: Option<String>) -> Result<Vec<crate::advisor::Finding>> {
    commands::get_findings(&state, profile_id).await
}

#[tauri::command]
pub async fn get_long_history(state: Shared<'_>, profile_id: String, hours: Option<i64>) -> Result<Vec<crate::history::MetricSeries>> {
    commands::get_long_history(&state, profile_id, hours).await
}

#[tauri::command]
pub async fn clear_claims(state: Shared<'_>, profile_id: String) -> Result<()> {
    commands::clear_claims(&state, profile_id).await
}

#[tauri::command]
pub async fn get_runtime_report(state: Shared<'_>) -> Result<RuntimeReport> {
    commands::get_runtime_report(&state).await
}

#[tauri::command]
pub async fn run_scenarios() -> Result<crate::scenarios::SuiteResult> {
    commands::run_scenarios().await
}

#[tauri::command]
pub async fn list_scenarios() -> Result<Vec<crate::scenarios::Scenario>> {
    commands::list_scenarios().await
}

#[tauri::command]
pub async fn run_backtest(state: Shared<'_>, profile_id: String, days: Option<i64>, step_minutes: Option<i64>) -> Result<crate::backtest::Replay> {
    commands::run_backtest(&state, profile_id, days, step_minutes).await
}

#[tauri::command]
pub async fn record_verdict(state: Shared<'_>, verdict: crate::journal::Verdict) -> Result<i64> {
    commands::record_verdict(&state, verdict).await
}

#[tauri::command]
pub async fn record_recall(state: Shared<'_>, recall: crate::journal::Recall) -> Result<i64> {
    commands::record_recall(&state, recall).await
}

#[tauri::command]
pub async fn get_verdicts(state: Shared<'_>, limit: Option<i64>) -> Result<Vec<crate::journal::RecordedVerdict>> {
    commands::get_verdicts(&state, limit).await
}

#[tauri::command]
pub async fn get_judged_incidents(state: Shared<'_>) -> Result<Vec<String>> {
    commands::get_judged_incidents(&state).await
}

#[tauri::command]
pub async fn get_scorecard(state: Shared<'_>) -> Result<crate::journal::Scorecard> {
    commands::get_scorecard(&state).await
}

#[tauri::command]
pub async fn get_capacity_risks(state: Shared<'_>, take: Option<usize>) -> Result<Vec<crate::history::CapacityRisk>> {
    commands::get_capacity_risks(&state, take).await
}

#[tauri::command]
pub async fn get_all_metric_history(state: Shared<'_>, minutes: Option<i64>) -> Result<std::collections::HashMap<String, Vec<MetricSeries>>> {
    commands::get_all_metric_history(&state, minutes).await
}

#[tauri::command]
pub async fn get_service_catalog() -> Result<Vec<ServiceDescriptor>> {
    commands::get_service_catalog().await
}

#[tauri::command]
pub async fn get_config(state: Shared<'_>) -> Result<crate::config::AppConfig> {
    commands::get_config(&state).await
}

#[tauri::command]
pub async fn secret_store_available() -> Result<bool> {
    commands::secret_store_available().await
}

#[tauri::command]
pub async fn save_profile(state: Shared<'_>, profile: ConnectionProfile, secret: Option<String>) -> Result<crate::config::AppConfig> {
    commands::save_profile(&state, profile, secret).await
}

#[tauri::command]
pub async fn delete_profile(state: Shared<'_>, profile_id: String) -> Result<crate::config::AppConfig> {
    commands::delete_profile(&state, profile_id).await
}

#[tauri::command]
pub async fn test_profile(profile: ConnectionProfile, secret: Option<String>) -> Result<HealthSnapshot> {
    commands::test_profile(profile, secret).await
}

#[tauri::command]
pub async fn test_snapshot_write(state: Shared<'_>, profile_id: String) -> Result<String> {
    commands::test_snapshot_write(&state, profile_id).await
}

#[tauri::command]
pub async fn save_llm_settings(state: Shared<'_>, settings: LlmSettings, api_key: Option<String>) -> Result<crate::config::AppConfig> {
    commands::save_llm_settings(&state, settings, api_key).await
}

#[tauri::command]
pub async fn list_models(state: Shared<'_>) -> Result<Vec<String>> {
    commands::list_models(&state).await
}

#[tauri::command]
pub async fn send_message(state: Shared<'_>, conversation_id: String, message: String) -> Result<AgentTurn> {
    commands::send_message(&state, conversation_id, message).await
}

#[tauri::command]
pub async fn explain_component(state: Shared<'_>, profile_id: String, section_id: String, row_id: String) -> Result<crate::state::ComponentReading> {
    commands::explain_component(&state, profile_id, section_id, row_id).await
}

#[tauri::command]
pub async fn explain_window(
    state: Shared<'_>,
    profile_id: String,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Result<crate::state::ComponentReading> {
    commands::explain_window(&state, profile_id, from, to).await
}

#[tauri::command]
pub async fn explain_lines(
    state: Shared<'_>,
    lines: Vec<crate::model::LogEvent>,
) -> Result<crate::state::ComponentReading> {
    commands::explain_lines(&state, lines).await
}

#[tauri::command]
pub async fn explain_alert(state: Shared<'_>, alert_id: String) -> Result<crate::state::ComponentReading> {
    commands::explain_alert(&state, alert_id).await
}

#[tauri::command]
pub async fn explain_incident(state: Shared<'_>, incident_id: String, refresh: Option<bool>) -> Result<crate::rca::RootCause> {
    commands::explain_incident(&state, incident_id, refresh).await
}

#[tauri::command]
pub async fn resolve_confirmation(state: Shared<'_>, call_id: String, approved: bool) -> Result<()> {
    commands::resolve_confirmation(&state, call_id, approved).await
}

#[tauri::command]
pub async fn pending_confirmations(state: Shared<'_>) -> Result<Vec<PendingConfirmation>> {
    commands::pending_confirmations(&state).await
}

#[tauri::command]
pub async fn get_conversation(state: Shared<'_>, conversation_id: String) -> Result<Vec<ChatMessage>> {
    commands::get_conversation(&state, conversation_id).await
}

#[tauri::command]
pub async fn clear_conversation(state: Shared<'_>, conversation_id: String) -> Result<()> {
    commands::clear_conversation(&state, conversation_id).await
}
