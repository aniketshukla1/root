//! Following declared log files, and systemd journals.
//!
//! Most logs reach root through a service's own channel: a container's output,
//! a pod's, Loki, OpenTelemetry. A service that only writes to a file on disk
//! was invisible to search, to alerts and to the assistant. A connection can
//! now name its log files, and on Linux its systemd unit, and root follows them
//! like `tail -f`; the lines join the same stream as everything else.
//!
//! Bounded the way configuration reading is (declared, never discovered;
//! patterns rooted; credential files named and not opened; binary refused;
//! every line through the redactor) and the way a follower has to be:
//!
//! - a first read starts near the end, not at the top of a year-old file;
//! - a rotated or truncated file is followed from its start;
//! - a file written faster than root reads it is skipped forward, and says so.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::ConnectionProfile;
use crate::error::{AppError, Result};
use crate::files::{Location, Reader};
use crate::model::{LogEvent, LogLevel, LogStream};

/// How far back a newly followed file starts.
const BACKLOG_BYTES: u64 = 64 * 1024;
/// Most bytes read from one file in one poll.
const READ_BYTES: u64 = 512 * 1024;
/// Further behind than this, a follower jumps to the end rather than work
/// through a backlog it can never catch up on.
const MAX_BEHIND_BYTES: u64 = 8 * 1024 * 1024;
/// Most lines one source yields in one poll; the newest are kept.
const MAX_LINES: usize = 1000;
/// A longer line is cut short; an unterminated line this long is let go of.
const MAX_LINE_BYTES: usize = 8 * 1024;
/// Entries a newly followed journal starts with.
const JOURNAL_BACKLOG: usize = 50;
/// Bytes examined when deciding whether a file is text.
const SNIFF_BYTES: usize = 8192;

/// One declared place a connection writes its logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LogSource {
    pub id: String,
    /// What the lines are labelled with. Falls back to the file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A file, or a directory with `include`. For a journal, the systemd unit.
    pub path: String,
    /// A glob under `path` when it is a directory, such as `*.log`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<String>,
    /// Follow the systemd journal of the unit `path` names, not a file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub journal: bool,
    #[serde(default)]
    pub location: Location,
}

/// Reject a source before it is stored, so a bad one is a form error rather
/// than a note on every poll.
pub fn validate(source: &LogSource) -> Result<()> {
    if source.journal {
        let unit = source.path.trim();
        let allowed = |c: char| c.is_ascii_alphanumeric() || "@._:-\\".contains(c);
        if unit.is_empty() || unit.starts_with('-') || unit.len() > 256 || !unit.chars().all(allowed) {
            return Err(AppError::Config(format!("`{unit}` is not a systemd unit name")));
        }
        return Ok(());
    }
    crate::configsource::validate(&crate::configsource::ConfigSource {
        id: source.id.clone(),
        label: None,
        path: source.path.clone(),
        include: source.include.clone(),
    })
}

/// Where each followed file or journal has got to, across polls.
#[derive(Default)]
pub struct Tails {
    entries: HashMap<String, Tail>,
}

#[derive(Debug, Default)]
struct Tail {
    identity: String,
    offset: u64,
    /// The end of the last read, when it stopped mid-line.
    partial: Vec<u8>,
    /// A journal's position.
    cursor: Option<String>,
    /// Said once why this one is not being followed; said again only after
    /// it has worked in between.
    noted: bool,
}

/// What one step of following a file produced.
#[derive(Debug, Default, PartialEq)]
struct Step {
    lines: Vec<String>,
    note: Option<String>,
}

fn decode(bytes: &[u8]) -> String {
    let cut = &bytes[..bytes.len().min(MAX_LINE_BYTES)];
    String::from_utf8_lossy(cut).trim_end_matches('\r').to_string()
}

/// Follow one file one step: the complete lines written since the last step.
async fn follow(reader: &dyn Reader, path: &str, tail: &mut Tail) -> Result<Step> {
    let stat = reader.stat(path).await?;
    let mut step = Step::default();
    // Set when reading starts mid-file, where the first line is a fragment.
    let mut mid_line = false;
    let fresh = tail.identity.is_empty();

    if fresh {
        tail.identity = stat.identity.clone();
        tail.offset = stat.size.saturating_sub(BACKLOG_BYTES);
        mid_line = tail.offset > 0;
    } else if stat.identity != tail.identity || stat.size < tail.offset {
        // Rotated or truncated: the path names a new file now.
        tail.identity = stat.identity.clone();
        tail.offset = 0;
        tail.partial.clear();
    }

    if stat.size - tail.offset.min(stat.size) > MAX_BEHIND_BYTES {
        let resume = stat.size - BACKLOG_BYTES;
        step.note = Some(format!(
            "skipped {} MB of {path}: it is being written faster than it can be read",
            (resume - tail.offset) / (1024 * 1024)
        ));
        tail.offset = resume;
        tail.partial.clear();
        mid_line = true;
    }
    if stat.size <= tail.offset {
        return Ok(step);
    }

    let bytes = reader.read(path, tail.offset, (stat.size - tail.offset).min(READ_BYTES)).await?;
    if bytes.iter().take(SNIFF_BYTES).any(|b| *b == 0) {
        return Err(AppError::Collector(format!("{path} is not text")));
    }
    tail.offset += bytes.len() as u64;

    let mut buf = std::mem::take(&mut tail.partial);
    buf.extend_from_slice(&bytes);
    let mut start = 0;
    for (i, b) in buf.iter().enumerate() {
        if *b == b'\n' {
            step.lines.push(decode(&buf[start..i]));
            start = i + 1;
        }
    }
    tail.partial = buf[start..].to_vec();
    if tail.partial.len() > MAX_LINE_BYTES {
        step.lines.push(decode(&tail.partial));
        tail.partial.clear();
    }
    if mid_line && !step.lines.is_empty() {
        step.lines.remove(0);
    }
    // The newest lines win. A first read is a backlog and is cut quietly.
    if step.lines.len() > MAX_LINES {
        let dropped = step.lines.len() - MAX_LINES;
        step.lines.drain(..dropped);
        if !fresh && step.note.is_none() {
            step.note = Some(format!("skipped {dropped} lines of {path}: more than {MAX_LINES} arrived between two reads"));
        }
    }
    Ok(step)
}

/// The arguments for `journalctl`, never through a shell.
fn journal_args(unit: &str, cursor: Option<&str>) -> Vec<String> {
    let mut args = vec!["--unit".to_string(), unit.to_string(), "--output=json".to_string(), "--no-pager".to_string(), "--quiet".to_string()];
    match cursor {
        Some(cursor) => {
            args.push(format!("--after-cursor={cursor}"));
            args.push(format!("--lines={MAX_LINES}"));
        }
        None => args.push(format!("--lines={JOURNAL_BACKLOG}")),
    }
    args
}

/// One entry of `journalctl --output=json`: level, message, time, cursor.
fn journal_entry(line: &str) -> Option<(LogLevel, String, DateTime<Utc>, String)> {
    let entry: Value = serde_json::from_str(line).ok()?;
    let cursor = entry.get("__CURSOR")?.as_str()?.to_string();
    let message = match entry.get("MESSAGE")? {
        Value::String(text) => text.clone(),
        // A message that is not UTF-8 comes as an array of byte values.
        Value::Array(bytes) => {
            let raw: Vec<u8> = bytes.iter().filter_map(|b| b.as_u64().map(|n| n as u8)).collect();
            String::from_utf8_lossy(&raw).into_owned()
        }
        _ => return None,
    };
    let level = match entry.get("PRIORITY").and_then(Value::as_str).and_then(|p| p.parse::<u8>().ok()) {
        Some(0..=2) => LogLevel::Fatal,
        Some(3) => LogLevel::Error,
        Some(4) => LogLevel::Warn,
        Some(7) => LogLevel::Debug,
        Some(_) => LogLevel::Info,
        None => LogLevel::infer(&message),
    };
    let at = entry
        .get("__REALTIME_TIMESTAMP")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<i64>().ok())
        .and_then(|micros| Utc.timestamp_micros(micros).single())
        .unwrap_or_else(Utc::now);
    Some((level, message, at, cursor))
}

/// New journal entries for a unit, and where to resume.
async fn journal(location: &Location, unit: &str, cursor: Option<&str>) -> Result<(Vec<(LogLevel, String, DateTime<Utc>)>, Option<String>)> {
    let output = match location {
        Location::Local => {
            let run = tokio::process::Command::new("journalctl")
                .args(journal_args(unit, cursor))
                .kill_on_drop(true)
                .output();
            match tokio::time::timeout(std::time::Duration::from_secs(15), run).await {
                Err(_) => return Err(AppError::Collector("journalctl did not answer within 15 s".into())),
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(AppError::Collector(
                        "journalctl is not on this machine; the journal is a systemd (Linux) feature".into(),
                    ))
                }
                Ok(Err(e)) => return Err(AppError::Collector(format!("journalctl: {e}"))),
                Ok(Ok(out)) if !out.status.success() => {
                    let why = String::from_utf8_lossy(&out.stderr);
                    return Err(AppError::Collector(format!("journalctl: {}", why.trim())));
                }
                Ok(Ok(out)) => out.stdout,
            }
        }
    };
    let mut entries = Vec::new();
    let mut last = cursor.map(str::to_string);
    for line in String::from_utf8_lossy(&output).lines() {
        if let Some((level, message, at, cursor)) = journal_entry(line) {
            entries.push((level, message, at));
            last = Some(cursor);
        }
    }
    Ok((entries, last))
}

fn event(profile: &ConnectionProfile, source: &str, level: LogLevel, line: &str, at: DateTime<Utc>) -> LogEvent {
    let message = crate::redact::text(line).text;
    LogEvent::new(&profile.id, &profile.name, profile.kind, source, level, message, at)
}

fn note(profile: &ConnectionProfile, source: &str, message: String) -> LogEvent {
    LogEvent::new(&profile.id, &profile.name, profile.kind, source, LogLevel::Warn, message, Utc::now())
        .stream(LogStream::Event)
}

fn file_label(source: &LogSource, path: &str) -> String {
    source
        .label
        .clone()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| path.rsplit(['/', '\\']).next().unwrap_or(path).to_string())
}

/// Follow every log source a connection declares, one step.
pub async fn poll(state: &crate::state::AppState, profile: &ConnectionProfile) -> Vec<LogEvent> {
    let prefix = format!("{}\u{1f}", profile.id);
    let mut seen = HashSet::new();
    let mut events = Vec::new();

    for source in &profile.log_sources {
        let source_key = format!("{prefix}{}", source.id);
        seen.insert(source_key.clone());
        let mut lines: Vec<(LogLevel, String, DateTime<Utc>)> = Vec::new();
        let label = file_label(source, &source.path);

        let outcome: Result<()> = async {
            if source.journal {
                let mut tail = state.log_tails.lock().await.entries.remove(&source_key).unwrap_or_default();
                let result = journal(&source.location, source.path.trim(), tail.cursor.as_deref()).await;
                if let Ok((entries, cursor)) = &result {
                    lines.extend(entries.iter().cloned());
                    tail.cursor = cursor.clone();
                }
                state.log_tails.lock().await.entries.insert(source_key.clone(), tail);
                return result.map(|_| ());
            }
            let reader = crate::files::reader(state, &source.location).await?;
            for path in reader.list(&source.path, source.include.as_deref()).await? {
                let key = format!("{source_key}\u{1f}{path}");
                seen.insert(key.clone());
                let mut tail = state.log_tails.lock().await.entries.remove(&key).unwrap_or_default();
                let label = file_label(source, &path);
                if crate::redact::secretish_filename(&path) {
                    if !tail.noted {
                        events.push(note(profile, &label, format!("Not following {path}: it looks like a file that holds credentials.")));
                        tail.noted = true;
                    }
                } else {
                    match follow(reader.as_ref(), &path, &mut tail).await {
                        Ok(step) => {
                            tail.noted = false;
                            if let Some(message) = step.note {
                                events.push(note(profile, &label, message));
                            }
                            // A file's lines carry no reliable time of their own, so
                            // each gets the time it was read, a microsecond apart:
                            // a stack trace sorts in the order it was written.
                            let now = Utc::now();
                            events.extend(
                                step.lines
                                    .iter()
                                    .filter(|l| !l.trim().is_empty())
                                    .enumerate()
                                    .map(|(i, l)| event(profile, &label, LogLevel::infer(l), l, now + chrono::Duration::microseconds(i as i64))),
                            );
                        }
                        Err(e) if !tail.noted => {
                            events.push(note(profile, &label, format!("Could not follow {path}: {e}")));
                            tail.noted = true;
                        }
                        Err(_) => {}
                    }
                }
                state.log_tails.lock().await.entries.insert(key, tail);
            }
            Ok(())
        }
        .await;

        let mut tails = state.log_tails.lock().await;
        let tail = tails.entries.entry(source_key).or_default();
        match outcome {
            Ok(()) => tail.noted = false,
            Err(e) if !tail.noted => {
                events.push(note(profile, &label, format!("Could not read {}: {e}", source.path)));
                tail.noted = true;
            }
            Err(_) => {}
        }
        drop(tails);
        events.extend(lines.into_iter().map(|(level, line, at)| event(profile, &label, level, &line, at)));
    }

    // Forget files this connection no longer names.
    state.log_tails.lock().await.entries.retain(|key, _| !key.starts_with(&prefix) || seen.contains(key));
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::Local;
    use std::io::Write;

    fn temp_file(name: &str, body: &[u8]) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("root-logfiles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        std::fs::write(&file, body).unwrap();
        let path = file.to_string_lossy().into_owned();
        (dir, path)
    }

    fn append(path: &str, body: &[u8]) {
        std::fs::OpenOptions::new().append(true).open(path).unwrap().write_all(body).unwrap();
    }

    #[tokio::test]
    async fn a_new_file_starts_near_its_end_on_a_whole_line() {
        let body: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        let (dir, path) = temp_file("app.log", body.as_bytes());
        let mut tail = Tail::default();

        let step = follow(&Local, &path, &mut tail).await.unwrap();

        assert_eq!(step.lines.last().unwrap(), "line 19999");
        // A fragment such as "45" or "ne 12345" would not parse.
        let first: usize = step.lines[0].strip_prefix("line ").unwrap().parse().unwrap();
        assert!(first > 0, "it started near the end, not at the top");
        assert_eq!(step.lines.len(), 20_000 - first, "every line from there to the end, once");
        assert!(step.note.is_none(), "a first read's backlog is cut quietly");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn appended_lines_arrive_once_and_a_partial_line_waits_for_its_end() {
        let (dir, path) = temp_file("app.log", b"a\nb\n");
        let mut tail = Tail::default();
        assert_eq!(follow(&Local, &path, &mut tail).await.unwrap().lines, vec!["a", "b"]);

        append(&path, b"c\nd");
        assert_eq!(follow(&Local, &path, &mut tail).await.unwrap().lines, vec!["c"]);
        append(&path, b"\r\n");
        assert_eq!(follow(&Local, &path, &mut tail).await.unwrap().lines, vec!["d"]);
        assert!(follow(&Local, &path, &mut tail).await.unwrap().lines.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_rotated_file_is_read_from_its_start() {
        let (dir, path) = temp_file("app.log", b"old\n");
        let mut tail = Tail::default();
        follow(&Local, &path, &mut tail).await.unwrap();

        std::fs::rename(&path, format!("{path}.1")).unwrap();
        std::fs::write(&path, b"new 1\nnew 2\n").unwrap();
        assert_eq!(follow(&Local, &path, &mut tail).await.unwrap().lines, vec!["new 1", "new 2"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_truncated_file_is_read_from_its_start() {
        let (dir, path) = temp_file("app.log", b"aaaa\nbbbb\n");
        let mut tail = Tail::default();
        follow(&Local, &path, &mut tail).await.unwrap();

        std::fs::OpenOptions::new().write(true).truncate(true).open(&path).unwrap().write_all(b"c\n").unwrap();
        assert_eq!(follow(&Local, &path, &mut tail).await.unwrap().lines, vec!["c"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_file_written_faster_than_it_is_read_is_skipped_forward_and_says_so() {
        let (dir, path) = temp_file("app.log", b"start\n");
        let mut tail = Tail::default();
        follow(&Local, &path, &mut tail).await.unwrap();

        let burst: String = (0..1_000_000).map(|i| format!("burst {i:07}\n")).collect();
        append(&path, burst.as_bytes());
        let step = follow(&Local, &path, &mut tail).await.unwrap();

        assert!(step.note.as_deref().unwrap_or("").starts_with("skipped "), "{:?}", step.note);
        assert_eq!(step.lines.last().unwrap(), "burst 0999999");
        assert!(step.lines.len() < 10_000, "only the last stretch, not the backlog");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_binary_file_is_refused() {
        let (dir, path) = temp_file("data.bin", b"\0\0\0binary\n");
        let mut tail = Tail::default();
        assert!(follow(&Local, &path, &mut tail).await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_poll_turns_lines_into_redacted_events_in_the_order_they_were_written() {
        let (dir, path) = temp_file("app.log", b"first\npostgres://app:hunter2@db:5432/x refused\nthird\n");
        std::fs::write(dir.join(".env"), b"TOKEN=abc\n").unwrap();
        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "web", "name": "web", "kind": "this_machine", "endpoint": "", "enabled": true, "options": {},
            "logSources": [
                { "id": "app", "path": path },
                { "id": "secrets", "path": dir.to_string_lossy(), "include": ".env" }
            ]
        }))
        .unwrap();
        let state = crate::state::AppState::new(crate::config::AppConfig::default(), dir.join("config.json"));

        let events = poll(&state, &profile).await;
        let lines: Vec<&LogEvent> = events.iter().filter(|e| e.stream == LogStream::Runtime).collect();
        assert_eq!(lines.iter().map(|e| e.message.as_str()).collect::<Vec<_>>()[0], "first");
        assert_eq!(lines[2].message, "third");
        assert!(!lines[1].message.contains("hunter2") && lines[1].level == LogLevel::Info);
        assert!(lines.windows(2).all(|w| w[0].timestamp < w[1].timestamp), "written order survives sorting by time");
        assert!(
            events.iter().any(|e| e.stream == LogStream::Event && e.message.contains(".env")),
            "a credential file is named, not opened"
        );
        assert!(poll(&state, &profile).await.is_empty(), "nothing new, and a note is not repeated");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_journal_entry_carries_its_priority_time_and_cursor() {
        let line = r#"{"__CURSOR":"s=abc;i=2","__REALTIME_TIMESTAMP":"1789000000000000","PRIORITY":"3","MESSAGE":"connection refused"}"#;
        let (level, message, at, cursor) = journal_entry(line).unwrap();
        assert_eq!(level, LogLevel::Error);
        assert_eq!(message, "connection refused");
        assert_eq!(at.timestamp(), 1_789_000_000);
        assert_eq!(cursor, "s=abc;i=2");

        let bytes = r#"{"__CURSOR":"c","PRIORITY":"6","MESSAGE":[104,105]}"#;
        assert_eq!(journal_entry(bytes).unwrap().1, "hi", "a non-UTF-8 message arrives as bytes");
        assert!(journal_entry("-- No entries --").is_none());
    }

    #[test]
    fn a_journal_resumes_after_its_cursor() {
        assert!(journal_args("nginx.service", None).contains(&"--lines=50".to_string()));
        let resumed = journal_args("nginx.service", Some("s=abc"));
        assert!(resumed.contains(&"--after-cursor=s=abc".to_string()));
        assert_eq!(&resumed[..2], &["--unit".to_string(), "nginx.service".to_string()]);
    }

    #[test]
    fn a_unit_name_cannot_smuggle_an_option() {
        let unit = |path: &str| LogSource { id: "j".into(), label: None, path: path.into(), include: None, journal: true, location: Location::Local };
        assert!(validate(&unit("nginx.service")).is_ok());
        assert!(validate(&unit("--since=today")).is_err());
        assert!(validate(&unit("nginx; rm -rf /")).is_err());
        let file = LogSource { journal: false, ..unit("var/log/app.log") };
        assert!(validate(&file).is_err(), "a log file path must be absolute");
    }
}
