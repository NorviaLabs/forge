use std::collections::{BTreeMap, HashSet};

use forge_types::{MessageRole, ModelResponse, ModelStreamEvent, ToolCall, Usage};
use futures::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{process_sse_lines, NativeModelClient};
use crate::prompt_cache::{usage_from_provider, InputTokens};
use crate::{ModelError, ModelRequest, StreamEventTx};

pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";

fn codex_responses_url(client: &NativeModelClient) -> String {
    format!(
        "{}/codex/responses",
        client
            .resolved_base_url(&["FORGE_CODEX_API_BASE"], DEFAULT_BASE_URL)
            .trim_end_matches('/')
    )
}

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
    // Unlike the Anthropic and OpenAI-compatible transports, the ChatGPT
    // backend-api Codex endpoint (`DEFAULT_BASE_URL` above) rejects an
    // `input[].content[].cache_control` field outright with `HTTP 400
    // "Unknown parameter"` — it is not the same Responses API surface as
    // api.openai.com's, despite the shared request shape. Do not apply
    // `apply_codex_prompt_cache` here; prompt caching for this profile isn't
    // supported, not merely disabled.
    let body = request_body(client, &req, model, &aliases);
    let request_id = Uuid::new_v4().to_string();
    let response = client
        .http
        .post(codex_responses_url(client))
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
        let status = response.status().as_u16();
        let detail = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED.as_u16() {
            // Bespoke guidance, unchanged, but tagged with the status so callers can
            // see this is an auth failure without matching on the wording.
            return Err(ModelError::ProviderAuth {
                status,
                message: "Forge's Codex login expired. Run `/connect openai_codex` again.".into(),
            });
        }
        return Err(ModelError::ProviderStatus { status, detail });
    }

    let reverse_aliases: BTreeMap<String, String> = aliases
        .iter()
        .map(|(original, alias)| (alias.clone(), original.clone()))
        .collect();
    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    let mut parsed = CodexStream::default();
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
            parsed.consume(&event, &reverse_aliases, tx.as_ref())?;
            Ok(())
        })?;
    }
    if let Some(tx) = tx {
        if let Some(ref usage) = parsed.usage {
            let _ = tx.send(ModelStreamEvent::Usage {
                usage: usage.clone(),
            });
        }
        let _ = tx.send(ModelStreamEvent::MessageEnd);
    }
    Ok(ModelResponse {
        text: parsed.text,
        tool_calls: parsed.tool_calls,
        usage: parsed.usage,
        thinking: (!parsed.thinking.is_empty()).then_some(parsed.thinking),
    })
}

fn codex_user_content(message: &forge_types::Message, workspace: &std::path::Path) -> Value {
    let mut parts = vec![json!({"type": "input_text", "text": message.content})];
    for image in &message.attachments {
        match crate::image::load_image_ref(workspace, image) {
            Ok(loaded) => parts.push(crate::image::codex_input_image_part(
                &loaded.mime,
                &loaded.bytes,
            )),
            Err(_) => parts.push(json!({
                "type": "input_text",
                "text": format!("image at `{}` is no longer available", image.path)
            })),
        }
    }
    Value::Array(parts)
}

fn codex_tool_output(message: &forge_types::Message, workspace: &std::path::Path) -> Value {
    if message.attachments.is_empty() {
        return json!(message.content);
    }
    let mut parts = Vec::new();
    if !message.content.is_empty() {
        parts.push(json!({"type": "input_text", "text": message.content}));
    }
    for image in &message.attachments {
        match crate::image::load_image_ref(workspace, image) {
            Ok(loaded) => parts.push(crate::image::codex_input_image_part(
                &loaded.mime,
                &loaded.bytes,
            )),
            Err(_) => parts.push(json!({
                "type": "input_text",
                "text": format!("image at `{}` is no longer available", image.path)
            })),
        }
    }
    Value::Array(parts)
}

pub(super) fn request_body(
    _client: &NativeModelClient,
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
    for message in req.messages.iter() {
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
                        "output": codex_tool_output(message, &req.workspace_root)
                    }));
                }
            }
            MessageRole::User => input.push(json!({
                "type": "message",
                "role": "user",
                "content": codex_user_content(message, &req.workspace_root)
            })),
            MessageRole::Assistant => {
                if !message.content.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": message.content}]
                    }));
                }
                input.extend(
                    message
                        .tool_calls
                        .iter()
                        .filter(|call| function_output_ids.contains(call.id.as_str()))
                        .map(|call| {
                            json!({
                                "type": "function_call",
                                "call_id": call.id,
                                "name": aliases.get(&call.name).unwrap_or(&call.name),
                                "arguments": call.arguments.to_string()
                            })
                        }),
                );
            }
            // `MessageRole` is `#[non_exhaustive]`. Carry an unrecognised future role
            // through as user content: never fabricate system/assistant authority for a
            // role this transport does not understand, and never silently drop content.
            _ => input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": message.content}]
            })),
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
    if let Some(effort) = codex_effort(req) {
        body["reasoning"]["effort"] = Value::String(effort);
    }
    body
}

#[derive(Default)]
struct CodexStream {
    text: String,
    thinking: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
    last_reasoning_part: Option<(String, String, u64)>,
}

impl CodexStream {
    fn consume(
        &mut self,
        event: &Value,
        reverse_aliases: &BTreeMap<String, String>,
        tx: Option<&StreamEventTx>,
    ) -> Result<(), ModelError> {
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.output_text.delta" => {
                let piece = event.get("delta").and_then(Value::as_str).unwrap_or("");
                self.text.push_str(piece);
                if let Some(tx) = tx {
                    let _ = tx.send(ModelStreamEvent::TextDelta { text: piece.into() });
                }
            }
            "response.reasoning_summary_part.added" => {
                self.enter_reasoning_part(reasoning_part_key(event), tx);
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.enter_reasoning_part(reasoning_part_key(event), tx);
                let piece = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if !piece.is_empty() {
                    self.thinking.push_str(piece);
                    if let Some(tx) = tx {
                        let _ = tx.send(ModelStreamEvent::ThinkingDelta { text: piece.into() });
                    }
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
                let call = ToolCall {
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
                };
                if let Some(tx) = tx {
                    let _ = tx.send(ModelStreamEvent::ToolCallStart {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    });
                    let _ = tx.send(ModelStreamEvent::ToolCallEnd { call: call.clone() });
                }
                self.tool_calls.push(call);
            }
            "response.completed" => {
                let raw_usage = event.pointer("/response/usage").unwrap_or(&Value::Null);
                self.usage = Some(usage_from_provider(
                    raw_usage,
                    raw_usage
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                    raw_usage
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                    InputTokens::Total,
                ));
            }
            "error" | "response.failed" => {
                return Err(ModelError::Provider(event.to_string()));
            }
            _ => {}
        }
        Ok(())
    }

    fn enter_reasoning_part(&mut self, key: (String, String, u64), tx: Option<&StreamEventTx>) {
        if self
            .last_reasoning_part
            .as_ref()
            .is_some_and(|previous| previous != &key)
            && !self.thinking.is_empty()
            && !self.thinking.ends_with('\n')
        {
            self.thinking.push_str("\n\n");
            if let Some(tx) = tx {
                let _ = tx.send(ModelStreamEvent::ThinkingDelta {
                    text: "\n\n".into(),
                });
            }
        }
        self.last_reasoning_part = Some(key);
    }
}

fn reasoning_part_key(event: &Value) -> (String, String, u64) {
    let kind = match event.get("type").and_then(Value::as_str).unwrap_or("") {
        "response.reasoning_text.delta" => "reasoning",
        _ => "summary",
    };
    let item_id = event
        .get("item_id")
        .or_else(|| event.pointer("/item/id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let summary_index = event
        .get("summary_index")
        .or_else(|| event.pointer("/part/summary_index"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    (kind.into(), item_id, summary_index)
}

pub(super) fn tool_aliases(req: &ModelRequest) -> BTreeMap<String, String> {
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

fn codex_effort(req: &ModelRequest) -> Option<String> {
    let effort = req.reasoning_effort.as_deref()?;
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
    use forge_config::Config;
    use forge_types::{Message, MessageRole, ModelStreamEvent, SideEffectClass, ToolDescriptor};

    fn request_with_tool(name: &str) -> ModelRequest {
        ModelRequest {
            workspace_root: std::path::PathBuf::new(),
            messages: vec![].into(),
            tools: vec![ToolDescriptor {
                name: name.into(),
                description: "test".into(),
                input_schema: json!({"type": "object"}),
                side_effect_class: SideEffectClass::Read,
                idempotent: true,
            }],
            model: "openai-codex/gpt-test".into(),
            route_id: Some("openai-chatgpt".into()),
            reasoning_effort: None,
            prompt_cache: true,
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
        let mut parsed = CodexStream::default();
        parsed
            .consume(
                &json!({"type":"response.output_item.done","item":{"type":"function_call","call_id":"c1","name":alias,"arguments":"{\"path\":\"README.md\"}"}}),
                &reverse,
                None,
            )
            .unwrap();
        assert_eq!(parsed.tool_calls[0].name, "mcp.server/read_file");
    }

    #[test]
    fn builds_codex_request_from_full_conversation() {
        let mut request = request_with_tool("read_file");
        request.reasoning_effort = Some("max".into());
        request.messages = vec![
            Message::new(MessageRole::System, "system prompt"),
            Message::new(MessageRole::User, "hello"),
            Message {
                outcome: Default::default(),
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
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Tool,
                content: "contents".into(),
                tool_call_id: Some("c1".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
        ]
        .into();
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        let _ = client;
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

    // Regression test for the P0 found in the 2026-08-01 usability audit:
    // the ChatGPT backend-api Codex endpoint rejects an
    // `input[].content[].cache_control` field with `HTTP 400 "Unknown
    // parameter"`, unlike the Anthropic and OpenAI-compatible transports.
    // The request body this crate actually sends must never contain that
    // field for this profile — there is no per-provider capability check to
    // rely on here, so the only correct fix is to never apply it in the
    // first place (see `complete`, which now calls `request_body` directly
    // with no `apply_codex_prompt_cache` step).
    #[test]
    fn codex_request_body_never_contains_cache_control() {
        let mut request = request_with_tool("read_file"); // prompt_cache: true by default
        request.messages = vec![
            Message::new(MessageRole::System, "system prompt"),
            Message::new(MessageRole::User, "hello"),
            Message {
                outcome: Default::default(),
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
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Tool,
                content: "contents".into(),
                tool_call_id: Some("c1".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
            Message::new(MessageRole::User, "and now?"),
        ]
        .into();
        let client = NativeModelClient::from_config(&Config::default()).unwrap();
        let aliases = tool_aliases(&request);

        let body = request_body(&client, &request, &request.model, &aliases);

        assert!(
            !body.to_string().contains("cache_control"),
            "codex request body must never contain cache_control: {body}"
        );
    }

    #[test]
    fn omits_unpaired_tool_history_from_codex_request() {
        let mut request = request_with_tool("read_file");
        request.messages = vec![
            Message::new(MessageRole::User, "hello"),
            Message {
                outcome: Default::default(),
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
                attachments: Vec::new(),
            },
            Message {
                outcome: Default::default(),
                role: MessageRole::Tool,
                content: "contents".into(),
                tool_call_id: Some("orphan-output".into()),
                name: Some("read_file".into()),
                thinking: None,
                thinking_duration_secs: None,
                tool_calls: vec![],
                attachments: Vec::new(),
            },
        ]
        .into();
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
        let mut parsed = CodexStream::default();
        let (tx, rx) = std::sync::mpsc::channel();
        for event in [
            json!({"type":"response.output_text.delta","delta":"hello"}),
            json!({"type":"response.reasoning_text.delta","delta":"think"}),
            json!({"type":"response.output_item.done","item":{"type":"function_call","id":"i1","name":"raw_tool","arguments":"not-json"}}),
            json!({"type":"response.completed","response":{"usage":{"input_tokens":2,"output_tokens":3}}}),
        ] {
            parsed.consume(&event, &reverse, Some(&tx)).unwrap();
        }
        assert_eq!(parsed.text, "hello");
        assert_eq!(parsed.thinking, "think");
        assert_eq!(parsed.tool_calls[0].id, "i1");
        assert_eq!(parsed.tool_calls[0].arguments["_raw"], "not-json");
        assert_eq!(parsed.usage.as_ref().unwrap().completion_tokens, 3);
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(events[0], ModelStreamEvent::TextDelta { .. }));
        assert!(matches!(events[1], ModelStreamEvent::ThinkingDelta { .. }));

        let error = parsed
            .consume(
                &json!({"type":"response.failed","error":"bad"}),
                &reverse,
                None,
            )
            .unwrap_err();
        assert!(error.to_string().contains("response.failed"));
    }

    fn consume_thinking(events: &[Value]) -> String {
        let reverse = BTreeMap::new();
        let mut parsed = CodexStream::default();
        for event in events {
            parsed.consume(event, &reverse, None).unwrap();
        }
        parsed.thinking
    }

    #[test]
    fn separates_reasoning_summary_parts_with_a_blank_line() {
        let thinking = consume_thinking(&[
            json!({"type":"response.reasoning_summary_part.added","item_id":"rs1","summary_index":0}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs1","summary_index":0,"delta":"**Designing SessionTemp**"}),
            json!({"type":"response.reasoning_summary_part.added","item_id":"rs1","summary_index":1}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs1","summary_index":1,"delta":"**Planning safe creation**"}),
        ]);
        assert_eq!(
            thinking,
            "**Designing SessionTemp**\n\n**Planning safe creation**"
        );
    }

    #[test]
    fn separates_reasoning_summary_index_changes_without_part_events() {
        let thinking = consume_thinking(&[
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs1","summary_index":0,"delta":"**Designing SessionTemp**"}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs1","summary_index":1,"delta":"**Planning safe creation**"}),
        ]);
        assert_eq!(
            thinking,
            "**Designing SessionTemp**\n\n**Planning safe creation**"
        );
    }

    #[test]
    fn keeps_same_summary_part_deltas_contiguous() {
        let thinking = consume_thinking(&[
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs1","summary_index":0,"delta":"**Design"}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs1","summary_index":0,"delta":"ing**"}),
        ]);
        assert_eq!(thinking, "**Designing**");
    }

    #[tokio::test]
    async fn completes_codex_sse_against_configured_base_url() {
        use crate::ModelClient;
        use forge_test_support::mock_http;

        let sse = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n",
        );
        let Some(base) = mock_http(vec![(
            200,
            sse,
            vec![("content-type", "text/event-stream")],
        )]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let mut config = Config::default();
        config.model.base_url = Some(base);
        let client = NativeModelClient::from_config(&config).unwrap();
        client.apply_provider_env(&[
            ("FORGE_CODEX_ACCESS_TOKEN".into(), "token".into()),
            ("FORGE_CODEX_ACCOUNT_ID".into(), "account-123".into()),
        ]);
        let request = ModelRequest {
            workspace_root: std::path::PathBuf::new(),
            model: "openai-codex/gpt-test".into(),
            route_id: Some("openai-chatgpt".into()),
            messages: vec![Message::new(MessageRole::User, "hello")].into(),
            tools: vec![],
            reasoning_effort: None,
            prompt_cache: true,
        };
        let response = client.complete(request).await.unwrap();
        assert_eq!(response.text, "hello");
        assert_eq!(response.usage.unwrap().completion_tokens, 2);
    }

    #[tokio::test]
    async fn codex_auth_failure_surfaces_provider_auth_error() {
        use crate::ModelClient;
        use forge_test_support::mock_http;

        let Some(base) = mock_http(vec![(401, "unauthorized", vec![])]) else {
            eprintln!("skipping: this host denies binding a listener");
            return;
        };
        let mut config = Config::default();
        config.model.base_url = Some(base);
        let client = NativeModelClient::from_config(&config).unwrap();
        client.apply_provider_env(&[
            ("FORGE_CODEX_ACCESS_TOKEN".into(), "token".into()),
            ("FORGE_CODEX_ACCOUNT_ID".into(), "account-123".into()),
        ]);
        let request = ModelRequest {
            workspace_root: std::path::PathBuf::new(),
            model: "openai-codex/gpt-test".into(),
            route_id: Some("openai-chatgpt".into()),
            messages: vec![Message::new(MessageRole::User, "hello")].into(),
            tools: vec![],
            reasoning_effort: None,
            prompt_cache: true,
        };
        let err = client.complete(request).await.unwrap_err();
        assert!(err.is_auth_failure());
    }
}
