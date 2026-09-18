//! The in-app MCP server: everything root knows, over loopback HTTP.
//!
//! [`super::standalone`] is a child process that can reach a gigapipe from a
//! connection profile. This is root itself, so it can reach the rest —
//! `get_health` off the latest snapshots, `search_logs` across the ring buffer,
//! `list_alerts` off the live alert set, `config_findings` off the rules. That
//! state only exists while the app is polling, which is why this server lives
//! here and the standalone one does not serve these tools at all.
//!
//! # Security posture
//!
//! This opens a socket in a desktop app holding infrastructure credentials, and
//! hands whatever answers to another program. `ingest.rs` sets out the rules
//! for that; this follows the same ones, for the same reasons:
//!
//! - **Disabled by default.** Enabling it is an explicit act in Settings.
//! - **Loopback only.** The listener binds `127.0.0.1`, never `0.0.0.0`, so
//!   nothing off the machine can reach it regardless of firewall state.
//! - **Bearer token required**, generated on first enable, stored in the OS
//!   keychain, and compared in constant time.
//! - **Body capped** well below anything a JSON-RPC call needs.
//! - **Read-only.** [`super::ToolBackend::tools`] filters on root's `mutating`
//!   flag, so the tools that stop at a confirmation dialog are never
//!   advertised and never dispatched. There is no dialog on an HTTP request,
//!   and a weaker approval is worse than none.
//!
//! One rule is this module's own, and it is the one worth stating plainly:
//! **an MCP client is not the operator.** It is a program the operator pointed
//! at root, and it gets exactly the read-only surface — no ability to restart a
//! container, trigger a build, or terminate a query, whatever the app's own
//! agent mode is set to.
//!
//! # Transport
//!
//! Streamable HTTP, in its simplest valid form: `POST /mcp` carrying one
//! JSON-RPC message, answered with one JSON response. A notification is
//! answered `202 Accepted` with no body, as the specification requires. `GET`
//! returns `405`, which is how a server says it offers no server-initiated
//! stream — there is nothing to push, since the tool list never changes while
//! the process runs.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use super::{advertise, handle, Request, ToolBackend};
use crate::config::AgentMode;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Keychain entry holding this listener's bearer token.
///
/// Distinct from the ingest token on purpose: the two grant different things,
/// and rotating one should not silently rotate the other.
pub const TOKEN_KEY: &str = "mcp::token";

/// A JSON-RPC call is a name and a few short arguments. Anything approaching
/// this is not one.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// The path a client points at.
pub const ENDPOINT: &str = "/mcp";

/// Root's full read-only tool surface, backed by the running app.
pub struct AppBackend {
    state: Arc<AppState>,
}

impl AppBackend {
    pub fn new(state: Arc<AppState>) -> Self {
        AppBackend { state }
    }
}

#[async_trait]
impl ToolBackend for AppBackend {
    /// Every read-only tool, under root's own names.
    ///
    /// No prefix is stripped here, unlike the standalone server: this serves
    /// `get_health` and `search_logs` beside the gigapipe reads, and
    /// `gigapipe_prometheus_query` says which of the two Prometheus-shaped
    /// things a call meant. That ambiguity is exactly what the prefix exists
    /// to remove.
    ///
    /// The catalog is asked for `ReadOnly` regardless of how the app's own
    /// agent mode is set, so turning on "Allow actions" for the operator's
    /// assistant never widens what a connected program can do.
    fn tools(&self) -> Vec<Value> {
        advertise(crate::agent::tools::catalog(AgentMode::ReadOnly), "")
    }

    async fn call(&self, name: &str, args: &Value) -> Result<String> {
        // Asking the read-only catalog is the gate: a mutating tool is not in
        // it, so a client naming one is told it does not exist rather than
        // having it run.
        if crate::agent::tools::find(AgentMode::ReadOnly, name).is_none() {
            return Err(AppError::Collector(format!(
                "no read-only tool called `{name}`"
            )));
        }
        crate::agent::tools::run_read_only(&self.state, name, args).await
    }

    fn instructions(&self) -> &'static str {
        "Reads a running root: what infrastructure is configured, its current health \
         and metrics, the shared log buffer, active alerts, configuration findings, and \
         a gigapipe warehouse if one is connected. Start with list_connections. \
         Everything here is read-only."
    }
}

#[derive(Clone)]
struct Ctx {
    backend: Arc<AppBackend>,
    token: String,
}

/// Constant-time bearer check. A plain `==` on a secret leaks its prefix to
/// anyone who can time the response.
fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(presented) = value.strip_prefix("Bearer ") else {
        return false;
    };
    presented.trim().as_bytes().ct_eq(token.as_bytes()).into()
}

/// Unauthenticated liveness, so a client can check reachability before it has
/// a token. Says nothing about the estate.
async fn health() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "server": "root", "transport": "streamable-http" })),
    )
}

/// A server offering no server-initiated stream says so with 405, rather than
/// leaving a client waiting on an SSE connection that will never carry
/// anything.
async fn no_stream() -> AxumResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "error": "this server pushes nothing; POST JSON-RPC to this endpoint instead"
        })),
    )
        .into_response()
}

async fn post_rpc(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    body: String,
) -> AxumResponse {
    if !authorized(&headers, &ctx.token) {
        // A bare 401 with the scheme named, and nothing about why — a client
        // that guessed wrong learns only that it guessed wrong.
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({ "error": "a bearer token is required" })),
        )
            .into_response();
    }

    let request = match serde_json::from_str::<Request>(&body) {
        Ok(request) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": super::PARSE_ERROR, "message": error.to_string() },
                })),
            )
                .into_response()
        }
    };

    match handle(ctx.backend.as_ref(), request).await {
        Some(response) => (StatusCode::OK, Json(response)).into_response(),
        // A notification is accepted and not answered. 202 with an empty body
        // is what the specification asks for, and a client that sent one is
        // not waiting for anything.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Bring the listener down, then back up if it is enabled.
///
/// Called on boot and whenever the settings or the token change, so the port is
/// genuinely closed when the feature is switched off rather than merely hidden.
pub async fn restart(state: &Arc<AppState>) -> Result<()> {
    if let Some(handle) = state.mcp.lock().await.take() {
        handle.abort();
    }

    let settings = state.config.read().await.mcp.clone();
    if !settings.enabled {
        return Ok(());
    }

    let Some(token) = crate::secrets::get_secret(TOKEN_KEY).await? else {
        // No token means nothing could authenticate anyway; refusing to listen
        // is safer than listening with a check nobody can pass.
        tracing::warn!("the MCP server is enabled but no token is stored; not listening");
        return Ok(());
    };

    let handle = serve(Arc::clone(state), settings.port, token).await?;
    *state.mcp.lock().await = Some(handle);
    Ok(())
}

/// Bring the listener up on loopback. Returns a handle that stops it.
pub async fn serve(
    state: Arc<AppState>,
    port: u16,
    token: String,
) -> Result<tokio::task::JoinHandle<()>> {
    let ctx = Ctx {
        backend: Arc::new(AppBackend::new(state)),
        token,
    };

    let router = Router::new()
        .route(ENDPOINT, post(post_rpc).get(no_stream))
        .route("/health", get(health))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(ctx);

    // Loopback, explicitly. Never `0.0.0.0` — see the module header.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        AppError::Rejected(format!("could not bind the MCP listener on {addr}: {e}"))
    })?;

    tracing::info!("MCP server listening on http://{addr}{ENDPOINT}");

    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::warn!("MCP listener stopped: {e}");
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_requires_the_exact_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer secret".parse().unwrap(),
        );
        assert!(authorized(&headers, "secret"));
        assert!(!authorized(&headers, "secre"));
        assert!(!authorized(&headers, "secrets"));
        assert!(!authorized(&HeaderMap::new(), "secret"));
    }

    #[test]
    fn bearer_rejects_other_schemes() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Basic c2VjcmV0".parse().unwrap(),
        );
        assert!(!authorized(&headers, "secret"));
    }

    /// The whole security argument of this module in one assertion: no tool
    /// that changes anything is ever advertised, whatever mode the app's own
    /// assistant is in.
    #[test]
    fn the_advertised_surface_is_read_only_in_both_agent_modes() {
        for mode in [AgentMode::ReadOnly, AgentMode::ConfirmActions] {
            let served = advertise(crate::agent::tools::catalog(AgentMode::ReadOnly), "");
            let names: Vec<&str> = served
                .iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect();

            for forbidden in [
                "restart_container",
                "start_container",
                "stop_container",
                "trigger_jenkins_build",
                "terminate_database_query",
            ] {
                assert!(
                    !names.contains(&forbidden),
                    "{forbidden} served in {mode:?}"
                );
            }
            // And the read-only surface is genuinely there, so the filter is
            // not passing by serving nothing at all.
            assert!(names.contains(&"list_connections"));
            assert!(names.contains(&"get_health"));
            assert!(names.contains(&"search_logs"));
        }
    }

    /// Unlike the standalone server, this one keeps root's own names — it
    /// serves both kinds of Prometheus and the prefix is what tells them apart.
    #[test]
    fn gigapipe_tools_keep_their_prefix_here() {
        let served = advertise(crate::agent::tools::catalog(AgentMode::ReadOnly), "");
        let names: Vec<&str> = served
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"gigapipe_prometheus_query"));
        assert!(!names.contains(&"prometheus_query"));
    }

    #[test]
    fn every_advertised_tool_carries_a_usable_schema() {
        for tool in advertise(crate::agent::tools::catalog(AgentMode::ReadOnly), "") {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool:?}");
            assert!(tool["inputSchema"]["properties"].is_object(), "{tool:?}");
            assert!(
                !tool["description"].as_str().unwrap_or_default().is_empty(),
                "{tool:?}"
            );
        }
    }
}
