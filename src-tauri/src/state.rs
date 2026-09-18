//! Shared application state: live collectors, the latest snapshots, the log
//! ring buffer, active alerts, assistant conversations, and the pending
//! action-approval table.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex, RwLock};

use crate::collectors::Collector;
use crate::config::AppConfig;
use crate::error::{AppError, Result};
use crate::history::HistoryStore;
use crate::llm::ChatMessage;
use crate::model::{
    Alert, AlertSeverity, DashboardState, Health, HealthSnapshot, LogEvent, LogLevel, LogStream,
    ServiceKind,
};

/// Query against the in-memory log buffer.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogFilter {
    pub profile_ids: Option<Vec<String>>,
    pub kinds: Option<Vec<ServiceKind>>,
    pub min_level: Option<LogLevel>,
    /// Which streams to include. Empty or absent means every stream — the
    /// caller decides what "default" is, since the log view and the correlation
    /// view want different things.
    pub streams: Option<Vec<LogStream>>,
    /// Case-insensitive substring match over the message and source.
    pub search: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    /// Restrict to lines matching one clustered message template, e.g.
    /// `ERROR: connection refused upstream <*>`.
    ///
    /// Matching is against the template itself rather than a re-run of the
    /// clustering, so a selection stays stable as new lines arrive.
    pub pattern: Option<String>,
}

impl LogFilter {
    fn matches(&self, event: &LogEvent) -> bool {
        if let Some(ids) = &self.profile_ids {
            if !ids.is_empty() && !ids.contains(&event.profile_id) {
                return false;
            }
        }
        if let Some(kinds) = &self.kinds {
            if !kinds.is_empty() && !kinds.contains(&event.kind) {
                return false;
            }
        }
        if let Some(min) = self.min_level {
            if event.level < min {
                return false;
            }
        }
        if let Some(streams) = &self.streams {
            if !streams.is_empty() && !streams.contains(&event.stream) {
                return false;
            }
        }
        if let Some(since) = self.since {
            if event.timestamp < since {
                return false;
            }
        }
        if let Some(search) = &self.search {
            if !search.is_empty() {
                let needle = search.to_ascii_lowercase();
                if !event.message.to_ascii_lowercase().contains(&needle)
                    && !event.source.to_ascii_lowercase().contains(&needle)
                {
                    return false;
                }
            }
        }
        if let Some(pattern) = &self.pattern {
            if !pattern.is_empty() && !crate::patterns::template_matches(pattern, &event.message) {
                return false;
            }
        }
        true
    }
}

/// Bounded in-memory log store. Oldest lines are dropped once it is full;
/// nothing is written to disk, so an operator's logs stay on their machine.
pub struct LogBuffer {
    entries: VecDeque<LogEvent>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        LogBuffer {
            entries: VecDeque::with_capacity(capacity.min(4096)),
            capacity: capacity.max(100),
        }
    }

    pub fn push_many(&mut self, events: impl IntoIterator<Item = LogEvent>) {
        for event in events {
            if self.entries.len() >= self.capacity {
                self.entries.pop_front();
            }
            self.entries.push_back(event);
        }
    }

    /// Newest first, capped by `filter.limit`.
    pub fn query(&self, filter: &LogFilter) -> Vec<LogEvent> {
        let limit = filter.limit.unwrap_or(500).clamp(1, 5000);
        self.entries
            .iter()
            .rev()
            .filter(|event| filter.matches(event))
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drop everything belonging to a connection that was removed.
    pub fn drop_profile(&mut self, profile_id: &str) {
        self.entries.retain(|event| event.profile_id != profile_id);
    }
}

/// A model reading and the state it was read from.
#[derive(Clone)]
pub struct CachedReading {
    /// Health, summary and alert, hashed. A change here invalidates the text.
    pub fingerprint: String,
    pub reading: ComponentReading,
}

/// What the assistant makes of one failing component.
///
/// `grounds` is not decoration. This is the one place in the product where a
/// model's words appear without the operator having asked a question, so the
/// facts it was given travel with it — a reading nobody can check against its
/// inputs is a rumour with good typography, and the rest of this UI refuses to
/// show one.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentReading {
    pub text: String,
    pub model: String,
    pub grounds: Vec<String>,
    pub read_at: DateTime<Utc>,
}

/// Alerts keyed by fingerprint so a persistent condition updates in place
/// instead of accumulating a new row on every poll.
#[derive(Default)]
pub struct AlertStore {
    alerts: HashMap<String, Alert>,
}

impl AlertStore {
    /// Replace the set of conditions currently firing for one connection.
    /// Anything previously active and now absent is marked resolved.
    pub fn reconcile(&mut self, profile_id: &str, incoming: Vec<Alert>) -> Vec<Alert> {
        self.reconcile_within(profile_id, incoming, false)
    }

    /// The same, for the prediction engine's own alerts.
    ///
    /// Kept apart from [`reconcile`](Self::reconcile) because the two engines
    /// answer different questions — what is true now, and what is expected —
    /// and neither should be able to silence the other.
    pub fn reconcile_predictions(&mut self, profile_id: &str, incoming: Vec<Alert>) -> Vec<Alert> {
        self.reconcile_within(profile_id, incoming, true)
    }

    fn reconcile_within(
        &mut self,
        profile_id: &str,
        incoming: Vec<Alert>,
        predictions: bool,
    ) -> Vec<Alert> {
        let now = Utc::now();
        let mut seen = HashSet::new();
        let mut newly_raised = Vec::new();

        for mut alert in incoming {
            seen.insert(alert.fingerprint.clone());
            match self.alerts.get_mut(&alert.fingerprint) {
                Some(existing) if existing.active => {
                    existing.last_seen = now;
                    existing.detail = alert.detail;
                    existing.severity = alert.severity;
                }
                Some(existing) => {
                    // Previously resolved and now firing again: reopen it, but
                    // clear the acknowledgement so it is not silently hidden.
                    existing.active = true;
                    existing.acknowledged = false;
                    existing.first_seen = now;
                    existing.last_seen = now;
                    existing.detail = alert.detail;
                    existing.severity = alert.severity;
                    newly_raised.push(existing.clone());
                }
                None => {
                    alert.first_seen = now;
                    alert.last_seen = now;
                    self.alerts.insert(alert.fingerprint.clone(), alert.clone());
                    newly_raised.push(alert);
                }
            }
        }

        // Resolve what this engine raised and no longer sees — but only what
        // *this* engine raised. Two sources now write alerts for the same
        // connection: the snapshot engine, from what is true right now, and
        // `predict.rs`, from what is expected. Each poll would otherwise
        // resolve every prediction, and each prediction run would resolve every
        // live alert, so both would flicker on and off forever.
        for alert in self.alerts.values_mut() {
            let mine = alert.fingerprint.starts_with(crate::predict::ALERT_PREFIX) == predictions;
            if mine
                && alert.profile_id == profile_id
                && alert.active
                && !seen.contains(&alert.fingerprint)
            {
                alert.active = false;
                alert.last_seen = now;
            }
        }

        // Keep resolved alerts around briefly so the UI can show what just
        // recovered, then forget them.
        let cutoff = now - ChronoDuration::hours(1);
        self.alerts
            .retain(|_, alert| alert.active || alert.last_seen > cutoff);

        newly_raised
    }

    pub fn all(&self) -> Vec<Alert> {
        let mut alerts: Vec<Alert> = self.alerts.values().cloned().collect();
        alerts.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then_with(|| b.severity.cmp(&a.severity))
                .then_with(|| b.last_seen.cmp(&a.last_seen))
        });
        alerts
    }

    pub fn acknowledge(&mut self, id: &str) -> Result<Alert> {
        let alert = self
            .alerts
            .values_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| AppError::Config(format!("no alert with id {id}")))?;
        alert.acknowledged = true;
        Ok(alert.clone())
    }

    pub fn drop_profile(&mut self, profile_id: &str) {
        self.alerts.retain(|_, alert| alert.profile_id != profile_id);
    }
}

/// Derive the alert set implied by one snapshot.
///
/// The rules are intentionally generic: collectors already express intent by
/// setting component health and metric thresholds, so the engine reads those
/// rather than hard-coding per-service knowledge.
pub fn alerts_from_snapshot(snapshot: &HealthSnapshot) -> Vec<Alert> {
    let mut alerts = Vec::new();
    let now = Utc::now();

    let make = |fingerprint: String,
                severity: AlertSeverity,
                title: String,
                detail: String,
                component: Option<String>| Alert {
        id: uuid::Uuid::new_v4().to_string(),
        fingerprint,
        profile_id: snapshot.profile_id.clone(),
        profile_name: snapshot.profile_name.clone(),
        kind: snapshot.kind,
        severity,
        title,
        detail,
        component,
        first_seen: now,
        last_seen: now,
        active: true,
        acknowledged: false,
    };

    if snapshot.health == Health::Down {
        alerts.push(make(
            format!("{}:unreachable", snapshot.profile_id),
            AlertSeverity::Critical,
            format!("{} is unreachable", snapshot.profile_name),
            snapshot
                .error
                .clone()
                .unwrap_or_else(|| snapshot.summary.clone()),
            None,
        ));
        // Nothing below can be trusted when the service is down.
        return alerts;
    }

    for metric in snapshot.metrics.iter().filter(|m| m.is_breaching()) {
        alerts.push(make(
            format!("{}:metric:{}", snapshot.profile_id, metric.key),
            AlertSeverity::Warning,
            format!("{} · {} above threshold", snapshot.profile_name, metric.label),
            format!(
                "{} is {} (threshold {})",
                metric.label,
                format_metric(metric.value, metric.unit),
                format_metric(metric.warn_above.unwrap_or_default(), metric.unit)
            ),
            None,
        ));
    }

    // Walk every section's rows. The fingerprint carries the section id so two
    // entities that happen to share a name in different sections — a Jenkins
    // job and an agent, say — never collide into one alert.
    for section in &snapshot.sections {
        for row in &section.rows {
            let severity = match row.health {
                Health::Down => AlertSeverity::Critical,
                Health::Degraded => AlertSeverity::Warning,
                _ => continue,
            };
            alerts.push(make(
                format!("{}:{}:{}", snapshot.profile_id, section.id, row.id),
                severity,
                format!("{} · {}", snapshot.profile_name, row.name),
                row.summary.clone(),
                Some(row.name.clone()),
            ));
        }
    }

    alerts
}

fn format_metric(value: f64, unit: crate::model::MetricUnit) -> String {
    use crate::model::MetricUnit::*;
    match unit {
        Percent => format!("{value:.1}%"),
        Bytes => {
            const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
            let mut v = value;
            let mut u = 0;
            while v >= 1024.0 && u < UNITS.len() - 1 {
                v /= 1024.0;
                u += 1;
            }
            format!("{v:.1} {}", UNITS[u])
        }
        Millis => format!("{value:.0}ms"),
        Seconds => format!("{value:.1}s"),
        PerSecond => format!("{value:.1}/s"),
        Count => format!("{value:.0}"),
    }
}

/// A mutating tool call waiting on the operator's decision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingConfirmation {
    pub call_id: String,
    pub conversation_id: String,
    pub tool: String,
    pub description: String,
    pub equivalent_command: String,
    pub profile_name: String,
    pub arguments: serde_json::Value,
    pub requested_at: DateTime<Utc>,
}

pub struct AppState {
    pub config: RwLock<AppConfig>,
    pub config_path: PathBuf,
    /// Built collectors, keyed by profile id. Rebuilt when a profile changes.
    pub collectors: RwLock<HashMap<String, Arc<dyn Collector>>>,
    pub snapshots: RwLock<HashMap<String, HealthSnapshot>>,
    pub logs: RwLock<LogBuffer>,
    pub alerts: RwLock<AlertStore>,
    /// Deploys and other changes derived by diffing consecutive snapshots.
    pub changes: RwLock<crate::changes::ChangeStore>,
    /// What the assistant says it found, so its claims can be rated beside the
    /// rules instead of disappearing into a conversation.
    pub claims: RwLock<crate::claims::ClaimStore>,
    /// Last log timestamp seen per profile, so each poll only asks for new lines.
    pub watermarks: RwLock<HashMap<String, DateTime<Utc>>>,
    /// Rolling metric history feeding sparklines, charts, and trend questions.
    pub history: RwLock<HistoryStore>,
    pub conversations: RwLock<HashMap<String, Vec<ChatMessage>>>,
    /// One-line model readings of a failing component, keyed by
    /// `profile:section:row`.
    ///
    /// The matrix asks for these on hover, and a hover is not a decision to
    /// spend money: 364 cells, and dragging the pointer across a block touches
    /// a dozen in a second. A reading is therefore computed once per *state* —
    /// the fingerprint covers the health, the summary and the alert behind it —
    /// and every later hover of an unchanged cell is free. It lives in memory
    /// with everything else here, so quitting forgets it.
    pub readings: RwLock<HashMap<String, CachedReading>>,
    /// Approval channels for in-flight mutating tool calls.
    pub pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    pub pending_meta: RwLock<HashMap<String, PendingConfirmation>>,
    /// Poller tasks, so reconfiguring a connection can cancel the old one.
    pub pollers: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// The deploy-ingest listener, so toggling it off actually closes the port.
    pub ingest: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The MCP listener, held for the same reason.
    pub mcp: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The judgement record — the one thing here that outlives the process.
    ///
    /// `None` when the store could not be opened. A journal that will not open
    /// must not stop the app from monitoring: losing the ability to rate
    /// findings is a bad day, and losing the ability to see the estate is an
    /// outage. The UI is told, rather than the failure being swallowed.
    pub journal: RwLock<Option<Arc<crate::journal::Journal>>>,
    /// Why the journal is unavailable, when it is.
    pub journal_error: RwLock<Option<String>>,
}

impl AppState {
    pub fn new(config: AppConfig, config_path: PathBuf) -> Self {
        let capacity = config.retention.log_capacity;
        AppState {
            config: RwLock::new(config),
            config_path,
            collectors: RwLock::new(HashMap::new()),
            snapshots: RwLock::new(HashMap::new()),
            logs: RwLock::new(LogBuffer::new(capacity)),
            alerts: RwLock::new(AlertStore::default()),
            changes: RwLock::new(crate::changes::ChangeStore::new()),
            claims: RwLock::new(crate::claims::ClaimStore::default()),
            watermarks: RwLock::new(HashMap::new()),
            history: RwLock::new(HistoryStore::default()),
            conversations: RwLock::new(HashMap::new()),
            readings: RwLock::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            pending_meta: RwLock::new(HashMap::new()),
            pollers: Mutex::new(HashMap::new()),
            ingest: Mutex::new(None),
            mcp: Mutex::new(None),
            journal: RwLock::new(None),
            journal_error: RwLock::new(None),
        }
    }

    /// Open the judgement record beside the config file.
    ///
    /// Deliberately fallible and deliberately non-fatal — see the field.
    pub async fn open_journal(&self) {
        let path = self
            .config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("journal.db");

        match crate::journal::Journal::open(&path).await {
            Ok(journal) => {
                *self.journal.write().await = Some(Arc::new(journal));
                *self.journal_error.write().await = None;
                tracing::info!(path = %path.display(), "judgement record opened");
            }
            Err(e) => {
                let message = e.to_string();
                tracing::error!(path = %path.display(), "judgement record unavailable: {message}");
                *self.journal_error.write().await = Some(message);
            }
        }
    }

    /// The journal, or an error naming why there is not one.
    pub async fn journal(&self) -> Result<Arc<crate::journal::Journal>> {
        if let Some(journal) = self.journal.read().await.clone() {
            return Ok(journal);
        }
        let reason = self
            .journal_error
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "not opened yet".to_string());
        Err(crate::error::AppError::Collector(format!(
            "the judgement record is unavailable: {reason}"
        )))
    }

    pub async fn save_config(&self) -> Result<()> {
        let config = self.config.read().await;
        config.save(&self.config_path)
    }

    pub async fn collector(&self, profile_id: &str) -> Result<Arc<dyn Collector>> {
        self.collectors
            .read()
            .await
            .get(profile_id)
            .cloned()
            .ok_or_else(|| {
                AppError::Collector(format!(
                    "the connection `{profile_id}` is not running. Enable it in Settings."
                ))
            })
    }

    pub async fn dashboard(&self) -> DashboardState {
        let snapshots_map = self.snapshots.read().await;
        let config = self.config.read().await;

        // Order the dashboard the way the user ordered their connections.
        let mut snapshots: Vec<HealthSnapshot> = config
            .profiles
            .iter()
            .filter_map(|p| snapshots_map.get(&p.id).cloned())
            .collect();
        snapshots.sort_by_key(|s| std::cmp::Reverse(s.health));

        let services_total = snapshots.len();
        let services_healthy = snapshots
            .iter()
            .filter(|s| s.health == Health::Healthy)
            .count();
        let services_down = snapshots.iter().filter(|s| s.health == Health::Down).count();
        let overall_health = snapshots
            .iter()
            .map(|s| s.health)
            .max()
            .unwrap_or(Health::Unknown);

        DashboardState {
            snapshots,
            alerts: self.alerts.read().await.all(),
            overall_health,
            services_total,
            services_healthy,
            services_down,
            generated_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnSpec, Metric, MetricUnit, Row, Section};

    fn snapshot(health: Health) -> HealthSnapshot {
        HealthSnapshot {
            profile_id: "p1".into(),
            profile_name: "prod docker".into(),
            kind: ServiceKind::Docker,
            health,
            summary: "summary".into(),
            metrics: Vec::new(),
            sections: Vec::new(),
            checked_at: Utc::now(),
            latency_ms: 5,
            error: None,
        }
    }

    fn event(level: LogLevel, message: &str) -> LogEvent {
        LogEvent::new("p1", "prod docker", ServiceKind::Docker, "api", level, message.into(), Utc::now())
    }

    #[test]
    fn log_buffer_drops_oldest_when_full() {
        let mut buffer = LogBuffer::new(100);
        buffer.push_many((0..150).map(|i| event(LogLevel::Info, &format!("line {i}"))));
        assert_eq!(buffer.len(), 100);
        let newest = buffer.query(&LogFilter::default());
        assert_eq!(newest[0].message, "line 149");
    }

    #[test]
    fn log_filter_respects_minimum_level() {
        let mut buffer = LogBuffer::new(100);
        buffer.push_many(vec![
            event(LogLevel::Debug, "noisy"),
            event(LogLevel::Error, "boom"),
        ]);
        let filter = LogFilter {
            min_level: Some(LogLevel::Warn),
            ..Default::default()
        };
        let found = buffer.query(&filter);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message, "boom");
    }

    #[test]
    fn stream_filter_separates_runtime_from_build_output() {
        let mut buffer = LogBuffer::new(100);
        buffer.push_many(vec![
            event(LogLevel::Info, "container stdout"),
            event(LogLevel::Error, "npm ERR! ELIFECYCLE").stream(LogStream::Build),
            event(LogLevel::Info, "build #7 finished").stream(LogStream::Event),
        ]);

        // The log view's default: real logs and discrete events, no console spam.
        let global = buffer.query(&LogFilter {
            streams: Some(vec![LogStream::Runtime, LogStream::Event]),
            ..Default::default()
        });
        assert_eq!(global.len(), 2);
        assert!(global.iter().all(|e| e.stream != LogStream::Build));

        // Nothing is discarded — an explicit request still reaches it.
        let build = buffer.query(&LogFilter {
            streams: Some(vec![LogStream::Build]),
            ..Default::default()
        });
        assert_eq!(build.len(), 1);
        assert_eq!(build[0].message, "npm ERR! ELIFECYCLE");

        // No stream filter means everything, which is what the assistant gets.
        assert_eq!(buffer.query(&LogFilter::default()).len(), 3);
    }

    #[test]
    fn only_runtime_and_event_belong_in_the_global_stream() {
        assert!(LogStream::Runtime.in_global_stream());
        assert!(LogStream::Event.in_global_stream());
        assert!(!LogStream::Build.in_global_stream());
        assert!(!LogStream::Diagnostic.in_global_stream());
    }

    #[test]
    fn log_search_matches_message_and_source() {
        let mut buffer = LogBuffer::new(10);
        buffer.push_many(vec![event(LogLevel::Info, "connection refused")]);
        let hit = |needle: &str| {
            !buffer
                .query(&LogFilter {
                    search: Some(needle.into()),
                    ..Default::default()
                })
                .is_empty()
        };
        assert!(hit("REFUSED"), "search is case-insensitive");
        assert!(hit("api"), "search also covers the source field");
        assert!(!hit("nothing here"));
    }

    #[test]
    fn unreachable_service_suppresses_downstream_alerts() {
        let mut snap = snapshot(Health::Down);
        snap.error = Some("connection refused".into());
        snap.sections.push(
            Section::new("components", "Components", vec![ColumnSpec::text("name", "Name").primary()])
                .rows(vec![Row::new("c1", "api", Health::Down)
                    .summary("exited")]),
        );

        let alerts = alerts_from_snapshot(&snap);
        assert_eq!(alerts.len(), 1, "one root cause, not a cascade");
        assert_eq!(alerts[0].severity, AlertSeverity::Critical);
    }

    #[test]
    fn breaching_metrics_and_components_both_alert() {
        let mut snap = snapshot(Health::Degraded);
        snap.metrics
            .push(Metric::new("cpu", "CPU", 95.0, MetricUnit::Percent).warn_above(85.0));
        snap.metrics
            .push(Metric::new("mem", "Memory", 10.0, MetricUnit::Percent).warn_above(85.0));
        snap.sections.push(
            Section::new("components", "Components", vec![ColumnSpec::text("name", "Name").primary()])
                .rows(vec![Row::new("c1", "api", Health::Down)
                    .summary("exited (137)")]),
        );

        let alerts = alerts_from_snapshot(&snap);
        assert_eq!(alerts.len(), 2, "only the breaching metric alerts");
        assert!(alerts.iter().any(|a| a.severity == AlertSeverity::Critical));
    }

    #[test]
    fn resolved_alerts_are_marked_inactive_not_deleted() {
        let mut store = AlertStore::default();
        let mut snap = snapshot(Health::Degraded);
        snap.sections.push(
            Section::new("components", "Components", vec![ColumnSpec::text("name", "Name").primary()])
                .rows(vec![Row::new("c1", "api", Health::Down)
                    .summary("exited")]),
        );

        let raised = store.reconcile("p1", alerts_from_snapshot(&snap));
        assert_eq!(raised.len(), 1);

        // Next poll: the component recovered.
        store.reconcile("p1", alerts_from_snapshot(&snapshot(Health::Healthy)));
        let all = store.all();
        assert_eq!(all.len(), 1);
        assert!(!all[0].active, "kept as recently-resolved");
    }

    #[test]
    fn a_persistent_condition_does_not_duplicate() {
        let mut store = AlertStore::default();
        let mut snap = snapshot(Health::Degraded);
        snap.sections.push(
            Section::new("components", "Components", vec![ColumnSpec::text("name", "Name").primary()])
                .rows(vec![Row::new("c1", "api", Health::Down)
                    .summary("exited")]),
        );

        store.reconcile("p1", alerts_from_snapshot(&snap));
        let second = store.reconcile("p1", alerts_from_snapshot(&snap));
        assert!(second.is_empty(), "already-firing alerts are not re-raised");
        assert_eq!(store.all().len(), 1);
    }
}
