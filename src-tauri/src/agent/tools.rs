//! The tool surface exposed to the assistant.
//!
//! Tools split into two groups. Read-only tools run the moment the model asks
//! for them. Mutating tools are only advertised in
//! [`AgentMode::ConfirmActions`], and even then every call stops at
//! [`super::confirm`] for an explicit yes from the operator before it touches
//! anything.
//!
//! There is deliberately no "run arbitrary SQL" or "run arbitrary shell"
//! escape hatch. Database access is limited to a fixed set of named
//! diagnostics, and remediation the app cannot perform safely is surfaced via
//! `propose_command` for a human to run.

use std::sync::Arc;

use serde_json::json;

use crate::collectors::{CollectorAction, DetailQuery, GigapipeRead};
use crate::config::AgentMode;
use crate::error::{AppError, Result};
use crate::llm::ToolSpec;
use crate::state::{AppState, LogFilter};
use crate::supervisor;

/// Tool results are fed straight back into the context window, so oversized
/// payloads are trimmed rather than allowed to crowd out the conversation.
const MAX_TOOL_RESULT_CHARS: usize = 8_000;

pub struct ToolDef {
    pub spec: ToolSpec,
    /// True when running this changes infrastructure state.
    pub mutating: bool,
}

fn tool(name: &str, description: &str, schema: serde_json::Value, mutating: bool) -> ToolDef {
    ToolDef {
        spec: ToolSpec {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: schema,
        },
        mutating,
    }
}

/// A `connection` property, shared by most tools.
fn connection_property() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "Name or id of the configured connection, as listed by list_connections."
    })
}

pub fn catalog(mode: AgentMode) -> Vec<ToolDef> {
    let mut tools = vec![
        tool(
            "list_connections",
            "List every configured piece of infrastructure with its current health, \
             a one-line summary, and when it was last checked. Start here when you do not \
             yet know what exists.",
            json!({ "type": "object", "properties": {}, "required": [] }),
            false,
        ),
        tool(
            "get_health",
            "Full health snapshot for one connection, or for all of them when `connection` \
             is omitted. Returns service-level metrics plus `sections` — one table per kind \
             of thing the service contains (containers, jobs, agents, build queue, nodes, \
             indices, databases, active queries). Each section declares its columns, and \
             each row carries typed `cells` keyed by column, its own health, a one-line \
             `summary`, and any `actions` available on it.",
            json!({
                "type": "object",
                "properties": { "connection": connection_property() },
                "required": []
            }),
            false,
        ),
        tool(
            "search_logs",
            "Search the collected log buffer across all infrastructure. Use this to find \
             what happened around an incident.",
            json!({
                "type": "object",
                "properties": {
                    "connection": connection_property(),
                    "search": {
                        "type": "string",
                        "description": "Case-insensitive substring to match in the message or source."
                    },
                    "level": {
                        "type": "string",
                        "enum": ["trace", "debug", "info", "warn", "error", "fatal"],
                        "description": "Minimum severity to return."
                    },
                    "sinceMinutes": {
                        "type": "integer",
                        "description": "Only return lines from the last N minutes.",
                        "minimum": 1
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum lines to return (default 100).",
                        "minimum": 1,
                        "maximum": 500
                    }
                },
                "required": []
            }),
            false,
        ),
        tool(
            "list_alerts",
            "List alerts raised by the monitoring engine, newest and most severe first.",
            json!({
                "type": "object",
                "properties": {
                    "activeOnly": {
                        "type": "boolean",
                        "description": "Exclude alerts that have already resolved (default true)."
                    }
                },
                "required": []
            }),
            false,
        ),
        tool(
            "refresh_connection",
            "Re-poll one connection right now instead of waiting for its next scheduled \
             cycle. Use after a change to confirm the effect.",
            json!({
                "type": "object",
                "properties": { "connection": connection_property() },
                "required": ["connection"]
            }),
            false,
        ),
        tool(
            "inspect_container",
            "Full `docker inspect` output for one container: state, exit code, restart \
             count, health check history, mounts, env, and network settings.",
            json!({
                "type": "object",
                "properties": {
                    "connection": connection_property(),
                    "container": {
                        "type": "string",
                        "description": "Container name or id."
                    }
                },
                "required": ["connection", "container"]
            }),
            false,
        ),
        tool(
            "jenkins_build_log",
            "Console output of a Jenkins build. Defaults to the most recent build of the job.",
            json!({
                "type": "object",
                "properties": {
                    "connection": connection_property(),
                    "job": {
                        "type": "string",
                        "description": "Job name; use `folder/job` for jobs inside a folder."
                    },
                    "build": {
                        "type": "integer",
                        "description": "Build number. Omit for the last build."
                    },
                    "tailLines": {
                        "type": "integer",
                        "description": "How many trailing lines to return (default 200)."
                    }
                },
                "required": ["connection", "job"]
            }),
            false,
        ),
        tool(
            "elasticsearch_search",
            "Run a read-only Elasticsearch `_search` against an index. Pass a normal query \
             DSL body. This cannot write, update, or delete.",
            json!({
                "type": "object",
                "properties": {
                    "connection": connection_property(),
                    "index": {
                        "type": "string",
                        "description": "Index or index pattern to search."
                    },
                    "query": {
                        "type": "object",
                        "description": "The _search request body, e.g. {\"size\":5,\"query\":{\"match_all\":{}}}."
                    }
                },
                "required": ["connection", "index", "query"]
            }),
            false,
        ),
        tool(
            "database_diagnostic",
            "Run one of a fixed set of database health checks. Arbitrary SQL is not available.",
            json!({
                "type": "object",
                "properties": {
                    "connection": connection_property(),
                    "check": {
                        "type": "string",
                        "enum": ["connections", "long_queries", "table_sizes", "locks", "replication"],
                        "description": "connections: who is connected and in what state. \
                                        long_queries: queries running past the slow threshold. \
                                        table_sizes: largest tables. \
                                        locks: blocked/blocking pairs. \
                                        replication: primary/replica role and lag."
                    }
                },
                "required": ["connection", "check"]
            }),
            false,
        ),
        tool(
            "get_metric_trend",
            "Recent history for a connection's metrics: min, max, latest, percentage change, \
             and the raw samples. Use this to answer whether something is getting worse, \
             recovering, or steady — a single reading cannot tell you that.",
            json!({
                "type": "object",
                "properties": {
                    "connection": connection_property(),
                    "minutes": {
                        "type": "integer",
                        "description": "How far back to look (default 60).",
                        "minimum": 1,
                        "maximum": 1440
                    },
                    "metric": {
                        "type": "string",
                        "description": "Restrict to one metric key, e.g. `cpu_total`. Omit for all."
                    }
                },
                "required": ["connection"]
            }),
            false,
        ),
        tool(
            "list_config",
            "List the configuration root can see for a connection: the files an operator \
             declared, and whatever the running target itself can be asked for. Returns \
             each document's label, where it came from, its size, and how many values \
             were redacted — not the contents. Call read_config for a body. Start here \
             when a question is about how something is set up rather than how it is \
             behaving.",
            json!({
                "type": "object",
                "properties": { "connection": connection_property() },
                "required": ["connection"]
            }),
            false,
        ),
        tool(
            "read_config",
            "The contents of one configuration document, with every credential removed \
             before it reaches you. Passwords, private keys, tokens and the password half \
             of connection URLs are replaced with «redacted» markers; settings are intact. \
             Do not ask the operator to paste an unredacted version, and do not treat a \
             marker as a value. Some documents have no body at all — a `.env` is named but \
             never opened — and the `withheld` field says why.",
            json!({
                "type": "object",
                "properties": {
                    "connection": connection_property(),
                    "document": {
                        "type": "string",
                        "description": "Document id or label, as listed by list_config."
                    }
                },
                "required": ["connection", "document"]
            }),
            false,
        ),
        tool(
            "config_findings",
            "Problems found by reading a connection's configuration against its own \
             telemetry — a memory ceiling a metric is climbing toward, a pool about to \
             exhaust, containers with no limits. Each finding carries the setting, the \
             metric and the projection it rests on, plus the change that would fix it. \
             These are deterministic rules, not opinions: prefer them over your own \
             reading of a config file, and cite the evidence when you repeat one. Omit \
             `connection` for the whole estate.",
            json!({
                "type": "object",
                "properties": { "connection": connection_property() }
            }),
            false,
        ),
        tool(
            "report_finding",
            "Record a problem you have found, so it can be rated beside the deterministic \
             rules and counted. Use this whenever you conclude something is wrong or about \
             to be — not for answering a question, and not for restating what the operator \
             already told you. A finding you only describe in prose cannot be scored, which \
             means it never becomes a rule. If you are restating something config_findings \
             already reported, say so in `restatesRule`: getting credited for a rule's work \
             makes both of us look worse than we are.",
            json!({
                "type": "object",
                "properties": {
                    "connection": connection_property(),
                    "title": {
                        "type": "string",
                        "description": "One line, specific. `Replication lag is climbing on \
                                        replica-2`, not `Database issue`."
                    },
                    "detail": {
                        "type": "string",
                        "description": "What is wrong, why it matters, and what happens if \
                                        nobody acts."
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["now", "soon", "advisory"],
                        "description": "`now` — already true or within hours. `soon` — lands \
                                        inside a week. `advisory` — worth knowing, no deadline."
                    },
                    "evidence": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "The specific facts this rests on: a log line, a metric \
                                        value, a setting, a timestamp. A finding whose evidence \
                                        cannot be checked will be rated as unsure, which helps \
                                        nobody."
                    },
                    "suggestion": {
                        "type": "object",
                        "description": "What to change, if you are confident enough to say.",
                        "properties": {
                            "setting": { "type": "string" },
                            "current": { "type": "string" },
                            "proposed": { "type": "string" },
                            "apply": {
                                "type": "string",
                                "description": "The exact command, for a human to run. Nothing \
                                                executes it."
                            }
                        },
                        "required": ["setting", "current", "proposed"]
                    },
                    "restatesRule": {
                        "type": "string",
                        "description": "The rule id from config_findings you are restating, if \
                                        any. Omit when this is your own observation."
                    }
                },
                "required": ["connection", "title", "detail", "severity", "evidence"]
            }),
            false,
        ),
        tool(
            "propose_command",
            "Surface a remediation command for the operator to run themselves. Use this for \
             anything you cannot or should not execute directly. The command is shown in the \
             UI; it is never run automatically.",
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The exact command to run."
                    },
                    "rationale": {
                        "type": "string",
                        "description": "Why this fixes the problem, and what to expect afterwards."
                    }
                },
                "required": ["command", "rationale"]
            }),
            false,
        ),
    ];

    tools.extend(gigapipe_tools());

    if mode == AgentMode::ConfirmActions {
        tools.extend([
            tool(
                "restart_container",
                "Restart a Docker container. Requires the operator to approve it first.",
                json!({
                    "type": "object",
                    "properties": {
                        "connection": connection_property(),
                        "container": { "type": "string", "description": "Container name or id." }
                    },
                    "required": ["connection", "container"]
                }),
                true,
            ),
            tool(
                "start_container",
                "Start a stopped Docker container. Requires the operator to approve it first.",
                json!({
                    "type": "object",
                    "properties": {
                        "connection": connection_property(),
                        "container": { "type": "string", "description": "Container name or id." }
                    },
                    "required": ["connection", "container"]
                }),
                true,
            ),
            tool(
                "stop_container",
                "Stop a running Docker container. Requires the operator to approve it first.",
                json!({
                    "type": "object",
                    "properties": {
                        "connection": connection_property(),
                        "container": { "type": "string", "description": "Container name or id." }
                    },
                    "required": ["connection", "container"]
                }),
                true,
            ),
            tool(
                "trigger_jenkins_build",
                "Queue a build of a Jenkins job. Requires the operator to approve it first.",
                json!({
                    "type": "object",
                    "properties": {
                        "connection": connection_property(),
                        "job": { "type": "string", "description": "Job name, `folder/job` if nested." }
                    },
                    "required": ["connection", "job"]
                }),
                true,
            ),
            tool(
                "terminate_database_query",
                "Terminate a running database backend by process id. Requires the operator to \
                 approve it first. The client will see its query cancelled.",
                json!({
                    "type": "object",
                    "properties": {
                        "connection": connection_property(),
                        "pid": { "type": "string", "description": "Process/thread id from database_diagnostic." }
                    },
                    "required": ["connection", "pid"]
                }),
                true,
            ),
        ]);
    }

    tools
}

/// The nine read APIs a gigapipe connection answers, one tool each.
///
/// Gigapipe is a store as well as a service, and these are how the assistant
/// reaches what it holds: PromQL for metrics, LogQL for logs, and Tempo's tag
/// and trace endpoints for spans. They exist because root's own history is a
/// ring buffer of a few hours and its log buffer is in memory — so "what did
/// this look like last Tuesday" is a question root cannot answer from itself,
/// and can answer from here.
///
/// Every one is read-only, and each names a fixed endpoint rather than taking
/// a path. That is the same boundary `database_diagnostic` draws: the model
/// chooses among reviewed operations, never composes a request.
///
/// The `gigapipe_` prefix is load-bearing. root also monitors Prometheus
/// directly, so an unprefixed `prometheus_query` would be ambiguous about
/// which connection it addressed.
fn gigapipe_tools() -> Vec<ToolDef> {
    /// `start` and `end`, which every one of these accepts.
    fn window() -> serde_json::Value {
        json!({
            "start": {
                "type": "string",
                "description": "Start of the window, RFC3339 or a Unix timestamp. \
                                Omit to let the store apply its own default."
            },
            "end": {
                "type": "string",
                "description": "End of the window, RFC3339 or a Unix timestamp. \
                                Omit to let the store apply its own default."
            }
        })
    }

    /// One tool's schema: the shared `connection`, the window, and its own
    /// properties on top.
    fn schema(extra: serde_json::Value, required: &[&str]) -> serde_json::Value {
        let mut properties = json!({ "connection": connection_property() });
        for source in [window(), extra] {
            if let (Some(target), Some(source)) =
                (properties.as_object_mut(), source.as_object())
            {
                for (key, value) in source {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
        })
    }

    vec![
        tool(
            "gigapipe_prometheus_query",
            "Run a PromQL query against a gigapipe connection over a time range, returning the \
             store's own `{status, data}` response with every series that matched. This is the \
             exploratory path and reaches further back than root's own ring buffer; \
             `get_metric_trend` is the one to use for a metric root already collects.",
            schema(
                json!({
                    "query": {
                        "type": "string",
                        "description": "PromQL expression, e.g. rate(http_requests_total[5m])."
                    },
                    "step": {
                        "type": "string",
                        "description": "Resolution between points, e.g. 15s, 1m, 1h. Omit and \
                                        the store chooses. A long range with a small step \
                                        returns a great many points."
                    }
                }),
                &["query"],
            ),
            false,
        ),
        tool(
            "gigapipe_prometheus_labels",
            "List every metric label name a gigapipe connection knows about. Start here when \
             you do not yet know how the metrics in this store are labelled.",
            schema(json!({}), &[]),
            false,
        ),
        tool(
            "gigapipe_prometheus_label_values",
            "List every value one metric label takes in a gigapipe connection — which services, \
             instances or jobs actually exist — before writing a PromQL selector against it.",
            schema(
                json!({
                    "label": {
                        "type": "string",
                        "description": "Label name, e.g. instance or job."
                    }
                }),
                &["label"],
            ),
            false,
        ),
        tool(
            "gigapipe_loki_query",
            "Run a LogQL query against a gigapipe connection over a time range. This searches \
             what was shipped into the store, which is a different and much longer-lived body \
             of logs than `search_logs` — that one searches root's own in-memory buffer.",
            schema(
                json!({
                    "query": {
                        "type": "string",
                        "description": "LogQL expression, e.g. {job=\"varlogs\"} |= \"error\"."
                    },
                    "limit": {
                        "type": "string",
                        "description": "Maximum entries to return. Omit for the store's default."
                    }
                }),
                &["query"],
            ),
            false,
        ),
        tool(
            "gigapipe_loki_labels",
            "List every log label name a gigapipe connection knows about — the stream selectors \
             available to a LogQL query.",
            schema(json!({}), &[]),
            false,
        ),
        tool(
            "gigapipe_loki_label_values",
            "List every value one log label takes in a gigapipe connection, so a LogQL selector \
             names a stream that exists rather than one that returns nothing.",
            schema(
                json!({
                    "label": {
                        "type": "string",
                        "description": "Label name, e.g. job or namespace."
                    }
                }),
                &["label"],
            ),
            false,
        ),
        tool(
            "gigapipe_tempo_query",
            "Fetch one trace from a gigapipe connection by id, as the store returns it. Use \
             `trace` instead when you want the spans assembled into a tree with self time \
             worked out; this one is the raw document.",
            schema(
                json!({
                    "trace_id": {
                        "type": "string",
                        "description": "Trace id, as hexadecimal."
                    }
                }),
                &["trace_id"],
            ),
            false,
        ),
        tool(
            "gigapipe_tempo_tags",
            "List every trace tag name available for search in a gigapipe connection.",
            schema(json!({}), &[]),
            false,
        ),
        tool(
            "gigapipe_tempo_tag_values",
            "List every value one trace tag takes in a gigapipe connection — which services are \
             actually represented in its traces, for instance.",
            schema(
                json!({
                    "tag": {
                        "type": "string",
                        "description": "Tag name, e.g. service.name."
                    }
                }),
                &["tag"],
            ),
            false,
        ),
    ]
}

/// The prefix every gigapipe store read carries. Also what the MCP server
/// strips to recover the tool names `gigapipe-mcp` itself uses.
pub const GIGAPIPE_PREFIX: &str = "gigapipe_";

/// Turn one `gigapipe_*` call's arguments into the read it names.
///
/// Shared rather than inlined into the dispatch because root serves this same
/// surface twice — to its own assistant, and over MCP to other clients (see
/// [`crate::mcp`]). Two parsers would be two chances to disagree about what
/// `limit` means.
///
/// Empty and whitespace-only values count as absent. A model filling in a
/// field it was told is optional should produce the same request as one that
/// left it alone.
pub fn gigapipe_read_from_args(name: &str, args: &serde_json::Value) -> Result<GigapipeRead> {
    let text = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let required =
        |key: &str| text(key).ok_or_else(|| AppError::Collector(format!("`{key}` is required")));
    let (start, end) = (text("start"), text("end"));

    let bare = name.strip_prefix(GIGAPIPE_PREFIX).unwrap_or(name);
    Ok(match bare {
        "prometheus_query" => GigapipeRead::PrometheusQuery {
            query: required("query")?,
            start,
            end,
            step: text("step"),
        },
        "prometheus_labels" => GigapipeRead::PrometheusLabels { start, end },
        "prometheus_label_values" => GigapipeRead::PrometheusLabelValues {
            label: required("label")?,
            start,
            end,
        },
        "loki_query" => GigapipeRead::LokiQuery {
            query: required("query")?,
            start,
            end,
            // Passed through as written: the store takes a bare integer, and
            // re-typing it here would only add a second parser to disagree.
            limit: text("limit"),
        },
        "loki_labels" => GigapipeRead::LokiLabels { start, end },
        "loki_label_values" => GigapipeRead::LokiLabelValues {
            label: required("label")?,
            start,
            end,
        },
        "tempo_query" => GigapipeRead::TempoTrace {
            trace_id: required("trace_id")?,
        },
        "tempo_tags" => GigapipeRead::TempoTags,
        "tempo_tag_values" => GigapipeRead::TempoTagValues {
            tag: required("tag")?,
        },
        other => return Err(AppError::Collector(format!("unknown tool `{other}`"))),
    })
}

pub fn find(mode: AgentMode, name: &str) -> Option<ToolDef> {
    catalog(mode).into_iter().find(|t| t.spec.name == name)
}

/// Turn a mutating tool call into the collector action it maps to, so the
/// confirmation dialog and the executor agree on exactly what will happen.
pub fn action_for(name: &str, args: &serde_json::Value) -> Result<CollectorAction> {
    let string_arg = |key: &str| -> Result<String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| AppError::Collector(format!("`{key}` is required")))
    };

    Ok(match name {
        "restart_container" => CollectorAction::RestartContainer {
            container: string_arg("container")?,
        },
        "start_container" => CollectorAction::StartContainer {
            container: string_arg("container")?,
        },
        "stop_container" => CollectorAction::StopContainer {
            container: string_arg("container")?,
        },
        "trigger_jenkins_build" => CollectorAction::TriggerJenkinsBuild {
            job: string_arg("job")?,
        },
        "terminate_database_query" => CollectorAction::TerminateDbQuery {
            pid: string_arg("pid")?,
        },
        other => return Err(AppError::Collector(format!("`{other}` is not an action"))),
    })
}

/// Resolve the `connection` argument to a profile id.
pub async fn resolve_connection(state: &Arc<AppState>, args: &serde_json::Value) -> Result<String> {
    let needle = args
        .get("connection")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Collector("`connection` is required".to_string()))?;
    let config = state.config.read().await;
    Ok(config.profile_by_ref(needle)?.id.clone())
}

/// Run a read-only tool. Mutating tools are handled by the agent loop, which
/// owns the approval step.
/// The rule findings currently live for one connection.
///
/// Used only as the backstop for attribution — a model claim that names the
/// same setting as a rule finding is the same claim, whatever wording it
/// arrived in.
async fn current_rule_findings(
    state: &Arc<AppState>,
    profile_id: &str,
) -> Vec<crate::advisor::Finding> {
    let Some(snapshot) = state.snapshots.read().await.get(profile_id).cloned() else {
        return Vec::new();
    };
    let Ok(documents) = config_documents(state, profile_id).await else {
        return Vec::new();
    };
    let series = state.history.read().await.series(profile_id, None);
    crate::advisor::findings(&snapshot, &documents, &series)
}

/// Declared files and live config together, for whichever tool asked.
///
/// A target that is down or cannot be asked contributes nothing rather than
/// failing the call: the declared files are still worth returning, and an
/// assistant that gets an error here will conclude there is no config at all.
async fn config_documents(
    state: &Arc<AppState>,
    profile_id: &str,
) -> Result<Vec<crate::configsource::ConfigDocument>> {
    let sources = {
        let config = state.config.read().await;
        config
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .map(|p| p.config_sources.clone())
            .unwrap_or_default()
    };

    let mut documents = Vec::new();
    for source in &sources {
        documents.extend(crate::configsource::read_source(profile_id, source).await);
    }
    if let Ok(collector) = state.collector(profile_id).await {
        if let Ok(live) = collector.config().await {
            documents.extend(live);
        }
    }
    Ok(documents)
}

pub async fn run_read_only(
    state: &Arc<AppState>,
    name: &str,
    args: &serde_json::Value,
) -> Result<String> {
    let value = match name {
        "list_connections" => {
            let config = state.config.read().await;
            let snapshots = state.snapshots.read().await;
            json!(config
                .profiles
                .iter()
                .map(|p| {
                    let snapshot = snapshots.get(&p.id);
                    json!({
                        "name": p.name,
                        "id": p.id,
                        "kind": p.kind.label(),
                        "endpoint": p.endpoint,
                        "enabled": p.enabled,
                        "health": snapshot.map(|s| s.health.label()).unwrap_or("unknown"),
                        "summary": snapshot.map(|s| s.summary.clone()).unwrap_or_default(),
                        "lastChecked": snapshot.map(|s| s.checked_at.to_rfc3339()),
                        "error": snapshot.and_then(|s| s.error.clone()),
                    })
                })
                .collect::<Vec<_>>())
        }

        "get_health" => {
            let snapshots = state.snapshots.read().await;
            match args.get("connection").and_then(|v| v.as_str()) {
                Some(_) => {
                    let id = resolve_connection(state, args).await?;
                    let snapshot = snapshots.get(&id).ok_or_else(|| {
                        AppError::Collector(
                            "that connection has not been polled yet; try refresh_connection"
                                .to_string(),
                        )
                    })?;
                    json!(snapshot)
                }
                None => json!(snapshots.values().collect::<Vec<_>>()),
            }
        }

        "search_logs" => {
            let profile_ids = match args.get("connection").and_then(|v| v.as_str()) {
                Some(_) => Some(vec![resolve_connection(state, args).await?]),
                None => None,
            };
            let filter = LogFilter {
                profile_ids,
                kinds: None,
                // The assistant sees every stream — build console and
                // diagnostics included — because it is investigating, not
                // reading a live tail.
                streams: None,
                min_level: args
                    .get("level")
                    .and_then(|v| v.as_str())
                    .and_then(parse_level),
                search: args
                    .get("search")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                since: args
                    .get("sinceMinutes")
                    .and_then(|v| v.as_i64())
                    .and_then(|m| chrono::Duration::try_minutes(m).map(|d| chrono::Utc::now() - d)),
                limit: args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .or(Some(100)),
                // The assistant searches by text, not by clustered template.
                pattern: None,
            };
            let events = state.logs.read().await.query(&filter);
            json!({
                "matched": events.len(),
                "lines": events
                    .iter()
                    .map(|e| json!({
                        "time": e.timestamp.to_rfc3339(),
                        "connection": e.profile_name,
                        "source": e.source,
                        "level": e.level,
                        "message": e.message,
                    }))
                    .collect::<Vec<_>>()
            })
        }

        "list_alerts" => {
            let active_only = args
                .get("activeOnly")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let alerts = state.alerts.read().await.all();
            json!(alerts
                .into_iter()
                .filter(|a| !active_only || a.active)
                .map(|a| json!({
                    "severity": a.severity,
                    "title": a.title,
                    "detail": a.detail,
                    "connection": a.profile_name,
                    "component": a.component,
                    "active": a.active,
                    "firstSeen": a.first_seen.to_rfc3339(),
                    "lastSeen": a.last_seen.to_rfc3339(),
                }))
                .collect::<Vec<_>>())
        }

        "refresh_connection" => {
            let id = resolve_connection(state, args).await?;
            json!(supervisor::poll_once(state, &id).await?)
        }

        "list_config" => {
            let id = resolve_connection(state, args).await?;
            let documents = config_documents(state, &id).await?;
            json!(documents
                .iter()
                .map(|d| json!({
                    "id": d.id,
                    "label": d.label,
                    "origin": d.origin,
                    "format": d.format,
                    "bytes": d.bytes,
                    "modified": d.modified.map(|m| m.to_rfc3339()),
                    "withheld": d.withheld,
                    // The count rather than the list, so a document with forty
                    // redactions does not crowd out the other nine documents.
                    "redactedValues": d.redactions.len(),
                }))
                .collect::<Vec<_>>())
        }

        "read_config" => {
            let id = resolve_connection(state, args).await?;
            let wanted = args
                .get("document")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::Collector("a document id or label is required".into()))?;

            let documents = config_documents(state, &id).await?;
            let document = documents
                .iter()
                .find(|d| d.id == wanted || d.label == wanted)
                .ok_or_else(|| {
                    AppError::Collector(format!(
                        "no document `{wanted}` — list_config names what exists"
                    ))
                })?;

            json!(document)
        }

        "config_findings" => {
            let scope = match args.get("connection").and_then(|v| v.as_str()) {
                Some(_) => Some(resolve_connection(state, args).await?),
                None => None,
            };

            let profiles: Vec<crate::config::ConnectionProfile> = {
                let config = state.config.read().await;
                config
                    .profiles
                    .iter()
                    .filter(|p| p.enabled)
                    .filter(|p| scope.as_ref().is_none_or(|id| &p.id == id))
                    .cloned()
                    .collect()
            };

            let mut found = Vec::new();
            for profile in &profiles {
                let Some(snapshot) = state.snapshots.read().await.get(&profile.id).cloned() else {
                    continue;
                };
                let documents = config_documents(state, &profile.id).await?;
                if documents.is_empty() {
                    continue;
                }
                let series = state.history.read().await.series(&profile.id, None);
                found.extend(crate::advisor::findings(&snapshot, &documents, &series));
            }
            found.sort_by_key(|f| std::cmp::Reverse(f.severity));

            if found.is_empty() {
                json!({
                    "findings": [],
                    "note": "Nothing found. Either no configuration is declared for these \
                             connections, or the rules that exist had nothing to say — \
                             absence here is not a clean bill of health."
                })
            } else {
                json!(found)
            }
        }

        "report_finding" => {
            let id = resolve_connection(state, args).await?;
            let field = |name: &str| {
                args.get(name)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_default()
            };

            let title = field("title");
            if title.trim().is_empty() {
                return Err(AppError::Collector("a finding needs a title".into()));
            }

            let severity = match args.get("severity").and_then(|v| v.as_str()) {
                Some("now") => crate::advisor::Severity::Now,
                Some("soon") => crate::advisor::Severity::Soon,
                _ => crate::advisor::Severity::Advisory,
            };

            let evidence: Vec<String> = args
                .get("evidence")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            let (profile_name, model) = {
                let config = state.config.read().await;
                (
                    config
                        .profiles
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| id.clone()),
                    config.llm.model.clone(),
                )
            };

            // The live rule findings, so a restatement the model did not
            // declare is still caught.
            let rule_findings = current_rule_findings(state, &id).await;

            let finding = crate::claims::attribute(
                &id,
                &profile_name,
                &model,
                crate::claims::Reported {
                    title,
                    detail: field("detail"),
                    severity,
                    evidence,
                    suggestion: args
                        .get("suggestion")
                        .and_then(|v| serde_json::from_value(v.clone()).ok()),
                    restates_rule: args
                        .get("restatesRule")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                },
                &rule_findings,
            );

            let attribution = match &finding.provenance {
                crate::advisor::Provenance::ModelCitingRule { rule, .. } => format!(
                    "Recorded, attributed to the `{rule}` rule you are restating rather than \
                     to you."
                ),
                _ => "Recorded as your own finding. It will be rated beside the rules."
                    .to_string(),
            };

            let mut claims = state.claims.write().await;
            claims.sweep();
            claims.record(
                &id,
                crate::claims::Claim {
                    finding: finding.clone(),
                    at: chrono::Utc::now(),
                    conversation_id: None,
                },
            );

            json!({ "recorded": true, "findingId": finding.id, "note": attribution })
        }

        "get_metric_trend" => {
            let id = resolve_connection(state, args).await?;
            let minutes = args.get("minutes").and_then(|v| v.as_i64()).unwrap_or(60);
            let since = chrono::Utc::now() - chrono::Duration::minutes(minutes.clamp(1, 1440));
            let wanted = args.get("metric").and_then(|v| v.as_str());

            let series = state.history.read().await.series(&id, Some(since));
            let series: Vec<_> = series
                .into_iter()
                .filter(|s| wanted.is_none_or(|key| s.key == key))
                .map(|s| {
                    json!({
                        "metric": s.key,
                        "label": s.label,
                        "unit": s.unit,
                        "latest": s.latest,
                        "min": s.min,
                        "max": s.max,
                        "changePct": s.change_pct,
                        "samples": s.points.len(),
                        // The full series would swamp the context; the shape is
                        // what matters, and min/max/change carry the rest.
                        "recent": s
                            .points
                            .iter()
                            .rev()
                            .take(20)
                            .rev()
                            .map(|p| json!({ "t": p.t.to_rfc3339(), "v": p.v }))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();

            if series.is_empty() {
                json!({
                    "note": "No history yet for that connection — it may have only just \
                             started polling, or the metric key may be wrong.",
                    "metrics": [],
                })
            } else {
                json!({ "windowMinutes": minutes, "metrics": series })
            }
        }

        "inspect_container" => {
            let id = resolve_connection(state, args).await?;
            let container = args
                .get("container")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::Collector("`container` is required".to_string()))?;
            state
                .collector(&id)
                .await?
                .detail(&DetailQuery::Container {
                    container: container.to_string(),
                })
                .await?
        }

        "jenkins_build_log" => {
            let id = resolve_connection(state, args).await?;
            let job = args
                .get("job")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::Collector("`job` is required".to_string()))?;
            state
                .collector(&id)
                .await?
                .detail(&DetailQuery::JenkinsBuildLog {
                    job: job.to_string(),
                    build: args.get("build").and_then(|v| v.as_i64()),
                    tail_lines: args
                        .get("tailLines")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize),
                })
                .await?
        }

        "elasticsearch_search" => {
            let id = resolve_connection(state, args).await?;
            let index = args
                .get("index")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::Collector("`index` is required".to_string()))?;
            let body = args
                .get("query")
                .cloned()
                .ok_or_else(|| AppError::Collector("`query` is required".to_string()))?;
            state
                .collector(&id)
                .await?
                .detail(&DetailQuery::EsSearch {
                    index: index.to_string(),
                    body,
                })
                .await?
        }

        "database_diagnostic" => {
            let id = resolve_connection(state, args).await?;
            let check = args
                .get("check")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::Collector("`check` is required".to_string()))?;
            state
                .collector(&id)
                .await?
                .detail(&DetailQuery::DbDiagnostic {
                    check: check.to_string(),
                })
                .await?
        }

        name if name.starts_with(GIGAPIPE_PREFIX) => {
            let id = resolve_connection(state, args).await?;
            let read = gigapipe_read_from_args(name, args)?;
            state
                .collector(&id)
                .await?
                .detail(&DetailQuery::GigapipeRead(read))
                .await?
        }

        "propose_command" => {
            // Recorded and surfaced in the UI; deliberately never executed.
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            json!({
                "recorded": true,
                "command": command,
                "note": "Shown to the operator as a suggestion. Nothing was run."
            })
        }

        other => {
            return Err(AppError::Collector(format!("unknown tool `{other}`")));
        }
    };

    Ok(truncate_result(&serde_json::to_string_pretty(&value)?))
}

fn parse_level(raw: &str) -> Option<crate::model::LogLevel> {
    use crate::model::LogLevel::*;
    Some(match raw.to_ascii_lowercase().as_str() {
        "trace" => Trace,
        "debug" => Debug,
        "info" => Info,
        "warn" | "warning" => Warn,
        "error" => Error,
        "fatal" | "critical" => Fatal,
        _ => return None,
    })
}

/// Trim on a character boundary and say so, rather than silently handing the
/// model a truncated JSON document it might misread as complete.
fn truncate_result(text: &str) -> String {
    if text.chars().count() <= MAX_TOOL_RESULT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_TOOL_RESULT_CHARS).collect();
    format!(
        "{head}\n\n[truncated: the full result was {} characters. Narrow the query \
         — add a filter, a smaller limit, or a specific connection.]",
        text.chars().count()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_mode_advertises_no_mutating_tools() {
        let tools = catalog(AgentMode::ReadOnly);
        assert!(tools.iter().all(|t| !t.mutating));
        assert!(tools.iter().any(|t| t.spec.name == "propose_command"));
        assert!(tools.iter().all(|t| t.spec.name != "restart_container"));
    }

    /// Every gigapipe store read the assistant can make.
    const GIGAPIPE_TOOLS: &[&str] = &[
        "gigapipe_prometheus_query",
        "gigapipe_prometheus_labels",
        "gigapipe_prometheus_label_values",
        "gigapipe_loki_query",
        "gigapipe_loki_labels",
        "gigapipe_loki_label_values",
        "gigapipe_tempo_query",
        "gigapipe_tempo_tags",
        "gigapipe_tempo_tag_values",
    ];

    /// Reading a store changes nothing, so none of these may be gated behind
    /// `ConfirmActions` — an operator who has deliberately kept the assistant
    /// read-only still gets to ask what the warehouse holds.
    #[test]
    fn every_gigapipe_read_is_available_in_read_only_mode() {
        for mode in [AgentMode::ReadOnly, AgentMode::ConfirmActions] {
            for name in GIGAPIPE_TOOLS {
                let found = find(mode, name)
                    .unwrap_or_else(|| panic!("{name} is missing from the {mode:?} catalog"));
                assert!(!found.mutating, "{name} must not be mutating");
            }
        }
    }

    /// The dispatch arm matches on the `gigapipe_` prefix, so a tool whose
    /// name loses the prefix would be advertised and then fail to run.
    #[test]
    fn every_gigapipe_tool_is_reachable_by_its_prefix() {
        let advertised: Vec<String> = catalog(AgentMode::ReadOnly)
            .into_iter()
            .map(|t| t.spec.name)
            .filter(|n| n.starts_with("gigapipe_"))
            .collect();
        assert_eq!(advertised.len(), GIGAPIPE_TOOLS.len());
        for name in GIGAPIPE_TOOLS {
            assert!(advertised.iter().any(|n| n == name), "{name} not advertised");
        }
    }

    /// Each schema has to declare the arguments its dispatch arm requires, or
    /// the model is invited to call something that can only fail.
    #[test]
    fn gigapipe_schemas_require_what_the_dispatch_demands() {
        let required = |name: &str| -> Vec<String> {
            find(AgentMode::ReadOnly, name).unwrap().spec.input_schema["required"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(required("gigapipe_prometheus_query"), ["query"]);
        assert_eq!(required("gigapipe_loki_query"), ["query"]);
        assert_eq!(required("gigapipe_prometheus_label_values"), ["label"]);
        assert_eq!(required("gigapipe_loki_label_values"), ["label"]);
        assert_eq!(required("gigapipe_tempo_query"), ["trace_id"]);
        assert_eq!(required("gigapipe_tempo_tag_values"), ["tag"]);
        assert!(required("gigapipe_tempo_tags").is_empty());
        assert!(required("gigapipe_prometheus_labels").is_empty());
        assert!(required("gigapipe_loki_labels").is_empty());
    }

    /// `start` and `end` are offered on every one of these, because a question
    /// about a store worth querying is nearly always a question about a window.
    #[test]
    fn every_gigapipe_tool_offers_a_time_window() {
        for name in GIGAPIPE_TOOLS {
            let schema = find(AgentMode::ReadOnly, name).unwrap().spec.input_schema;
            let properties = &schema["properties"];
            for key in ["connection", "start", "end"] {
                assert!(
                    properties.get(key).is_some(),
                    "{name} does not offer `{key}`"
                );
            }
        }
    }

    #[test]
    fn confirm_mode_adds_mutating_tools_and_keeps_the_read_only_ones() {
        let read_only = catalog(AgentMode::ReadOnly).len();
        let tools = catalog(AgentMode::ConfirmActions);
        assert!(tools.len() > read_only);
        assert!(tools.iter().any(|t| t.spec.name == "restart_container" && t.mutating));
        assert!(tools.iter().any(|t| t.spec.name == "search_logs" && !t.mutating));
    }

    #[test]
    fn every_mutating_tool_maps_to_an_action() {
        for tool in catalog(AgentMode::ConfirmActions).iter().filter(|t| t.mutating) {
            let args = json!({
                "connection": "c", "container": "x", "job": "j", "pid": "42"
            });
            assert!(
                action_for(&tool.spec.name, &args).is_ok(),
                "{} has no action mapping",
                tool.spec.name
            );
        }
    }

    #[test]
    fn actions_reject_missing_arguments() {
        assert!(action_for("restart_container", &json!({})).is_err());
        assert!(action_for("restart_container", &json!({ "container": "  " })).is_err());
    }

    #[test]
    fn oversized_results_are_flagged_not_silently_cut() {
        let long = "x".repeat(MAX_TOOL_RESULT_CHARS + 50);
        let out = truncate_result(&long);
        assert!(out.contains("[truncated"));
        assert!(out.len() < long.len() + 300);
    }

    #[test]
    fn tool_schemas_are_well_formed_objects() {
        for tool in catalog(AgentMode::ConfirmActions) {
            let schema = &tool.spec.input_schema;
            assert_eq!(schema["type"], "object", "{}", tool.spec.name);
            assert!(schema.get("properties").is_some(), "{}", tool.spec.name);
            assert!(!tool.spec.description.is_empty(), "{}", tool.spec.name);
        }
    }
}
