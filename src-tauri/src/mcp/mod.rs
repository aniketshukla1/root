//! root as an MCP server — the protocol, and the two ways it is served.
//!
//! The Model Context Protocol is JSON-RPC 2.0 with a small, fixed vocabulary:
//! `initialize`, `tools/list`, `tools/call`, `ping`, and notifications that are
//! never answered. That is the whole of what a tools-only server needs, so it
//! is written out here rather than pulled in — the same judgement `promtext`
//! makes about the Prometheus text format and `memcached` about its own.
//!
//! ## Two servers, one protocol, different reach
//!
//! | | [`standalone`] | [`http`] |
//! | --- | --- | --- |
//! | Transport | stdio, client spawns it | HTTP on loopback, client connects |
//! | Needs root running | no | yes — it *is* root |
//! | Tools | the nine gigapipe store reads | every read-only tool root has |
//! | Auth | none needed; it is a child process | bearer token |
//!
//! The split is not arbitrary. Most of root's tools answer from state a
//! *running* app accumulates — the latest snapshots, the log ring buffer, the
//! active alert set. A cold subprocess has none of that, and a `get_health`
//! that always answers "no snapshot" would be a trap rather than a tool. So the
//! standalone server serves only what is answerable from a connection profile
//! alone, and the in-app server serves everything.
//!
//! ## Read-only, structurally, in both
//!
//! [`ToolBackend::tools`] is filtered on root's own `mutating` flag. Root's
//! mutating tools stop at a confirmation dialog for a human before they touch
//! anything, and neither a pipe nor an HTTP request has a dialog — so rather
//! than invent a weaker approval, they are never advertised and never
//! dispatched.

pub mod http;
pub mod standalone;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::error::Result;

/// Protocol revisions this server knows how to speak.
///
/// A client asks for one in `initialize`. Echoing back a version we recognise
/// keeps an older client working; anything else gets our default rather than an
/// error, and the client decides whether it can live with the answer.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
pub const DEFAULT_PROTOCOL: &str = "2024-11-05";

const SERVER_NAME: &str = "root";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// JSON-RPC 2.0 error codes, as the specification numbers them.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// What a server can actually run.
///
/// Two implementations: [`standalone::Server`] reaches a gigapipe from a
/// connection profile, and [`http::AppBackend`] reaches everything a running
/// root holds.
#[async_trait]
pub trait ToolBackend: Send + Sync {
    /// Tools to advertise, already in MCP's shape.
    fn tools(&self) -> Vec<Value>;

    /// Run one, returning the text a client should see.
    async fn call(&self, name: &str, args: &Value) -> Result<String>;

    /// A sentence describing what this server is for, shown by some clients
    /// after the handshake.
    fn instructions(&self) -> &'static str;
}

/// One JSON-RPC message from the client.
///
/// `id` is absent on a notification, and a notification must never be answered
/// — that distinction is the whole of the difference in this protocol.
#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Turn root's catalog into MCP tool descriptors.
///
/// `strip` removes a prefix from each name, which the standalone server uses to
/// serve `prometheus_query` where root calls it `gigapipe_prometheus_query`.
/// Mutating tools are filtered here, in one place, so neither transport can
/// forget to.
pub fn advertise(
    tools: impl IntoIterator<Item = crate::agent::tools::ToolDef>,
    strip: &str,
) -> Vec<Value> {
    tools
        .into_iter()
        .filter(|t| !t.mutating)
        .map(|t| {
            let name = match strip.is_empty() {
                true => t.spec.name.clone(),
                false => t.spec.name.trim_start_matches(strip).to_string(),
            };
            json!({
                "name": name,
                "description": t.spec.description,
                "inputSchema": t.spec.input_schema,
            })
        })
        .collect()
}

/// Handle one parsed request. `None` means the message was a notification and
/// must not be answered.
pub async fn handle(backend: &dyn ToolBackend, request: Request) -> Option<Response> {
    let id = request.id.clone();
    let is_notification = id.is_none();
    // A notification carries no id, and a response must carry one; null is
    // what the specification uses when a request could not be identified.
    let id = id.unwrap_or(Value::Null);

    let response = match request.method.as_str() {
        "initialize" => {
            let asked = request
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str());
            let version = asked
                .filter(|v| SUPPORTED_PROTOCOLS.contains(v))
                .unwrap_or(DEFAULT_PROTOCOL);

            Response::ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                    "instructions": backend.instructions(),
                }),
            )
        }

        // Nothing to do, and nothing to answer.
        "notifications/initialized" | "notifications/cancelled" => return None,

        "ping" => Response::ok(id, json!({})),

        "tools/list" => Response::ok(id, json!({ "tools": backend.tools() })),

        "tools/call" => {
            let params = request.params.unwrap_or(Value::Null);
            let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
                return Some(Response::err(id, INVALID_PARAMS, "`name` is required"));
            };
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

            // A tool that fails is a *result* carrying isError, not a protocol
            // error: the model is meant to read the reason and try something
            // else, which it cannot do with a transport-level failure.
            match backend.call(name, &args).await {
                Ok(text) => Response::ok(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false,
                    }),
                ),
                Err(error) => Response::ok(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": error.to_string() }],
                        "isError": true,
                    }),
                ),
            }
        }

        // Declared unsupported rather than answered empty: a client that asks
        // deserves to know the difference between "none" and "not a thing this
        // server does".
        other => Response::err(id, METHOD_NOT_FOUND, format!("unknown method `{other}`")),
    };

    // Belt and braces: nothing above returns a response for a notification,
    // and if something ever does, the protocol says drop it.
    if is_notification {
        return None;
    }
    Some(response)
}

/// Parse one line and handle it, or explain why it could not be parsed.
pub async fn handle_line(backend: &dyn ToolBackend, line: &str) -> Option<Response> {
    match serde_json::from_str::<Request>(line) {
        Ok(request) => handle(backend, request).await,
        Err(error) => Some(Response::err(
            Value::Null,
            // A well-formed object we could not map is a different fault from
            // text that was never JSON, and the codes say so.
            if line.trim_start().starts_with('{') {
                INVALID_REQUEST
            } else {
                PARSE_ERROR
            },
            error.to_string(),
        )),
    }
}

/// Read requests from stdin and write responses to stdout until the pipe
/// closes, which is how an MCP client says it is finished.
///
/// stdout is the protocol channel and carries nothing else. A stray `println!`
/// corrupts the stream, and the client's error will name JSON rather than the
/// line that caused it.
pub async fn serve_stdio(backend: impl ToolBackend) -> std::io::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some(response) = handle_line(&backend, line).await else {
            continue;
        };
        let mut encoded = serde_json::to_vec(&response).unwrap_or_else(|_| {
            br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"could not encode the response"}}"#.to_vec()
        });
        encoded.push(b'\n');
        stdout.write_all(&encoded).await?;
        stdout.flush().await?;
    }

    Ok(())
}
