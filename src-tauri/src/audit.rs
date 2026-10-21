//! Who changed what on a shared server.
//!
//! Every command that changes something — a connection, a rule, a silence,
//! an incident, a restart — is entered here as it is answered, with who asked,
//! what it named, and whether it went through; and so is every one refused.
//! Only names: arguments can carry passwords and keys, so none is kept, and a
//! failure's message is stripped of anything shaped like a secret. The log is
//! append-only JSON lines beside the config, rolled over at 10 MiB, and only
//! an administrator may read it.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{Identity, Role};

/// One log file's size before it is rolled over to `audit.1.jsonl`.
const ROLL_AT: u64 = 10 << 20;

/// An id comes from the request, so it is cut to a length that cannot fill a disk.
const TARGET_CAP: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Refused,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub at: DateTime<Utc>,
    pub actor: String,
    /// Two people can share a name; not an address.
    pub email: String,
    pub role: Role,
    pub command: String,
    /// The id the command named — a connection, a rule, an incident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub outcome: Outcome,
    /// Why it failed or was refused, or the role an account was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AuditEntry {
    pub fn new(identity: &Identity, command: &str, target: Option<String>, outcome: Outcome, detail: Option<String>) -> Self {
        AuditEntry {
            at: Utc::now(),
            actor: identity.display_name.clone(),
            email: identity.email.clone(),
            role: identity.role,
            command: command.to_string(),
            target,
            outcome,
            // An error can quote what it was given.
            detail: detail.map(|d| crate::redact::text(&d).text),
        }
    }
}

pub fn path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("audit.jsonl")
}

/// The id a command's arguments name, looked for in the places commands put
/// one: a top-level `…Id`, or the `id` of the object being saved.
pub fn target_of(args: &Value) -> Option<String> {
    const IDS: [&str; 9] = ["profileId", "ruleId", "sloId", "incidentId", "dashboardId", "silenceId", "alertId", "channelId", "conversationId"];
    const SAVED: [&str; 6] = ["profile", "rule", "slo", "dashboard", "silence", "channel"];
    IDS.iter()
        .find_map(|key| args.get(*key)?.as_str())
        .or_else(|| SAVED.iter().find_map(|key| args.get(*key)?.get("id")?.as_str()))
        .map(|id| id.chars().take(TARGET_CAP).collect())
}

/// Add one entry. A failure to write is logged, not raised: the change it
/// records has already happened, and refusing the answer would not undo it.
pub fn record(log: &Path, entry: &AuditEntry) {
    let write = || -> std::io::Result<()> {
        if std::fs::metadata(log).is_ok_and(|m| m.len() >= ROLL_AT) {
            // Another request may have rolled it over first; either way the
            // entry goes on.
            let _ = std::fs::rename(log, log.with_extension("1.jsonl"));
        }
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        std::fs::OpenOptions::new().create(true).append(true).open(log)?.write_all(&line)
    };
    if let Err(e) = write() {
        tracing::warn!("audit write failed: {e}");
    }
}

/// The newest entries first, from the current file and the one rolled over.
pub fn recent(log: &Path, limit: usize) -> Vec<AuditEntry> {
    let files: Vec<String> = [log.with_extension("1.jsonl"), log.to_path_buf()]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    files.iter().flat_map(|text| text.lines()).rev().filter_map(|line| serde_json::from_str(line).ok()).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_target_is_the_named_id_and_never_the_arguments() {
        assert_eq!(target_of(&json!({ "profileId": "orders", "secret": "hunter2" })).as_deref(), Some("orders"));
        assert_eq!(target_of(&json!({ "rule": { "id": "r1", "name": "x" } })).as_deref(), Some("r1"));
        assert_eq!(target_of(&json!({ "settings": { "apiKey": "sk-…" } })), None);
        assert_eq!(target_of(&json!({ "profileId": "x".repeat(10_000) })).map(|t| t.len()), Some(TARGET_CAP));
    }

    #[test]
    fn entries_come_back_newest_first_across_a_roll_over() {
        let dir = std::env::temp_dir().join(format!("root-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("audit.jsonl");
        let who = Identity { user_id: None, email: "priya@example.com".into(), display_name: "Priya".into(), role: Role::Operator };
        record(&log, &AuditEntry::new(&who, "save_profile", Some("orders".into()), Outcome::Done, None));
        std::fs::rename(&log, log.with_extension("1.jsonl")).unwrap();
        let failure = "postgres://admin:hunter2@db:5432 is unreachable".to_string();
        record(&log, &AuditEntry::new(&who, "delete_alert_rule", None, Outcome::Failed, Some(failure)));

        let read = recent(&log, 10);
        assert_eq!(read.iter().map(|e| e.command.as_str()).collect::<Vec<_>>(), vec!["delete_alert_rule", "save_profile"]);
        assert_eq!(recent(&log, 1).len(), 1);
        assert!(!std::fs::read_to_string(&log).unwrap().contains("hunter2"), "a secret in an error is stripped");
        std::fs::remove_dir_all(&dir).ok();
    }
}
