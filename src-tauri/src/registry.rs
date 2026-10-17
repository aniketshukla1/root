//! The service registry: what kinds of infrastructure this app can monitor,
//! and everything the UI needs in order to present them.
//!
//! This module exists so that **adding a new kind of infrastructure is a
//! backend-only change**. The frontend enumerates no service types, hardcodes
//! no form fields, and maps no icons. It asks for the catalog, renders whatever
//! it is given, and a new collector appears in the UI — in the connection
//! picker, the settings form, the dashboard, the sidebar — with no frontend
//! edit at all.
//!
//! To add a service type:
//!   1. implement [`Collector`](crate::collectors::Collector),
//!   2. add a `ServiceKind` variant and wire it into `build_collector`,
//!   3. append a [`ServiceDescriptor`] to [`catalog`].
//!
//! Nothing else, anywhere, needs to know it exists.

use serde::{Deserialize, Serialize};

use crate::model::ServiceKind;

/// How the settings form should render one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    /// Rendered masked, and routed to the OS keychain rather than the config file.
    Password,
    Number,
    Boolean,
    Select,
    /// Text, but hinted as a URL.
    Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// A declarative description of one input. The frontend builds the control from
/// this; it has no per-service knowledge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldSpec {
    /// Key within `ConnectionProfile.options`, camelCase.
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SelectOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Tucked behind an "Advanced" disclosure so the common path stays short.
    pub advanced: bool,
}

impl FieldSpec {
    pub fn new(key: &str, label: &str, kind: FieldKind) -> Self {
        FieldSpec {
            key: key.to_string(),
            label: label.to_string(),
            kind,
            placeholder: None,
            help: None,
            required: false,
            default: None,
            options: Vec::new(),
            min: None,
            max: None,
            advanced: false,
        }
    }

    pub fn placeholder(mut self, text: &str) -> Self {
        self.placeholder = Some(text.to_string());
        self
    }

    pub fn help(mut self, text: &str) -> Self {
        self.help = Some(text.to_string());
        self
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }

    pub fn default(mut self, value: serde_json::Value) -> Self {
        self.default = Some(value);
        self
    }

    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }
}

/// A mutating operation a service supports, for display in Settings so an
/// operator can see exactly what enabling actions would grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    pub tool: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Whether this service contributes to the unified log stream.
    pub logs: bool,
    /// Mutating tools, empty when the service is inspect-only.
    pub actions: Vec<ActionSpec>,
    /// Named read-only drill-downs the assistant can call.
    pub diagnostics: Vec<String>,
}

/// Everything the UI needs to present one kind of infrastructure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDescriptor {
    pub kind: ServiceKind,
    pub label: String,
    /// Short monogram for the badge. The UI has no icon assets, so a tinted
    /// monogram beats a grab-bag of unicode glyphs that render inconsistently.
    pub monogram: String,
    /// Index into the categorical chart palette, so a service keeps one
    /// identity colour across the sidebar, its cards, and its charts.
    ///
    /// The UI maps this through a table that omits the green and red slots —
    /// those sit too close to the reserved status hues, and an identity colour
    /// must never read as a health signal. Six distinct accents are available;
    /// beyond that they wrap, which is fine because the monogram and label
    /// carry identity anyway.
    pub accent: usize,
    /// Grouping for the connection picker.
    pub category: String,
    pub blurb: String,
    pub endpoint: FieldSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<FieldSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<FieldSpec>,
    /// Service-specific options, plus the shared ones.
    pub fields: Vec<FieldSpec>,
    pub default_poll_secs: u64,
    pub capabilities: Capabilities,
}

/// Options every collector honours, appended to each descriptor so they are
/// declared once rather than repeated per service.
fn shared_fields(tls: bool) -> Vec<FieldSpec> {
    let mut fields = vec![FieldSpec::new("maxLogLines", "Max log lines per poll", FieldKind::Number)
        .default(serde_json::json!(200))
        .range(1.0, 2000.0)
        .help("Caps how much this connection can contribute per cycle.")
        .advanced()];
    if tls {
        fields.push(
            FieldSpec::new("insecureTls", "Skip TLS verification", FieldKind::Boolean)
                .help("For internal endpoints with self-signed certificates.")
                .advanced(),
        );
    }
    fields
}

fn token(label: &str, help: &str) -> FieldSpec {
    FieldSpec::new("__secret", label, FieldKind::Password).help(help)
}

fn user(placeholder: &str) -> FieldSpec {
    FieldSpec::new("__username", "Username", FieldKind::Text).placeholder(placeholder)
}

/// Every service type this build can monitor.
pub fn catalog() -> Vec<ServiceDescriptor> {
    vec![
        ServiceDescriptor {
            kind: ServiceKind::Docker,
            label: "Docker".to_string(),
            monogram: "DK".to_string(),
            accent: 0,
            category: "Containers".to_string(),
            blurb: "Containers, resource usage, health checks, and logs from a Docker Engine."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Docker host", FieldKind::Text)
                .placeholder("leave empty for the local socket")
                .help(
                    "Empty uses the local socket — Docker Desktop, Colima, Rancher Desktop, and \
                     rootless Podman are all detected automatically. For a remote daemon use \
                     tcp://host:2375.",
                ),
            username: None,
            credential: None,
            fields: shared_fields(false),
            default_poll_secs: 5,
            capabilities: Capabilities {
                logs: true,
                actions: vec![
                    ActionSpec { tool: "restart_container".into(), label: "Restart a container".into() },
                    ActionSpec { tool: "start_container".into(), label: "Start a container".into() },
                    ActionSpec { tool: "stop_container".into(), label: "Stop a container".into() },
                ],
                diagnostics: vec!["inspect_container".into()],
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Podman,
            label: "Podman".to_string(),
            monogram: "PO".to_string(),
            accent: 23,
            category: "Containers".to_string(),
            blurb: "Containers, their resource use and their logs — the same Engine API Docker \
                    serves, from a daemonless, rootless engine."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Socket or host", FieldKind::Text)
                .placeholder("leave blank for the local socket")
                .help(
                    "Blank searches the rootless socket, then the rootful one, then a podman \
                     machine on macOS. Podman has no daemon, so the socket only exists once \
                     `podman.socket` or `podman machine` is started.",
                ),
            username: None,
            credential: None,
            fields: shared_fields(false),
            default_poll_secs: 10,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::ThisMachine,
            label: "This computer".to_string(),
            monogram: "PC".to_string(),
            accent: 28,
            category: "Hosts".to_string(),
            blurb: "The machine root runs on, read directly: CPU, memory, every disk, network \
                    and the busiest processes. Nothing to install and nothing to enter."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Machine", FieldKind::Text)
                .placeholder("this computer")
                .help("Nothing to enter: root reads the machine it is running on."),
            username: None,
            credential: None,
            fields: {
                let mut fields = vec![
                    FieldSpec::new("diskWarn", "Disk warning", FieldKind::Number)
                        .default(serde_json::json!(90))
                        .range(1.0, 100.0)
                        .help("Percent full, on the fullest disk rather than the average."),
                    FieldSpec::new("memoryWarn", "Memory warning", FieldKind::Number)
                        .default(serde_json::json!(90))
                        .range(1.0, 100.0)
                        .help("Percent of memory in use, counting reclaimable memory as free."),
                ];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 5,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Otlp,
            label: "OpenTelemetry".to_string(),
            monogram: "OT".to_string(),
            accent: 29,
            category: "Observability".to_string(),
            blurb: "Receive OTLP from your services — traces, logs and metrics, protobuf or JSON — \
                    and see them beside everything root watches. Nothing to run in between."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Listen on", FieldKind::Text)
                .placeholder(crate::collectors::otlp::DEFAULT_ADDRESS)
                .help("Address and port for OTLP/HTTP. This machine only unless you give another address, which needs a token."),
            username: None,
            credential: Some(token(
                "Token",
                "Optional on this machine, required beyond it. Exporters send it as a bearer token.",
            )),
            fields: shared_fields(false),
            default_poll_secs: 15,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::LinuxHost,
            label: "Linux host".to_string(),
            monogram: "LX".to_string(),
            accent: 24,
            category: "Hosts".to_string(),
            blurb: "A VM or bare-metal server through node_exporter: disk fill per mount, \
                    memory that is actually available, and load measured per core."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "node_exporter URL", FieldKind::Text)
                .placeholder("web-1.internal")
                .required()
                .help(
                    "Port 9100 is assumed if you leave it off. This host needs node_exporter \
                     listening — root reaches servers through an agent rather than by holding \
                     SSH keys for the estate.",
                ),
            username: Some(user("optional")),
            credential: Some(token("Password", "If the exporter is behind basic auth.")),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("diskWarn", "Disk warning", FieldKind::Number)
                        .default(serde_json::json!(85))
                        .range(1.0, 100.0)
                        .help(
                            "Percent full, measured on the fullest filesystem rather than the \
                             average — a root volume at 99% beside an empty data volume \
                             averages to comfortable.",
                        ),
                    FieldSpec::new("memoryWarn", "Memory warning", FieldKind::Number)
                        .default(serde_json::json!(90))
                        .range(1.0, 100.0)
                        .help(
                            "Percent of memory in use, counted from MemAvailable. Free memory \
                             is near zero on a healthy Linux box and is not the same question.",
                        ),
                    FieldSpec::new("loadWarn", "Load per core warning", FieldKind::Number)
                        .default(serde_json::json!(1.5))
                        .range(0.1, 100.0)
                        .help("Load average divided by logical CPUs. 1.0 means fully committed."),
                ];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Nfs,
            label: "NFS exports".to_string(),
            monogram: "NF".to_string(),
            accent: 25,
            category: "Storage".to_string(),
            blurb: "Shared exports as the client sees them: fill, inode exhaustion, and stale \
                    mounts — which report zeroes that read as an empty, healthy export."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "node_exporter URL", FieldKind::Text)
                .placeholder("build-1.internal")
                .required()
                .help(
                    "A client that mounts the exports, not the NFS server. What matters is \
                     whether the path works from where the team uses it — a server can be \
                     healthy while a client's mount is stale.",
                ),
            username: Some(user("optional")),
            credential: Some(token("Password", "If the exporter is behind basic auth.")),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("pathPrefix", "Path prefix", FieldKind::Text)
                        .placeholder("/mnt/team-a")
                        .help(
                            "Narrow to the exports under one path, so a team can watch its own \
                             allocation. Blank watches every NFS mount on the client.",
                        ),
                    FieldSpec::new("fillWarn", "Fill warning", FieldKind::Number)
                        .default(serde_json::json!(85))
                        .range(1.0, 100.0)
                        .help("Percent of bytes used above which an export is degraded."),
                    FieldSpec::new("inodeWarn", "Inode warning", FieldKind::Number)
                        .default(serde_json::json!(85))
                        .range(1.0, 100.0)
                        .help(
                            "Percent of inodes used. An export full of small files runs out of \
                             these long before its bytes, and writes then fail with `No space \
                             left on device` while df still shows free space.",
                        ),
                ];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 30,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Jenkins,
            label: "Jenkins".to_string(),
            monogram: "JK".to_string(),
            accent: 1,
            category: "CI/CD".to_string(),
            blurb: "Job results, build queue, agents, and console output from failing builds."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Controller URL", FieldKind::Url)
                .placeholder("https://jenkins.internal")
                .required(),
            username: Some(user("monitor")),
            credential: Some(token(
                "API token",
                "Generate one under your Jenkins user → Configure → API Token.",
            )),
            fields: {
                let mut fields = vec![FieldSpec::new("jobFilter", "Job filter", FieldKind::Text)
                    .placeholder("only jobs containing this text")
                    .help("Leave empty to watch every job the credential can see.")];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: true,
                actions: vec![ActionSpec {
                    tool: "trigger_jenkins_build".into(),
                    label: "Trigger a build".into(),
                }],
                diagnostics: vec!["jenkins_build_log".into()],
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Elasticsearch,
            label: "Elasticsearch".to_string(),
            monogram: "ES".to_string(),
            accent: 2,
            category: "Search & analytics".to_string(),
            blurb: "Cluster health, node resources, shard and index stats, plus log tailing. \
                    Works with OpenSearch."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Cluster URL", FieldKind::Url)
                .placeholder("https://es.internal:9200")
                .required(),
            username: Some(
                FieldSpec::new("__username", "Username", FieldKind::Text)
                    .placeholder("optional")
                    .help("Leave empty to send the credential below as an API key instead."),
            ),
            credential: Some(token("Password or API key", "Basic auth password, or a base64 API key.")),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("logIndex", "Log index pattern", FieldKind::Text)
                        .placeholder("logs-*")
                        .help("Leave empty to monitor cluster health without tailing documents."),
                    FieldSpec::new("timestampField", "Timestamp field", FieldKind::Text)
                        .placeholder("@timestamp")
                        .default(serde_json::json!("@timestamp"))
                        .advanced(),
                ];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 15,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: vec!["elasticsearch_search".into()],
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Postgres,
            label: "PostgreSQL".to_string(),
            monogram: "PG".to_string(),
            accent: 3,
            category: "Databases".to_string(),
            blurb: "Connection saturation, table sizes, long-running queries, locks, and \
                    replication lag."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Connection URL", FieldKind::Url)
                .placeholder("postgres://host:5432/dbname")
                .help("The password goes in the field below, not in this URL.")
                .required(),
            username: Some(user("monitor")),
            credential: Some(token("Password", "Stored in the OS keychain, never in the config file.")),
            fields: database_fields(),
            default_poll_secs: 15,
            capabilities: database_capabilities(),
        },
        ServiceDescriptor {
            kind: ServiceKind::Mysql,
            label: "MySQL".to_string(),
            monogram: "MY".to_string(),
            accent: 4,
            category: "Databases".to_string(),
            blurb: "Thread and connection counts, schema sizes, long-running queries, and \
                    replication state. Works with MariaDB."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Connection URL", FieldKind::Url)
                .placeholder("mysql://host:3306/dbname")
                .help("The password goes in the field below, not in this URL.")
                .required(),
            username: Some(user("root")),
            credential: Some(token("Password", "Stored in the OS keychain, never in the config file.")),
            fields: database_fields(),
            default_poll_secs: 15,
            capabilities: database_capabilities(),
        },
        ServiceDescriptor {
            kind: ServiceKind::HttpEndpoint,
            label: "HTTP endpoint".to_string(),
            monogram: "EP".to_string(),
            accent: 5,
            category: "Endpoints".to_string(),
            blurb: "Uptime, status code, and response time for any URL — a health endpoint, an \
                    API, or a website."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "URL", FieldKind::Url)
                .placeholder("https://api.internal/health")
                .required(),
            username: Some(user("optional")),
            credential: Some(token(
                "Password or bearer token",
                "Sent as basic auth when a username is set, otherwise as a bearer token.",
            )),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("expectStatus", "Expected status", FieldKind::Number)
                        .default(serde_json::json!(200))
                        .range(100.0, 599.0)
                        .help("Any other status marks the endpoint degraded."),
                    FieldSpec::new("expectBody", "Body must contain", FieldKind::Text)
                        .placeholder("optional substring, e.g. \"ok\"")
                        .help("Catches the case where a service returns 200 but is not actually well."),
                    FieldSpec::new("slowMillis", "Slow threshold (ms)", FieldKind::Number)
                        .default(serde_json::json!(1000))
                        .range(1.0, 120_000.0),
                    FieldSpec::new("method", "Method", FieldKind::Select)
                        .default(serde_json::json!("GET"))
                        .advanced(),
                ];
                fields[3].options = ["GET", "HEAD", "POST"]
                    .iter()
                    .map(|m| SelectOption {
                        value: (*m).to_string(),
                        label: (*m).to_string(),
                    })
                    .collect();
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 30,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Kubernetes,
            label: "Kubernetes".to_string(),
            monogram: "K8".to_string(),
            // Seventh kind, and only six status-safe hues exist — see the
            // `accent` field docs. The index is still sequential so that
            // widening the palette later gives Kubernetes its own colour with
            // no change here.
            accent: 6,
            category: "Orchestration".to_string(),
            blurb: "Node readiness, workload rollout state, pods that are crash-looping or \
                    stuck pending, and cluster warning events."
                .to_string(),
            // Unlike every other kind, the address is not typed in — it comes
            // from kubeconfig, which already holds the CA, the auth plugin, and
            // the cluster URL. Asking for an API server URL and a token here
            // would mean re-entering credentials the machine already has, and
            // would not work at all for exec-plugin auth (EKS, GKE).
            endpoint: FieldSpec::new("endpoint", "Context", FieldKind::Text)
                .placeholder("leave empty for the current context")
                .help("A context name from your kubeconfig, e.g. docker-desktop."),
            username: None,
            credential: None,
            fields: {
                let mut fields = vec![
                    FieldSpec::new("namespace", "Namespace", FieldKind::Text)
                        .placeholder("all namespaces")
                        .help("Limit workloads, pods, and events to one namespace."),
                    FieldSpec::new("kubeconfig", "Kubeconfig path", FieldKind::Text)
                        .placeholder("~/.kube/config")
                        .help("Only needed when the config is not in the default location.")
                        .advanced(),
                    FieldSpec::new("restartThreshold", "Restart alert threshold", FieldKind::Number)
                        .default(serde_json::json!(5))
                        .range(1.0, 1000.0)
                        .help("Container restarts above this mark a pod degraded.")
                        .advanced(),
                ];
                // No TLS toggle: kubeconfig carries the cluster CA already.
                fields.extend(shared_fields(false));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Redis,
            label: "Redis".to_string(),
            monogram: "RD".to_string(),
            accent: 7,
            category: "Caching".to_string(),
            blurb: "Memory pressure and eviction, connected clients, hit rate, replication \
                    link state, and the slow command log."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Redis URL", FieldKind::Url)
                .placeholder("redis://cache.internal:6379")
                .required(),
            username: Some(
                FieldSpec::new("__username", "Username", FieldKind::Text)
                    .placeholder("optional")
                    .help("Only for Redis 6+ ACL users. Leave empty for a password-only setup."),
            ),
            credential: Some(token("Password", "The AUTH password, if one is set.")),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("memoryWarnPercent", "Memory warning (%)", FieldKind::Number)
                        .default(serde_json::json!(85))
                        .range(1.0, 100.0)
                        .help("Share of maxmemory that marks the instance degraded. Ignored when maxmemory is 0."),
                    FieldSpec::new("slowlogCount", "Slow entries to read", FieldKind::Number)
                        .default(serde_json::json!(20))
                        .range(0.0, 128.0)
                        .advanced(),
                ];
                fields.extend(shared_fields(false));
                fields
            },
            default_poll_secs: 15,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Mongodb,
            label: "MongoDB".to_string(),
            monogram: "MG".to_string(),
            accent: 8,
            category: "Databases".to_string(),
            blurb: "Connection saturation, replica set health and lag, long-running \
                    operations, and database sizes."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Connection string", FieldKind::Url)
                .placeholder("mongodb://mongo.internal:27017")
                .required(),
            username: Some(user("optional")),
            credential: Some(token("Password", "Leave empty when auth is disabled.")),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("slowOpMillis", "Long operation threshold (ms)", FieldKind::Number)
                        .default(serde_json::json!(1000))
                        .range(1.0, 600_000.0)
                        .help("Operations running longer than this are listed."),
                    FieldSpec::new("authSource", "Auth database", FieldKind::Text)
                        .placeholder("admin")
                        .advanced(),
                ];
                fields.extend(shared_fields(false));
                fields
            },
            default_poll_secs: 15,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Prometheus,
            label: "Prometheus".to_string(),
            monogram: "PM".to_string(),
            accent: 9,
            category: "Observability".to_string(),
            blurb: "Scrape target health, firing alert rules, and time-series database \
                    size — the monitor watching your monitor."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Server URL", FieldKind::Url)
                .placeholder("http://prometheus.internal:9090")
                .required(),
            username: Some(user("optional")),
            credential: Some(token(
                "Password or bearer token",
                "Sent as basic auth when a username is set, otherwise as a bearer token.",
            )),
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "downTargetsCritical",
                    "Down targets before critical",
                    FieldKind::Number,
                )
                .default(serde_json::json!(1))
                .range(1.0, 1000.0)
                .help("How many scrape targets must be down to mark this connection down.")
                .advanced()];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 30,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Rabbitmq,
            label: "RabbitMQ".to_string(),
            monogram: "RB".to_string(),
            accent: 10,
            category: "Messaging".to_string(),
            blurb: "Queue depth and growth, consumer counts, unacknowledged messages, and \
                    node memory and disk alarms, via the management API."
                .to_string(),
            // Port 15672, not 5672 — the management plugin, not AMQP.
            endpoint: FieldSpec::new("endpoint", "Management URL", FieldKind::Url)
                .placeholder("http://rabbit.internal:15672")
                .required()
                .help("The management plugin's HTTP port, not the AMQP port."),
            username: Some(user("guest")),
            credential: Some(token("Password", "The management user's password.")),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("queueDepthWarn", "Queue depth warning", FieldKind::Number)
                        .default(serde_json::json!(1000))
                        .range(1.0, 10_000_000.0)
                        .help("Ready messages above this mark a queue degraded."),
                    FieldSpec::new("vhost", "Virtual host", FieldKind::Text)
                        .placeholder("all vhosts")
                        .advanced(),
                ];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Nginx,
            label: "Nginx".to_string(),
            monogram: "NG".to_string(),
            accent: 11,
            category: "Endpoints".to_string(),
            blurb: "Active connections, request throughput, and the reading/writing/waiting \
                    breakdown from the stub_status module."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Status URL", FieldKind::Url)
                .placeholder("http://nginx.internal/nginx_status")
                .required()
                .help("The location where stub_status is exposed. It is off by default."),
            username: Some(user("optional")),
            credential: Some(token("Password", "If the status location is password protected.")),
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "connectionWarn",
                    "Active connection warning",
                    FieldKind::Number,
                )
                .default(serde_json::json!(1000))
                .range(1.0, 1_000_000.0)
                .help("Active connections above this mark the server degraded.")];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Apache,
            label: "Apache httpd".to_string(),
            monogram: "AP".to_string(),
            accent: 13,
            category: "Endpoints".to_string(),
            blurb: "Worker saturation from the mod_status scoreboard, request and byte rates, \
                    and the per-state breakdown that separates a busy server from a stalled one."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Status URL", FieldKind::Url)
                .placeholder("http://apache.internal/server-status?auto")
                .required()
                .help(
                    "mod_status with the `?auto` query, which is what makes it machine \
                     readable. Without it the page is HTML and root cannot read it.",
                ),
            username: Some(user("optional")),
            credential: Some(token("Password", "If the status location is password protected.")),
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "saturationWarn",
                    "Worker saturation warning",
                    FieldKind::Number,
                )
                .default(serde_json::json!(85))
                .range(1.0, 100.0)
                .help(
                    "Percent of worker slots in use above which the server is degraded. \
                     Counted against every slot httpd will open, not just the ones it has.",
                )];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Haproxy,
            label: "HAProxy".to_string(),
            monogram: "HA".to_string(),
            accent: 14,
            category: "Endpoints".to_string(),
            blurb: "Frontends, backends and the servers behind them, with the queue depth that \
                    moves before the errors do."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Stats URL", FieldKind::Url)
                .placeholder("http://haproxy.internal:8404/stats")
                .required()
                .help(
                    "The stats page. root appends `;csv` for you — that is what turns the \
                     HTML page into something it can read.",
                ),
            username: Some(user("optional")),
            credential: Some(token("Password", "If the stats page requires authentication.")),
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "queueWarn",
                    "Queued request warning",
                    FieldKind::Number,
                )
                .default(serde_json::json!(1))
                .range(1.0, 100_000.0)
                .help(
                    "Requests queued on one backend, waiting for a free server, above which \
                     the proxy is degraded. Anything queueing at all is usually worth seeing.",
                )];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 15,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Memcached,
            label: "Memcached".to_string(),
            monogram: "MC".to_string(),
            accent: 15,
            category: "Caching".to_string(),
            blurb: "Hit rate, memory fill, and the eviction rate that tells you a cache is too \
                    small even while its hit rate looks fine."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Host and port", FieldKind::Text)
                .placeholder("cache.internal:11211")
                .required()
                .help("Port 11211 is assumed if you leave it off."),
            username: None,
            credential: None,
            fields: {
                let mut fields = vec![
                    FieldSpec::new("evictionWarn", "Eviction rate warning", FieldKind::Number)
                        .default(serde_json::json!(1))
                        .range(0.0, 100_000.0)
                        .help(
                            "Evictions per second above which the cache is degraded. Evicting \
                             at all means it is discarding valid items to make room.",
                        ),
                    FieldSpec::new("fillWarn", "Memory warning", FieldKind::Number)
                        .default(serde_json::json!(90))
                        .range(1.0, 100.0)
                        .help("Percent of the configured memory limit in use."),
                ];
                fields.extend(shared_fields(false));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Traefik,
            label: "Traefik".to_string(),
            monogram: "TR".to_string(),
            accent: 16,
            category: "Endpoints".to_string(),
            blurb: "Routers, services and the servers behind them — including routes that fail \
                    to build, which the proxy answers for and an uptime check never sees."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "API base URL", FieldKind::Url)
                .placeholder("http://traefik.internal:8080")
                .required()
                .help(
                    "The dashboard or API base. Traefik needs `api.dashboard` or \
                     `api.insecure` enabled for this to answer.",
                ),
            username: Some(user("optional")),
            credential: Some(token("Password", "If the API is behind basic auth.")),
            fields: shared_fields(true),
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Tomcat,
            label: "Apache Tomcat".to_string(),
            monogram: "TC".to_string(),
            accent: 17,
            category: "Endpoints".to_string(),
            blurb: "Thread pool saturation per connector, request and error rates, read from \
                    the manager's JMX proxy."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Base URL", FieldKind::Url)
                .placeholder("http://tomcat.internal:8080")
                .required()
                .help("Tomcat's root. root appends `/manager/jmxproxy` itself."),
            username: Some(user("required")),
            credential: Some(token(
                "Password",
                "The user needs the `manager-jmx` role, which is separate from `manager-gui`.",
            )),
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "threadWarn",
                    "Thread saturation warning",
                    FieldKind::Number,
                )
                .default(serde_json::json!(85))
                .range(1.0, 100.0)
                .help(
                    "Percent of one connector's thread limit in use above which Tomcat is \
                     degraded. Measured per connector — a busy HTTP pool is not excused by an \
                     idle HTTPS one.",
                )];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Zookeeper,
            label: "ZooKeeper".to_string(),
            monogram: "ZK".to_string(),
            accent: 18,
            category: "Coordination".to_string(),
            blurb: "Ensemble role, request backlog, and the synced-follower count that says \
                    whether quorum has any margin left."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Host and port", FieldKind::Text)
                .placeholder("zk-1.internal:2181")
                .required()
                .help(
                    "One server, not the whole ensemble — `mntr` answers for the server you \
                     ask, and the leader is the one that reports quorum.",
                ),
            username: None,
            credential: None,
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "outstandingWarn",
                    "Queued request warning",
                    FieldKind::Number,
                )
                .default(serde_json::json!(10))
                .range(1.0, 100_000.0)
                .help("Requests queued waiting to be serviced above which this is degraded.")];
                fields.extend(shared_fields(false));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Consul,
            label: "Consul".to_string(),
            monogram: "CS".to_string(),
            accent: 19,
            category: "Coordination".to_string(),
            blurb: "Raft leadership and service health, with a dead agent counted once rather \
                    than once per service registered on it."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "HTTP API address", FieldKind::Url)
                .placeholder("http://consul.internal:8500")
                .required()
                .help("Any agent's HTTP API. It will answer for the whole cluster."),
            username: None,
            credential: Some(token(
                "ACL token",
                "Needs `node:read` and `service:read`. Omit it if ACLs are not enabled.",
            )),
            fields: shared_fields(true),
            default_poll_secs: 15,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Etcd,
            label: "etcd".to_string(),
            monogram: "ET".to_string(),
            accent: 20,
            category: "Coordination".to_string(),
            blurb: "Raft health and — the one that matters — database size against the backend \
                    quota, which turns etcd read-only when it is reached."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Client URL", FieldKind::Url)
                .placeholder("http://etcd-1.internal:2379")
                .required()
                .help("A client port. root reads `/metrics` from it."),
            username: Some(user("optional")),
            credential: Some(token("Password", "If the metrics endpoint requires auth.")),
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "quotaWarn",
                    "Backend quota warning",
                    FieldKind::Number,
                )
                .default(serde_json::json!(80))
                .range(1.0, 100.0)
                .help(
                    "Percent of the backend quota used above which etcd is degraded. At 100% \
                     it stops accepting writes entirely until it is compacted and defragmented.",
                )];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 15,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Nats,
            label: "NATS".to_string(),
            monogram: "NA".to_string(),
            accent: 21,
            category: "Messaging".to_string(),
            blurb: "Connections, subscriptions and cluster routes — and the slow-consumer \
                    counter, which is the only trace NATS leaves when it drops messages."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Monitoring URL", FieldKind::Url)
                .placeholder("http://nats.internal:8222")
                .required()
                .help(
                    "The HTTP monitoring port, not the client port — NATS needs `http_port` \
                     set for this to answer.",
                ),
            username: Some(user("optional")),
            credential: Some(token("Password", "If the monitoring port is behind basic auth.")),
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "slowConsumerWarn",
                    "Slow consumer rate warning",
                    FieldKind::Number,
                )
                .default(serde_json::json!(0.1))
                .range(0.0, 10_000.0)
                .help(
                    "Slow-consumer events per second above which this is degraded. Each one is \
                     a client that was disconnected and had its pending messages dropped.",
                )];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 15,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Clickhouse,
            label: "ClickHouse".to_string(),
            monogram: "CH".to_string(),
            accent: 22,
            category: "Databases".to_string(),
            blurb: "Running queries, merges, and replicated tables — including the read-only \
                    ones, which keep answering queries while replication has stopped."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "HTTP interface URL", FieldKind::Url)
                .placeholder("http://clickhouse.internal:8123")
                .required()
                .help("The HTTP port (8123), not the native protocol port (9000)."),
            username: Some(user("required")),
            credential: Some(token(
                "Password",
                "The user needs SELECT on system.metrics, and on system.replicas to see \
                 replication.",
            )),
            fields: {
                let mut fields = vec![FieldSpec::new(
                    "delayWarn",
                    "Replication delay warning",
                    FieldKind::Number,
                )
                .default(serde_json::json!(60))
                .range(1.0, 86_400.0)
                .help("Seconds a replica may be behind before it is degraded.")];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 20,
            capabilities: Capabilities {
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Aws,
            label: "AWS".to_string(),
            monogram: "AW".to_string(),
            // Accents are assigned sequentially with no gaps; this is the next
            // free slot, and a test enforces it.
            accent: 26,
            category: "Cloud".to_string(),
            blurb: "S3 bucket growth and lifecycle policy, SQS depth and dead-letter \
                    configuration. One connection is an account and region; buckets and \
                    queues are components inside it."
                .to_string(),
            // Empty for real AWS, which resolves endpoints itself. A value
            // here means an emulator or a private gateway, and also switches
            // S3 to path-style addressing — virtual-hosted style would try to
            // resolve `bucket.localhost` and fail.
            endpoint: FieldSpec::new("endpoint", "Endpoint URL", FieldKind::Text)
                .placeholder("leave empty for real AWS")
                .help(
                    "Only for an emulator or a private gateway — http://localhost:4566 for \
                     LocalStack or Floci. Empty uses AWS's own endpoints.",
                ),
            username: Some(
                FieldSpec::new("__username", "Access key id", FieldKind::Text)
                    .placeholder("leave empty to use the default credential chain")
                    .help(
                        "Empty is usually right: on EC2 or in a pod, the instance or service \
                         account role should provide credentials rather than root holding a \
                         key.",
                    ),
            ),
            credential: Some(token(
                "Secret access key",
                "Stored in the keychain. Leave empty to use the environment, a shared \
                 credentials file, or an instance role.",
            )),
            fields: vec![
                FieldSpec::new("region", "Region", FieldKind::Text)
                    .default(serde_json::json!("us-east-1"))
                    .help("One connection covers one region."),
                FieldSpec::new("queueDepthWarn", "Queue backlog warning", FieldKind::Number)
                    .default(serde_json::json!(1000))
                    .range(1.0, 10_000_000.0)
                    .help("Total messages waiting across all queues before this is degraded."),
                FieldSpec::new("queueStalledWarn", "Unattended warning", FieldKind::Number)
                    .default(serde_json::json!(1))
                    .range(1.0, 1_000_000.0)
                    .help(
                        "Messages waiting with nothing in flight means no consumer is reading. \
                         Worse than a deep queue that is moving, so the threshold is lower.",
                    ),
                FieldSpec::new("bucketBytesWarn", "Storage warning", FieldKind::Number)
                    .help(
                        "Total stored bytes worth warning about. Leave empty for none — a \
                         number with no limit still gets a growth rate, just no deadline.",
                    ),
            ],
            default_poll_secs: 60,
            capabilities: Capabilities {
                // CloudTrail is a different service with a different design.
                logs: false,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Kafka,
            label: "Kafka".to_string(),
            monogram: "KF".to_string(),
            accent: 12,
            category: "Messaging".to_string(),
            blurb: "Consumer group lag, offline and under-replicated partitions, topic \
                    sizes, and broker membership."
                .to_string(),
            // Not a URL: bootstrap.servers is a comma-separated host:port list,
            // and pasting a URL here is the most common way to get an opaque
            // librdkafka error.
            endpoint: FieldSpec::new("endpoint", "Bootstrap servers", FieldKind::Text)
                .placeholder("broker1:9092,broker2:9092")
                .required()
                .help("Comma-separated host:port pairs, with no scheme."),
            username: Some(
                FieldSpec::new("__username", "SASL username", FieldKind::Text)
                    .placeholder("only for SASL")
                    .help("Leave empty unless the security protocol below is a SASL one."),
            ),
            credential: Some(token("SASL password", "Used with the SASL mechanism below.")),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("lagWarn", "Consumer lag warning", FieldKind::Number)
                        .default(serde_json::json!(10_000))
                        .range(1.0, 1_000_000_000.0)
                        .help("Total lag above this marks the cluster degraded."),
                    FieldSpec::new("securityProtocol", "Security protocol", FieldKind::Select)
                        .default(serde_json::json!("PLAINTEXT")),
                    FieldSpec::new("saslMechanism", "SASL mechanism", FieldKind::Select)
                        .default(serde_json::json!("PLAIN"))
                        .help("Only used when the protocol above is SASL_PLAINTEXT or SASL_SSL.")
                        .advanced(),
                ];
                fields[1].options = ["PLAINTEXT", "SSL", "SASL_PLAINTEXT", "SASL_SSL"]
                    .iter()
                    .map(|value| SelectOption {
                        value: (*value).to_string(),
                        label: (*value).to_string(),
                    })
                    .collect();
                // GSSAPI is absent on purpose: it needs cyrus-sasl on the host,
                // which cannot be statically bundled the way OpenSSL is.
                fields[2].options = ["PLAIN", "SCRAM-SHA-256", "SCRAM-SHA-512"]
                    .iter()
                    .map(|value| SelectOption {
                        value: (*value).to_string(),
                        label: (*value).to_string(),
                    })
                    .collect();
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 30,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
        ServiceDescriptor {
            kind: ServiceKind::Gigapipe,
            label: "Gigapipe".to_string(),
            monogram: "GP".to_string(),
            accent: 27,
            category: "Observability".to_string(),
            blurb: "Ingest rates per pipeline, query APIs that answer or do not, and — \
                    with a LogQL selector — the applications shipping into it. Also \
                    serves as a long-range history source."
                .to_string(),
            endpoint: FieldSpec::new("endpoint", "Server URL", FieldKind::Url)
                .placeholder("http://gigapipe.internal:3100")
                .required(),
            username: Some(user("optional")),
            credential: Some(token(
                "Password or bearer token",
                "Sent as basic auth when a username is set, otherwise as a bearer token.",
            )),
            fields: {
                let mut fields = vec![
                    FieldSpec::new("logQuery", "LogQL selector", FieldKind::Text)
                        .placeholder("{service_name=\"checkout-api\"}")
                        .help(
                            "Lines matching this join root's shared log stream — so anything \
                             shipped to gigapipe over OpenTelemetry, Datadog or Elastic shows \
                             up here without root needing a receiver of its own. Leave empty \
                             to monitor gigapipe and nothing else.",
                        ),
                    FieldSpec::new("traceQuery", "TraceQL selector", FieldKind::Text)
                        .placeholder("{ duration > 2s }")
                        .help(
                            "Traces matching this are listed, and each one opens as a \
                             waterfall. Leave empty to skip trace search entirely — a \
                             warehouse holding no traces should not grow an empty table.",
                        ),
                    FieldSpec::new("traceSlowMs", "Slow trace threshold (ms)", FieldKind::Number)
                        .default(serde_json::json!(0))
                        .range(0.0, 600_000.0)
                        .help(
                            "Traces longer than this are marked degraded and alerted on. \
                             Zero means the listing carries no verdict, which is right when \
                             the TraceQL already selects what you care about.",
                        )
                        .advanced(),
                    FieldSpec::new("traceLimit", "Traces per search", FieldKind::Number)
                        .default(serde_json::json!(20))
                        .range(1.0, 200.0)
                        .advanced(),
                    FieldSpec::new(
                        "traceLookbackMinutes",
                        "Trace search window (minutes)",
                        FieldKind::Number,
                    )
                    .default(serde_json::json!(15))
                    .range(1.0, 1440.0)
                    .advanced(),
                    FieldSpec::new("profileQuery", "Pyroscope selector", FieldKind::Text)
                        .placeholder(
                            "process_cpu:cpu:nanoseconds:cpu:nanoseconds{service_name=\"checkout\"}",
                        )
                        .help(
                            "The profile this connection opens its flame graph on: a profile \
                             type, then a label set. Leave empty to leave profiling off — a \
                             store holding no profiles should not grow a screen for them.",
                        ),
                    FieldSpec::new(
                        "idleIngestMinutes",
                        "Degrade after idle minutes",
                        FieldKind::Number,
                    )
                    .default(serde_json::json!(0))
                    .range(0.0, 1440.0)
                    .help(
                        "Mark this connection degraded when nothing has been ingested for \
                         this long. Zero disables it — a warehouse waiting for its first \
                         write is idle, not broken.",
                    )
                    .advanced(),
                    FieldSpec::new(
                        "signalsDegrade",
                        "Degrade when a query API stops answering",
                        FieldKind::Boolean,
                    )
                    .default(serde_json::json!(true))
                    .help(
                        "A store whose process is up while its backend is gone answers \
                         /ready and fails every query. Leave this on unless you deliberately \
                         run a write-only instance.",
                    )
                    .advanced(),
                ];
                fields.extend(shared_fields(true));
                fields
            },
            default_poll_secs: 30,
            capabilities: Capabilities {
                logs: true,
                actions: Vec::new(),
                diagnostics: Vec::new(),
            },
        },
    ]
}

fn database_fields() -> Vec<FieldSpec> {
    let mut fields = vec![
        FieldSpec::new("database", "Database", FieldKind::Text)
            .placeholder("only if not already in the URL"),
        FieldSpec::new("slowQuerySecs", "Slow query threshold (s)", FieldKind::Number)
            .default(serde_json::json!(5))
            .range(1.0, 3600.0)
            .help("Queries running longer than this are reported and alerted on."),
    ];
    fields.extend(shared_fields(true));
    fields
}

fn database_capabilities() -> Capabilities {
    Capabilities {
        logs: true,
        actions: vec![ActionSpec {
            tool: "terminate_database_query".into(),
            label: "Terminate a running query".into(),
        }],
        diagnostics: vec![
            "connections".into(),
            "long_queries".into(),
            "table_sizes".into(),
            "locks".into(),
            "replication".into(),
        ],
    }
}

pub fn descriptor(kind: ServiceKind) -> Option<ServiceDescriptor> {
    catalog().into_iter().find(|d| d.kind == kind)
}

/// The kind a descriptor label names, if any.
///
/// The inverse of `ServiceKind::label`, resolved through the registry rather
/// than a second hand-written match — a mapping written twice is a mapping
/// that will disagree with itself.
pub fn kind_from_label(label: &str) -> Option<crate::model::ServiceKind> {
    catalog()
        .into_iter()
        .find(|d| d.label.eq_ignore_ascii_case(label.trim()))
        .map(|d| d.kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_service_kind_has_a_descriptor() {
        // The frontend renders purely from the catalog, so a kind missing here
        // would be invisible in the UI while still being pollable — exactly the
        // drift this registry exists to prevent.
        for kind in ServiceKind::ALL {
            assert!(
                descriptor(*kind).is_some(),
                "{kind:?} has no descriptor; the UI could not render it"
            );
        }
    }

    #[test]
    fn descriptors_are_complete_enough_to_render() {
        for descriptor in catalog() {
            assert!(!descriptor.label.is_empty(), "{:?}", descriptor.kind);
            assert!(!descriptor.blurb.is_empty(), "{:?}", descriptor.kind);
            assert!(!descriptor.category.is_empty(), "{:?}", descriptor.kind);
            assert!(
                (1..=2).contains(&descriptor.monogram.chars().count()),
                "{:?} monogram must be 1-2 chars",
                descriptor.kind
            );
            // Accent range is not bounded here. It used to be `< 6`, matching
            // the number of status-safe hues, which silently made a seventh
            // service kind impossible to add. `accentVar` wraps with a modulo,
            // so any index renders; that the seventh kind reuses a hue is the
            // documented trade-off. Numbering is checked by
            // `accents_are_assigned_sequentially_without_gaps`.
            assert!(descriptor.default_poll_secs >= 2);
        }
    }

    #[test]
    fn option_keys_are_unique_within_a_service() {
        for descriptor in catalog() {
            let mut seen = HashSet::new();
            for field in &descriptor.fields {
                assert!(
                    seen.insert(field.key.clone()),
                    "{:?} declares `{}` twice",
                    descriptor.kind,
                    field.key
                );
            }
        }
    }

    #[test]
    fn select_fields_always_offer_options() {
        for descriptor in catalog() {
            for field in &descriptor.fields {
                if field.kind == FieldKind::Select {
                    assert!(
                        !field.options.is_empty(),
                        "{:?}.{} is a select with no options",
                        descriptor.kind,
                        field.key
                    );
                }
            }
        }
    }

    /// Accents must be `0..n-1`: no repeats, no gaps.
    ///
    /// This used to assert only that they were distinct. That is the same thing
    /// while there are fewer kinds than palette slots, but it says nothing about
    /// *which* indices were used, so a catalog could drift to `0, 3, 9` and
    /// still pass while wrapping two kinds onto one hue for no reason.
    ///
    /// Only six of the eight categorical hues are safe for identity — the other
    /// two sit too close to the warning and critical colours — so from the
    /// seventh kind onward two services do share a rendered colour. That is a
    /// deliberate, documented wrap in `accentVar`, and the monogram, icon, and
    /// label all still separate them.
    #[test]
    fn accents_are_assigned_sequentially_without_gaps() {
        let mut accents: Vec<usize> = catalog().iter().map(|d| d.accent).collect();
        accents.sort_unstable();
        let expected: Vec<usize> = (0..catalog().len()).collect();
        assert_eq!(
            accents, expected,
            "accents must be 0..n-1 — no duplicates, no gaps"
        );
    }
}
