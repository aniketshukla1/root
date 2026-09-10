//! HAProxy monitor, via the stats page in CSV mode.
//!
//! `GET /stats;csv` returns one row per proxy object, with a `#` comment line
//! naming the columns. Each proxy contributes a FRONTEND row, a BACKEND row,
//! and one row per server behind it.
//!
//! Two things are worth reading carefully, and both are easy to get wrong.
//!
//! A BACKEND row says `UP` while any single server behind it is up. That is
//! the correct answer to "can this backend serve", and the wrong answer to
//! "is this backend healthy": eleven of twelve servers down is `UP`, and it is
//! also an incident. So server rows are counted rather than trusting the
//! aggregate, and a backend running on its last server says so.
//!
//! And `qcur` — requests queued waiting for a free server — is the number that
//! moves first. Errors and timeouts are what a saturated backend produces
//! *after* the queue has been full for a while; the queue itself is visible
//! immediately, while there is still time to do something about it. It is the
//! single most useful column on the page.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct HaproxyCollector {
    client: reqwest::Client,
    url: String,
    username: Option<String>,
    secret: Option<String>,
    /// Queued requests on one backend above which the proxy is degraded.
    queue_warn: f64,
    profile_id: String,
    profile_name: String,
}

/// One row of the stats CSV, reduced to the columns worth having.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatRow {
    pub proxy: String,
    pub service: String,
    /// FRONTEND, BACKEND, or a server name.
    pub kind: RowKind,
    pub status: String,
    pub sessions: f64,
    pub session_limit: Option<f64>,
    pub queued: f64,
    pub errors: f64,
    pub check_status: Option<String>,
    /// Seconds in the current state.
    pub last_change: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKind {
    Frontend,
    Backend,
    Server,
}

impl StatRow {
    /// Whether this row is serving.
    ///
    /// HAProxy reports several flavours of not-serving and they are not the
    /// same event. `MAINT` is somebody's decision and should never page;
    /// `DOWN` is a failed health check and should. `DRAIN` and `NOLB` are
    /// deliberate removals from load balancing that finish in-flight work, so
    /// they read as degraded rather than down.
    pub fn health(&self) -> Health {
        let status = self.status.split_whitespace().next().unwrap_or("");
        match status {
            "UP" | "OPEN" => Health::Healthy,
            "DOWN" => Health::Down,
            "MAINT" | "DRAIN" | "NOLB" => Health::Degraded,
            // "no check" means the server is in rotation with no health check
            // configured. It is serving, and root cannot say more than that.
            "" | "no" => Health::Unknown,
            _ => Health::Unknown,
        }
    }

    /// Deliberately out of rotation rather than broken.
    pub fn is_administrative(&self) -> bool {
        let status = self.status.split_whitespace().next().unwrap_or("");
        matches!(status, "MAINT" | "DRAIN")
    }
}

impl HaproxyCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim();
        if raw.is_empty() {
            return Err(AppError::Config(
                "HAProxy connections need the stats URL, e.g. http://haproxy.internal:8404/stats"
                    .into(),
            ));
        }

        // `;csv` is what turns the HTML page into something parseable, and
        // forgetting it is the most likely configuration mistake. Appending it
        // is safe and idempotent, so the operator does not have to know.
        let url = if raw.contains(";csv") {
            raw.to_string()
        } else {
            format!("{raw};csv;norefresh")
        };

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let queue_warn = profile
            .options
            .get("queueWarn")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0);

        Ok(HaproxyCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            url,
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            queue_warn,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }
}

/// Parse the stats CSV.
///
/// Split on commas without quote handling, because HAProxy configuration does
/// not permit commas or whitespace in proxy or server names — the values in
/// those columns cannot contain a separator. Columns are located by the header
/// line rather than by position: the column set has grown across HAProxy
/// versions and reading `scur` by index would silently read something else on
/// a different build.
pub(crate) fn parse_stats_csv(body: &str) -> Vec<StatRow> {
    let mut lines = body.lines().filter(|l| !l.trim().is_empty());

    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let columns: BTreeMap<&str, usize> = header
        .trim_start_matches('#')
        .split(',')
        .enumerate()
        .map(|(i, name)| (name.trim(), i))
        .collect();

    let index = |name: &str| columns.get(name).copied();
    let (Some(pxname), Some(svname)) = (index("pxname"), index("svname")) else {
        return Vec::new();
    };

    lines
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let cells: Vec<&str> = line.split(',').collect();
            let get = |idx: Option<usize>| idx.and_then(|i| cells.get(i)).copied().unwrap_or("");
            let number = |idx: Option<usize>| get(idx).parse::<f64>().ok();

            let proxy = get(Some(pxname));
            let service = get(Some(svname));
            if proxy.is_empty() || service.is_empty() {
                return None;
            }

            let kind = match service {
                "FRONTEND" => RowKind::Frontend,
                "BACKEND" => RowKind::Backend,
                _ => RowKind::Server,
            };

            Some(StatRow {
                proxy: proxy.to_string(),
                service: service.to_string(),
                kind,
                status: get(index("status")).to_string(),
                sessions: number(index("scur")).unwrap_or(0.0),
                session_limit: number(index("slim")),
                queued: number(index("qcur")).unwrap_or(0.0),
                // Connection and response errors both mean the backend failed
                // the request; which one it was matters less than that it did.
                errors: number(index("econ")).unwrap_or(0.0)
                    + number(index("eresp")).unwrap_or(0.0),
                check_status: {
                    let value = get(index("check_status"));
                    (!value.is_empty()).then(|| value.to_string())
                },
                last_change: number(index("lastchg")),
            })
        })
        .collect()
}

/// Servers behind one backend, counted by what they are actually doing.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct BackendTally {
    pub up: usize,
    pub down: usize,
    pub administrative: usize,
    pub total: usize,
}

pub(crate) fn tally(rows: &[StatRow], proxy: &str) -> BackendTally {
    let mut out = BackendTally::default();
    for row in rows.iter().filter(|r| r.kind == RowKind::Server && r.proxy == proxy) {
        out.total += 1;
        if row.is_administrative() {
            out.administrative += 1;
        } else if row.health() == Health::Down {
            out.down += 1;
        } else {
            out.up += 1;
        }
    }
    out
}

#[async_trait]
impl Collector for HaproxyCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Haproxy
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
                    ServiceKind::Haproxy,
                    format!("{} returned {}", self.url, response.status()),
                    elapsed_ms(start),
                ));
            }
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Haproxy,
                    format!("{}: {e}", self.url),
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let rows = parse_stats_csv(&body);
        if rows.is_empty() {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Haproxy,
                format!(
                    "{} returned no stats rows — check the URL points at the stats page and \
                     that the socket has `stats enable`",
                    self.url
                ),
                latency,
            ));
        }

        let frontends: Vec<&StatRow> = rows.iter().filter(|r| r.kind == RowKind::Frontend).collect();
        let backends: Vec<&StatRow> = rows.iter().filter(|r| r.kind == RowKind::Backend).collect();

        let queued: f64 = backends.iter().map(|b| b.queued).sum();
        let sessions: f64 = frontends.iter().map(|f| f.sessions).sum();

        // A backend with nothing serving is the outage; a backend down to its
        // last server is the warning that one is coming.
        let mut dead: Vec<&str> = Vec::new();
        let mut last_server: Vec<&str> = Vec::new();
        for backend in &backends {
            let counts = tally(&rows, &backend.proxy);
            if counts.total == 0 {
                continue;
            }
            if counts.up == 0 {
                dead.push(&backend.proxy);
            } else if counts.up == 1 && counts.total > 1 {
                last_server.push(&backend.proxy);
            }
        }

        let health = if !dead.is_empty() {
            Health::Down
        } else if !last_server.is_empty() || queued >= self.queue_warn {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if !dead.is_empty() {
            format!(
                "{} with no servers up: {}",
                if dead.len() == 1 { "backend" } else { "backends" },
                dead.join(", ")
            )
        } else if queued >= self.queue_warn {
            format!("{queued:.0} requests queued waiting for a free server")
        } else if !last_server.is_empty() {
            format!("{} running on a single server", last_server.join(", "))
        } else {
            format!(
                "{} frontends · {} backends · {sessions:.0} sessions",
                frontends.len(),
                backends.len()
            )
        };

        let metrics = vec![
            Metric::new("sessions", "Sessions", sessions, MetricUnit::Count),
            Metric::new("queued", "Queued requests", queued, MetricUnit::Count)
                .warn_above(self.queue_warn),
            Metric::new("frontends", "Frontends", frontends.len() as f64, MetricUnit::Count),
            Metric::new("backends", "Backends", backends.len() as f64, MetricUnit::Count),
            Metric::new(
                "servers_down",
                "Servers down",
                rows.iter()
                    .filter(|r| r.kind == RowKind::Server && !r.is_administrative())
                    .filter(|r| r.health() == Health::Down)
                    .count() as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
        ];

        let backend_rows: Vec<Row> = backends
            .iter()
            .map(|backend| {
                let counts = tally(&rows, &backend.proxy);
                let backend_health = if counts.total > 0 && counts.up == 0 {
                    Health::Down
                } else if backend.queued >= self.queue_warn
                    || (counts.up == 1 && counts.total > 1)
                    || counts.down > 0
                {
                    Health::Degraded
                } else {
                    backend.health()
                };
                Row::new(
                    format!("backend:{}", backend.proxy),
                    backend.proxy.clone(),
                    backend_health,
                )
                .summary(format!(
                    "{} of {} servers up{}",
                    counts.up,
                    counts.total,
                    if counts.administrative > 0 {
                        format!(", {} in maintenance", counts.administrative)
                    } else {
                        String::new()
                    }
                ))
                .cell("backend", CellValue::text(backend.proxy.clone()))
                .cell("state", CellValue::status(backend_health))
                .cell("servers", CellValue::text(format!("{}/{}", counts.up, counts.total)))
                .cell("sessions", CellValue::number(backend.sessions))
                .cell("queued", CellValue::number(backend.queued))
                .cell("errors", CellValue::number(backend.errors))
            })
            .collect();

        let server_rows: Vec<Row> = rows
            .iter()
            .filter(|r| r.kind == RowKind::Server)
            .map(|server| {
                Row::new(
                    format!("server:{}:{}", server.proxy, server.service),
                    format!("{}/{}", server.proxy, server.service),
                    server.health(),
                )
                .summary(match &server.check_status {
                    Some(check) => format!("{} · last check {check}", server.status),
                    None => server.status.clone(),
                })
                .cell("server", CellValue::text(server.service.clone()))
                .cell("backend", CellValue::text(server.proxy.clone()))
                .cell("state", CellValue::status(server.health()))
                .cell("sessions", CellValue::number(server.sessions))
                .cell(
                    "for",
                    match server.last_change {
                        Some(secs) => CellValue::duration(secs),
                        None => CellValue::text("—"),
                    },
                )
            })
            .collect();

        let frontend_rows: Vec<Row> = frontends
            .iter()
            .map(|frontend| {
                Row::new(
                    format!("frontend:{}", frontend.proxy),
                    frontend.proxy.clone(),
                    frontend.health(),
                )
                .cell("frontend", CellValue::text(frontend.proxy.clone()))
                .cell("state", CellValue::status(frontend.health()))
                .cell(
                    "sessions",
                    CellValue::gauge(frontend.sessions, frontend.session_limit),
                )
            })
            .collect();

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Haproxy,
            health,
            summary,
            metrics,
            sections: vec![
                Section::new(
                    "backends",
                    "Backends",
                    vec![
                        ColumnSpec::text("backend", "Backend").primary(),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::text("servers", "Servers up"),
                        ColumnSpec::metric("sessions", "Sessions", MetricUnit::Count),
                        ColumnSpec::metric("queued", "Queued", MetricUnit::Count),
                        ColumnSpec::metric("errors", "Errors", MetricUnit::Count),
                    ],
                )
                .rows(backend_rows)
                .tracked()
                .empty_text("No backends are configured.")
                .priority(10),
                Section::new(
                    "servers",
                    "Servers",
                    vec![
                        ColumnSpec::text("server", "Server").primary(),
                        ColumnSpec::text("backend", "Backend"),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::metric("sessions", "Sessions", MetricUnit::Count),
                        ColumnSpec::duration("for", "In state for"),
                    ],
                )
                .rows(server_rows)
                .tracked()
                .empty_text("No servers are configured behind any backend.")
                .priority(20),
                Section::new(
                    "frontends",
                    "Frontends",
                    vec![
                        ColumnSpec::text("frontend", "Frontend").primary(),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::gauge("sessions", "Sessions", MetricUnit::Count),
                    ],
                )
                .rows(frontend_rows)
                .tracked()
                .priority(30),
            ],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// The stats page is a gauge. HAProxy's logs go to syslog, which is not
    /// something this collector can reach.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "# pxname,svname,qcur,qmax,scur,smax,slim,stot,bin,bout,dreq,dresp,ereq,econ,eresp,wretr,wredis,status,weight,act,bck,chkfail,chkdown,lastchg,downtime,qlimit,pid,iid,sid,throttle,lbtot,tracked,type,rate,rate_lim,rate_max,check_status";

    fn csv(rows: &[String]) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        for row in rows {
            out.push_str(row);
            out.push('\n');
        }
        out
    }

    /// pxname,svname,qcur,...,scur(4),...,slim(6),...,status(17),...,lastchg(22),...,check_status(35)
    fn row(proxy: &str, service: &str, qcur: &str, scur: &str, status: &str) -> String {
        let mut cells = vec![""; 36];
        cells[0] = proxy;
        cells[1] = service;
        cells[2] = qcur;
        cells[4] = scur;
        cells[6] = "2000";
        cells[17] = status;
        cells[22] = "600";
        cells[35] = "L4OK";
        cells.join(",")
    }

    #[test]
    fn locates_columns_by_header_not_position() {
        let body = csv(&[row("web", "FRONTEND", "0", "12", "OPEN")]);
        let parsed = parse_stats_csv(&body);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kind, RowKind::Frontend);
        assert_eq!(parsed[0].sessions, 12.0);
        assert_eq!(parsed[0].session_limit, Some(2000.0));
        assert_eq!(parsed[0].status, "OPEN");
    }

    /// The point of counting servers rather than trusting the BACKEND row.
    #[test]
    fn a_backend_is_up_while_almost_every_server_is_down() {
        let body = csv(&[
            row("api", "BACKEND", "0", "3", "UP"),
            row("api", "web1", "0", "3", "UP"),
            row("api", "web2", "0", "0", "DOWN"),
            row("api", "web3", "0", "0", "DOWN"),
        ]);
        let rows = parse_stats_csv(&body);
        let counts = tally(&rows, "api");
        assert_eq!(counts.up, 1);
        assert_eq!(counts.down, 2);
        assert_eq!(counts.total, 3);

        let backend = rows.iter().find(|r| r.kind == RowKind::Backend).unwrap();
        assert_eq!(
            backend.health(),
            Health::Healthy,
            "HAProxy says UP, which is why the servers have to be counted",
        );
    }

    /// Somebody's decision is not an incident.
    #[test]
    fn maintenance_is_not_counted_as_down() {
        let body = csv(&[
            row("api", "web1", "0", "3", "UP"),
            row("api", "web2", "0", "0", "MAINT"),
        ]);
        let rows = parse_stats_csv(&body);
        let counts = tally(&rows, "api");
        assert_eq!(counts.up, 1);
        assert_eq!(counts.down, 0);
        assert_eq!(counts.administrative, 1);
    }

    /// DRAIN and NOLB finish in-flight work; they are a removal, not a failure.
    #[test]
    fn draining_reads_as_degraded_and_down_reads_as_down() {
        let body = csv(&[
            row("api", "a", "0", "1", "DRAIN"),
            row("api", "b", "0", "0", "NOLB"),
            row("api", "c", "0", "0", "DOWN"),
        ]);
        let rows = parse_stats_csv(&body);
        assert_eq!(rows[0].health(), Health::Degraded);
        assert_eq!(rows[1].health(), Health::Degraded);
        assert_eq!(rows[2].health(), Health::Down);
    }

    /// A server in rotation with no health check is serving, and root cannot
    /// honestly say more than that.
    #[test]
    fn a_server_with_no_check_is_unknown_rather_than_healthy() {
        let body = csv(&[row("api", "web1", "0", "3", "no check")]);
        let rows = parse_stats_csv(&body);
        assert_eq!(rows[0].health(), Health::Unknown);
    }

    #[test]
    fn errors_add_connection_and_response_failures_together() {
        let mut cells = vec![""; 36];
        cells[0] = "api";
        cells[1] = "BACKEND";
        cells[13] = "4"; // econ
        cells[14] = "7"; // eresp
        cells[17] = "UP";
        let body = csv(&[cells.join(",")]);
        assert_eq!(parse_stats_csv(&body)[0].errors, 11.0);
    }

    #[test]
    fn the_html_stats_page_yields_no_rows() {
        assert!(parse_stats_csv("<html><body>HAProxy Statistics</body></html>").is_empty());
    }

    #[test]
    fn an_empty_body_is_not_a_panic() {
        assert!(parse_stats_csv("").is_empty());
    }
}
