use std::collections::BTreeMap;

use forge_types::{ModelResponse, ModelStreamEvent, ToolCall, Usage};
use futures::StreamExt;
use serde_json::{json, Value};

use super::{process_sse_lines, NativeModelClient};
use crate::normalize::tools_to_openai_functions;
use crate::prompt_cache::{apply_openai_prompt_cache, usage_from_provider};
use crate::{ModelError, ModelRequest, StreamEventTx};

struct Route {
    base_url: String,
    api_key: Option<String>,
    model: String,
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
    start_sent: bool,
}

pub(super) async fn complete(
    client: &NativeModelClient,
    req: ModelRequest,
    model: &str,
    tx: Option<StreamEventTx>,
) -> Result<ModelResponse, ModelError> {
    let route = route(client, model)?;
    let mut body = json!({
        "model": route.model,
        "messages": crate::normalize::forge_messages_to_wire_in(&req.messages, &req.workspace_root),
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    let tools = tools_to_openai_functions(&req.tools);
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if req.prompt_cache {
        apply_openai_prompt_cache(&mut body);
    }
    apply_reasoning_effort(&mut body, model, req.reasoning_effort.as_deref());

    let url = format!("{}/chat/completions", route.base_url.trim_end_matches('/'));
    let mut request = client.http.post(url).json(&body);
    if let Some(api_key) = route.api_key {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| ModelError::Transport(error.to_string()))?;
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }

    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tool_calls = BTreeMap::new();
    let mut usage = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ModelError::Transport(error.to_string()))?;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        process_sse_lines(&mut pending, |line| {
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                return Ok(());
            };
            if data.is_empty() || data == "[DONE]" {
                return Ok(());
            }
            let event: Value = serde_json::from_str(data)
                .map_err(|error| ModelError::Protocol(format!("invalid SSE JSON: {error}")))?;
            consume_event(
                &event,
                &mut text,
                &mut thinking,
                &mut tool_calls,
                &mut usage,
                tx.as_ref(),
            );
            Ok(())
        })?;
    }

    let tool_calls = finalize_tool_calls(tool_calls)?;
    if let Some(tx) = tx {
        for call in &tool_calls {
            let _ = tx.send(ModelStreamEvent::ToolCallEnd { call: call.clone() });
        }
        if let Some(ref usage) = usage {
            let _ = tx.send(ModelStreamEvent::Usage {
                usage: usage.clone(),
            });
        }
        let _ = tx.send(ModelStreamEvent::MessageEnd);
    }
    Ok(ModelResponse {
        text,
        tool_calls,
        usage,
        thinking: (!thinking.is_empty()).then_some(thinking),
    })
}

fn route(client: &NativeModelClient, model: &str) -> Result<Route, ModelError> {
    let (prefix, model_id) = model.split_once('/').unwrap_or(("openai", model));
    let configured_base = client.configured_base_url.clone();
    let route = match prefix {
        "openai" => Route {
            base_url: configured_base
                .or_else(|| client.injected_or_env(&["OPENAI_API_BASE"]))
                .unwrap_or_else(|| "https://api.openai.com/v1".into()),
            api_key: client.credential(&["OPENAI_API_KEY"]),
            model: model_id.into(),
        },
        "xai" | "grok" => Route {
            base_url: configured_base
                .or_else(|| client.injected_or_env(&["XAI_API_BASE"]))
                .unwrap_or_else(|| "https://api.x.ai/v1".into()),
            api_key: client.credential(&["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"]),
            model: model_id.into(),
        },
        "opencode-go" | "opencode" => Route {
            base_url: client
                .injected_or_env(&["OPENCODE_API_BASE", "OPENCODE_GO_API_BASE"])
                .unwrap_or_else(|| "https://opencode.ai/zen/go/v1".into()),
            api_key: client.credential(&["OPENCODE_API_KEY", "OPENCODE_GO_API_KEY"]),
            model: model_id.into(),
        },
        "opencode-zen" => Route {
            base_url: client
                .injected_or_env(&["OPENCODE_ZEN_API_BASE"])
                .unwrap_or_else(|| "https://opencode.ai/zen/v1".into()),
            api_key: client.credential(&[
                "OPENCODE_ZEN_API_KEY",
                "OPENCODE_API_KEY",
                "OPENCODE_GO_API_KEY",
            ]),
            model: model_id.into(),
        },
        "ollama" | "ollama_chat" => {
            let base = client
                .injected_or_env(&["OLLAMA_API_BASE"])
                .or(configured_base)
                .unwrap_or_else(|| "http://localhost:11434".into());
            Route {
                base_url: if base.trim_end_matches('/').ends_with("/v1") {
                    base
                } else {
                    format!("{}/v1", base.trim_end_matches('/'))
                },
                api_key: client.credential(&["OLLAMA_API_KEY"]),
                model: model_id.into(),
            }
        }
        other => {
            return Err(ModelError::Provider(format!(
                "unsupported native model provider prefix `{other}`"
            )))
        }
    };
    if route.api_key.is_none() && !matches!(prefix, "ollama" | "ollama_chat") {
        return Err(ModelError::MissingApiKey);
    }
    Ok(route)
}

fn consume_event(
    event: &Value,
    text: &mut String,
    thinking: &mut String,
    tool_calls: &mut BTreeMap<usize, ToolCallAccumulator>,
    usage: &mut Option<Usage>,
    tx: Option<&StreamEventTx>,
) {
    if let Some(raw_usage) = event.get("usage") {
        *usage = Some(usage_from_provider(
            raw_usage,
            raw_usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            raw_usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
        ));
    }
    let Some(delta) = event.pointer("/choices/0/delta") else {
        return;
    };
    if let Some(piece) = delta.get("content").and_then(Value::as_str) {
        text.push_str(piece);
        if let Some(tx) = tx {
            let _ = tx.send(ModelStreamEvent::TextDelta { text: piece.into() });
        }
    }
    for key in [
        "reasoning_content",
        "reasoning",
        "thinking",
        "reasoning_text",
    ] {
        if let Some(piece) = delta.get(key).and_then(Value::as_str) {
            thinking.push_str(piece);
            if let Some(tx) = tx {
                let _ = tx.send(ModelStreamEvent::ThinkingDelta { text: piece.into() });
            }
            break;
        }
    }
    for raw_call in delta
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let index = raw_call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let call = tool_calls.entry(index).or_default();
        if let Some(id) = raw_call.get("id").and_then(Value::as_str) {
            if call.id.is_empty() {
                call.id = id.into();
            }
        }
        if let Some(name) = raw_call.pointer("/function/name").and_then(Value::as_str) {
            // Prefer full name replacement; only append true deltas.
            if call.name.is_empty()
                || (name.starts_with(&call.name) && name.len() > call.name.len())
            {
                call.name = name.into();
            } else if !call.name.contains(name) && !name.contains(&call.name) {
                call.name.push_str(name);
            }
        }
        maybe_emit_tool_call_start(index, call, tx);
        if let Some(arguments) = raw_call.pointer("/function/arguments") {
            let before = call.arguments.len();
            accumulate_tool_arguments(&mut call.arguments, arguments);
            if let Some(tx) = tx {
                let after = call.arguments.len();
                if after > before {
                    let id = tool_call_stream_id(index, call);
                    let _ = tx.send(ModelStreamEvent::ToolCallDelta {
                        id,
                        arguments_delta: call.arguments[before..].to_string(),
                    });
                }
            }
        }
    }
}

fn tool_call_stream_id(index: usize, call: &ToolCallAccumulator) -> String {
    if call.id.is_empty() {
        format!("call_{index}")
    } else {
        call.id.clone()
    }
}

fn maybe_emit_tool_call_start(
    index: usize,
    call: &mut ToolCallAccumulator,
    tx: Option<&StreamEventTx>,
) {
    if call.start_sent || call.name.is_empty() {
        return;
    }
    if let Some(tx) = tx {
        let id = tool_call_stream_id(index, call);
        let _ = tx.send(ModelStreamEvent::ToolCallStart {
            id,
            name: call.name.clone(),
        });
        call.start_sent = true;
    }
}

/// Assemble streamed tool arguments without concatenating complete JSON snapshots.
fn accumulate_tool_arguments(acc: &mut String, fragment: &Value) {
    match fragment {
        Value::String(s) => {
            if s.is_empty() {
                return;
            }
            // Provider sent a full JSON object snapshot as a string (not a delta).
            if matches!(serde_json::from_str::<Value>(s), Ok(Value::Object(_))) {
                *acc = s.clone();
                return;
            }
            acc.push_str(s);
        }
        Value::Object(_) | Value::Array(_) => {
            // Non-delta providers may emit already-parsed argument objects.
            *acc = fragment.to_string();
        }
        Value::Null => {}
        other => {
            // Numbers/bools as sole argument value are invalid for tools; keep as text for
            // finalize to reject with a clear protocol error.
            if acc.is_empty() {
                *acc = other.to_string();
            }
        }
    }
}

fn finalize_tool_calls(
    tool_calls: BTreeMap<usize, ToolCallAccumulator>,
) -> Result<Vec<ToolCall>, ModelError> {
    tool_calls
        .into_iter()
        .map(|(index, call)| {
            let arguments = if call.arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&call.arguments).map_err(|error| {
                    ModelError::Protocol(format!("tool arguments are not valid JSON: {error}"))
                })?
            };
            Ok(ToolCall {
                id: if call.id.is_empty() {
                    format!("call_{index}")
                } else {
                    call.id
                },
                name: call.name,
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
    } else if effort == "max" {
        effort = "xhigh".into();
    }
    let supported = catalog_supports_openai_effort(model).unwrap_or_else(|| {
        let model_id = model.split_once('/').map(|(_, id)| id).unwrap_or(model);
        model.starts_with("opencode-")
            || (model.starts_with("openai/")
                && ["gpt-5", "o1", "o3", "o4"]
                    .iter()
                    .any(|prefix| model_id.starts_with(prefix)))
            || ((model.starts_with("xai/") || model.starts_with("grok/"))
                && ["grok-4.3", "grok-4.5", "grok-4.20", "grok-4.6"]
                    .iter()
                    .any(|marker| model_id.contains(marker)))
    });
    if supported {
        body["reasoning_effort"] = Value::String(effort);
    }
}

fn catalog_supports_openai_effort(model: &str) -> Option<bool> {
    forge_connect::ModelCatalogCache::user_default()
        .model_effort_options(model)
        .map(|options| !options.is_empty())
}

async fn response_error(response: reqwest::Response) -> ModelError {
    let status = response.status().as_u16();
    let detail = response.text().await.unwrap_or_default();
    ModelError::ProviderStatus { status, detail }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::test_support::serve_once;
    use crate::ModelClient;
    use forge_config::Config;
    use forge_types::{Message, MessageRole, ModelStreamEvent, SideEffectClass, ToolDescriptor};

    fn request(model: &str) -> ModelRequest {
        ModelRequest {
            workspace_root: std::path::PathBuf::new(),
            messages: vec![Message::new(MessageRole::User, "hello")],
            tools: vec![ToolDescriptor {
                name: "bash".into(),
                description: "run command".into(),
                input_schema: json!({"type":"object"}),
                side_effect_class: SideEffectClass::Exec,
                idempotent: false,
            }],
            model: model.into(),
            route_id: None,
            reasoning_effort: None,
            prompt_cache: true,
        }
    }

    #[test]
    fn routes_built_in_openai_compatible_providers() {
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        client.apply_provider_env(&[
            ("OPENAI_API_KEY".into(), "openai-key".into()),
            ("XAI_API_KEY".into(), "xai-key".into()),
            ("OPENCODE_API_KEY".into(), "opencode-key".into()),
        ]);
        assert_eq!(route(&client, "openai/gpt-4.1").unwrap().model, "gpt-4.1");
        assert_eq!(route(&client, "xai/grok-3").unwrap().model, "grok-3");
        assert_eq!(
            route(&client, "opencode-go/gpt-4.1-mini").unwrap().base_url,
            "https://opencode.ai/zen/go/v1"
        );
        assert_eq!(
            route(&client, "ollama/llama3.2").unwrap().base_url,
            "http://localhost:11434/v1"
        );
    }

    #[test]
    fn accumulates_streamed_tool_calls_and_emits_stream_events() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = BTreeMap::new();
        let mut usage = None;
        consume_event(
            &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":"}}]}}]}),
            &mut text,
            &mut thinking,
            &mut calls,
            &mut usage,
            Some(&tx),
        );
        consume_event(
            &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]}}]}),
            &mut text,
            &mut thinking,
            &mut calls,
            &mut usage,
            Some(&tx),
        );
        let calls = finalize_tool_calls(calls).unwrap();
        for call in &calls {
            let _ = tx.send(ModelStreamEvent::ToolCallEnd { call: call.clone() });
        }
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|event| {
            matches!(event, ModelStreamEvent::ToolCallStart { name, .. } if name == "bash")
        }));
        assert!(events
            .iter()
            .any(|event| { matches!(event, ModelStreamEvent::ToolCallDelta { .. }) }));
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments["command"], "ls");
    }

    #[test]
    fn accumulates_streamed_tool_calls() {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = BTreeMap::new();
        let mut usage = None;
        consume_event(
            &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{\"command\":"}}]}}]}),
            &mut text,
            &mut thinking,
            &mut calls,
            &mut usage,
            None,
        );
        consume_event(
            &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]}}]}),
            &mut text,
            &mut thinking,
            &mut calls,
            &mut usage,
            None,
        );
        let calls = finalize_tool_calls(calls).unwrap();
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments["command"], "ls");
    }

    #[test]
    fn accumulates_read_file_offset_limit_without_concat() {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = BTreeMap::new();
        let mut usage = None;
        // Streamed JSON fragments must keep offset and limit as distinct fields.
        for frag in [
            r#"{"path":"README.md","offset":"#,
            "1",
            r#","limit":"#,
            "100",
            "}",
        ] {
            consume_event(
                &json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":frag}}]}}]}),
                &mut text,
                &mut thinking,
                &mut calls,
                &mut usage,
                None,
            );
        }
        let calls = finalize_tool_calls(calls).unwrap();
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "README.md");
        assert_eq!(calls[0].arguments["offset"], 1);
        assert_eq!(calls[0].arguments["limit"], 100);
    }

    #[test]
    fn full_argument_object_snapshot_replaces_instead_of_concat() {
        let mut acc = String::new();
        accumulate_tool_arguments(&mut acc, &json!({"path": "a", "offset": 1}));
        accumulate_tool_arguments(&mut acc, &json!({"path": "a", "offset": 1, "limit": 100}));
        let parsed: Value = serde_json::from_str(&acc).unwrap();
        assert_eq!(parsed["offset"], 1);
        assert_eq!(parsed["limit"], 100);
        assert!(acc.matches("offset").count() == 1);
    }

    #[test]
    fn interleaved_tool_call_indexes_stay_isolated() {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = BTreeMap::new();
        let mut usage = None;
        consume_event(
            &json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"a","function":{"name":"read_file","arguments":"{\"path\":\"a\",\"offset\":"}},
                {"index":1,"id":"b","function":{"name":"read_file","arguments":"{\"path\":\"b\",\"offset\":"}}
            ]}}]}),
            &mut text,
            &mut thinking,
            &mut calls,
            &mut usage,
            None,
        );
        consume_event(
            &json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"1,\"limit\":100}"}},
                {"index":1,"function":{"arguments":"2,\"limit\":50}"}}
            ]}}]}),
            &mut text,
            &mut thinking,
            &mut calls,
            &mut usage,
            None,
        );
        let calls = finalize_tool_calls(calls).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments["offset"], 1);
        assert_eq!(calls[0].arguments["limit"], 100);
        assert_eq!(calls[1].arguments["offset"], 2);
        assert_eq!(calls[1].arguments["limit"], 50);
    }

    #[tokio::test]
    async fn completes_openai_sse_and_emits_stream_events() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"ls\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"world\"}}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4}}\n\n",
            "data: [DONE]\n\n"
        );
        let (base_url, request_rx) = serve_once("200 OK", "text/event-stream", sse).await;
        let mut config = Config::default();
        config.model.base_url = Some(format!("{base_url}/v1"));
        config.model.api_key = Some("secret".into());
        let client = NativeModelClient::from_config(&config).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();

        let mut request = request("openai/gpt-5-test");
        request.reasoning_effort = Some("high".into());

        let response = client
            .complete_with_stream(request, Some(tx))
            .await
            .unwrap();

        assert_eq!(response.text, "hello world");
        assert_eq!(response.thinking.as_deref(), Some("think "));
        assert_eq!(response.tool_calls[0].arguments["command"], "ls");
        assert_eq!(response.usage.unwrap().completion_tokens, 4);
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(
            events.first(),
            Some(ModelStreamEvent::ThinkingDelta { .. })
        ));
        assert!(matches!(events.last(), Some(ModelStreamEvent::MessageEnd)));

        let raw_request = request_rx.await.unwrap();
        assert!(raw_request.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(raw_request
            .to_ascii_lowercase()
            .contains("authorization: bearer secret"));
        assert!(raw_request.contains("\"reasoning_effort\":\"high\""));
        assert!(raw_request.contains("\"tools\""));
    }

    #[tokio::test]
    async fn reports_provider_http_errors() {
        let (base_url, _) =
            serve_once("429 Too Many Requests", "application/json", "rate limited").await;
        let mut config = Config::default();
        config.model.base_url = Some(base_url);
        config.model.api_key = Some("secret".into());
        let client = NativeModelClient::from_config(&config).unwrap();

        let error = client
            .complete(request("openai/gpt-test"))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("429"));
        assert!(error.to_string().contains("rate limited"));
    }

    #[test]
    fn rejects_missing_keys_unknown_prefixes_and_invalid_tool_json() {
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        assert!(matches!(
            route(&client, "openai/gpt"),
            Err(ModelError::MissingApiKey)
        ));
        assert!(route(&client, "unknown/model").is_err());
        let calls = BTreeMap::from([(
            0,
            ToolCallAccumulator {
                id: String::new(),
                name: "bash".into(),
                arguments: "not-json".into(),
                start_sent: false,
            },
        )]);
        assert!(finalize_tool_calls(calls).is_err());
    }
}
