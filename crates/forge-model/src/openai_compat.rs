use async_trait::async_trait;
use forge_types::{Message, MessageRole, ModelResponse, ToolCall, Usage};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ModelClient, ModelError, ModelRequest};

pub struct OpenAiCompatibleClient {
    base_url: String,
    api_key: String,
    default_model: String,
    http: reqwest::Client,
}

impl OpenAiCompatibleClient {
    pub fn new(base_url: String, api_key: String, default_model: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            default_model,
            http: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct Choice {
    message: RespMessage,
}

#[derive(Deserialize)]
struct RespMessage {
    content: Option<String>,
    tool_calls: Option<Vec<RawToolCall>>,
}

#[derive(Deserialize)]
struct RawToolCall {
    id: String,
    function: RawFunction,
}

#[derive(Deserialize)]
struct RawFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct RawUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[async_trait]
impl ModelClient for OpenAiCompatibleClient {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
        let model = if req.model.is_empty() {
            self.default_model.clone()
        } else {
            req.model
        };

        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();

        let messages: Vec<ChatMessage> = req.messages.iter().map(to_chat_message).collect();

        let body = ChatRequest {
            model,
            messages,
            tools,
        };

        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ModelError::Provider(format!("{status}: {text}")));
        }

        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| ModelError::Provider(e.to_string()))?;

        let msg = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or_else(|| ModelError::Provider("empty choices".into()))?;

        let mut tool_calls = Vec::new();
        if let Some(tcs) = msg.tool_calls {
            for tc in tcs {
                let arguments = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| json!({}));
                tool_calls.push(ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments,
                });
            }
        }

        Ok(ModelResponse {
            text: msg.content.unwrap_or_default(),
            tool_calls,
            usage: parsed.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
            }),
        })
    }
}

fn to_chat_message(m: &Message) -> ChatMessage {
    let role = match m.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };
    ChatMessage {
        role: role.into(),
        content: json!(m.content),
        tool_call_id: m.tool_call_id.clone(),
        name: m.name.clone(),
        tool_calls: None,
    }
}
