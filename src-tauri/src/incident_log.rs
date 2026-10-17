//! Incidents that outlast their alerts.
//!
//! `incidents::correlate` groups what is firing right now, and an incident
//! derived that way is gone the moment its alerts clear — with nowhere to
//! say who took it, what they found, or what went out to customers. The log
//! keeps a record per incident: opened when correlation first sees it,
//! joined by every connection that turns, resolved when its alerts clear and
//! reopened if they come back; and between those, an owner, a status, notes,
//! public updates and a postmortem draft. A status page shows the public half.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::incidents::Incident;
use crate::model::{AlertSeverity, Health};

/// Records kept; the oldest go first.
const KEEP: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Investigating,
    Identified,
    Monitoring,
    Resolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Opened,
    Joined,
    Escalated,
    Status,
    Owner,
    Note,
    /// Written for the status page.
    Update,
    Resolved,
    Reopened,
    Postmortem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub at: DateTime<Utc>,
    pub kind: EntryKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentRecord {
    /// The correlated incident's id, stable while its originating condition persists.
    pub id: String,
    pub title: String,
    pub severity: AlertSeverity,
    pub status: Status,
    pub opened_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Every connection it ever touched, in the order they joined.
    pub profile_ids: Vec<String>,
    pub lead_profile_id: String,
    pub timeline: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postmortem: Option<String>,
}

/// A change someone makes by hand.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Change {
    Status { status: Status },
    Owner { owner: Option<String> },
    Note { text: String },
    Update { text: String },
}

#[derive(Default)]
pub struct IncidentLog {
    records: Vec<IncidentRecord>,
}

pub fn log_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("incidents.json")
}

impl IncidentLog {
    /// Whatever is on disk; a missing or unreadable file starts a new log
    /// rather than stopping root from starting.
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(records) => IncidentLog { records },
                Err(e) => {
                    tracing::warn!("incident log unreadable, starting a new one: {e}");
                    IncidentLog::default()
                }
            },
            Err(_) => IncidentLog::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.records)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Newest first.
    pub fn recent(&self, limit: usize) -> Vec<IncidentRecord> {
        let mut records = self.records.clone();
        records.sort_by_key(|r| std::cmp::Reverse(r.opened_at));
        records.truncate(limit);
        records
    }

    pub fn get(&self, id: &str) -> Option<&IncidentRecord> {
        self.records.iter().find(|r| r.id == id)
    }

    /// Bring the log in line with the incidents true now. Returns whether
    /// anything changed, so the caller writes only when it did.
    pub fn sync(&mut self, live: &[Incident], names: &HashMap<String, String>, now: DateTime<Utc>) -> bool {
        let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_string());
        let mut changed = false;
        for incident in live {
            let Some(record) = self.records.iter_mut().find(|r| r.id == incident.id) else {
                self.records.push(IncidentRecord {
                    id: incident.id.clone(),
                    title: incident.title.clone(),
                    severity: incident.severity,
                    status: Status::Investigating,
                    opened_at: incident.started,
                    resolved_at: None,
                    owner: None,
                    profile_ids: incident.profile_ids.clone(),
                    lead_profile_id: incident.lead_profile_id.clone(),
                    timeline: vec![Entry {
                        at: incident.started,
                        kind: EntryKind::Opened,
                        text: format!(
                            "Opened on {}",
                            incident.profile_ids.iter().map(|id| name(id)).collect::<Vec<_>>().join(", ")
                        ),
                        author: None,
                    }],
                    postmortem: None,
                });
                changed = true;
                continue;
            };
            if record.resolved_at.take().is_some() {
                record.status = Status::Investigating;
                record.timeline.push(Entry { at: now, kind: EntryKind::Reopened, text: "It came back".into(), author: None });
                changed = true;
            }
            for id in &incident.profile_ids {
                if !record.profile_ids.contains(id) {
                    record.profile_ids.push(id.clone());
                    record.timeline.push(Entry { at: now, kind: EntryKind::Joined, text: format!("{} joined", name(id)), author: None });
                    changed = true;
                }
            }
            if incident.severity > record.severity {
                record.severity = incident.severity;
                record.timeline.push(Entry { at: now, kind: EntryKind::Escalated, text: format!("Now {}", severity_word(incident.severity)), author: None });
                changed = true;
            }
            if record.title != incident.title || record.lead_profile_id != incident.lead_profile_id {
                record.title = incident.title.clone();
                record.lead_profile_id = incident.lead_profile_id.clone();
                changed = true;
            }
        }
        let live_ids: HashSet<&str> = live.iter().map(|i| i.id.as_str()).collect();
        for record in self.records.iter_mut().filter(|r| r.resolved_at.is_none() && !live_ids.contains(r.id.as_str())) {
            record.resolved_at = Some(now);
            record.status = Status::Resolved;
            record.timeline.push(Entry { at: now, kind: EntryKind::Resolved, text: "Every alert in it cleared".into(), author: None });
            changed = true;
        }
        if self.records.len() > KEEP {
            self.records.sort_by_key(|r| std::cmp::Reverse(r.opened_at));
            self.records.truncate(KEEP);
        }
        changed
    }

    /// A change made by hand, entered on the timeline in the author's name.
    pub fn apply(&mut self, id: &str, change: Change, author: Option<String>, now: DateTime<Utc>) -> Result<IncidentRecord> {
        let record = self
            .records
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| AppError::Config("No such incident.".into()))?;
        let entry = |kind, text: String| Entry { at: now, kind, text, author: author.clone() };
        match change {
            Change::Status { status } => {
                // Resolution follows the alerts: marking it resolved while
                // they still fire would only see it reopened on the next poll.
                if status == Status::Resolved {
                    return Err(AppError::Config("An incident resolves when its alerts clear.".into()));
                }
                if record.resolved_at.is_some() {
                    return Err(AppError::Config("This incident is already resolved.".into()));
                }
                if record.status != status {
                    record.status = status;
                    record.timeline.push(entry(EntryKind::Status, format!("Marked {}", status_word(status))));
                }
            }
            Change::Owner { owner } => {
                let owner = owner.map(|o| o.trim().to_string()).filter(|o| !o.is_empty());
                if record.owner != owner {
                    let text = match &owner {
                        Some(o) => format!("{o} took it"),
                        None => "No longer owned".to_string(),
                    };
                    record.owner = owner;
                    record.timeline.push(entry(EntryKind::Owner, text));
                }
            }
            Change::Note { text } | Change::Update { text } if text.trim().is_empty() => {
                return Err(AppError::Config("Write something first.".into()));
            }
            Change::Note { text } => record.timeline.push(entry(EntryKind::Note, text.trim().to_string())),
            Change::Update { text } => record.timeline.push(entry(EntryKind::Update, text.trim().to_string())),
        }
        Ok(record.clone())
    }

    pub fn set_postmortem(&mut self, id: &str, text: String, now: DateTime<Utc>) -> Result<IncidentRecord> {
        let record = self
            .records
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| AppError::Config("No such incident.".into()))?;
        record.postmortem = Some(text);
        record.timeline.push(Entry { at: now, kind: EntryKind::Postmortem, text: "Postmortem drafted".into(), author: None });
        Ok(record.clone())
    }
}

pub fn severity_word(severity: AlertSeverity) -> &'static str {
    match severity {
        AlertSeverity::Critical => "critical",
        AlertSeverity::Warning => "a warning",
        AlertSeverity::Info => "informational",
    }
}

pub fn status_word(status: Status) -> &'static str {
    match status {
        Status::Investigating => "investigating",
        Status::Identified => "identified",
        Status::Monitoring => "monitoring",
        Status::Resolved => "resolved",
    }
}

/// The facts a postmortem draft may use, one per line: nothing else.
pub fn postmortem_grounds(record: &IncidentRecord, names: &HashMap<String, String>) -> Vec<String> {
    let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_string());
    let lasted = record.resolved_at.unwrap_or_else(Utc::now) - record.opened_at;
    let mut lines = vec![
        format!("Incident: {}", record.title),
        format!("Severity: {}", severity_word(record.severity)),
        format!("Opened: {}", record.opened_at.to_rfc3339()),
        match record.resolved_at {
            Some(at) => format!("Resolved: {} (after {} minutes)", at.to_rfc3339(), lasted.num_minutes()),
            None => format!("Still open after {} minutes", lasted.num_minutes()),
        },
        format!("Connections involved, in the order they joined: {}", record.profile_ids.iter().map(|id| name(id)).collect::<Vec<_>>().join(", ")),
        format!("Owner: {}", record.owner.as_deref().unwrap_or("none recorded")),
        "Timeline:".to_string(),
    ];
    for e in &record.timeline {
        lines.push(format!(
            "- {} [{:?}] {}{}",
            e.at.to_rfc3339(),
            e.kind,
            e.text,
            e.author.as_deref().map(|a| format!(" — {a}")).unwrap_or_default()
        ));
    }
    lines
}

pub const POSTMORTEM_SYSTEM: &str = "You draft blameless incident postmortems for an infrastructure team. \
Write Markdown with these sections: Summary, Impact, Timeline, Root cause, What went well, What went wrong, \
Action items. Use only the facts you are given. Where a section needs a fact that is not there, write \
\"To confirm:\" and the question to answer, rather than guessing. Keep it short enough to read in two minutes.";

// ------------------------------------------------------------ status page

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StatusPageSettings {
    pub enabled: bool,
    pub title: String,
    /// The connections the page shows; none named is every one.
    pub profile_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicComponent {
    pub name: String,
    pub health: Health,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicIncident {
    pub title: String,
    pub status: Status,
    pub opened_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// Only the updates written for the public, newest first.
    pub updates: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPageView {
    pub settings: StatusPageSettings,
    pub title: String,
    pub overall: Health,
    pub components: Vec<PublicComponent>,
    /// Open ones, and those resolved in the last week.
    pub incidents: Vec<PublicIncident>,
    pub generated_at: DateTime<Utc>,
}

/// The public half: the chosen connections' health, and the incidents that
/// touched them with only their public updates — never notes, owners or
/// connections nobody chose to show.
pub fn status_view(
    settings: &StatusPageSettings,
    components: &[(String, String, Health)],
    records: &[IncidentRecord],
    now: DateTime<Utc>,
) -> StatusPageView {
    let shown: Vec<&(String, String, Health)> = components
        .iter()
        .filter(|(id, _, _)| settings.profile_ids.is_empty() || settings.profile_ids.contains(id))
        .collect();
    let ids: HashSet<&str> = shown.iter().map(|(id, _, _)| id.as_str()).collect();
    let mut incidents: Vec<PublicIncident> = records
        .iter()
        .filter(|r| r.profile_ids.iter().any(|id| ids.contains(id.as_str())))
        .filter(|r| r.resolved_at.is_none_or(|at| now - at < Duration::days(7)))
        .map(|r| PublicIncident {
            title: r.title.clone(),
            status: r.status,
            opened_at: r.opened_at,
            resolved_at: r.resolved_at,
            updates: r.timeline.iter().rev().filter(|e| e.kind == EntryKind::Update).map(|e| Entry { author: None, ..e.clone() }).collect(),
        })
        .collect();
    incidents.sort_by(|a, b| a.resolved_at.is_some().cmp(&b.resolved_at.is_some()).then(b.opened_at.cmp(&a.opened_at)));
    StatusPageView {
        settings: settings.clone(),
        title: if settings.title.trim().is_empty() { "Service status".into() } else { settings.title.clone() },
        overall: shown.iter().map(|(_, _, h)| *h).filter(|h| *h != Health::Unknown).max().unwrap_or(Health::Unknown),
        components: shown.iter().map(|(_, name, health)| PublicComponent { name: name.clone(), health: *health }).collect(),
        incidents,
        generated_at: now,
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// The status page as a self-contained document: public and unauthenticated,
/// so every value from the estate is escaped, nothing is fetched, and it
/// refreshes itself once a minute.
pub fn status_html(view: &StatusPageView) -> String {
    let (headline, tone) = match view.overall {
        Health::Healthy => ("All systems operational", "ok"),
        Health::Degraded => ("Some systems are degraded", "warn"),
        Health::Down => ("Some systems are down", "crit"),
        Health::Unknown => ("Status unknown", "unknown"),
    };
    let word = |h: Health| match h {
        Health::Healthy => ("Operational", "ok"),
        Health::Degraded => ("Degraded", "warn"),
        Health::Down => ("Down", "crit"),
        Health::Unknown => ("Unknown", "unknown"),
    };
    let components: String = view
        .components
        .iter()
        .map(|c| {
            let (w, t) = word(c.health);
            format!("<li><span>{}</span><span class=\"{t}\">{w}</span></li>", escape(&c.name))
        })
        .collect();
    let incidents: String = if view.incidents.is_empty() {
        "<p class=\"quiet\">No incidents in the last seven days.</p>".to_string()
    } else {
        view.incidents
            .iter()
            .map(|i| {
                let updates: String = if i.updates.is_empty() {
                    "<p class=\"quiet\">We are looking into it.</p>".to_string()
                } else {
                    i.updates
                        .iter()
                        .map(|u| format!("<p><time>{}</time> {}</p>", u.at.format("%b %-d, %H:%M UTC"), escape(&u.text)))
                        .collect()
                };
                format!(
                    "<article><h3>{}</h3><p class=\"meta\">{} · since {}</p>{updates}</article>",
                    escape(&i.title),
                    if i.resolved_at.is_some() { "Resolved" } else { status_word(i.status) },
                    i.opened_at.format("%b %-d, %H:%M UTC"),
                )
            })
            .collect()
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta http-equiv=\"refresh\" content=\"60\"><title>{title}</title><style>\
body{{margin:0;font:15px/1.5 system-ui,sans-serif;background:#0b0b0c;color:#e8e8ea}}main{{max-width:720px;margin:0 auto;padding:32px 16px}}\
h1{{font-size:22px;margin:0 0 16px}}.banner{{padding:14px 16px;border-radius:10px;margin-bottom:24px;font-weight:600}}\
.banner.ok{{background:#12321f;color:#7ee2a8}}.banner.warn{{background:#3a2c0b;color:#f2c063}}.banner.crit{{background:#3d1414;color:#ff8f8f}}.banner.unknown{{background:#222;color:#aaa}}\
ul{{list-style:none;padding:0;margin:0 0 28px;border:1px solid #26262a;border-radius:10px}}li{{display:flex;justify-content:space-between;padding:10px 14px;border-top:1px solid #26262a}}li:first-child{{border-top:0}}\
.ok{{color:#7ee2a8}}.warn{{color:#f2c063}}.crit{{color:#ff8f8f}}.unknown{{color:#999}}h2{{font-size:15px;color:#aaa;margin:0 0 10px}}\
article{{border:1px solid #26262a;border-radius:10px;padding:12px 14px;margin-bottom:12px}}h3{{margin:0;font-size:15px}}.meta,.quiet{{color:#999;font-size:13px}}time{{color:#999;font-size:13px;margin-right:6px}}\
footer{{color:#777;font-size:12px;margin-top:24px}}</style></head><body><main><h1>{title}</h1>\
<div class=\"banner {tone}\">{headline}</div><ul>{components}</ul><h2>Incidents</h2>{incidents}\
<footer>Updated {updated}</footer></main></body></html>",
        title = escape(&view.title),
        updated = view.generated_at.format("%b %-d, %H:%M:%S UTC"),
    )
}

// ------------------------------------------------------------ the sync loop

/// Every fifteen seconds: correlate what is firing, bring the log in line,
/// and write it when it moved. Idle when nothing changes.
pub fn spawn(state: std::sync::Arc<crate::state::AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            sync_now(&state).await;
        }
    });
}

pub async fn sync_now(state: &crate::state::AppState) {
    let (graph, names) = {
        let config = state.config.read().await;
        (
            crate::topology::Topology::build(&config.profiles),
            config.profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect::<HashMap<_, _>>(),
        )
    };
    let dashboard = state.dashboard().await;
    let live = crate::incidents::correlate(&dashboard.alerts, &dashboard.snapshots, &graph);
    let mut log = state.incident_log.write().await;
    if log.sync(&live, &names, Utc::now()) {
        if let Err(e) = log.save(&log_path(&state.config_path)) {
            tracing::warn!("incident log write failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_000_000, 0).unwrap() + Duration::minutes(minutes)
    }

    fn incident(ids: &[&str], severity: AlertSeverity) -> Incident {
        Incident {
            id: "inc-1".into(),
            title: "orders-api — 3 containers exited".into(),
            severity,
            health: Health::Down,
            started: at(0),
            last_seen: at(5),
            profile_ids: ids.iter().map(|s| s.to_string()).collect(),
            alert_ids: Vec::new(),
            roots: vec![ids[0].to_string()],
            lead_profile_id: ids[0].to_string(),
            acknowledged: false,
            confidence: crate::incidents::Confidence::High,
            grouping: Vec::new(),
        }
    }

    fn names() -> HashMap<String, String> {
        [("p1", "orders-api"), ("p2", "payments-db")].into_iter().map(|(a, b)| (a.into(), b.into())).collect()
    }

    #[test]
    fn a_record_opens_grows_resolves_and_reopens_with_its_alerts() {
        let mut log = IncidentLog::default();
        assert!(log.sync(&[incident(&["p1"], AlertSeverity::Warning)], &names(), at(1)));
        assert!(!log.sync(&[incident(&["p1"], AlertSeverity::Warning)], &names(), at(2)), "nothing new: nothing to write");
        log.sync(&[incident(&["p1", "p2"], AlertSeverity::Critical)], &names(), at(3));
        log.sync(&[], &names(), at(9));
        log.sync(&[incident(&["p1", "p2"], AlertSeverity::Critical)], &names(), at(12));
        let record = log.get("inc-1").unwrap();
        let kinds: Vec<EntryKind> = record.timeline.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![EntryKind::Opened, EntryKind::Joined, EntryKind::Escalated, EntryKind::Resolved, EntryKind::Reopened]);
        assert_eq!(record.timeline[1].text, "payments-db joined");
        assert!(record.resolved_at.is_none() && record.status == Status::Investigating);
    }

    #[test]
    fn hand_changes_are_on_the_timeline_but_resolution_follows_the_alerts() {
        let mut log = IncidentLog::default();
        log.sync(&[incident(&["p1"], AlertSeverity::Critical)], &names(), at(1));
        log.apply("inc-1", Change::Owner { owner: Some("Aniket".into()) }, Some("aniket".into()), at(2)).unwrap();
        log.apply("inc-1", Change::Status { status: Status::Identified }, None, at(3)).unwrap();
        log.apply("inc-1", Change::Update { text: "A bad deploy; rolling back.".into() }, None, at(4)).unwrap();
        assert!(log.apply("inc-1", Change::Status { status: Status::Resolved }, None, at(5)).is_err());
        assert!(log.apply("inc-1", Change::Note { text: "  ".into() }, None, at(5)).is_err());
        let record = log.get("inc-1").unwrap();
        assert_eq!(record.owner.as_deref(), Some("Aniket"));
        assert_eq!(record.timeline[1].text, "Aniket took it");
        assert_eq!(record.timeline[1].author.as_deref(), Some("aniket"));
    }

    #[test]
    fn the_status_page_shows_only_what_was_chosen_and_escapes_it() {
        let mut log = IncidentLog::default();
        log.sync(&[incident(&["p1"], AlertSeverity::Critical)], &names(), at(1));
        log.apply("inc-1", Change::Note { text: "internal: blame the db".into() }, None, at(2)).unwrap();
        log.apply("inc-1", Change::Update { text: "Checkout <is> slow".into() }, Some("aniket".into()), at(3)).unwrap();
        let components = vec![
            ("p1".to_string(), "orders-api".to_string(), Health::Down),
            ("p2".to_string(), "payments-db".to_string(), Health::Healthy),
        ];
        let settings = StatusPageSettings { enabled: true, title: "Acme status".into(), profile_ids: vec!["p1".into()] };
        let view = status_view(&settings, &components, &log.recent(10), at(4));
        assert_eq!(view.components.len(), 1, "only the chosen connection");
        assert_eq!(view.overall, Health::Down);
        assert_eq!(view.incidents[0].updates.len(), 1, "public updates only, never notes");
        assert!(view.incidents[0].updates[0].author.is_none(), "no names on the public page");
        let html = status_html(&view);
        assert!(html.contains("Checkout &lt;is&gt; slow") && !html.contains("blame the db") && !html.contains("payments-db"));
    }
}
