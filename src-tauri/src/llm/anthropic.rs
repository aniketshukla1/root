//! Anthropic Messages API.
//!
//! The shape difference that matters: Anthropic has no `tool` role. A tool
//! result is a `tool_result` content block inside a *user* message, and
//! consecutive results must be merged into a single message.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{
    error_from_response, ChatMessage, CompletionRequest, CompletionResponse, LlmProvider, Role,
    ToolCall, Usage,
};
use crate::error::{AppError, Result};

const API_VERSION: &str = "2023-06-01";

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

    /// Fold our flat message list into Anthropic's alternating user/assistant
    /// structure with typed content blocks.
    fn build_messages(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        let mut out: Vec<serde_json::Value> = Vec::new();
        // Tool results accumulate here until a non-tool message flushes them.
        let mut pending_results: Vec<serde_json::Value> = Vec::new();

        let flush = |pending: &mut Vec<serde_json::Value>, out: &mut Vec<serde_json::Value>| {
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
    content: Vec<ContentBlock>,
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
        input: serde_json::Value,
    },
    /// Extended-thinking and any future block types are ignored rather than
    /// failing the whole response.
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

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let tools: Vec<serde_json::Value> = request
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

        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature,
            "system": request.system,
            "messages": Self::build_messages(&request.messages),
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        let resp = self
            .headers(self.client.post(format!("{}/v1/messages", self.base_url)))
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

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for block in parsed.content {
            match block {
                ContentBlock::Text { text: chunk } => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&chunk);
                }
                ContentBlock::ToolUse { id, name, input } => tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: input,
                }),
                ContentBlock::Other => {}
            }
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
        })
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
}
