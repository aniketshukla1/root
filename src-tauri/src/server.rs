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
    ///
    /// The machine path: a scrape, a script, the bootstrap before any account
    /// exists. Treated as an administrator, because whoever holds it
    /// configured this server.
    pub token: String,
    /// Accounts and sessions, when a database is configured.
    ///
    /// `None` means the shared token is the only way in. Accounts need
    /// somewhere to live, and a server that pretended to have users without a
    /// database would lose them all on restart.
    pub directory: Option<crate::auth::Directory>,
    /// Where the built frontend lives, when this server is also serving it.
    pub web_root: Option<std::path::PathBuf>,
    /// The identity provider, when one is configured.
    pub oidc: Option<crate::oidc::Provider>,
}

/// Build the router. Split from [`serve`] so tests can drive it without a port.
pub fn router(ctx: Ctx) -> Router {
    let web_root = ctx.web_root.clone();

    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/rpc/{command}", post(rpc))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(whoami))
        .route("/auth/users", get(list_users).post(create_user))
        .route("/auth/oidc/start", get(oidc_start))
        .route("/auth/oidc/callback", get(oidc_callback))
        .route("/events", get(events))
        .route("/status", get(status_page))
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
    // Whether anyone has claimed this server yet. A client cannot infer it —
    // an anonymous caller is refused the account list with 401 whether the
    // list is empty or not — and guessing produces a "claim this server" form
    // on a server that already has an administrator, which then fails.
    //
    // It reveals only that the server has been set up, which is the same
    // thing its own sign-in page says to whoever opens it.
    let claimed = match ctx.directory.as_ref() {
        Some(directory) => directory.count().await.unwrap_or(1) > 0,
        // No database, no accounts to claim: the shared token is the way in.
        None => true,
    };

    Json(json!({
        "service": "root",
        "version": env!("CARGO_PKG_VERSION"),
        "commands": crate::rpc::known().len(),
        "history": ctx.state.store.read().await.is_some(),
        "accounts": ctx.directory.is_some(),
        "claimed": claimed,
        // So the client knows whether to draw a single sign-on button at all.
        "sso": ctx.oidc.is_some(),
    }))
}

/// Who is making this request, if anyone.
///
/// Two credentials reach the same place: the shared token, which has no person
/// behind it, and a session token belonging to an account. Both arrive as a
/// bearer, so a client never has to know which kind it holds.
async fn identify(ctx: &Ctx, headers: &HeaderMap) -> Option<crate::auth::Identity> {
    if crate::local_listener::authorized(headers, &ctx.token) {
        return Some(crate::auth::Identity::machine());
    }

    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))?;

    ctx.directory.as_ref()?.identify(bearer).await
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
    let Some(identity) = identify(&ctx, &headers).await else {
        // No detail. A caller without a credential learns that it needs one,
        // and a caller with a wrong one learns nothing about why.
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" })))
            .into_response();
    };

    // Authentication is not authorization. A viewer holds a perfectly valid
    // session and still may not restart a container.
    let log = crate::audit::path(&ctx.state.config_path);
    if !crate::auth::may(identity.role, &command) {
        let why = if crate::auth::ADMIN_ONLY.contains(&command.as_str()) {
            format!("`{command}` is for administrators")
        } else {
            format!("`{command}` changes something, and this account can only read")
        };
        let target = serde_json::from_slice::<Value>(&body).ok().as_ref().and_then(crate::audit::target_of);
        crate::audit::record(&log, &crate::audit::AuditEntry::new(&identity, &command, target, crate::audit::Outcome::Refused, Some(why.clone())));
        return (StatusCode::FORBIDDEN, Json(json!({ "error": why }))).into_response();
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

    // Who wrote it is the session's to say, not the request's.
    let mut args = args;
    if command == "update_incident" {
        if let Some(fields) = args.as_object_mut() {
            fields.insert("author".into(), Value::String(identity.display_name.clone()));
        }
    }

    // A change goes in the audit log as it is answered: by name and target,
    // never its arguments.
    let changes = crate::auth::MUTATING.contains(&command.as_str());
    let target = changes.then(|| crate::audit::target_of(&args)).flatten();
    let result = crate::rpc::dispatch(&ctx.state, &command, args).await;
    if changes {
        let (outcome, detail) = match &result {
            Ok(_) => (crate::audit::Outcome::Done, None),
            Err(e) => (crate::audit::Outcome::Failed, Some(e.to_string())),
        };
        crate::audit::record(&log, &crate::audit::AuditEntry::new(&identity, &command, target, outcome, detail));
    }

    match result {
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

/// The public status page, when someone has turned it on: no session, by
/// design, so it carries only what was chosen for it — and a policy that lets
/// the browser run nothing and fetch nothing, since it has no reason to.
async fn status_page(State(ctx): State<Ctx>) -> Response {
    if !ctx.state.config.read().await.status_page.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    match crate::commands::get_status_page(&ctx.state).await {
        Ok(view) => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (axum::http::header::CACHE_CONTROL, "no-store"),
                (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (axum::http::header::CONTENT_SECURITY_POLICY, "default-src 'none'; style-src 'unsafe-inline'"),
            ],
            crate::incident_log::status_html(&view),
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Sign in and open a session.
///
/// Rate limiting is deliberately absent here and belongs at the proxy: a
/// counter in this process is per-instance, resets on deploy, and gives a
/// false sense of a control somebody else is better placed to apply. What is
/// here instead is a single failure message and a password check that costs
/// the same whether or not the account exists.
async fn login(State(ctx): State<Ctx>, body: axum::body::Bytes) -> Response {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Body {
        email: String,
        password: String,
    }

    let Some(directory) = ctx.directory.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "this server has no database, so it has no accounts. \
                          Use the shared token."
            })),
        )
            .into_response();
    };

    let Ok(body) = serde_json::from_slice::<Body>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "expected an email and a password" })),
        )
            .into_response();
    };

    match directory.sign_in(&body.email, &body.password).await {
        Ok((token, identity)) => (
            StatusCode::OK,
            // The token is returned exactly once and never again. It is not
            // stored anywhere this server can read it back — only its digest.
            Json(json!({ "token": token, "identity": identity })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// End this session.
async fn logout(State(ctx): State<Ctx>, headers: HeaderMap) -> Response {
    if let (Some(directory), Some(bearer)) = (
        ctx.directory.as_ref(),
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ")),
    ) {
        let _ = directory.sign_out(bearer).await;
    }
    // Always 200. Whether the token was real is not something an unauthorized
    // caller should be able to learn by signing out with a guess.
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

/// Who the caller is. The client uses this on load to decide whether to show
/// a sign-in screen or the estate.
async fn whoami(State(ctx): State<Ctx>, headers: HeaderMap) -> Response {
    match identify(&ctx, &headers).await {
        Some(identity) => (
            StatusCode::OK,
            Json(json!({
                "identity": identity,
                // So the client knows whether a sign-in form is worth showing
                // at all on a server with no database behind it.
                "accounts": ctx.directory.is_some(),
            })),
        )
            .into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "accounts": ctx.directory.is_some() })),
        )
            .into_response(),
    }
}

/// Every account. Administrators only.
async fn list_users(State(ctx): State<Ctx>, headers: HeaderMap) -> Response {
    let Some(identity) = identify(&ctx, &headers).await else {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" })))
            .into_response();
    };
    if identity.role != crate::auth::Role::Admin {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "only an administrator can see the account list" })),
        )
            .into_response();
    }
    let Some(directory) = ctx.directory.as_ref() else {
        return (StatusCode::OK, Json(json!([]))).into_response();
    };

    match directory.list().await {
        Ok(accounts) => (StatusCode::OK, Json(json!(accounts))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Create an account.
///
/// Open only while there are none, so a fresh server can be claimed by the
/// first person to reach it; after that it is an administrator's job. The
/// alternative — a password printed into a log on first boot — is a secret
/// sitting in whatever collects that log.
async fn create_user(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Body {
        email: String,
        password: String,
        #[serde(default)]
        display_name: String,
        #[serde(default)]
        role: Option<crate::auth::Role>,
    }

    let Some(directory) = ctx.directory.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "this server has no database, so it has no accounts" })),
        )
            .into_response();
    };

    let caller = identify(&ctx, &headers).await;
    let log = crate::audit::path(&ctx.state.config_path);
    let bootstrapping = directory.count().await.unwrap_or(1) == 0;
    if !bootstrapping {
        let admin = caller.as_ref().is_some_and(|i| i.role == crate::auth::Role::Admin);
        if !admin {
            if let Some(who) = &caller {
                let entry = crate::audit::AuditEntry::new(who, "create_user", None, crate::audit::Outcome::Refused, Some("not an administrator".into()));
                crate::audit::record(&log, &entry);
            }
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "only an administrator can create an account" })),
            )
                .into_response();
        }
    }

    let Ok(body) = serde_json::from_slice::<Body>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "expected an email and a password" })),
        )
            .into_response();
    };

    let name = if body.display_name.trim().is_empty() {
        body.email.clone()
    } else {
        body.display_name.clone()
    };

    match directory.create(&body.email, &body.password, &name, body.role).await {
        Ok(account) => {
            let created = json!(account);
            // The first account has nobody before it to name, so it names itself.
            let who = caller.unwrap_or_else(|| crate::auth::Identity {
                user_id: None,
                email: body.email.clone(),
                display_name: name.clone(),
                role: crate::auth::Role::Admin,
            });
            let role = created["role"].as_str().unwrap_or("?");
            let entry = crate::audit::AuditEntry::new(&who, "create_user", Some(body.email.clone()), crate::audit::Outcome::Done, Some(format!("as {role}")));
            crate::audit::record(&log, &entry);
            (StatusCode::CREATED, Json(created)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Send the browser to the identity provider.
async fn oidc_start(State(ctx): State<Ctx>) -> Response {
    let Some(provider) = ctx.oidc.as_ref() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "single sign-on is not configured on this server" })),
        )
            .into_response();
    };

    let (url, _state) = provider.start().await;
    axum::response::Redirect::temporary(&url).into_response()
}

/// Where the provider sends them back.
///
/// Ends in a redirect to the client with the session in the fragment rather
/// than the query string: a fragment is not sent to the server, does not reach
/// access logs, and does not end up in a `Referer` header on the next request.
/// The client takes it out of the URL and puts it in storage.
async fn oidc_callback(
    State(ctx): State<Ctx>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let fail = |message: String| {
        axum::response::Redirect::temporary(&format!(
            "/?sso_error={}",
            urlencoding::encode(&message)
        ))
        .into_response()
    };

    let (Some(provider), Some(directory)) = (ctx.oidc.as_ref(), ctx.directory.as_ref()) else {
        return fail("single sign-on is not configured on this server".into());
    };

    // The provider's own refusal, which is the only part anyone can act on.
    if let Some(error) = params.get("error") {
        return fail(format!("the provider refused: {error}"));
    }

    let (Some(code), Some(state)) = (params.get("code"), params.get("state")) else {
        return fail("that sign-in came back without a code".into());
    };

    let claims = match provider.finish(code, state).await {
        Ok(claims) => claims,
        Err(e) => return fail(e.to_string()),
    };

    let identity = match directory.upsert_federated(&claims.email, &claims.name).await {
        Ok(identity) => identity,
        Err(e) => return fail(e.to_string()),
    };

    let Some(user_id) = identity.user_id.as_deref() else {
        return fail("that identity has no account".into());
    };
    match directory.open_session(user_id).await {
        Ok(token) => axum::response::Redirect::temporary(&format!("/#token={token}")).into_response(),
        Err(e) => fail(e.to_string()),
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
    let via_header = identify(&ctx, &headers).await.is_some();
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
        // A directory each: the audit log is written beside the config.
        let dir = std::env::temp_dir().join(format!("root-server-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("test dir");
        Ctx {
            state: Arc::new(AppState::new(AppConfig::default(), dir.join("config.json"))),
            events: tx,
            token: token.to_string(),
            directory: None,
            web_root: None,
            oidc: None,
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
    async fn the_status_page_is_not_there_until_someone_turns_it_on() {
        use tower::ServiceExt;
        let ctx = ctx("secret");
        let state = Arc::clone(&ctx.state);
        let get = || axum::http::Request::builder().uri("/status").body(axum::body::Body::empty()).unwrap();

        let off = router(ctx.clone()).oneshot(get()).await.unwrap();
        assert_eq!(off.status(), StatusCode::NOT_FOUND, "off means absent, not an empty page");

        state.config.write().await.status_page =
            crate::incident_log::StatusPageSettings { enabled: true, title: "Acme".into(), profile_ids: Vec::new() };
        let on = router(ctx).oneshot(get()).await.unwrap();
        assert_eq!(on.status(), StatusCode::OK, "public: no token");
        assert_eq!(on.headers()["content-security-policy"], "default-src 'none'; style-src 'unsafe-inline'");
        let bytes = axum::body::to_bytes(on.into_body(), 1 << 20).await.unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("<title>Acme</title>"));
    }

    #[tokio::test]
    async fn a_change_goes_in_the_audit_log_and_a_read_does_not() {
        let ctx = ctx("secret");
        let (status, _) = call(router(ctx.clone()), post_rpc("delete_slo", Some("secret"), json!({ "sloId": "s9" }))).await;
        assert_eq!(status, StatusCode::OK);
        call(router(ctx.clone()), post_rpc("get_dashboard", Some("secret"), json!({}))).await;

        let (status, entries) = call(router(ctx), post_rpc("get_audit_log", Some("secret"), json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        let entries = entries.as_array().expect("entries");
        assert_eq!(entries.len(), 1, "a read is not entered: {entries:?}");
        assert_eq!(entries[0]["command"], "delete_slo");
        assert_eq!(entries[0]["target"], "s9");
        assert_eq!(entries[0]["actor"], "Shared token");
        assert_eq!(entries[0]["outcome"], "done");
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
        assert_eq!(body["commands"], 103);
        // No database, so there are no accounts to claim and the client must
        // not offer to create one.
        assert_eq!(body["accounts"], false);
        assert_eq!(body["claimed"], true);
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
