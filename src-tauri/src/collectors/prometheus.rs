//! Prometheus monitor — the monitor watching the monitor.
//!
//! Three read-only API calls: `/api/v1/targets` for scrape health,
//! `/api/v1/rules` for firing alerts, and `/api/v1/status/tsdb` for storage.
//!
//! Deliberately not a PromQL client. Running arbitrary queries would make this
//! a second query UI competing with Prometheus' own, and the interesting
//! question here is not "what do the metrics say" but "is Prometheus itself
//! healthy and is it actually scraping what it thinks it is". A target that
//! silently stopped being scraped is invisible in a dashboard built on its
//! data — the graph just goes flat.

use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, LogStream, Metric,
    MetricUnit, Row, Section, ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Down targets listed before the table is truncated.
const MAX_TARGET_ROWS: usize = 100;

pub struct PrometheusCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    down_targets_critical: usize,
    profile_id: String,
    profile_name: String,
    /// Alerts already reported, so a long-firing alert is logged once.
    reported_alerts: Mutex<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    status: String,
    // No `#[serde(default)]` on these: serde already maps a missing field to
    // `None` for an Option, and the attribute would add a `T: Default` bound
    // that none of the payload types satisfy.
    data: Option<T>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TargetsData {
    #[serde(rename = "activeTargets", default)]
    active_targets: Vec<ActiveTarget>,
}

#[derive(Debug, Deserialize)]
struct ActiveTarget {
    #[serde(default)]
    labels: std::collections::HashMap<String, String>,
    #[serde(rename = "scrapePool", default)]
    scrape_pool: String,
    #[serde(rename = "scrapeUrl", default)]
    scrape_url: String,
    #[serde(default)]
    health: String,
    #[serde(rename = "lastError", default)]
    last_error: String,
    #[serde(rename = "lastScrapeDuration", default)]
    last_scrape_duration: f64,
    #[serde(rename = "lastScrape", default)]
    last_scrape: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct RulesData {
    #[serde(default)]
    groups: Vec<RuleGroup>,
}

#[derive(Debug, Deserialize)]
struct RuleGroup {
    #[serde(default)]
    name: String,
    #[serde(default)]
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
struct Rule {
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    #[serde(rename = "type", default)]
    rule_type: String,
    #[serde(default)]
    health: String,
    #[serde(default)]
    alerts: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct TsdbData {
    #[serde(rename = "numSeries", default)]
    num_series: f64,
    #[serde(rename = "headStats", default)]
    head_stats: Option<HeadStats>,
}

#[derive(Debug, Deserialize, Default)]
struct HeadStats {
    #[serde(rename = "numSeries", default)]
    num_series: f64,
}

impl PrometheusCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let base = profile.endpoint.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err(AppError::Config(
                "Prometheus connections need a URL, e.g. http://prometheus.internal:9090"
                    .to_string(),
            ));
        }
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(AppError::Config(format!(
                "`{base}` must start with http:// or https://"
            )));
        }

        Ok(PrometheusCollector {
            client: http_client(profile.options.insecure_tls(), REQUEST_TIMEOUT)?,
            base,
            username: profile.username.clone().filter(|u| !u.is_empty()),
            secret,
            down_targets_critical: profile
                .options
                .u64("downTargetsCritical")
                .unwrap_or(1)
                .max(1) as usize,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            reported_alerts: Mutex::new(Vec::new()),
        })
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (&self.username, &self.secret) {
            (Some(user), secret) => req.basic_auth(user, secret.as_ref()),
            (None, Some(token)) => req.bearer_auth(token),
            (None, None) => req,
        }
    }

    /// GET one API path and unwrap Prometheus' `{status, data}` envelope.
    ///
    /// A 200 with `status: "error"` is a real failure mode here, so the envelope
    /// is checked rather than trusting the HTTP status alone.
    async fn api<T: serde::de::DeserializeOwned>(&self, path: &str) -> Option<T> {
        let url = format!("{}{path}", self.base);
        let response = self.authed(self.client.get(&url)).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let parsed: ApiResponse<T> = response.json().await.ok()?;
        if parsed.status != "success" {
            tracing::warn!(
                path,
                error = parsed.error.unwrap_or_default(),
                "prometheus API returned an error envelope"
            );
            return None;
        }
        parsed.data
    }
}

/// The most useful name for a target: its `job` label, falling back to the pool.
fn target_name(target: &ActiveTarget) -> String {
    let instance = target.labels.get("instance").cloned().unwrap_or_default();
    let job = target
        .labels
        .get("job")
        .cloned()
        .unwrap_or_else(|| target.scrape_pool.clone());
    if instance.is_empty() {
        job
    } else {
        format!("{job} · {instance}")
    }
}

#[async_trait]
impl Collector for PrometheusCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Prometheus
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let Some(targets) = self.api::<TargetsData>("/api/v1/targets").await else {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Prometheus,
                format!("{} did not answer /api/v1/targets", self.base),
                elapsed_ms(start),
            ));
        };
        let latency = elapsed_ms(start);

        let rules = self.api::<RulesData>("/api/v1/rules").await;
        let tsdb = self.api::<TsdbData>("/api/v1/status/tsdb").await;

        // ------------------------------------------------------- targets
        let total = targets.active_targets.len();
        let down: Vec<&ActiveTarget> = targets
            .active_targets
            .iter()
            .filter(|t| t.health != "up")
            .collect();
        let up = total - down.len();

        // Only failing targets get rows. A Prometheus with 4,000 healthy
        // targets should not render a 4,000-row table.
        let target_rows: Vec<Row> = down
            .iter()
            .take(MAX_TARGET_ROWS)
            .map(|target| {
                let name = target_name(target);
                Row::new(
                    format!("target:{}", target.scrape_url),
                    &name,
                    // "unknown" means never scraped yet, which is not the same
                    // as a target that is actively failing.
                    if target.health == "down" { Health::Down } else { Health::Degraded },
                )
                .summary(if target.last_error.is_empty() {
                    target.health.clone()
                } else {
                    target.last_error.clone()
                })
                .cell("target", CellValue::text(&name))
                .cell("pool", CellValue::text(&target.scrape_pool))
                .cell("url", CellValue::text(&target.scrape_url))
                .cell("error", CellValue::text(&target.last_error))
                .cell(
                    "duration",
                    CellValue::number(target.last_scrape_duration * 1000.0),
                )
                .cell(
                    "last_scrape",
                    match target.last_scrape {
                        Some(at) => CellValue::timestamp(at),
                        None => CellValue::Null,
                    },
                )
            })
            .collect();

        // --------------------------------------------------------- alerts
        let mut firing_rows = Vec::new();
        let mut firing_count = 0usize;
        let mut broken_rules = 0usize;

        if let Some(rules) = &rules {
            for group in &rules.groups {
                for rule in &group.rules {
                    if rule.health == "err" {
                        broken_rules += 1;
                    }
                    if rule.rule_type != "alerting" || rule.state != "firing" {
                        continue;
                    }
                    firing_count += 1;
                    firing_rows.push(
                        Row::new(
                            format!("alert:{}/{}", group.name, rule.name),
                            &rule.name,
                            Health::Down,
                        )
                        .summary(format!(
                            "{} instance{} firing",
                            rule.alerts.len(),
                            if rule.alerts.len() == 1 { "" } else { "s" }
                        ))
                        .cell("alert", CellValue::text(&rule.name))
                        .cell("group", CellValue::text(&group.name))
                        .cell("instances", CellValue::number(rule.alerts.len() as f64))
                        .cell("state", CellValue::text(&rule.state)),
                    );
                }
            }
        }

        // ---------------------------------------------------------- rollup
        let series = tsdb
            .as_ref()
            .map(|t| {
                let head = t.head_stats.as_ref().map(|h| h.num_series).unwrap_or(0.0);
                if head > 0.0 { head } else { t.num_series }
            })
            .unwrap_or(0.0);

        let health = if firing_count > 0 || down.len() >= self.down_targets_critical {
            Health::Down
        } else if !down.is_empty() || broken_rules > 0 {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if firing_count > 0 {
            format!(
                "{firing_count} alert{} firing",
                if firing_count == 1 { "" } else { "s" }
            )
        } else if !down.is_empty() {
            format!("{} of {total} targets down", down.len())
        } else if broken_rules > 0 {
            format!("{broken_rules} rules failing to evaluate")
        } else {
            format!("{up} targets up · {series:.0} series")
        };

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Prometheus,
            health,
            summary,
            metrics: vec![
                Metric::new("targets_up", "Targets up", up as f64, MetricUnit::Count),
                Metric::new("targets_total", "Targets", total as f64, MetricUnit::Count),
                Metric::new("targets_down", "Targets down", down.len() as f64, MetricUnit::Count)
                    .warn_above(0.0),
                Metric::new("alerts_firing", "Alerts firing", firing_count as f64, MetricUnit::Count)
                    .warn_above(0.0),
                Metric::new("rules_broken", "Rules failing", broken_rules as f64, MetricUnit::Count)
                    .warn_above(0.0),
                Metric::new("series", "Active series", series, MetricUnit::Count),
            ],
            sections: vec![
                Section::new(
                    "alerts",
                    "Firing alerts",
                    vec![
                        ColumnSpec::text("alert", "Alert").primary(),
                        ColumnSpec::text("group", "Group"),
                        ColumnSpec::metric("instances", "Instances", MetricUnit::Count),
                        ColumnSpec::text("state", "State"),
                    ],
                )
                .rows(firing_rows)
                .priority(5)
                .empty_text("No alerts firing."),
                Section::new(
                    "targets",
                    "Targets needing attention",
                    vec![
                        ColumnSpec::text("target", "Target").primary(),
                        ColumnSpec::text("pool", "Pool"),
                        ColumnSpec::text("url", "URL"),
                        ColumnSpec::text("error", "Last error"),
                        ColumnSpec::metric("duration", "Scrape time", MetricUnit::Millis),
                        ColumnSpec::timestamp("last_scrape", "Last scrape"),
                    ],
                )
                .rows(target_rows)
                .priority(10)
                .sorted_by_health()
                .empty_text("Every scrape target is up."),
            ],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    async fn fetch_logs(&self, _since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let Some(rules) = self.api::<RulesData>("/api/v1/rules").await else {
            return Ok(Vec::new());
        };

        let mut reported = self.reported_alerts.lock().await;
        let mut out = Vec::new();

        for group in &rules.groups {
            for rule in &group.rules {
                if rule.rule_type != "alerting" || rule.state != "firing" {
                    continue;
                }
                let key = format!("{}/{}", group.name, rule.name);
                if reported.contains(&key) {
                    continue;
                }
                reported.push(key.clone());
                out.push(
                    LogEvent::new(
                        &self.profile_id,
                        &self.profile_name,
                        ServiceKind::Prometheus,
                        &group.name,
                        LogLevel::Error,
                        format!(
                            "alert firing: {} ({} instance{})",
                            rule.name,
                            rule.alerts.len(),
                            if rule.alerts.len() == 1 { "" } else { "s" }
                        ),
                        Utc::now(),
                    )
                    .stream(LogStream::Event),
                );
            }
        }

        if reported.len() > 512 {
            let drop_to = reported.len() - 256;
            reported.drain(..drop_to);
        }
        if out.len() > limit {
            out.truncate(limit);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn target(job: &str, instance: &str, health: &str) -> ActiveTarget {
        let mut labels = HashMap::new();
        labels.insert("job".to_string(), job.to_string());
        if !instance.is_empty() {
            labels.insert("instance".to_string(), instance.to_string());
        }
        ActiveTarget {
            labels,
            scrape_pool: job.to_string(),
            scrape_url: format!("http://{instance}/metrics"),
            health: health.to_string(),
            last_error: String::new(),
            last_scrape_duration: 0.01,
            last_scrape: None,
        }
    }

    #[test]
    fn target_name_combines_job_and_instance() {
        assert_eq!(target_name(&target("api", "10.0.0.1:9100", "up")), "api · 10.0.0.1:9100");
    }

    #[test]
    fn target_name_falls_back_to_the_job_alone() {
        assert_eq!(target_name(&target("api", "", "up")), "api");
    }

    #[test]
    fn target_name_falls_back_to_the_scrape_pool() {
        // A target with no job label at all still needs a readable name.
        let mut bare = target("", "", "up");
        bare.labels.clear();
        bare.scrape_pool = "federate".to_string();
        assert_eq!(target_name(&bare), "federate");
    }

    #[test]
    fn parses_the_api_envelope() {
        let raw = r#"{"status":"success","data":{"activeTargets":[
            {"labels":{"job":"node","instance":"h1:9100"},"scrapePool":"node",
             "scrapeUrl":"http://h1:9100/metrics","health":"up",
             "lastError":"","lastScrapeDuration":0.004}
        ]}}"#;
        let parsed: ApiResponse<TargetsData> = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.status, "success");
        let data = parsed.data.expect("data");
        assert_eq!(data.active_targets.len(), 1);
        assert_eq!(data.active_targets[0].health, "up");
    }

    #[test]
    fn an_error_envelope_is_recognised() {
        // Prometheus answers 200 with status:"error" for a bad request, so the
        // HTTP status alone is not enough to know the call worked.
        let raw = r#"{"status":"error","errorType":"bad_data","error":"nope"}"#;
        let parsed: ApiResponse<TargetsData> = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.status, "error");
        assert!(parsed.data.is_none());
        assert_eq!(parsed.error.as_deref(), Some("nope"));
    }

    #[test]
    fn parses_rules_with_firing_alerts() {
        let raw = r#"{"status":"success","data":{"groups":[
            {"name":"app","rules":[
                {"name":"HighErrorRate","state":"firing","type":"alerting","health":"ok",
                 "alerts":[{"state":"firing"},{"state":"firing"}]},
                {"name":"job:rate","state":"","type":"recording","health":"ok"}
            ]}
        ]}}"#;
        let parsed: ApiResponse<RulesData> = serde_json::from_str(raw).expect("parse");
        let groups = parsed.data.expect("data").groups;
        assert_eq!(groups[0].rules.len(), 2);
        let firing: Vec<&Rule> = groups[0]
            .rules
            .iter()
            .filter(|r| r.rule_type == "alerting" && r.state == "firing")
            .collect();
        assert_eq!(firing.len(), 1, "recording rules must not count as alerts");
        assert_eq!(firing[0].alerts.len(), 2);
    }

    #[test]
    fn tsdb_prefers_head_series_over_the_top_level_count() {
        let raw = r#"{"status":"success","data":{"numSeries":10,"headStats":{"numSeries":42,"chunkCount":7}}}"#;
        let parsed: ApiResponse<TsdbData> = serde_json::from_str(raw).expect("parse");
        let data = parsed.data.expect("data");
        assert_eq!(data.head_stats.as_ref().unwrap().num_series, 42.0);
    }
}
