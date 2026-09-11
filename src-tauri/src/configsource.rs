//! Where a connection's configuration lives, and reading it safely.
//!
//! Monitoring tells you a thing is wrong. Configuration tells you why, and
//! what to change. A Redis at 1.9 GB of a 2 GB `maxmemory` with
//! `maxmemory-policy noeviction` is not a memory problem to be watched — it is
//! a config decision that is about to start refusing writes, and no amount of
//! metric history says so on its own.
//!
//! ## Declared, never discovered
//!
//! An operator names the paths. Nothing here crawls a filesystem looking for
//! things that might be config, for the same reason the estate's dependency
//! edges are declared rather than inferred: a tool that goes looking will
//! eventually find something it should not have opened, and the first time
//! that is a private key it does not matter how good the intent was.
//!
//! ## Read-only, and it stays that way
//!
//! There is no writer in this module and there is not going to be one. A
//! recommendation is a diff a human applies. An app that edits nginx.conf on
//! the strength of a language model's opinion is a new and worse kind of
//! outage, and the blast radius of getting it wrong is the whole estate at
//! once rather than one container.
//!
//! ## The four refusals
//!
//! Reading arbitrary operator-named paths needs limits that hold even when the
//! path is wrong, hostile, or points somewhere enormous:
//!
//! - a file whose every byte is a credential is **named but not opened**
//! - a file over [`MAX_FILE_BYTES`] is **listed with its size, not its body**
//! - a file with a NUL in its head is **binary**, and its bytes are not text
//! - a glob is **rooted**, and a match that resolves outside its root is
//!   dropped — a symlink is not an escape hatch
//!
//! Everything that does come back has been through [`crate::redact`] before
//! any caller sees it. There is no accessor that returns the original bytes.

use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::redact::{self, Redaction};

/// Largest file read whole.
///
/// Generous for configuration — the largest nginx.conf in a large estate is a
/// few tens of kilobytes — and small enough that a path pointed at a log or a
/// database file is refused rather than pulled into memory.
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

/// Most files one source may yield, so a glob over a big tree is bounded.
pub const MAX_FILES_PER_SOURCE: usize = 200;

/// Bytes examined when deciding whether a file is text.
const SNIFF_BYTES: usize = 8192;

/// One declared place a connection's configuration lives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSource {
    pub id: String,
    /// What to call this in the UI. Falls back to the file or directory name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// An absolute path to a file or a directory.
    pub path: String,
    /// A glob applied under `path` when it is a directory — `*.conf`,
    /// `conf.d/**/*.conf`. Absent means every readable file directly inside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<String>,
}

/// How a document is structured, so a reader can be shown it properly and a
/// rule can parse it without guessing twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConfigFormat {
    Ini,
    Yaml,
    Json,
    Toml,
    Xml,
    Properties,
    Text,
}

impl ConfigFormat {
    pub fn of(path: &str) -> Self {
        let lower = path.to_ascii_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        match () {
            _ if name.ends_with(".yaml") || name.ends_with(".yml") => ConfigFormat::Yaml,
            _ if name.ends_with(".json") => ConfigFormat::Json,
            _ if name.ends_with(".toml") => ConfigFormat::Toml,
            _ if name.ends_with(".xml") => ConfigFormat::Xml,
            _ if name.ends_with(".properties") => ConfigFormat::Properties,
            _ if name.ends_with(".ini") || name.ends_with(".cnf") || name.ends_with(".conf") => {
                ConfigFormat::Ini
            }
            _ => ConfigFormat::Text,
        }
    }
}

/// Why a document has no body.
///
/// Withholding is a result, not a failure: "there is a `.env` here and I did
/// not open it" is a more useful thing to render than either its contents or
/// its absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Withheld {
    /// Every byte of it is a credential.
    Secret,
    /// Over [`MAX_FILE_BYTES`].
    TooLarge { bytes: u64 },
    /// Not text.
    Binary,
    Unreadable { message: String },
}

/// One piece of configuration, as it will be shown and as it will be sent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDocument {
    /// Stable across reads, so the UI and the assistant can name the same
    /// document without passing a path around.
    pub id: String,
    pub profile_id: String,
    pub label: String,
    pub origin: Origin,
    pub format: ConfigFormat,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<DateTime<Utc>>,
    /// Redacted. Absent when `withheld` says why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withheld: Option<Withheld>,
    /// What was taken out, so an operator can see when this has been too eager.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<Redaction>,
    pub read_at: DateTime<Utc>,
}

/// Where a document came from.
///
/// The distinction is load-bearing. A file is what is on disk, which may not
/// be what the process loaded — the commonest config bug in the world is an
/// edit nobody reloaded. Live config is what the target is actually running
/// with, after includes and defaults, and comparing the two finds that bug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Origin {
    File { path: String },
    /// Read back from the running target — `CONFIG GET`, `SHOW ALL`, an
    /// inspect call. `via` names the call, so a reader knows what they are
    /// looking at.
    Live { via: String },
}

impl ConfigDocument {
    /// A live document, already structured. The caller passes the value it got
    /// from the target; redaction happens here so no collector can forget it.
    pub fn live(
        profile_id: &str,
        label: &str,
        via: &str,
        value: &serde_json::Value,
    ) -> ConfigDocument {
        let (clean, redactions) = redact::json(value);
        let body = serde_json::to_string_pretty(&clean).unwrap_or_else(|_| clean.to_string());
        ConfigDocument {
            id: document_id(profile_id, via),
            profile_id: profile_id.to_string(),
            label: label.to_string(),
            origin: Origin::Live {
                via: via.to_string(),
            },
            format: ConfigFormat::Json,
            bytes: body.len() as u64,
            modified: None,
            body: Some(body),
            withheld: None,
            redactions,
            read_at: Utc::now(),
        }
    }

    /// A live document that is already text — the output of `nginx -T`, a
    /// `CONFIG GET` rendered as lines.
    pub fn live_text(profile_id: &str, label: &str, via: &str, text: &str) -> ConfigDocument {
        let redacted = redact::text(text);
        ConfigDocument {
            id: document_id(profile_id, via),
            profile_id: profile_id.to_string(),
            label: label.to_string(),
            origin: Origin::Live {
                via: via.to_string(),
            },
            format: ConfigFormat::Ini,
            bytes: redacted.text.len() as u64,
            modified: None,
            body: Some(redacted.text),
            withheld: None,
            redactions: redacted.redactions,
            read_at: Utc::now(),
        }
    }
}

/// A short stable id from the path or call that produced a document.
fn document_id(profile_id: &str, key: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    profile_id.hash(&mut hasher);
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Resolve one declared source to the documents it names.
///
/// Never errors on a path that is merely wrong: a source pointing at something
/// deleted last week produces one unreadable document rather than failing the
/// whole read, because the operator needs to see which of their sources has
/// gone stale.
pub async fn read_source(profile_id: &str, source: &ConfigSource) -> Vec<ConfigDocument> {
    let root = PathBuf::from(&source.path);
    if !root.is_absolute() {
        return vec![unreadable(
            profile_id,
            &source.path,
            source.label.clone(),
            "path must be absolute — a relative path means whatever directory the app \
             happens to be running in",
        )];
    }

    let paths = match expand(&root, source.include.as_deref()) {
        Ok(paths) => paths,
        Err(e) => {
            return vec![unreadable(
                profile_id,
                &source.path,
                source.label.clone(),
                &e.to_string(),
            )]
        }
    };

    if paths.is_empty() {
        return vec![unreadable(
            profile_id,
            &source.path,
            source.label.clone(),
            "nothing matched",
        )];
    }

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        out.push(read_file(profile_id, &path).await);
    }
    out
}

/// Every file a source names, bounded and rooted.
fn expand(root: &Path, include: Option<&str>) -> Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(AppError::Collector(
            "no such file or directory".to_string(),
        ));
    }

    let pattern = include.unwrap_or("*");
    if pattern.split(['/', '\\']).any(|c| c == "..") {
        return Err(AppError::Collector(
            "the include pattern may not contain `..` — a source reaches into its own \
             directory, not out of it"
                .to_string(),
        ));
    }

    let joined = root.join(pattern);
    let glob = joined.to_string_lossy();
    let matches = glob::glob(&glob)
        .map_err(|e| AppError::Collector(format!("bad include pattern: {e}")))?;

    // Resolve every hit against the real root, so a symlink inside the
    // directory cannot quietly widen the source to somewhere else on the
    // machine. A source is a statement about one directory.
    let real_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut out = Vec::new();
    for entry in matches.flatten() {
        if out.len() >= MAX_FILES_PER_SOURCE {
            break;
        }
        if !entry.is_file() {
            continue;
        }
        let real = match entry.canonicalize() {
            Ok(real) => real,
            Err(_) => continue,
        };
        if !real.starts_with(&real_root) {
            continue;
        }
        out.push(entry);
    }

    out.sort();
    Ok(out)
}

/// Read one file, applying every refusal and then redaction.
pub async fn read_file(profile_id: &str, path: &Path) -> ConfigDocument {
    let display = path.to_string_lossy().to_string();
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| display.clone());

    let metadata = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) => return unreadable(profile_id, &display, Some(label), &e.to_string()),
    };
    let bytes = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .map(DateTime::<Utc>::from);

    let base = |withheld: Option<Withheld>, body: Option<String>, redactions: Vec<Redaction>| {
        ConfigDocument {
            id: document_id(profile_id, &display),
            profile_id: profile_id.to_string(),
            label: label.clone(),
            origin: Origin::File {
                path: display.clone(),
            },
            format: ConfigFormat::of(&display),
            bytes,
            modified,
            body,
            withheld,
            redactions,
            read_at: Utc::now(),
        }
    };

    if redact::secretish_filename(&display) {
        return base(Some(Withheld::Secret), None, Vec::new());
    }
    if bytes > MAX_FILE_BYTES {
        return base(Some(Withheld::TooLarge { bytes }), None, Vec::new());
    }

    let raw = match tokio::fs::read(path).await {
        Ok(raw) => raw,
        Err(e) => return unreadable(profile_id, &display, Some(label), &e.to_string()),
    };
    if raw.iter().take(SNIFF_BYTES).any(|b| *b == 0) {
        return base(Some(Withheld::Binary), None, Vec::new());
    }

    let text = String::from_utf8_lossy(&raw);
    let redacted = redact::text(&text);
    base(None, Some(redacted.text), redacted.redactions)
}

fn unreadable(
    profile_id: &str,
    path: &str,
    label: Option<String>,
    message: &str,
) -> ConfigDocument {
    ConfigDocument {
        id: document_id(profile_id, path),
        profile_id: profile_id.to_string(),
        label: label.unwrap_or_else(|| path.to_string()),
        origin: Origin::File {
            path: path.to_string(),
        },
        format: ConfigFormat::of(path),
        bytes: 0,
        modified: None,
        body: None,
        withheld: Some(Withheld::Unreadable {
            message: message.to_string(),
        }),
        redactions: Vec::new(),
        read_at: Utc::now(),
    }
}

/// Reject a source before it is stored, so a bad path is a form error rather
/// than a document that fails on every poll.
pub fn validate(source: &ConfigSource) -> Result<()> {
    let path = Path::new(&source.path);
    if source.path.trim().is_empty() {
        return Err(AppError::Collector("a path is required".to_string()));
    }
    if !path.is_absolute() {
        return Err(AppError::Collector(
            "the path must be absolute".to_string(),
        ));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(AppError::Collector(
            "the path may not contain `..`".to_string(),
        ));
    }
    if let Some(include) = &source.include {
        if include.split(['/', '\\']).any(|c| c == "..") {
            return Err(AppError::Collector(
                "the include pattern may not contain `..`".to_string(),
            ));
        }
        glob::Pattern::new(include)
            .map_err(|e| AppError::Collector(format!("bad include pattern: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("root-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    #[tokio::test]
    async fn a_file_comes_back_redacted() {
        let dir = tempdir();
        let path = write(&dir, "redis.conf", "maxmemory 2gb\nrequirepass hunter2\n");

        let doc = read_file("p1", &path).await;
        let body = doc.body.expect("a body");
        assert!(body.contains("maxmemory 2gb"), "{body}");
        assert!(!body.contains("hunter2"), "{body}");
        assert_eq!(doc.redactions.len(), 1);
        assert_eq!(doc.format, ConfigFormat::Ini);
    }

    /// Named, so the operator knows it is there and knows it was not opened.
    #[tokio::test]
    async fn a_file_that_is_entirely_secret_is_listed_but_not_read() {
        let dir = tempdir();
        let path = write(&dir, ".env", "DB_PASSWORD=hunter2\n");

        let doc = read_file("p1", &path).await;
        assert_eq!(doc.withheld, Some(Withheld::Secret));
        assert!(doc.body.is_none());
        assert_eq!(doc.label, ".env");
    }

    #[tokio::test]
    async fn an_oversized_file_reports_its_size_rather_than_its_contents() {
        let dir = tempdir();
        let big = "x".repeat(MAX_FILE_BYTES as usize + 1);
        let path = write(&dir, "huge.conf", &big);

        let doc = read_file("p1", &path).await;
        assert!(matches!(doc.withheld, Some(Withheld::TooLarge { .. })));
        assert!(doc.body.is_none());
        assert!(doc.bytes > MAX_FILE_BYTES);
    }

    #[tokio::test]
    async fn a_binary_file_is_not_treated_as_text() {
        let dir = tempdir();
        let path = dir.join("cache.db");
        std::fs::write(&path, [0x00, 0x01, 0x02, 0x00]).unwrap();

        let doc = read_file("p1", &path).await;
        assert_eq!(doc.withheld, Some(Withheld::Binary));
    }

    #[tokio::test]
    async fn a_glob_collects_a_conf_directory() {
        let dir = tempdir();
        write(&dir, "conf.d/a.conf", "worker_processes auto;\n");
        write(&dir, "conf.d/b.conf", "keepalive_timeout 65;\n");
        write(&dir, "conf.d/notes.txt", "ignore me\n");

        let source = ConfigSource {
            id: "s1".into(),
            label: None,
            path: dir.to_string_lossy().to_string(),
            include: Some("conf.d/*.conf".into()),
        };
        let docs = read_source("p1", &source).await;
        let labels: Vec<_> = docs.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, vec!["a.conf", "b.conf"]);
    }

    /// A source is a statement about one directory. A symlink inside it is not
    /// a way to make it a statement about somewhere else.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_symlink_out_of_the_root_is_dropped() {
        let dir = tempdir();
        let outside = tempdir();
        write(&outside, "secrets.conf", "password = hunter2\n");
        write(&dir, "real.conf", "listen 80;\n");
        std::os::unix::fs::symlink(outside.join("secrets.conf"), dir.join("escape.conf")).unwrap();

        let source = ConfigSource {
            id: "s1".into(),
            label: None,
            path: dir.to_string_lossy().to_string(),
            include: Some("*.conf".into()),
        };
        let docs = read_source("p1", &source).await;
        let labels: Vec<_> = docs.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, vec!["real.conf"], "the symlink escaped the root");
    }

    #[tokio::test]
    async fn a_source_that_matches_nothing_says_so_rather_than_vanishing() {
        let dir = tempdir();
        let source = ConfigSource {
            id: "s1".into(),
            label: None,
            path: dir.to_string_lossy().to_string(),
            include: Some("*.conf".into()),
        };
        let docs = read_source("p1", &source).await;
        assert_eq!(docs.len(), 1);
        assert!(matches!(
            docs[0].withheld,
            Some(Withheld::Unreadable { .. })
        ));
    }

    #[tokio::test]
    async fn a_relative_path_is_refused_rather_than_resolved() {
        let source = ConfigSource {
            id: "s1".into(),
            label: None,
            path: "etc/nginx".into(),
            include: None,
        };
        let docs = read_source("p1", &source).await;
        assert!(matches!(
            docs[0].withheld,
            Some(Withheld::Unreadable { .. })
        ));
        assert!(validate(&source).is_err());
    }

    #[test]
    fn a_traversing_include_is_refused_at_validation() {
        let source = ConfigSource {
            id: "s1".into(),
            label: None,
            path: "/etc/nginx".into(),
            include: Some("../../*.conf".into()),
        };
        assert!(validate(&source).is_err());
    }

    #[test]
    fn an_id_is_stable_across_reads_and_distinct_per_profile() {
        assert_eq!(
            document_id("p1", "/etc/redis.conf"),
            document_id("p1", "/etc/redis.conf")
        );
        assert_ne!(
            document_id("p1", "/etc/redis.conf"),
            document_id("p2", "/etc/redis.conf")
        );
    }

    #[test]
    fn a_live_document_is_redacted_before_anyone_can_hold_it() {
        let doc = ConfigDocument::live(
            "p1",
            "Effective config",
            "CONFIG GET *",
            &serde_json::json!({ "maxmemory": "2gb", "requirepass": "hunter2" }),
        );
        let body = doc.body.expect("a body");
        assert!(body.contains("2gb"));
        assert!(!body.contains("hunter2"), "{body}");
        assert!(!doc.redactions.is_empty());
    }
}
