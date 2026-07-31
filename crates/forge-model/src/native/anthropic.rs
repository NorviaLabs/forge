use std::collections::BTreeMap;

use forge_types::{MessageRole, ModelResponse, ModelStreamEvent, ToolCall, Usage};
use futures::StreamExt;
use serde_json::{json, Value};

use super::NativeModelClient;
use crate::{ModelError, ModelRequest, StreamEventTx};

#[derive(Default)]
struct ToolUseAccumulator {
    id: String,
    name: String,
    input: String,
    start_sent: bool,
}

pub(super) async fn complete(
    client: &NativeModelClient,
    req: ModelRequest,
    model: &str,
    tx: Option<StreamEventTx>,
) -> Result<ModelResponse, ModelError> {
    let api_key = client
        .credential(&["ANTHROPIC_API_KEY"])
        .ok_or(ModelError::MissingApiKey)?;
    let base_url = client
        .configured_base_url
        .clone()
        .or_else(|| client.injected_or_env(&["ANTHROPIC_API_BASE"]))
        .unwrap_or_else(|| "https://api.anthropic.com".into());
    let endpoint = if base_url.trim_end_matches('/').ends_with("/v1") {
        format!("{}/messages", base_url.trim_end_matches('/'))
    } else {
        format!("{}/v1/messages", base_url.trim_end_matches('/'))
    };
    let (system, messages) = messages_body(&req);
    let mut body = json!({
        "model": model.trim_start_matches("anthropic/"),
        "max_tokens": 8192,
        "messages": messages,
        "stream": true
    });
    if !system.is_empty() {
        body["system"] = Value::String(system);
    }
    if req.prompt_cache {
        apply_prompt_cache(&mut body);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema
                    })
                })
                .collect(),
        );
    }
    let reasoning_effort = req.reasoning_effort.as_deref();
    apply_reasoning_effort(&mut body, model, reasoning_effort);

    let response = client
        .http
        .post(endpoint)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "interleaved-thinking-2025-05-14")
        .json(&body)
        .send()
        .await
        .map_err(|error| ModelError::Transport(error.to_string()))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let detail = response.text().await.unwrap_or_default();
        return Err(ModelError::ProviderStatus { status, detail });
    }

    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tool_uses = BTreeMap::new();
    let mut prompt_tokens = 0;
    let mut completion_tokens = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ModelError::Transport(error.to_string()))?;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = pending.find('\n') {
            let line = pending[..newline].trim().to_string();
            pending.drain(..=newline);
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data.is_empty() {
                continue;
            }
            let event: Value = serde_json::from_str(data)
                .map_err(|error| ModelError::Protocol(format!("invalid SSE JSON: {error}")))?;
            consume_event(
                &event,
                &mut text,
                &mut thinking,
                &mut tool_uses,
                &mut prompt_tokens,
                &mut completion_tokens,
                tx.as_ref(),
            )?;
        }
    }

    let tool_calls = finalize_tool_uses(tool_uses)?;
    let usage = Usage {
        prompt_tokens,
        completion_tokens,
    };
    if let Some(tx) = tx {
        for call in &tool_calls {
            let _ = tx.send(ModelStreamEvent::ToolCallEnd { call: call.clone() });
        }
        let _ = tx.send(ModelStreamEvent::Usage {
            usage: usage.clone(),
        });
        let _ = tx.send(ModelStreamEvent::MessageEnd);
    }
    Ok(ModelResponse {
        text,
        tool_calls,
        usage: Some(usage),
        thinking: (!thinking.is_empty()).then_some(thinking),
    })
}

fn messages_body(req: &ModelRequest) -> (String, Vec<Value>) {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in &req.messages {
        match message.role {
            MessageRole::System => system.push(message.content.clone()),
            MessageRole::Tool => messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": message.content
                }]
            })),
            MessageRole::User => messages.push(json!({
                "role": "user",
                "content": message.content
            })),
            MessageRole::Assistant => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(json!({"type": "text", "text": message.content}));
                }
                content.extend(message.tool_calls.iter().map(|call| {
                    json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments
                    })
                }));
                messages.push(json!({"role": "assistant", "content": content}));
            }
            // `MessageRole` is `#[non_exhaustive]`. Carry an unrecognised future role
            // through as user content: never fabricate system/assistant authority for a
            // role this transport does not understand, and never silently drop content.
            _ => messages.push(json!({
                "role": "user",
                "content": message.content
            })),
        }
    }
    (system.join("\n\n"), messages)
}

#[allow(clippy::too_many_arguments)]
fn consume_event(
    event: &Value,
    text: &mut String,
    thinking: &mut String,
    tool_uses: &mut BTreeMap<usize, ToolUseAccumulator>,
    prompt_tokens: &mut u32,
    completion_tokens: &mut u32,
    tx: Option<&StreamEventTx>,
) -> Result<(), ModelError> {
    match event.get("type").and_then(Value::as_str).unwrap_or("") {
        "message_start" => {
            *prompt_tokens = event
                .pointer("/message/usage/input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
        }
        "message_delta" => {
            *completion_tokens = event
                .pointer("/usage/output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(*completion_tokens as u64) as u32;
        }
        "content_block_start" => {
            if event.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let id = event
                    .pointer("/content_block/id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into();
                let name: String = event
                    .pointer("/content_block/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into();
                let mut tool_use = ToolUseAccumulator {
                    id,
                    name: name.clone(),
                    input: String::new(),
                    start_sent: false,
                };
                if let Some(tx) = tx {
                    if !tool_use.name.is_empty() {
                        let stream_id = if tool_use.id.is_empty() {
                            format!("tool_{index}")
                        } else {
                            tool_use.id.clone()
                        };
                        let _ = tx.send(ModelStreamEvent::ToolCallStart {
                            id: stream_id,
                            name,
                        });
                        tool_use.start_sent = true;
                    }
                }
                tool_uses.insert(index, tool_use);
            }
        }
        "content_block_delta" => {
            let delta_type = event
                .pointer("/delta/type")
                .and_then(Value::as_str)
                .unwrap_or("");
            let piece = match delta_type {
                "text_delta" => event.pointer("/delta/text").and_then(Value::as_str),
                "thinking_delta" => event.pointer("/delta/thinking").and_then(Value::as_str),
                _ => None,
            };
            if let Some(piece) = piece {
                if delta_type == "text_delta" {
                    text.push_str(piece);
                    if let Some(tx) = tx {
                        let _ = tx.send(ModelStreamEvent::TextDelta { text: piece.into() });
                    }
                } else {
                    thinking.push_str(piece);
                    if let Some(tx) = tx {
                        let _ = tx.send(ModelStreamEvent::ThinkingDelta { text: piece.into() });
                    }
                }
            }
            if delta_type == "input_json_delta" {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(tool_use) = tool_uses.get_mut(&index) {
                    let piece = event
                        .pointer("/delta/partial_json")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    tool_use.input.push_str(piece);
                    if let Some(tx) = tx {
                        if !piece.is_empty() {
                            let id = if tool_use.id.is_empty() {
                                format!("tool_{index}")
                            } else {
                                tool_use.id.clone()
                            };
                            let _ = tx.send(ModelStreamEvent::ToolCallDelta {
                                id,
                                arguments_delta: piece.into(),
                            });
                        }
                    }
                }
            }
        }
        "error" => {
            return Err(ModelError::Provider(
                event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Anthropic stream error")
                    .into(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn finalize_tool_uses(
    tool_uses: BTreeMap<usize, ToolUseAccumulator>,
) -> Result<Vec<ToolCall>, ModelError> {
    tool_uses
        .into_iter()
        .map(|(index, tool_use)| {
            let arguments = if tool_use.input.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&tool_use.input).map_err(|error| {
                    ModelError::Protocol(format!("tool input is not valid JSON: {error}"))
                })?
            };
            Ok(ToolCall {
                id: if tool_use.id.is_empty() {
                    format!("tool_{index}")
                } else {
                    tool_use.id
                },
                name: tool_use.name,
                arguments,
            })
        })
        .collect()
}

fn apply_reasoning_effort(body: &mut Value, model: &str, reasoning_effort: Option<&str>) {
    let Some(raw_effort) = reasoning_effort else {
        return;
    };
    let mut effort = raw_effort.trim().to_ascii_lowercase();
    if effort.is_empty() || effort == "auto" {
        return;
    }
    if effort == "minimal" {
        effort = "low".into();
    }
    let model_id = model.trim_start_matches("anthropic/");
    let supported = [
        "sonnet-5",
        "opus-4-8",
        "opus-4-7",
        "opus-4-6",
        "sonnet-4-6",
        "opus-4-5",
    ]
    .iter()
    .any(|marker| model_id.contains(marker));
    if supported {
        if effort == "xhigh" && (model_id.contains("4-6") || model_id.contains("opus-4-5")) {
            effort = "high".into();
        }
        body["output_config"] = json!({"effort": effort});
    }
}

fn apply_prompt_cache(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(first) = messages.first_mut() {
        if first.get("role").and_then(Value::as_str) == Some("user") {
            if let Some(obj) = first.as_object_mut() {
                obj.insert("cache_control".into(), json!({"type": "ephemeral"}));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::test_support::serve_once;
    use crate::ModelClient;
    use forge_config::Config;
    use forge_types::{Message, ModelStreamEvent, SideEffectClass, ToolDescriptor};

    #[test]
    fn maps_system_tools_and_tool_results() {
        let req = ModelRequest {
            model: "anthropic/claude".into(),
            prompt_cache: true,
            reasoning_effort: None,
            messages: vec![
                Message::new(MessageRole::System, "system"),
                Message::new(MessageRole::User, "hello"),
                Message {
                    role: MessageRole::Assistant,
                    content: String::new(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![ToolCall {
                        id: "tool-1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path": "README.md"}),
                    }],
                },
                Message {
                    role: MessageRole::Tool,
                    content: "contents".into(),
                    tool_call_id: Some("tool-1".into()),
                    name: Some("read_file".into()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                },
            ],
            tools: Vec::<ToolDescriptor>::new(),
        };
        let (system, messages) = messages_body(&req);
        assert_eq!(system, "system");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn accumulates_thinking_and_tool_json() {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut tools = BTreeMap::new();
        let mut input = 0;
        let mut output = 0;
        consume_event(
            &json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"bash"}}),
            &mut text,
            &mut thinking,
            &mut tools,
            &mut input,
            &mut output,
            None,
        )
        .unwrap();
        consume_event(
            &json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}}),
            &mut text,
            &mut thinking,
            &mut tools,
            &mut input,
            &mut output,
            None,
        )
        .unwrap();
        let calls = finalize_tool_uses(tools).unwrap();
        assert_eq!(calls[0].arguments["command"], "ls");
    }

    #[tokio::test]
    async fn completes_anthropic_sse_with_tools_thinking_and_usage() {
        let sse = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"think\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"bash\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"ls\\\"}\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":7}}\n\n"
        );
        let (base_url, request_rx) = serve_once("200 OK", "text/event-stream", sse).await;
        let mut config = Config::default();
        config.model.base_url = Some(base_url);
        config.model.api_key = Some("anthropic-secret".into());
        let client = NativeModelClient::from_config(&config).unwrap();
        let _ = client;
        // Credential resolution consults injected values and the ambient
        // environment before the configured key, so inject explicitly here.
        // Without this the assertion below fails for any developer who has
        // ANTHROPIC_API_KEY exported, because the ambient key is sent instead.
        client.apply_provider_env(&[("ANTHROPIC_API_KEY".into(), "anthropic-secret".into())]);
        let (tx, rx) = std::sync::mpsc::channel();
        let request = ModelRequest {
            model: "anthropic/claude-sonnet-4-6".into(),
            reasoning_effort: Some("high".into()),
            messages: vec![
                Message::new(MessageRole::System, "system"),
                Message::new(MessageRole::User, "hello"),
            ],
            tools: vec![ToolDescriptor {
                name: "bash".into(),
                description: "run".into(),
                input_schema: json!({"type":"object"}),
                side_effect_class: SideEffectClass::Exec,
                idempotent: false,
            }],
            prompt_cache: true,
        };

        let response = client
            .complete_with_stream(request, Some(tx))
            .await
            .unwrap();

        assert_eq!(response.text, "answer");
        assert_eq!(response.thinking.as_deref(), Some("think"));
        assert_eq!(response.tool_calls[0].arguments["command"], "ls");
        assert_eq!(response.usage.unwrap().prompt_tokens, 5);
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(
            events.first(),
            Some(ModelStreamEvent::ThinkingDelta { .. })
        ));
        assert!(matches!(events.last(), Some(ModelStreamEvent::MessageEnd)));

        let raw_request = request_rx.await.unwrap();
        assert!(raw_request.starts_with("POST /v1/messages HTTP/1.1"));
        assert!(raw_request
            .to_ascii_lowercase()
            .contains("x-api-key: anthropic-secret"));
        assert!(raw_request.contains("\"system\":\"system\""));
        assert!(raw_request.contains("\"output_config\":{\"effort\":\"high\"}"));
    }

    #[tokio::test]
    async fn handles_missing_key_http_error_and_stream_error() {
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        // `injected_or_env` discards empty values, so injecting an empty key
        // masks an ambient ANTHROPIC_API_KEY and keeps the MissingApiKey
        // assertion below deterministic.
        client.apply_provider_env(&[("ANTHROPIC_API_KEY".into(), String::new())]);
        let request = ModelRequest {
            model: "anthropic/claude".into(),
            reasoning_effort: None,
            messages: vec![],
            tools: vec![],
            prompt_cache: true,
        };
        assert!(matches!(
            client.complete(request.clone()).await,
            Err(ModelError::MissingApiKey)
        ));

        let (base_url, _) = serve_once("401 Unauthorized", "text/plain", "bad key").await;
        let mut config = Config::default();
        config.model.base_url = Some(format!("{base_url}/v1"));
        config.model.api_key = Some("bad".into());
        let client = NativeModelClient::from_config(&config).unwrap();
        client.apply_provider_env(&[("ANTHROPIC_API_KEY".into(), "bad".into())]);
        assert!(client
            .complete(request)
            .await
            .unwrap_err()
            .to_string()
            .contains("401"));

        let mut text = String::new();
        let mut thinking = String::new();
        let mut tools = BTreeMap::new();
        let mut input = 0;
        let mut output = 0;
        let error = consume_event(
            &json!({"type":"error","error":{"message":"stream failed"}}),
            &mut text,
            &mut thinking,
            &mut tools,
            &mut input,
            &mut output,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("stream failed"));
    }

    #[test]
    fn rejects_invalid_tool_input_and_assigns_fallback_id() {
        let invalid = BTreeMap::from([(
            1,
            ToolUseAccumulator {
                id: String::new(),
                name: "bash".into(),
                input: "invalid".into(),
                start_sent: false,
            },
        )]);
        assert!(finalize_tool_uses(invalid).is_err());
        let empty = BTreeMap::from([(
            2,
            ToolUseAccumulator {
                id: String::new(),
                name: "bash".into(),
                input: String::new(),
                start_sent: false,
            },
        )]);
        let calls = finalize_tool_uses(empty).unwrap();
        assert_eq!(calls[0].id, "tool_2");
        assert_eq!(calls[0].arguments, json!({}));
    }
}
