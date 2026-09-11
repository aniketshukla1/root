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

use crate::collectors::{CollectorAction, DetailQuery};
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
            found.sort_by(|a, b| b.severity.cmp(&a.severity));

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
