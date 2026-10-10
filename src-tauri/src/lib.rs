//! root — agentic infrastructure management.
//!
//! Monitoring is half of it. Watching tells you a thing is wrong; reading the
//! configuration that governs it tells you why, and what number to change.
//! `configsource` brings config in, `redact` keeps its secrets from leaving,
//! and `advisor` reads the two against each other.
//!
//! The backend owns everything stateful: it polls each configured connection,
//! keeps the latest health snapshots, a bounded log buffer, and the active
//! alert set, and runs the assistant's tool-calling loop. The UI is a view over
//! that state, driven by Tauri events.

pub mod agent;
pub mod archive;
pub mod changes;
pub mod claims;
pub mod collectors;
pub mod commands;
pub mod config;
pub mod error;
pub mod events;
pub mod forecast;
pub mod advisor;
pub mod auth;
pub mod backtest;
pub mod configsource;
pub mod redact;
pub mod history;
pub mod identify;
pub mod incidents;
pub mod journal;
pub mod influx;
pub mod ingest;
pub mod llm;
pub mod local_listener;
pub mod mcp;
pub mod model;
pub mod notify;
pub mod oidc;
pub mod patterns;
pub mod predict;
pub mod profiles;
pub mod promquery;
pub mod rca;
pub mod registry;
pub mod rpc;
pub mod server;
pub mod runtime;
pub mod scenarios;
pub mod secrets;
pub mod sim;
pub mod state;
pub mod store;
pub mod team;
pub mod topology;
pub mod traces;
pub mod supervisor;
pub mod tauri_cmds;

use std::sync::Arc;

use tauri::Manager;

use crate::config::{config_path, migrate_legacy_config, AppConfig};
use crate::state::AppState;

/// Pick a rustls crypto provider before anything opens a TLS connection.
///
/// Two of our dependencies disagree: kube's `rustls-tls` brings in aws-lc-rs
/// while sqlx's `tls-rustls-ring` brings in ring. With both compiled in, rustls
/// will not guess — it panics on the first handshake with "Could not
/// automatically determine the process-level CryptoProvider". That surfaces at
/// runtime, on whichever TLS call happens first, so it must be settled at
/// startup rather than left to load order.
///
/// Ring is chosen because sqlx already required it. Idempotent: the second call
/// returns Err, which is fine and deliberately ignored.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    install_crypto_provider();
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "inframonitoring=info,warn".to_string()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            std::fs::create_dir_all(&config_dir)?;
            let path = config_path(&config_dir);

            if migrate_legacy_config(&path)? {
                tracing::info!("migrated configuration from the legacy bundle identifier");
            }

            // A corrupt config must not stop the app from starting; fall back
            // to defaults and let the user fix it in Settings.
            let config = AppConfig::load(&path).unwrap_or_else(|e| {
                tracing::error!("could not read {}: {e}. Starting with defaults.", path.display());
                AppConfig::default()
            });

            tracing::info!(
                "loaded {} connection(s) from {}",
                config.profiles.len(),
                path.display()
            );

            let state = Arc::new(AppState::new(config, path));
            app.manage(Arc::clone(&state));

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // The transport, chosen here because this is the desktop app.
                // Until this line every emit is a no-op; after it they reach
                // the webview exactly as they did before `events.rs` existed.
                *state.events.write().await = crate::events::Events::Tauri(handle.clone());

                // Before the pollers, so a verdict recorded in the first
                // seconds of a session has somewhere to land.
                state.open_journal().await;

                // The ingest listener is a no-op unless it was explicitly
                // enabled and a token exists, so this is safe to always call.
                if let Err(e) = ingest::restart(&state).await {
                    tracing::warn!("deploy ingest did not start: {e}");
                }
                // Same contract: nothing listens unless it was switched on and
                // a token exists.
                if let Err(e) = mcp::http::restart(&state).await {
                    tracing::warn!("the MCP server did not start: {e}");
                }

                // Forward-looking risk, on its own timer. Off unless switched
                // on, and a failed sweep is logged rather than fatal: a
                // provider that is briefly unreachable must not take the app
                // with it, and the rules keep working regardless.
                {
                    let state = Arc::clone(&state);
                    tauri::async_runtime::spawn(async move {
                        loop {
                            let settings = state.config.read().await.prediction.clone();
                            let wait = std::time::Duration::from_secs(
                                settings.interval_minutes.clamp(5, 1440) * 60,
                            );
                            // Sleep first: a sweep at boot would run against an
                            // estate that has not been polled yet and read as
                            // "everything unknown".
                            tokio::time::sleep(wait).await;

                            if !state.config.read().await.prediction.enabled {
                                continue;
                            }
                            match predict::sweep(&state).await {
                                Ok(found) if !found.is_empty() => {
                                    tracing::info!("prediction sweep raised {}", found.len());
                                }
                                Ok(_) => {}
                                Err(e) => tracing::warn!("prediction sweep failed: {e}"),
                            }
                        }
                    });
                }
                // Before the pollers start, so the first snapshot of the
                // session already has somewhere to land. A database that
                // cannot be reached is logged and skipped: root has always
                // worked without one and still does.
                let url = state.config.read().await.storage.resolved_url();
                if let Some(url) = url {
                    match crate::store::Store::open(&url).await {
                        Ok(store) => {
                            tracing::info!(
                                timescale = store.has_timescale(),
                                "history is being kept"
                            );
                            *state.store.write().await = Some(store);
                        }
                        Err(e) => tracing::warn!(
                            "history will not be kept in Postgres — the database is not reachable: {e}"
                        ),
                    }
                }
                // No database, or one that would not answer: keep history in
                // the file beside the config instead, and carry on from it.
                if state.store.read().await.is_none() {
                    state.open_local_store().await;
                }

                // Before the pollers, so the first alert of the session is
                // already being watched for. Idle until a channel exists.
                notify::spawn(Arc::clone(&state));

                supervisor::reload_all(state).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            tauri_cmds::get_dashboard,
            tauri_cmds::refresh_connection,
            tauri_cmds::acknowledge_alert,
            tauri_cmds::run_row_action,
            tauri_cmds::query_logs,
            tauri_cmds::clear_logs,
            tauri_cmds::log_histogram,
            tauri_cmds::log_patterns,
            tauri_cmds::correlate,
            tauri_cmds::get_build_console,
            tauri_cmds::get_trace,
            tauri_cmds::get_profile,
            tauri_cmds::get_metric_history,
            tauri_cmds::get_health_runs,
            tauri_cmds::get_topology,
            tauri_cmds::get_alert_history,
            tauri_cmds::get_flapping,
            tauri_cmds::get_change_history,
            tauri_cmds::explain_window,
            tauri_cmds::explain_lines,
            tauri_cmds::explain_alert,
            tauri_cmds::get_history_stats,
            tauri_cmds::get_incidents,
            tauri_cmds::get_team,
            tauri_cmds::save_team_server,
            tauri_cmds::sign_in,
            tauri_cmds::sign_out,
            tauri_cmds::get_workspace,
            tauri_cmds::get_change_events,
            tauri_cmds::get_component_history,
            tauri_cmds::get_capacity_risks,
            tauri_cmds::get_config_documents,
            tauri_cmds::get_findings,
            tauri_cmds::record_verdict,
            tauri_cmds::record_recall,
            tauri_cmds::get_verdicts,
            tauri_cmds::get_judged_incidents,
            tauri_cmds::get_scorecard,
            tauri_cmds::get_long_history,
            tauri_cmds::run_backtest,
            tauri_cmds::clear_claims,
            tauri_cmds::get_runtime_report,
            tauri_cmds::run_scenarios,
            tauri_cmds::list_scenarios,
            tauri_cmds::get_ingest_settings,
            tauri_cmds::save_ingest_settings,
            tauri_cmds::get_notifications,
            tauri_cmds::save_notification_channel,
            tauri_cmds::delete_notification_channel,
            tauri_cmds::test_notification_channel,
            tauri_cmds::get_alert_notices,
            tauri_cmds::get_demo_status,
            tauri_cmds::start_demo,
            tauri_cmds::stop_demo,
            tauri_cmds::trigger_demo_story,
            tauri_cmds::rotate_ingest_token,
            tauri_cmds::get_mcp_settings,
            tauri_cmds::save_mcp_settings,
            tauri_cmds::rotate_mcp_token,
            tauri_cmds::run_prediction,
            tauri_cmds::preview_prediction_evidence,
            tauri_cmds::get_prediction_settings,
            tauri_cmds::save_prediction_settings,
            tauri_cmds::get_all_metric_history,
            tauri_cmds::get_service_catalog,
            tauri_cmds::get_config,
            tauri_cmds::secret_store_available,
            tauri_cmds::save_profile,
            tauri_cmds::delete_profile,
            tauri_cmds::test_profile,
            tauri_cmds::test_snapshot_write,
            tauri_cmds::save_llm_settings,
            tauri_cmds::list_models,
            tauri_cmds::detect_ollama,
            tauri_cmds::save_dashboard,
            tauri_cmds::delete_dashboard,
            tauri_cmds::query_prometheus,
            tauri_cmds::send_message,
            tauri_cmds::explain_component,
            tauri_cmds::explain_incident,
            tauri_cmds::resolve_confirmation,
            tauri_cmds::pending_confirmations,
            tauri_cmds::get_conversation,
            tauri_cmds::clear_conversation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
