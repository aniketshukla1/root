//! Any OpenAI-compatible `/chat/completions` endpoint.
//!
//! This covers OpenAI itself plus the many gateways and local servers that
//! speak the same protocol — Groq, OpenRouter, Together, DeepSeek, Fireworks,
//! vLLM, LM Studio, llama.cpp. Point `base_url` at the right host and supply
//! that service's key.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{
    error_from_response, ChatMessage, CompletionRequest, CompletionResponse, LlmProvider, Role,
    ToolCall, Usage,
};
use crate::error::{AppError, Result};

pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl OpenAiProvider {
    pub fn new(client: reqwest::Client, base_url: String, api_key: String) -> Self {
        OpenAiProvider {
            client,
            base_url,
            api_key,
        }
    }

    fn build_messages(system: &str, messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        let mut out = vec![json!({ "role": "system", "content": system })];

        for message in messages {
            match message.role {
                Role::System => out.push(json!({ "role": "system", "content": message.text })),
                Role::User => out.push(json!({ "role": "user", "content": message.text })),
                Role::Tool => out.push(json!({
                    "role": "tool",
                    "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": message.text,
                })),
                Role::Assistant => {
                    let mut entry = json!({ "role": "assistant" });
                    // `content` must be present even when the turn is pure
                    // tool calls; null is the accepted spelling for "nothing".
                    entry["content"] = if message.text.trim().is_empty() {
                        serde_json::Value::Null
                    } else {
                        json!(message.text)
                    };
                    if !message.tool_calls.is_empty() {
                        entry["tool_calls"] = json!(message
                            .tool_calls
                            .iter()
                            .map(|call| json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    // Arguments are a JSON *string* here, not
                                    // an object.
                                    "arguments": call.arguments.to_string(),
                                }
                            }))
                            .collect::<Vec<_>>());
                    }
                    out.push(entry);
                }
            }
        }
        out
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<ResponseMessage>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ResponseToolCall>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ResponseFunction>,
}

#[derive(Debug, Deserialize)]
struct ResponseFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
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
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    }
                })
            })
            .collect();

        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature,
            "messages": Self::build_messages(&request.system, &request.messages),
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Llm(format!("could not reach the model endpoint: {e}")))?;

        if !resp.status().is_success() {
            return Err(error_from_response("The model endpoint", resp).await);
        }

        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Llm(format!("unexpected response from the endpoint: {e}")))?;

        let message = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message)
            .ok_or_else(|| AppError::Llm("the endpoint returned no choices".to_string()))?;

        let tool_calls = message
            .tool_calls
            .into_iter()
            .enumerate()
            .map(|(index, call)| {
                let function = call.function.unwrap_or(ResponseFunction {
                    name: None,
                    arguments: None,
                });
                let raw = function.arguments.unwrap_or_else(|| "{}".to_string());
                ToolCall {
                    // Some gateways omit the id; the agent needs one to pair
                    // the result back up, so synthesize a stable fallback.
                    id: call.id.unwrap_or_else(|| format!("call_{index}")),
                    name: function.name.unwrap_or_default(),
                    // Models occasionally emit malformed argument JSON. Passing
                    // the raw text through lets the tool layer report a useful
                    // error instead of the request dying here.
                    arguments: serde_json::from_str(&raw)
                        .unwrap_or_else(|_| json!({ "_raw": raw })),
                }
            })
            .collect();

        Ok(CompletionResponse {
            text: message.content.unwrap_or_default(),
            tool_calls,
            usage: parsed
                .usage
                .map(|u| Usage {
                    input_tokens: u.prompt_tokens,
                    output_tokens: u.completion_tokens,
                })
                .unwrap_or_default(),
        })
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let resp = self
            .client
            .get(format!("{}/models", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(|e| AppError::Llm(format!("could not reach the model endpoint: {e}")))?;

        if !resp.status().is_success() {
            return Err(error_from_response("The model endpoint", resp).await);
        }

        let parsed: ModelsResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Llm(format!("unexpected response from the endpoint: {e}")))?;

        let mut models: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
        models.sort();
        Ok(models)
    }

    fn label(&self) -> &'static str {
        "OpenAI-compatible"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: "search_logs".into(),
            arguments: json!({ "level": "error" }),
        }
    }

    #[test]
    fn serializes_tool_arguments_as_a_json_string() {
        let messages = vec![ChatMessage::assistant("", vec![call()])];
        let built = OpenAiProvider::build_messages("sys", &messages);
        let tool_calls = built[1]["tool_calls"].as_array().unwrap();
        let arguments = tool_calls[0]["function"]["arguments"].as_str().unwrap();
        assert_eq!(arguments, r#"{"level":"error"}"#);
    }

    #[test]
    fn tool_only_assistant_turn_has_null_content() {
        let built = OpenAiProvider::build_messages("sys", &[ChatMessage::assistant("", vec![call()])]);
        assert!(built[1]["content"].is_null());
    }

    #[test]
    fn tool_results_carry_their_call_id() {
        let built = OpenAiProvider::build_messages(
            "sys",
            &[ChatMessage::tool_result(&call(), "3 errors", false)],
        );
        assert_eq!(built[1]["role"], "tool");
        assert_eq!(built[1]["tool_call_id"], "c1");
    }

    #[test]
    fn system_prompt_leads_the_message_list() {
        let built = OpenAiProvider::build_messages("you are ops", &[]);
        assert_eq!(built[0]["role"], "system");
        assert_eq!(built[0]["content"], "you are ops");
    }
}
