use std::collections::BTreeMap;

use forge_types::{MessageRole, ModelResponse, ModelStreamEvent, ToolCall, Usage};
use futures::StreamExt;
use serde_json::{json, Value};

use super::{process_sse_lines, NativeModelClient};
use crate::prompt_cache::{apply_anthropic_prompt_cache, usage_from_provider, InputTokens};
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
    let (system, messages) = messages_body(&req, Some(client));
    let mut body = json!({
        "model": model.trim_start_matches("anthropic/"),
        "max_tokens": 8192,
        "messages": messages,
        "stream": true
    });
    if !system.is_empty() {
        body["system"] = Value::String(system);
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
    if req.prompt_cache {
        apply_anthropic_prompt_cache(&mut body);
    }
    let reasoning_effort = req.reasoning_effort.as_deref();
    apply_reasoning_settings(&mut body, model, reasoning_effort, req.thinking_enabled);

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
    let mut prompt_tokens = 0_u32;
    let mut prompt_cache_read_tokens = 0_u32;
    let mut prompt_cache_write_tokens = 0_u32;
    let mut completion_tokens = 0_u32;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ModelError::Transport(error.to_string()))?;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        process_sse_lines(&mut pending, |line| {
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                return Ok(());
            };
            if data.is_empty() {
                return Ok(());
            }
            let event: Value = serde_json::from_str(data)
                .map_err(|error| ModelError::Protocol(format!("invalid SSE JSON: {error}")))?;
            consume_event(
                &event,
                &mut text,
                &mut thinking,
                &mut tool_uses,
                &mut prompt_tokens,
                &mut prompt_cache_read_tokens,
                &mut prompt_cache_write_tokens,
                &mut completion_tokens,
                tx.as_ref(),
            )?;
            Ok(())
        })?;
    }

    let tool_calls = finalize_tool_uses(tool_uses)?;
    let usage = Usage {
        prompt_tokens,
        completion_tokens,
        prompt_cache_read_tokens,
        prompt_cache_write_tokens,
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

fn anthropic_payload_content(
    message: &forge_types::Message,
    workspace: &std::path::Path,
    client: Option<&NativeModelClient>,
) -> Value {
    let mut parts = Vec::new();
    if !message.content.is_empty() {
        parts.push(json!({"type": "text", "text": message.content}));
    }
    for image in &message.attachments {
        let loaded = client.map_or_else(
            || crate::image::load_image_ref(workspace, image).map(std::sync::Arc::new),
            |client| client.load_image_ref(workspace, image),
        );
        match loaded {
            Ok(loaded) => parts.push(crate::image::anthropic_image_part_with_loaded(&loaded)),
            Err(_) => parts.push(json!({
                "type": "text",
                "text": format!("image at `{}` is no longer available", image.path)
            })),
        }
    }
    if parts.is_empty() {
        parts.push(json!({"type": "text", "text": message.content}));
    }
    Value::Array(parts)
}

pub(super) fn messages_body(
    req: &ModelRequest,
    client: Option<&NativeModelClient>,
) -> (String, Vec<Value>) {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in req.messages.iter() {
        match message.role {
            MessageRole::System => system.push(message.content.clone()),
            MessageRole::Tool => messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": anthropic_payload_content(message, &req.workspace_root, client)
                }]
            })),
            MessageRole::User => messages.push(json!({
                "role": "user",
                "content": anthropic_payload_content(message, &req.workspace_root, client)
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
    prompt_cache_read_tokens: &mut u32,
    prompt_cache_write_tokens: &mut u32,
    completion_tokens: &mut u32,
    tx: Option<&StreamEventTx>,
) -> Result<(), ModelError> {
    match event.get("type").and_then(Value::as_str).unwrap_or("") {
        "message_start" => {
            if let Some(raw_usage) = event.pointer("/message/usage") {
                let parsed = usage_from_provider(
                    raw_usage,
                    raw_usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                    *completion_tokens,
                    InputTokens::UncachedOnly,
                );
                *prompt_tokens = parsed.prompt_tokens;
                *prompt_cache_read_tokens = parsed.prompt_cache_read_tokens;
                *prompt_cache_write_tokens = parsed.prompt_cache_write_tokens;
            }
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

fn apply_reasoning_settings(
    body: &mut Value,
    model: &str,
    reasoning_effort: Option<&str>,
    thinking_enabled: bool,
) {
    if !thinking_enabled {
        body["thinking"] = json!({"type": "disabled"});
    }
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
    let supported = catalog_supports_anthropic_effort(model).unwrap_or_else(|| {
        [
            "sonnet-5",
            "opus-4-8",
            "opus-4-7",
            "opus-4-6",
            "sonnet-4-6",
            "opus-4-5",
        ]
        .iter()
        .any(|marker| model_id.contains(marker))
    });
    if supported {
        if effort == "xhigh" && (model_id.contains("4-6") || model_id.contains("opus-4-5")) {
            effort = "high".into();
        }
        body["output_config"] = json!({"effort": effort});
    }
}

fn catalog_supports_anthropic_effort(model: &str) -> Option<bool> {
    forge_connect::ModelCatalogCache::user_default()
        .model_effort_options(model)
        .map(|options| !options.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::test_support::serve_once;
    use crate::ModelClient;
    use forge_config::Config;
    use forge_types::{Message, ModelStreamEvent, SideEffectClass, ToolDescriptor};

    #[test]
    fn thinking_off_sends_disabled_mode() {
        let mut body = json!({});
        apply_reasoning_settings(
            &mut body,
            "anthropic/claude-sonnet-4-6",
            Some("high"),
            false,
        );
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["output_config"]["effort"], "high");
    }

    #[test]
    fn user_and_tool_image_refs_become_anthropic_image_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), forge_types::sample_png_bytes()).unwrap();
        let req = ModelRequest {
            workspace_root: dir.path().to_path_buf(),
            model: "anthropic/claude".into(),
            route_id: None,
            prompt_cache: true,
            reasoning_effort: None,
            thinking_enabled: true,
            messages: vec![
                Message::new(MessageRole::User, "compare")
                    .with_attachments(vec![forge_types::ImageRef::new("shot.png", "image/png", 1)]),
                Message::from_tool_output(
                    &forge_types::ToolCall {
                        id: "v1".into(),
                        name: "view_image".into(),
                        arguments: serde_json::json!({"path": "shot.png"}),
                    },
                    &forge_types::ToolOutput {
                        content: "image loaded · 1 KB · shot.png".into(),
                        is_error: false,
                        exit_code: None,
                        outcome: Some(forge_types::ExecutionOutcome::Success),
                        attachments: vec![forge_types::ImageRef::new("shot.png", "image/png", 1)],
                    },
                ),
            ]
            .into(),
            tools: vec![],
        };
        let (_system, messages) = messages_body(&req, None);
        let user = &messages[0]["content"];
        assert_eq!(user[0]["type"], "text");
        assert_eq!(user[1]["type"], "image");
        assert_eq!(user[1]["source"]["media_type"], "image/png");
        assert!(user[1]["source"]["data"].as_str().unwrap().len() > 8);
        let tool = &messages[1]["content"][0];
        assert_eq!(tool["type"], "tool_result");
        assert_eq!(tool["content"][1]["type"], "image");
        let dumped = serde_json::to_string(&messages).unwrap();
        assert!(!dumped.contains("data:image"), "{dumped}");
    }

    #[test]
    fn maps_system_tools_and_tool_results() {
        let req = ModelRequest {
            workspace_root: std::path::PathBuf::new(),
            model: "anthropic/claude".into(),
            route_id: None,
            prompt_cache: true,
            reasoning_effort: None,
            thinking_enabled: true,
            messages: vec![
                Message::new(MessageRole::System, "system"),
                Message::new(MessageRole::User, "hello"),
                Message {
                    outcome: Default::default(),
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
                    attachments: Vec::new(),
                },
                Message {
                    outcome: Default::default(),
                    role: MessageRole::Tool,
                    content: "contents".into(),
                    tool_call_id: Some("tool-1".into()),
                    name: Some("read_file".into()),
                    thinking: None,
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                    attachments: Vec::new(),
                },
            ]
            .into(),
            tools: Vec::<ToolDescriptor>::new(),
        };
        let (system, messages) = messages_body(&req, None);
        assert_eq!(system, "system");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn messages_body_omits_assistant_thinking() {
        let req = ModelRequest {
            workspace_root: std::path::PathBuf::new(),
            model: "anthropic/claude".into(),
            route_id: None,
            prompt_cache: true,
            reasoning_effort: None,
            thinking_enabled: true,
            messages: vec![
                Message::new(MessageRole::User, "hello"),
                Message {
                    outcome: Default::default(),
                    role: MessageRole::Assistant,
                    content: "visible".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: Some("secret thoughts".into()),
                    thinking_duration_secs: None,
                    tool_calls: vec![],
                    attachments: Vec::new(),
                },
            ]
            .into(),
            tools: Vec::<ToolDescriptor>::new(),
        };
        let (_system, messages) = messages_body(&req, None);
        let dumped = serde_json::to_string(&messages).unwrap();
        assert!(dumped.contains("visible"), "{dumped}");
        assert!(
            !dumped.contains("secret thoughts"),
            "thinking must not be replayed on the Anthropic wire: {dumped}"
        );
        assert_eq!(messages[1]["content"][0]["type"], "text");
        assert_eq!(messages[1]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn message_start_records_cache_usage() {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut tools = BTreeMap::new();
        let mut input = 0;
        let mut cache_read = 0;
        let mut cache_write = 0;
        let mut output = 0;
        consume_event(
            &json!({"type":"message_start","message":{"usage":{"input_tokens":120,"cache_creation_input_tokens":40,"cache_read_input_tokens":80}}}),
            &mut text,
            &mut thinking,
            &mut tools,
            &mut input,
            &mut cache_read,
            &mut cache_write,
            &mut output,
            None,
        )
        .unwrap();
        assert_eq!(
            input, 240,
            "Anthropic's input_tokens is the uncached remainder (120); the \
             recorded prompt total must add the 80 read and 40 written back, \
             so the cache ratio means the same here as on a provider that \
             reports a total already"
        );
        assert_eq!(cache_read, 80);
        assert_eq!(cache_write, 40);
    }

    #[test]
    fn accumulates_thinking_and_tool_json() {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut tools = BTreeMap::new();
        let mut input = 0;
        let mut cache_read = 0;
        let mut cache_write = 0;
        let mut output = 0;
        consume_event(
            &json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"bash"}}),
            &mut text,
            &mut thinking,
            &mut tools,
            &mut input,
            &mut cache_read,
            &mut cache_write,
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
            &mut cache_read,
            &mut cache_write,
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
        let Some((base_url, request_rx)) = serve_once("200 OK", "text/event-stream", sse).await
        else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
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
            workspace_root: std::path::PathBuf::new(),
            model: "anthropic/claude-sonnet-4-6".into(),
            route_id: Some("anthropic-api".into()),
            reasoning_effort: Some("high".into()),
            thinking_enabled: true,
            messages: vec![
                Message::new(MessageRole::System, "system"),
                Message::new(MessageRole::User, "hello"),
            ]
            .into(),
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
        assert!(raw_request.contains("\"text\":\"system\""));
        assert!(raw_request.contains("\"cache_control\":{\"type\":\"ephemeral\"}"));
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
            workspace_root: std::path::PathBuf::new(),
            model: "anthropic/claude".into(),
            route_id: Some("anthropic-api".into()),
            reasoning_effort: None,
            thinking_enabled: true,
            messages: vec![].into(),
            tools: vec![],
            prompt_cache: true,
        };
        assert!(matches!(
            client.complete(request.clone()).await,
            Err(ModelError::MissingApiKey)
        ));

        let Some((base_url, _)) = serve_once("401 Unauthorized", "text/plain", "bad key").await
        else {
            eprintln!("skipping: this host denies binding a mock listener");
            return;
        };
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
        let mut cache_read = 0;
        let mut cache_write = 0;
        let mut output = 0;
        let error = consume_event(
            &json!({"type":"error","error":{"message":"stream failed"}}),
            &mut text,
            &mut thinking,
            &mut tools,
            &mut input,
            &mut cache_read,
            &mut cache_write,
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
