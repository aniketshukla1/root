//! Collectors turn a live piece of infrastructure into two things: a
//! [`HealthSnapshot`] describing its current state, and a stream of normalized
//! [`LogEvent`]s.
//!
//! Log delivery is pull-based with a per-connection watermark rather than N
//! long-lived follow streams. At the default 5–10s poll interval this is
//! effectively real time, and it degrades far better: a flapping endpoint costs
//! one failed request instead of leaking a task per container.

pub mod apache;
pub mod haproxy;
pub mod clickhouse;
pub mod consul;
pub mod database;
pub mod docker;
pub mod elasticsearch;
pub mod etcd;
pub mod http;
pub mod jenkins;
pub mod kafka;
pub mod kubernetes;
pub mod linux_host;
pub mod memcached;
pub mod mongodb;
pub mod nats;
pub mod nfs;
pub mod nginx;
pub mod prometheus;
pub mod promtext;
pub mod rabbitmq;
pub mod tomcat;
pub mod traefik;
pub mod zookeeper;
pub mod redis;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{HealthSnapshot, LogEvent, ServiceKind};

/// A state-changing operation the assistant may request. Every variant here is
/// gated behind explicit user approval before it runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CollectorAction {
    RestartContainer { container: String },
    StartContainer { container: String },
    StopContainer { container: String },
    TriggerJenkinsBuild { job: String },
    TerminateDbQuery { pid: String },
}

impl CollectorAction {
    /// Human-readable description shown in the confirmation dialog. This is the
    /// text the user actually approves, so it must name the real target.
    pub fn describe(&self) -> String {
        match self {
            CollectorAction::RestartContainer { container } => {
                format!("Restart container `{container}`")
            }
            CollectorAction::StartContainer { container } => {
                format!("Start container `{container}`")
            }
            CollectorAction::StopContainer { container } => {
                format!("Stop container `{container}`")
            }
            CollectorAction::TriggerJenkinsBuild { job } => {
                format!("Trigger a build of Jenkins job `{job}`")
            }
            CollectorAction::TerminateDbQuery { pid } => {
                format!("Terminate the database backend running as PID `{pid}`")
            }
        }
    }

    /// The equivalent shell command, shown alongside the confirmation and used
    /// verbatim in read-only mode where the operator runs it themselves.
    pub fn equivalent_command(&self, endpoint: &str) -> String {
        match self {
            CollectorAction::RestartContainer { container } => {
                format!("docker restart {container}")
            }
            CollectorAction::StartContainer { container } => format!("docker start {container}"),
            CollectorAction::StopContainer { container } => format!("docker stop {container}"),
            CollectorAction::TriggerJenkinsBuild { job } => {
                format!("curl -X POST -u <user>:<token> {endpoint}/job/{job}/build")
            }
            CollectorAction::TerminateDbQuery { pid } => {
                format!("SELECT pg_terminate_backend({pid});  -- MySQL: KILL {pid};")
            }
        }
    }
}

/// Free-form read-only detail lookup, used by the assistant to drill into one
/// component without a dedicated tool per service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DetailQuery {
    /// Full `docker inspect` output for one container.
    Container { container: String },
    /// Console output of a Jenkins build; `build` defaults to the last build.
    JenkinsBuildLog {
        job: String,
        build: Option<i64>,
        tail_lines: Option<usize>,
    },
    /// A read-only Elasticsearch `_search` body against an index.
    EsSearch {
        index: String,
        body: serde_json::Value,
    },
    /// One of a fixed set of database diagnostics — never arbitrary SQL.
    DbDiagnostic { check: String },
}

#[async_trait]
pub trait Collector: Send + Sync {
    fn kind(&self) -> ServiceKind;

    /// One polling cycle. Should return a `Down` snapshot rather than an error
    /// when the target is merely unreachable; errors are for internal faults.
    async fn collect(&self) -> Result<HealthSnapshot>;

    /// Log lines newer than `since`, oldest first, capped at `limit`.
    async fn fetch_logs(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>>;

    /// Read-only drill-down. Default: unsupported.
    async fn detail(&self, _query: &DetailQuery) -> Result<serde_json::Value> {
        Err(AppError::UnsupportedAction(self.kind().label().to_string()))
    }

    /// State-changing operation. Default: unsupported. Callers are responsible
    /// for having obtained approval before calling this.
    async fn run_action(&self, _action: &CollectorAction) -> Result<String> {
        Err(AppError::UnsupportedAction(self.kind().label().to_string()))
    }
}

/// Build the right collector for a profile. `secret` is the password or API
/// token pulled from the keychain, if one is stored.
pub async fn build_collector(
    profile: &ConnectionProfile,
    secret: Option<String>,
) -> Result<Arc<dyn Collector>> {
    let collector: Arc<dyn Collector> = match profile.kind {
        ServiceKind::Docker | ServiceKind::Podman => {
            Arc::new(docker::DockerCollector::new(profile)?)
        }
        ServiceKind::LinuxHost => Arc::new(linux_host::LinuxHostCollector::new(profile, secret)?),
        ServiceKind::Nfs => Arc::new(nfs::NfsCollector::new(profile, secret)?),
        ServiceKind::Jenkins => Arc::new(jenkins::JenkinsCollector::new(profile, secret)?),
        ServiceKind::Elasticsearch => {
            Arc::new(elasticsearch::ElasticsearchCollector::new(profile, secret)?)
        }
        ServiceKind::Postgres | ServiceKind::Mysql => {
            Arc::new(database::DatabaseCollector::new(profile, secret).await?)
        }
        ServiceKind::HttpEndpoint => Arc::new(http::HttpCollector::new(profile, secret)?),
        ServiceKind::Kubernetes => Arc::new(kubernetes::KubernetesCollector::new(profile).await?),
        ServiceKind::Redis => Arc::new(redis::RedisCollector::new(profile, secret)?),
        ServiceKind::Mongodb => Arc::new(mongodb::MongoCollector::new(profile, secret).await?),
        ServiceKind::Prometheus => Arc::new(prometheus::PrometheusCollector::new(profile, secret)?),
        ServiceKind::Rabbitmq => Arc::new(rabbitmq::RabbitCollector::new(profile, secret)?),
        ServiceKind::Nginx => Arc::new(nginx::NginxCollector::new(profile, secret)?),
        ServiceKind::Apache => Arc::new(apache::ApacheCollector::new(profile, secret)?),
        ServiceKind::Haproxy => Arc::new(haproxy::HaproxyCollector::new(profile, secret)?),
        ServiceKind::Memcached => Arc::new(memcached::MemcachedCollector::new(profile)?),
        ServiceKind::Traefik => Arc::new(traefik::TraefikCollector::new(profile, secret)?),
        ServiceKind::Tomcat => Arc::new(tomcat::TomcatCollector::new(profile, secret)?),
        ServiceKind::Zookeeper => Arc::new(zookeeper::ZookeeperCollector::new(profile)?),
        ServiceKind::Consul => Arc::new(consul::ConsulCollector::new(profile, secret)?),
        ServiceKind::Etcd => Arc::new(etcd::EtcdCollector::new(profile, secret)?),
        ServiceKind::Nats => Arc::new(nats::NatsCollector::new(profile, secret)?),
        ServiceKind::Clickhouse => Arc::new(clickhouse::ClickhouseCollector::new(profile, secret)?),
        ServiceKind::Kafka => Arc::new(kafka::KafkaCollector::new(profile, secret)?),
    };
    Ok(collector)
}

/// Shared HTTP client factory. Honors the per-profile TLS escape hatch that
/// internal endpoints with self-signed certs need.
pub(crate) fn http_client(insecure_tls: bool, timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .danger_accept_invalid_certs(insecure_tls)
        .user_agent("InfraMonitor/0.1")
        .build()
        .map_err(|e| AppError::Collector(format!("could not build HTTP client: {e}")))
}

/// Milliseconds elapsed since `start`, saturating.
pub(crate) fn elapsed_ms(start: std::time::Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

/// Invariants every collector's output must satisfy.
///
/// Not `#[cfg(test)]`: integration tests link the library without that flag, and
/// these are the checks that guarantee the generic renderer can never be handed
/// something it cannot draw — the property that keeps a brand-new service type
/// working with no frontend code at all.
pub mod validate {
    use crate::model::HealthSnapshot;

    /// Panics with a specific message if any section could not be rendered.
    pub fn assert_sections_are_renderable(snapshot: &HealthSnapshot) {
        for section in &snapshot.sections {
            assert!(!section.id.is_empty(), "a section has no id");
            assert!(!section.label.is_empty(), "section `{}` has no label", section.id);
            assert!(
                !section.columns.is_empty(),
                "section `{}` declares no columns",
                section.id
            );
            assert_eq!(
                section.columns.iter().filter(|c| c.primary).count(),
                1,
                "section `{}` must have exactly one primary column",
                section.id
            );

            let declared: Vec<&str> = section.columns.iter().map(|c| c.key.as_str()).collect();
            for row in &section.rows {
                assert!(!row.id.is_empty(), "a row in `{}` has no id", section.id);
                assert!(!row.name.is_empty(), "row `{}` has no name", row.id);
                // Every cell must correspond to a declared column, or it would
                // silently never render.
                for key in row.cells.keys() {
                    assert!(
                        declared.contains(&key.as_str()),
                        "section `{}` row `{}` has cell `{key}` with no matching column (declared: {declared:?})",
                        section.id,
                        row.id
                    );
                }
            }
        }
    }

    /// Row ids must be unique within a section — alert fingerprints are built
    /// from them, so a collision would merge two entities into one alert.
    pub fn assert_row_ids_are_unique(snapshot: &HealthSnapshot) {
        for section in &snapshot.sections {
            let mut seen = std::collections::HashSet::new();
            for row in &section.rows {
                assert!(
                    seen.insert(&row.id),
                    "section `{}` has duplicate row id `{}`",
                    section.id,
                    row.id
                );
            }
        }
    }

    /// Both checks at once.
    pub fn assert_renderable(snapshot: &HealthSnapshot) {
        assert_sections_are_renderable(snapshot);
        assert_row_ids_are_unique(snapshot);
    }
}
