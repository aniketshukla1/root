//! Apache httpd monitor, via `mod_status` in machine-readable mode.
//!
//! `/server-status?auto` returns plain `Key: Value` lines, and one of them is
//! not a value at all:
//!
//! ```text
//! Total Accesses: 421337
//! Total kBytes: 1284915
//! CPULoad: .184
//! Uptime: 84213
//! ReqPerSec: 5.00
//! BusyWorkers: 118
//! IdleWorkers: 10
//! Scoreboard: WWWW_WW.KKW__W.................
//! ```
//!
//! The scoreboard is the interesting one, and it is why this collector exists
//! rather than pointing the generic HTTP check at the same URL.
//!
//! Apache's characteristic failure is not slowness, it is running out of
//! workers: every slot busy, new requests queueing in the kernel backlog, and
//! response times cliff-edging rather than degrading. `BusyWorkers` alone
//! cannot see that coming, because 118 busy is fine at MaxRequestWorkers 400
//! and fatal at 128. The scoreboard carries the denominator — one character
//! per slot, with `.` for a slot that has not been opened — so saturation is
//! knowable here and nowhere else in the response.
//!
//! The states are worth keeping apart for the same reason. A wall of `W`
//! (writing) is a server under load; a wall of `R` (reading) is a server being
//! held open by slow clients, which is a different problem with a different
//! fix. `G` — gracefully finishing — piling up means a reload is stuck behind
//! long-running requests.

use std::collections::BTreeMap;
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

/// What each scoreboard character means, in Apache's own order.
///
/// `.` is deliberately not here: an unopened slot is not a worker in a state,
/// it is the headroom left, and counting it as a state would hide the one
/// number this whole module exists to produce.
const SCOREBOARD: &[(char, &str)] = &[
    ('_', "Waiting"),
    ('S', "Starting"),
    ('R', "Reading request"),
    ('W', "Sending reply"),
    ('K', "Keepalive"),
    ('D', "DNS lookup"),
    ('C', "Closing"),
    ('L', "Logging"),
    ('G', "Finishing gracefully"),
    ('I', "Idle cleanup"),
];

pub struct ApacheCollector {
    client: reqwest::Client,
    url: String,
    username: Option<String>,
    secret: Option<String>,
    /// Percent of worker slots in use above which the server is degraded.
    saturation_warn: f64,
    profile_id: String,
    profile_name: String,
    previous: Mutex<Option<Sample>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerStatus {
    pub busy_workers: f64,
    pub idle_workers: f64,
    pub total_accesses: f64,
    pub total_kbytes: f64,
    pub cpu_load: Option<f64>,
    pub uptime_secs: Option<f64>,
    pub req_per_sec: Option<f64>,
    /// Counts per state, keyed by the label. Empty when no scoreboard was sent.
    pub scoreboard: BTreeMap<&'static str, f64>,
    /// Slots never opened. `None` when there was no scoreboard to count.
    pub open_slots: Option<f64>,
}

impl ServerStatus {
    /// Worker slots in use, as a percentage of every slot Apache will ever
    /// open.
    ///
    /// `None` without a scoreboard: busy over busy-plus-idle would read 92%
    /// on a server with three hundred untouched slots, which is the exact
    /// false alarm this collector exists to avoid.
    pub fn saturation(&self) -> Option<f64> {
        let open = self.open_slots?;
        let total = self.busy_workers + self.idle_workers + open;
        (total > 0.0).then(|| (self.busy_workers / total) * 100.0)
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    accesses: f64,
    kbytes: f64,
    at: DateTime<Utc>,
}

impl ApacheCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let url = profile.endpoint.trim().to_string();
        if url.is_empty() {
            return Err(AppError::Config(
                "Apache connections need the server-status URL, e.g. \
                 http://apache.internal/server-status?auto"
                    .into(),
            ));
        }

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let saturation_warn = profile
            .options
            .get("saturationWarn")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(85.0);

        Ok(ApacheCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            url,
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            saturation_warn,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            previous: Mutex::new(None),
        })
    }
}

/// Parse `?auto` output.
///
/// Returns `None` when the body carries neither worker count, which is what
/// pointing at the HTML status page or at the site root looks like.
pub(crate) fn parse_server_status(body: &str) -> Option<ServerStatus> {
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    for line in body.lines() {
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(key.trim(), value.trim());
        }
    }

    let number = |key: &str| fields.get(key).and_then(|v| v.parse::<f64>().ok());

    let busy_workers = number("BusyWorkers");
    let idle_workers = number("IdleWorkers");
    if busy_workers.is_none() && idle_workers.is_none() {
        return None;
    }

    let mut scoreboard = BTreeMap::new();
    let mut open_slots = None;
    if let Some(board) = fields.get("Scoreboard") {
        let mut open = 0.0;
        for slot in board.chars() {
            if slot == '.' {
                open += 1.0;
                continue;
            }
            if let Some((_, label)) = SCOREBOARD.iter().find(|(c, _)| *c == slot) {
                *scoreboard.entry(*label).or_insert(0.0) += 1.0;
            }
        }
        open_slots = Some(open);
    }

    Some(ServerStatus {
        busy_workers: busy_workers.unwrap_or(0.0),
        idle_workers: idle_workers.unwrap_or(0.0),
        total_accesses: number("Total Accesses").unwrap_or(0.0),
        total_kbytes: number("Total kBytes").unwrap_or(0.0),
        // `.184` — no leading zero — is valid Apache output and valid Rust
        // float parsing, so this needs no special case. Noted because it
        // looks like it should.
        cpu_load: number("CPULoad"),
        uptime_secs: number("Uptime"),
        req_per_sec: number("ReqPerSec"),
        scoreboard,
        open_slots,
    })
}

#[async_trait]
impl Collector for ApacheCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Apache
    }

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
                    ServiceKind::Apache,
                    format!("{} returned {}", self.url, response.status()),
                    elapsed_ms(start),
                ));
            }
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Apache,
                    format!("{}: {e}", self.url),
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let Some(status) = parse_server_status(&body) else {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Apache,
                format!(
                    "{} did not return mod_status output — the URL needs the `?auto` query, \
                     which is what makes it machine readable",
                    self.url
                ),
                latency,
            ));
        };

        // Totals are cumulative since httpd started, so a rate needs a previous
        // sample. The first poll reports none rather than a fabricated zero,
        // and a counter that went backwards means a restart, not a negative
        // rate.
        let now = Utc::now();
        let mut previous = self.previous.lock().await;
        let rates = previous.and_then(|last| {
            let seconds = (now - last.at).num_milliseconds() as f64 / 1000.0;
            if seconds <= 0.0 || status.total_accesses < last.accesses {
                return None;
            }
            Some((
                (status.total_accesses - last.accesses) / seconds,
                ((status.total_kbytes - last.kbytes) * 1024.0) / seconds,
            ))
        });
        *previous = Some(Sample {
            accesses: status.total_accesses,
            kbytes: status.total_kbytes,
            at: now,
        });
        drop(previous);

        let saturation = status.saturation();
        let no_headroom = status.open_slots == Some(0.0) && status.idle_workers == 0.0;

        let health = if no_headroom {
            // Every slot open and every worker busy: the next request waits in
            // the kernel backlog. This is the cliff, not the slope.
            Health::Down
        } else if saturation.is_some_and(|s| s >= self.saturation_warn) {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if no_headroom {
            format!(
                "no free workers — all {:.0} slots busy",
                status.busy_workers
            )
        } else if let Some(pct) = saturation {
            format!(
                "{:.0} of {:.0} worker slots busy · {pct:.0}%",
                status.busy_workers,
                status.busy_workers + status.idle_workers + status.open_slots.unwrap_or(0.0),
            )
        } else {
            // No scoreboard: say what is known and not what is not.
            format!(
                "{:.0} busy · {:.0} idle workers (no scoreboard, so no saturation)",
                status.busy_workers, status.idle_workers
            )
        };

        let mut metrics = vec![
            Metric::new("busy_workers", "Busy workers", status.busy_workers, MetricUnit::Count),
            Metric::new("idle_workers", "Idle workers", status.idle_workers, MetricUnit::Count),
        ];
        if let Some(pct) = saturation {
            metrics.push(
                Metric::new("saturation", "Worker saturation", pct, MetricUnit::Percent)
                    .warn_above(self.saturation_warn),
            );
        }
        if let Some(open) = status.open_slots {
            metrics.push(Metric::new("open_slots", "Unopened slots", open, MetricUnit::Count));
        }
        if let Some(load) = status.cpu_load {
            metrics.push(Metric::new("cpu_load", "CPU load", load, MetricUnit::Percent));
        }
        if let Some(uptime) = status.uptime_secs {
            metrics.push(Metric::new("uptime", "Uptime", uptime, MetricUnit::Seconds));
        }
        match rates {
            Some((rps, bps)) => {
                metrics.push(Metric::new("request_rate", "Requests/sec", rps, MetricUnit::PerSecond));
                metrics.push(Metric::new("byte_rate", "Bytes/sec", bps, MetricUnit::PerSecond));
            }
            None => {
                // ReqPerSec is Apache's own lifetime average, which is a
                // different number from the rate right now and is only worth
                // showing while there is no rate to show.
                if let Some(avg) = status.req_per_sec {
                    metrics.push(Metric::new(
                        "request_rate_lifetime",
                        "Requests/sec (lifetime avg)",
                        avg,
                        MetricUnit::PerSecond,
                    ));
                }
            }
        }

        let mut sections = Vec::new();
        if !status.scoreboard.is_empty() {
            let total_slots =
                status.busy_workers + status.idle_workers + status.open_slots.unwrap_or(0.0);
            let mut rows: Vec<Row> = status
                .scoreboard
                .iter()
                .map(|(label, count)| {
                    Row::new(
                        label.to_lowercase().replace(' ', "_"),
                        *label,
                        // A state is a state, not a fault. Only the aggregate
                        // above decides health; colouring "Sending reply" red
                        // because there are a lot of them would be nonsense.
                        Health::Healthy,
                    )
                    .cell("state", CellValue::text(*label))
                    .cell("workers", CellValue::number(*count))
                    .cell(
                        "share",
                        CellValue::gauge(*count, (total_slots > 0.0).then_some(total_slots)),
                    )
                })
                .collect();
            if let Some(open) = status.open_slots {
                rows.push(
                    Row::new("open", "Unopened slots", Health::Healthy)
                        .summary("Headroom — slots httpd has not needed to start yet")
                        .cell("state", CellValue::text("Unopened slots"))
                        .cell("workers", CellValue::number(open))
                        .cell(
                            "share",
                            CellValue::gauge(open, (total_slots > 0.0).then_some(total_slots)),
                        ),
                );
            }

            sections.push(
                Section::new(
                    "scoreboard",
                    "Worker scoreboard",
                    vec![
                        ColumnSpec::text("state", "State").primary(),
                        ColumnSpec::metric("workers", "Workers", MetricUnit::Count),
                        ColumnSpec::gauge("share", "Share", MetricUnit::Count),
                    ],
                )
                .rows(rows)
                // Bounded and long-lived: eleven states, the same eleven every
                // poll, so keeping history for them is worth it.
                .tracked()
                .priority(10),
            );
        }

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Apache,
            health,
            summary,
            metrics,
            sections,
            checked_at: now,
            latency_ms: latency,
            error: None,
        })
    }

    /// mod_status is a gauge, not a journal. Apache's logs are files on the
    /// host and reading them needs an agent this does not have.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Total Accesses: 421337\n\
                          Total kBytes: 1284915\n\
                          CPULoad: .184\n\
                          Uptime: 84213\n\
                          ReqPerSec: 5.00\n\
                          BytesPerSec: 15621\n\
                          BusyWorkers: 6\n\
                          IdleWorkers: 4\n\
                          Scoreboard: WWWWWW____..........\n";

    #[test]
    fn parses_the_auto_payload() {
        let status = parse_server_status(SAMPLE).expect("parsed");
        assert_eq!(status.busy_workers, 6.0);
        assert_eq!(status.idle_workers, 4.0);
        assert_eq!(status.total_accesses, 421_337.0);
        assert_eq!(status.cpu_load, Some(0.184));
        assert_eq!(status.scoreboard.get("Sending reply"), Some(&6.0));
        assert_eq!(status.scoreboard.get("Waiting"), Some(&4.0));
        assert_eq!(status.open_slots, Some(10.0));
    }

    /// The whole reason this is not the generic HTTP check: 6 busy of 10
    /// started workers is 60%, but of 20 slots it is 30%, and only the second
    /// number tells you whether the next request has somewhere to go.
    #[test]
    fn saturation_counts_slots_that_were_never_opened() {
        let status = parse_server_status(SAMPLE).expect("parsed");
        assert_eq!(status.saturation(), Some(30.0));
    }

    #[test]
    fn saturation_is_absent_without_a_scoreboard() {
        let status = parse_server_status("BusyWorkers: 6\nIdleWorkers: 4\n").expect("parsed");
        assert_eq!(status.open_slots, None);
        assert_eq!(
            status.saturation(),
            None,
            "busy over busy+idle would read 60% and mean nothing",
        );
    }

    #[test]
    fn every_slot_busy_is_the_cliff() {
        let status = parse_server_status(
            "BusyWorkers: 8\nIdleWorkers: 0\nScoreboard: WWWWWWWW\n",
        )
        .expect("parsed");
        assert_eq!(status.open_slots, Some(0.0));
        assert_eq!(status.saturation(), Some(100.0));
    }

    #[test]
    fn the_html_status_page_is_not_mistaken_for_the_auto_one() {
        assert!(parse_server_status("<html><body>Apache Server Status</body></html>").is_none());
    }

    #[test]
    fn unknown_scoreboard_characters_are_ignored_rather_than_guessed() {
        let status =
            parse_server_status("BusyWorkers: 1\nIdleWorkers: 0\nScoreboard: W?z.\n").expect("parsed");
        assert_eq!(status.scoreboard.get("Sending reply"), Some(&1.0));
        assert_eq!(status.scoreboard.len(), 1);
        assert_eq!(status.open_slots, Some(1.0));
    }
}
