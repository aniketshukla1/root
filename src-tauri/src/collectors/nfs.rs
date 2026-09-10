//! NFS export monitor, via node_exporter on a client that mounts them.
//!
//! Shared storage handed out per team is its own kind of problem, and three of
//! its failure modes are ones a disk check gets wrong.
//!
//! **A stale mount reports zeroes.** When an NFS server re-exports a
//! filesystem, or a mount is severed while the client still holds it, every
//! operation on that path fails with ESTALE. The mount is still listed. `df`
//! either hangs or answers with zeros, and zero size with zero used reads as
//! either "empty, plenty of room" or "0% of 0, full" depending on which way the
//! arithmetic falls. Neither happened. node_exporter sets
//! `node_filesystem_device_error` to 1 when it could not stat the filesystem at
//! all, and that flag is the only thing separating *this export is fine and
//! empty* from *root could not ask*. It is checked before any fill number is
//! believed.
//!
//! **Inodes run out while `df -h` says there is room.** An export holding
//! millions of small files — build artefacts, per-request logs, a maildir —
//! exhausts its inode table long before its bytes. Writes start failing with
//! `No space left on device` while every byte-based dashboard shows forty per
//! cent free, and the person on call reasonably concludes the error is lying.
//! Inodes are reported beside bytes, and either one filling is a fault.
//!
//! **One server failing looks like every team failing.** A host mounting six
//! exports from `nfs-01` shows six broken paths when `nfs-01` goes away. They
//! are grouped by the server they come from, so the row that matters is the
//! server and not the six teams downstream of it — the same discipline the
//! incident correlator applies to alerts.
//!
//! This reads node_exporter on a client that has the exports mounted, not the
//! NFS server itself. That is deliberate: what matters operationally is
//! whether the team's path works *from where the team uses it*, and a server
//! can be perfectly healthy while a client's mount is stale.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::promtext::{parse_prometheus, Sample};
use super::{elapsed_ms, http_client, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Every filesystem type that is an NFS mount.
pub(crate) const NFS_TYPES: &[&str] = &["nfs", "nfs3", "nfs4", "nfsd"];

pub struct NfsCollector {
    client: reqwest::Client,
    base: String,
    username: Option<String>,
    secret: Option<String>,
    /// Only mounts under this path. Blank means every NFS mount on the host.
    path_prefix: String,
    fill_warn: f64,
    inode_warn: f64,
    profile_id: String,
    profile_name: String,
}

/// One NFS mount as the client sees it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Export {
    pub mountpoint: String,
    /// `nfs-01:/exports/team-a` — server and export path.
    pub device: String,
    pub fstype: String,
    pub size: f64,
    pub available: f64,
    pub inodes: f64,
    pub inodes_free: f64,
    /// node_exporter could not stat this filesystem: stale handle, or a server
    /// that never answered.
    pub unreachable: bool,
    pub readonly: bool,
}

impl Export {
    /// The server half of `server:/export`.
    ///
    /// Falls back to the whole device string: a mount written some other way
    /// still needs a stable grouping key, and inventing "unknown" would put
    /// unrelated exports in one bucket.
    pub fn server(&self) -> &str {
        self.device
            .split_once(':')
            .map(|(server, _)| server)
            .unwrap_or(&self.device)
    }

    /// Percent of bytes used.
    ///
    /// `None` when the mount is unreachable or reports no size. A stale mount
    /// answers with zeros, and turning that into a percentage is how "root
    /// could not ask" becomes "0% used, plenty of room".
    pub fn used_percent(&self) -> Option<f64> {
        if self.unreachable || self.size <= 0.0 {
            return None;
        }
        Some(((self.size - self.available) / self.size) * 100.0)
    }

    /// Percent of inodes used.
    ///
    /// The number that explains an ENOSPC on an export with free bytes.
    pub fn inodes_used_percent(&self) -> Option<f64> {
        if self.unreachable || self.inodes <= 0.0 {
            return None;
        }
        Some(((self.inodes - self.inodes_free) / self.inodes) * 100.0)
    }

    /// The worse of the two fills, since either one filling stops writes.
    pub fn worst_fill(&self) -> Option<f64> {
        match (self.used_percent(), self.inodes_used_percent()) {
            (Some(bytes), Some(inodes)) => Some(bytes.max(inodes)),
            (Some(bytes), None) => Some(bytes),
            (None, Some(inodes)) => Some(inodes),
            (None, None) => None,
        }
    }
}

/// Read every NFS mount out of a node_exporter scrape.
pub(crate) fn exports(samples: &[Sample], prefix: &str) -> Vec<Export> {
    let mut by_mount: BTreeMap<String, Export> = BTreeMap::new();

    for sample in samples {
        let field = match sample.name.as_str() {
            "node_filesystem_size_bytes" => "size",
            "node_filesystem_avail_bytes" => "avail",
            "node_filesystem_files" => "inodes",
            "node_filesystem_files_free" => "inodes_free",
            "node_filesystem_device_error" => "error",
            "node_filesystem_readonly" => "readonly",
            _ => continue,
        };

        let Some(fstype) = sample.labels.get("fstype") else {
            continue;
        };
        if !NFS_TYPES.contains(&fstype.as_str()) {
            continue;
        }
        let Some(mountpoint) = sample.labels.get("mountpoint") else {
            continue;
        };
        if !prefix.is_empty() && !mountpoint.starts_with(prefix) {
            continue;
        }

        let entry = by_mount.entry(mountpoint.clone()).or_insert_with(|| Export {
            mountpoint: mountpoint.clone(),
            device: sample.labels.get("device").cloned().unwrap_or_default(),
            fstype: fstype.clone(),
            size: 0.0,
            available: 0.0,
            inodes: 0.0,
            inodes_free: 0.0,
            unreachable: false,
            readonly: false,
        });

        match field {
            "size" => entry.size = sample.value,
            "avail" => entry.available = sample.value,
            "inodes" => entry.inodes = sample.value,
            "inodes_free" => entry.inodes_free = sample.value,
            "error" => entry.unreachable = sample.value > 0.0,
            "readonly" => entry.readonly = sample.value > 0.0,
            _ => {}
        }
    }

    // Unreachable first — a mount nobody can read outranks one that is merely
    // full — then fullest, then by name so the order is stable between polls.
    let mut out: Vec<Export> = by_mount.into_values().collect();
    out.sort_by(|a, b| {
        b.unreachable
            .cmp(&a.unreachable)
            .then_with(|| {
                b.worst_fill()
                    .unwrap_or(-1.0)
                    .total_cmp(&a.worst_fill().unwrap_or(-1.0))
            })
            .then_with(|| a.mountpoint.cmp(&b.mountpoint))
    });
    out
}

/// Servers with at least one unreachable mount, and how many each has.
///
/// One entry per server rather than per mount: six broken paths from one
/// vanished server is one problem, and reporting six would send somebody to
/// look at six teams.
pub(crate) fn failed_servers(exports: &[Export]) -> BTreeMap<&str, usize> {
    let mut out: BTreeMap<&str, usize> = BTreeMap::new();
    for export in exports.iter().filter(|e| e.unreachable) {
        *out.entry(export.server()).or_insert(0) += 1;
    }
    out
}

impl NfsCollector {
    pub fn new(profile: &ConnectionProfile, secret: Option<String>) -> Result<Self> {
        let raw = profile.endpoint.trim().trim_end_matches('/');
        if raw.is_empty() {
            return Err(AppError::Config(
                "NFS connections need the node_exporter URL of a client that mounts the \
                 exports, e.g. http://build-1.internal:9100"
                    .into(),
            ));
        }
        let with_scheme = if raw.contains("://") {
            raw.to_string()
        } else if raw.contains(':') {
            format!("http://{raw}")
        } else {
            format!("http://{raw}:9100")
        };

        let option = |key: &str, fallback: f64| {
            profile
                .options
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(fallback)
        };

        Ok(NfsCollector {
            client: http_client(
                profile
                    .options
                    .get("insecureTls")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                REQUEST_TIMEOUT,
            )?,
            base: with_scheme.trim_end_matches("/metrics").to_string(),
            username: profile.username.clone().filter(|u| !u.trim().is_empty()),
            secret,
            path_prefix: profile
                .options
                .get("pathPrefix")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            fill_warn: option("fillWarn", 85.0),
            inode_warn: option("inodeWarn", 85.0),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }
}

#[async_trait]
impl Collector for NfsCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Nfs
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
                    ServiceKind::Nfs,
                    format!("{url} returned {}", response.status()),
                    elapsed_ms(start),
                ));
            }
            Err(e) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Nfs,
                    format!("{url}: {e} — this client needs node_exporter listening"),
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let samples = parse_prometheus(&body);
        let mounts = exports(&samples, &self.path_prefix);

        if mounts.is_empty() {
            // No NFS mounts is a finding, not a failure. Saying which of the
            // two it is saves someone checking the exporter.
            let has_filesystems = samples
                .iter()
                .any(|s| s.name == "node_filesystem_size_bytes");
            let detail = if !has_filesystems {
                "no filesystem metrics at all — check node_exporter's filesystem collector is enabled"
            } else if self.path_prefix.is_empty() {
                "node_exporter is reporting filesystems, but none of them are NFS"
            } else {
                "node_exporter is reporting NFS mounts, but none under this path prefix"
            };
            return Ok(HealthSnapshot {
                profile_id: self.profile_id.clone(),
                profile_name: self.profile_name.clone(),
                kind: ServiceKind::Nfs,
                health: Health::Unknown,
                summary: detail.to_string(),
                metrics: Vec::new(),
                sections: Vec::new(),
                checked_at: Utc::now(),
                latency_ms: latency,
                error: None,
            });
        }

        let dead_servers = failed_servers(&mounts);
        let unreachable: Vec<&Export> = mounts.iter().filter(|e| e.unreachable).collect();
        let readonly: Vec<&Export> = mounts.iter().filter(|e| e.readonly && !e.unreachable).collect();

        let byte_full: Vec<&Export> = mounts
            .iter()
            .filter(|e| e.used_percent().is_some_and(|p| p >= self.fill_warn))
            .collect();
        let inode_full: Vec<&Export> = mounts
            .iter()
            .filter(|e| e.inodes_used_percent().is_some_and(|p| p >= self.inode_warn))
            .collect();

        let health = if !unreachable.is_empty() {
            Health::Down
        } else if !byte_full.is_empty() || !inode_full.is_empty() || !readonly.is_empty() {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if !dead_servers.is_empty() {
            let (server, count) = dead_servers.iter().next().expect("non-empty");
            if dead_servers.len() == 1 && *count > 1 {
                format!("{server} is not answering — {count} mounts from it are unusable")
            } else if dead_servers.len() == 1 {
                format!("{} is stale or unreachable", unreachable[0].mountpoint)
            } else {
                format!("{} servers not answering", dead_servers.len())
            }
        } else if !inode_full.is_empty() {
            // Named explicitly, because the byte view will not explain it.
            let worst = inode_full[0];
            format!(
                "{} is out of inodes ({:.0}% used) — writes will fail with no space while {} of \
                 bytes are still free",
                worst.mountpoint,
                worst.inodes_used_percent().unwrap_or(0.0),
                human_bytes(worst.available)
            )
        } else if !byte_full.is_empty() {
            let worst = byte_full[0];
            format!(
                "{} is {:.0}% full — {} free",
                worst.mountpoint,
                worst.used_percent().unwrap_or(0.0),
                human_bytes(worst.available)
            )
        } else if !readonly.is_empty() {
            format!("{} has been remounted read-only", readonly[0].mountpoint)
        } else {
            format!(
                "{} export{} across {} server{}",
                mounts.len(),
                if mounts.len() == 1 { "" } else { "s" },
                distinct_servers(&mounts),
                if distinct_servers(&mounts) == 1 { "" } else { "s" }
            )
        };

        let worst_bytes = mounts.iter().filter_map(Export::used_percent).fold(0.0, f64::max);
        let worst_inodes = mounts
            .iter()
            .filter_map(Export::inodes_used_percent)
            .fold(0.0, f64::max);

        let metrics = vec![
            Metric::new("exports", "Exports", mounts.len() as f64, MetricUnit::Count),
            Metric::new(
                "unreachable",
                "Unreachable mounts",
                unreachable.len() as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new("fullest", "Fullest export", worst_bytes, MetricUnit::Percent)
                .warn_above(self.fill_warn),
            Metric::new("inodes_fullest", "Most inodes used", worst_inodes, MetricUnit::Percent)
                .warn_above(self.inode_warn),
            Metric::new(
                "readonly",
                "Read-only mounts",
                readonly.len() as f64,
                MetricUnit::Count,
            )
            .warn_above(0.0),
            Metric::new(
                "capacity",
                "Total capacity",
                mounts.iter().filter(|e| !e.unreachable).map(|e| e.size).sum(),
                MetricUnit::Bytes,
            ),
        ];

        let mut sections = Vec::new();

        if !dead_servers.is_empty() {
            sections.push(
                Section::new(
                    "servers",
                    "Servers not answering",
                    vec![
                        ColumnSpec::text("server", "Server").primary(),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::metric("mounts", "Mounts affected", MetricUnit::Count),
                    ],
                )
                .rows(
                    dead_servers
                        .iter()
                        .map(|(server, count)| {
                            Row::new(format!("server:{server}"), (*server).to_string(), Health::Down)
                                .summary(format!(
                                    "{count} mount{} from this server {} unusable — one problem, \
                                     not {count}",
                                    if *count == 1 { "" } else { "s" },
                                    if *count == 1 { "is" } else { "are" },
                                ))
                                .cell("server", CellValue::text((*server).to_string()))
                                .cell("state", CellValue::status(Health::Down))
                                .cell("mounts", CellValue::number(*count as f64))
                        })
                        .collect(),
                )
                .priority(5),
            );
        }

        sections.push(
            Section::new(
                "exports",
                "Exports",
                vec![
                    ColumnSpec::text("mount", "Mount").primary(),
                    ColumnSpec::status("state", "State"),
                    ColumnSpec::gauge("used", "Bytes used", MetricUnit::Bytes),
                    ColumnSpec::gauge("inodes", "Inodes used", MetricUnit::Count),
                    ColumnSpec::metric("free", "Free", MetricUnit::Bytes),
                    ColumnSpec::text("export", "Server export"),
                ],
            )
            .rows(
                mounts
                    .iter()
                    .map(|export| {
                        let state = if export.unreachable {
                            Health::Down
                        } else if export
                            .worst_fill()
                            .is_some_and(|p| p >= self.fill_warn.min(self.inode_warn))
                            || export.readonly
                        {
                            Health::Degraded
                        } else {
                            Health::Healthy
                        };

                        let summary = if export.unreachable {
                            format!(
                                "stale or unreachable — {} is not answering, so nothing here is \
                                 measured",
                                export.server()
                            )
                        } else if export.readonly {
                            "remounted read-only — writes are failing".to_string()
                        } else {
                            format!(
                                "{} free of {} · {:.0}% of inodes used",
                                human_bytes(export.available),
                                human_bytes(export.size),
                                export.inodes_used_percent().unwrap_or(0.0)
                            )
                        };

                        Row::new(
                            format!("mount:{}", export.mountpoint),
                            export.mountpoint.clone(),
                            state,
                        )
                        .summary(summary)
                        .cell("mount", CellValue::text(export.mountpoint.clone()))
                        .cell("state", CellValue::status(state))
                        // An unreachable mount gets no gauge at all rather than
                        // a zero, because a zero draws as an empty bar and an
                        // empty bar reads as plenty of room.
                        .cell(
                            "used",
                            match export.used_percent() {
                                Some(_) => CellValue::gauge_warn(
                                    export.size - export.available,
                                    Some(export.size),
                                    export.size * (self.fill_warn / 100.0),
                                ),
                                None => CellValue::text("—"),
                            },
                        )
                        .cell(
                            "inodes",
                            match export.inodes_used_percent() {
                                Some(_) => CellValue::gauge_warn(
                                    export.inodes - export.inodes_free,
                                    Some(export.inodes),
                                    export.inodes * (self.inode_warn / 100.0),
                                ),
                                None => CellValue::text("—"),
                            },
                        )
                        .cell(
                            "free",
                            if export.unreachable {
                                CellValue::text("—")
                            } else {
                                CellValue::number(export.available)
                            },
                        )
                        .cell("export", CellValue::text(export.device.clone()))
                    })
                    .collect(),
            )
            .tracked()
            .priority(10),
        );

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Nfs,
            health,
            summary,
            metrics,
            sections,
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// node_exporter serves gauges. NFS client errors go to the host's kernel
    /// log, which needs the shell access this deliberately does without.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

fn distinct_servers(exports: &[Export]) -> usize {
    exports
        .iter()
        .map(Export::server)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two healthy team exports, one out of inodes with bytes to spare, and
    /// two stale mounts from a server that stopped answering.
    const SCRAPE: &str = concat!(
        // team-a: comfortable
        "node_filesystem_size_bytes{device=\"nfs-01:/exports/team-a\",fstype=\"nfs4\",mountpoint=\"/mnt/team-a\"} 1.0e+12\n",
        "node_filesystem_avail_bytes{device=\"nfs-01:/exports/team-a\",fstype=\"nfs4\",mountpoint=\"/mnt/team-a\"} 6.0e+11\n",
        "node_filesystem_files{device=\"nfs-01:/exports/team-a\",fstype=\"nfs4\",mountpoint=\"/mnt/team-a\"} 1.0e+07\n",
        "node_filesystem_files_free{device=\"nfs-01:/exports/team-a\",fstype=\"nfs4\",mountpoint=\"/mnt/team-a\"} 9.0e+06\n",
        "node_filesystem_device_error{device=\"nfs-01:/exports/team-a\",fstype=\"nfs4\",mountpoint=\"/mnt/team-a\"} 0\n",
        "node_filesystem_readonly{device=\"nfs-01:/exports/team-a\",fstype=\"nfs4\",mountpoint=\"/mnt/team-a\"} 0\n",
        // team-b: 40% of bytes free, but the inode table is gone
        "node_filesystem_size_bytes{device=\"nfs-01:/exports/team-b\",fstype=\"nfs4\",mountpoint=\"/mnt/team-b\"} 1.0e+12\n",
        "node_filesystem_avail_bytes{device=\"nfs-01:/exports/team-b\",fstype=\"nfs4\",mountpoint=\"/mnt/team-b\"} 4.0e+11\n",
        "node_filesystem_files{device=\"nfs-01:/exports/team-b\",fstype=\"nfs4\",mountpoint=\"/mnt/team-b\"} 1.0e+07\n",
        "node_filesystem_files_free{device=\"nfs-01:/exports/team-b\",fstype=\"nfs4\",mountpoint=\"/mnt/team-b\"} 2.0e+05\n",
        "node_filesystem_device_error{device=\"nfs-01:/exports/team-b\",fstype=\"nfs4\",mountpoint=\"/mnt/team-b\"} 0\n",
        // team-c and team-d: stale, from a second server, reporting zeroes
        "node_filesystem_size_bytes{device=\"nfs-02:/exports/team-c\",fstype=\"nfs4\",mountpoint=\"/mnt/team-c\"} 0\n",
        "node_filesystem_avail_bytes{device=\"nfs-02:/exports/team-c\",fstype=\"nfs4\",mountpoint=\"/mnt/team-c\"} 0\n",
        "node_filesystem_device_error{device=\"nfs-02:/exports/team-c\",fstype=\"nfs4\",mountpoint=\"/mnt/team-c\"} 1\n",
        "node_filesystem_size_bytes{device=\"nfs-02:/exports/team-d\",fstype=\"nfs4\",mountpoint=\"/mnt/team-d\"} 0\n",
        "node_filesystem_avail_bytes{device=\"nfs-02:/exports/team-d\",fstype=\"nfs4\",mountpoint=\"/mnt/team-d\"} 0\n",
        "node_filesystem_device_error{device=\"nfs-02:/exports/team-d\",fstype=\"nfs4\",mountpoint=\"/mnt/team-d\"} 1\n",
        // The host's own disk, which is not this collector's business
        "node_filesystem_size_bytes{device=\"/dev/sda1\",fstype=\"ext4\",mountpoint=\"/\"} 1.0e+10\n",
        "node_filesystem_avail_bytes{device=\"/dev/sda1\",fstype=\"ext4\",mountpoint=\"/\"} 1.0e+08\n",
    );

    fn all() -> Vec<Export> {
        exports(&parse_prometheus(SCRAPE), "")
    }

    #[test]
    fn reads_only_nfs_mounts() {
        let found = all();
        assert_eq!(found.len(), 4);
        assert!(found.iter().all(|e| e.fstype == "nfs4"));
        assert!(
            !found.iter().any(|e| e.mountpoint == "/"),
            "the host's own ext4 root is the Linux host collector's job",
        );
    }

    /// The failure this module exists for. A stale mount answers with zeros,
    /// and a percentage of zero would read as an empty, healthy export.
    #[test]
    fn a_stale_mount_has_no_fill_rather_than_zero_fill() {
        let stale = all()
            .into_iter()
            .find(|e| e.mountpoint == "/mnt/team-c")
            .expect("team-c");
        assert!(stale.unreachable);
        assert_eq!(
            stale.used_percent(),
            None,
            "zero of zero must not be reported as an empty export",
        );
        assert_eq!(stale.worst_fill(), None);
    }

    /// The other one: `df -h` says 40% free and writes fail with ENOSPC.
    #[test]
    fn inode_exhaustion_is_visible_while_bytes_look_fine() {
        let export = all()
            .into_iter()
            .find(|e| e.mountpoint == "/mnt/team-b")
            .expect("team-b");
        assert_eq!(export.used_percent(), Some(60.0));
        assert_eq!(export.inodes_used_percent(), Some(98.0));
        assert_eq!(
            export.worst_fill(),
            Some(98.0),
            "either one filling stops writes, so the worse of the two decides",
        );
    }

    /// Six broken paths from one vanished server is one problem.
    #[test]
    fn stale_mounts_are_attributed_to_the_server_they_came_from() {
        let found = all();
        let failed = failed_servers(&found);
        assert_eq!(failed.len(), 1, "two mounts, one server");
        assert_eq!(failed.get("nfs-02"), Some(&2));
    }

    #[test]
    fn unreachable_mounts_sort_above_full_ones() {
        let found = all();
        assert!(found[0].unreachable && found[1].unreachable);
        assert_eq!(found[2].mountpoint, "/mnt/team-b", "then the fullest");
    }

    /// Team scoping: one connection per path handed to a team.
    #[test]
    fn a_path_prefix_narrows_to_one_teams_exports() {
        let scoped = exports(&parse_prometheus(SCRAPE), "/mnt/team-a");
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].mountpoint, "/mnt/team-a");
    }

    #[test]
    fn the_server_is_read_from_the_export_string() {
        let export = all()
            .into_iter()
            .find(|e| e.mountpoint == "/mnt/team-a")
            .expect("team-a");
        assert_eq!(export.server(), "nfs-01");
        assert_eq!(export.device, "nfs-01:/exports/team-a");
    }

    /// A device string in some other shape still needs a stable grouping key.
    #[test]
    fn a_device_without_a_colon_groups_by_itself() {
        let samples = parse_prometheus(
            "node_filesystem_size_bytes{device=\"odd-mount\",fstype=\"nfs\",mountpoint=\"/mnt/x\"} 10\n",
        );
        assert_eq!(exports(&samples, "")[0].server(), "odd-mount");
    }

    #[test]
    fn a_scrape_with_no_nfs_mounts_yields_none() {
        let samples = parse_prometheus(
            "node_filesystem_size_bytes{device=\"/dev/sda1\",fstype=\"ext4\",mountpoint=\"/\"} 10\n",
        );
        assert!(exports(&samples, "").is_empty());
    }
}
