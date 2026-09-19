//! Local deploy ingest.
//!
//! A pipeline that already knows it deployed something should be able to say
//! so, rather than waiting for the change engine to notice a container image
//! move on the next poll. This is that path.
//!
//! # Security posture
//!
//! This opens a socket in a desktop app that holds infrastructure credentials,
//! so the defaults are deliberately narrow:
//!
//! - **Disabled by default.** Enabling it is an explicit act in Settings.
//! - **Loopback only.** The listener binds `127.0.0.1`, never `0.0.0.0`, so
//!   nothing off the machine can reach it regardless of firewall state.
//! - **Bearer token required**, generated on first enable, stored in the OS
//!   keychain, and compared in constant time.
//! - **Body capped** well below anything a deploy record needs.
//! - **Every field is data.** Strings are trimmed, length-clamped and stored;
//!   nothing posted here is ever interpreted as an instruction, and the
//!   timestamp is clamped to a sane window so a bad clock cannot smear the
//!   timeline.
//!
//! Because it is loopback-only, a cloud CI runner cannot reach it directly —
//! the poster has to be on the machine (a deploy script, a git hook, a
//! self-hosted runner) or arrive through a tunnel the operator has set up.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use crate::events::Events;

use crate::error::{AppError, Result};
use crate::local_listener::{authorized, origin_allowed};
use crate::model::{ChangeEvent, ChangeKind, ChangeSource};
use crate::state::AppState;
use crate::supervisor::EVENT_CHANGES;

/// Keychain key for the bearer token.
pub const TOKEN_KEY: &str = "ingest::token";

/// Generous for a deploy record, tiny for an attacker.
const MAX_BODY_BYTES: usize = 8 * 1024;

/// Longest any single field may be once trimmed.
const MAX_FIELD: usize = 200;
const MAX_SUMMARY: usize = 500;

/// How far from now a reported timestamp may sit before it is pulled back. A
/// pipeline with a skewed clock should not be able to place a marker in next
/// week or last year.
const MAX_CLOCK_SKEW_HOURS: i64 = 24;

/// What a pipeline POSTs. Every field beyond `service`, `version` and
/// `connection` is optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeployPayload {
    /// What was deployed — a container, a job, an app name.
    pub service: String,
    /// The version now running.
    pub version: String,
    /// Which configured connection this belongs to, by id or name.
    pub connection: String,
    /// The version it replaced, when the pipeline knows it.
    #[serde(default)]
    pub previous_version: Option<String>,
    /// When it happened. Defaults to now.
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
    /// Free-text note shown in the marker's tooltip.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptedBody {
    accepted: bool,
    id: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

fn clamp(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect()
}

/// Turn a validated payload into a change event bound to a real connection.
///
/// Returns `Err` when the named connection does not exist — a marker on the
/// timeline has to belong to something, and silently inventing a connection
/// would make the timeline untrustworthy.
pub async fn event_from_payload(
    state: &Arc<AppState>,
    payload: DeployPayload,
) -> Result<ChangeEvent> {
    let service = clamp(&payload.service, MAX_FIELD);
    let version = clamp(&payload.version, MAX_FIELD);
    let connection = clamp(&payload.connection, MAX_FIELD);

    if service.is_empty() || version.is_empty() || connection.is_empty() {
        return Err(AppError::Rejected(
            "`service`, `version` and `connection` must all be non-empty".into(),
        ));
    }

    // Matches by id first, then case-insensitive name, so a pipeline can post
    // the human name it already knows. An unknown connection is rejected
    // rather than invented: every marker must belong to something real.
    let config = state.config.read().await;
    let profile = config.profile_by_ref(&connection)?;
    let (profile_id, profile_name, kind) =
        (profile.id.clone(), profile.name.clone(), profile.kind);
    drop(config);

    let now = Utc::now();
    let at = match payload.at {
        Some(t) => {
            let skew = Duration::hours(MAX_CLOCK_SKEW_HOURS);
            t.clamp(now - skew, now)
        }
        None => now,
    };

    let previous = payload
        .previous_version
        .map(|v| clamp(&v, MAX_FIELD))
        .filter(|v| !v.is_empty());

    let summary = match payload.note.map(|n| clamp(&n, MAX_SUMMARY)) {
        Some(note) if !note.is_empty() => note,
        _ => match &previous {
            Some(from) => format!("{service}: {from} → {version}"),
            None => format!("{service} deployed at {version}"),
        },
    };

    Ok(ChangeEvent {
        // Deterministic on the reported facts, so a pipeline that retries its
        // POST does not draw a second marker.
        //
        // Bucketed to the minute rather than the second: a retry that omits
        // `at` gets a fresh `now` and would otherwise produce a new id every
        // attempt. Keeping the bucket also means a genuine redeploy of the same
        // version later — after a rollback, say — is still its own event.
        id: format!(
            "reported:{profile_id}:{service}:{version}:{}",
            at.timestamp() / 60
        ),
        profile_id,
        profile_name,
        kind,
        change: ChangeKind::Deployed,
        source: ChangeSource::Reported,
        section: "Reported".to_string(),
        component: service,
        from: previous,
        to: version,
        at,
        summary,
    })
}

#[derive(Clone)]
struct Ctx {
    state: Arc<AppState>,
    events: Events,
    token: String,
}

async fn health() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "service": "root" })),
    )
}

async fn post_deploy(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    body: std::result::Result<Json<DeployPayload>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Shared with the MCP listener, so this hardening landed in both at once
    // rather than in whichever file somebody remembered.
    if !origin_allowed(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!(ErrorBody {
                error: "cross-origin requests are refused".into()
            })),
        );
    }

    if !authorized(&headers, &ctx.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!(ErrorBody {
                error: "missing or invalid bearer token".into()
            })),
        );
    }

    // Auth is checked before the body is even looked at, so a malformed
    // payload from an unauthenticated caller reveals nothing about the schema.
    let payload = match body {
        Ok(Json(payload)) => payload,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!(ErrorBody {
                    error: rejection.body_text()
                })),
            );
        }
    };

    match event_from_payload(&ctx.state, payload).await {
        Ok(event) => {
            let id = event.id.clone();
            let fresh = ctx.state.changes.write().await.record(vec![event]);
            if !fresh.is_empty() {
                ctx.events.emit(EVENT_CHANGES, &fresh);
            }
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!(AcceptedBody { accepted: true, id })),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!(ErrorBody {
                error: e.to_string()
            })),
        ),
    }
}

/// Stop any running listener, then start one if the config asks for it.
///
/// Called on boot and whenever the settings or the token change, so the port is
/// genuinely closed when the feature is switched off rather than merely hidden.
pub async fn restart(state: &Arc<AppState>) -> Result<()> {
    if let Some(handle) = state.ingest.lock().await.take() {
        handle.abort();
    }

    let settings = state.config.read().await.ingest.clone();
    if !settings.enabled {
        return Ok(());
    }

    let Some(token) = crate::secrets::get_secret(TOKEN_KEY).await? else {
        // No token means nothing could authenticate anyway; refusing to listen
        // is safer than listening with a check nobody can pass.
        tracing::warn!("deploy ingest is enabled but no token is stored; not listening");
        return Ok(());
    };

    let handle = serve(Arc::clone(state), state.events().await, settings.port, token).await?;
    *state.ingest.lock().await = Some(handle);
    Ok(())
}

/// Bring the listener up on loopback. Returns a handle that stops it on drop.
pub async fn serve(
    state: Arc<AppState>,
    events: Events,
    port: u16,
    token: String,
) -> Result<tokio::task::JoinHandle<()>> {
    let ctx = Ctx { state, events, token };

    let router = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/deploys", post(post_deploy))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(ctx);

    crate::local_listener::bind(router, port, "deploy ingest").await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_trims_and_caps() {
        assert_eq!(clamp("  api  ", 10), "api");
        assert_eq!(clamp(&"x".repeat(500), 8), "x".repeat(8));
    }


}
