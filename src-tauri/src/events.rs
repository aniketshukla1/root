//! Where an event goes, without the sender knowing.
//!
//! root pushes five kinds of event — a fresh snapshot, new log lines, raised
//! alerts, observed changes, and the assistant's progress — and until now every
//! one of them was emitted through a `tauri::AppHandle`. That was correct while
//! the frontend lived in the same process: Tauri's IPC was the only way out.
//!
//! It is the wrong type the moment the frontend can be somewhere else. A
//! collector that emits a snapshot should not know whether the reader is a
//! webview in this process or a browser across a network, any more than it
//! knows which database the snapshot will be written to.
//!
//! So the emit sites take this instead. There are twelve of them, in three
//! modules, and that is the entire coupling between root's engine and how its
//! output is delivered.
//!
//! ## An enum rather than a trait object
//!
//! Three variants, all in this file, all known at compile time. A trait would
//! buy the ability to add a fourth transport from outside the crate, which
//! nothing needs and which would put a `dyn` in the signature of every module
//! that emits. The enum is cheap to clone, has no allocation on the send path,
//! and keeps the whole question in one place someone can read.
//!
//! ## Nothing here may fail loudly
//!
//! Every send is best-effort. A webview that has gone away, a browser that
//! closed its socket, a channel with no receivers — none of those are reasons
//! to disturb a poll cycle. The pattern is the one `influx.rs` and `store.rs`
//! already follow: the engine's job is to be right about the estate, and
//! delivery is somebody else's problem.

use serde::Serialize;

/// One event on its way to whoever is listening.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    /// The channel name — `monitor://snapshot` and friends. Kept as the string
    /// the frontend already subscribes to, so the two transports are
    /// indistinguishable from the other side.
    pub event: String,
    pub payload: serde_json::Value,
}

/// How this process delivers events.
#[derive(Clone)]
pub enum Events {
    /// In-process, to the Tauri webview.
    #[cfg(feature = "desktop")]
    Tauri(tauri::AppHandle),
    /// Out to every connected client. The receiver count is allowed to be
    /// zero — a server with nobody watching still polls, and still writes its
    /// history, because the point was never the audience.
    Broadcast(tokio::sync::broadcast::Sender<Envelope>),
    /// Nothing is listening and nothing should pretend to be.
    ///
    /// Used by tests and by the paths that run without a UI at all — the
    /// standalone MCP binary polls nothing and emits nothing, and had to be
    /// handed an `AppHandle` it did not want before this existed.
    Silent,
}

impl std::fmt::Debug for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(feature = "desktop")]
            Events::Tauri(_) => "Events::Tauri",
            Events::Broadcast(_) => "Events::Broadcast",
            Events::Silent => "Events::Silent",
        })
    }
}

impl Events {
    /// Send one event. Failure is swallowed on purpose — see the module note.
    pub fn emit<T: Serialize>(&self, event: &str, payload: &T) {
        match self {
            #[cfg(feature = "desktop")]
            Events::Tauri(app) => {
                use tauri::Emitter;
                let _ = app.emit(event, payload);
            }
            Events::Broadcast(tx) => {
                // Serialized once here rather than per subscriber. A failure
                // to serialize is a bug in the payload type, not a runtime
                // condition, and there is nobody on this path to tell.
                let Ok(payload) = serde_json::to_value(payload) else {
                    tracing::warn!(event, "event payload could not be serialized");
                    return;
                };
                let _ = tx.send(Envelope {
                    event: event.to_string(),
                    payload,
                });
            }
            Events::Silent => {}
        }
    }

    /// A broadcast emitter and the sender behind it, for the server to
    /// subscribe new clients to.
    pub fn broadcast(capacity: usize) -> (Self, tokio::sync::broadcast::Sender<Envelope>) {
        let (tx, _) = tokio::sync::broadcast::channel(capacity);
        (Events::Broadcast(tx.clone()), tx)
    }
}

impl Default for Events {
    /// Silent, so a state built before anything is listening — which is every
    /// state, since the transport is chosen after the engine is constructed —
    /// is valid rather than an `Option` every emit site has to unwrap.
    fn default() -> Self {
        Events::Silent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_emitter_accepts_events_and_drops_them() {
        // The property every test in this crate relies on without saying so.
        Events::Silent.emit("monitor://snapshot", &serde_json::json!({"a": 1}));
        Events::default().emit("monitor://logs", &Vec::<u8>::new());
    }

    #[tokio::test]
    async fn a_broadcast_reaches_every_subscriber() {
        let (events, tx) = Events::broadcast(16);
        let mut first = tx.subscribe();
        let mut second = tx.subscribe();

        events.emit("monitor://alerts", &serde_json::json!({"title": "down"}));

        for rx in [&mut first, &mut second] {
            let envelope = rx.recv().await.expect("delivered");
            assert_eq!(envelope.event, "monitor://alerts");
            assert_eq!(envelope.payload["title"], "down");
        }
    }

    #[tokio::test]
    async fn emitting_with_nobody_listening_is_not_an_error() {
        // A server polls whether or not a browser is open, and a poll that
        // failed because nobody was watching would be the worst kind of bug:
        // invisible exactly when nobody is there to see it.
        let (events, _tx) = Events::broadcast(4);
        events.emit("monitor://snapshot", &serde_json::json!({}));
    }

    #[tokio::test]
    async fn a_slow_subscriber_does_not_block_the_engine() {
        // The channel is bounded. A client that stops reading loses the
        // oldest events rather than applying back-pressure to the poll loop —
        // dropping an update is recoverable, stalling every collector is not.
        let (events, tx) = Events::broadcast(2);
        let mut rx = tx.subscribe();
        for i in 0..10 {
            events.emit("monitor://logs", &serde_json::json!({"i": i}));
        }
        // Lagged, not blocked: the send path never waited for this receiver.
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
        ));
    }
}
