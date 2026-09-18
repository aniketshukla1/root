//! The in-app MCP server, driven over a real socket.
//!
//! Two things are being asserted here, and only one of them is the protocol.
//!
//! The first is that a client can complete a handshake, list tools and call
//! one over HTTP — the ordinary path.
//!
//! The second matters more. This listener is reachable by any program on the
//! machine that has the token, and it is attached to an app holding
//! infrastructure credentials. So the tests that count are the ones that try
//! to get more out of it than it should give: calling without a token, calling
//! with the wrong token, and naming a tool that restarts a container. Those
//! are the assertions worth keeping, and they are why this file exists rather
//! than a handful more unit tests on the handler.
//!
//! Never skips. It binds a port on loopback and talks to itself.

use std::sync::Arc;

use inframonitoring_lib::config::AppConfig;
use inframonitoring_lib::mcp::http;
use inframonitoring_lib::state::AppState;
use serde_json::{json, Value};

/// A listener on a free loopback port, with a known token.
struct Server {
    base: String,
    token: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start() -> Server {
        // Ask the OS for a free port, then let go of it. A fixed port makes
        // this test fail when anything else on the machine happens to hold it.
        let port = {
            let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a free port");
            probe.local_addr().unwrap().port()
        };

        let dir = std::env::temp_dir().join(format!("root-mcp-http-{port}"));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let state = Arc::new(AppState::new(
            AppConfig::default(),
            dir.join("config.json"),
        ));

        let token = "test-token-not-a-real-secret".to_string();
        let handle = http::serve(state, port, token.clone())
            .await
            .expect("the listener should bind");

        Server {
            base: format!("http://127.0.0.1:{port}"),
            token,
            handle,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}{}", self.base, http::ENDPOINT)
    }

    /// POST a JSON-RPC message with the good token.
    async fn rpc(&self, message: Value) -> (reqwest::StatusCode, Value) {
        self.post(message, Some(&self.token)).await
    }

    async fn post(&self, message: Value, token: Option<&str>) -> (reqwest::StatusCode, Value) {
        let mut request = reqwest::Client::new().post(self.endpoint()).json(&message);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.expect("the listener should answer");
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let parsed = serde_json::from_str(&body).unwrap_or(Value::Null);
        (status, parsed)
    }

    async fn handshake(&self) -> Value {
        let (status, body) = self
            .rpc(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2024-11-05", "capabilities": {},
                            "clientInfo": { "name": "test", "version": "0" } }
            }))
            .await;
        assert_eq!(status, 200);
        body["result"].clone()
    }

    async fn tool_names(&self) -> Vec<String> {
        let (_, body) = self
            .rpc(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .await;
        body["result"]["tools"]
            .as_array()
            .expect("tools should be an array")
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[tokio::test]
async fn a_client_can_handshake_and_list_the_read_only_surface() {
    let server = Server::start().await;

    let result = server.handshake().await;
    assert_eq!(result["serverInfo"]["name"], "root");
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert!(result["capabilities"]["tools"].is_object());

    let names = server.tool_names().await;
    // The whole point of the in-app server: tools the standalone one cannot
    // serve, because they answer from state a running app holds.
    for expected in ["list_connections", "get_health", "search_logs", "list_alerts"] {
        assert!(names.iter().any(|n| n == expected), "{expected} missing");
    }
    // And the gigapipe reads are here too, under root's own prefixed names.
    assert!(names.iter().any(|n| n == "gigapipe_prometheus_query"));
}

/// The security assertion. Every mutating tool stops at a confirmation dialog
/// for a human, and an HTTP request has no dialog — so none of them may be
/// advertised, whatever the app's own agent mode is.
#[tokio::test]
async fn no_mutating_tool_is_advertised() {
    let server = Server::start().await;
    server.handshake().await;
    let names = server.tool_names().await;

    for forbidden in [
        "restart_container",
        "start_container",
        "stop_container",
        "trigger_jenkins_build",
        "terminate_database_query",
    ] {
        assert!(!names.iter().any(|n| n == forbidden), "{forbidden} served");
    }
}

/// Advertising is not the gate — refusing to dispatch is. A client that knows
/// the name anyway must still be turned away.
#[tokio::test]
async fn naming_a_mutating_tool_directly_is_refused() {
    let server = Server::start().await;
    server.handshake().await;

    let (status, body) = server
        .rpc(json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "name": "restart_container",
                "arguments": { "connection": "anything", "container": "anything" }
            }
        }))
        .await;

    assert_eq!(status, 200);
    assert_eq!(body["result"]["isError"], true, "{body}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("no read-only tool"),
        "should refuse by name, got: {text}"
    );
}

#[tokio::test]
async fn a_request_without_a_token_is_rejected() {
    let server = Server::start().await;
    let (status, _) = server
        .post(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            None,
        )
        .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn a_request_with_the_wrong_token_is_rejected() {
    let server = Server::start().await;
    let (status, _) = server
        .post(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            Some("not-the-token"),
        )
        .await;
    assert_eq!(status, 401);

    // A prefix of the real token must fail too, or the comparison is leaking.
    let (status, _) = server
        .post(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            Some(&server.token[..8]),
        )
        .await;
    assert_eq!(status, 401);
}

/// A real call against an app with nothing configured. The answer is empty,
/// but it is a *successful* answer — the tool ran.
#[tokio::test]
async fn a_tool_call_runs_against_the_live_app_state() {
    let server = Server::start().await;
    server.handshake().await;

    let (status, body) = server
        .rpc(json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "list_connections", "arguments": {} }
        }))
        .await;

    assert_eq!(status, 200);
    assert!(body.get("error").is_none(), "{body}");
    assert_eq!(body["result"]["isError"], false, "{body}");
    assert!(body["result"]["content"][0]["text"].is_string());
}

/// A notification has no id and gets no body. Answering one would leave a
/// client reading a response it never asked for.
#[tokio::test]
async fn a_notification_is_accepted_with_no_body() {
    let server = Server::start().await;
    let response = reqwest::Client::new()
        .post(server.endpoint())
        .bearer_auth(&server.token)
        .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .send()
        .await
        .expect("the listener should answer");

    assert_eq!(response.status(), 202);
    assert!(response.text().await.unwrap_or_default().is_empty());
}

/// A server that pushes nothing says so, rather than leaving a client waiting
/// on a stream that will never carry anything.
#[tokio::test]
async fn a_get_on_the_endpoint_reports_that_nothing_is_streamed() {
    let server = Server::start().await;
    let response = reqwest::Client::new()
        .get(server.endpoint())
        .bearer_auth(&server.token)
        .send()
        .await
        .expect("the listener should answer");
    assert_eq!(response.status(), 405);
}

/// Reachability without a token, so a client can tell "not running" from
/// "wrong credentials". It says nothing about the estate.
#[tokio::test]
async fn health_needs_no_token_and_reveals_nothing() {
    let server = Server::start().await;
    let response = reqwest::Client::new()
        .get(format!("{}/health", server.base))
        .send()
        .await
        .expect("the listener should answer");

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["server"], "root");
    assert!(body.get("connections").is_none());
    assert!(body.get("profiles").is_none());
}

#[tokio::test]
async fn an_unknown_method_is_a_protocol_error() {
    let server = Server::start().await;
    let (status, body) = server
        .rpc(json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/destroy" }))
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["error"]["code"], -32601);
}

#[tokio::test]
async fn a_body_that_is_not_json_rpc_is_a_parse_error() {
    let server = Server::start().await;
    let response = reqwest::Client::new()
        .post(server.endpoint())
        .bearer_auth(&server.token)
        .header("content-type", "application/json")
        .body("this is not json")
        .send()
        .await
        .expect("the listener should answer");

    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32700);
}
