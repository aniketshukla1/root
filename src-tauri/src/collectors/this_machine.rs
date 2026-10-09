//! This computer, read directly — no agent, no exporter, no credential.
//!
//! Every other collector reaches something over a network, which means a
//! fresh install has nothing to show until someone has an address and a
//! password to hand. This one asks the operating system, through `sysinfo`,
//! on macOS, Windows and Linux alike — so the first minute of root can be the
//! machine it is running on: CPU, memory, every disk, network throughput and
//! the processes doing the work.
//!
//! Health follows the same rules as the Linux host collector: the fullest
//! disk decides, memory counts what is available rather than what is free,
//! and a CPU at 100% is reported but is not, on its own, a fault — a laptop
//! compiling something is doing its job.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sysinfo::{Disks, Networks, ProcessesToUpdate, System};

use super::linux_host::IGNORED_FILESYSTEMS;
use super::{elapsed_ms, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

/// Processes listed, busiest first: enough to name the culprit, few enough to read.
const TOP_PROCESSES: usize = 10;

/// The operating system handles, kept between polls: CPU usage and network
/// rates are differences between two readings, so the previous one matters.
struct Probe {
    system: System,
    disks: Disks,
    networks: Networks,
    sampled: Instant,
}

/// One poll's worth of numbers, read off the probe in one go.
struct Reading {
    host: Option<String>,
    cpu: f64,
    cores: usize,
    memory_used: u64,
    memory_total: u64,
    load_one: Option<f64>,
    uptime: u64,
    disks: Vec<(String, String, u64, u64)>,
    interfaces: Vec<(String, f64, f64)>,
    processes: Vec<(u32, String, f64, u64)>,
}

pub struct ThisMachineCollector {
    probe: Arc<Mutex<Probe>>,
    profile_id: String,
    profile_name: String,
    disk_warn: f64,
    memory_warn: f64,
}

impl ThisMachineCollector {
    pub fn new(profile: &ConnectionProfile) -> Result<Self> {
        let option = |key: &str, fallback: f64| {
            profile
                .options
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(fallback)
        };
        // A first reading now, so the first poll a few seconds from now has
        // something to measure CPU and traffic against rather than reporting
        // zero.
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_processes(ProcessesToUpdate::All, true);
        Ok(ThisMachineCollector {
            probe: Arc::new(Mutex::new(Probe {
                system,
                disks: Disks::new_with_refreshed_list(),
                networks: Networks::new_with_refreshed_list(),
                sampled: Instant::now(),
            })),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            disk_warn: option("diskWarn", 90.0),
            memory_warn: option("memoryWarn", 90.0),
        })
    }
}

/// Read everything once. Blocking — the caller runs it off the async runtime.
fn read(probe: &mut Probe) -> Reading {
    probe.system.refresh_cpu_usage();
    probe.system.refresh_memory();
    probe.system.refresh_processes(ProcessesToUpdate::All, true);
    probe.disks.refresh(true);
    probe.networks.refresh(true);
    let seconds = probe.sampled.elapsed().as_secs_f64().max(0.001);
    probe.sampled = Instant::now();

    let cores = probe.system.cpus().len().max(1);
    let load = System::load_average();

    let mut disks: Vec<(String, String, u64, u64)> = probe
        .disks
        .list()
        .iter()
        .filter(|d| d.total_space() > 0)
        // Read-only volumes are full by design and never fill further: the
        // sealed system volume macOS mounts at `/` reads 100% on every Mac,
        // and would make every one of them look down. Its writes land on the
        // data volume, which is listed and measured instead.
        .filter(|d| !d.is_read_only())
        .filter(|d| !IGNORED_FILESYSTEMS.contains(&d.file_system().to_string_lossy().as_ref()))
        .map(|d| {
            (
                d.mount_point().to_string_lossy().into_owned(),
                d.file_system().to_string_lossy().into_owned(),
                d.total_space(),
                d.available_space(),
            )
        })
        .collect();
    disks.sort_by(|a, b| a.0.cmp(&b.0));
    // One volume mounted twice (macOS lists its data volume under several
    // paths) is one disk to an operator.
    disks.dedup_by(|a, b| a.2 == b.2 && a.3 == b.3 && a.1 == b.1);

    let mut interfaces: Vec<(String, f64, f64)> = probe
        .networks
        .list()
        .iter()
        .filter(|(name, _)| !name.starts_with("lo"))
        .map(|(name, data)| {
            (
                name.clone(),
                data.received() as f64 / seconds,
                data.transmitted() as f64 / seconds,
            )
        })
        .collect();
    interfaces.sort_by(|a, b| a.0.cmp(&b.0));

    let mut processes: Vec<(u32, String, f64, u64)> = probe
        .system
        .processes()
        .iter()
        .map(|(pid, p)| {
            (
                pid.as_u32(),
                p.name().to_string_lossy().into_owned(),
                // A process's figure is a share of one core; the machine's is
                // a share of all of them, and the two must read on one scale.
                p.cpu_usage() as f64 / cores as f64,
                p.memory(),
            )
        })
        .collect();
    processes.sort_by(|a, b| b.2.total_cmp(&a.2).then(b.3.cmp(&a.3)));
    processes.truncate(TOP_PROCESSES);

    Reading {
        host: System::host_name(),
        cpu: probe.system.global_cpu_usage() as f64,
        cores,
        memory_used: probe.system.total_memory().saturating_sub(probe.system.available_memory()),
        memory_total: probe.system.total_memory(),
        // Windows has no load average and reports zeros, which would read as
        // an idle machine rather than an unanswerable question.
        load_one: (!cfg!(windows)).then_some(load.one),
        uptime: System::uptime(),
        disks,
        interfaces,
        processes,
    }
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64 * 100.0
    }
}

#[async_trait]
impl Collector for ThisMachineCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::ThisMachine
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let started = Instant::now();
        let probe = Arc::clone(&self.probe);
        let r = tokio::task::spawn_blocking(move || {
            let mut guard = probe.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            read(&mut guard)
        })
        .await
        .map_err(|e| AppError::Collector(format!("reading this computer failed: {e}")))?;

        let memory_pct = percent(r.memory_used, r.memory_total);
        let fullest = r
            .disks
            .iter()
            .map(|(mount, _, total, free)| (mount, percent(total - free.min(total), *total)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let disk_pct = fullest.map(|(_, pct)| pct);

        let health = if disk_pct.is_some_and(|p| p >= 98.0) {
            Health::Down
        } else if disk_pct.is_some_and(|p| p >= self.disk_warn) || memory_pct >= self.memory_warn {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let mut summary = format!("CPU {:.0}% · memory {memory_pct:.0}%", r.cpu);
        if let Some((mount, pct)) = fullest {
            summary.push_str(&format!(" · fullest disk {pct:.0}% ({mount})"));
        }

        let rx: f64 = r.interfaces.iter().map(|i| i.1).sum();
        let tx: f64 = r.interfaces.iter().map(|i| i.2).sum();
        let mut metrics = vec![
            Metric::new("cpu", "CPU", r.cpu, MetricUnit::Percent).warn_above(90.0),
            Metric::new("memory_used", "Memory used", memory_pct, MetricUnit::Percent)
                .warn_above(self.memory_warn),
            Metric::new(
                "memory_available",
                "Memory available",
                r.memory_total.saturating_sub(r.memory_used) as f64,
                MetricUnit::Bytes,
            ),
            Metric::new("net_rx", "Network in", rx, MetricUnit::BytesPerSecond),
            Metric::new("net_tx", "Network out", tx, MetricUnit::BytesPerSecond),
            Metric::new("cores", "Cores", r.cores as f64, MetricUnit::Count),
            Metric::new("uptime", "Uptime", r.uptime as f64, MetricUnit::Seconds),
        ];
        if let Some(pct) = disk_pct {
            metrics.insert(
                2,
                Metric::new("disk_fullest", "Fullest disk", pct, MetricUnit::Percent)
                    .warn_above(self.disk_warn),
            );
        }
        if let Some(load) = r.load_one {
            metrics.push(Metric::new(
                "load_per_core",
                "Load per core",
                load / r.cores as f64,
                MetricUnit::Count,
            ));
        }

        // Disks and interfaces are few and long-lived, so they keep history;
        // processes come and go and are listed only as they are now.
        let mut disks = Section::new(
            "disks",
            "Disks",
            vec![
                ColumnSpec::text("mount", "Mount").primary(),
                ColumnSpec::text("fs", "File system").secondary(),
                ColumnSpec::gauge("used", "Used", MetricUnit::Percent),
                ColumnSpec::metric("free", "Free", MetricUnit::Bytes),
            ],
        )
        .tracked();
        disks.rows = r
            .disks
            .iter()
            .map(|(mount, fs, total, free)| {
                let used = percent(total - (*free).min(*total), *total);
                let health = if used >= 98.0 {
                    Health::Down
                } else if used >= self.disk_warn {
                    Health::Degraded
                } else {
                    Health::Healthy
                };
                Row::new(mount.clone(), mount.clone(), health)
                    .summary(format!("{used:.0}% used"))
                    .cell("mount", CellValue::text(mount.clone()))
                    .cell("fs", CellValue::text(fs.clone()))
                    .cell(
                        "used",
                        CellValue::Gauge {
                            value: used,
                            max: Some(100.0),
                            warn_above: Some(self.disk_warn),
                        },
                    )
                    .cell("free", CellValue::number(*free as f64))
            })
            .collect();

        let mut network = Section::new(
            "interfaces",
            "Network",
            vec![
                ColumnSpec::text("name", "Interface").primary(),
                ColumnSpec::metric("rx", "In", MetricUnit::BytesPerSecond),
                ColumnSpec::metric("tx", "Out", MetricUnit::BytesPerSecond),
            ],
        )
        .tracked();
        network.rows = r
            .interfaces
            .iter()
            .map(|(name, rx, tx)| {
                Row::new(name.clone(), name.clone(), Health::Healthy)
                    .cell("name", CellValue::text(name.clone()))
                    .cell("rx", CellValue::number(*rx))
                    .cell("tx", CellValue::number(*tx))
            })
            .collect();

        let mut processes = Section::new(
            "processes",
            "Processes",
            vec![
                ColumnSpec::text("name", "Process").primary(),
                ColumnSpec::text("pid", "PID").secondary(),
                ColumnSpec::metric("cpu", "CPU", MetricUnit::Percent),
                ColumnSpec::metric("memory", "Memory", MetricUnit::Bytes),
            ],
        );
        processes.rows = r
            .processes
            .iter()
            .map(|(pid, name, cpu, memory)| {
                Row::new(pid.to_string(), name.clone(), Health::Healthy)
                    .cell("name", CellValue::text(name.clone()))
                    .cell("pid", CellValue::text(pid.to_string()))
                    .cell("cpu", CellValue::number(*cpu))
                    .cell("memory", CellValue::number(*memory as f64))
            })
            .collect();

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: r.host.filter(|_| self.profile_name.is_empty()).unwrap_or_else(|| self.profile_name.clone()),
            kind: ServiceKind::ThisMachine,
            health,
            summary,
            metrics,
            sections: vec![disks, network, processes],
            checked_at: Utc::now(),
            latency_ms: elapsed_ms(started),
            error: None,
        })
    }

    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_this_machine_with_nothing_configured() {
        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "local",
            "name": "This computer",
            "kind": "this_machine",
            "endpoint": "",
        }))
        .unwrap();
        let collector = ThisMachineCollector::new(&profile).unwrap();
        let snapshot = collector.collect().await.unwrap();
        let metric = |key: &str| snapshot.metrics.iter().find(|m| m.key == key).map(|m| m.value);
        assert!(metric("memory_used").is_some_and(|m| m > 0.0 && m <= 100.0), "{:?}", snapshot.metrics);
        assert!(metric("cores").is_some_and(|c| c >= 1.0));
        assert!(metric("uptime").is_some_and(|u| u > 0.0));
        let processes = snapshot.sections.iter().find(|s| s.id == "processes").unwrap();
        assert!(!processes.rows.is_empty(), "a running machine has processes");
        assert!(processes.rows.len() <= TOP_PROCESSES);
    }
}
