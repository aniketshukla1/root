//! The posture every local listener in this app shares, in one place.
//!
//! root opens two sockets — [`crate::ingest`] for deploy records and
//! [`crate::mcp::http`] for the MCP server — and both are the same decision: a
//! port in a desktop app that holds infrastructure credentials. They had their
//! own copies of the bearer check, the loopback bind and the body limit, which
//! meant a hardening change had to be made twice and a reviewer reading one
//! file could not tell whether the other matched.
//!
//! The rules live here now, so there is one answer to "what protects these".
//!
//! - **Loopback only.** [`bind`] constructs the address from
//!   [`Ipv4Addr::LOCALHOST`]; `0.0.0.0` is not reachable from this API.
//! - **Bearer token, compared in constant time.** A plain `==` on a secret
//!   leaks its prefix to anyone who can time the reply.
//! - **Origin refused when it is not loopback.** A token is the primary
//!   defence, and the token is also the part most likely to leak — it gets
//!   pasted into client configuration files. This is what survives that: a
//!   page the user visits can point an attacker-controlled name at 127.0.0.1
//!   (DNS rebinding) and reach the port from the browser, and only the browser
//!   attaches an `Origin`. A request without one is a normal client and passes.

use std::net::{Ipv4Addr, SocketAddr};

use axum::http::HeaderMap;
use axum::Router;
use subtle::ConstantTimeEq;

use crate::error::{AppError, Result};

/// Constant-time bearer check.
pub fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(presented) = value.strip_prefix("Bearer ") else {
        return false;
    };
    presented.trim().as_bytes().ct_eq(token.as_bytes()).into()
}

/// Whether a caller's `Origin` may reach a loopback listener.
///
/// No `Origin` is allowed — that is what an ordinary client sends. Only a
/// browser sets one, and a browser is the caller this exists to turn away.
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(axum::http::header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Some(rest) = origin.split("://").nth(1) else {
        // No scheme at all — `null` from a sandboxed frame lands here.
        return false;
    };
    // An IPv6 literal carries its own brackets and its own colons, so the port
    // cannot be found by splitting on `:` until the brackets are past.
    let host = match rest.strip_prefix('[') {
        Some(after) => after.split_once(']').map(|(host, _)| host).unwrap_or(after),
        None => rest.split(':').next().unwrap_or(""),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Bind `router` on loopback and serve it until the returned handle is aborted.
///
/// `what` names the listener in the log line and in any bind error, so an
/// operator who has put two listeners on one port is told which one failed.
pub async fn bind(
    router: Router,
    port: u16,
    what: &str,
) -> Result<tokio::task::JoinHandle<()>> {
    // Loopback, explicitly. Never `0.0.0.0` — see the module header.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| AppError::Rejected(format!("could not bind {what} on {addr}: {e}")))?;

    tracing::info!("{what} listening on http://{addr}");

    let name = what.to_string();
    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::warn!("{name} stopped: {e}");
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_header(name: axum::http::HeaderName, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, value.parse().unwrap());
        headers
    }

    #[test]
    fn bearer_requires_the_exact_token() {
        let headers = with_header(axum::http::header::AUTHORIZATION, "Bearer secret");
        assert!(authorized(&headers, "secret"));
        assert!(!authorized(&headers, "secre"));
        assert!(!authorized(&headers, "secrets"));
        assert!(!authorized(&HeaderMap::new(), "secret"));
    }

    #[test]
    fn bearer_rejects_other_schemes() {
        let headers = with_header(axum::http::header::AUTHORIZATION, "Basic c2VjcmV0");
        assert!(!authorized(&headers, "secret"));
    }

    /// An ordinary MCP or CI client sends no Origin at all.
    #[test]
    fn a_request_without_an_origin_is_allowed() {
        assert!(origin_allowed(&HeaderMap::new()));
    }

    #[test]
    fn a_loopback_origin_is_allowed() {
        for origin in [
            "http://localhost",
            "http://localhost:3000",
            "http://127.0.0.1:8080",
            "https://[::1]:9000",
        ] {
            let headers = with_header(axum::http::header::ORIGIN, origin);
            assert!(origin_allowed(&headers), "{origin} should be allowed");
        }
    }

    /// The rebinding case: a page served from anywhere else, reaching a name
    /// that now resolves to 127.0.0.1.
    #[test]
    fn a_remote_origin_is_refused() {
        for origin in [
            "https://evil.example",
            "http://attacker.test:1234",
            "null",
            "http://localhost.evil.example",
        ] {
            let headers = with_header(axum::http::header::ORIGIN, origin);
            assert!(!origin_allowed(&headers), "{origin} should be refused");
        }
    }
}
