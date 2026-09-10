//! Traefik monitor, via the API.
//!
//! `GET /api/overview` gives the counts, and `/api/http/routers` and
//! `/api/http/services` give the objects behind them.
//!
//! What makes this worth a collector rather than an HTTP check on the front
//! door is that Traefik has two independent ways to be broken, and only one of
//! them is visible from outside.
//!
//! A router can be `warning` or `critical` — pointing at a service that does
//! not exist, carrying a middleware that failed to build, holding a
//! certificate resolver that never resolved. Traefik itself is up, the port
//! answers, and every request to that route 404s. An uptime check on the proxy
//! reports green throughout, because the proxy genuinely is.
//!
//! And a service can be configured perfectly while every server behind it is
//! `DOWN`. That is the failure an uptime check does catch, eventually, as 502s
//! — after the fact. `serverStatus` says it immediately.
//!
//! So both are read: configuration health, which nothing else can see, and
//! backend health, which everything else sees late.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub struct TraefikCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    profile_id: String,
    profile_name: String,
}

/// Traefik also sends `online`, which is deliberately not taken.
///
/// `total - warning - critical` and `online` are two routes to the same number
/// only while Traefik has exactly these three states, and it has added states
/// before. Counting what is broken is the answer that survives a new one being
/// introduced; counting what is fine is the answer that quietly starts
/// including it.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Counts {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub warning: u64,
    #[serde(default)]
    pub critical: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Section1 {
    #[serde(default)]
    pub routers: Counts,
    #[serde(default)]
    pub services: Counts,
    #[serde(default)]
    pub middlewares: Counts,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Overview {
    #[serde(default)]
    pub http: Section1,
    #[serde(default)]
    pub tcp: Section1,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Router {
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub rule: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    /// Why it is not enabled. Present only when something failed to build.
    #[serde(default)]
    pub error: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Service {
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub provider: Option<String>,
    /// Server URL to "UP" or "DOWN". Absent for services with no health check.
    #[serde(default)]
    pub server_status: BTreeMap<String, String>,
}

impl Service {
    /// Servers reporting UP, and how many there are in total.
    ///
    /// `None` when Traefik published no `serverStatus` — that is a service
    /// without a health check, which is not the same as a service with no
    /// healthy servers, and must not be counted as an outage.
    pub fn servers(&self) -> Option<(usize, usize)> {
        if self.server_status.is_empty() {
            return None;
        }
        let up = self
            .server_status
            .values()
            .filter(|state| state.eq_ignore_ascii_case("UP"))
            .count();
        Some((up, self.server_status.len()))
    }
}

/// Traefik's own word for the state of a configuration object.
pub(crate) fn config_health(status: &str) -> Health {
    match status.to_ascii_lowercase().as_str() {
        "enabled" => Health::Healthy,
        "warning" => Health::Degraded,
        "disabled" => Health::Down,
        // Traefik has added states before; an unrecognised one is unknown
        // rather than assumed fine.
        _ => Health::Unknown,
    }
}

impl TraefikCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim().trim_end_matches('/');
        if raw.is_empty() {
            return Err(AppError::Config(
                "Traefik connections need the API base URL, e.g. http://traefik.internal:8080"
                    .into(),
            ));
        }
        // Accept both the dashboard base and the API path, so pasting either
        // out of a browser works.
        let base = raw.trim_end_matches("/api").to_string();

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        Ok(TraefikCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            base,
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> std::result::Result<T, String> {
        let url = format!("{}{path}", self.base);
        let request = match (&self.username, &self.secret) {
            (Some(user), secret) => self.client.get(&url).basic_auth(user, secret.as_ref()),
            _ => self.client.get(&url),
        };
        let response = request.send().await.map_err(|e| format!("{url}: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("{url} returned {}", response.status()));
        }
        response
            .json::<T>()
            .await
            .map_err(|e| format!("{url} sent a body root could not read: {e}"))
    }
}

#[async_trait]
impl Collector for TraefikCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Traefik
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let overview: Overview = match self.get("/api/overview").await {
            Ok(overview) => overview,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Traefik,
                    format!(
                        "{message} — the API needs `api.dashboard` or `api.insecure` enabled"
                    ),
                    elapsed_ms(start),
                ));
            }
        };

        // The object lists are a nicety on top of the counts. If they fail —
        // an older build, a stricter API surface — the overview still carries
        // the totals, so the poll degrades rather than failing.
        let routers: Vec<Router> = self.get("/api/http/routers").await.unwrap_or_default();
        let services: Vec<Service> = self.get("/api/http/services").await.unwrap_or_default();
        let latency = elapsed_ms(start);

        let broken_routers: Vec<&Router> = routers
            .iter()
            .filter(|r| config_health(&r.status) != Health::Healthy)
            .collect();

        // Only services Traefik actually health-checks can be judged.
        let starved: Vec<&Service> = services
            .iter()
            .filter(|s| matches!(s.servers(), Some((0, total)) if total > 0))
            .collect();
        let thinned: Vec<&Service> = services
            .iter()
            .filter(|s| matches!(s.servers(), Some((up, total)) if up > 0 && up < total))
            .collect();

        let health = if !starved.is_empty() {
            Health::Down
        } else if !broken_routers.is_empty()
            || !thinned.is_empty()
            || overview.http.routers.critical > 0
            || overview.http.services.critical > 0
        {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if !starved.is_empty() {
            format!(
                "{} with no servers up: {}",
                if starved.len() == 1 { "service" } else { "services" },
                starved.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
            )
        } else if !broken_routers.is_empty() {
            // The failure nothing outside can see, so it gets said plainly.
            format!(
                "{} not routing: {}",
                if broken_routers.len() == 1 { "router" } else { "routers" },
                broken_routers.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
            )
        } else if !thinned.is_empty() {
            format!(
                "{} running on fewer servers than configured",
                thinned.len()
            )
        } else {
            format!(
                "{} routers · {} services · {} middlewares",
                overview.http.routers.total, overview.http.services.total, overview.http.middlewares.total
            )
        };

        let metrics = vec![
            Metric::new("routers", "Routers", overview.http.routers.total as f64, MetricUnit::Count),
            Metric::new(
                "routers_broken",
                "Routers not routing",
                (overview.http.routers.warning + overview.http.routers.critical) as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new("services", "Services", overview.http.services.total as f64, MetricUnit::Count),
            Metric::new(
                "services_broken",
                "Services degraded",
                (overview.http.services.warning + overview.http.services.critical) as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new("tcp_routers", "TCP routers", overview.tcp.routers.total as f64, MetricUnit::Count),
        ];

        let router_rows: Vec<Row> = routers
            .iter()
            .map(|router| {
                let state = config_health(&router.status);
                Row::new(format!("router:{}", router.name), router.name.clone(), state)
                    .summary(if router.error.is_empty() {
                        router.rule.clone().unwrap_or_default()
                    } else {
                        router.error.join("; ")
                    })
                    .cell("router", CellValue::text(router.name.clone()))
                    .cell("state", CellValue::status(state))
                    .cell(
                        "service",
                        CellValue::text(router.service.clone().unwrap_or_else(|| "—".into())),
                    )
                    .cell(
                        "provider",
                        CellValue::text(router.provider.clone().unwrap_or_else(|| "—".into())),
                    )
                    .cell(
                        "rule",
                        CellValue::text(router.rule.clone().unwrap_or_else(|| "—".into())),
                    )
            })
            .collect();

        let service_rows: Vec<Row> = services
            .iter()
            .map(|service| {
                let state = match service.servers() {
                    Some((0, total)) if total > 0 => Health::Down,
                    Some((up, total)) if up < total => Health::Degraded,
                    Some(_) => Health::Healthy,
                    // No health check configured: Traefik is forwarding to it
                    // and has no opinion, so neither does root.
                    None => config_health(&service.status),
                };
                Row::new(format!("service:{}", service.name), service.name.clone(), state)
                    .summary(match service.servers() {
                        Some((up, total)) => format!("{up} of {total} servers up"),
                        None => "no health check configured".to_string(),
                    })
                    .cell("service", CellValue::text(service.name.clone()))
                    .cell("state", CellValue::status(state))
                    .cell(
                        "servers",
                        match service.servers() {
                            Some((up, total)) => CellValue::text(format!("{up}/{total}")),
                            None => CellValue::text("—"),
                        },
                    )
                    .cell(
                        "provider",
                        CellValue::text(service.provider.clone().unwrap_or_else(|| "—".into())),
                    )
            })
            .collect();

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Traefik,
            health,
            summary,
            metrics,
            sections: vec![
                Section::new(
                    "services",
                    "Services",
                    vec![
                        ColumnSpec::text("service", "Service").primary(),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::text("servers", "Servers up"),
                        ColumnSpec::text("provider", "Provider"),
                    ],
                )
                .rows(service_rows)
                .tracked()
                .empty_text("No HTTP services are configured.")
                .priority(10),
                Section::new(
                    "routers",
                    "Routers",
                    vec![
                        ColumnSpec::text("router", "Router").primary(),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::text("service", "Service"),
                        ColumnSpec::text("provider", "Provider"),
                        ColumnSpec::text("rule", "Rule"),
                    ],
                )
                .rows(router_rows)
                .tracked()
                .empty_text("No HTTP routers are configured.")
                .priority(20),
            ],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// Traefik's access log is a file on the host, not something the API
    /// serves.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str, status: &str, servers: &[(&str, &str)]) -> Service {
        Service {
            name: name.into(),
            status: status.into(),
            provider: Some("docker".into()),
            server_status: servers
                .iter()
                .map(|(url, state)| ((*url).to_string(), (*state).to_string()))
                .collect(),
        }
    }

    #[test]
    fn parses_the_overview() {
        let overview: Overview = serde_json::from_str(
            r#"{"http":{"routers":{"total":9,"online":8,"warning":0,"critical":1},
                       "services":{"total":7,"online":7},
                       "middlewares":{"total":3,"online":3}},
                "tcp":{"routers":{"total":1,"online":1}}}"#,
        )
        .expect("overview");
        assert_eq!(overview.http.routers.total, 9);
        assert_eq!(overview.http.routers.critical, 1);
        assert_eq!(overview.tcp.routers.total, 1);
    }

    /// The failure an uptime check on the front door cannot see: Traefik is up,
    /// the port answers, and this route 404s.
    #[test]
    fn a_disabled_router_is_down_even_though_traefik_is_up() {
        let router: Router = serde_json::from_str(
            r#"{"name":"api@docker","status":"disabled","error":["service \"missing\" does not exist"]}"#,
        )
        .expect("router");
        assert_eq!(config_health(&router.status), Health::Down);
        assert_eq!(router.error.len(), 1);
    }

    #[test]
    fn a_warning_router_is_degraded() {
        assert_eq!(config_health("warning"), Health::Degraded);
    }

    /// Traefik has added states before, and guessing "enabled" for an
    /// unrecognised one would report a route as healthy on no evidence.
    #[test]
    fn an_unrecognised_state_is_unknown_rather_than_assumed_fine() {
        assert_eq!(config_health("something-new"), Health::Unknown);
    }

    #[test]
    fn counts_servers_that_are_up() {
        let svc = service("web@docker", "enabled", &[("http://a:80", "UP"), ("http://b:80", "DOWN")]);
        assert_eq!(svc.servers(), Some((1, 2)));
    }

    /// A service with no health check has no servers *known* to be down, and
    /// calling that an outage would fail every service that never configured
    /// one.
    #[test]
    fn a_service_without_health_checks_reports_no_server_counts() {
        let svc = service("web@docker", "enabled", &[]);
        assert_eq!(svc.servers(), None);
    }

    #[test]
    fn server_state_comparison_ignores_case() {
        let svc = service("web@docker", "enabled", &[("http://a:80", "up")]);
        assert_eq!(svc.servers(), Some((1, 1)));
    }

    /// Missing sections are absent counts, not zeroes that would read as an
    /// empty proxy.
    #[test]
    fn an_overview_without_tcp_still_parses() {
        let overview: Overview = serde_json::from_str(r#"{"http":{"routers":{"total":2}}}"#)
            .expect("overview");
        assert_eq!(overview.http.routers.total, 2);
        assert_eq!(overview.tcp.routers.total, 0);
    }
}
