//! `root-server` — the engine, without a window.
//!
//! Same collectors, same rules, same sixty-four commands as the desktop app.
//! What it does not have is a webview, a keychain, or a person sitting in
//! front of it, and each of those absences is a decision this file has to make
//! explicitly rather than inherit.
//!
//! ## Configuration is the environment
//!
//! A container is configured by its environment, not by a file somebody edits
//! inside it. Everything here reads an env var first; `ROOT_CONFIG` still
//! points at the same `config.json` the desktop app writes, so an operator can
//! develop against the app and deploy the same connections.
//!
//! ```text
//! ROOT_TOKEN          required. The shared bearer token.
//! ROOT_ADDR           default 0.0.0.0:7777
//! ROOT_CONFIG         default ./config.json
//! ROOT_DATABASE_URL   optional. Without it, history is not kept.
//! ROOT_WEB_ROOT       optional. Serves the built frontend from here.
//! ```
//!
//! ## It refuses to start rather than start open
//!
//! No token, no server. Not a warning, not a generated default printed to a
//! log nobody reads — a non-zero exit. This process holds the credentials to
//! every system it watches and a map of which ones are already failing, which
//! makes an accidentally-public instance worse than no monitoring at all.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use inframonitoring_lib::{config, events, server, state::AppState, store, supervisor};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            tracing::error!("{message}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let token = std::env::var("ROOT_TOKEN").unwrap_or_default();
    if token.trim().len() < 16 {
        return Err(
            "ROOT_TOKEN is required and must be at least 16 characters. This process holds \
             the credentials for everything it watches; it will not start without one. \
             Generate one with: openssl rand -hex 32"
                .to_string(),
        );
    }

    let addr: SocketAddr = std::env::var("ROOT_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:7777".to_string())
        .parse()
        .map_err(|e| format!("ROOT_ADDR is not an address: {e}"))?;

    let config_path = PathBuf::from(
        std::env::var("ROOT_CONFIG").unwrap_or_else(|_| "config.json".to_string()),
    );
    let loaded = config::AppConfig::load(&config_path).unwrap_or_else(|e| {
        // A missing file is an empty estate, not a failure: a fresh container
        // has nothing configured yet and should come up so somebody can
        // configure it.
        tracing::warn!("starting with no connections ({e})");
        config::AppConfig::default()
    });
    tracing::info!(
        "loaded {} connection(s) from {}",
        loaded.profiles.len(),
        config_path.display()
    );

    let state = Arc::new(AppState::new(loaded, config_path));

    // The transport, before anything can emit.
    let (emitter, sender) = events::Events::broadcast(server::event_buffer());
    *state.events.write().await = emitter;

    state.open_journal().await;

    if let Some(url) = state.config.read().await.storage.resolved_url() {
        match store::Store::open(&url).await {
            Ok(opened) => {
                tracing::info!(timescale = opened.has_timescale(), "history is being kept");
                *state.store.write().await = Some(opened);
            }
            // The same posture as everywhere else: a database that is not
            // reachable costs the history, not the monitoring.
            Err(e) => tracing::warn!("history will not be kept: {e}"),
        }
    } else {
        tracing::warn!(
            "ROOT_DATABASE_URL is not set — this session's history will be lost when it stops"
        );
    }

    // Accounts live in the same database as the history. Without one the
    // shared token is the only way in, which the client is told rather than
    // left to discover at a sign-in form that cannot work.
    let directory = match state.config.read().await.storage.resolved_url() {
        Some(url) => match sqlx::PgPool::connect(&url).await {
            Ok(pool) => match inframonitoring_lib::auth::Directory::open(pool).await {
                Ok(directory) => {
                    let swept = directory.sweep().await.unwrap_or(0);
                    let count = directory.count().await.unwrap_or(0);
                    if count == 0 {
                        tracing::warn!(
                            "no accounts yet — the first POST to /auth/users claims this \
                             server as an administrator"
                        );
                    } else {
                        tracing::info!(accounts = count, expired_sessions_removed = swept, "accounts ready");
                    }
                    Some(directory)
                }
                Err(e) => {
                    tracing::warn!("accounts are unavailable: {e}");
                    None
                }
            },
            Err(e) => {
                tracing::warn!("accounts are unavailable: {e}");
                None
            }
        },
        None => {
            tracing::warn!("no database — the shared token is the only way in");
            None
        }
    };

    // Single sign-on, when it is fully configured. Discovery happens now
    // rather than on the first click, so a wrong issuer is a line in the log
    // at startup instead of a failure halfway through somebody's sign-in.
    let oidc = match state.config.read().await.oidc.resolved() {
        Some(settings) => match inframonitoring_lib::oidc::Provider::discover(settings).await {
            Ok(provider) => {
                tracing::info!("single sign-on is ready");
                Some(provider)
            }
            Err(e) => {
                tracing::warn!("single sign-on is configured but unusable: {e}");
                None
            }
        },
        None => None,
    };
    if oidc.is_some() && directory.is_none() {
        // The provider says who somebody is; the account they become lives in
        // the database. Without one there is nowhere to put them.
        tracing::warn!("single sign-on needs a database for the accounts it creates");
    }

    supervisor::reload_all(Arc::clone(&state)).await;

    let web_root = std::env::var("ROOT_WEB_ROOT").ok().map(PathBuf::from);
    if let Some(root) = &web_root {
        tracing::info!("serving the client from {}", root.display());
    }

    server::serve(
        server::Ctx {
            state,
            events: sender,
            token,
            directory,
            web_root,
            oidc,
        },
        addr,
    )
    .await
    .map_err(|e| e.to_string())
}
