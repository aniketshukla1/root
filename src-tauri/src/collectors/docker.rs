//! Container-engine collector: inventory, per-container CPU/memory/network
//! stats, health-check state, and log tailing via the Docker Engine API.
//!
//! Serves both Docker and Podman, because Podman implements the same API. What
//! it does not do is pretend they are the same product. Podman is daemonless
//! and rootless by default, its socket lives somewhere else entirely, and it
//! is usually started on demand by a systemd socket unit — so a connection
//! that fails needs a different sentence, and a connection that works should
//! be labelled with the engine the operator actually runs. The collector
//! carries its kind rather than hard-coding Docker into every snapshot.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use async_trait::async_trait;
use bollard::container::LogOutput;
use bollard::models::{ContainerStatsResponse, ContainerSummary, ContainerSummaryStateEnum};
use bollard::query_parameters::{
    ListContainersOptions, LogsOptions, RestartContainerOptions, StartContainerOptions,
    StatsOptions, StopContainerOptions,
};
use bollard::{Docker, API_DEFAULT_VERSION};
use chrono::{DateTime, TimeZone, Utc};
use futures_util::stream::StreamExt;
use tokio::sync::Mutex;

use super::{elapsed_ms, Collector, CollectorAction, DetailQuery};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, LogLevel, Metric, MetricUnit, Row,
    RowAction, Section, ServiceKind,
};

/// The Engine API reports cumulative CPU counters, and this stubs version of
/// the stats payload carries no `precpu` block, so a percentage needs two
/// samples. We keep the previous poll's counters and diff against them, which
/// also makes the number an average over the poll window rather than a 1s
/// instantaneous spike.
/// Socket read timeout, in seconds. Generous: `logs` and `stats` are long
/// polls, and a slow daemon should degrade rather than error.
const DOCKER_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Copy)]
struct CpuSample {
    total_usage: u64,
    system_usage: u64,
}

/// Resource reading for one container.
#[derive(Debug, Clone, Copy, Default)]
struct ContainerSample {
    cpu_percent: f64,
    memory_used: f64,
    /// `None` when the container has no explicit memory cap.
    memory_limit: Option<f64>,
    net_rx: f64,
    net_tx: f64,
}

pub struct DockerCollector {
    docker: Docker,
    /// Docker or Podman. Only the labelling and the socket search differ.
    kind: ServiceKind,
    profile_id: String,
    profile_name: String,
    log_limit: usize,
    cpu_cache: Mutex<HashMap<String, CpuSample>>,
}

impl DockerCollector {
    pub fn new(profile: &ConnectionProfile) -> Result<Self> {
        let kind = profile.kind;
        let endpoint = profile.endpoint.trim();
        let docker = if endpoint.is_empty() || endpoint.eq_ignore_ascii_case("local") {
            connect_local(kind)
        } else {
            Docker::connect_with_host(endpoint).map_err(|e| {
                AppError::Collector(format!(
                    "could not open the {} connection: {e}",
                    kind.label()
                ))
            })
        }?;

        Ok(DockerCollector {
            docker,
            kind,
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            log_limit: profile.options.log_limit(),
            cpu_cache: Mutex::new(HashMap::new()),
        })
    }

    fn container_name(summary: &ContainerSummary) -> String {
        summary
            .names
            .as_ref()
            .and_then(|names| names.first().cloned())
            .map(|n| n.trim_start_matches('/').to_string())
            .or_else(|| summary.id.as_ref().map(|id| id.chars().take(12).collect()))
            .unwrap_or_else(|| "<unnamed>".to_string())
    }

    /// Map Docker's container state, and its health check when one is defined,
    /// onto our health scale. A container with a failing health check is
    /// `Degraded` rather than `Down` — it is still up, just not serving.
    fn container_health(summary: &ContainerSummary) -> (Health, String) {
        let status_text = summary.status.clone().unwrap_or_default();
        let health_status = summary
            .health
            .as_ref()
            .and_then(|h| h.status.as_ref())
            .map(|s| s.to_string().to_ascii_lowercase());

        match summary.state {
            Some(ContainerSummaryStateEnum::RUNNING) => match health_status.as_deref() {
                // Docker already suffixes the status with "(unhealthy)"; only
                // add it when it is missing so the detail does not read
                // "Up 2 minutes (unhealthy) (unhealthy)".
                Some("unhealthy") => (Health::Degraded, annotate(&status_text, "unhealthy")),
                Some("starting") => (Health::Unknown, annotate(&status_text, "health starting")),
                _ => (Health::Healthy, status_text),
            },
            Some(ContainerSummaryStateEnum::RESTARTING) => {
                (Health::Degraded, format!("{status_text} (restarting)"))
            }
            Some(ContainerSummaryStateEnum::PAUSED) => (Health::Degraded, status_text),
            Some(ContainerSummaryStateEnum::CREATED) => (Health::Unknown, status_text),
            Some(ContainerSummaryStateEnum::EXITED)
            | Some(ContainerSummaryStateEnum::DEAD)
            | Some(ContainerSummaryStateEnum::REMOVING)
            | Some(ContainerSummaryStateEnum::STOPPING) => {
                // A clean `Exited (0)` is a completed job, not a fault.
                if status_text.contains("Exited (0)") {
                    (Health::Unknown, status_text)
                } else if exited_recently(&status_text) {
                    (Health::Down, status_text)
                } else {
                    // A container that died weeks ago is history, not an
                    // incident. Any dev machine accumulates dozens of these,
                    // and alerting on all of them buries the one that just
                    // crashed. It stays listed, just not as a live fault.
                    (Health::Unknown, status_text)
                }
            }
            _ => (Health::Unknown, status_text),
        }
    }

    /// One-shot resource sample for a container.
    async fn container_metrics(&self, id: &str) -> ContainerSample {
        let options = StatsOptions {
            stream: false,
            one_shot: true,
        };
        let stats: Option<ContainerStatsResponse> = self
            .docker
            .stats(id, Some(options))
            .next()
            .await
            .and_then(|r| r.ok());

        let mut sample = ContainerSample::default();
        let Some(stats) = stats else {
            return sample;
        };

        // --- CPU: diff against the previous poll's cumulative counters.
        let mut cpu_percent = 0.0;
        if let Some(cpu) = stats.cpu_stats.as_ref() {
            let total = cpu.cpu_usage.as_ref().and_then(|u| u.total_usage).unwrap_or(0);
            let system = cpu.system_cpu_usage.unwrap_or(0);
            let cores = cpu.online_cpus.unwrap_or(1).max(1) as f64;

            let mut cache = self.cpu_cache.lock().await;
            if let Some(prev) = cache.get(id) {
                let total_delta = total.saturating_sub(prev.total_usage) as f64;
                let system_delta = system.saturating_sub(prev.system_usage) as f64;
                if system_delta > 0.0 && total_delta > 0.0 {
                    cpu_percent = (total_delta / system_delta) * cores * 100.0;
                }
            }
            cache.insert(
                id.to_string(),
                CpuSample {
                    total_usage: total,
                    system_usage: system,
                },
            );
        }
        sample.cpu_percent = (cpu_percent * 100.0).round() / 100.0;

        // --- Memory: subtract page cache the way `docker stats` does.
        if let Some(mem) = stats.memory_stats.as_ref() {
            let cache_bytes = mem
                .stats
                .as_ref()
                .and_then(|s| {
                    s.get("inactive_file") // cgroup v2
                        .or_else(|| s.get("total_inactive_file")) // cgroup v1
                        .or_else(|| s.get("cache"))
                })
                .copied()
                .unwrap_or(0);
            sample.memory_used = mem.usage.unwrap_or(0).saturating_sub(cache_bytes) as f64;
            // A container with no explicit limit reports the host's total,
            // which makes a "% of limit" reading meaningless — leave it unset
            // so the UI shows an absolute figure instead of a misleading bar.
            sample.memory_limit = mem.limit.filter(|l| *l > 0).map(|l| l as f64);
        }

        if let Some(networks) = stats.networks.as_ref() {
            sample.net_rx = networks.values().filter_map(|n| n.rx_bytes).sum::<u64>() as f64;
            sample.net_tx = networks.values().filter_map(|n| n.tx_bytes).sum::<u64>() as f64;
        }

        sample
    }

    async fn running_containers(&self) -> Result<Vec<ContainerSummary>> {
        let options = ListContainersOptions {
            all: false,
            ..Default::default()
        };
        Ok(self.docker.list_containers(Some(options)).await?)
    }
}

#[async_trait]
impl Collector for DockerCollector {
    fn kind(&self) -> ServiceKind {
        self.kind
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let version = match self.docker.version().await {
            Ok(v) => v,
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    self.kind,
                    e.to_string(),
                    elapsed_ms(start),
                ))
            }
        };

        let options = ListContainersOptions {
            all: true,
            ..Default::default()
        };
        let containers = match self.docker.list_containers(Some(options)).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    self.kind,
                    e.to_string(),
                    elapsed_ms(start),
                ))
            }
        };

        // Stats are only meaningful for running containers, and each one is a
        // round trip, so gather them concurrently.
        let running_ids: Vec<String> = containers
            .iter()
            .filter(|c| matches!(c.state, Some(ContainerSummaryStateEnum::RUNNING)))
            .filter_map(|c| c.id.clone())
            .collect();

        let stat_futures = running_ids
            .iter()
            .map(|id| async move { (id.clone(), self.container_metrics(id).await) });
        let stats: HashMap<String, ContainerSample> = futures_util::future::join_all(stat_futures)
            .await
            .into_iter()
            .collect();

        let mut rows = Vec::with_capacity(containers.len());
        let mut total_cpu = 0.0;
        let mut total_mem_bytes = 0.0;
        let mut running = 0usize;
        let mut unhealthy = 0usize;

        for summary in &containers {
            let id = summary.id.clone().unwrap_or_default();
            let name = Self::container_name(summary);
            let (health, state_text) = Self::container_health(summary);

            if matches!(summary.state, Some(ContainerSummaryStateEnum::RUNNING)) {
                running += 1;
            }
            if matches!(health, Health::Down | Health::Degraded) {
                unhealthy += 1;
            }

            let sample = stats.get(&id).copied();
            if let Some(sample) = sample {
                total_cpu += sample.cpu_percent;
                total_mem_bytes += sample.memory_used;
            }

            let project = summary
                .labels
                .as_ref()
                .and_then(|l| l.get("com.docker.compose.project"))
                .cloned();

            let mut row = Row::new(&id, &name, health)
                .summary(state_text.clone())
                .cell("name", CellValue::text(&name))
                .cell("state", CellValue::text(&state_text))
                .cell("image", CellValue::maybe_text(summary.image.clone()))
                .cell("project", CellValue::maybe_text(project));

            match sample {
                Some(sample) => {
                    row = row
                        .cell(
                            "cpu",
                            CellValue::gauge_warn(sample.cpu_percent, Some(100.0), 85.0),
                        )
                        .cell(
                            "memory",
                            CellValue::gauge(sample.memory_used, sample.memory_limit),
                        )
                        .cell("net_rx", CellValue::number(sample.net_rx))
                        .cell("net_tx", CellValue::number(sample.net_tx));
                }
                // A stopped container has no resource reading; leave the cells
                // null rather than implying a genuine zero.
                None => {
                    row = row
                        .cell("cpu", CellValue::Null)
                        .cell("memory", CellValue::Null)
                        .cell("net_rx", CellValue::Null)
                        .cell("net_tx", CellValue::Null);
                }
            }

            // Offer the lifecycle action that makes sense for the current
            // state; the UI and the assistant share the same confirmed path.
            row = if matches!(summary.state, Some(ContainerSummaryStateEnum::RUNNING)) {
                row.action(RowAction::new(
                    "restart_container",
                    "Restart",
                    serde_json::json!({ "container": name }),
                ))
                .action(
                    RowAction::new(
                        "stop_container",
                        "Stop",
                        serde_json::json!({ "container": name }),
                    )
                    .destructive(),
                )
            } else {
                row.action(RowAction::new(
                    "start_container",
                    "Start",
                    serde_json::json!({ "container": name }),
                ))
            };

            rows.push(row);
        }

        // Worst row wins, but a stopped one-shot container should not drag the
        // daemon's own health down — `container_health` already maps clean and
        // long-past exits to `Unknown`.
        let health = rows
            .iter()
            .map(|r| r.health)
            .filter(|h| *h != Health::Unknown)
            .max()
            .unwrap_or(Health::Healthy);

        let sections = vec![Section::new(
            "containers",
            "Containers",
            vec![
                ColumnSpec::text("name", "Container").primary(),
                ColumnSpec::text("state", "State"),
                ColumnSpec::gauge("cpu", "CPU", MetricUnit::Percent),
                ColumnSpec::gauge("memory", "Memory", MetricUnit::Bytes),
                // A container's image moving is a redeploy; the change engine
                // diffs this column to place a marker on the timeline.
                ColumnSpec::text("image", "Image").secondary().revision(),
                ColumnSpec::text("project", "Project").secondary(),
                ColumnSpec::metric("net_rx", "Net RX", MetricUnit::Bytes).secondary(),
                ColumnSpec::metric("net_tx", "Net TX", MetricUnit::Bytes).secondary(),
            ],
        )
        .rows(rows)
        .priority(10)
        .empty_text("This engine has no containers.")
        // Bounded and long-lived: a host runs tens of containers, each for
        // hours or days, so per-container CPU and memory are worth charting.
        .tracked()
        .sorted_by_health()];

        let metrics = vec![
            Metric::new("containers", "Containers", containers.len() as f64, MetricUnit::Count),
            Metric::new("running", "Running", running as f64, MetricUnit::Count),
            Metric::new("unhealthy", "Unhealthy", unhealthy as f64, MetricUnit::Count)
                .warn_above(0.0),
            Metric::new(
                "cpu_total",
                "Total CPU",
                (total_cpu * 100.0).round() / 100.0,
                MetricUnit::Percent,
            ),
            Metric::new("mem_total", "Total memory", total_mem_bytes, MetricUnit::Bytes),
        ];

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: self.kind,
            health,
            summary: format!(
                "Engine {} — {running}/{} containers running{}",
                version.version.unwrap_or_else(|| "?".into()),
                containers.len(),
                if unhealthy > 0 {
                    format!(", {unhealthy} unhealthy")
                } else {
                    String::new()
                }
            ),
            metrics,
            sections,
            checked_at: Utc::now(),
            latency_ms: elapsed_ms(start),
            error: None,
        })
    }

    async fn fetch_logs(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<LogEvent>> {
        let containers = self.running_containers().await?;
        let per_container = (limit / containers.len().max(1)).clamp(10, self.log_limit);
        let mut events = Vec::new();

        for summary in containers {
            let Some(id) = summary.id.clone() else { continue };
            let name = Self::container_name(&summary);

            let options = LogsOptions {
                follow: false,
                stdout: true,
                stderr: true,
                // The Engine API takes seconds; nudge forward by one so we do
                // not re-read the boundary second on every poll.
                since: (since.timestamp() + 1).clamp(0, i64::from(i32::MAX)) as i32,
                until: 0,
                timestamps: true,
                tail: per_container.to_string(),
            };

            let mut stream = self.docker.logs(&id, Some(options));
            let mut collected = 0usize;
            while let Some(frame) = stream.next().await {
                let Ok(frame) = frame else { break };
                let is_stderr = matches!(frame, LogOutput::StdErr { .. });
                let text = frame.to_string();
                for line in text.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let (ts, message) = split_docker_timestamp(line);
                    if ts <= since {
                        continue;
                    }
                    // stderr is not automatically an error, but when the line
                    // itself gives no clue it is the better default.
                    let inferred = LogLevel::infer(message);
                    let level = if is_stderr && inferred == LogLevel::Info {
                        LogLevel::Warn
                    } else {
                        inferred
                    };
                    events.push(LogEvent::new(
                        &self.profile_id,
                        &self.profile_name,
                        self.kind,
                        &name,
                        level,
                        message.to_string(),
                        ts,
                    ));
                    collected += 1;
                    if collected >= per_container {
                        break;
                    }
                }
                if collected >= per_container {
                    break;
                }
            }
        }

        events.sort_by_key(|e| e.timestamp);
        events.truncate(limit);
        Ok(events)
    }

    async fn detail(&self, query: &DetailQuery) -> Result<serde_json::Value> {
        match query {
            DetailQuery::Container { container } => {
                let inspect = self.docker.inspect_container(container, None::<bollard::query_parameters::InspectContainerOptions>).await?;
                Ok(serde_json::to_value(inspect)?)
            }
            _ => Err(AppError::UnsupportedAction("Docker".to_string())),
        }
    }

    async fn run_action(&self, action: &CollectorAction) -> Result<String> {
        match action {
            CollectorAction::RestartContainer { container } => {
                let options = RestartContainerOptions {
                    t: Some(10),
                    ..Default::default()
                };
                self.docker.restart_container(container, Some(options)).await?;
                Ok(format!("Container `{container}` restarted."))
            }
            CollectorAction::StartContainer { container } => {
                self.docker
                    .start_container(container, None::<StartContainerOptions>)
                    .await?;
                Ok(format!("Container `{container}` started."))
            }
            CollectorAction::StopContainer { container } => {
                let options = StopContainerOptions {
                    t: Some(10),
                    ..Default::default()
                };
                self.docker.stop_container(container, Some(options)).await?;
                Ok(format!("Container `{container}` stopped."))
            }
            _ => Err(AppError::UnsupportedAction("Docker".to_string())),
        }
    }
}

/// Append a qualifier only if the status does not already carry it.
fn annotate(status: &str, note: &str) -> String {
    if status.to_ascii_lowercase().contains(&note.to_ascii_lowercase()) {
        status.to_string()
    } else if status.is_empty() {
        format!("({note})")
    } else {
        format!("{status} ({note})")
    }
}

/// Whether a stopped container died recently enough to still be an incident.
///
/// The Engine's container list carries no exit timestamp — only the rendered
/// status string ("Exited (137) 4 weeks ago"), produced by Docker's stable
/// `HumanDuration` formatter. Reading that avoids an `inspect` round trip per
/// container just to date a corpse. Anything measured in seconds, minutes, or
/// hours counts as recent; days and beyond do not.
fn exited_recently(status: &str) -> bool {
    let text = status.to_ascii_lowercase();
    if !text.contains("ago") {
        // No age rendered at all (e.g. a bare "Exited (1)") — treat it as
        // current rather than silently swallowing a real failure.
        return true;
    }
    ["second", "minute", "hour"]
        .iter()
        .any(|unit| text.contains(unit))
}

/// Socket locations to try, in order, when no endpoint is configured.
///
/// bollard's own "local defaults" only knows `DOCKER_HOST` and
/// `/var/run/docker.sock`. That misses the most common desktop setup there is:
/// Docker Desktop on macOS puts its socket under `~/.docker/run/`, and unless
/// the user has ticked "allow the default Docker socket to be used" there is
/// nothing at `/var/run/docker.sock` at all. Colima, Rancher Desktop, and
/// rootless Podman each place it somewhere else again, so probe for a socket
/// that actually exists rather than assuming one path.
/// Where each engine keeps its socket.
///
/// Searched in the order an operator would expect to be believed: the engine
/// they asked for first. A Podman connection that quietly fell through to a
/// Docker socket would report Docker's containers under Podman's name, which
/// is worse than failing.
fn local_socket_candidates(kind: ServiceKind) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let mut candidates = Vec::new();

    if kind == ServiceKind::Podman {
        // Rootless, which is Podman's default and the common case.
        if let Some(runtime) = &runtime {
            candidates.push(runtime.join("podman/podman.sock"));
        }
        // Rootful, on a host where the socket unit runs as root.
        candidates.push(PathBuf::from("/run/podman/podman.sock"));
        if let Some(home) = &home {
            // `podman machine` on macOS forwards the VM's socket to here.
            candidates.push(home.join(".local/share/containers/podman/machine/podman.sock"));
            candidates.push(home.join(".local/share/containers/podman/machine/qemu/podman.sock"));
        }
        return candidates;
    }

    if let Some(home) = &home {
        candidates.push(home.join(".docker/run/docker.sock")); // Docker Desktop
        candidates.push(home.join(".colima/default/docker.sock")); // Colima
        candidates.push(home.join(".rd/docker.sock")); // Rancher Desktop
    }

    candidates.push(PathBuf::from("/var/run/docker.sock"));

    if let Some(runtime) = &runtime {
        candidates.push(runtime.join("docker.sock"));
    }

    candidates
}

fn connect_local(kind: ServiceKind) -> Result<Docker> {
    // Each engine has its own "point somewhere else" variable, and an explicit
    // one always wins — it is how an operator reaches a remote or non-standard
    // socket. Podman reads CONTAINER_HOST; honouring DOCKER_HOST for a Podman
    // connection would send it to the wrong engine on any machine running both.
    let (var, value) = match kind {
        ServiceKind::Podman => ("CONTAINER_HOST", std::env::var("CONTAINER_HOST")),
        _ => ("DOCKER_HOST", std::env::var("DOCKER_HOST")),
    };
    if let Ok(host) = value {
        if !host.trim().is_empty() {
            return Docker::connect_with_host(&host).map_err(|e| {
                AppError::Collector(format!("{var} is set to `{host}` but failed: {e}"))
            });
        }
    }

    let candidates = local_socket_candidates(kind);
    for path in &candidates {
        if !path.exists() {
            continue;
        }
        if let Ok(docker) = Docker::connect_with_socket(
            &path.to_string_lossy(),
            DOCKER_TIMEOUT_SECS,
            API_DEFAULT_VERSION,
        ) {
            tracing::debug!("using the {} socket at {}", kind.label(), path.display());
            return Ok(docker);
        }
    }

    let hint = if kind == ServiceKind::Podman {
        // Podman has no daemon: the socket only exists once the socket unit or
        // `podman machine` has been started, which is the usual cause.
        "Start it with `systemctl --user start podman.socket`, or `podman machine start` \
         on macOS. You can also set CONTAINER_HOST or enter the endpoint explicitly."
    } else {
        "Set DOCKER_HOST, or enter the endpoint explicitly on the connection."
    };

    Err(AppError::Collector(format!(
        "no {} socket found. Tried {}. {hint}",
        kind.label(),
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Docker's `timestamps: true` prefixes each line with an RFC3339 instant.
/// Falls back to "now" for the occasional frame that lacks one.
fn split_docker_timestamp(line: &str) -> (DateTime<Utc>, &str) {
    match line.split_once(' ') {
        Some((prefix, rest)) => match DateTime::parse_from_rfc3339(prefix) {
            Ok(ts) => (ts.with_timezone(&Utc), rest),
            Err(_) => (Utc::now(), line),
        },
        None => (Utc::now(), line),
    }
}

/// Convert Docker's epoch-seconds container `created` field.
#[allow(dead_code)]
pub(crate) fn epoch_to_utc(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_timestamp_prefix() {
        let (ts, msg) = split_docker_timestamp("2024-05-01T10:00:00.123456789Z hello world");
        assert_eq!(msg, "hello world");
        assert_eq!(ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true), "2024-05-01T10:00:00Z");
    }

    #[test]
    fn falls_back_when_no_timestamp() {
        let (_, msg) = split_docker_timestamp("plain line without timestamp");
        assert_eq!(msg, "plain line without timestamp");
    }

    #[test]
    fn recent_exits_are_incidents_and_old_ones_are_history() {
        assert!(exited_recently("Exited (137) 5 seconds ago"));
        assert!(exited_recently("Exited (1) About a minute ago"));
        assert!(exited_recently("Exited (137) 3 hours ago"));
        assert!(!exited_recently("Exited (137) 4 weeks ago"));
        assert!(!exited_recently("Exited (255) 3 years ago"));
        assert!(!exited_recently("Exited (1) 2 days ago"));
        // No rendered age: assume it matters rather than hide it.
        assert!(exited_recently("Exited (1)"));
    }

    #[test]
    fn annotate_does_not_duplicate_an_existing_qualifier() {
        assert_eq!(annotate("Up 2 minutes (unhealthy)", "unhealthy"), "Up 2 minutes (unhealthy)");
        assert_eq!(annotate("Up 2 minutes", "unhealthy"), "Up 2 minutes (unhealthy)");
    }

    #[test]
    fn infers_levels() {
        assert_eq!(LogLevel::infer("ERROR: connection refused"), LogLevel::Error);
        assert_eq!(LogLevel::infer("WARN slow response"), LogLevel::Warn);
        assert_eq!(LogLevel::infer("started listening on 8080"), LogLevel::Info);
    }
}
