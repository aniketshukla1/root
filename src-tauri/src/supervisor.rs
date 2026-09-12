//! Per-connection polling tasks.
//!
//! Each enabled profile gets one task that alternates between a health
//! snapshot and a log pull, then sleeps for the configured interval. Tasks are
//! tracked by profile id so reconfiguring a connection cancels the old task
//! before starting a replacement.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use tauri::{AppHandle, Emitter};

use crate::collectors::build_collector;
use crate::config::ConnectionProfile;
use crate::error::Result;
use crate::secrets;
use crate::changes::changes_from_snapshots;
use crate::state::{alerts_from_snapshot, AppState};

pub const EVENT_SNAPSHOT: &str = "monitor://snapshot";
pub const EVENT_LOGS: &str = "monitor://logs";
pub const EVENT_ALERTS: &str = "monitor://alerts";
pub const EVENT_STATUS: &str = "monitor://connection-status";
pub const EVENT_CHANGES: &str = "monitor://changes";

/// On first poll there is no watermark, so this much history is backfilled.
const INITIAL_LOG_LOOKBACK_MINUTES: i64 = 15;

/// Stop every poller, then start one for each enabled profile. Called at
/// startup and whenever the connection list changes.
pub async fn reload_all(app: AppHandle, state: Arc<AppState>) {
    let profiles = state.config.read().await.profiles.clone();

    stop_all(&state).await;
    state.collectors.write().await.clear();

    for profile in profiles.into_iter().filter(|p| p.enabled) {
        start_profile(app.clone(), state.clone(), profile).await;
    }
}

pub async fn stop_all(state: &Arc<AppState>) {
    let mut pollers = state.pollers.lock().await;
    for (_, handle) in pollers.drain() {
        handle.abort();
    }
}

pub async fn stop_profile(state: &Arc<AppState>, profile_id: &str) {
    if let Some(handle) = state.pollers.lock().await.remove(profile_id) {
        handle.abort();
    }
    state.collectors.write().await.remove(profile_id);
    state.snapshots.write().await.remove(profile_id);
    state.watermarks.write().await.remove(profile_id);
    state.history.write().await.drop_profile(profile_id);
    state.changes.write().await.forget(profile_id);
}

/// Build the collector for a profile and spawn its polling task. A profile
/// that cannot even be constructed (bad URL, missing credential) reports the
/// failure to the UI rather than silently vanishing.
pub async fn start_profile(app: AppHandle, state: Arc<AppState>, profile: ConnectionProfile) {
    stop_profile(&state, &profile.id).await;

    let secret = match secrets::get_secret(&profile.secret_key()).await {
        Ok(secret) => secret,
        Err(e) => {
            tracing::warn!(profile = %profile.name, "could not read the stored credential: {e}");
            None
        }
    };

    let collector = match build_collector(&profile, secret).await {
        Ok(collector) => collector,
        Err(e) => {
            let _ = app.emit(
                EVENT_STATUS,
                serde_json::json!({
                    "profileId": profile.id,
                    "profileName": profile.name,
                    "state": "error",
                    "message": e.to_string(),
                }),
            );
            tracing::error!(profile = %profile.name, "could not start the collector: {e}");
            return;
        }
    };

    state
        .collectors
        .write()
        .await
        .insert(profile.id.clone(), collector.clone());

    let profile_id = profile.id.clone();
    let task_state = Arc::clone(&state);

    let handle = tokio::spawn(async move {
        let state = task_state;
        let interval = profile.poll_interval();
        let log_limit = profile.options.log_limit();

        loop {
            // --- Health snapshot
            match collector.collect().await {
                Ok(mut snapshot) => {
                    // Say which connection already watches each component.
                    //
                    // Done here rather than in the collector because it is a
                    // join across connections, and a collector that knew about
                    // its siblings would be a collector that could not be
                    // tested alone. This is the only place a finished snapshot
                    // and the whole configuration are both in hand.
                    link_to_connections(&state, &mut snapshot).await;

                    // `insert` hands back the snapshot this one replaces, which
                    // is exactly the input the change engine needs — no second
                    // copy of the previous state has to be kept anywhere.
                    let previous = state
                        .snapshots
                        .write()
                        .await
                        .insert(profile.id.clone(), snapshot.clone());
                    state.history.write().await.record(&snapshot);
                    let _ = app.emit(EVENT_SNAPSHOT, &snapshot);

                    let observed = changes_from_snapshots(previous.as_ref(), &snapshot);
                    if !observed.is_empty() {
                        let fresh = state.changes.write().await.record(observed);
                        if !fresh.is_empty() {
                            let _ = app.emit(EVENT_CHANGES, &fresh);
                        }
                    }

                    let derived = alerts_from_snapshot(&snapshot);
                    let raised = state.alerts.write().await.reconcile(&profile.id, derived);
                    if !raised.is_empty() {
                        let _ = app.emit(EVENT_ALERTS, &raised);
                    }
                }
                Err(e) => {
                    tracing::warn!(profile = %profile.name, "snapshot failed: {e}");
                }
            }

            // --- Logs since the watermark
            let since = state
                .watermarks
                .read()
                .await
                .get(&profile.id)
                .copied()
                .unwrap_or_else(|| {
                    Utc::now() - ChronoDuration::minutes(INITIAL_LOG_LOOKBACK_MINUTES)
                });

            match collector.fetch_logs(since, log_limit).await {
                Ok(events) if !events.is_empty() => {
                    if let Some(newest) = events.iter().map(|e| e.timestamp).max() {
                        state
                            .watermarks
                            .write()
                            .await
                            .insert(profile.id.clone(), newest.max(since));
                    }
                    state.logs.write().await.push_many(events.clone());
                    let _ = app.emit(EVENT_LOGS, &events);
                }
                Ok(_) => {
                    // Nothing new. Advance the watermark anyway so a quiet
                    // connection does not keep re-scanning the same window.
                    state
                        .watermarks
                        .write()
                        .await
                        .insert(profile.id.clone(), Utc::now() - ChronoDuration::seconds(5));
                }
                Err(e) => {
                    tracing::debug!(profile = %profile.name, "log fetch failed: {e}");
                }
            }

            tokio::time::sleep(interval).await;
        }
    });

    state.pollers.lock().await.insert(profile_id, handle);
}

/// Run one collection cycle immediately, outside the poll schedule. Used by the
/// "Test connection" button and the assistant's refresh tool.
pub async fn poll_once(state: &Arc<AppState>, profile_id: &str) -> Result<crate::model::HealthSnapshot> {
    let collector = state.collector(profile_id).await?;
    let snapshot = collector.collect().await?;
    state.history.write().await.record(&snapshot);
    let previous = state
        .snapshots
        .write()
        .await
        .insert(profile_id.to_string(), snapshot.clone());
    let observed = changes_from_snapshots(previous.as_ref(), &snapshot);
    if !observed.is_empty() {
        state.changes.write().await.record(observed);
    }
    let derived = alerts_from_snapshot(&snapshot);
    state.alerts.write().await.reconcile(profile_id, derived);
    Ok(snapshot)
}

/// Mark components that a configured connection is already monitoring.
///
/// In a containerised estate the same Postgres appears twice: once as a
/// container on a Docker host, once as the Postgres connection pointed at it.
/// Root showed them as unrelated, so an operator reading the container list
/// could not tell which rows were already covered properly and which were only
/// being watched as processes.
///
/// The match is on published host port, not on the image: a connection to
/// `localhost:5442` and a container publishing 5442 are the same service, and
/// that is a fact rather than a resemblance. It also survives an image root
/// does not recognise.
async fn link_to_connections(state: &Arc<AppState>, snapshot: &mut crate::model::HealthSnapshot) {
    // Only the Docker host itself hosts other people's services this way.
    if !matches!(
        snapshot.kind,
        crate::model::ServiceKind::Docker | crate::model::ServiceKind::Podman
    ) {
        return;
    }

    let connections: Vec<(String, String)> = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .filter(|p| p.enabled && p.id != snapshot.profile_id)
            .map(|p| (p.id.clone(), p.endpoint.clone()))
            .collect()
    };
    if connections.is_empty() {
        return;
    }

    for section in &mut snapshot.sections {
        if section.id != "containers" {
            continue;
        }
        for row in &mut section.rows {
            let ports: Vec<u16> = row
                .extra
                .get("hostPorts")
                .and_then(|v| v.as_array())
                .map(|values| values.iter().filter_map(|v| v.as_u64()).map(|p| p as u16).collect())
                .unwrap_or_default();

            let Some(watcher) = crate::identify::monitored_by(
                &ports,
                connections.iter().map(|(id, e)| (id.as_str(), e.as_str())),
            ) else {
                continue;
            };

            // Alongside the ports rather than replacing them, so a later
            // reader can still see what the match was made on.
            if let Some(extra) = row.extra.as_object_mut() {
                extra.insert("watchedBy".to_string(), serde_json::Value::String(watcher));
            }
        }
    }
}
