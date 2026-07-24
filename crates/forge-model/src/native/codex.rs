use std::collections::{BTreeMap, HashSet};

use forge_types::{MessageRole, ModelResponse, ModelStreamEvent, ToolCall, Usage};
use futures::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::NativeModelClient;
use crate::{ModelError, ModelRequest, StreamEventTx};

const CODEX_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

pub(super) async fn complete(
    client: &NativeModelClient,
    req: ModelRequest,
    model: &str,
    tx: Option<StreamEventTx>,
) -> Result<ModelResponse, ModelError> {
    let access_token = client
        .credential(&["FORGE_CODEX_ACCESS_TOKEN"])
        .ok_or_else(|| {
            ModelError::Provider(
                "No Forge ChatGPT session found. Run `/connect openai_codex`.".into(),
            )
        })?;
    let account_id = client
        .credential(&["FORGE_CODEX_ACCOUNT_ID"])
        .ok_or_else(|| {
            ModelError::Provider(
                "OpenAI Codex account id is missing. Reconnect with `/connect openai_codex`."
                    .into(),
            )
        })?;
    let aliases = tool_aliases(&req);
    let body = request_body(client, &req, model, &aliases);
    let request_id = Uuid::new_v4().to_string();
    let response = client
        .http
        .post(CODEX_URL)
        .bearer_auth(access_token)
        .header("chatgpt-account-id", account_id)
        .header("originator", "forge")
        .header("OpenAI-Beta", "responses=experimental")
        .header("accept", "text/event-stream")
        .header("session-id", &request_id)
        .header("x-client-request-id", &request_id)
        .header("User-Agent", "forge")
        .json(&body)
        .send()
        .await
        .map_err(|error| ModelError::Transport(error.to_string()))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ModelError::Provider(
                "Forge's Codex login expired. Run `/connect openai_codex` again.".into(),
            ));
        }
        return Err(ModelError::Provider(format!("HTTP {status}: {detail}")));
    }

    let reverse_aliases: BTreeMap<String, String> = aliases
        .iter()
        .map(|(original, alias)| (alias.clone(), original.clone()))
        .collect();
    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ModelError::Transport(error.to_string()))?;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = pending.find('\n') {
            let line = pending[..newline].trim().to_string();
            pending.drain(..=newline);
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let event: Value = serde_json::from_str(data)
                .map_err(|error| ModelError::Protocol(format!("invalid SSE JSON: {error}")))?;
            consume_event(
                &event,
                &reverse_aliases,
                &mut text,
                &mut thinking,
                &mut tool_calls,
                &mut usage,
                tx.as_ref(),
            )?;
        }
    }
    if let Some(tx) = tx {
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

fn request_body(
    client: &NativeModelClient,
    req: &ModelRequest,
    model: &str,
    aliases: &BTreeMap<String, String>,
) -> Value {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    let function_call_ids: HashSet<&str> = req
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter().map(|call| call.id.as_str()))
        .collect();
    let function_output_ids: HashSet<&str> = req
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    for message in &req.messages {
        match message.role {
            MessageRole::System => instructions.push(message.content.clone()),
            MessageRole::Tool => {
                if let Some(call_id) = message
                    .tool_call_id
                    .as_deref()
                    .filter(|call_id| function_call_ids.contains(call_id))
                {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": message.content
                    }));
                }
            }
            MessageRole::User => input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": message.content}]
            })),
            MessageRole::Assistant => {
                if !message.content.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": message.content}]
                    }));
                }
                input.extend(message.tool_calls.iter().filter_map(|call| {
                    function_output_ids.contains(call.id.as_str()).then(|| {
                        json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": aliases.get(&call.name).unwrap_or(&call.name),
                            "arguments": call.arguments.to_string()
                        })
                    })
                }));
            }
        }
    }
    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": aliases.get(&tool.name).unwrap_or(&tool.name),
                "description": tool.description,
                "parameters": tool.input_schema
            })
        })
        .collect();
    let mut body = json!({
        "model": model.trim_start_matches("openai-codex/"),
        "store": false,
        "stream": true,
        "instructions": if instructions.is_empty() {
            "You are a helpful coding assistant.".into()
        } else {
            instructions.join("\n\n")
        },
        "input": input,
        "include": ["reasoning.encrypted_content"],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "text": {"verbosity": "low"},
        "reasoning": {"summary": "auto"}
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if let Some(effort) = codex_effort(client) {
        body["reasoning"]["effort"] = Value::String(effort);
    }
    body
}

#[allow(clippy::too_many_arguments)]
fn consume_event(
    event: &Value,
    reverse_aliases: &BTreeMap<String, String>,
    text: &mut String,
    thinking: &mut String,
    tool_calls: &mut Vec<ToolCall>,
    usage: &mut Option<Usage>,
    tx: Option<&StreamEventTx>,
) -> Result<(), ModelError> {
    match event.get("type").and_then(Value::as_str).unwrap_or("") {
        "response.output_text.delta" => {
            let piece = event.get("delta").and_then(Value::as_str).unwrap_or("");
            text.push_str(piece);
            if let Some(tx) = tx {
                let _ = tx.send(ModelStreamEvent::TextDelta { text: piece.into() });
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let piece = event.get("delta").and_then(Value::as_str).unwrap_or("");
            thinking.push_str(piece);
            if let Some(tx) = tx {
                let _ = tx.send(ModelStreamEvent::ThinkingDelta { text: piece.into() });
            }
        }
        "response.output_item.done"
            if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
        {
            let alias = event
                .pointer("/item/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let raw_arguments = event
                .pointer("/item/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let arguments = serde_json::from_str(raw_arguments)
                .unwrap_or_else(|_| json!({"_raw": raw_arguments}));
            tool_calls.push(ToolCall {
                id: event
                    .pointer("/item/call_id")
                    .or_else(|| event.pointer("/item/id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                name: reverse_aliases
                    .get(alias)
                    .cloned()
                    .unwrap_or_else(|| alias.into()),
                arguments,
            });
        }
        "response.completed" => {
            let raw_usage = event.pointer("/response/usage").unwrap_or(&Value::Null);
            *usage = Some(Usage {
                prompt_tokens: raw_usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                completion_tokens: raw_usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
            });
        }
        "error" | "response.failed" => {
            return Err(ModelError::Provider(event.to_string()));
        }
        _ => {}
    }
    Ok(())
}

fn tool_aliases(req: &ModelRequest) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    let mut used = BTreeMap::<String, String>::new();
    for tool in &req.tools {
        let valid = !tool.name.is_empty()
            && tool.name.len() <= 64
            && tool
                .name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
        let mut alias = if valid {
            tool.name.clone()
        } else {
            let stem: String = tool
                .name
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                        ch
                    } else {
                        '_'
                    }
                })
                .take(55)
                .collect();
            format!("{}_{}", stem.trim_matches('_'), digest(&tool.name, 8))
        };
        if used
            .get(&alias)
            .is_some_and(|original| original != &tool.name)
        {
            alias = format!(
                "{}_{}",
                alias.chars().take(51).collect::<String>(),
                digest(&tool.name, 12)
            );
        }
        used.insert(alias.clone(), tool.name.clone());
        aliases.insert(tool.name.clone(), alias);
    }
    aliases
}

fn digest(value: &str, length: usize) -> String {
    let encoded = format!("{:x}", Sha256::digest(value.as_bytes()));
    encoded[..length].to_string()
}

fn codex_effort(client: &NativeModelClient) -> Option<String> {
    let effort = client.injected_or_env(&["FORGE_REASONING_EFFORT"])?;
    let effort = effort.trim().to_ascii_lowercase();
    if effort.is_empty() || effort == "auto" {
        None
    } else if effort == "max" {
        Some("xhigh".into())
    } else {
        Some(effort)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelClient;
    use forge_config::Config;
    use forge_types::{Message, MessageRole, ModelStreamEvent, SideEffectClass, ToolDescriptor};

    fn request_with_tool(name: &str) -> ModelRequest {
        ModelRequest {
            messages: vec![],
            tools: vec![ToolDescriptor {
                name: name.into(),
                description: "test".into(),
                input_schema: json!({"type": "object"}),
                side_effect_class: SideEffectClass::Read,
                idempotent: true,
            }],
            model: "openai-codex/gpt-test".into(),
        }
    }

    #[test]
    fn aliases_invalid_tool_names_stably() {
        let req = request_with_tool("mcp.server/read_file");
        let aliases = tool_aliases(&req);
        let alias = &aliases["mcp.server/read_file"];
        assert!(alias.len() <= 64);
        assert!(alias
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'));
    }

    #[test]
    fn maps_codex_tool_result_back_to_original_name() {
        let req = request_with_tool("mcp.server/read_file");
        let aliases = tool_aliases(&req);
        let alias = aliases["mcp.server/read_file"].clone();
        let reverse = BTreeMap::from([(alias.clone(), "mcp.server/read_file".into())]);
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = Vec::new();
        let mut usage = None;
        consume_event(
            &json!({"type":"response.output_item.done","item":{"type":"function_call","call_id":"c1","name":alias,"arguments":"{\"path\":\"README.md\"}"}}),
            &reverse,
            &mut text,
            &mut thinking,
            &mut calls,
            &mut usage,
            None,
        )
        .unwrap();
        assert_eq!(calls[0].name, "mcp.server/read_file");
    }

    #[test]
    fn builds_codex_request_from_full_conversation() {
        let mut request = request_with_tool("read_file");
        request.messages = vec![
            Message::new(MessageRole::System, "system prompt"),
            Message::new(MessageRole::User, "hello"),
            Message {
                role: MessageRole::Assistant,
                content: "working".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":"README.md"}),
                }],
            },
            Message {
                role: MessageRole::Tool,
                content: "contents".into(),
                tool_call_id: Some("c1".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        client.apply_provider_env(&[("FORGE_REASONING_EFFORT".into(), "max".into())]);
        let aliases = tool_aliases(&request);

        let body = request_body(&client, &request, &request.model, &aliases);

        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["instructions"], "system prompt");
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output"));
        assert_eq!(body["tools"][0]["name"], "read_file");
    }

    #[test]
    fn omits_unpaired_tool_history_from_codex_request() {
        let mut request = request_with_tool("read_file");
        request.messages = vec![
            Message::new(MessageRole::User, "hello"),
            Message {
                role: MessageRole::Assistant,
                content: "working".into(),
                tool_call_id: None,
                name: None,
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![ToolCall {
                    id: "orphan-call".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":"README.md"}),
                }],
            },
            Message {
                role: MessageRole::Tool,
                content: "contents".into(),
                tool_call_id: Some("orphan-output".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
            },
        ];
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        let aliases = tool_aliases(&request);

        let body = request_body(&client, &request, &request.model, &aliases);
        let input = body["input"].as_array().unwrap();

        assert_eq!(input.len(), 2);
        assert!(input.iter().all(|item| item["type"] != "function_call"));
        assert!(input
            .iter()
            .all(|item| item["type"] != "function_call_output"));
    }

    #[test]
    fn consumes_text_thinking_usage_raw_tools_and_errors() {
        let reverse = BTreeMap::new();
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = Vec::new();
        let mut usage = None;
        let (tx, rx) = std::sync::mpsc::channel();
        for event in [
            json!({"type":"response.output_text.delta","delta":"hello"}),
            json!({"type":"response.reasoning_text.delta","delta":"think"}),
            json!({"type":"response.output_item.done","item":{"type":"function_call","id":"i1","name":"raw_tool","arguments":"not-json"}}),
            json!({"type":"response.completed","response":{"usage":{"input_tokens":2,"output_tokens":3}}}),
        ] {
            consume_event(
                &event,
                &reverse,
                &mut text,
                &mut thinking,
                &mut calls,
                &mut usage,
                Some(&tx),
            )
            .unwrap();
        }
        assert_eq!(text, "hello");
        assert_eq!(thinking, "think");
        assert_eq!(calls[0].id, "i1");
        assert_eq!(calls[0].arguments["_raw"], "not-json");
        assert_eq!(usage.unwrap().completion_tokens, 3);
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(events[0], ModelStreamEvent::TextDelta { .. }));
        assert!(matches!(events[1], ModelStreamEvent::ThinkingDelta { .. }));

        let mut error_usage = None;
        let error = consume_event(
            &json!({"type":"response.failed","error":"bad"}),
            &reverse,
            &mut text,
            &mut thinking,
            &mut calls,
            &mut error_usage,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("response.failed"));
    }
}
