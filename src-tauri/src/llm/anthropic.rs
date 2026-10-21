//! Anthropic Messages API.
//!
//! The shape difference that matters: Anthropic has no `tool` role. A tool
//! result is a `tool_result` content block inside a *user* message, and
//! consecutive results must be merged into a single message.
//!
//! Two request shapes. Claude 3.x and 4.0–4.6 take `temperature` and think
//! only when asked, so they get the request they always got. Every model since
//! rejects sampling parameters and thinks on every turn — Claude Opus 5.5
//! cannot be told not to — so those get adaptive thinking at an explicit
//! effort, room in `max_tokens` for the thinking, and their thinking blocks
//! back, unchanged and in order, for the rest of the tool loop that made them.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    error_from_response, ChatMessage, CompletionRequest, CompletionResponse, LlmProvider, Role,
    ToolCall, Usage,
};
use crate::error::{AppError, Result};

const API_VERSION: &str = "2023-06-01";

/// Thinking counts toward `max_tokens`, and callers size `max_tokens` for the
/// reply they want to read.
const THINKING_ROOM: u32 = 8_000;

/// Claude Opus 5.5's own default, set rather than assumed: it is a level below
/// Claude Opus 5's, and the levels do not mean the same thing across models.
const EFFORT: &str = "medium";

/// Models that take server-side refusal fallbacks in their `"default"` form.
const FALLBACK_MODELS: [&str; 4] = ["claude-opus-5-5", "claude-opus-5", "claude-fable-5-1", "claude-sonnet-5-5"];

/// Claude 3.x and 4.0–4.6: these take `temperature`, and think only when asked.
fn takes_sampling(model: &str) -> bool {
    model.starts_with("claude-3")
        || ["claude-opus-4", "claude-sonnet-4", "claude-haiku-4"].iter().any(|family| {
            model.strip_prefix(family).is_some_and(|rest| !rest.starts_with("-7") && !rest.starts_with("-8"))
        })
}

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl AnthropicProvider {
    pub fn new(client: reqwest::Client, base_url: String, api_key: String) -> Self {
        AnthropicProvider {
            client,
            base_url,
            api_key,
        }
    }

    fn headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
    }

    /// The request body, and the beta features it uses.
    fn request(&self, request: &CompletionRequest) -> (Value, Vec<&'static str>) {
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "system": request.system,
            "messages": Self::build_messages(&request.messages),
        });
        let mut betas = Vec::new();
        if takes_sampling(&request.model) {
            body["temperature"] = json!(request.temperature);
        } else {
            body["max_tokens"] = json!(request.max_tokens.saturating_add(THINKING_ROOM));
            // A replayed block whose conversation changed is dropped, not a
            // failed answer. The history is append-only within a tool loop,
            // so this only ever catches a slip.
            body["thinking"] = json!({
                "type": "adaptive",
                "block_binding": { "prefix_mismatch_behavior": "drop_block" },
            });
            betas.push("thinking-binding-controls-2026-08-01");
            body["output_config"] = json!({ "effort": EFFORT });
            // A safeguard's false positive is answered by another model
            // rather than not at all. Claude API only.
            if FALLBACK_MODELS.contains(&request.model.as_str())
                && self.base_url.trim_end_matches('/') == "https://api.anthropic.com"
            {
                body["fallbacks"] = json!("default");
                betas.push("server-side-fallback-2026-07-01");
            }
        }
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect();
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        (body, betas)
    }

    /// Fold our flat message list into Anthropic's alternating user/assistant
    /// structure with typed content blocks.
    fn build_messages(messages: &[ChatMessage]) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        // Tool results accumulate here until a non-tool message flushes them.
        let mut pending_results: Vec<Value> = Vec::new();

        let flush = |pending: &mut Vec<Value>, out: &mut Vec<Value>| {
            if !pending.is_empty() {
                out.push(json!({ "role": "user", "content": std::mem::take(pending) }));
            }
        };

        for message in messages {
            match message.role {
                Role::Tool => {
                    pending_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                        "content": message.text,
                        "is_error": message.is_error,
                    }));
                }
                Role::User => {
                    flush(&mut pending_results, &mut out);
                    out.push(json!({ "role": "user", "content": message.text }));
                }
                Role::Assistant => {
                    flush(&mut pending_results, &mut out);
                    // Within a tool loop the turn goes back exactly as it came,
                    // thinking blocks included.
                    if let Some(Value::Array(blocks)) = &message.replay {
                        if !blocks.is_empty() {
                            out.push(json!({ "role": "assistant", "content": blocks }));
                            continue;
                        }
                    }
                    let mut blocks = Vec::new();
                    if !message.text.trim().is_empty() {
                        blocks.push(json!({ "type": "text", "text": message.text }));
                    }
                    for call in &message.tool_calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": call.arguments,
                        }));
                    }
                    // An assistant turn must never be empty.
                    if blocks.is_empty() {
                        blocks.push(json!({ "type": "text", "text": "(no output)" }));
                    }
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
                // A system prompt travels in its own top-level field.
                Role::System => {}
            }
        }

        flush(&mut pending_results, &mut out);
        out
    }
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<Value>,
    #[serde(default)]
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// Thinking, a fallback's switch point, and any future block type: kept
    /// for the replay, not read.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

/// The answer in a response, checked before it is read: a refusal is a 200
/// whose content is empty or a partial to be discarded.
fn read(parsed: MessagesResponse) -> Result<CompletionResponse> {
    if parsed.stop_reason.as_deref() == Some("refusal") {
        // Branch on the stop reason; the category is informational, and may be absent.
        let category = parsed.stop_details.as_ref().and_then(|d| d.get("category")).and_then(Value::as_str);
        return Err(AppError::Llm(match category {
            Some(category) => format!("Claude declined this request (its {category} safeguard flagged it)."),
            None => "Claude declined this request.".to_string(),
        }));
    }

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in &parsed.content {
        match ContentBlock::deserialize(block) {
            Ok(ContentBlock::Text { text: chunk }) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&chunk);
            }
            Ok(ContentBlock::ToolUse { id, name, input }) => tool_calls.push(ToolCall {
                id,
                name,
                arguments: input,
            }),
            _ => {}
        }
    }

    if parsed.stop_reason.as_deref() == Some("max_tokens") && text.trim().is_empty() && tool_calls.is_empty() {
        return Err(AppError::Llm(
            "Claude spent its whole token allowance before answering. Raise the assistant's max tokens in Settings."
                .into(),
        ));
    }

    Ok(CompletionResponse {
        text,
        tool_calls,
        usage: parsed
            .usage
            .map(|u| Usage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
            })
            .unwrap_or_default(),
        replay: (!parsed.content.is_empty()).then_some(Value::Array(parsed.content)),
    })
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let (body, betas) = self.request(&request);
        let mut req = self.headers(self.client.post(format!("{}/v1/messages", self.base_url)));
        if !betas.is_empty() {
            req = req.header("anthropic-beta", betas.join(","));
        }

        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Llm(format!("could not reach Anthropic: {e}")))?;

        if !resp.status().is_success() {
            return Err(error_from_response("Anthropic", resp).await);
        }

        let parsed: MessagesResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Llm(format!("unexpected Anthropic response: {e}")))?;
        read(parsed)
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let resp = self
            .headers(self.client.get(format!("{}/v1/models?limit=100", self.base_url)))
            .send()
            .await
            .map_err(|e| AppError::Llm(format!("could not reach Anthropic: {e}")))?;

        if !resp.status().is_success() {
            return Err(error_from_response("Anthropic", resp).await);
        }

        let parsed: ModelsResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Llm(format!("unexpected Anthropic response: {e}")))?;
        Ok(parsed.data.into_iter().map(|m| m.id).collect())
    }

    fn label(&self) -> &'static str {
        "Anthropic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolCall;

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "get_health".into(),
            arguments: json!({ "profile": "docker" }),
        }
    }

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new(reqwest::Client::new(), "https://api.anthropic.com".into(), "test".into())
    }

    fn ask(model: &str) -> CompletionRequest {
        CompletionRequest {
            model: model.into(),
            system: "You watch infrastructure.".into(),
            messages: vec![ChatMessage::user("what is broken?")],
            tools: Vec::new(),
            max_tokens: 1400,
            temperature: 0.2,
        }
    }

    #[test]
    fn merges_consecutive_tool_results_into_one_user_message() {
        let messages = vec![
            ChatMessage::user("what is broken?"),
            ChatMessage::assistant("", vec![call("a"), call("b")]),
            ChatMessage::tool_result(&call("a"), "ok", false),
            ChatMessage::tool_result(&call("b"), "boom", true),
        ];

        let built = AnthropicProvider::build_messages(&messages);

        assert_eq!(built.len(), 3);
        assert_eq!(built[2]["role"], "user");
        let blocks = built[2]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2, "both results belong to one user message");
        assert_eq!(blocks[0]["tool_use_id"], "a");
        assert_eq!(blocks[1]["is_error"], true);
    }

    #[test]
    fn assistant_tool_calls_become_tool_use_blocks() {
        let messages = vec![ChatMessage::assistant("checking", vec![call("x")])];
        let built = AnthropicProvider::build_messages(&messages);
        let blocks = built[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "x");
    }

    #[test]
    fn never_emits_an_empty_assistant_turn() {
        let messages = vec![ChatMessage::assistant("", vec![])];
        let built = AnthropicProvider::build_messages(&messages);
        assert!(!built[0]["content"].as_array().unwrap().is_empty());
    }

    #[test]
    fn sampling_goes_only_to_the_models_that_take_it() {
        for old in ["claude-3-5-sonnet-20241022", "claude-sonnet-4-20250514", "claude-opus-4-1-20250805", "claude-sonnet-4-5", "claude-haiku-4-5", "claude-opus-4-6"] {
            assert!(takes_sampling(old), "{old}");
        }
        for current in ["claude-opus-4-7", "claude-opus-4-8", "claude-opus-5", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"] {
            assert!(!takes_sampling(current), "{current}");
        }
    }

    #[test]
    fn opus_5_5_thinks_at_an_explicit_effort_with_room_for_it_and_a_fallback() {
        let (body, betas) = provider().request(&ask("claude-opus-5-5"));
        assert!(body.get("temperature").is_none(), "sampling parameters are a 400 on this model");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["block_binding"]["prefix_mismatch_behavior"], "drop_block");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["max_tokens"], 1400 + THINKING_ROOM);
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(betas, vec!["thinking-binding-controls-2026-08-01", "server-side-fallback-2026-07-01"]);

        let elsewhere = AnthropicProvider::new(reqwest::Client::new(), "https://gateway.example.com".into(), "test".into());
        let (body, betas) = elsewhere.request(&ask("claude-opus-5-5"));
        assert!(body.get("fallbacks").is_none(), "fallbacks are a Claude API feature");
        assert_eq!(betas, vec!["thinking-binding-controls-2026-08-01"]);
    }

    #[test]
    fn an_older_model_gets_the_request_it_always_got() {
        let (body, betas) = provider().request(&ask("claude-sonnet-4-5"));
        assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(body["max_tokens"], 1400);
        assert!(body.get("thinking").is_none() && body.get("output_config").is_none() && body.get("fallbacks").is_none());
        assert!(betas.is_empty());
    }

    #[test]
    fn a_turn_goes_back_exactly_as_it_came_thinking_included() {
        let came: MessagesResponse = serde_json::from_value(json!({
            "content": [
                { "type": "thinking", "thinking": "", "signature": "sig-1" },
                { "type": "tool_use", "id": "t1", "name": "get_health", "input": { "profile": "docker" } }
            ],
            "stop_reason": "tool_use"
        }))
        .unwrap();
        let read = read(came).unwrap();
        assert_eq!(read.tool_calls.len(), 1);
        assert!(read.text.is_empty());

        let turn = ChatMessage { replay: read.replay.clone(), ..ChatMessage::assistant(read.text, read.tool_calls) };
        let built = AnthropicProvider::build_messages(&[ChatMessage::user("what is broken?"), turn]);
        assert_eq!(built[1]["content"], read.replay.unwrap(), "unchanged and in order");
        assert_eq!(built[1]["content"][0]["signature"], "sig-1");
    }

    #[test]
    fn a_refusal_is_reported_and_its_partial_is_not_read_as_an_answer() {
        let declined: MessagesResponse = serde_json::from_value(json!({
            "content": [{ "type": "text", "text": "Here is how to" }],
            "stop_reason": "refusal",
            "stop_details": { "type": "refusal", "category": "bio", "explanation": null }
        }))
        .unwrap();
        let error = read(declined).unwrap_err().to_string();
        assert!(error.contains("declined") && error.contains("bio"), "{error}");
    }
}
