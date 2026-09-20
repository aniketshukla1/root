//! The server over a real socket.
//!
//! `server.rs`'s own tests drive the router directly, which is fast and cannot
//! exercise the two things that only exist on a connection:
//!
//!   - **The WebSocket upgrade.** Hyper inserts the `OnUpgrade` extension when
//!     it serves a connection; a router driven in-process has none, so the
//!     extractor rejects every handshake regardless of the token. An upgrade
//!     that works against a router and not against a socket has tested
//!     nothing, so it is tested here.
//!   - **That events actually arrive.** The whole point of the server is that
//!     a browser somewhere else sees what the engine sees. That is one
//!     assertion and it needs both ends.
//!
//! No live dependency: the server binds an ephemeral port on loopback and the
//! test is the only client. Nothing here skips.

use std::net::SocketAddr;
use std::sync::Arc;

use inframonitoring_lib::config::AppConfig;
use inframonitoring_lib::events::Events;
use inframonitoring_lib::server;
use inframonitoring_lib::state::AppState;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const TOKEN: &str = "a-token-of-at-least-sixteen";

/// Start a server on an ephemeral port. Returns its address and the emitter,
/// so a test can make the engine speak.
async fn start() -> (SocketAddr, Events) {
    let state = Arc::new(AppState::new(
        AppConfig::default(),
        std::path::PathBuf::from("/tmp/root-server-test.json"),
    ));
    let (emitter, sender) = Events::broadcast(64);
    *state.events.write().await = emitter.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = server::router(server::Ctx {
        state,
        events: sender,
        token: TOKEN.to_string(),
        directory: None,
        web_root: None,
        oidc: None,
    });

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // Give the accept loop a moment to be ready.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (addr, emitter)
}

/// A minimal HTTP/1.1 request, written by hand so the test depends on no
/// client library and reads exactly like what a browser sends.
async fn request(addr: SocketAddr, raw: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream.write_all(raw.as_bytes()).await.expect("write");

    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.expect("status line");

    let mut headers = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        headers.push_str(&line);
    }

    // Body, if the response declared one. Read exactly the declared length so
    // this never blocks on a connection the server is keeping open.
    let length: usize = headers
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);

    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).await.expect("body");
    }
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn the_health_endpoint_answers_over_a_socket() {
    let (addr, _) = start().await;
    let (status, body) = request(
        addr,
        &format!("GET /healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"),
    )
    .await;

    assert!(status.contains("200"), "{status}");
    assert!(body.contains("\"service\":\"root\""), "{body}");
    assert!(!body.contains(TOKEN), "the token leaked into health: {body}");
}

#[tokio::test]
async fn a_command_runs_over_a_socket_with_the_token() {
    let (addr, _) = start().await;
    let (status, body) = request(
        addr,
        &format!(
            "POST /rpc/get_service_catalog HTTP/1.1\r\nHost: {addr}\r\n\
             Authorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\n\
             Content-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        ),
    )
    .await;

    assert!(status.contains("200"), "{status}");
    assert!(body.contains("docker"), "the catalog should be here: {body}");
}

#[tokio::test]
async fn a_command_without_the_token_is_refused_over_a_socket() {
    let (addr, _) = start().await;
    let (status, body) = request(
        addr,
        &format!(
            "POST /rpc/get_service_catalog HTTP/1.1\r\nHost: {addr}\r\n\
             Content-Type: application/json\r\nContent-Length: 2\r\n\
             Connection: close\r\n\r\n{{}}"
        ),
    )
    .await;

    assert!(status.contains("401"), "{status}");
    assert!(!body.contains("docker"), "the estate leaked to an anonymous caller");
}

#[tokio::test]
async fn the_event_stream_upgrades_and_delivers_what_the_engine_emits() {
    // The assertion the whole server exists for: something the engine emits
    // reaches a client that is not in this process.
    let (addr, emitter) = start().await;

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(
            format!(
                "GET /events HTTP/1.1\r\nHost: {addr}\r\n\
                 Authorization: Bearer {TOKEN}\r\n\
                 Connection: Upgrade\r\nUpgrade: websocket\r\n\
                 Sec-WebSocket-Version: 13\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("handshake");

    let mut reader = BufReader::new(&mut stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.expect("status");
    assert!(
        status.contains("101"),
        "the socket should have upgraded: {status}"
    );

    // Drain the handshake headers so the next read is the first frame.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }

    emitter.emit(
        "monitor://alerts",
        &serde_json::json!({ "title": "orders-db is down" }),
    );

    // One text frame. Read the raw bytes rather than decoding the protocol:
    // the payload is small and unmasked from the server, so the text is
    // findable without a WebSocket library standing between the assertion and
    // what actually went over the wire.
    let mut frame = vec![0u8; 512];
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read(&mut frame),
    )
    .await
    .expect("a frame within five seconds")
    .expect("read");

    let text = String::from_utf8_lossy(&frame[..read]);
    assert!(
        text.contains("monitor://alerts") && text.contains("orders-db is down"),
        "the event did not arrive intact: {text}"
    );
}

#[tokio::test]
async fn the_event_stream_refuses_an_unauthenticated_upgrade_over_a_socket() {
    let (addr, _) = start().await;
    let (status, _) = request(
        addr,
        &format!(
            "GET /events HTTP/1.1\r\nHost: {addr}\r\n\
             Connection: Upgrade\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;

    assert!(
        status.contains("401"),
        "an anonymous client must not reach the event stream: {status}"
    );
}
