//! Domain types shared between the collectors, the agent, and the UI.
//!
//! Every type here is serialized straight to the frontend, so field names are
//! camelCase on the wire.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kinds of infrastructure this app knows how to talk to.
///
/// Adding a variant here is one of the three steps to supporting a new service;
/// see [`crate::registry`]. The frontend never enumerates these — it receives
/// them as opaque strings and renders from the registry's descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    Docker,
    Jenkins,
    Elasticsearch,
    Postgres,
    Mysql,
    HttpEndpoint,
    Kubernetes,
    Redis,
    Mongodb,
    Prometheus,
    Rabbitmq,
    Nginx,
    Kafka,
}

impl ServiceKind {
    /// Every variant. A registry test walks this to prove no kind can ship
    /// without the descriptor the UI needs to display it.
    pub const ALL: &'static [ServiceKind] = &[
        ServiceKind::Docker,
        ServiceKind::Jenkins,
        ServiceKind::Elasticsearch,
        ServiceKind::Postgres,
        ServiceKind::Mysql,
        ServiceKind::HttpEndpoint,
        ServiceKind::Kubernetes,
        ServiceKind::Redis,
        ServiceKind::Mongodb,
        ServiceKind::Prometheus,
        ServiceKind::Rabbitmq,
        ServiceKind::Nginx,
        ServiceKind::Kafka,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ServiceKind::Docker => "Docker",
            ServiceKind::Jenkins => "Jenkins",
            ServiceKind::Elasticsearch => "Elasticsearch",
            ServiceKind::Postgres => "PostgreSQL",
            ServiceKind::Mysql => "MySQL",
            ServiceKind::HttpEndpoint => "HTTP endpoint",
            ServiceKind::Kubernetes => "Kubernetes",
            ServiceKind::Redis => "Redis",
            ServiceKind::Mongodb => "MongoDB",
            ServiceKind::Prometheus => "Prometheus",
            ServiceKind::Rabbitmq => "RabbitMQ",
            ServiceKind::Nginx => "Nginx",
            ServiceKind::Kafka => "Kafka",
        }
    }

    /// SQL databases specifically — the kinds the shared `database` collector
    /// and its query diagnostics apply to. MongoDB and Redis are stores too but
    /// have nothing in common with this code path, so they are deliberately not
    /// included.
    pub fn is_database(self) -> bool {
        matches!(self, ServiceKind::Postgres | ServiceKind::Mysql)
    }
}

/// Rolled-up health of a service or one of its components.
///
/// Ordered worst-last so `iter().max()` gives the aggregate health of a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Unknown,
    Healthy,
    Degraded,
    Down,
}

impl Health {
    pub fn label(self) -> &'static str {
        match self {
            Health::Unknown => "unknown",
            Health::Healthy => "healthy",
            Health::Degraded => "degraded",
            Health::Down => "down",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl LogLevel {
    /// Best-effort level detection for log lines that carry no structure.
    ///
    /// Deliberately conservative: it only matches tokens that are very unlikely
    /// to appear as ordinary prose in a log body.
    pub fn infer(line: &str) -> LogLevel {
        let head: String = line.chars().take(200).collect::<String>().to_ascii_lowercase();
        if head.contains("fatal") || head.contains("panic") || head.contains("emergency") {
            LogLevel::Fatal
        } else if head.contains("error")
            || head.contains("err ")
            || head.contains("exception")
            || head.contains("traceback")
            || head.contains("failed")
            || head.contains("failure")
        {
            LogLevel::Error
        } else if head.contains("warn") || head.contains("deprecat") {
            LogLevel::Warn
        } else if head.contains("debug") {
            LogLevel::Debug
        } else if head.contains("trace") {
            LogLevel::Trace
        } else {
            LogLevel::Info
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricUnit {
    Count,
    Percent,
    Bytes,
    Millis,
    Seconds,
    PerSecond,
}

/// A single named measurement attached to a service or component.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub unit: MetricUnit,
    /// When set and exceeded, the UI tints the metric and the alert engine
    /// raises a warning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn_above: Option<f64>,
}

impl Metric {
    pub fn new(key: &str, label: &str, value: f64, unit: MetricUnit) -> Self {
        Metric {
            key: key.to_string(),
            label: label.to_string(),
            value,
            unit,
            warn_above: None,
        }
    }

    pub fn warn_above(mut self, threshold: f64) -> Self {
        self.warn_above = Some(threshold);
        self
    }

    pub fn is_breaching(&self) -> bool {
        self.warn_above.is_some_and(|t| self.value > t)
    }
}

// ---------------------------------------------------------------- sections
//
// A service's inventory is a set of *sections*, each a table of typed rows.
//
// This replaces an earlier flat `Vec<ComponentStatus>` where every entity — a
// container, a Jenkins job, an ES shard, a running query — shared one
// `{ name, health, detail, metrics, tags }` shape. That model had nowhere to
// record *what kind of thing* a row was, so collectors encoded it into the
// display name (`"node · es-data-1"`, `"query · pid 14832"`) and crammed
// several facts into one `detail` string. The SQL of a slow query ended up
// truncated into a column it shared with `"Up 6 days (healthy)"`.
//
// Sections fix that: each entity type gets its own table with its own typed,
// sortable columns.

/// How a cell should be rendered. The UI has one renderer per kind, so a new
/// service type gets correct formatting without any frontend work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CellKind {
    Text,
    /// Monospace and wrapping — SQL, commands, stack frames, identifiers.
    Code,
    /// Rendered as a health pill with glyph + label.
    Status,
    Metric {
        unit: MetricUnit,
    },
    /// Seconds, formatted as `4m 12s`.
    Duration,
    Timestamp,
    Tags,
    /// A proportional bar. The ceiling travels with the value, since a limit
    /// like a container's memory cap differs per row.
    Gauge {
        unit: MetricUnit,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Start,
    End,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSpec {
    pub key: String,
    pub label: String,
    pub kind: CellKind,
    pub align: Align,
    /// The identifying column, rendered with the row's health dot.
    pub primary: bool,
    pub sortable: bool,
    /// Hidden on narrow layouts; the row stays readable without it.
    pub secondary: bool,
    /// This column identifies *which version* of the component is running — a
    /// container image, a build number, a chart revision.
    ///
    /// The change engine diffs it between polls, and a move becomes a deploy
    /// marker on the timeline. Tagging one column is the entire opt-in: a
    /// collector gets deploy detection without writing any, and the engine
    /// stays free of per-service knowledge.
    pub revision: bool,
}

impl ColumnSpec {
    fn of(key: &str, label: &str, kind: CellKind) -> Self {
        // Numeric kinds right-align by default so digits line up.
        let align = match kind {
            CellKind::Metric { .. } | CellKind::Duration | CellKind::Gauge { .. } => Align::End,
            _ => Align::Start,
        };
        ColumnSpec {
            key: key.to_string(),
            label: label.to_string(),
            kind,
            align,
            primary: false,
            sortable: true,
            secondary: false,
            revision: false,
        }
    }

    pub fn text(key: &str, label: &str) -> Self {
        Self::of(key, label, CellKind::Text)
    }

    pub fn code(key: &str, label: &str) -> Self {
        Self::of(key, label, CellKind::Code).not_sortable()
    }

    pub fn status(key: &str, label: &str) -> Self {
        Self::of(key, label, CellKind::Status)
    }

    pub fn metric(key: &str, label: &str, unit: MetricUnit) -> Self {
        Self::of(key, label, CellKind::Metric { unit })
    }

    pub fn duration(key: &str, label: &str) -> Self {
        Self::of(key, label, CellKind::Duration)
    }

    pub fn timestamp(key: &str, label: &str) -> Self {
        Self::of(key, label, CellKind::Timestamp)
    }

    pub fn tags(key: &str, label: &str) -> Self {
        Self::of(key, label, CellKind::Tags).not_sortable()
    }

    pub fn gauge(key: &str, label: &str, unit: MetricUnit) -> Self {
        Self::of(key, label, CellKind::Gauge { unit })
    }

    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    pub fn secondary(mut self) -> Self {
        self.secondary = true;
        self
    }

    pub fn not_sortable(mut self) -> Self {
        self.sortable = false;
        self
    }

    /// Mark this column as the component's version identifier. See
    /// [`ColumnSpec::revision`]; at most one per section is meaningful.
    pub fn revision(mut self) -> Self {
        self.revision = true;
        self
    }

    /// The unit this column's values carry, when it holds numbers at all.
    /// `None` for text, status, tags, and timestamps — nothing chartable.
    pub fn unit(&self) -> Option<MetricUnit> {
        match self.kind {
            CellKind::Metric { unit } | CellKind::Gauge { unit } => Some(unit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CellValue {
    Null,
    Text {
        value: String,
    },
    Number {
        value: f64,
    },
    /// Seconds.
    Duration {
        value: f64,
    },
    Timestamp {
        value: DateTime<Utc>,
    },
    Tags {
        value: Vec<String>,
    },
    Status {
        value: Health,
    },
    Gauge {
        value: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        warn_above: Option<f64>,
    },
}

impl CellValue {
    pub fn text(value: impl Into<String>) -> Self {
        CellValue::Text {
            value: value.into(),
        }
    }

    pub fn number(value: f64) -> Self {
        CellValue::Number { value }
    }

    pub fn duration(seconds: f64) -> Self {
        CellValue::Duration { value: seconds }
    }

    pub fn timestamp(value: DateTime<Utc>) -> Self {
        CellValue::Timestamp { value }
    }

    pub fn tags(value: Vec<String>) -> Self {
        CellValue::Tags { value }
    }

    pub fn status(value: Health) -> Self {
        CellValue::Status { value }
    }

    pub fn gauge(value: f64, max: Option<f64>) -> Self {
        CellValue::Gauge {
            value,
            max,
            warn_above: None,
        }
    }

    pub fn gauge_warn(value: f64, max: Option<f64>, warn_above: f64) -> Self {
        CellValue::Gauge {
            value,
            max,
            warn_above: Some(warn_above),
        }
    }

    /// Optional text that reads as absent when empty, so collectors can pass a
    /// possibly-blank field straight through.
    pub fn maybe_text(value: Option<String>) -> Self {
        match value.filter(|v| !v.trim().is_empty()) {
            Some(value) => CellValue::Text { value },
            None => CellValue::Null,
        }
    }
}

/// A mutating operation offered on one row, wired to an assistant tool so the
/// UI button and the agent take exactly the same confirmed path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowAction {
    pub tool: String,
    pub label: String,
    /// Tool arguments minus `connection`, which the caller supplies.
    pub args: serde_json::Value,
    pub destructive: bool,
}

impl RowAction {
    pub fn new(tool: &str, label: &str, args: serde_json::Value) -> Self {
        RowAction {
            tool: tool.to_string(),
            label: label.to_string(),
            args,
            destructive: false,
        }
    }

    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub id: String,
    /// Human name, used by alerts and the assistant.
    pub name: String,
    pub health: Health,
    /// One line describing this row's state, for alert bodies and toasts.
    pub summary: String,
    pub cells: std::collections::HashMap<String, CellValue>,
    #[serde(default)]
    pub actions: Vec<RowAction>,
    /// Domain payload only a purpose-built view reads — e.g. a Jenkins job's
    /// recent build results for the history strip. The generic table ignores
    /// it, which is what lets bespoke views exist without the shared model
    /// growing a field per service.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

impl Row {
    pub fn new(id: impl Into<String>, name: impl Into<String>, health: Health) -> Self {
        let name = name.into();
        Row {
            id: id.into(),
            summary: name.clone(),
            name,
            health,
            cells: std::collections::HashMap::new(),
            actions: Vec::new(),
            extra: serde_json::Value::Null,
        }
    }

    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn cell(mut self, key: &str, value: CellValue) -> Self {
        self.cells.insert(key.to_string(), value);
        self
    }

    pub fn action(mut self, action: RowAction) -> Self {
        self.actions.push(action);
        self
    }

    pub fn extra(mut self, value: serde_json::Value) -> Self {
        self.extra = value;
        self
    }

    pub fn is_problem(&self) -> bool {
        matches!(self.health, Health::Down | Health::Degraded)
    }
}

/// One table of like things inside a service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub id: String,
    pub label: String,
    pub columns: Vec<ColumnSpec>,
    pub rows: Vec<Row>,
    /// Display order; lower sorts first. A section that only matters during an
    /// incident (active queries, the build queue) should outrank inventory.
    pub priority: u8,
    pub empty_text: String,
    /// Keep per-row history for this section's numeric columns.
    ///
    /// Opt-in because cardinality is the whole problem: a host has 25
    /// containers and a cluster has 8 nodes — worth charting — while an index
    /// list runs to hundreds and a pod list to thousands. Set it only where the
    /// entities are bounded *and* live long enough for a series to mean
    /// something. Untracked sections still get a component page; it shows live
    /// values instead of history.
    #[serde(default)]
    pub tracked: bool,
}

impl Section {
    pub fn new(id: &str, label: &str, columns: Vec<ColumnSpec>) -> Self {
        Section {
            id: id.to_string(),
            label: label.to_string(),
            columns,
            rows: Vec::new(),
            priority: 50,
            empty_text: "Nothing to show.".to_string(),
            tracked: false,
        }
    }

    /// Keep per-row history for this section. See [`Section::tracked`] — only
    /// for bounded, long-lived entities.
    pub fn tracked(mut self) -> Self {
        self.tracked = true;
        self
    }

    pub fn rows(mut self, rows: Vec<Row>) -> Self {
        self.rows = rows;
        self
    }

    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    pub fn empty_text(mut self, text: &str) -> Self {
        self.empty_text = text.to_string();
        self
    }

    /// Worst health first, then by name — problems surface without the reader
    /// having to sort.
    pub fn sorted_by_health(mut self) -> Self {
        self.rows
            .sort_by(|a, b| b.health.cmp(&a.health).then_with(|| a.name.cmp(&b.name)));
        self
    }

    pub fn problem_count(&self) -> usize {
        self.rows.iter().filter(|r| r.is_problem()).count()
    }
}

/// The result of one polling cycle against one configured connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSnapshot {
    pub profile_id: String,
    pub profile_name: String,
    pub kind: ServiceKind,
    pub health: Health,
    pub summary: String,
    pub metrics: Vec<Metric>,
    pub sections: Vec<Section>,
    pub checked_at: DateTime<Utc>,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl HealthSnapshot {
    /// Snapshot representing a connection we could not reach at all.
    pub fn unreachable(
        profile_id: &str,
        profile_name: &str,
        kind: ServiceKind,
        error: String,
        latency_ms: u64,
    ) -> Self {
        HealthSnapshot {
            profile_id: profile_id.to_string(),
            profile_name: profile_name.to_string(),
            kind,
            health: Health::Down,
            summary: format!("{} is unreachable", kind.label()),
            metrics: Vec::new(),
            sections: Vec::new(),
            checked_at: Utc::now(),
            latency_ms,
            error: Some(error),
        }
    }
}

/// What *kind* of record a log line is.
///
/// A single blended stream was the problem: forty lines of Jenkins console
/// output per failed build, plus a slow query that was already a row in the
/// Active queries table, drowned the container stdout an operator actually
/// wanted. Classifying at the source lets the log view stay clean by default
/// without discarding anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    /// Continuous operational output — container stdout/stderr, ES documents.
    Runtime,
    /// Discrete and meaningful — build results, endpoint up/down transitions.
    Event,
    /// Build console output. Belongs to a build, not to the global stream.
    Build,
    /// Derived from a diagnostic rather than emitted by the service.
    Diagnostic,
}

impl LogStream {
    /// Whether this stream belongs in the shared log view by default.
    pub fn in_global_stream(self) -> bool {
        matches!(self, LogStream::Runtime | LogStream::Event)
    }
}

/// A normalized log line from any source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEvent {
    pub id: String,
    pub profile_id: String,
    pub profile_name: String,
    pub kind: ServiceKind,
    /// The component the line came from: container name, job name, index, etc.
    pub source: String,
    pub level: LogLevel,
    pub message: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default = "default_stream")]
    pub stream: LogStream,
}

fn default_stream() -> LogStream {
    LogStream::Runtime
}

impl LogEvent {
    pub fn new(
        profile_id: &str,
        profile_name: &str,
        kind: ServiceKind,
        source: &str,
        level: LogLevel,
        message: String,
        timestamp: DateTime<Utc>,
    ) -> Self {
        LogEvent {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            profile_name: profile_name.to_string(),
            kind,
            source: source.to_string(),
            level,
            message,
            timestamp,
            stream: LogStream::Runtime,
        }
    }

    /// Reclassify. Collectors call this for anything that is not continuous
    /// runtime output.
    pub fn stream(mut self, stream: LogStream) -> Self {
        self.stream = stream;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

/// A condition the alert engine considers worth surfacing.
///
/// `fingerprint` identifies the *condition*, not the occurrence, so a
/// still-failing check updates `last_seen` instead of piling up duplicates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: String,
    pub fingerprint: String,
    pub profile_id: String,
    pub profile_name: String,
    pub kind: ServiceKind,
    pub severity: AlertSeverity,
    pub title: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub active: bool,
    /// Set once the user acknowledges it in the UI; acknowledged alerts stay
    /// visible but stop counting toward the dashboard badge.
    pub acknowledged: bool,
}

/// What kind of change was observed. Kept small and generic — the engine that
/// produces these knows nothing about any particular service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A component's revision column moved: a redeploy.
    Deployed,
    /// A component that was not in the previous snapshot.
    Appeared,
}

/// Where a change came from.
///
/// Worth distinguishing on the timeline: `Observed` means the app watched a
/// revision move and is certain it happened, while `Reported` means something
/// external asserted it. A pipeline that posts a deploy and then fails still
/// leaves a marker, so the two must not read identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSource {
    Observed,
    Reported,
}

impl Default for ChangeSource {
    fn default() -> Self {
        ChangeSource::Observed
    }
}

/// A moment when something about the infrastructure changed, derived by diffing
/// consecutive snapshots.
///
/// This is the deploy marker the timeline draws. It is not an alert: nothing is
/// wrong, and it never has a severity or an acknowledgement. It exists so a
/// spike in a chart can be read next to the thing that caused it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeEvent {
    pub id: String,
    pub profile_id: String,
    pub profile_name: String,
    pub kind: ServiceKind,
    pub change: ChangeKind,
    #[serde(default)]
    pub source: ChangeSource,
    /// Which section of the inventory it came from, e.g. "Containers".
    /// Externally reported deploys use the reporter's label, or "Reported".
    pub section: String,
    /// The row's name, e.g. a container or job.
    pub component: String,
    /// Previous revision. `None` when the component is newly present.
    pub from: Option<String>,
    pub to: String,
    pub at: DateTime<Utc>,
    /// One line suitable for a tooltip.
    pub summary: String,
}

/// Everything the dashboard needs in one payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardState {
    pub snapshots: Vec<HealthSnapshot>,
    pub alerts: Vec<Alert>,
    pub overall_health: Health,
    pub services_total: usize,
    pub services_healthy: usize,
    pub services_down: usize,
    pub generated_at: DateTime<Utc>,
}
