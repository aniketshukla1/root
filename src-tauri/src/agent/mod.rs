//! The agent loop.
//!
//! One turn is: send the conversation to the model, run whatever tools it asks
//! for, feed the results back, repeat until it produces an answer with no
//! further tool calls (or hits [`MAX_STEPS`]).
//!
//! Mutating tools never execute on the model's say-so. Each one blocks on
//! [`confirm`], which pushes a request to the UI and waits for the operator to
//! approve or decline. Declining is not an error — the refusal is fed back as
//! the tool result so the model can adapt.

pub mod tools;

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;
use crate::events::Events;
use tokio::sync::oneshot;

use crate::config::AgentMode;
use crate::error::{AppError, Result};
use crate::llm::{build_provider, ChatMessage, CompletionRequest, ToolCall, Usage};
use crate::secrets;
use crate::state::{AppState, PendingConfirmation};

/// Upper bound on model→tool→model round trips in a single turn. High enough
/// for a real multi-service investigation, low enough to bound cost.
const MAX_STEPS: usize = 10;

/// How long an approval request waits before it is treated as declined. A
/// prompt the operator never saw must not leave a task pinned forever.
const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(300);

/// Conversation messages kept before older ones are dropped.
///
/// Tool results here are large — a health snapshot or a log search can be
/// thousands of characters — so an unbounded history would eventually blow the
/// context window and hard-fail mid-incident, which is the worst possible time.
const MAX_HISTORY_MESSAGES: usize = 40;

pub const EVENT_STEP: &str = "agent://step";
pub const EVENT_CONFIRM_REQUEST: &str = "agent://confirm-request";
pub const EVENT_CONFIRM_RESOLVED: &str = "agent://confirm-resolved";

// `rename_all` only renames the variants; struct-variant *fields* need
// `rename_all_fields` to reach the UI as camelCase like every other payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "type")]
pub enum AgentEvent {
    /// Prose the model produced on its way to an answer.
    Thinking {
        conversation_id: String,
        text: String,
    },
    ToolStart {
        conversation_id: String,
        call_id: String,
        tool: String,
        arguments: serde_json::Value,
        mutating: bool,
    },
    ToolEnd {
        conversation_id: String,
        call_id: String,
        tool: String,
        ok: bool,
        summary: String,
        duration_ms: u64,
    },
    Done {
        conversation_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStep {
    pub tool: String,
    pub arguments: serde_json::Value,
    pub ok: bool,
    pub mutating: bool,
    /// `Some(false)` when the operator declined a mutating call.
    pub approved: Option<bool>,
    pub result: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTurn {
    pub conversation_id: String,
    pub text: String,
    pub steps: Vec<AgentStep>,
    pub usage: Usage,
    /// True when the loop stopped at MAX_STEPS rather than finishing.
    pub truncated: bool,
}

/// Ask the operator to approve a mutating action, and wait.
///
/// Returns `false` on decline, on timeout, or if the UI channel drops — every
/// ambiguous outcome is a refusal, never an approval.
async fn confirm(
    events: &Events,
    state: &Arc<AppState>,
    request: PendingConfirmation,
) -> bool {
    let (tx, rx) = oneshot::channel();
    let call_id = request.call_id.clone();

    state.pending.lock().await.insert(call_id.clone(), tx);
    state
        .pending_meta
        .write()
        .await
        .insert(call_id.clone(), request.clone());

    events.emit(EVENT_CONFIRM_REQUEST, &request);

    let approved = match tokio::time::timeout(CONFIRMATION_TIMEOUT, rx).await {
        Ok(Ok(decision)) => decision,
        // Channel dropped, or nobody answered in time.
        Ok(Err(_)) | Err(_) => false,
    };

    state.pending.lock().await.remove(&call_id);
    state.pending_meta.write().await.remove(&call_id);
    events.emit(
        EVENT_CONFIRM_RESOLVED,
        &serde_json::json!({ "callId": call_id, "approved": approved }),
    );

    approved
}

/// Record the operator's decision for a pending action. Returns an error if the
/// request already timed out or was answered.
pub async fn resolve_confirmation(
    state: &Arc<AppState>,
    call_id: &str,
    approved: bool,
) -> Result<()> {
    let sender = state.pending.lock().await.remove(call_id).ok_or_else(|| {
        AppError::Config(
            "that approval request is no longer waiting — it may have timed out".to_string(),
        )
    })?;
    sender
        .send(approved)
        .map_err(|_| AppError::Config("the assistant is no longer waiting for it".to_string()))
}

/// Drop the oldest messages once the history grows past
/// [`MAX_HISTORY_MESSAGES`], always resuming at a user turn.
///
/// The cut point matters: an assistant turn that requests tools must be
/// followed by its results, and a result must be preceded by its call. Slicing
/// mid-sequence leaves an orphan that Anthropic rejects outright and that
/// confuses the other two. Starting at a `User` message is the only cut that is
/// always coherent.
fn trim_history(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    if messages.len() <= MAX_HISTORY_MESSAGES {
        return messages;
    }

    let start = messages.len() - MAX_HISTORY_MESSAGES;
    let cut = messages
        .iter()
        .skip(start)
        .position(|m| m.role == crate::llm::Role::User)
        .map(|offset| start + offset);

    match cut {
        Some(index) => messages[index..].to_vec(),
        // No user turn in the tail at all (pathological). Keeping the newest
        // exchange whole beats sending a broken one.
        None => messages,
    }
}

/// Build the system prompt, grounding the model in what is actually configured
/// and what authority it has.
async fn system_prompt(state: &Arc<AppState>, mode: AgentMode) -> String {
    let config = state.config.read().await;
    let snapshots = state.snapshots.read().await;

    let inventory = if config.profiles.is_empty() {
        "Nothing is configured yet. Tell the user to add a connection in Settings.".to_string()
    } else {
        config
            .profiles
            .iter()
            .map(|p| {
                let snapshot = snapshots.get(&p.id);
                format!(
                    "- {} ({}) — {}{}",
                    p.name,
                    p.kind.label(),
                    snapshot.map(|s| s.health.label()).unwrap_or("not polled yet"),
                    snapshot
                        .map(|s| format!(": {}", s.summary))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let authority = match mode {
        AgentMode::ReadOnly => {
            "You are in READ-ONLY mode. You can inspect anything but change nothing. \
             When something needs fixing, call `propose_command` with the exact command \
             and explain what it will do. Never claim you performed an action."
        }
        AgentMode::ConfirmActions => {
            "You are in CONFIRM-ACTIONS mode. You may call mutating tools (restart/start/stop \
             a container, trigger a Jenkins build, terminate a database query), but each one \
             pauses for the operator's explicit approval before it runs. Say what you intend \
             to do and why *before* calling it. If the operator declines, accept that, do not \
             retry the same action, and suggest an alternative. For anything outside those \
             tools, use `propose_command`."
        }
    };

    format!(
        "You are the assistant inside root, a desktop tool an operator uses to run their \
infrastructure — containers, clusters, queues, caches, databases, hosts and storage. You can \
see two things about any of it: how it is behaving, and how it is configured. Most useful \
answers come from reading those against each other.

{authority}

Currently configured connections:
{inventory}

Working with configuration:
- Every config body you receive has already had its credentials removed. Passwords, private \
keys, tokens and the password half of connection URLs arrive as «redacted» markers. Never ask \
the operator to paste an unredacted version, never treat a marker as a value, and never \
suggest disabling redaction.
- Some documents have no body at all — a `.env` is listed but never opened. That is deliberate. \
Say what is there and move on.
- `config_findings` runs deterministic rules over config and telemetry together. Prefer it to \
your own reading of a config file: it will not invent a setting, and it cites the metric and \
the projection behind every claim. When you repeat a finding, repeat its evidence.
- When you conclude something is wrong, call `report_finding`. A problem you only describe in \
a sentence cannot be rated, and a finding nobody rates never becomes a rule — which is the \
one way a thing you spotted stops depending on you spotting it again. Report the things the \
rules did not catch; that is where you are worth having. If you are restating a rule, say so \
in `restatesRule` rather than letting it count as yours.
- Scaling advice needs all three: the limit from config, the current value from metrics, and \
the trend. \"Memory is high\" is not advice. \"maxmemory is 2 GB, you are at 1.9 GB, climbing \
40 MB/h, and the policy is noeviction, so writes start failing in about two hours\" is.
- A file on disk and the running value disagreeing usually means a reload that never happened. \
That is worth saying explicitly.

How to work:
- Investigate before concluding. Pull real data with the tools rather than speculating; if you \
have not checked, say so.
- Chain tools when it helps: an alert leads to a health snapshot, which leads to logs, which \
leads to a container inspect.
- Prefer specific evidence — exit codes, error lines, metric values, timestamps — over \
generalities. Quote the line that proves your point.
- When you cannot determine something, say what you would need in order to.
- Be concise and operational. An operator reading this is probably mid-incident: lead with what \
is wrong and what to do about it. Skip preamble.
- Refer to connections by the names listed above.
- Today is {}.",
        Utc::now().format("%Y-%m-%d %H:%M UTC")
    )
}

/// Run one user turn to completion.
pub async fn run_turn(
    events: Events,
    state: Arc<AppState>,
    conversation_id: String,
    user_message: String,
) -> Result<AgentTurn> {
    let (settings, mode) = {
        let config = state.config.read().await;
        (config.llm.clone(), config.llm.agent_mode)
    };

    let api_key = secrets::get_secret(settings.provider.secret_key()).await?;
    let provider = build_provider(&settings, api_key)?;

    let catalog = tools::catalog(mode);
    let specs: Vec<_> = catalog.iter().map(|t| t.spec.clone()).collect();
    let system = system_prompt(&state, mode).await;

    // Append to the stored history so follow-up questions keep their context.
    let mut messages = {
        let mut conversations = state.conversations.write().await;
        let history = conversations.entry(conversation_id.clone()).or_default();
        history.push(ChatMessage::user(user_message));
        trim_history(history.clone())
    };

    let mut steps: Vec<AgentStep> = Vec::new();
    let mut usage = Usage::default();
    let mut final_text = String::new();
    let mut truncated = true;

    for _ in 0..MAX_STEPS {
        let response = provider
            .complete(CompletionRequest {
                model: settings.model.clone(),
                system: system.clone(),
                messages: messages.clone(),
                tools: specs.clone(),
                max_tokens: settings.max_tokens,
                temperature: settings.temperature,
            })
            .await?;

        usage.input_tokens += response.usage.input_tokens;
        usage.output_tokens += response.usage.output_tokens;

        if !response.text.trim().is_empty() {
            events.emit(
                EVENT_STEP,
                &AgentEvent::Thinking {
                    conversation_id: conversation_id.clone(),
                    text: response.text.clone(),
                },
            );
        }

        messages.push(ChatMessage {
            replay: response.replay.clone(),
            ..ChatMessage::assistant(response.text.clone(), response.tool_calls.clone())
        });

        // No tools requested: this is the answer.
        if response.tool_calls.is_empty() {
            final_text = response.text;
            truncated = false;
            break;
        }

        for call in &response.tool_calls {
            let (result, step) =
                execute_call(&events, &state, &conversation_id, mode, &catalog, call).await;
            steps.push(step);
            messages.push(result);
        }

        // Keep prose from intermediate steps so a run that hits MAX_STEPS still
        // returns something useful rather than an empty answer.
        if !response.text.trim().is_empty() {
            final_text = response.text;
        }
    }

    if truncated && final_text.trim().is_empty() {
        final_text = format!(
            "I stopped after {MAX_STEPS} tool calls without reaching a conclusion. \
             Here is what I gathered — try narrowing the question."
        );
    }

    // Persist the completed exchange. A replayed turn is bound to this
    // question's system prompt, which the next question rebuilds, so from here
    // on it goes back as its text and tool calls.
    for message in &mut messages {
        message.replay = None;
    }
    state
        .conversations
        .write()
        .await
        .insert(conversation_id.clone(), messages);

    events.emit(
        EVENT_STEP,
        &AgentEvent::Done {
            conversation_id: conversation_id.clone(),
        },
    );

    Ok(AgentTurn {
        conversation_id,
        text: final_text,
        steps,
        usage,
        truncated,
    })
}

/// Execute one tool call, gating it behind approval when it mutates state.
async fn execute_call(
    events: &Events,
    state: &Arc<AppState>,
    conversation_id: &str,
    mode: AgentMode,
    catalog: &[tools::ToolDef],
    call: &ToolCall,
) -> (ChatMessage, AgentStep) {
    let started = Instant::now();
    let definition = catalog.iter().find(|t| t.spec.name == call.name);
    let mutating = definition.map(|d| d.mutating).unwrap_or(false);

    events.emit(
        EVENT_STEP,
        &AgentEvent::ToolStart {
            conversation_id: conversation_id.to_string(),
            call_id: call.id.clone(),
            tool: call.name.clone(),
            arguments: call.arguments.clone(),
            mutating,
        },
    );

    let outcome: Result<String> = async {
        if definition.is_none() {
            return Err(AppError::Collector(format!(
                "`{}` is not an available tool",
                call.name
            )));
        }

        if !mutating {
            return tools::run_read_only(state, &call.name, &call.arguments).await;
        }

        // Belt and braces: even if a mutating tool somehow reaches here in
        // read-only mode, refuse it.
        if mode != AgentMode::ConfirmActions {
            return Err(AppError::Rejected(
                "This assistant is in read-only mode and cannot change infrastructure. \
                 Use propose_command instead."
                    .to_string(),
            ));
        }

        let profile_id = tools::resolve_connection(state, &call.arguments).await?;
        let action = tools::action_for(&call.name, &call.arguments)?;

        let (profile_name, endpoint) = {
            let config = state.config.read().await;
            let profile = config.profile(&profile_id)?;
            (profile.name.clone(), profile.endpoint.clone())
        };

        let approved = confirm(
            events,
            state,
            PendingConfirmation {
                call_id: call.id.clone(),
                conversation_id: conversation_id.to_string(),
                tool: call.name.clone(),
                description: format!("{} on `{profile_name}`", action.describe()),
                equivalent_command: action.equivalent_command(&endpoint),
                profile_name,
                arguments: call.arguments.clone(),
                requested_at: Utc::now(),
            },
        )
        .await;

        if !approved {
            return Err(AppError::Rejected(
                "The operator declined this action. Do not retry it; explain the alternatives \
                 or propose a command they can run themselves."
                    .to_string(),
            ));
        }

        state.collector(&profile_id).await?.run_action(&action).await
    }
    .await;

    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let (ok, body) = match &outcome {
        Ok(text) => (true, text.clone()),
        Err(e) => (false, e.to_string()),
    };

    // A decline is a legitimate outcome, not a failure of the tool.
    let approved = if mutating {
        Some(!matches!(&outcome, Err(AppError::Rejected(_))))
    } else {
        None
    };

    events.emit(
        EVENT_STEP,
        &AgentEvent::ToolEnd {
            conversation_id: conversation_id.to_string(),
            call_id: call.id.clone(),
            tool: call.name.clone(),
            ok,
            summary: summarize(&body),
            duration_ms,
        },
    );

    (
        ChatMessage::tool_result(call, body.clone(), !ok),
        AgentStep {
            tool: call.name.clone(),
            arguments: call.arguments.clone(),
            ok,
            mutating,
            approved,
            result: body,
            duration_ms,
        },
    )
}

/// One-line preview for the UI's tool transcript.
fn summarize(body: &str) -> String {
    let first_line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let cleaned = first_line.trim();
    if cleaned.chars().count() <= 120 {
        cleaned.to_string()
    } else {
        format!("{}…", cleaned.chars().take(120).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::llm::{Role, ToolCall};

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "get_health".into(),
            arguments: serde_json::json!({}),
        }
    }

    /// A repeating user → assistant(tool) → tool cycle, the shape a real
    /// investigation produces.
    fn history(cycles: usize) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        for i in 0..cycles {
            let id = format!("c{i}");
            messages.push(ChatMessage::user(format!("question {i}")));
            messages.push(ChatMessage::assistant("", vec![call(&id)]));
            messages.push(ChatMessage::tool_result(&call(&id), "result", false));
        }
        messages
    }

    #[test]
    fn short_histories_pass_through_untouched() {
        let messages = history(3);
        assert_eq!(trim_history(messages.clone()).len(), messages.len());
    }

    #[test]
    fn trimming_always_resumes_at_a_user_turn() {
        let trimmed = trim_history(history(40));
        assert!(trimmed.len() <= MAX_HISTORY_MESSAGES);
        assert_eq!(
            trimmed[0].role,
            Role::User,
            "a tool result or assistant turn as the first message orphans its pair"
        );
    }

    #[test]
    fn trimming_never_orphans_a_tool_result() {
        let trimmed = trim_history(history(40));
        // Every tool result must have its originating call earlier in the slice.
        for message in trimmed.iter().filter(|m| m.role == Role::Tool) {
            let id = message.tool_call_id.clone().unwrap();
            assert!(
                trimmed
                    .iter()
                    .any(|m| m.tool_calls.iter().any(|c| c.id == id)),
                "orphaned tool result {id}"
            );
        }
    }

    #[test]
    fn the_most_recent_exchange_always_survives() {
        let trimmed = trim_history(history(40));
        assert_eq!(trimmed.last().unwrap().role, Role::Tool);
        assert!(trimmed.iter().any(|m| m.text == "question 39"));
    }

    #[test]
    fn summarize_uses_the_first_meaningful_line() {
        assert_eq!(summarize("\n\n  hello\nworld"), "hello");
        assert_eq!(summarize(""), "");
        assert!(summarize(&"x".repeat(300)).ends_with('…'));
    }
}
