//! OpenAI-shaped JSON → Forge ModelResponse.

use forge_types::{Message, MessageRole, ModelResponse, ToolCall, ToolDescriptor, Usage};
use serde_json::{json, Value};

use crate::ModelError;

/// Map wire `complete` result object to Forge `ModelResponse`.
pub fn complete_result_from_value(result: &Value) -> Result<ModelResponse, ModelError> {
    // Already-normalized Forge shape (worker preferred):
    if result.get("text").is_some() || result.get("tool_calls").is_some() {
        let text = result
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let tool_calls = parse_tool_calls(result.get("tool_calls").unwrap_or(&Value::Null))?;
        let usage = parse_usage(result.get("usage"));
        let thinking = result
            .get("thinking")
            .and_then(|t| t.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        return Ok(ModelResponse {
            text,
            tool_calls,
            usage,
            thinking,
        });
    }

    // Raw OpenAI chat completion shape:
    let choice = result
        .pointer("/choices/0")
        .ok_or_else(|| ModelError::Protocol("missing choices[0]".into()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| ModelError::Protocol("missing message".into()))?;
    let text = content_to_text(message.get("content"));
    let tool_calls = parse_tool_calls(message.get("tool_calls").unwrap_or(&Value::Null))?;
    let usage = parse_usage(result.get("usage"));
    let thinking = extract_thinking_from_message(message);
    Ok(ModelResponse {
        text,
        tool_calls,
        usage,
        thinking,
    })
}

fn extract_thinking_from_message(message: &Value) -> Option<String> {
    for key in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(s) = message.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    // thinking_blocks: [{type, thinking/text}, ...]
    if let Some(arr) = message.get("thinking_blocks").and_then(|v| v.as_array()) {
        let mut out = String::new();
        for b in arr {
            if let Some(s) = b.get("thinking").and_then(|v| v.as_str()) {
                out.push_str(s);
            } else if let Some(s) = b.get("text").and_then(|v| v.as_str()) {
                out.push_str(s);
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

fn content_to_text(content: Option<&Value>) -> String {
    match content {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for p in parts {
                if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                    out.push_str(t);
                } else if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                        out.push_str(t);
                    }
                }
            }
            out
        }
        Some(other) => other.to_string(),
    }
}

fn parse_tool_calls(v: &Value) -> Result<Vec<ToolCall>, ModelError> {
    let Some(arr) = v.as_array() else {
        return Ok(vec![]);
    };
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        // Forge shape
        if item.get("name").is_some() && item.get("function").is_none() {
            let id = item
                .get("id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("call_{i}"));
            let name = item
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = parse_arguments(item.get("arguments"))?;
            out.push(ToolCall {
                id,
                name,
                arguments,
            });
            continue;
        }
        // OpenAI shape
        let id = item
            .get("id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("call_{i}"));
        let func = item.get("function").unwrap_or(item);
        let name = func
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let arguments = parse_arguments(func.get("arguments"))?;
        out.push(ToolCall {
            id,
            name,
            arguments,
        });
    }
    Ok(out)
}

fn parse_arguments(v: Option<&Value>) -> Result<Value, ModelError> {
    match v {
        None | Some(Value::Null) => Ok(json!({})),
        Some(Value::Object(_)) => Ok(v.unwrap().clone()),
        Some(Value::String(s)) => {
            if s.is_empty() {
                return Ok(json!({}));
            }
            serde_json::from_str(s).map_err(|e| {
                ModelError::Protocol(format!("tool arguments not valid JSON object: {e}"))
            })
        }
        Some(other) => Ok(other.clone()),
    }
}

fn parse_usage(v: Option<&Value>) -> Option<Usage> {
    let u = v?;
    let prompt = u.get("prompt_tokens").and_then(|x| x.as_u64())? as u32;
    let completion = u.get("completion_tokens").and_then(|x| x.as_u64())? as u32;
    Some(crate::prompt_cache::usage_from_provider(
        u, prompt, completion,
    ))
}

pub fn forge_messages_to_wire(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
                // `MessageRole` is `#[non_exhaustive]`; map an unrecognised future role to
                // user rather than escalating it to system/assistant.
                _ => "user",
            };
            let mut obj = json!({
                "role": role,
                "content": m.content,
            });
            if let Some(ref id) = m.tool_call_id {
                obj["tool_call_id"] = json!(id);
            }
            if let Some(ref name) = m.name {
                obj["name"] = json!(name);
            }
            // DeepSeek (and OpenCode Go's DeepSeek route) require `reasoning_content`
            // on assistant turns that include tool_calls. OpenCode CLI always keeps
            // the interleaved reasoning field; omitting it yields HTTP 400
            // "Upstream request failed".
            if let Some(ref thinking) = m.thinking {
                obj["reasoning_content"] = json!(thinking);
            } else if !m.tool_calls.is_empty() {
                obj["reasoning_content"] = json!("");
            }
            if !m.tool_calls.is_empty() {
                obj["tool_calls"] = json!(m
                    .tool_calls
                    .iter()
                    .map(|call| json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        }
                    }))
                    .collect::<Vec<_>>());
            }
            obj
        })
        .collect()
}

pub fn tools_to_openai_functions(tools: &[ToolDescriptor]) -> Vec<Value> {
    tools
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
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::SideEffectClass;

    #[test]
    fn forge_shaped_result() {
        let v = json!({
            "text": "hello",
            "tool_calls": [{
                "id": "1",
                "name": "read_file",
                "arguments": {"path": "a.txt"}
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}
        });
        let r = complete_result_from_value(&v).unwrap();
        assert_eq!(r.text, "hello");
        assert_eq!(r.tool_calls[0].name, "read_file");
        assert_eq!(r.usage.unwrap().prompt_tokens, 1);
    }

    #[test]
    fn openai_shaped_result() {
        let v = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "hi",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"ls\"}"
                        }
                    }]
                }
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4}
        });
        let r = complete_result_from_value(&v).unwrap();
        assert_eq!(r.text, "hi");
        assert_eq!(r.tool_calls[0].name, "bash");
        assert_eq!(r.tool_calls[0].arguments["command"], "ls");
    }

    #[test]
    fn content_array_parts() {
        let v = json!({
            "choices": [{
                "message": {
                    "content": [
                        {"type": "text", "text": "A"},
                        {"type": "text", "text": "B"}
                    ]
                }
            }]
        });
        let r = complete_result_from_value(&v).unwrap();
        assert_eq!(r.text, "AB");
    }

    #[test]
    fn invalid_tool_json_errors() {
        let v = json!({
            "text": "",
            "tool_calls": [{
                "id": "1",
                "name": "x",
                "arguments": "{not-json"
            }]
        });
        assert!(complete_result_from_value(&v).is_err());
    }

    #[test]
    fn messages_and_tools_wire() {
        let msgs = forge_messages_to_wire(&[Message {
            outcome: Default::default(),
            role: MessageRole::User,
            content: "hi".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }]);
        assert_eq!(msgs[0]["role"], "user");
        let assistant = forge_messages_to_wire(&[Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![forge_types::ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "README.md"}),
            }],
        }]);
        assert_eq!(assistant[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            assistant[0]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        let tools = tools_to_openai_functions(&[ToolDescriptor {
            name: "read_file".into(),
            description: "r".into(),
            input_schema: json!({"type":"object"}),
            side_effect_class: SideEffectClass::Read,
            idempotent: true,
        }]);
        assert_eq!(tools[0]["function"]["name"], "read_file");
    }

    #[test]
    fn wire_includes_reasoning_content_for_tool_calls() {
        let with_thinking = forge_messages_to_wire(&[Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("need the file".into()),
            thinking_duration_secs: None,
            tool_calls: vec![ToolCall {
                id: "call_0".into(),
                name: "read_file".into(),
                arguments: json!({"path": "main.rs"}),
            }],
        }]);
        assert_eq!(with_thinking[0]["reasoning_content"], "need the file");
        assert_eq!(with_thinking[0]["tool_calls"][0]["id"], "call_0");

        let without_thinking = forge_messages_to_wire(&[Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: json!({"command": "ls"}),
            }],
        }]);
        // Empty string is enough for DeepSeek; the field must be present.
        assert_eq!(without_thinking[0]["reasoning_content"], "");
        assert!(without_thinking[0].get("tool_calls").is_some());

        let plain = forge_messages_to_wire(&[Message {
            outcome: Default::default(),
            role: MessageRole::Assistant,
            content: "hi".into(),
            tool_call_id: None,
            name: None,
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
        }]);
        assert!(plain[0].get("reasoning_content").is_none());
    }

    #[test]
    fn thinking_is_read_from_every_supported_key() {
        for key in ["reasoning_content", "reasoning", "thinking"] {
            let message = json!({ "content": "hi", key: "deliberating" });
            assert_eq!(
                extract_thinking_from_message(&message).as_deref(),
                Some("deliberating"),
                "key {key:?} should supply thinking text"
            );
        }
    }

    #[test]
    fn blank_or_absent_thinking_is_reported_as_none() {
        assert_eq!(extract_thinking_from_message(&json!({})), None);
        assert_eq!(
            extract_thinking_from_message(&json!({"reasoning_content": ""})),
            None
        );
        assert_eq!(
            extract_thinking_from_message(&json!({"thinking_blocks": []})),
            None
        );
    }

    #[test]
    fn thinking_blocks_are_concatenated_in_order() {
        let message = json!({
            "thinking_blocks": [
                {"type": "thinking", "thinking": "first "},
                {"type": "text", "text": "second"},
                {"type": "redacted"},
            ]
        });
        assert_eq!(
            extract_thinking_from_message(&message).as_deref(),
            Some("first second")
        );
    }

    #[test]
    fn content_to_text_normalises_every_wire_shape() {
        assert_eq!(content_to_text(None), "");
        assert_eq!(content_to_text(Some(&Value::Null)), "");
        assert_eq!(content_to_text(Some(&json!("plain"))), "plain");
        // Content-part arrays concatenate their text, with or without a type tag.
        assert_eq!(
            content_to_text(Some(&json!([{"text": "a"}, {"type": "text", "text": "b"}]))),
            "ab"
        );
        // Parts carrying no usable text contribute nothing.
        assert_eq!(content_to_text(Some(&json!([{"type": "image"}]))), "");
        assert_eq!(
            content_to_text(Some(&json!([{"type": "text", "text": 123}]))),
            ""
        );
        // Anything else degrades to its JSON representation rather than being dropped.
        assert_eq!(content_to_text(Some(&json!(42))), "42");
    }

    #[test]
    fn parse_arguments_normalises_every_wire_shape() {
        assert_eq!(parse_arguments(None).unwrap(), json!({}));
        assert_eq!(parse_arguments(Some(&Value::Null)).unwrap(), json!({}));
        assert_eq!(parse_arguments(Some(&json!(""))).unwrap(), json!({}));
        assert_eq!(
            parse_arguments(Some(&json!({"a": 1}))).unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            parse_arguments(Some(&json!(r#"{"a":1}"#))).unwrap(),
            json!({"a": 1})
        );
        // A non-object, non-string value passes through untouched.
        assert_eq!(
            parse_arguments(Some(&json!([1, 2]))).unwrap(),
            json!([1, 2])
        );
    }

    #[test]
    fn parse_arguments_rejects_malformed_json_strings() {
        let err = parse_arguments(Some(&json!("{not json"))).unwrap_err();
        assert!(
            matches!(err, ModelError::Protocol(_)),
            "expected a protocol error, got {err:?}"
        );
    }

    #[test]
    fn wire_messages_carry_tool_metadata_and_reasoning() {
        let mut assistant = Message::new(MessageRole::Assistant, "working");
        assistant.thinking = Some("pondering".into());
        assistant.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "a.txt"}),
        }];

        let mut tool = Message::new(MessageRole::Tool, "contents");
        tool.tool_call_id = Some("c1".into());
        tool.name = Some("read_file".into());

        let wire = forge_messages_to_wire(&[
            Message::new(MessageRole::System, "sys"),
            Message::new(MessageRole::User, "hi"),
            assistant,
            tool,
        ]);

        assert_eq!(wire[0]["role"], "system");
        assert_eq!(wire[1]["role"], "user");

        assert_eq!(wire[2]["role"], "assistant");
        assert_eq!(wire[2]["reasoning_content"], "pondering");
        assert_eq!(wire[2]["tool_calls"][0]["id"], "c1");
        assert_eq!(wire[2]["tool_calls"][0]["type"], "function");
        assert_eq!(wire[2]["tool_calls"][0]["function"]["name"], "read_file");
        // Arguments are serialised as a JSON *string*, not a nested object.
        assert_eq!(
            wire[2]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"a.txt"}"#
        );

        assert_eq!(wire[3]["role"], "tool");
        assert_eq!(wire[3]["tool_call_id"], "c1");
        assert_eq!(wire[3]["name"], "read_file");
    }
}
