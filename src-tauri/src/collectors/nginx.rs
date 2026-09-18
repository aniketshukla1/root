//! Nginx monitor, via the `stub_status` module.
//!
//! `stub_status` returns seven numbers in a fixed, decidedly non-machine
//! friendly shape:
//!
//! ```text
//! Active connections: 43
//! server accepts handled requests
//!  8129 8129 21344
//! Reading: 0 Writing: 3 Waiting: 40
//! ```
//!
//! Three of those — accepts, handled, requests — are monotonic counters since
//! the worker started, so their raw values say almost nothing. What matters is
//! the *rate*, and the difference between accepts and handled: when those
//! diverge, nginx is dropping connections because it ran out of file
//! descriptors or worker slots. That gap is the single most useful signal
//! stub_status offers and it is invisible if you only read the totals.

use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct NginxCollector {
    client: reqwest::Client,
    url: String,
    username: Option<String>,
    secret: Option<String>,
    connection_warn: f64,
    profile_id: String,
    profile_name: String,
    /// Previous counters, to turn monotonic totals into rates.
    previous: Mutex<Option<Sample>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StubStatus {
    pub active: f64,
    pub accepts: f64,
    pub handled: f64,
    pub requests: f64,
    pub reading: f64,
    pub writing: f64,
    pub waiting: f64,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    status: StubStatus,
    at: DateTime<Utc>,
}

impl NginxCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let url = profile.endpoint.trim().to_string();
        if url.is_empty() {
            return Err(AppError::Config(
                "Nginx connections need the stub_status URL, e.g. http://nginx.internal/nginx_status"
                    .to_string(),
            ));
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(AppError::Config(format!(
                "`{url}` must start with http:// or https://"
            )));
        }

        Ok(NginxCollector {
            client: http_client(profile.options.insecure_tls(), REQUEST_TIMEOUT)?,
            url,
            username: profile.username.clone().filter(|u| !u.is_empty()),
            secret,
            connection_warn: profile.options.u64("connectionWarn").unwrap_or(1000) as f64,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            previous: Mutex::new(None),
        })
    }
}

/// Parse the seven numbers out of stub_status output.
///
/// Positional rather than keyed, because the middle line has no labels at all —
/// the header names the columns and the values follow on the next line.
pub(crate) fn parse_stub_status(body: &str) -> Option<StubStatus> {
    let mut active = None;
    let mut totals: Option<(f64, f64, f64)> = None;
    let mut reading = 0.0;
    let mut writing = 0.0;
    let mut waiting = 0.0;

    let mut lines = body.lines().map(str::trim).filter(|l| !l.is_empty());
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("Active connections:") {
            active = rest.trim().parse::<f64>().ok();
        } else if line.starts_with("server accepts handled requests") {
            // The values are on the *following* line, unlabelled.
            let values: Vec<f64> = lines
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|v| v.parse::<f64>().ok())
                .collect();
            if values.len() >= 3 {
                totals = Some((values[0], values[1], values[2]));
            }
        } else if line.starts_with("Reading:") {
            // `Reading: 0 Writing: 3 Waiting: 40` on one line.
            let tokens: Vec<&str> = line.split_whitespace().collect();
            for pair in tokens.windows(2) {
                let value = pair[1].parse::<f64>().unwrap_or(0.0);
                match pair[0] {
                    "Reading:" => reading = value,
                    "Writing:" => writing = value,
                    "Waiting:" => waiting = value,
                    _ => {}
                }
            }
        }
    }

    let (accepts, handled, requests) = totals?;
    Some(StubStatus {
        active: active?,
        accepts,
        handled,
        requests,
        reading,
        writing,
        waiting,
    })
}

/// Connections accepted but never handled — nginx dropping work.
pub(crate) fn dropped(status: &StubStatus) -> f64 {
    (status.accepts - status.handled).max(0.0)
}

#[async_trait]
impl Collector for NginxCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Nginx
    }

    // Health is decided as a chain of `else if`s naming one condition each,
    // and several arms legitimately return the same `Health`. The chain mirrors
    // the `summary` chain below it so the two read in the same order and a
    // reader can see which condition produced which sentence; collapsing the
    // arms with `||` would satisfy the lint and break that correspondence.
    #[allow(clippy::if_same_then_else)]
    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let request = match (&self.username, &self.secret) {
            (Some(user), secret) => self.client.get(&self.url).basic_auth(user, secret.as_ref()),
            _ => self.client.get(&self.url),
        };

        let body = match request.send().await {
            Ok(response) if response.status().is_success() => {
                response.text().await.unwrap_or_default()
            }
            Ok(response) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Nginx,
                    format!("{} returned {}", self.url, response.status()),
                    elapsed_ms(start),
                ));
            }
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Nginx,
                    format!("{}: {e}", self.url),
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let Some(status) = parse_stub_status(&body) else {
            // Pointing at the site root instead of the status location is the
            // usual cause, and it deserves a message that says so.
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Nginx,
                format!(
                    "{} did not return stub_status output — check the URL points at a \
                     location with `stub_status;` enabled",
                    self.url
                ),
                latency,
            ));
        };

        // Counters are cumulative since worker start, so a rate needs a
        // previous sample. The first poll after startup has none and reports
        // no rate rather than a fabricated zero.
        let now = Utc::now();
        let mut previous = self.previous.lock().await;
        let rates = previous.and_then(|last| {
            let seconds = (now - last.at).num_milliseconds() as f64 / 1000.0;
            if seconds <= 0.0 {
                return None;
            }
            // A counter that went backwards means nginx was reloaded and the
            // workers reset; a negative rate would be nonsense.
            if status.requests < last.status.requests {
                return None;
            }
            Some((
                (status.requests - last.status.requests) / seconds,
                (status.handled - last.status.handled) / seconds,
            ))
        });
        *previous = Some(Sample { status, at: now });
        drop(previous);

        let dropped_total = dropped(&status);
        let health = if dropped_total > 0.0 {
            Health::Degraded
        } else if status.active >= self.connection_warn {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if dropped_total > 0.0 {
            format!("{dropped_total:.0} connections accepted but never handled")
        } else if status.active >= self.connection_warn {
            format!("{:.0} active connections", status.active)
        } else {
            match rates {
                Some((rps, _)) => format!("{:.0} active · {rps:.1} req/sec", status.active),
                None => format!("{:.0} active connections", status.active),
            }
        };

        let mut metrics = vec![
            Metric::new("active", "Active connections", status.active, MetricUnit::Count)
                .warn_above(self.connection_warn),
            Metric::new("reading", "Reading", status.reading, MetricUnit::Count),
            Metric::new("writing", "Writing", status.writing, MetricUnit::Count),
            Metric::new("waiting", "Waiting", status.waiting, MetricUnit::Count),
            Metric::new("requests_total", "Requests", status.requests, MetricUnit::Count),
            Metric::new("dropped", "Dropped connections", dropped_total, MetricUnit::Count)
                .warn_above(0.0),
        ];
        if let Some((rps, cps)) = rates {
            metrics.push(Metric::new("request_rate", "Requests/sec", rps, MetricUnit::PerSecond));
            metrics.push(Metric::new(
                "connection_rate",
                "Connections/sec",
                cps,
                MetricUnit::PerSecond,
            ));
        }

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Nginx,
            health,
            summary: summary.clone(),
            metrics,
            sections: vec![
                Section::new(
                    "connections",
                    "Connections",
                    vec![
                        ColumnSpec::text("url", "Status URL").primary(),
                        ColumnSpec::metric("active", "Active", MetricUnit::Count),
                        ColumnSpec::metric("reading", "Reading", MetricUnit::Count),
                        ColumnSpec::metric("writing", "Writing", MetricUnit::Count),
                        ColumnSpec::metric("waiting", "Waiting", MetricUnit::Count),
                    ],
                )
                .rows(vec![Row::new("connections", &self.url, health)
                    .summary(&summary)
                    .cell("url", CellValue::text(&self.url))
                    .cell("active", CellValue::number(status.active))
                    .cell("reading", CellValue::number(status.reading))
                    .cell("writing", CellValue::number(status.writing))
                    .cell("waiting", CellValue::number(status.waiting))])
                .priority(10),
                Section::new(
                    "throughput",
                    "Throughput",
                    vec![
                        ColumnSpec::text("metric", "Counter").primary(),
                        ColumnSpec::metric("total", "Total", MetricUnit::Count),
                        ColumnSpec::metric("rate", "Per second", MetricUnit::PerSecond),
                    ],
                )
                .rows(vec![
                    Row::new("accepts", "Accepted", Health::Healthy)
                        .summary("connections accepted")
                        .cell("metric", CellValue::text("Accepted"))
                        .cell("total", CellValue::number(status.accepts))
                        .cell("rate", CellValue::Null),
                    Row::new(
                        "handled",
                        "Handled",
                        if dropped_total > 0.0 { Health::Degraded } else { Health::Healthy },
                    )
                    .summary(if dropped_total > 0.0 {
                        format!("{dropped_total:.0} dropped")
                    } else {
                        "all accepted connections handled".to_string()
                    })
                    .cell("metric", CellValue::text("Handled"))
                    .cell("total", CellValue::number(status.handled))
                    .cell(
                        "rate",
                        match rates {
                            Some((_, cps)) => CellValue::number(cps),
                            None => CellValue::Null,
                        },
                    ),
                    Row::new("requests", "Requests", Health::Healthy)
                        .summary("requests served")
                        .cell("metric", CellValue::text("Requests"))
                        .cell("total", CellValue::number(status.requests))
                        .cell(
                            "rate",
                            match rates {
                                Some((rps, _)) => CellValue::number(rps),
                                None => CellValue::Null,
                            },
                        ),
                ])
                .priority(15),
            ],
            checked_at: now,
            latency_ms: latency,
            error: None,
        })
    }

    /// stub_status exposes counters, not messages. Nginx's access and error
    /// logs are files on the host, which is a different connection entirely —
    /// so this reports nothing rather than inventing lines.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Active connections: 43 \n\
server accepts handled requests\n\
 8129 8129 21344 \n\
Reading: 0 Writing: 3 Waiting: 40 \n";

    #[test]
    fn parses_stub_status() {
        let status = parse_stub_status(SAMPLE).expect("parse");
        assert_eq!(status.active, 43.0);
        assert_eq!(status.accepts, 8129.0);
        assert_eq!(status.handled, 8129.0);
        assert_eq!(status.requests, 21344.0);
        assert_eq!(status.reading, 0.0);
        assert_eq!(status.writing, 3.0);
        assert_eq!(status.waiting, 40.0);
    }

    #[test]
    fn detects_dropped_connections() {
        // accepts > handled is nginx running out of worker slots or file
        // descriptors. It is the one genuinely alarming thing stub_status says,
        // and it is invisible unless the two counters are compared.
        let mut status = parse_stub_status(SAMPLE).expect("parse");
        assert_eq!(dropped(&status), 0.0);
        status.accepts = 8200.0;
        assert_eq!(dropped(&status), 71.0);
    }

    #[test]
    fn dropped_never_goes_negative() {
        // handled can briefly exceed accepts across a counter read.
        let mut status = parse_stub_status(SAMPLE).expect("parse");
        status.handled = status.accepts + 5.0;
        assert_eq!(dropped(&status), 0.0);
    }

    #[test]
    fn rejects_output_that_is_not_stub_status() {
        // Pointing at the site root is the usual misconfiguration, and it must
        // not parse into a snapshot full of zeroes.
        assert!(parse_stub_status("<html><body>Welcome to nginx!</body></html>").is_none());
        assert!(parse_stub_status("").is_none());
    }

    #[test]
    fn requires_the_totals_line() {
        // Active connections alone is not enough to build a snapshot.
        assert!(parse_stub_status("Active connections: 3\n").is_none());
    }

    #[test]
    fn tolerates_missing_reading_writing_line() {
        let trimmed = "Active connections: 7\nserver accepts handled requests\n 1 2 3\n";
        let status = parse_stub_status(trimmed).expect("parse");
        assert_eq!(status.active, 7.0);
        assert_eq!(status.waiting, 0.0);
    }
}
