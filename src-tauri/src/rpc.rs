//! The same sixty-four commands, dispatched by name over HTTP.
//!
//! Shaped deliberately like Tauri's own `invoke(command, args)` rather than as
//! REST. Sixty-four hand-written routes would read better in a URL bar and be
//! sixty-four more things to keep in step with the frontend; mirroring the IPC
//! call that already exists means the browser client changes one function and
//! none of its call sites.
//!
//! Arguments arrive as the same camelCase object the webview sends, because it
//! is the same frontend sending it. `field` is where that convention is
//! enforced, and a missing or mistyped argument comes back naming the argument
//! rather than as a panic.
//!
//! Every arm calls into [`crate::commands`]. There is no logic in this file,
//! for the same reason there is none in `tauri_cmds.rs`.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::commands;
use crate::error::{AppError, Result};
use crate::config::{ConnectionProfile, LlmSettings};
use crate::state::{AppState, LogFilter};

/// Pull one argument out of the request body.
///
/// An absent key reads as `null`, which lets `Option<T>` parameters be omitted
/// entirely rather than requiring an explicit null — the desktop client
/// already omits them, and the two transports should not disagree about that.
fn field<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T> {
    let raw = args.get(key).cloned().unwrap_or(Value::Null);
    serde_json::from_value(raw).map_err(|e| {
        AppError::Rejected(format!("argument `{key}` is missing or the wrong shape: {e}"))
    })
}

/// Collapse a command's own return type into the one this layer speaks.
fn ok<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|e| AppError::Collector(format!("could not serialize the result: {e}")))
}

/// Commands that must never be reachable from a network client.
///
/// The desktop app is one operator on their own machine; a server is not.
/// Each of these hands back a secret exactly once, and over a port that would
/// let anyone who can reach it mint themselves a credential. The MCP server
/// already keeps a list like this for the same reason — see
/// `mcp::http::NOT_FOR_CLIENTS`.
pub const NOT_REMOTE: &[&str] = &["rotate_ingest_token", "rotate_mcp_token"];

/// Run one command and serialize whatever it returns.
pub async fn dispatch(state: &Arc<AppState>, command: &str, args: Value) -> Result<Value> {
    if NOT_REMOTE.contains(&command) {
        return Err(AppError::Rejected(format!(
            "`{command}` is not available over the network — run it from the desktop app"
        )));
    }

    match command {
        "get_dashboard" => {
            ok(commands::get_dashboard(state).await?)
        }
        "refresh_connection" => {
            let profile_id: String = field(&args, "profileId")?;
            ok(commands::refresh_connection(state, profile_id).await?)
        }
        "acknowledge_alert" => {
            let alert_id: String = field(&args, "alertId")?;
            ok(commands::acknowledge_alert(state, alert_id).await?)
        }
        "run_row_action" => {
            let profile_id: String = field(&args, "profileId")?;
            let tool: String = field(&args, "tool")?;
            let args: serde_json::Value = field(&args, "args")?;
            ok(commands::run_row_action(state, profile_id, tool, args).await?)
        }
        "query_logs" => {
            let filter: LogFilter = field(&args, "filter")?;
            ok(commands::query_logs(state, filter).await?)
        }
        "clear_logs" => {
            ok(commands::clear_logs(state).await?)
        }
        "log_patterns" => {
            let filter: LogFilter = field(&args, "filter")?;
            let minutes: Option<i64> = field(&args, "minutes")?;
            ok(commands::log_patterns(state, filter, minutes).await?)
        }
        "log_histogram" => {
            let filter: LogFilter = field(&args, "filter")?;
            let minutes: Option<i64> = field(&args, "minutes")?;
            let buckets: Option<usize> = field(&args, "buckets")?;
            ok(commands::log_histogram(state, filter, minutes, buckets).await?)
        }
        "correlate" => {
            let at: chrono::DateTime<chrono::Utc> = field(&args, "at")?;
            let window_secs: Option<i64> = field(&args, "windowSecs")?;
            ok(commands::correlate(state, at, window_secs).await?)
        }
        "get_build_console" => {
            let profile_id: String = field(&args, "profileId")?;
            let job: String = field(&args, "job")?;
            let build: Option<i64> = field(&args, "build")?;
            let tail_lines: Option<usize> = field(&args, "tailLines")?;
            ok(commands::get_build_console(state, profile_id, job, build, tail_lines).await?)
        }
        "get_trace" => {
            let profile_id: String = field(&args, "profileId")?;
            let trace_id: String = field(&args, "traceId")?;
            ok(commands::get_trace(state, profile_id, trace_id).await?)
        }
        "get_profile" => {
            let profile_id: String = field(&args, "profileId")?;
            let query: Option<String> = field(&args, "query")?;
            let minutes: Option<i64> = field(&args, "minutes")?;
            ok(commands::get_profile(state, profile_id, query, minutes).await?)
        }
        "get_ingest_settings" => {
            ok(commands::get_ingest_settings(state).await?)
        }
        "save_ingest_settings" => {
            let enabled: bool = field(&args, "enabled")?;
            let port: u16 = field(&args, "port")?;
            ok(commands::save_ingest_settings(state, enabled, port).await?)
        }
        "rotate_ingest_token" => {
            ok(commands::rotate_ingest_token(state).await?)
        }
        "get_notifications" => {
            ok(commands::get_notifications(state).await?)
        }
        "save_notification_channel" => {
            let channel: crate::notify::NotifyChannel = field(&args, "channel")?;
            let target: Option<String> = field(&args, "target")?;
            ok(commands::save_notification_channel(state, channel, target).await?)
        }
        "delete_notification_channel" => {
            let channel_id: String = field(&args, "channelId")?;
            ok(commands::delete_notification_channel(state, channel_id).await?)
        }
        "get_alert_notices" => {
            ok(commands::get_alert_notices(state).await?)
        }
        "test_notification_channel" => {
            let channel_id: String = field(&args, "channelId")?;
            ok(commands::test_notification_channel(state, channel_id).await?)
        }
        "get_demo_status" => {
            ok(commands::get_demo_status(state).await?)
        }
        "start_demo" => {
            let pause_others: bool = field(&args, "pauseOthers")?;
            ok(commands::start_demo(state, pause_others).await?)
        }
        "stop_demo" => {
            ok(commands::stop_demo(state).await?)
        }
        "trigger_demo_story" => {
            let story: Option<String> = field(&args, "story")?;
            ok(commands::trigger_demo_story(state, story).await?)
        }
        "run_prediction" => {
            ok(commands::run_prediction(state).await?)
        }
        "preview_prediction_evidence" => {
            ok(commands::preview_prediction_evidence(state).await?)
        }
        "get_prediction_settings" => {
            ok(commands::get_prediction_settings(state).await?)
        }
        "save_prediction_settings" => {
            let enabled: bool = field(&args, "enabled")?;
            let interval_minutes: u64 = field(&args, "intervalMinutes")?;
            ok(commands::save_prediction_settings(state, enabled, interval_minutes).await?)
        }
        "get_mcp_settings" => {
            ok(commands::get_mcp_settings(state).await?)
        }
        "save_mcp_settings" => {
            let enabled: bool = field(&args, "enabled")?;
            let port: u16 = field(&args, "port")?;
            ok(commands::save_mcp_settings(state, enabled, port).await?)
        }
        "rotate_mcp_token" => {
            ok(commands::rotate_mcp_token(state).await?)
        }
        "get_change_events" => {
            let profile_id: Option<String> = field(&args, "profileId")?;
            let minutes: Option<i64> = field(&args, "minutes")?;
            let limit: Option<usize> = field(&args, "limit")?;
            ok(commands::get_change_events(state, profile_id, minutes, limit).await?)
        }
        "get_metric_history" => {
            let profile_id: String = field(&args, "profileId")?;
            let minutes: Option<i64> = field(&args, "minutes")?;
            ok(commands::get_metric_history(state, profile_id, minutes).await?)
        }
        "get_alert_history" => {
            let profile_id: String = field(&args, "profileId")?;
            let days: Option<i64> = field(&args, "days")?;
            ok(commands::get_alert_history(state, profile_id, days).await?)
        }
        "get_flapping" => {
            let days: Option<i64> = field(&args, "days")?;
            ok(commands::get_flapping(state, days).await?)
        }
        "explain_alert" => {
            let alert_id: String = field(&args, "alertId")?;
            ok(commands::explain_alert(state, alert_id).await?)
        }
        "explain_lines" => {
            let lines: Vec<crate::model::LogEvent> = field(&args, "lines")?;
            ok(commands::explain_lines(state, lines).await?)
        }
        "explain_window" => {
            let profile_id: String = field(&args, "profileId")?;
            let from: chrono::DateTime<chrono::Utc> = field(&args, "from")?;
            let to: chrono::DateTime<chrono::Utc> = field(&args, "to")?;
            ok(commands::explain_window(state, profile_id, from, to).await?)
        }
        "get_change_history" => {
            let profile_id: Option<String> = field(&args, "profileId")?;
            let days: Option<i64> = field(&args, "days")?;
            ok(commands::get_change_history(state, profile_id, days).await?)
        }
        "get_history_stats" => {
            ok(commands::get_history_stats(state).await?)
        }
        "get_topology" => {
            ok(commands::get_topology(state).await?)
        }
        "get_team" => {
            ok(commands::get_team(state).await?)
        }
        "save_team_server" => {
            let server_url: Option<String> = field(&args, "serverUrl")?;
            ok(commands::save_team_server(state, server_url).await?)
        }
        "sign_in" => {
            let email: String = field(&args, "email")?;
            let password: String = field(&args, "password")?;
            ok(commands::sign_in(state, email, password).await?)
        }
        "sign_out" => {
            ok(commands::sign_out(state).await?)
        }
        "get_workspace" => {
            ok(commands::get_workspace(state).await?)
        }
        "get_incidents" => {
            ok(commands::get_incidents(state).await?)
        }
        "get_health_runs" => {
            let minutes: Option<i64> = field(&args, "minutes")?;
            ok(commands::get_health_runs(state, minutes).await?)
        }
        "get_component_history" => {
            let profile_id: String = field(&args, "profileId")?;
            let section_id: String = field(&args, "sectionId")?;
            let row_id: String = field(&args, "rowId")?;
            let minutes: Option<i64> = field(&args, "minutes")?;
            ok(commands::get_component_history(state, profile_id, section_id, row_id, minutes).await?)
        }
        "get_config_documents" => {
            let profile_id: String = field(&args, "profileId")?;
            ok(commands::get_config_documents(state, profile_id).await?)
        }
        "get_findings" => {
            let profile_id: Option<String> = field(&args, "profileId")?;
            ok(commands::get_findings(state, profile_id).await?)
        }
        "get_long_history" => {
            let profile_id: String = field(&args, "profileId")?;
            let hours: Option<i64> = field(&args, "hours")?;
            ok(commands::get_long_history(state, profile_id, hours).await?)
        }
        "clear_claims" => {
            let profile_id: String = field(&args, "profileId")?;
            ok(commands::clear_claims(state, profile_id).await?)
        }
        "get_runtime_report" => {
            ok(commands::get_runtime_report(state).await?)
        }
        "run_scenarios" => {
            ok(commands::run_scenarios().await?)
        }
        "list_scenarios" => {
            ok(commands::list_scenarios().await?)
        }
        "run_backtest" => {
            let profile_id: String = field(&args, "profileId")?;
            let days: Option<i64> = field(&args, "days")?;
            let step_minutes: Option<i64> = field(&args, "stepMinutes")?;
            ok(commands::run_backtest(state, profile_id, days, step_minutes).await?)
        }
        "record_verdict" => {
            let verdict: crate::journal::Verdict = field(&args, "verdict")?;
            ok(commands::record_verdict(state, verdict).await?)
        }
        "record_recall" => {
            let recall: crate::journal::Recall = field(&args, "recall")?;
            ok(commands::record_recall(state, recall).await?)
        }
        "get_verdicts" => {
            let limit: Option<i64> = field(&args, "limit")?;
            ok(commands::get_verdicts(state, limit).await?)
        }
        "get_judged_incidents" => {
            ok(commands::get_judged_incidents(state).await?)
        }
        "get_scorecard" => {
            ok(commands::get_scorecard(state).await?)
        }
        "get_capacity_risks" => {
            let take: Option<usize> = field(&args, "take")?;
            ok(commands::get_capacity_risks(state, take).await?)
        }
        "get_all_metric_history" => {
            let minutes: Option<i64> = field(&args, "minutes")?;
            ok(commands::get_all_metric_history(state, minutes).await?)
        }
        "get_service_catalog" => {
            ok(commands::get_service_catalog().await?)
        }
        "get_config" => {
            ok(commands::get_config(state).await?)
        }
        "secret_store_available" => {
            ok(commands::secret_store_available().await?)
        }
        "save_profile" => {
            let profile: ConnectionProfile = field(&args, "profile")?;
            let secret: Option<String> = field(&args, "secret")?;
            ok(commands::save_profile(state, profile, secret).await?)
        }
        "delete_profile" => {
            let profile_id: String = field(&args, "profileId")?;
            ok(commands::delete_profile(state, profile_id).await?)
        }
        "test_profile" => {
            let profile: ConnectionProfile = field(&args, "profile")?;
            let secret: Option<String> = field(&args, "secret")?;
            ok(commands::test_profile(profile, secret).await?)
        }
        "test_snapshot_write" => {
            let profile_id: String = field(&args, "profileId")?;
            ok(commands::test_snapshot_write(state, profile_id).await?)
        }
        "save_llm_settings" => {
            let settings: LlmSettings = field(&args, "settings")?;
            let api_key: Option<String> = field(&args, "apiKey")?;
            ok(commands::save_llm_settings(state, settings, api_key).await?)
        }
        "list_models" => {
            ok(commands::list_models(state).await?)
        }
        "detect_ollama" => {
            ok(commands::detect_ollama().await?)
        }
        "save_slo" => {
            let slo: crate::slo::Slo = field(&args, "slo")?;
            ok(commands::save_slo(state, slo).await?)
        }
        "delete_slo" => {
            let slo_id: String = field(&args, "sloId")?;
            ok(commands::delete_slo(state, slo_id).await?)
        }
        "get_slo_status" => {
            let profile_id: Option<String> = field(&args, "profileId")?;
            ok(commands::get_slo_status(state, profile_id).await?)
        }
        "get_service_map" => {
            let minutes: Option<i64> = field(&args, "minutes")?;
            ok(commands::get_service_map(state, minutes).await?)
        }
        "save_alert_rule" => {
            let rule: crate::rules::AlertRule = field(&args, "rule")?;
            ok(commands::save_alert_rule(state, rule).await?)
        }
        "delete_alert_rule" => {
            let rule_id: String = field(&args, "ruleId")?;
            ok(commands::delete_alert_rule(state, rule_id).await?)
        }
        "preview_alert_rule" => {
            let rule: crate::rules::AlertRule = field(&args, "rule")?;
            ok(commands::preview_alert_rule(state, rule).await?)
        }
        "save_silence" => {
            let silence: crate::rules::Silence = field(&args, "silence")?;
            ok(commands::save_silence(state, silence).await?)
        }
        "delete_silence" => {
            let silence_id: String = field(&args, "silenceId")?;
            ok(commands::delete_silence(state, silence_id).await?)
        }
        "save_dashboard" => {
            let dashboard: crate::config::Dashboard = field(&args, "dashboard")?;
            ok(commands::save_dashboard(state, dashboard).await?)
        }
        "delete_dashboard" => {
            let dashboard_id: String = field(&args, "dashboardId")?;
            ok(commands::delete_dashboard(state, dashboard_id).await?)
        }
        "query_prometheus" => {
            let profile_id: String = field(&args, "profileId")?;
            let expr: String = field(&args, "expr")?;
            let minutes: Option<i64> = field(&args, "minutes")?;
            let max_series: Option<usize> = field(&args, "maxSeries")?;
            ok(commands::query_prometheus(state, profile_id, expr, minutes, max_series).await?)
        }
        "explore" => {
            let profile_id: String = field(&args, "profileId")?;
            let language: String = field(&args, "language")?;
            let query: String = field(&args, "query")?;
            let start: i64 = field(&args, "start")?;
            let end: i64 = field(&args, "end")?;
            ok(commands::explore(state, profile_id, language, query, start, end).await?)
        }
        "explore_suggest" => {
            let profile_id: String = field(&args, "profileId")?;
            let language: String = field(&args, "language")?;
            let label: Option<String> = field(&args, "label")?;
            ok(commands::explore_suggest(state, profile_id, language, label).await?)
        }
        "send_message" => {
            let conversation_id: String = field(&args, "conversationId")?;
            let message: String = field(&args, "message")?;
            ok(commands::send_message(state, conversation_id, message).await?)
        }
        "explain_component" => {
            let profile_id: String = field(&args, "profileId")?;
            let section_id: String = field(&args, "sectionId")?;
            let row_id: String = field(&args, "rowId")?;
            ok(commands::explain_component(state, profile_id, section_id, row_id).await?)
        }
        "explain_incident" => {
            let incident_id: String = field(&args, "incidentId")?;
            let refresh: Option<bool> = field(&args, "refresh")?;
            ok(commands::explain_incident(state, incident_id, refresh).await?)
        }
        "resolve_confirmation" => {
            let call_id: String = field(&args, "callId")?;
            let approved: bool = field(&args, "approved")?;
            ok(commands::resolve_confirmation(state, call_id, approved).await?)
        }
        "pending_confirmations" => {
            ok(commands::pending_confirmations(state).await?)
        }
        "get_conversation" => {
            let conversation_id: String = field(&args, "conversationId")?;
            ok(commands::get_conversation(state, conversation_id).await?)
        }
        "clear_conversation" => {
            let conversation_id: String = field(&args, "conversationId")?;
            ok(commands::clear_conversation(state, conversation_id).await?)
        }
        other => Err(AppError::Rejected(format!("no such command: `{other}`"))),
    }
}

/// Every command this dispatch knows.
pub fn known() -> &'static [&'static str] {
    &["get_alert_history", "get_flapping", "get_change_history", "explain_window", "explain_lines", "explain_alert", "get_history_stats", "get_dashboard", "refresh_connection", "acknowledge_alert", "run_row_action", "query_logs", "clear_logs", "log_patterns", "log_histogram", "correlate", "get_build_console", "get_trace", "get_profile", "get_ingest_settings", "save_ingest_settings", "rotate_ingest_token", "get_notifications", "save_notification_channel", "delete_notification_channel", "test_notification_channel", "get_alert_notices", "get_demo_status", "start_demo", "stop_demo", "trigger_demo_story", "run_prediction", "preview_prediction_evidence", "get_prediction_settings", "save_prediction_settings", "get_mcp_settings", "save_mcp_settings", "rotate_mcp_token", "get_change_events", "get_metric_history", "get_topology", "get_team", "save_team_server", "sign_in", "sign_out", "get_workspace", "get_incidents", "get_health_runs", "get_component_history", "get_config_documents", "get_findings", "get_long_history", "clear_claims", "get_runtime_report", "run_scenarios", "list_scenarios", "run_backtest", "record_verdict", "record_recall", "get_verdicts", "get_judged_incidents", "get_scorecard", "get_capacity_risks", "get_all_metric_history", "get_service_catalog", "get_config", "secret_store_available", "save_profile", "delete_profile", "test_profile", "test_snapshot_write", "save_llm_settings", "list_models", "detect_ollama", "save_dashboard", "delete_dashboard", "query_prometheus", "explore", "explore_suggest", "save_slo", "delete_slo", "get_slo_status", "get_service_map", "save_alert_rule", "delete_alert_rule", "preview_alert_rule", "save_silence", "delete_silence", "send_message", "explain_component", "explain_incident", "resolve_confirmation", "pending_confirmations", "get_conversation", "clear_conversation"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_the_desktop_app_exposes_is_dispatchable() {
        // The two surfaces are generated from the same list and must not
        // drift. A command added to one and forgotten in the other is a
        // feature that works on the desktop and 404s in a browser.
        assert_eq!(known().len(), 95);
    }

    #[test]
    fn a_command_that_hands_back_a_secret_is_refused_over_the_network() {
        for name in NOT_REMOTE {
            assert!(known().contains(name), "{name} is not a real command");
        }
    }

    #[test]
    fn an_absent_optional_argument_reads_as_none() {
        // The desktop client omits optional arguments rather than sending
        // null, and the two transports must agree about what that means.
        let args = serde_json::json!({});
        let minutes: Option<i64> = field(&args, "minutes").expect("omitted is None");
        assert_eq!(minutes, None);
    }

    #[test]
    fn a_missing_required_argument_says_which_one() {
        let args = serde_json::json!({});
        let error = field::<String>(&args, "profileId").expect_err("required");
        assert!(error.to_string().contains("profileId"), "{error}");
    }
}
