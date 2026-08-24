//! InfraMonitor — agentic infrastructure monitoring.
//!
//! The backend owns everything stateful: it polls each configured connection,
//! keeps the latest health snapshots, a bounded log buffer, and the active
//! alert set, and runs the assistant's tool-calling loop. The UI is a view over
//! that state, driven by Tauri events.

pub mod agent;
pub mod changes;
pub mod collectors;
pub mod commands;
pub mod config;
pub mod error;
pub mod history;
pub mod ingest;
pub mod llm;
pub mod model;
pub mod patterns;
pub mod registry;
pub mod secrets;
pub mod state;
pub mod supervisor;

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
                // The ingest listener is a no-op unless it was explicitly
                // enabled and a token exists, so this is safe to always call.
                if let Err(e) = ingest::restart(&state, &handle).await {
                    tracing::warn!("deploy ingest did not start: {e}");
                }
                supervisor::reload_all(handle, state).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_dashboard,
            commands::refresh_connection,
            commands::acknowledge_alert,
            commands::run_row_action,
            commands::query_logs,
            commands::clear_logs,
            commands::log_histogram,
            commands::log_patterns,
            commands::correlate,
            commands::get_build_console,
            commands::get_metric_history,
            commands::get_change_events,
            commands::get_component_history,
            commands::get_ingest_settings,
            commands::save_ingest_settings,
            commands::rotate_ingest_token,
            commands::get_all_metric_history,
            commands::get_service_catalog,
            commands::get_config,
            commands::secret_store_available,
            commands::save_profile,
            commands::delete_profile,
            commands::test_profile,
            commands::save_llm_settings,
            commands::list_models,
            commands::send_message,
            commands::resolve_confirmation,
            commands::pending_confirmations,
            commands::get_conversation,
            commands::clear_conversation,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
