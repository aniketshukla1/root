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
