//! Provider prompt-wire snapshots for prefix diagnostics.
//!
//! Compared object is tools + system/instructions + messages, never
//! `model` / effort / route, and never `cache_control`.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::normalize::{forge_messages_to_wire_in, tools_to_openai_functions};
use crate::ModelRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTransport {
    OpenaiCompat,
    Anthropic,
    Codex,
    Mock,
}

impl PromptTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompat => "openai_compat",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
            Self::Mock => "mock",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PromptSnapshot {
    pub value: Value,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

pub fn prompt_wire(req: &ModelRequest, transport: PromptTransport) -> Value {
    match transport {
        PromptTransport::Anthropic | PromptTransport::Codex => {
            // Native transports build the exact prompt object in their complete
            // path and pass it through `prompt_object_from_body`. Mock/tests
            // use the OpenAI-compat shape, which is enough for prefix checks.
            openai_compat_prompt(req)
        }
        PromptTransport::OpenaiCompat | PromptTransport::Mock => openai_compat_prompt(req),
    }
}

pub fn openai_compat_prompt(req: &ModelRequest) -> Value {
    let mut wire = json!({
        "messages": forge_messages_to_wire_in(&req.messages, &req.workspace_root),
    });
    let tools = tools_to_openai_functions(&req.tools);
    if !tools.is_empty() {
        wire["tools"] = Value::Array(tools);
    }
    wire
}

pub fn prompt_object_from_body(body: &Value) -> Value {
    let mut wire = json!({});
    if let Some(tools) = body.get("tools") {
        wire["tools"] = tools.clone();
    }
    if let Some(system) = body.get("system") {
        wire["system"] = system.clone();
    }
    if let Some(instructions) = body.get("instructions") {
        wire["instructions"] = instructions.clone();
    }
    if let Some(messages) = body.get("messages") {
        wire["messages"] = messages.clone();
    }
    if let Some(input) = body.get("input") {
        wire["input"] = input.clone();
    }
    wire
}

pub fn strip_cache_control(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                if key == "cache_control" {
                    continue;
                }
                out.insert(key.clone(), strip_cache_control(child));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(strip_cache_control).collect()),
        other => other.clone(),
    }
}

pub fn snapshot_prompt(wire: &Value) -> PromptSnapshot {
    let value = strip_cache_control(wire);
    let bytes = encode_prompt_parts(&value);
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    PromptSnapshot {
        value,
        bytes,
        sha256,
    }
}

/// Concatenate tools, system/instructions, then each message/input item.
/// A single JSON object cannot be a byte prefix when an array grows (`]`
/// becomes `,`), so diagnostics hash this append-only encoding instead.
fn encode_prompt_parts(wire: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    append_part(&mut out, wire.get("tools"));
    append_part(
        &mut out,
        wire.get("system").or_else(|| wire.get("instructions")),
    );
    match wire
        .get("messages")
        .or_else(|| wire.get("input"))
        .and_then(Value::as_array)
    {
        Some(items) => {
            for item in items {
                append_part(&mut out, Some(item));
            }
        }
        None => append_part(&mut out, wire.get("messages").or_else(|| wire.get("input"))),
    }
    out
}

fn append_part(out: &mut Vec<u8>, part: Option<&Value>) {
    let Some(part) = part else {
        return;
    };
    if let Ok(bytes) = serde_json::to_vec(part) {
        out.extend_from_slice(&bytes);
        out.push(b'\n');
    }
}

pub fn common_prefix_len(previous: &[u8], current: &[u8]) -> usize {
    previous
        .iter()
        .zip(current.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

pub fn first_json_pointer(previous: &Value, current: &Value) -> Option<String> {
    let mut path = String::new();
    first_diff(previous, current, &mut path)
}

fn first_diff(previous: &Value, current: &Value, path: &mut String) -> Option<String> {
    match (previous, current) {
        (Value::Object(left), Value::Object(right)) => {
            let mut keys: Vec<&String> = left.keys().chain(right.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                match (left.get(key), right.get(key)) {
                    (Some(lv), Some(rv)) => {
                        let mark = path.len();
                        path.push('/');
                        path.push_str(key);
                        if let Some(diff) = first_diff(lv, rv, path) {
                            return Some(diff);
                        }
                        path.truncate(mark);
                    }
                    _ => {
                        path.push('/');
                        path.push_str(key);
                        return Some(owned_or_root(path));
                    }
                }
            }
            None
        }
        (Value::Array(left), Value::Array(right)) => {
            let shared = left.len().min(right.len());
            for index in 0..shared {
                let mark = path.len();
                path.push('/');
                path.push_str(&index.to_string());
                if let Some(diff) = first_diff(&left[index], &right[index], path) {
                    return Some(diff);
                }
                path.truncate(mark);
            }
            if left.len() != right.len() {
                path.push('/');
                path.push_str(&shared.to_string());
                return Some(owned_or_root(path));
            }
            None
        }
        (left, right) if left == right => None,
        _ => Some(owned_or_root(path)),
    }
}

fn owned_or_root(path: &str) -> String {
    if path.is_empty() {
        "/".into()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SharedMessages;
    use forge_types::{Message, MessageRole, ToolDescriptor};
    use serde_json::json;

    fn req(messages: Vec<Message>, tools: Vec<ToolDescriptor>) -> ModelRequest {
        ModelRequest {
            messages: SharedMessages::from(messages),
            tools,
            model: "test".into(),
            workspace_root: std::path::PathBuf::from("."),
            route_id: None,
            reasoning_effort: Some("high".into()),
            prompt_cache: true,
        }
    }

    fn tool(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
            side_effect_class: forge_types::SideEffectClass::Read,
            idempotent: true,
        }
    }

    #[test]
    fn openai_compat_omits_model_and_effort() {
        let request = req(
            vec![Message::new(MessageRole::User, "hi")],
            vec![tool("read_file")],
        );
        let wire = openai_compat_prompt(&request);
        assert!(wire.get("model").is_none());
        assert!(wire.get("reasoning_effort").is_none());
        assert!(wire.get("messages").is_some());
        assert!(wire.get("tools").is_some());
    }

    #[test]
    fn append_only_messages_are_a_prefix() {
        let first = req(vec![Message::new(MessageRole::User, "one")], vec![]);
        let second = req(
            vec![
                Message::new(MessageRole::User, "one"),
                Message::new(MessageRole::Assistant, "two"),
            ],
            vec![],
        );
        let a = snapshot_prompt(&prompt_wire(&first, PromptTransport::Mock));
        let b = snapshot_prompt(&prompt_wire(&second, PromptTransport::Mock));
        assert_eq!(common_prefix_len(&a.bytes, &b.bytes), a.bytes.len());
    }

    #[test]
    fn strip_hides_cache_control() {
        let previous = json!({"messages":[{"role":"user","content":"a"}]});
        let mut current = previous.clone();
        current["messages"][0]["cache_control"] = json!({"type": "ephemeral"});
        let a = snapshot_prompt(&previous);
        let b = snapshot_prompt(&current);
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn first_pointer_names_the_mutated_field() {
        let previous = json!({"messages":[{"role":"user","content":"a"}]});
        let current = json!({"messages":[{"role":"user","content":"b"}]});
        assert_eq!(
            first_json_pointer(&previous, &current).as_deref(),
            Some("/messages/0/content")
        );
    }

    #[test]
    fn effort_toggle_does_not_change_snapshot() {
        let mut request = req(vec![Message::new(MessageRole::User, "hi")], vec![]);
        let a = snapshot_prompt(&openai_compat_prompt(&request));
        request.reasoning_effort = Some("low".into());
        let b = snapshot_prompt(&openai_compat_prompt(&request));
        assert_eq!(a.sha256, b.sha256);
    }
}
