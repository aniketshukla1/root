//! The standalone gigapipe server — what `root-mcp` spawns.
//!
//! Serves the nine gigapipe store reads over stdio, with no running copy of
//! root and no port. See [`super`] for the protocol and for why this serves a
//! narrower surface than the in-app server does.
//!
//! ## Where connections come from
//!
//! `gigapipe-mcp` is configured with `GIGAPIPE_HOST`, `GIGAPIPE_USERNAME` and
//! `GIGAPIPE_PASSWORD`. Those work here unchanged, so an existing client
//! configuration keeps working with the binary swapped.
//!
//! With no `GIGAPIPE_HOST` set, this reads root's own configuration instead and
//! serves the gigapipe connections already set up in the app, with their
//! secrets from the OS keychain. A `connection` argument picks between them
//! when there is more than one; with exactly one, it is the default and can be
//! left off.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{advertise, ToolBackend};
use crate::agent::tools::{gigapipe_read_from_args, GIGAPIPE_PREFIX};
use crate::collectors::{self, Collector};
use crate::config::{AgentMode, AppConfig, ConnectionProfile};
use crate::error::{AppError, Result};
use crate::model::ServiceKind;

/// Where this server gets a gigapipe to talk to.
pub enum Source {
    /// `GIGAPIPE_HOST` and friends — the way `gigapipe-mcp` is configured.
    ///
    /// Boxed because a profile is much larger than the vector beside it, and
    /// an enum is as big as its widest variant.
    Environment(Box<ConnectionProfile>),
    /// root's own configuration, which may hold several gigapipe connections.
    Configured(Vec<ConnectionProfile>),
}

pub struct Server {
    source: Source,
}

impl Server {
    /// Build from the environment if it names a host, and from root's
    /// configuration otherwise.
    ///
    /// An empty result is not an error here. A client that spawned this before
    /// any connection was configured should get a working server that says so
    /// when a tool is called, rather than a process that exits during startup
    /// and shows up as "server crashed".
    pub fn from_environment_or_config(config_dir: Option<PathBuf>) -> Self {
        if let Some(profile) = profile_from_env() {
            return Server {
                source: Source::Environment(Box::new(profile)),
            };
        }

        let profiles = config_dir
            .map(|dir| crate::config::config_path(&dir))
            .filter(|path| path.exists())
            .and_then(|path| AppConfig::load(&path).ok())
            .map(|config| {
                config
                    .profiles
                    .into_iter()
                    .filter(|p| p.kind == ServiceKind::Gigapipe)
                    .collect()
            })
            .unwrap_or_default();

        Server {
            source: Source::Configured(profiles),
        }
    }

    /// Pick the profile a call addresses.
    fn profile(&self, wanted: Option<&str>) -> Result<&ConnectionProfile> {
        match &self.source {
            Source::Environment(profile) => Ok(profile),
            Source::Configured(profiles) => match (wanted, profiles.len()) {
                (_, 0) => Err(AppError::Config(
                    "no gigapipe connection is configured in root, and GIGAPIPE_HOST is not set"
                        .to_string(),
                )),
                (None, 1) => Ok(&profiles[0]),
                (None, _) => Err(AppError::Config(format!(
                    "several gigapipe connections are configured; name one with `connection`: {}",
                    profiles
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))),
                (Some(wanted), _) => profiles
                    .iter()
                    .find(|p| {
                        p.id == wanted || p.name.eq_ignore_ascii_case(wanted.trim())
                    })
                    .ok_or_else(|| {
                        AppError::Config(format!("no gigapipe connection called `{wanted}`"))
                    }),
            },
        }
    }

    async fn collector(&self, wanted: Option<&str>) -> Result<Arc<dyn Collector>> {
        let profile = self.profile(wanted)?;
        let secret = match &self.source {
            // The environment already carried it.
            Source::Environment(_) => std::env::var("GIGAPIPE_PASSWORD").ok(),
            Source::Configured(_) if profile.has_secret => {
                crate::secrets::get_secret(&profile.secret_key()).await?
            }
            Source::Configured(_) => None,
        };
        collectors::build_collector(profile, secret).await
    }

}

#[async_trait]
impl ToolBackend for Server {
    /// Root's gigapipe reads, with the `gigapipe_` prefix removed.
    ///
    /// The prefix exists inside root because it also monitors Prometheus
    /// directly and `prometheus_query` would be ambiguous there. Here nothing
    /// else is served, so the bare names are unambiguous — and they are the
    /// names `gigapipe-mcp` uses, which makes this a drop-in for it.
    ///
    /// Derived from root's own catalog rather than written out again, so the
    /// two surfaces cannot drift apart.
    fn tools(&self) -> Vec<Value> {
        let gigapipe = crate::agent::tools::catalog(AgentMode::ReadOnly)
            .into_iter()
            .filter(|t| t.spec.name.starts_with(GIGAPIPE_PREFIX));
        advertise(gigapipe, GIGAPIPE_PREFIX)
    }

    async fn call(&self, name: &str, args: &Value) -> Result<String> {
        let read = gigapipe_read_from_args(name, args)?;
        let wanted = args
            .get("connection")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let value = self
            .collector(wanted)
            .await?
            .detail(&collectors::DetailQuery::GigapipeRead(read))
            .await?;
        // The same cap the in-app path applies. A broad PromQL selector can
        // answer with tens of megabytes, and writing that to stdout as one
        // JSON-RPC line blows the client's context rather than informing it.
        Ok(crate::agent::tools::truncate_result(
            &serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        ))
    }

    fn instructions(&self) -> &'static str {
        "Reads metrics, logs and traces from a gigapipe observability warehouse. \
         Start with the label and tag listings to find out what the store actually \
         holds before writing a PromQL or LogQL selector against it."
    }
}

/// A gigapipe profile assembled from `gigapipe-mcp`'s own environment
/// variables, so an existing client configuration keeps working.
///
/// The host is normalised the way that server normalises it: a bare host:port
/// gets `http://`, because a URL without a scheme is not one.
fn profile_from_env() -> Option<ConnectionProfile> {
    let host = std::env::var("GIGAPIPE_HOST")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())?;
    let endpoint = if host.starts_with("http://") || host.starts_with("https://") {
        host
    } else {
        format!("http://{host}")
    };

    let username = std::env::var("GIGAPIPE_USERNAME")
        .ok()
        .filter(|u| !u.is_empty());
    let has_secret = std::env::var("GIGAPIPE_PASSWORD")
        .map(|p| !p.is_empty())
        .unwrap_or(false);

    Some(ConnectionProfile {
        id: "gigapipe-env".to_string(),
        name: "gigapipe".to_string(),
        kind: ServiceKind::Gigapipe,
        endpoint,
        username,
        has_secret,
        enabled: true,
        poll_interval_secs: 30,
        options: Default::default(),
        depends_on: Vec::new(),
        config_sources: Vec::new(),
        history_source: None,
        runtime: Default::default(),
        snapshot_sink: None,
        metadata: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::{handle, Request, DEFAULT_PROTOCOL, INVALID_PARAMS, METHOD_NOT_FOUND};
    use serde_json::json;

    fn server() -> Server {
        Server {
            source: Source::Configured(Vec::new()),
        }
    }

    fn request(method: &str, params: Value) -> Request {
        Request {
            id: Some(json!(1)),
            method: method.to_string(),
            params: Some(params),
        }
    }

    #[tokio::test]
    async fn initialize_reports_tool_capability_and_identifies_the_server() {
        let response = handle(&server(), request("initialize", json!({})))
            .await
            .expect("initialize is a request and must be answered");
        let result = response.result.unwrap();
        assert_eq!(result["protocolVersion"], DEFAULT_PROTOCOL);
        assert_eq!(result["serverInfo"]["name"], "root");
        assert!(result["capabilities"]["tools"].is_object());
    }

    /// A client's own version is echoed when we know it, so an older client
    /// is not told to speak something it cannot.
    #[tokio::test]
    async fn initialize_echoes_a_protocol_version_it_recognises() {
        let response = handle(
            &server(),
            request("initialize", json!({ "protocolVersion": "2025-06-18" })),
        )
        .await
        .unwrap();
        assert_eq!(response.result.unwrap()["protocolVersion"], "2025-06-18");
    }

    #[tokio::test]
    async fn an_unknown_protocol_version_falls_back_rather_than_failing() {
        let response = handle(
            &server(),
            request("initialize", json!({ "protocolVersion": "1999-01-01" })),
        )
        .await
        .unwrap();
        let result = response.result.expect("this must not be an error");
        assert_eq!(result["protocolVersion"], DEFAULT_PROTOCOL);
    }

    /// Answering a notification is a protocol violation, and the one every
    /// hand-written server gets wrong first.
    #[tokio::test]
    async fn notifications_are_never_answered() {
        let notification = Request {
            id: None,
            method: "notifications/initialized".to_string(),
            params: None,
        };
        assert!(handle(&server(), notification).await.is_none());
    }

    #[tokio::test]
    async fn tools_list_serves_the_nine_reads_without_roots_prefix() {
        let response = handle(&server(), request("tools/list", json!({})))
            .await
            .unwrap();
        let tools = response.result.unwrap()["tools"].clone();
        let names: Vec<String> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();

        assert_eq!(names.len(), 9, "got {names:?}");
        for expected in [
            "prometheus_query",
            "prometheus_labels",
            "prometheus_label_values",
            "loki_query",
            "loki_labels",
            "loki_label_values",
            "tempo_query",
            "tempo_tags",
            "tempo_tag_values",
        ] {
            assert!(names.iter().any(|n| n == expected), "{expected} missing");
        }
        assert!(
            names.iter().all(|n| !n.starts_with(GIGAPIPE_PREFIX)),
            "the prefix is root's own and should not be served: {names:?}"
        );
    }

    /// Every advertised tool has to carry a schema a client can build a form
    /// from, or the tool is advertised and uncallable.
    #[tokio::test]
    async fn every_advertised_tool_has_an_object_schema() {
        let response = handle(&server(), request("tools/list", json!({})))
            .await
            .unwrap();
        for tool in response.result.unwrap()["tools"].as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool:?}");
            assert!(tool["inputSchema"]["properties"].is_object(), "{tool:?}");
            assert!(
                !tool["description"].as_str().unwrap_or_default().is_empty(),
                "{tool:?}"
            );
        }
    }

    /// Root's mutating tools stop at a confirmation dialog, and a pipe has no
    /// dialog. None of them may ever appear here.
    #[tokio::test]
    async fn no_mutating_tool_is_ever_served() {
        let response = handle(&server(), request("tools/list", json!({})))
            .await
            .unwrap();
        let names: Vec<String> = response.result.unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for forbidden in [
            "restart_container",
            "start_container",
            "stop_container",
            "trigger_jenkins_build",
            "terminate_database_query",
        ] {
            assert!(!names.iter().any(|n| n == forbidden), "{forbidden} served");
        }
    }

    #[tokio::test]
    async fn an_unknown_method_is_a_protocol_error() {
        let response = handle(&server(), request("tools/destroy", json!({})))
            .await
            .unwrap();
        assert_eq!(response.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn a_call_without_a_name_is_rejected_as_bad_params() {
        let response = handle(&server(), request("tools/call", json!({})))
            .await
            .unwrap();
        assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
    }

    /// A tool that cannot run comes back as a *result* carrying isError, so
    /// the model can read why and choose differently. A transport error would
    /// just look like the server broke.
    #[tokio::test]
    async fn a_failing_call_is_a_result_not_a_transport_error() {
        let response = handle(
            &server(),
            request(
                "tools/call",
                json!({ "name": "tempo_tags", "arguments": {} }),
            ),
        )
        .await
        .unwrap();

        assert!(response.error.is_none(), "must not be a protocol error");
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("no gigapipe connection"),
            "unhelpful message: {text}"
        );
    }

    #[tokio::test]
    async fn a_missing_required_argument_names_the_argument() {
        let response = handle(
            &server(),
            request(
                "tools/call",
                json!({ "name": "loki_query", "arguments": {} }),
            ),
        )
        .await
        .unwrap();
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("`query` is required"));
    }

    #[test]
    fn a_host_without_a_scheme_is_given_one() {
        // Matches how gigapipe-mcp normalises GIGAPIPE_HOST, so a config that
        // worked there works here.
        temp_env(&[("GIGAPIPE_HOST", Some("gigapipe.internal:3100"))], || {
            let profile = profile_from_env().expect("a host was set");
            assert_eq!(profile.endpoint, "http://gigapipe.internal:3100");
            assert_eq!(profile.kind, ServiceKind::Gigapipe);
        });
    }

    #[test]
    fn an_https_host_is_left_alone() {
        temp_env(&[("GIGAPIPE_HOST", Some("https://gp.internal:3100"))], || {
            assert_eq!(
                profile_from_env().unwrap().endpoint,
                "https://gp.internal:3100"
            );
        });
    }

    #[test]
    fn no_host_means_no_environment_profile() {
        temp_env(&[("GIGAPIPE_HOST", None)], || {
            assert!(profile_from_env().is_none());
        });
    }

    /// Set environment variables for the duration of one closure and restore
    /// them afterwards. Tests in a process share the environment, so these are
    /// serialised by a mutex rather than left to race.
    fn temp_env(vars: &[(&str, Option<&str>)], body: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let previous: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(key, _)| ((*key).to_string(), std::env::var(key).ok()))
            .collect();
        for (key, value) in vars {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }

        body();

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(&key, value),
                None => std::env::remove_var(&key),
            }
        }
    }
}
