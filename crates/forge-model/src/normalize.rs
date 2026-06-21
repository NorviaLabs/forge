//! LiteLLM / OpenAI-shaped JSON → Forge ModelResponse (litellm-normalization.md).

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
        return Ok(ModelResponse {
            text,
            tool_calls,
            usage,
        });
    }

    // Raw LiteLLM / OpenAI chat completion shape:
    let choice = result
        .pointer("/choices/0")
        .ok_or_else(|| ModelError::Protocol("missing choices[0]".into()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| ModelError::Protocol("missing message".into()))?;
    let text = content_to_text(message.get("content"));
    let tool_calls = parse_tool_calls(message.get("tool_calls").unwrap_or(&Value::Null))?;
    let usage = parse_usage(result.get("usage"));
    Ok(ModelResponse {
        text,
        tool_calls,
        usage,
    })
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
    Some(Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
    })
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
            role: MessageRole::User,
            content: "hi".into(),
            tool_call_id: None,
            name: None,
        }]);
        assert_eq!(msgs[0]["role"], "user");
        let tools = tools_to_openai_functions(&[ToolDescriptor {
            name: "read_file".into(),
            description: "r".into(),
            input_schema: json!({"type":"object"}),
            side_effect_class: SideEffectClass::Read,
            idempotent: true,
        }]);
        assert_eq!(tools[0]["function"]["name"], "read_file");
    }
}
