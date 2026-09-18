//! Linux host monitor, via node_exporter.
//!
//! A VM or bare-metal server is the one thing in an estate with no API of its
//! own. Reaching it means either an agent or a shell, and this collector takes
//! the agent: node_exporter is already installed on most Linux fleets, it is
//! read-only by construction, and it needs no credential of the kind an SSH
//! login would.
//!
//! That is a real constraint and it is stated in the connection form rather
//! than discovered later: without node_exporter listening, root cannot see
//! this host. The alternative — holding SSH keys for every server in the
//! estate — buys broader reach at the cost of making the monitoring tool the
//! most valuable credential store on the network. This is the trade this
//! collector makes.
//!
//! What it reads is chosen for what actually pages someone at three in the
//! morning:
//!
//! **Disk fill, per mount.** The single most common host outage, and the one
//! an average hides: a root filesystem at 99% while a data volume sits at 3%
//! averages to comfortable, and only one of them stops the machine. Mounts are
//! reported individually and the fullest decides health.
//!
//! **Memory available, not memory free.** `MemFree` on a healthy Linux box is
//! near zero, because the kernel uses everything spare for page cache and
//! gives it back on demand. Alerting on free memory means alerting constantly
//! on machines that are fine. `MemAvailable` is the kernel's own estimate of
//! what a new process could actually get, and it is the only one worth
//! watching.
//!
//! **Load per core.** A load average of 8 is idle on a 16-core box and a
//! queue forming on a 4-core one. The core count comes from the same scrape,
//! so the ratio is knowable rather than assumed.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::promtext::{gauge, parse_prometheus, Sample};
use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Filesystems that are not the operator's problem.
///
/// node_exporter reports every mount the kernel knows about, and on a
/// container host most of them are overlays, tmpfs and squashfs images that
/// are full by design. A read-only squashfs at 100% is not an incident; it is
/// what a squashfs is.
const IGNORED_FILESYSTEMS: &[&str] = &[
    "tmpfs", "devtmpfs", "overlay", "squashfs", "ramfs", "iso9660", "autofs", "cgroup", "cgroup2",
    "proc", "sysfs", "devpts", "fuse.lxcfs", "nsfs", "efivarfs", "debugfs", "tracefs",
];

pub struct LinuxHostCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    disk_warn: f64,
    memory_warn: f64,
    /// Load average per core above which the host is degraded.
    load_warn: f64,
    profile_id: String,
    profile_name: String,
}

/// One mounted filesystem worth reporting.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mount {
    pub mountpoint: String,
    pub device: String,
    pub fstype: String,
    pub size: f64,
    pub available: f64,
}

impl Mount {
    /// Percent of the filesystem in use.
    ///
    /// Measured against `size - available` rather than the `free` series:
    /// `free` includes the root-reserved blocks that an ordinary process
    /// cannot touch, so a filesystem reports space that nothing can allocate.
    /// The number that matters is what a normal write would find.
    pub fn used_percent(&self) -> Option<f64> {
        (self.size > 0.0).then(|| ((self.size - self.available) / self.size) * 100.0)
    }
}

/// Filesystems a person is responsible for, largest fill first.
pub(crate) fn mounts(samples: &[Sample]) -> Vec<Mount> {
    let mut by_mount: BTreeMap<String, Mount> = BTreeMap::new();

    for sample in samples {
        let is_size = sample.name == "node_filesystem_size_bytes";
        let is_avail = sample.name == "node_filesystem_avail_bytes";
        if !is_size && !is_avail {
            continue;
        }
        let Some(mountpoint) = sample.labels.get("mountpoint") else {
            continue;
        };
        let fstype = sample.labels.get("fstype").cloned().unwrap_or_default();
        // Network mounts belong to the NFS collector, not to this one. A
        // machine's disks and the shared storage it happens to mount are
        // different questions with different owners, and mixing them
        // attributes the wrong failure: a stale export from a server across
        // the datacentre would otherwise make this host read as down when
        // nothing about the host is wrong.
        if IGNORED_FILESYSTEMS.contains(&fstype.as_str())
            || super::nfs::NFS_TYPES.contains(&fstype.as_str())
        {
            continue;
        }

        let entry = by_mount.entry(mountpoint.clone()).or_insert_with(|| Mount {
            mountpoint: mountpoint.clone(),
            device: sample.labels.get("device").cloned().unwrap_or_default(),
            fstype: fstype.clone(),
            size: 0.0,
            available: 0.0,
        });
        if is_size {
            entry.size = sample.value;
        } else {
            entry.available = sample.value;
        }
    }

    let mut out: Vec<Mount> = by_mount.into_values().filter(|m| m.size > 0.0).collect();
    out.sort_by(|a, b| {
        b.used_percent()
            .unwrap_or(0.0)
            .total_cmp(&a.used_percent().unwrap_or(0.0))
    });
    out
}

/// Logical CPUs, counted from the per-CPU idle series.
///
/// node_exporter has no "how many cores" gauge; the count of distinct `cpu`
/// labels is the answer, and without it a load average means nothing.
pub(crate) fn cpu_count(samples: &[Sample]) -> Option<f64> {
    let count = samples
        .iter()
        .filter(|s| s.name == "node_cpu_seconds_total")
        .filter(|s| s.labels.get("mode").map(String::as_str) == Some("idle"))
        .filter_map(|s| s.labels.get("cpu"))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    (count > 0).then_some(count as f64)
}

/// Memory a new process could actually get, as a percentage of the total.
///
/// `MemAvailable` rather than `MemFree` — see the module doc.
pub(crate) fn memory_used_percent(samples: &[Sample]) -> Option<f64> {
    let total = gauge(samples, "node_memory_MemTotal_bytes")?;
    let available = gauge(samples, "node_memory_MemAvailable_bytes")?;
    (total > 0.0).then(|| ((total - available) / total) * 100.0)
}

impl LinuxHostCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim().trim_end_matches('/');
        if raw.is_empty() {
            return Err(AppError::Config(
                "Linux host connections need the node_exporter URL, e.g. \
                 http://web-1.internal:9100"
                    .into(),
            ));
        }
        // Accept a bare host, since 9100 is node_exporter's only conventional
        // port and typing it every time is friction with no upside.
        let with_scheme = if raw.contains("://") {
            raw.to_string()
        } else if raw.contains(':') {
            format!("http://{raw}")
        } else {
            format!("http://{raw}:9100")
        };
        let base = with_scheme.trim_end_matches("/metrics").to_string();

        let insecure = profile
            .options
            .get("insecureTls")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let option = |key: &str, fallback: f64| {
            profile
                .options
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(fallback)
        };

        Ok(LinuxHostCollector {
            client: http_client(insecure, REQUEST_TIMEOUT)?,
            base,
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            disk_warn: option("diskWarn", 85.0),
            memory_warn: option("memoryWarn", 90.0),
            load_warn: option("loadWarn", 1.5),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }
}

#[async_trait]
impl Collector for LinuxHostCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::LinuxHost
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();
        let url = format!("{}/metrics", self.base);

        let request = match (&self.username, &self.secret) {
            (Some(user), secret) => self.client.get(&url).basic_auth(user, secret.as_ref()),
            _ => self.client.get(&url),
        };
        let body = match request.send().await {
            Ok(response) if response.status().is_success() => {
                response.text().await.unwrap_or_default()
            }
            Ok(response) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::LinuxHost,
                    format!("{url} returned {}", response.status()),
                    elapsed_ms(start),
                ));
            }
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::LinuxHost,
                    format!("{url}: {e} — this host needs node_exporter listening"),
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let samples = parse_prometheus(&body);
        if gauge(&samples, "node_memory_MemTotal_bytes").is_none()
            && cpu_count(&samples).is_none()
        {
            return Ok(HealthSnapshot::unreachable(
                &self.profile_id,
                &self.profile_name,
                ServiceKind::LinuxHost,
                format!(
                    "{url} answered, but with no node_exporter metrics — another exporter on \
                     the same port will not carry host CPU, memory or disk"
                ),
                latency,
            ));
        }

        let filesystems = mounts(&samples);
        let fullest = filesystems.first().and_then(|m| m.used_percent().map(|p| (m, p)));
        let memory = memory_used_percent(&samples);
        let cores = cpu_count(&samples);
        let load1 = gauge(&samples, "node_load1");
        let load_per_core = match (load1, cores) {
            (Some(load), Some(cores)) if cores > 0.0 => Some(load / cores),
            _ => None,
        };

        let disk_critical = fullest.is_some_and(|(_, pct)| pct >= 98.0);
        let disk_high = fullest.is_some_and(|(_, pct)| pct >= self.disk_warn);
        let memory_high = memory.is_some_and(|pct| pct >= self.memory_warn);
        let load_high = load_per_core.is_some_and(|per| per >= self.load_warn);

        let health = if disk_critical {
            Health::Down
        } else if disk_high || memory_high || load_high {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = match fullest {
            Some((mount, pct)) if disk_high => format!(
                "{} is {pct:.0}% full — {} free",
                mount.mountpoint,
                human_bytes(mount.available)
            ),
            _ if memory_high => format!(
                "{:.0}% of memory in use, {} available",
                memory.unwrap_or(0.0),
                human_bytes(gauge(&samples, "node_memory_MemAvailable_bytes").unwrap_or(0.0))
            ),
            _ if load_high => format!(
                "load {:.2} across {:.0} cores — {:.2} per core",
                load1.unwrap_or(0.0),
                cores.unwrap_or(0.0),
                load_per_core.unwrap_or(0.0)
            ),
            _ => {
                let uptime = gauge(&samples, "node_time_seconds")
                    .zip(gauge(&samples, "node_boot_time_seconds"))
                    .map(|(now, boot)| now - boot);
                match (cores, uptime) {
                    (Some(cores), Some(up)) => format!(
                        "{cores:.0} cores · up {} · {} filesystems",
                        human_duration(up),
                        filesystems.len()
                    ),
                    _ => format!("{} filesystems", filesystems.len()),
                }
            }
        };

        let mut metrics = Vec::new();
        if let Some((_, pct)) = fullest {
            metrics.push(
                Metric::new("disk_fullest", "Fullest filesystem", pct, MetricUnit::Percent)
                    .warn_above(self.disk_warn),
            );
        }
        if let Some(pct) = memory {
            metrics.push(
                Metric::new("memory_used", "Memory used", pct, MetricUnit::Percent)
                    .warn_above(self.memory_warn),
            );
        }
        if let Some(available) = gauge(&samples, "node_memory_MemAvailable_bytes") {
            metrics.push(Metric::new(
                "memory_available",
                "Memory available",
                available,
                MetricUnit::Bytes,
            ));
        }
        if let Some(per) = load_per_core {
            metrics.push(
                Metric::new("load_per_core", "Load per core", per, MetricUnit::Count)
                    .warn_above(self.load_warn),
            );
        }
        for (key, label, name) in [
            ("load1", "Load (1m)", "node_load1"),
            ("load5", "Load (5m)", "node_load5"),
            ("load15", "Load (15m)", "node_load15"),
        ] {
            if let Some(value) = gauge(&samples, name) {
                metrics.push(Metric::new(key, label, value, MetricUnit::Count));
            }
        }
        if let Some(cores) = cores {
            metrics.push(Metric::new("cores", "Logical CPUs", cores, MetricUnit::Count));
        }
        if let Some(up) = gauge(&samples, "node_time_seconds")
            .zip(gauge(&samples, "node_boot_time_seconds"))
            .map(|(now, boot)| now - boot)
        {
            metrics.push(Metric::new("uptime", "Uptime", up, MetricUnit::Seconds));
        }

        let disk_rows: Vec<Row> = filesystems
            .iter()
            .map(|mount| {
                let pct = mount.used_percent().unwrap_or(0.0);
                let state = if pct >= 98.0 {
                    Health::Down
                } else if pct >= self.disk_warn {
                    Health::Degraded
                } else {
                    Health::Healthy
                };
                Row::new(
                    format!("mount:{}", mount.mountpoint),
                    mount.mountpoint.clone(),
                    state,
                )
                .summary(format!(
                    "{} free of {} · {}",
                    human_bytes(mount.available),
                    human_bytes(mount.size),
                    mount.device
                ))
                .cell("mount", CellValue::text(mount.mountpoint.clone()))
                .cell("state", CellValue::status(state))
                .cell(
                    "used",
                    CellValue::gauge_warn(
                        mount.size - mount.available,
                        Some(mount.size),
                        mount.size * (self.disk_warn / 100.0),
                    ),
                )
                .cell("free", CellValue::number(mount.available))
                .cell("fstype", CellValue::text(mount.fstype.clone()))
            })
            .collect();

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::LinuxHost,
            health,
            summary,
            metrics,
            sections: vec![Section::new(
                "filesystems",
                "Filesystems",
                vec![
                    ColumnSpec::text("mount", "Mount").primary(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::gauge("used", "Used", MetricUnit::Bytes),
                    ColumnSpec::metric("free", "Free", MetricUnit::Bytes),
                    ColumnSpec::text("fstype", "Type"),
                ],
            )
            .rows(disk_rows)
            .tracked()
            .empty_text(
                "node_exporter reported no real filesystems — only virtual ones, which are full \
                 by design and not worth watching.",
            )
            .priority(10)],
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// The host's journal is not something node_exporter serves. Reading it
    /// would need the shell access this collector deliberately does without.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn human_duration(seconds: f64) -> String {
    let days = (seconds / 86_400.0).floor();
    if days >= 1.0 {
        return format!("{days:.0}d");
    }
    let hours = (seconds / 3_600.0).floor();
    if hours >= 1.0 {
        return format!("{hours:.0}h");
    }
    format!("{:.0}m", (seconds / 60.0).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: &str = "node_memory_MemTotal_bytes 1.7179869184e+10\n\
                        node_memory_MemFree_bytes 2.68435456e+08\n\
                        node_memory_MemAvailable_bytes 8.589934592e+09\n\
                        node_load1 6.5\n\
                        node_load5 4.2\n\
                        node_load15 3.1\n\
                        node_cpu_seconds_total{cpu=\"0\",mode=\"idle\"} 100\n\
                        node_cpu_seconds_total{cpu=\"1\",mode=\"idle\"} 100\n\
                        node_cpu_seconds_total{cpu=\"2\",mode=\"idle\"} 100\n\
                        node_cpu_seconds_total{cpu=\"3\",mode=\"idle\"} 100\n\
                        node_cpu_seconds_total{cpu=\"0\",mode=\"user\"} 20\n\
                        node_filesystem_size_bytes{device=\"/dev/sda1\",fstype=\"ext4\",mountpoint=\"/\"} 1.0e+10\n\
                        node_filesystem_avail_bytes{device=\"/dev/sda1\",fstype=\"ext4\",mountpoint=\"/\"} 1.0e+08\n\
                        node_filesystem_size_bytes{device=\"/dev/sdb1\",fstype=\"ext4\",mountpoint=\"/data\"} 1.0e+12\n\
                        node_filesystem_avail_bytes{device=\"/dev/sdb1\",fstype=\"ext4\",mountpoint=\"/data\"} 9.7e+11\n\
                        node_filesystem_size_bytes{device=\"overlay\",fstype=\"overlay\",mountpoint=\"/var/lib/docker/overlay2/x\"} 1.0e+10\n\
                        node_filesystem_avail_bytes{device=\"overlay\",fstype=\"overlay\",mountpoint=\"/var/lib/docker/overlay2/x\"} 0\n";

    #[test]
    fn counts_logical_cpus_from_the_idle_series() {
        let samples = parse_prometheus(NODE);
        assert_eq!(
            cpu_count(&samples),
            Some(4.0),
            "one per distinct cpu label, counted once despite several modes",
        );
    }

    /// The reason load is reported per core: 6.5 is a queue on 4 cores and
    /// nothing at all on 32.
    #[test]
    fn load_is_meaningless_without_the_core_count() {
        let samples = parse_prometheus(NODE);
        let per_core = gauge(&samples, "node_load1").unwrap() / cpu_count(&samples).unwrap();
        assert!((per_core - 1.625).abs() < 0.001, "{per_core}");
    }

    /// MemFree is near zero on a healthy Linux box because the kernel uses the
    /// spare for cache. Alerting on it would page constantly.
    #[test]
    fn memory_is_measured_by_available_not_free() {
        let samples = parse_prometheus(NODE);
        let by_available = memory_used_percent(&samples).unwrap();
        assert!((by_available - 50.0).abs() < 0.001, "{by_available}");

        let total = gauge(&samples, "node_memory_MemTotal_bytes").unwrap();
        let free = gauge(&samples, "node_memory_MemFree_bytes").unwrap();
        let by_free = ((total - free) / total) * 100.0;
        assert!(
            by_free > 98.0,
            "free would read {by_free:.0}% used on a machine that is fine",
        );
    }

    /// The average hides the outage: a root filesystem at 99% next to a data
    /// volume at 3% averages to comfortable, and only one of them stops the
    /// machine.
    #[test]
    fn the_fullest_filesystem_decides_not_the_average() {
        let samples = parse_prometheus(NODE);
        let found = mounts(&samples);
        assert_eq!(found[0].mountpoint, "/");
        assert!((found[0].used_percent().unwrap() - 99.0).abs() < 0.001);
        assert!(found[1].used_percent().unwrap() < 5.0);
    }

    /// A full overlay or squashfs is what those are; reporting them would bury
    /// the real mounts under noise that is never actionable.
    #[test]
    fn virtual_filesystems_are_left_out() {
        let found = mounts(&parse_prometheus(NODE));
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|m| m.fstype == "ext4"));
    }

    #[test]
    fn a_filesystem_with_no_size_is_not_reported_as_full() {
        let samples = parse_prometheus(
            "node_filesystem_size_bytes{fstype=\"ext4\",mountpoint=\"/x\"} 0\n\
             node_filesystem_avail_bytes{fstype=\"ext4\",mountpoint=\"/x\"} 0\n",
        );
        assert!(mounts(&samples).is_empty());
    }

    #[test]
    fn a_body_that_is_not_node_exporter_yields_nothing() {
        let samples = parse_prometheus("etcd_server_has_leader 1\n");
        assert_eq!(cpu_count(&samples), None);
        assert_eq!(memory_used_percent(&samples), None);
        assert!(mounts(&samples).is_empty());
    }
}
