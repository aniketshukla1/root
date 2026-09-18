//! Generic HTTP endpoint monitor.
//!
//! Watches any URL: uptime, status code, response time, and optionally a
//! substring the body must contain — which catches the common case of a service
//! answering 200 while being unable to reach its own database.
//!
//! This collector is also the proof that the [registry](crate::registry)
//! architecture holds. It was added after the UI was built, and required no
//! frontend change: it declares its own options (`expectStatus`, `expectBody`,
//! `slowMillis`, `method`) in its descriptor, and the settings form, dashboard,
//! sidebar, charts, and assistant tooling all picked it up automatically.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, Section, ServiceKind,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Body kept for substring matching. Enough for a health payload, small enough
/// that pointing this at a large page costs nothing.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// The outcome of the most recent probe, so `fetch_logs` can report transitions
/// rather than restating the same state every cycle.
#[derive(Debug, Clone, PartialEq)]
struct Probe {
    healthy: bool,
    detail: String,
    at: DateTime<Utc>,
}

pub struct HttpCollector {
    client: reqwest::Client,
    url: String,
    method: reqwest::Method,
    username: Option<String>,
    secret: Option<String>,
    expect_status: Option<u16>,
    expect_body: Option<String>,
    slow_millis: u64,
    profile_id: String,
    profile_name: String,
    /// Result of the most recent probe.
    last: Mutex<Option<Probe>>,
    /// The health state already turned into a log line, so only transitions are
    /// reported.
    reported: Mutex<Option<bool>>,
}

impl HttpCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let url = profile.endpoint.trim().to_string();
        if url.is_empty() {
            return Err(AppError::Config(
                "HTTP endpoint connections need a URL, e.g. https://api.internal/health"
                    .to_string(),
            ));
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(AppError::Config(format!(
                "`{url}` must start with http:// or https://"
            )));
        }

        let method = match profile
            .options
            .string("method")
            .unwrap_or_else(|| "GET".to_string())
            .to_ascii_uppercase()
            .as_str()
        {
            "HEAD" => reqwest::Method::HEAD,
            "POST" => reqwest::Method::POST,
            _ => reqwest::Method::GET,
        };

        Ok(HttpCollector {
            client: http_client(profile.options.insecure_tls(), REQUEST_TIMEOUT)?,
            url,
            method,
            username: profile.username.clone().filter(|u| !u.is_empty()),
            secret,
            expect_status: profile.options.u64("expectStatus").map(|s| s as u16),
            expect_body: profile.options.string("expectBody"),
            slow_millis: profile.options.u64("slowMillis").unwrap_or(1000),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            last: Mutex::new(None),
            reported: Mutex::new(None),
        })
    }

    /// Basic auth when a username is set, otherwise the secret is a bearer
    /// token — the two shapes that cover almost every internal endpoint.
    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (&self.username, &self.secret) {
            (Some(user), secret) => req.basic_auth(user, secret.as_ref()),
            (None, Some(token)) => req.bearer_auth(token),
            (None, None) => req,
        }
    }
}

#[async_trait]
impl Collector for HttpCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::HttpEndpoint
    }

    // Health is decided as a chain of `else if`s naming one condition each,
    // and several arms legitimately return the same `Health`. The chain mirrors
    // the `summary` chain below it so the two read in the same order and a
    // reader can see which condition produced which sentence; collapsing the
    // arms with `||` would satisfy the lint and break that correspondence.
    #[allow(clippy::if_same_then_else)]
    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let response = self
            .authed(self.client.request(self.method.clone(), &self.url))
            .send()
            .await;

        let latency = elapsed_ms(start);

        let (health, summary, status_code, body_ok) = match response {
            Err(e) => {
                let detail = if e.is_timeout() {
                    format!("no response within {}s", REQUEST_TIMEOUT.as_secs())
                } else {
                    e.to_string()
                };
                let mut snapshot = HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::HttpEndpoint,
                    detail,
                    latency,
                );
                snapshot.metrics = vec![
                    Metric::new("up", "Up", 0.0, MetricUnit::Count),
                    Metric::new("latency", "Response time", latency as f64, MetricUnit::Millis),
                ];
                *self.last.lock().await = Some(Probe {
                    healthy: false,
                    detail: snapshot.error.clone().unwrap_or_default(),
                    at: Utc::now(),
                });
                return Ok(snapshot);
            }
            Ok(response) => {
                let status = response.status();
                let expected = self.expect_status.unwrap_or(200);

                // Only read the body when something actually checks it.
                let body_ok = match &self.expect_body {
                    None => true,
                    Some(needle) => {
                        let body = response.text().await.unwrap_or_default();
                        body.chars().take(MAX_BODY_BYTES).collect::<String>().contains(needle)
                    }
                };

                let status_ok = status.as_u16() == expected;
                let health = if !status_ok {
                    // A 5xx is the endpoint being down; anything else unexpected
                    // is a mismatch worth flagging but not the same thing.
                    if status.is_server_error() {
                        Health::Down
                    } else {
                        Health::Degraded
                    }
                } else if !body_ok {
                    Health::Degraded
                } else if latency > self.slow_millis {
                    Health::Degraded
                } else {
                    Health::Healthy
                };

                let summary = if !status_ok {
                    format!("{status} (expected {expected}) in {latency}ms")
                } else if !body_ok {
                    format!(
                        "{status} in {latency}ms, but the body is missing `{}`",
                        self.expect_body.clone().unwrap_or_default()
                    )
                } else if latency > self.slow_millis {
                    format!("{status} in {latency}ms — slower than {}ms", self.slow_millis)
                } else {
                    format!("{status} in {latency}ms")
                };

                (health, summary, status.as_u16(), body_ok)
            }
        };

        *self.last.lock().await = Some(Probe {
            healthy: health == Health::Healthy,
            detail: summary.clone(),
            at: Utc::now(),
        });

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::HttpEndpoint,
            health,
            summary: summary.clone(),
            metrics: vec![
                Metric::new(
                    "up",
                    "Up",
                    if health == Health::Down { 0.0 } else { 1.0 },
                    MetricUnit::Count,
                ),
                Metric::new("latency", "Response time", latency as f64, MetricUnit::Millis)
                    .warn_above(self.slow_millis as f64),
                Metric::new("status", "Status code", status_code as f64, MetricUnit::Count),
            ],
            sections: vec![Section::new(
                "checks",
                "Check",
                vec![
                    ColumnSpec::text("url", "URL").primary(),
                    ColumnSpec::text("method", "Method"),
                    ColumnSpec::metric("status", "Status", MetricUnit::Count),
                    ColumnSpec::metric("latency", "Response time", MetricUnit::Millis),
                    ColumnSpec::text("body", "Body check"),
                ],
            )
            .rows(vec![Row::new(&self.url, &self.url, health)
                .summary(summary)
                .cell("url", CellValue::text(&self.url))
                .cell("method", CellValue::text(self.method.to_string()))
                .cell("status", CellValue::number(status_code as f64))
                .cell("latency", CellValue::number(latency as f64))
                .cell(
                    "body",
                    match &self.expect_body {
                        None => CellValue::Null,
                        Some(_) if body_ok => CellValue::text("matched"),
                        Some(needle) => CellValue::text(format!("missing `{needle}`")),
                    },
                )])
            .priority(10)],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// Only state *changes* become log lines. A healthy endpoint polled every
    /// 30s would otherwise emit 2,880 identical "200 OK" lines a day and drown
    /// the shared log view.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        let Some(current) = self.last.lock().await.clone() else {
            return Ok(Vec::new());
        };

        let mut reported = self.reported.lock().await;
        if *reported == Some(current.healthy) {
            return Ok(Vec::new());
        }

        // The first observation is not a transition, so a healthy endpoint
        // starts silent. An endpoint that is already failing is worth saying.
        let first_observation = reported.is_none();
        *reported = Some(current.healthy);
        if first_observation && current.healthy {
            return Ok(Vec::new());
        }

        Ok(vec![LogEvent::new(
            &self.profile_id,
            &self.profile_name,
            ServiceKind::HttpEndpoint,
            &self.url,
            if current.healthy { LogLevel::Info } else { LogLevel::Error },
            if current.healthy {
                format!("Endpoint recovered — {}", current.detail)
            } else {
                format!("Endpoint check failed — {}", current.detail)
            },
            current.at,
        )
        .stream(LogStream::Event)])
    }
}
