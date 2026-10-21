//! Provider-agnostic chat + tool-calling.
//!
//! The three implementations differ in wire format but agree on the model
//! above: a flat list of messages where an assistant turn may carry tool calls
//! and a tool turn carries one result. Each provider is responsible for
//! reshaping that into whatever its API expects — Anthropic's content blocks,
//! OpenAI's `tool_calls` array, or Ollama's variant of the latter.

pub mod anthropic;
pub mod ollama;
pub mod openai;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::{LlmProviderKind, LlmSettings};
use crate::error::{AppError, Result};

/// Assistant calls can involve several sequential tool round trips, and some
/// self-hosted models are slow, so this is generous.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A request from the model to run one tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub role: Role,
    /// Prose content. Empty for an assistant turn that only calls tools.
    #[serde(default)]
    pub text: String,
    /// Populated on assistant turns that request tools.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Set on `Role::Tool` messages, matching the originating call.
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    /// True when the tool failed, so the model knows to change approach.
    #[serde(default)]
    pub is_error: bool,
    /// The provider's own record of an assistant turn, sent back verbatim for
    /// the rest of the tool loop that produced it: Anthropic's thinking blocks
    /// must return unchanged and in order. Never stored or shown.
    #[serde(skip)]
    pub replay: Option<serde_json::Value>,
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> Self {
        ChatMessage {
            role: Role::User,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            is_error: false,
            replay: None,
        }
    }

    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        ChatMessage {
            role: Role::Assistant,
            text: text.into(),
            tool_calls,
            tool_call_id: None,
            tool_name: None,
            is_error: false,
            replay: None,
        }
    }

    pub fn tool_result(call: &ToolCall, content: impl Into<String>, is_error: bool) -> Self {
        ChatMessage {
            role: Role::Tool,
            text: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call.id.clone()),
            tool_name: Some(call.name.clone()),
            is_error,
            replay: None,
        }
    }
}

/// A tool as advertised to the model. `input_schema` is JSON Schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    /// See [`ChatMessage::replay`].
    pub replay: Option<serde_json::Value>,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;

    /// Model ids available to this key/endpoint, for the settings dropdown.
    async fn list_models(&self) -> Result<Vec<String>>;

    fn label(&self) -> &'static str;
}

/// Construct the configured provider. `api_key` comes from the keychain and is
/// required for everything except a local Ollama server.
pub fn build_provider(
    settings: &LlmSettings,
    api_key: Option<String>,
) -> Result<Arc<dyn LlmProvider>> {
    if settings.provider.requires_api_key() && api_key.as_deref().unwrap_or("").is_empty() {
        return Err(AppError::Llm(format!(
            "no API key is saved for {}. Add one in Settings.",
            settings.provider.default_base_url()
        )));
    }

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("root/0.1")
        .build()
        .map_err(|e| AppError::Llm(format!("could not build the HTTP client: {e}")))?;

    let base_url = settings.resolved_base_url();
    let key = api_key.unwrap_or_default();

    Ok(match settings.provider {
        LlmProviderKind::Anthropic => {
            Arc::new(anthropic::AnthropicProvider::new(client, base_url, key))
        }
        LlmProviderKind::OpenAiCompatible => {
            Arc::new(openai::OpenAiProvider::new(client, base_url, key))
        }
        LlmProviderKind::Ollama => Arc::new(ollama::OllamaProvider::new(client, base_url)),
    })
}

/// Turn a non-2xx response into an error that names the provider and quotes the
/// body, which is where the real reason (bad key, unknown model, rate limit)
/// almost always is.
pub(crate) async fn error_from_response(
    provider: &str,
    resp: reqwest::Response,
) -> AppError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message").or(Some(e)))
                .map(|m| m.to_string())
                .or_else(|| v.get("message").map(|m| m.to_string()))
        })
        .unwrap_or_else(|| body.chars().take(400).collect());

    AppError::Llm(format!("{provider} returned {status}: {detail}"))
}
