//! ZooKeeper monitor, via the `mntr` four-letter command.
//!
//! Open a socket, write `mntr`, read tab-separated `key\tvalue` lines until
//! the server closes the connection. No client library, no session, no watches
//! — which matters, because a monitor that establishes a real ZooKeeper
//! session is one more client competing for the same connection limit it is
//! supposed to be watching.
//!
//! The number worth having is `zk_synced_followers`, and it is only reported
//! by the leader.
//!
//! An ensemble of five tolerates two failures — but only while the survivors
//! are *synced*. A follower can be connected and counted in `zk_followers`
//! while still catching up, and a follower that is catching up does not vote.
//! So a leader reporting four followers and two synced has already lost its
//! fault tolerance: it currently has quorum, and the next node to go takes the
//! ensemble read-only. Nothing about the port, the process, or
//! `zk_server_state` shows that.
//!
//! `zk_outstanding_requests` is the other one. ZooKeeper queues requests it
//! cannot service yet, and that queue growing is the earliest sign of a
//! disk-bound ensemble — long before latency moves.

use std::collections::BTreeMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{elapsed_ms, Collector};
use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::model::{
    CellValue, ColumnSpec, Health, HealthSnapshot, LogEvent, Metric, MetricUnit, Row, Section,
    ServiceKind,
};

const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// `mntr` output is a few dozen lines. A cap rather than read-to-end so a
/// wrong port that streams cannot hang the poll.
const MAX_RESPONSE: usize = 64 * 1024;

pub struct ZookeeperCollector {
    address: String,
    /// Queued requests above which the ensemble is degraded.
    outstanding_warn: f64,
    profile_id: String,
    profile_name: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Mntr {
    pub fields: BTreeMap<String, String>,
}

impl Mntr {
    pub fn number(&self, key: &str) -> Option<f64> {
        self.fields.get(key).and_then(|v| v.parse::<f64>().ok())
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    /// leader, follower, standalone, or observer.
    pub fn role(&self) -> &str {
        self.text("zk_server_state").unwrap_or("unknown")
    }

    /// Followers that are actually caught up, and how many are connected.
    ///
    /// `None` on anything but a leader: followers and standalone servers do
    /// not report these keys at all, and inventing a zero would make every
    /// follower in a healthy ensemble look like it had lost its peers.
    pub fn synced(&self) -> Option<(f64, f64)> {
        Some((self.number("zk_synced_followers")?, self.number("zk_followers")?))
    }

    /// Nodes voting, including the leader itself.
    pub fn voting_members(&self) -> Option<f64> {
        self.synced().map(|(synced, _)| synced + 1.0)
    }
}

impl ZookeeperCollector {
    pub fn new(profile: &ConnectionProfile) -> Result<Self> {
        let raw = profile.endpoint.trim();
        if raw.is_empty() {
            return Err(AppError::Config(
                "ZooKeeper connections need a host and port, e.g. zk-1.internal:2181".into(),
            ));
        }

        let stripped = raw
            .strip_prefix("zk://")
            .or_else(|| raw.strip_prefix("zookeeper://"))
            .or_else(|| raw.strip_prefix("tcp://"))
            .unwrap_or(raw)
            .trim_end_matches('/');
        // A ZooKeeper connection string may list the whole ensemble. Only the
        // first is used: `mntr` answers for the server you asked, and asking
        // several would blend a leader's numbers with a follower's.
        let first = stripped.split(',').next().unwrap_or(stripped).trim();
        let address = if first.contains(':') {
            first.to_string()
        } else {
            format!("{first}:2181")
        };

        Ok(ZookeeperCollector {
            address,
            outstanding_warn: profile
                .options
                .get("outstandingWarn")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(10.0),
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        })
    }

    async fn mntr(&self) -> std::result::Result<Mntr, String> {
        let connect = tokio::time::timeout(IO_TIMEOUT, TcpStream::connect(&self.address));
        let mut stream = match connect.await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => return Err(format!("{}: {e}", self.address)),
            Err(_) => return Err(format!("{} did not answer within 10s", self.address)),
        };

        stream
            .write_all(b"mntr\n")
            .await
            .map_err(|e| format!("could not send mntr to {}: {e}", self.address))?;

        // ZooKeeper closes the connection after answering a four-letter
        // command, so reading to EOF is correct here — unlike memcached, there
        // is no terminator to look for.
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = tokio::time::timeout(IO_TIMEOUT, stream.read(&mut chunk))
                .await
                .map_err(|_| format!("{} stopped responding mid-mntr", self.address))?
                .map_err(|e| format!("could not read from {}: {e}", self.address))?;
            if read == 0 || buffer.len() >= MAX_RESPONSE {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }

        let text = String::from_utf8_lossy(&buffer);
        // Since 3.5 the four-letter commands are disabled unless whitelisted,
        // and the server answers with this rather than an error. Saying so is
        // the difference between a five-minute fix and an afternoon.
        if text.contains("is not executed because it is not in the whitelist") {
            return Err(format!(
                "{} refused mntr — add `4lw.commands.whitelist=mntr` to zoo.cfg and restart",
                self.address
            ));
        }

        let parsed = parse_mntr(&text);
        if parsed.fields.is_empty() {
            return Err(format!(
                "{} answered, but not with mntr output — check the client port",
                self.address
            ));
        }
        Ok(parsed)
    }
}

pub(crate) fn parse_mntr(body: &str) -> Mntr {
    let mut fields = BTreeMap::new();
    for line in body.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        // Tab-separated, but some builds use spaces. Splitting on the first
        // run of whitespace handles both, and no `mntr` key contains one.
        if let Some((key, value)) = line.split_once(|c: char| c.is_whitespace()) {
            fields.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Mntr { fields }
}

#[async_trait]
impl Collector for ZookeeperCollector {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Zookeeper
    }

    async fn collect(&self) -> Result<HealthSnapshot> {
        let start = Instant::now();

        let mntr = match self.mntr().await {
            Ok(mntr) => mntr,
            Err(message) => {
                return Ok(HealthSnapshot::unreachable(
                    &self.profile_id,
                    &self.profile_name,
                    ServiceKind::Zookeeper,
                    message,
                    elapsed_ms(start),
                ));
            }
        };
        let latency = elapsed_ms(start);

        let role = mntr.role().to_string();
        let outstanding = mntr.number("zk_outstanding_requests").unwrap_or(0.0);
        let synced = mntr.synced();

        // A leader with fewer synced followers than connected ones has quorum
        // now and no margin: the next node to go takes the ensemble read-only.
        let lagging = synced.is_some_and(|(synced, connected)| synced < connected);
        let backed_up = outstanding >= self.outstanding_warn;

        let health = if role == "unknown" {
            Health::Unknown
        } else if lagging || backed_up {
            Health::Degraded
        } else {
            Health::Healthy
        };

        let summary = if lagging {
            let (synced_count, connected) = synced.unwrap_or((0.0, 0.0));
            format!(
                "leader with {synced_count:.0} of {connected:.0} followers synced — the rest do \
                 not vote"
            )
        } else if backed_up {
            format!("{outstanding:.0} requests queued waiting to be serviced")
        } else {
            match synced {
                Some((synced_count, _)) => format!(
                    "{role} · {:.0} voting members",
                    synced_count + 1.0
                ),
                None => format!(
                    "{role} · {:.0} znodes",
                    mntr.number("zk_znode_count").unwrap_or(0.0)
                ),
            }
        };

        let mut metrics = vec![Metric::new(
            "outstanding",
            "Queued requests",
            outstanding,
            MetricUnit::Count,
        )
        .warn_above(self.outstanding_warn)];

        for (key, label, unit) in [
            ("zk_avg_latency", "Average latency", MetricUnit::Millis),
            ("zk_max_latency", "Max latency", MetricUnit::Millis),
            ("zk_znode_count", "Znodes", MetricUnit::Count),
            ("zk_watch_count", "Watches", MetricUnit::Count),
            ("zk_num_alive_connections", "Client connections", MetricUnit::Count),
            ("zk_approximate_data_size", "Data size", MetricUnit::Bytes),
        ] {
            if let Some(value) = mntr.number(key) {
                metrics.push(Metric::new(key, label, value, unit));
            }
        }

        if let Some((synced_count, connected)) = synced {
            metrics.push(Metric::new(
                "synced_followers",
                "Synced followers",
                synced_count,
                MetricUnit::Count,
            ));
            metrics.push(Metric::new(
                "followers",
                "Connected followers",
                connected,
                MetricUnit::Count,
            ));
        }

        let mut sections = Vec::new();
        if let Some((synced_count, connected)) = synced {
            let unsynced = (connected - synced_count).max(0.0);
            sections.push(
                Section::new(
                    "quorum",
                    "Quorum",
                    vec![
                        ColumnSpec::text("what", "Member").primary(),
                        ColumnSpec::status("state", "State"),
                        ColumnSpec::metric("count", "Count", MetricUnit::Count),
                    ],
                )
                .rows(vec![
                    Row::new("leader", "This server", Health::Healthy)
                        .summary("Leader — votes, and serves every write")
                        .cell("what", CellValue::text("This server (leader)"))
                        .cell("state", CellValue::status(Health::Healthy))
                        .cell("count", CellValue::number(1.0)),
                    Row::new("synced", "Synced followers", Health::Healthy)
                        .summary("Caught up, and counted toward quorum")
                        .cell("what", CellValue::text("Synced followers"))
                        .cell("state", CellValue::status(Health::Healthy))
                        .cell("count", CellValue::number(synced_count)),
                    Row::new(
                        "unsynced",
                        "Connected but not synced",
                        if unsynced > 0.0 { Health::Degraded } else { Health::Healthy },
                    )
                    .summary("Still catching up — connected, but these do not vote")
                    .cell("what", CellValue::text("Connected but not synced"))
                    .cell(
                        "state",
                        CellValue::status(if unsynced > 0.0 {
                            Health::Degraded
                        } else {
                            Health::Healthy
                        }),
                    )
                    .cell("count", CellValue::number(unsynced)),
                ])
                .tracked()
                .priority(10),
            );
        }

        sections.push(
            Section::new(
                "server",
                "Server",
                vec![
                    ColumnSpec::text("field", "Field").primary(),
                    ColumnSpec::text("value", "Value"),
                ],
            )
            .rows(
                [
                    ("zk_version", "Version"),
                    ("zk_server_state", "Role"),
                    ("zk_ephemerals_count", "Ephemeral nodes"),
                    ("zk_open_file_descriptor_count", "Open file descriptors"),
                    ("zk_max_file_descriptor_count", "File descriptor limit"),
                ]
                .into_iter()
                .filter_map(|(key, label)| {
                    mntr.text(key).map(|value| {
                        Row::new(key, label, Health::Healthy)
                            .cell("field", CellValue::text(label))
                            .cell("value", CellValue::text(value))
                    })
                })
                .collect(),
            )
            .priority(20),
        );

        Ok(HealthSnapshot {
            profile_id: self.profile_id.clone(),
            profile_name: self.profile_name.clone(),
            kind: ServiceKind::Zookeeper,
            health,
            summary,
            metrics,
            sections,
            checked_at: Utc::now(),
            latency_ms: latency,
            error: None,
        })
    }

    /// ZooKeeper's transaction log is not something a four-letter command
    /// serves, and its stdout is a file on the host.
    async fn fetch_logs(&self, _since: DateTime<Utc>, _limit: usize) -> Result<Vec<LogEvent>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEADER: &str = "zk_version\t3.8.1\n\
                          zk_avg_latency\t0.4\n\
                          zk_max_latency\t112\n\
                          zk_outstanding_requests\t0\n\
                          zk_server_state\tleader\n\
                          zk_znode_count\t4127\n\
                          zk_watch_count\t812\n\
                          zk_num_alive_connections\t46\n\
                          zk_followers\t4\n\
                          zk_synced_followers\t4\n\
                          zk_pending_syncs\t0\n";

    #[test]
    fn parses_tab_separated_output() {
        let mntr = parse_mntr(LEADER);
        assert_eq!(mntr.text("zk_version"), Some("3.8.1"));
        assert_eq!(mntr.number("zk_znode_count"), Some(4127.0));
        assert_eq!(mntr.role(), "leader");
    }

    /// Some builds separate with spaces rather than a tab.
    #[test]
    fn parses_space_separated_output() {
        let mntr = parse_mntr("zk_server_state follower\nzk_znode_count 12\n");
        assert_eq!(mntr.role(), "follower");
        assert_eq!(mntr.number("zk_znode_count"), Some(12.0));
    }

    #[test]
    fn a_healthy_leader_reports_every_follower_synced() {
        let mntr = parse_mntr(LEADER);
        assert_eq!(mntr.synced(), Some((4.0, 4.0)));
        assert_eq!(mntr.voting_members(), Some(5.0));
    }

    /// The point of the module. Connected is not synced, and a follower that
    /// is catching up does not vote — so this ensemble has quorum right now
    /// and no margin left.
    #[test]
    fn a_follower_that_is_connected_but_catching_up_does_not_count() {
        let mntr = parse_mntr(
            "zk_server_state\tleader\nzk_followers\t4\nzk_synced_followers\t2\n",
        );
        let (synced, connected) = mntr.synced().expect("leader reports both");
        assert!(synced < connected);
        assert_eq!(
            mntr.voting_members(),
            Some(3.0),
            "three of five vote, so one more loss is read-only",
        );
    }

    /// Followers and standalone servers do not report these keys, and a zero
    /// would make every healthy follower look like it had lost its peers.
    #[test]
    fn a_follower_reports_no_quorum_numbers_rather_than_zero() {
        let mntr = parse_mntr("zk_server_state\tfollower\nzk_znode_count\t4127\n");
        assert_eq!(mntr.synced(), None);
        assert_eq!(mntr.voting_members(), None);
    }

    #[test]
    fn a_standalone_server_is_a_role_not_a_fault() {
        let mntr = parse_mntr("zk_server_state\tstandalone\n");
        assert_eq!(mntr.role(), "standalone");
        assert_eq!(mntr.synced(), None);
    }

    #[test]
    fn a_non_zookeeper_response_yields_nothing() {
        assert!(parse_mntr("").fields.is_empty());
    }

    #[test]
    fn an_unknown_role_is_not_guessed() {
        assert_eq!(parse_mntr("zk_znode_count\t1\n").role(), "unknown");
    }
}
