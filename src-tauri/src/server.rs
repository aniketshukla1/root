//! root as a server: the same engine, reachable over a network.
//!
//! The desktop app runs collectors on the machine somebody is sitting at,
//! which was always the weak part. An estate is polled from a laptop behind a
//! VPN, only while the lid is open, and everything the process learns dies
//! when it closes. This runs the identical engine somewhere it can stay up.
//!
//! Nothing about the engine changed to make this possible. The collectors, the
//! supervisor, the advisor, the incident grouping, the forecaster and all
//! sixty-four commands are the same code the desktop app runs — `rpc.rs`
//! dispatches them by name and `events.rs` decides where their output goes.
//! This file is the transport and the door, and it is deliberately the only
//! place in the crate that knows a network exists.
//!
//! ## The door is not the same door
//!
//! `local_listener.rs` binds loopback and refuses anything else, because the
//! ingest and MCP listeners exist for things already on the machine. This one
//! binds an address an operator chooses, which inverts the entire security
//! posture: everything that was true by construction now has to be enforced.
//!
//! So:
//!
//!   - **A token is required, always.** There is no unauthenticated mode and
//!     no default token. A server started without one refuses to start rather
//!     than starting open, because an infrastructure monitor is a map of
//!     everywhere worth attacking and holds the credentials to reach them.
//!   - **Compared in constant time**, reusing `local_listener::authorized` so
//!     there is one implementation of that comparison in the crate.
//!   - **Two commands are refused outright** — see `rpc::NOT_REMOTE`. They
//!     hand back a secret exactly once and no network client has any business
//!     asking.
//!
//! ## Events
//!
//! The browser subscribes to `/events` and receives the same five channels the
//! webview gets over Tauri IPC, in the same shape. A client that stops reading
//! is dropped from the broadcast rather than being waited for: losing an
//! update is recoverable and stalling every collector in the estate is not.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::events::Envelope;
use crate::state::AppState;

/// Events buffered per client before a slow one starts losing the oldest.
///
/// A log pull can deliver a few thousand lines at once and the client renders
/// them in one frame; this only has to absorb the gap between a burst arriving
/// and a browser getting around to it.
const EVENT_BUFFER: usize = 1024;

/// What every handler needs.
#[derive(Clone)]
pub struct Ctx {
    pub state: Arc<AppState>,
    pub events: broadcast::Sender<Envelope>,
    /// The shared bearer token. Never logged, never returned.
    pub token: String,
    /// Where the built frontend lives, when this server is also serving it.
    pub web_root: Option<std::path::PathBuf>,
}

/// Build the router. Split from [`serve`] so tests can drive it without a port.
pub fn router(ctx: Ctx) -> Router {
    let web_root = ctx.web_root.clone();

    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/rpc/{command}", post(rpc))
        .route("/events", get(events))
        .with_state(ctx);

    match web_root {
        // The bundle, with index.html as the fallback so the client's own
        // routing survives a refresh on a deep link.
        Some(root) => {
            let index = root.join("index.html");
            api.fallback_service(
                tower_http::services::ServeDir::new(root)
                    .not_found_service(tower_http::services::ServeFile::new(index)),
            )
        }
        None => api,
    }
}

/// Bind and serve until the process ends.
pub async fn serve(ctx: Ctx, addr: SocketAddr) -> crate::error::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        crate::error::AppError::Config(format!("could not bind {addr}: {e}"))
    })?;

    tracing::info!("root is listening on {addr}");
    axum::serve(listener, router(ctx))
        .await
        .map_err(|e| crate::error::AppError::Config(format!("the server stopped: {e}")))
}

/// Liveness, and the one thing worth knowing before authenticating: whether
/// this is a root server at all, and which commands it speaks.
///
/// Unauthenticated on purpose. It reveals the version and a command count,
/// which a client needs in order to fail usefully against a server that is too
/// old — and nothing about the estate, which is the part worth protecting.
async fn healthz(State(ctx): State<Ctx>) -> Json<Value> {
    Json(json!({
        "service": "root",
        "version": env!("CARGO_PKG_VERSION"),
        "commands": crate::rpc::known().len(),
        "history": ctx.state.store.read().await.is_some(),
    }))
}

/// One command, by name, with its arguments as the body.
async fn rpc(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path(command): Path<String>,
    // Raw bytes, not `Json<Value>`. The JSON extractor rejects a missing or
    // wrong content-type with 415 *before* this function runs, which means an
    // unauthenticated caller who sends a malformed body learns that the
    // endpoint exists and is parsing — and never meets the token check at all.
    // Authentication must not depend on the caller getting the rest of the
    // request right, so the body is parsed below, after the door.
    body: axum::body::Bytes,
) -> Response {
    if !crate::local_listener::authorized(&headers, &ctx.token) {
        // No detail. A caller without a token learns that it needs one, and
        // a caller with a wrong one learns nothing about why.
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" })))
            .into_response();
    }

    let args = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Null) => json!({}),
            Ok(value) => value,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("the request body is not JSON: {e}") })),
                )
                    .into_response()
            }
        }
    };

    match crate::rpc::dispatch(&ctx.state, &command, args).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => {
            // The command's own message, because it is written for an operator
            // and is the only part anyone can act on. The status separates a
            // caller's mistake from the estate's.
            let status = match &e {
                crate::error::AppError::Rejected(_) => StatusCode::BAD_REQUEST,
                crate::error::AppError::UnknownProfile(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// The event stream.
///
/// Authenticated before the upgrade, because a WebSocket that authenticates
/// after handshaking has already accepted the connection.
async fn events(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    // Optional on purpose. Taken as a required extractor it rejects a
    // malformed handshake with 426 *before* this function runs, which means
    // the token is never checked — fine for a broken client and wrong as a
    // posture. Authentication should not depend on the caller getting the
    // rest of the request right.
    upgrade: std::result::Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    // Browsers cannot set headers on a WebSocket handshake, so the token may
    // also arrive as a subprotocol — which is the standard workaround and is
    // why this does not simply reuse the header check alone.
    let via_header = crate::local_listener::authorized(&headers, &ctx.token);
    let via_protocol = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(str::trim)
                .any(|p| p.strip_prefix("root.").is_some_and(|t| t == ctx.token))
        });

    if !via_header && !via_protocol {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    let Ok(upgrade) = upgrade else {
        return (
            StatusCode::UPGRADE_REQUIRED,
            "this endpoint speaks WebSocket",
        )
            .into_response();
    };

    let rx = ctx.events.subscribe();
    if via_protocol {
        upgrade
            .protocols([format!("root.{}", ctx.token)])
            .on_upgrade(move |socket| pump(socket, rx))
    } else {
        upgrade.on_upgrade(move |socket| pump(socket, rx))
    }
}

/// Forward events to one client until it goes away.
async fn pump(mut socket: WebSocket, mut rx: broadcast::Receiver<Envelope>) {
    loop {
        match rx.recv().await {
            Ok(envelope) => {
                let Ok(text) = serde_json::to_string(&envelope) else {
                    continue;
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    return; // client gone
                }
            }
            // The client fell behind and lost events. It is told how many, so
            // it can decide to refetch rather than quietly rendering a gap.
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                let notice = json!({
                    "event": "monitor://lagged",
                    "payload": { "missed": missed }
                });
                if socket
                    .send(Message::Text(notice.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// The buffer size, for the binary that builds the channel.
pub fn event_buffer() -> usize {
    EVENT_BUFFER
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn ctx(token: &str) -> Ctx {
        let (_, tx) = crate::events::Events::broadcast(16);
        Ctx {
            state: Arc::new(AppState::new(
                AppConfig::default(),
                std::path::PathBuf::from("/tmp/root-test-config.json"),
            )),
            events: tx,
            token: token.to_string(),
            web_root: None,
        }
    }

    async fn call(router: Router, req: axum::http::Request<axum::body::Body>) -> (StatusCode, Value) {
        use tower::ServiceExt;
        let response = router.oneshot(req).await.expect("response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("body");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    fn post_rpc(command: &str, token: Option<&str>, body: Value) -> axum::http::Request<axum::body::Body> {
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri(format!("/rpc/{command}"))
            .header("content-type", "application/json");
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        req.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn a_call_without_a_token_is_refused() {
        let (status, body) = call(
            router(ctx("secret")),
            post_rpc("get_service_catalog", None, json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Nothing about why, and nothing about the estate.
        assert_eq!(body["error"], "unauthorized");
    }

    #[tokio::test]
    async fn a_call_with_the_wrong_token_is_refused_the_same_way() {
        let (status, body) = call(
            router(ctx("secret")),
            post_rpc("get_service_catalog", Some("guess"), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");
    }

    #[tokio::test]
    async fn a_call_with_the_token_reaches_the_command() {
        let (status, body) = call(
            router(ctx("secret")),
            post_rpc("get_service_catalog", Some("secret"), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_array(), "the catalog should come back: {body}");
        assert!(!body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_command_that_mints_a_credential_is_refused_even_with_the_token() {
        // Authentication is not authorization. Whoever holds the shared token
        // still may not ask the server to hand out another one.
        let (status, body) = call(
            router(ctx("secret")),
            post_rpc("rotate_ingest_token", Some("secret"), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"].as_str().unwrap().contains("not available over the network"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_unknown_command_says_so_rather_than_panicking() {
        let (status, body) = call(
            router(ctx("secret")),
            post_rpc("drop_everything", Some("secret"), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("no such command"), "{body}");
    }

    #[tokio::test]
    async fn a_missing_argument_names_itself() {
        let (status, body) = call(
            router(ctx("secret")),
            post_rpc("refresh_connection", Some("secret"), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("profileId"), "{body}");
    }

    #[tokio::test]
    async fn a_call_with_no_content_type_is_still_refused_for_the_right_reason() {
        // Regression: taken as `Json<Value>`, the body extractor rejected this
        // with 415 before the token was ever checked. An anonymous caller
        // should meet the door first, whatever they send.
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/rpc/get_service_catalog")
            .body(axum::body::Body::from("not json at all"))
            .unwrap();
        let (status, body) = call(router(ctx("secret")), req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");
    }

    #[tokio::test]
    async fn an_authenticated_call_with_a_broken_body_says_what_is_wrong() {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/rpc/get_service_catalog")
            .header("authorization", "Bearer secret")
            .body(axum::body::Body::from("{oh dear"))
            .unwrap();
        let (status, body) = call(router(ctx("secret")), req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("not JSON"), "{body}");
    }

    #[tokio::test]
    async fn an_empty_body_means_no_arguments() {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/rpc/get_service_catalog")
            .header("authorization", "Bearer secret")
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, _) = call(router(ctx("secret")), req).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn health_answers_without_a_token_and_says_nothing_about_the_estate() {
        let req = axum::http::Request::builder()
            .uri("/healthz")
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, body) = call(router(ctx("secret")), req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["service"], "root");
        assert_eq!(body["commands"], 64);
        // No connection names, no alerts, no token.
        assert!(body.get("profiles").is_none());
        assert!(!body.to_string().contains("secret"));
    }

    fn ws_request(token: Option<&str>) -> axum::http::Request<axum::body::Body> {
        let mut req = axum::http::Request::builder()
            .uri("/events")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        req.body(axum::body::Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn the_event_stream_refuses_an_unauthenticated_upgrade() {
        let (status, _) = call(router(ctx("secret")), ws_request(None)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a well-formed handshake with no token must be refused for that reason"
        );
    }

    #[tokio::test]
    async fn the_event_stream_refuses_a_wrong_token() {
        let (status, _) = call(router(ctx("secret")), ws_request(Some("guess"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // The success path — a token accepted and a socket upgraded — is not
    // testable here. `oneshot` drives the router without hyper, so the
    // `OnUpgrade` extension a real connection carries is absent and the
    // extractor rejects every handshake regardless of the token. It is
    // covered over a real port in `tests/server_http.rs`, which is where it
    // belongs anyway: an upgrade that works against a router and not against
    // a socket has tested nothing.

    #[tokio::test]
    async fn a_plain_get_to_the_event_stream_is_told_what_it_speaks() {
        // Authenticated, but not a WebSocket request. The token check ran
        // first, which is the property the optional extractor exists for.
        let req = axum::http::Request::builder()
            .uri("/events")
            .header("authorization", "Bearer secret")
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, _) = call(router(ctx("secret")), req).await;
        assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
    }
}
