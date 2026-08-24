//! Local Ollama server.
//!
//! Close to the OpenAI shape with two differences worth knowing: tool call
//! arguments arrive as a JSON *object* rather than a string, and tool calls
//! carry no id, so ids are synthesized here to keep the agent's
//! call/result pairing intact.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{
    error_from_response, ChatMessage, CompletionRequest, CompletionResponse, LlmProvider, Role,
    ToolCall, Usage,
};
use crate::error::{AppError, Result};

pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
}

impl OllamaProvider {
    pub fn new(client: reqwest::Client, base_url: String) -> Self {
        OllamaProvider { client, base_url }
    }

    fn build_messages(system: &str, messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        let mut out = vec![json!({ "role": "system", "content": system })];

        for message in messages {
            match message.role {
                Role::System => out.push(json!({ "role": "system", "content": message.text })),
                Role::User => out.push(json!({ "role": "user", "content": message.text })),
                Role::Tool => out.push(json!({
                    "role": "tool",
                    // Ollama keys tool results by name, not by call id.
                    "tool_name": message.tool_name.clone().unwrap_or_default(),
                    "content": message.text,
                })),
                Role::Assistant => {
                    let mut entry = json!({
                        "role": "assistant",
                        "content": message.text,
                    });
                    if !message.tool_calls.is_empty() {
                        entry["tool_calls"] = json!(message
                            .tool_calls
                            .iter()
                            .map(|call| json!({
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments,
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
    message: Option<ResponseMessage>,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<ResponseToolCall>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    #[serde(default)]
    function: Option<ResponseFunction>,
}

#[derive(Debug, Deserialize)]
struct ResponseFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Debug, Deserialize)]
struct TagEntry {
    #[serde(default)]
    name: String,
}

#[async_trait]
impl LlmProvider for OllamaProvider {
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
            "messages": Self::build_messages(&request.system, &request.messages),
            "stream": false,
            "options": {
                "temperature": request.temperature,
                "num_predict": request.max_tokens,
            },
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }

        let resp = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                AppError::Llm(format!(
                    "could not reach Ollama at {}: {e}. Is `ollama serve` running?",
                    self.base_url
                ))
            })?;

        if !resp.status().is_success() {
            return Err(error_from_response("Ollama", resp).await);
        }

        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Llm(format!("unexpected Ollama response: {e}")))?;

        // Ollama can answer 200 with an error body, e.g. for a model that has
        // not been pulled yet.
        if let Some(error) = parsed.error.filter(|e| !e.is_empty()) {
            return Err(AppError::Llm(format!("Ollama: {error}")));
        }

        let message = parsed.message.unwrap_or(ResponseMessage {
            content: String::new(),
            tool_calls: Vec::new(),
        });

        let tool_calls = message
            .tool_calls
            .into_iter()
            .enumerate()
            .filter_map(|(index, call)| {
                let function = call.function?;
                Some(ToolCall {
                    id: format!("ollama_call_{index}"),
                    name: function.name,
                    arguments: function.arguments,
                })
            })
            .collect();

        Ok(CompletionResponse {
            text: message.content,
            tool_calls,
            usage: Usage {
                input_tokens: parsed.prompt_eval_count,
                output_tokens: parsed.eval_count,
            },
        })
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let resp = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(|e| {
                AppError::Llm(format!(
                    "could not reach Ollama at {}: {e}. Is `ollama serve` running?",
                    self.base_url
                ))
            })?;

        if !resp.status().is_success() {
            return Err(error_from_response("Ollama", resp).await);
        }

        let parsed: TagsResponse = resp
            .json()
            .await
            .map_err(|e| AppError::Llm(format!("unexpected Ollama response: {e}")))?;

        let mut models: Vec<String> = parsed.models.into_iter().map(|m| m.name).collect();
        models.sort();
        Ok(models)
    }

    fn label(&self) -> &'static str {
        "Ollama"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_arguments_stay_an_object() {
        let call = ToolCall {
            id: "x".into(),
            name: "list_alerts".into(),
            arguments: json!({ "activeOnly": true }),
        };
        let built = OllamaProvider::build_messages("sys", &[ChatMessage::assistant("", vec![call])]);
        let arguments = &built[1]["tool_calls"][0]["function"]["arguments"];
        assert!(arguments.is_object(), "Ollama takes objects, not strings");
        assert_eq!(arguments["activeOnly"], true);
    }

    #[test]
    fn tool_results_are_keyed_by_name() {
        let call = ToolCall {
            id: "x".into(),
            name: "list_alerts".into(),
            arguments: json!({}),
        };
        let built = OllamaProvider::build_messages(
            "sys",
            &[ChatMessage::tool_result(&call, "none active", false)],
        );
        assert_eq!(built[1]["tool_name"], "list_alerts");
    }
}
