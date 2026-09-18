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
//! - **Read-only, and narrower than that.** The advertised set is the
//!   read-only catalog minus [`NOT_FOR_CLIENTS`]. `mutating` alone was not
//!   enough: it means "changes infrastructure", which correctly gates the
//!   operator's own assistant, and leaves through two tools that still have
//!   effects — one writes the findings record, the other polls production on
//!   demand. Both are withheld here.
//! - **Origin is checked**, not only the token, because the token is the part
//!   most likely to leak.
//!
//! One rule is this module's own, and it is the one worth stating plainly:
//! **an MCP client is not the operator.** It is a program the operator pointed
//! at root, so it gets strictly less than the operator does — it cannot restart
//! a container, trigger a build or terminate a query whatever the app's agent
//! mode says, and it cannot write a finding or force a poll either.
//!
//! # Transport
//!
//! Streamable HTTP, in its simplest valid form: `POST /mcp` carrying one
//! JSON-RPC message, answered with one JSON response. A notification is
//! answered `202 Accepted` with no body, as the specification requires. `GET`
//! returns `405`, which is how a server says it offers no server-initiated
//! stream — there is nothing to push, since the tool list never changes while
//! the process runs.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::{advertise, ToolBackend};
use crate::local_listener::{authorized, origin_allowed};
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

/// Read-only tools that are nonetheless not safe to hand to a program.
///
/// `ToolDef::mutating` means "changes infrastructure", which is the right
/// question for the operator's own assistant — it decides what stops at a
/// confirmation dialog. It is the wrong question here, and reusing it was a
/// mistake: two tools clear that bar and still have effects nobody asked a
/// network caller to be able to cause.
///
/// `report_finding` writes to the claims store, and those claims render in the
/// Config tab as findings and can be rated into `journal.rs`, which is the one
/// thing here that persists to disk. A client that can write findings can
/// forge the record root is judged by.
///
/// `refresh_connection` polls a connection immediately. That is an outbound
/// request to production infrastructure using root's credentials, at whatever
/// rate the caller chooses, ignoring the configured interval.
///
/// Neither is advertised and neither will dispatch. The operator's assistant
/// still has both; it is reached through a UI the operator is sitting in front
/// of, which is the whole difference.
const NOT_FOR_CLIENTS: &[&str] = &["report_finding", "refresh_connection"];

/// The advertised tool list, built once.
///
/// `catalog()` allocates a `ToolDef` and a nested `json!` schema for every tool
/// each time it is called, and this is answered on every `tools/list`. The set
/// cannot change while the process runs, so it is built on first use and shared
/// after.
static ADVERTISED: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();

/// The names `call` will accept, for the same reason.
static CALLABLE: std::sync::OnceLock<std::collections::HashSet<String>> =
    std::sync::OnceLock::new();

fn advertised() -> &'static Vec<Value> {
    ADVERTISED.get_or_init(|| {
        let allowed = crate::agent::tools::catalog(AgentMode::ReadOnly)
            .into_iter()
            .filter(|t| !NOT_FOR_CLIENTS.contains(&t.spec.name.as_str()));
        advertise(allowed, "")
    })
}

/// The names this server actually serves, for Settings to show.
///
/// Derived from the same list `tools()` advertises, so the panel cannot
/// promise a tool the server withholds.
pub fn served_tool_names() -> Vec<String> {
    let mut names: Vec<String> = callable().iter().cloned().collect();
    names.sort();
    names
}

fn callable() -> &'static std::collections::HashSet<String> {
    CALLABLE.get_or_init(|| {
        advertised()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect()
    })
}

#[async_trait]
impl ToolBackend for AppBackend {
    /// Every read-only tool that is safe to expose, under root's own names.
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
        advertised().clone()
    }

    async fn call(&self, name: &str, args: &Value) -> Result<String> {
        // The gate is the advertised set, not the catalog: a tool that is
        // mutating, or that is read-only but carries an effect (see
        // `NOT_FOR_CLIENTS`), is absent from it, so naming one directly is
        // refused rather than run.
        if !callable().contains(name) {
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

async fn post_rpc(State(ctx): State<Ctx>, headers: HeaderMap, body: String) -> AxumResponse {
    if !origin_allowed(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "cross-origin requests are refused" })),
        )
            .into_response();
    }

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

    // Syntax is this transport's own to report, because it answers with a 400
    // where stdio answers with a JSON-RPC object. Everything past the parse —
    // which error code a well-formed non-request earns, how a batch is
    // answered — is shared, so the two transports cannot drift apart.
    let parsed = match serde_json::from_str::<Value>(&body) {
        Ok(parsed) => parsed,
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

    match super::handle_json(ctx.backend.as_ref(), parsed).await {
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

    crate::local_listener::bind(router, port, "the MCP server").await
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
