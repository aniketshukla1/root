//! Persisted configuration: connection profiles and assistant settings.
//!
//! Credentials never live in this file. Each profile stores only a reference;
//! the actual secret sits in the OS keychain (see [`crate::secrets`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::model::ServiceKind;

/// Per-connection options, held as an open key/value map rather than a fixed
/// struct.
///
/// This is what makes a new service type a backend-only change. A collector
/// declares whatever options it needs in its [`ServiceDescriptor`], the settings
/// form renders them from that declaration, and the values land here — without
/// this struct, `config.rs`, or any frontend type having to learn about them.
///
/// Keys are camelCase to match the field specs and the UI.
///
/// [`ServiceDescriptor`]: crate::registry::ServiceDescriptor
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileOptions(pub serde_json::Map<String, serde_json::Value>);

impl ProfileOptions {
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }

    /// Empty strings read as absent — a cleared form field means "unset", not
    /// "the empty string", and every caller wants that.
    pub fn string(&self, key: &str) -> Option<String> {
        self.get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// Accepts a JSON number or a numeric string, since a form input may submit
    /// either depending on how it was bound.
    pub fn u64(&self, key: &str) -> Option<u64> {
        match self.get(key)? {
            serde_json::Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn bool(&self, key: &str) -> bool {
        match self.get(key) {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => s == "true",
            _ => false,
        }
    }

    pub fn set(&mut self, key: &str, value: serde_json::Value) {
        self.0.insert(key.to_string(), value);
    }

    // --- Options shared by every collector.

    pub fn insecure_tls(&self) -> bool {
        self.bool("insecureTls")
    }

    pub fn log_limit(&self) -> usize {
        self.u64("maxLogLines").unwrap_or(200).clamp(1, 2000) as usize
    }
}

/// One configured piece of infrastructure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub kind: ServiceKind,
    /// Docker socket path or TCP URL, Jenkins/ES base URL, or a DB connection
    /// URL. For Docker this may be empty to use the platform default socket.
    pub endpoint: String,
    #[serde(default)]
    pub username: Option<String>,
    /// True when a secret for this profile exists in the keychain. The secret
    /// itself is never serialized.
    #[serde(default)]
    pub has_secret: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub options: ProfileOptions,
    /// What this connection needs in order to work, as profile ids.
    ///
    /// DECLARED, never discovered: root watches connections, not the network
    /// between them, so it cannot infer this and does not try. An edge here is
    /// an assertion by whoever configured it — the same standing as a reported
    /// deploy — and may be stale.
    ///
    /// Only this direction is stored. Dependents are derived, because
    /// "what does this need?" is a question an operator can answer and "what
    /// needs this?" is one they usually cannot. Unknown ids are ignored rather
    /// than rejected, so deleting a profile cannot invalidate another's config.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Open, non-collector metadata for ownership, environment, runbooks, and
    /// future platform integrations. Keeping this separate from `options`
    /// means collectors never become coupled to the operating model.
    #[serde(default)]
    pub metadata: ProfileOptions,
    /// Where this connection's configuration lives on disk.
    ///
    /// DECLARED, like `depends_on` and for the same reason: a tool that goes
    /// looking for things that might be config will eventually open something
    /// it should not have. Live config read back from the target itself needs
    /// no entry here — the collector already knows how to ask.
    #[serde(default)]
    pub config_sources: Vec<crate::configsource::ConfigSource>,
    /// Where this connection's long-range history lives, when somewhere other
    /// than root's own ring buffer.
    ///
    /// root keeps hours; a forecast that matters needs weeks. Most estates
    /// already have a Prometheus holding them, and this says which one and how
    /// its series map onto this connection's metrics. Declared, because
    /// `memory` and `redis_memory_used_bytes` cannot be matched up by
    /// inference and a wrong guess forecasts the wrong number convincingly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_source: Option<crate::promquery::HistorySource>,
    /// Where this connection actually runs, as declared.
    ///
    /// Optional and empty by default. root infers what a container is from its
    /// image and its published port, which is reading rather than guessing but
    /// is still root talking to itself — nothing checks it. A declaration
    /// gives the inference something to be wrong against, and the
    /// disagreements are the output.
    #[serde(default)]
    pub runtime: crate::runtime::Runtime,
    /// Where this connection's snapshots are written, when anywhere.
    ///
    /// Absent by default and absent for almost every connection. root reads;
    /// this is the one place it writes to somebody else's store, so it is
    /// turned on one connection at a time and never inferred. See
    /// [`crate::influx`] for why that asymmetry is deliberate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_sink: Option<crate::influx::SnapshotSink>,
}

fn default_true() -> bool {
    true
}

fn default_poll_interval() -> u64 {
    10
}

impl ConnectionProfile {
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.poll_interval_secs.clamp(2, 3600))
    }

    /// Keychain entry name for this profile's password or API token.
    pub fn secret_key(&self) -> String {
        format!("profile:{}", self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    /// Anthropic Messages API.
    Anthropic,
    /// Any OpenAI-compatible `/chat/completions` endpoint — OpenAI, Groq,
    /// OpenRouter, Together, DeepSeek, vLLM, LM Studio, and so on.
    OpenAiCompatible,
    /// A local Ollama server.
    Ollama,
}

impl LlmProviderKind {
    pub fn default_base_url(self) -> &'static str {
        match self {
            LlmProviderKind::Anthropic => "https://api.anthropic.com",
            LlmProviderKind::OpenAiCompatible => "https://api.openai.com/v1",
            LlmProviderKind::Ollama => "http://localhost:11434",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            LlmProviderKind::Anthropic => "claude-sonnet-4-5",
            LlmProviderKind::OpenAiCompatible => "gpt-4o",
            LlmProviderKind::Ollama => "llama3.1",
        }
    }

    /// Ollama runs locally and needs no key; the others do.
    pub fn requires_api_key(self) -> bool {
        !matches!(self, LlmProviderKind::Ollama)
    }

    pub fn secret_key(self) -> &'static str {
        match self {
            LlmProviderKind::Anthropic => "llm:anthropic",
            LlmProviderKind::OpenAiCompatible => "llm:openai_compatible",
            LlmProviderKind::Ollama => "llm:ollama",
        }
    }
}

/// How much authority the assistant has over the infrastructure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AgentMode {
    /// Diagnose only. Remediation is surfaced as a command for the operator to
    /// run; the agent has no mutating tools at all.
    #[default]
    ReadOnly,
    /// Diagnose, plus mutating tools that each require explicit approval in the
    /// UI before they execute.
    ConfirmActions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmSettings {
    pub provider: LlmProviderKind,
    pub model: String,
    /// Overrides the provider default when talking to a compatible gateway.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default)]
    pub agent_mode: AgentMode,
    #[serde(default)]
    pub has_api_key: bool,
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_temperature() -> f32 {
    0.2
}

impl Default for LlmSettings {
    fn default() -> Self {
        LlmSettings {
            provider: LlmProviderKind::Anthropic,
            model: LlmProviderKind::Anthropic.default_model().to_string(),
            base_url: None,
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
            agent_mode: AgentMode::default(),
            has_api_key: false,
        }
    }
}

impl LlmSettings {
    pub fn resolved_base_url(&self) -> String {
        self.base_url
            .as_deref()
            .map(|s| s.trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.provider.default_base_url().to_string())
    }
}

/// How much history to keep in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retention {
    #[serde(default = "default_log_capacity")]
    pub log_capacity: usize,
}

fn default_log_capacity() -> usize {
    20_000
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            log_capacity: default_log_capacity(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub profiles: Vec<ConnectionProfile>,
    pub llm: LlmSettings,
    pub retention: Retention,
    #[serde(default)]
    pub ingest: IngestSettings,
    #[serde(default)]
    pub mcp: McpSettings,
    #[serde(default)]
    pub prediction: PredictionSettings,
    #[serde(default)]
    pub team: TeamSettings,
    #[serde(default)]
    pub storage: StorageSettings,
    /// The identity provider, when the server has one. Ignored by the desktop
    /// app, which has nobody to authenticate to.
    #[serde(default)]
    pub oidc: crate::oidc::OidcSettings,
}

/// Where the estate's history is kept, when it is kept anywhere.
///
/// Absent by default, and absent means exactly what it has always meant: the
/// ring buffers in `history.rs` and `state.rs` are the whole of root's memory
/// and quitting forgets. Naming a database adds a layer underneath them and
/// changes nothing about how the live screens read.
///
/// The URL is a Postgres connection string. TimescaleDB is used when the
/// extension is present and not required when it is not — see
/// [`crate::store`] for what is lost without it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageSettings {
    /// `postgres://user:pass@host:5432/root`. Empty means history is not kept.
    ///
    /// Stored here rather than in the keychain because it is a location, and
    /// because a connection string an operator cannot read back is one they
    /// cannot fix. A deployment that minds the password in it should pass
    /// `ROOT_DATABASE_URL` in the environment instead, which wins over this.
    #[serde(default)]
    pub database_url: String,
}

impl StorageSettings {
    /// The URL to use, environment first.
    ///
    /// The environment wins so a container can be handed its database without
    /// anyone editing a config file, which is how every deployment of this
    /// will actually be configured.
    pub fn resolved_url(&self) -> Option<String> {
        if let Ok(url) = std::env::var("ROOT_DATABASE_URL") {
            let url = url.trim().to_string();
            if !url.is_empty() {
                return Some(url);
            }
        }
        let url = self.database_url.trim();
        (!url.is_empty()).then(|| url.to_string())
    }
}

/// Where a shared workspace lives, if there is one.
///
/// root is local-first and stays that way: with no server configured it is a
/// single-operator tool reading a config file on this machine, which is the
/// only mode that needs no trust in anything but the OS keychain.
///
/// Pointing it at a root server is opt-in and additive. The server is one you
/// run — there is no hosted service and no account to create — and all it adds
/// is a shared identity so several people can look at the same estate and be
/// told apart. The session token is never written here; it lives in the
/// keychain beside the connection credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TeamSettings {
    /// `None` is local mode, and is the default.
    pub server_url: Option<String>,
    /// Who is signed in. Absent until a sign-in succeeds.
    pub account: Option<Account>,
}

/// What a signed-in person is, as far as this machine is concerned.
///
/// A cache of what the server said, not a claim this app can make on its own:
/// the role here decides what the UI offers, and the server decides what the
/// UI is allowed to do. A client that enforced its own permissions would be
/// enforcing nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub email: String,
    pub name: String,
    pub role: Role,
}

/// Ascending authority. `Viewer` is the floor because it is what an SSO domain
/// match grants, and granting anything more by default would make joining a
/// workspace an escalation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The default in every sense: the floor a domain match grants, and what
    /// an unreadable or absent role must fall back to rather than up to.
    #[default]
    Viewer,
    Responder,
    Admin,
    Owner,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "Viewer",
            Role::Responder => "Responder",
            Role::Admin => "Admin",
            Role::Owner => "Owner",
        }
    }

    /// What this role may do, in the app's own words. The server is the thing
    /// that enforces it; this is only what the UI should bother offering.
    pub fn summary(self) -> &'static str {
        match self {
            Role::Viewer => "Read everything. Change nothing, and the assistant stays read-only.",
            Role::Responder => "Acknowledge alerts and run collector actions. Cannot change connections.",
            Role::Admin => "Add, edit and remove connections, and manage members.",
            Role::Owner => "Everything an admin can do, plus transferring the workspace.",
        }
    }
}

/// The local deploy-ingest listener.
///
/// Disabled by default and bound to loopback only — enabling it is a deliberate
/// act, and even then nothing off the machine can reach it. The bearer token
/// lives in the OS keychain, never in this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ingest_port")]
    pub port: u16,
}

fn default_ingest_port() -> u16 {
    7717
}

/// The in-app MCP server.
///
/// Same posture as [`IngestSettings`] and for the same reasons: off by default,
/// loopback only, bearer token in the OS keychain. What it exposes is root's
/// read-only tool surface, so a client can read the estate and change nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_mcp_port")]
    pub port: u16,
}

fn default_mcp_port() -> u16 {
    7718
}

/// Forward-looking risk, read by the model.
///
/// Off by default and deliberately so. It sends a description of the estate —
/// connection names, health, metric trends, log templates — to whichever
/// provider the assistant is configured with, and that is a decision an
/// operator makes rather than inherits. With no assistant configured it cannot
/// run at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictionSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Minutes between sweeps. One model call each, so this is the cost dial.
    #[serde(default = "default_prediction_interval")]
    pub interval_minutes: u64,
}

fn default_prediction_interval() -> u64 {
    30
}

impl Default for PredictionSettings {
    fn default() -> Self {
        PredictionSettings {
            enabled: false,
            interval_minutes: default_prediction_interval(),
        }
    }
}

impl Default for McpSettings {
    fn default() -> Self {
        McpSettings {
            enabled: false,
            port: default_mcp_port(),
        }
    }
}

impl Default for IngestSettings {
    fn default() -> Self {
        IngestSettings {
            enabled: false,
            port: default_ingest_port(),
        }
    }
}

impl AppConfig {
    pub fn profile(&self, id: &str) -> Result<&ConnectionProfile> {
        self.profiles
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::UnknownProfile(id.to_string()))
    }

    /// Resolve a profile by id first, then by case-insensitive name. The
    /// assistant refers to connections by name, so this keeps its tool calls
    /// working without making it memorize UUIDs.
    pub fn profile_by_ref(&self, needle: &str) -> Result<&ConnectionProfile> {
        self.profiles
            .iter()
            .find(|p| p.id == needle)
            .or_else(|| {
                self.profiles
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(needle))
            })
            .ok_or_else(|| AppError::UnknownProfile(needle.to_string()))
    }

    pub fn load(path: &Path) -> Result<AppConfig> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| {
                AppError::Config(format!("{} is not valid config JSON: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppConfig::default()),
            Err(e) => Err(AppError::Io(e)),
        }
    }

    /// Write via a temp file + rename so a crash mid-write cannot truncate the
    /// user's connection list.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// `<app config dir>/config.json`
pub fn config_path(base: &Path) -> PathBuf {
    base.join("config.json")
}

/// Copy config from either previous product identifier on the first `root`
/// launch. Candidates are newest-first so an actively used root config
/// wins over the original `.app` location. Old files remain rollback paths.
pub fn migrate_legacy_config(current: &Path) -> Result<bool> {
    if current.exists() {
        return Ok(false);
    }

    let Some(app_support) = current.parent().and_then(Path::parent) else {
        return Ok(false);
    };
    for identifier in ["com.inframonitor.desktop", "com.inframonitor.app"] {
        let legacy = app_support.join(identifier).join("config.json");
        if !legacy.exists() {
            continue;
        }

        if let Some(parent) = current.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&legacy, current)?;
        return Ok(true);
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_without_metadata_remain_backward_compatible() {
        let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "name": "Legacy Docker",
            "kind": "docker",
            "endpoint": "",
            "options": {}
        }))
        .unwrap();

        assert!(profile.metadata.0.is_empty());
    }

    #[test]
    fn open_operational_metadata_round_trips() {
        let value = serde_json::json!({
            "id": "prod",
            "name": "Production",
            "kind": "docker",
            "endpoint": "",
            "options": {},
            "metadata": {
                "operations": {
                    "team": "Platform Runtime",
                    "criticality": "tier_0"
                },
                "cmdb": { "serviceId": "svc-42" }
            }
        });
        let profile: ConnectionProfile = serde_json::from_value(value).unwrap();
        let serialized = serde_json::to_value(profile).unwrap();

        assert_eq!(serialized["metadata"]["operations"]["team"], "Platform Runtime");
        assert_eq!(serialized["metadata"]["cmdb"]["serviceId"], "svc-42");
    }

    #[test]
    fn legacy_config_is_copied_without_removing_the_rollback_file() {
        let root =
            std::env::temp_dir().join(format!("root-migration-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("com.inframonitor.desktop").join("config.json");
        let current = root.join("com.root.observability").join("config.json");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, br#"{"profiles":[]}"#).unwrap();

        assert!(migrate_legacy_config(&current).unwrap());
        assert_eq!(std::fs::read(&current).unwrap(), br#"{"profiles":[]}"#);
        assert!(legacy.exists());

        std::fs::remove_dir_all(root).unwrap();
    }
}
