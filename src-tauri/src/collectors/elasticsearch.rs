//! Elasticsearch / OpenSearch collector: cluster health, per-node JVM heap,
//! CPU and disk, index inventory, and application log tailing from a
//! configured index pattern.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{elapsed_ms, http_client, Collector, DetailQuery};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, Metric, MetricUnit, Row,
    Section, ServiceKind,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize)]
struct ClusterHealth {
    #[serde(default)]
    cluster_name: Option<String>,
    status: String,
    #[serde(default)]
    number_of_nodes: Option<i64>,
    #[serde(default)]
    number_of_data_nodes: Option<i64>,
    #[serde(default)]
    active_shards: Option<i64>,
    #[serde(default)]
    relocating_shards: Option<i64>,
    #[serde(default)]
    initializing_shards: Option<i64>,
    #[serde(default)]
    unassigned_shards: Option<i64>,
    #[serde(default)]
    active_shards_percent_as_number: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct NodesStats {
    #[serde(default)]
    nodes: HashMap<String, NodeStats>,
}

#[derive(Debug, Deserialize)]
struct NodeStats {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    jvm: Option<Jvm>,
    #[serde(default)]
    os: Option<Os>,
    #[serde(default)]
    fs: Option<Fs>,
}

#[derive(Debug, Deserialize)]
struct Jvm {
    #[serde(default)]
    mem: Option<JvmMem>,
}

#[derive(Debug, Deserialize)]
struct JvmMem {
    #[serde(default)]
    heap_used_percent: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Os {
    #[serde(default)]
    cpu: Option<OsCpu>,
}

#[derive(Debug, Deserialize)]
struct OsCpu {
    #[serde(default)]
    percent: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Fs {
    #[serde(default)]
    total: Option<FsTotal>,
}

#[derive(Debug, Deserialize)]
struct FsTotal {
    #[serde(default)]
    total_in_bytes: Option<i64>,
    #[serde(default)]
    available_in_bytes: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CatIndex {
    #[serde(default)]
    index: Option<String>,
    #[serde(default)]
    health: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "docs.count")]
    docs_count: Option<String>,
    #[serde(default, rename = "store.size")]
    store_size: Option<String>,
    #[serde(default)]
    pri: Option<String>,
    #[serde(default)]
    rep: Option<String>,
}

pub struct ElasticsearchCollector {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    secret: Option<String>,
    log_index: Option<String>,
    timestamp_field: String,
    profile_id: String,
    profile_name: String,
    log_limit: usize,
}

impl ElasticsearchCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let base_url = profile.endpoint.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(AppError::Config(
                "Elasticsearch connections need a base URL, e.g. https://es.internal:9200"
                    .to_string(),
            ));
        }
        Ok(ElasticsearchCollector {
            client: http_client(profile.options.insecure_tls(), REQUEST_TIMEOUT)?,
            base_url,
            username: profile.username.clone().filter(|u| !u.is_empty()),
            secret,
            log_index: profile.options.string("logIndex"),
            timestamp_field: profile
                .options
                .string("timestampField")
                .unwrap_or_else(|| "@timestamp".to_string()),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            log_limit: profile.options.log_limit(),
        })
    }

    /// Basic auth when a username is set; otherwise the stored secret is
    /// treated as a base64 API key, which is how ES/OpenSearch service
    /// credentials are usually issued.
    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (&self.username, &self.secret) {
            (Some(user), secret) => req.basic_auth(user, secret.as_ref()),
            (None, Some(api_key)) => req.header("Authorization", format!("ApiKey {api_key}")),
            (None, None) => req,
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.authed(self.client.get(&url)).send().await?;
        self.decode(resp, path).await
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.authed(self.client.post(&url)).json(body).send().await?;
        self.decode(resp, path).await
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        &self,
        resp: reqwest::Response,
        path: &str,
    ) -> Result<T> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Collector(format!(
                "Elasticsearch returned {status} for {path}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        Ok(resp.json::<T>().await?)
    }
}

#[async_trait]
impl Collector for ElasticsearchCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Elasticsearch
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let cluster: ClusterHealth = match self.get_json("/_cluster/health").await {
            Ok(c) => c,
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Elasticsearch,
                    e.to_string(),
                    elapsed_ms(start),
                ))
            }
        };

        let health = match cluster.status.to_ascii_lowercase().as_str() {
            "green" => Health::Healthy,
            "yellow" => Health::Degraded,
            "red" => Health::Down,
            _ => Health::Unknown,
        };

        let nodes: NodesStats = self
            .get_json("/_nodes/stats/jvm,os,fs")
            .await
            .unwrap_or(NodesStats {
                nodes: HashMap::new(),
            });

        let indices: Vec<CatIndex> = self
            .get_json(
                "/_cat/indices?format=json&bytes=b&h=index,health,status,docs.count,store.size,pri,rep",
            )
            .await
            .unwrap_or_default();

        let mut node_rows = Vec::new();
        let mut index_rows = Vec::new();
        let mut max_heap = 0.0f64;
        let mut total_store_bytes = 0.0f64;

        for (id, node) in &nodes.nodes {
            let name = node.name.clone().unwrap_or_else(|| id.clone());
            let heap = node
                .jvm
                .as_ref()
                .and_then(|j| j.mem.as_ref())
                .and_then(|m| m.heap_used_percent)
                .unwrap_or(0.0);
            max_heap = max_heap.max(heap);

            let cpu = node
                .os
                .as_ref()
                .and_then(|o| o.cpu.as_ref())
                .and_then(|c| c.percent)
                .unwrap_or(0.0);

            let (disk_used_pct, disk_free) = node
                .fs
                .as_ref()
                .and_then(|f| f.total.as_ref())
                .map(|t| {
                    let total = t.total_in_bytes.unwrap_or(0) as f64;
                    let avail = t.available_in_bytes.unwrap_or(0) as f64;
                    if total > 0.0 {
                        ((total - avail) / total * 100.0, avail)
                    } else {
                        (0.0, avail)
                    }
                })
                .unwrap_or((0.0, 0.0));

            // ES starts shedding shards at the 85% watermark, so disk is the
            // early-warning signal that matters most here.
            let node_health = if heap > 90.0 || disk_used_pct > 90.0 {
                Health::Degraded
            } else {
                Health::Healthy
            };

            node_rows.push(
                Row::new(id.clone(), &name, node_health)
                    .summary(format!(
                        "heap {heap:.0}%, cpu {cpu:.0}%, disk {disk_used_pct:.0}% used"
                    ))
                    .cell("node", CellValue::text(&name))
                    .cell("roles", CellValue::tags(node.roles.clone()))
                    .cell("heap", CellValue::gauge_warn(heap, Some(100.0), 85.0))
                    .cell("cpu", CellValue::gauge_warn(cpu, Some(100.0), 85.0))
                    .cell(
                        "disk_used",
                        CellValue::gauge_warn(disk_used_pct, Some(100.0), 85.0),
                    )
                    .cell("disk_free", CellValue::number(disk_free)),
            );
        }

        for index in &indices {
            let Some(name) = index.index.clone() else {
                continue;
            };
            // Skip ES-internal indices; they add noise without signal.
            if name.starts_with('.') {
                continue;
            }
            let size = index
                .store_size
                .as_deref()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            total_store_bytes += size;

            let index_health = match index.health.as_deref() {
                Some("green") => Health::Healthy,
                Some("yellow") => Health::Degraded,
                Some("red") => Health::Down,
                _ => Health::Unknown,
            };

            let parse = |v: &Option<String>| {
                v.as_deref()
                    .and_then(|d| d.parse::<f64>().ok())
                    .map(CellValue::number)
                    .unwrap_or(CellValue::Null)
            };

            index_rows.push(
                Row::new(format!("index:{name}"), &name, index_health)
                    .summary(format!(
                        "{} docs, {}",
                        index.docs_count.clone().unwrap_or_else(|| "?".into()),
                        human_bytes(size)
                    ))
                    .cell("index", CellValue::text(&name))
                    .cell("health", CellValue::status(index_health))
                    .cell("docs", parse(&index.docs_count))
                    .cell("size", CellValue::number(size))
                    .cell("primaries", parse(&index.pri))
                    .cell("replicas", parse(&index.rep))
                    .cell("status", CellValue::maybe_text(index.status.clone())),
            );
        }

        let metrics = vec![
            Metric::new(
                "nodes",
                "Nodes",
                cluster.number_of_nodes.unwrap_or(0) as f64,
                MetricUnit::Count,
            ),
            Metric::new(
                "data_nodes",
                "Data nodes",
                cluster.number_of_data_nodes.unwrap_or(0) as f64,
                MetricUnit::Count,
            ),
            Metric::new(
                "active_shards",
                "Active shards",
                cluster.active_shards.unwrap_or(0) as f64,
                MetricUnit::Count,
            ),
            Metric::new(
                "unassigned_shards",
                "Unassigned shards",
                cluster.unassigned_shards.unwrap_or(0) as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new(
                "relocating_shards",
                "Relocating",
                cluster.relocating_shards.unwrap_or(0) as f64,
                MetricUnit::Count,
            ),
            Metric::new(
                "initializing_shards",
                "Initializing",
                cluster.initializing_shards.unwrap_or(0) as f64,
                MetricUnit::Count,
            ),
            Metric::new(
                "active_shards_pct",
                "Shards active",
                cluster.active_shards_percent_as_number.unwrap_or(0.0),
                MetricUnit::Percent,
            ),
            Metric::new("max_heap", "Peak node heap", max_heap, MetricUnit::Percent)
                .warn_above(85.0),
            Metric::new("store_total", "Total store", total_store_bytes, MetricUnit::Bytes),
        ];

        let sections = vec![
            Section::new(
                "nodes",
                "Nodes",
                vec![
                    ColumnSpec::text("node", "Node").primary(),
                    ColumnSpec::gauge("heap", "JVM heap", MetricUnit::Percent),
                    ColumnSpec::gauge("cpu", "CPU", MetricUnit::Percent),
                    ColumnSpec::gauge("disk_used", "Disk used", MetricUnit::Percent),
                    ColumnSpec::metric("disk_free", "Disk free", MetricUnit::Bytes),
                    ColumnSpec::tags("roles", "Roles").secondary(),
                ],
            )
            .rows(node_rows)
            .priority(10)
            .empty_text("No node stats were returned.")
            // A cluster has a handful of nodes that live for months — heap,
            // CPU and disk per node are exactly what you want a series for.
            // `indices` below is deliberately not tracked: it runs to hundreds.
            .tracked()
            .sorted_by_health(),
            Section::new(
                "indices",
                "Indices",
                vec![
                    ColumnSpec::text("index", "Index").primary(),
                    ColumnSpec::status("health", "Health"),
                    ColumnSpec::metric("size", "Size", MetricUnit::Bytes),
                    ColumnSpec::metric("docs", "Docs", MetricUnit::Count),
                    ColumnSpec::metric("primaries", "Primaries", MetricUnit::Count).secondary(),
                    ColumnSpec::metric("replicas", "Replicas", MetricUnit::Count).secondary(),
                    ColumnSpec::text("status", "State").secondary(),
                ],
            )
            .rows(index_rows)
            .priority(20)
            .empty_text("No user indices. (Internal dot-indices are hidden.)")
            .sorted_by_health(),
        ];


        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Elasticsearch,
            health,
            summary: format!(
                "Cluster `{}` is {} — {} nodes, {} unassigned shards",
                cluster.cluster_name.clone().unwrap_or_else(|| "?".into()),
                cluster.status,
                cluster.number_of_nodes.unwrap_or(0),
                cluster.unassigned_shards.unwrap_or(0),
            ),
            metrics,
            sections,
            checked_at: Utc::now(),
            latency_ms: elapsed_ms(start),
            error: None,
        })
    }

    async fn fetch_logs(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        // Without a configured log index there is nothing application-level to
        // tail; cluster state alone is covered by `collect`.
        let Some(index) = self.log_index.clone() else {
            return Ok(Vec::new());
        };

        let size = limit.min(self.log_limit);
        let body = serde_json::json!({
            "size": size,
            "sort": [{ &self.timestamp_field: { "order": "desc" } }],
            "query": {
                "range": {
                    &self.timestamp_field: { "gt": since.to_rfc3339() }
                }
            }
        });

        let resp: serde_json::Value = self
            .post_json(&format!("/{index}/_search"), &body)
            .await?;

        let hits = resp
            .get("hits")
            .and_then(|h| h.get("hits"))
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();

        let mut events = Vec::new();
        for hit in hits {
            let source = hit.get("_source").cloned().unwrap_or(serde_json::Value::Null);
            let timestamp = source
                .get(&self.timestamp_field)
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);

            let message = first_string(&source, &["message", "msg", "log", "event.original"])
                .unwrap_or_else(|| source.to_string());

            let level = first_string(&source, &["level", "log.level", "severity", "loglevel"])
                .map(|l| parse_level(&l))
                .unwrap_or_else(|| LogLevel::infer(&message));

            let source_name = first_string(
                &source,
                &["service.name", "container.name", "kubernetes.pod.name", "host.name", "logger"],
            )
            .unwrap_or_else(|| index.clone());

            events.push(LogEvent::new(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::Elasticsearch,
                &source_name,
                level,
                message,
                timestamp,
            ));
        }

        events.sort_by_key(|e| e.timestamp);
        Ok(events)
    }

    async fn detail(&self, query: &DetailQuery) -> Result<serde_json::Value> {
        match query {
            DetailQuery::EsSearch { index, body } => {
                // `_search` is read-only; the agent has no path to write here.
                self.post_json(&format!("/{index}/_search"), body).await
            }
            _ => Err(AppError::UnsupportedAction("Elasticsearch".to_string())),
        }
    }
}

/// Look up the first present key, supporting both dotted and nested forms so
/// ECS documents (`log.level`) and flat ones both work.
fn first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(v) = value.get(*key).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
        let mut cursor = value;
        let mut found = true;
        for part in key.split('.') {
            match cursor.get(part) {
                Some(next) => cursor = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            if let Some(v) = cursor.as_str() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn parse_level(raw: &str) -> LogLevel {
    match raw.trim().to_ascii_lowercase().as_str() {
        "trace" => LogLevel::Trace,
        "debug" => LogLevel::Debug,
        "warn" | "warning" => LogLevel::Warn,
        "error" | "err" => LogLevel::Error,
        "fatal" | "critical" | "crit" => LogLevel::Fatal,
        _ => LogLevel::Info,
    }
}

fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_dotted_and_nested_fields() {
        let doc = serde_json::json!({
            "log": { "level": "warn" },
            "message": "disk filling up"
        });
        assert_eq!(first_string(&doc, &["log.level"]).unwrap(), "warn");
        assert_eq!(first_string(&doc, &["message"]).unwrap(), "disk filling up");
        assert!(first_string(&doc, &["nope"]).is_none());
    }

    #[test]
    fn normalizes_level_names() {
        assert_eq!(parse_level("WARNING"), LogLevel::Warn);
        assert_eq!(parse_level("critical"), LogLevel::Fatal);
        assert_eq!(parse_level("anything else"), LogLevel::Info);
    }
}
