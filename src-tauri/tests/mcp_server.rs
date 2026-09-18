//! `root-mcp` driven the way a client drives it: spawned, over stdio.
//!
//! The unit tests in `mcp.rs` cover the handlers. They cannot cover the thing
//! most likely to break a server that works perfectly in isolation — that the
//! process speaks the protocol on a real pipe and keeps stdout clean.
//!
//! A single stray `println!`, a log line written to stdout instead of stderr,
//! or a response missing its newline turns every request into a parse error on
//! the client side, and the error the user sees names JSON rather than the
//! cause. So this spawns the actual binary, talks to it, and asserts that what
//! comes back is exactly what a client expects.
//!
//! Never skips: the gigapipe it reads is wiremock, and the binary is built by
//! cargo as a dependency of this test.

use std::process::Stdio;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A spawned server, plus the two pipe ends a client holds.
struct Client {
    child: Child,
    stdout: BufReader<ChildStdout>,
}

impl Client {
    /// Spawn the real binary, pointed at `host` through the same environment
    /// variables `gigapipe-mcp` takes.
    ///
    /// `ROOT_CONFIG_DIR` is set to somewhere empty so the test can never pick
    /// up a developer's own configured connections.
    fn spawn(host: &str, config_dir: &std::path::Path) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_root-mcp"))
            .env("GIGAPIPE_HOST", host)
            .env("ROOT_CONFIG_DIR", config_dir)
            .env("RUST_LOG", "error")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("root-mcp should be built and runnable");

        let stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));
        Client { child, stdout }
    }

    async fn send(&mut self, message: Value) {
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        self.child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(&line)
            .await
            .expect("the server should still be reading");
    }

    /// Read one response. Anything that is not a JSON object on its own line
    /// fails here, which is the point: that is exactly what a client sees.
    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .await
            .expect("stdout should be readable");
        assert!(read > 0, "the server closed stdout instead of answering");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout carried something that is not JSON-RPC: {e}\n{line}"))
    }

    async fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await;
        let response = self.recv().await;
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], id, "answered the wrong request");
        response
    }

    /// The opening exchange every client performs before anything else.
    async fn handshake(&mut self) -> Value {
        let response = self
            .request(
                1,
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "integration-test", "version": "0" },
                }),
            )
            .await;
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        response["result"].clone()
    }

    async fn shutdown(mut self) {
        drop(self.child.stdin.take());
        let status = self.child.wait().await.expect("the server should exit");
        assert!(
            status.success(),
            "closing the pipe should be a clean exit, got {status}"
        );
    }
}

fn empty_config_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("root-mcp-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir should be creatable");
    dir
}

#[tokio::test]
async fn the_server_completes_a_handshake_and_lists_its_tools() {
    let store = MockServer::start().await;
    let dir = empty_config_dir();
    let mut client = Client::spawn(&store.uri(), &dir);

    let result = client.handshake().await;
    assert_eq!(result["serverInfo"]["name"], "root");
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert!(result["capabilities"]["tools"].is_object());

    let listed = client.request(2, "tools/list", json!({})).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 9, "got {names:?}");
    assert!(names.contains(&"prometheus_query"));
    assert!(names.contains(&"loki_query"));
    assert!(names.contains(&"tempo_tags"));

    client.shutdown().await;
}

/// The whole point of the server: a call reaches the store and the answer
/// comes back as MCP content.
#[tokio::test]
async fn a_tool_call_reaches_the_store_and_returns_its_answer() {
    let store = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query_range"))
        .and(query_param("query", "up"))
        .and(query_param("step", "1m"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "success",
            "data": { "resultType": "matrix", "result": [{ "metric": { "job": "web" } }] }
        })))
        .expect(1)
        .mount(&store)
        .await;

    let dir = empty_config_dir();
    let mut client = Client::spawn(&store.uri(), &dir);
    client.handshake().await;

    let response = client
        .request(
            2,
            "tools/call",
            json!({
                "name": "prometheus_query",
                "arguments": { "query": "up", "step": "1m" }
            }),
        )
        .await;

    let result = &response["result"];
    assert_eq!(result["isError"], false);
    let text = result["content"][0]["text"].as_str().expect("text content");
    let payload: Value = serde_json::from_str(text).expect("the content should be the store's JSON");
    assert_eq!(payload["data"]["result"][0]["metric"]["job"], "web");

    client.shutdown().await;
}

/// A store that rejects a query must surface that as tool content the model
/// can read, never as a transport error that looks like the server broke.
#[tokio::test]
async fn a_rejected_query_comes_back_as_tool_content_not_a_crash() {
    let store = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .respond_with(ResponseTemplate::new(400).set_body_string("parse error: unexpected }"))
        .mount(&store)
        .await;

    let dir = empty_config_dir();
    let mut client = Client::spawn(&store.uri(), &dir);
    client.handshake().await;

    let response = client
        .request(
            2,
            "tools/call",
            json!({ "name": "loki_query", "arguments": { "query": "}" } }),
        )
        .await;

    assert!(
        response.get("error").is_none(),
        "a failing tool is a result, not a protocol error: {response}"
    );
    assert_eq!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("parse error"), "reason lost: {text}");

    // The server must still be usable afterwards.
    let listed = client.request(3, "tools/list", json!({})).await;
    assert!(listed["result"]["tools"].is_array());

    client.shutdown().await;
}

/// A notification has no id and must produce no reply. If the server answers
/// one, the client's next request reads that stale reply and every response
/// afterwards is off by one — which presents as nonsense rather than an error.
#[tokio::test]
async fn a_notification_is_not_answered_and_does_not_desynchronise_the_stream() {
    let store = MockServer::start().await;
    let dir = empty_config_dir();
    let mut client = Client::spawn(&store.uri(), &dir);
    client.handshake().await;

    client
        .send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled" }))
        .await;

    // If the notification had been answered, this would return that answer.
    let response = client.request(7, "ping", json!({})).await;
    assert_eq!(response["id"], 7);
    assert!(response["result"].is_object());

    client.shutdown().await;
}

#[tokio::test]
async fn an_unparseable_line_is_answered_and_the_server_keeps_going() {
    let store = MockServer::start().await;
    let dir = empty_config_dir();
    let mut client = Client::spawn(&store.uri(), &dir);
    client.handshake().await;

    client
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"this is not json\n")
        .await
        .unwrap();
    let response = client.recv().await;
    assert_eq!(response["error"]["code"], -32700);

    let listed = client.request(3, "tools/list", json!({})).await;
    assert!(listed["result"]["tools"].is_array());

    client.shutdown().await;
}
