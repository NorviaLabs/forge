//! Prompt-cache breakpoints and usage parsing shared across native transports.

use forge_types::Usage;
use serde_json::{json, Value};

fn ephemeral_cache_control() -> Value {
    json!({"type": "ephemeral"})
}

/// Parse cache read/write token counts from a provider usage object.
pub fn cache_tokens_from_usage(raw: &Value) -> (u32, u32) {
    let read = raw
        .get("cache_read_input_tokens")
        .or_else(|| raw.pointer("/input_tokens_details/cached_tokens"))
        .or_else(|| raw.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let write = raw
        .get("cache_creation_input_tokens")
        .or_else(|| raw.pointer("/input_tokens_details/cache_creation_input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    (read, write)
}

pub fn usage_from_provider(raw: &Value, prompt_tokens: u32, completion_tokens: u32) -> Usage {
    let (prompt_cache_read_tokens, prompt_cache_write_tokens) = cache_tokens_from_usage(raw);
    Usage {
        prompt_tokens,
        completion_tokens,
        prompt_cache_read_tokens,
        prompt_cache_write_tokens,
    }
}

fn attach_cache_control_to_object(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.insert("cache_control".into(), ephemeral_cache_control());
    }
}

fn attach_anthropic_cache_to_content(content: &mut Value) {
    let cache = ephemeral_cache_control();
    if content.is_string() {
        *content = json!([{
            "type": "text",
            "text": content.as_str().unwrap_or(""),
            "cache_control": cache
        }]);
        return;
    }
    if let Some(blocks) = content.as_array_mut() {
        if let Some(last) = blocks.last_mut() {
            if let Some(block) = last.as_object_mut() {
                block.insert("cache_control".into(), cache);
            }
        }
    }
}

fn attach_anthropic_cache_to_message(message: &mut Value) {
    let Some(obj) = message.as_object_mut() else {
        return;
    };
    if let Some(content) = obj.get_mut("content") {
        attach_anthropic_cache_to_content(content);
    }
}

fn attach_anthropic_cache_to_system(body: &mut Value) {
    let Some(system) = body.get("system").cloned() else {
        return;
    };
    if system.as_str().is_some_and(str::is_empty) {
        return;
    }
    if system.is_string() {
        body["system"] = json!([{
            "type": "text",
            "text": system.as_str().unwrap_or(""),
            "cache_control": ephemeral_cache_control()
        }]);
        return;
    }
    if let Some(blocks) = body.get_mut("system").and_then(Value::as_array_mut) {
        if let Some(last) = blocks.last_mut() {
            if let Some(block) = last.as_object_mut() {
                block.insert("cache_control".into(), ephemeral_cache_control());
            }
        }
    }
}

/// Anthropic: sticky marks on last tool + last system block; slide the
/// conversation mark onto the last message only. Rebuilds from scratch each
/// request, so the previous tail mark is absent automatically.
pub fn apply_anthropic_prompt_cache(body: &mut Value) {
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        if let Some(last) = tools.last_mut() {
            attach_cache_control_to_object(last);
        }
    }
    attach_anthropic_cache_to_system(body);
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(last) = messages.last_mut() {
        attach_anthropic_cache_to_message(last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_anthropic_and_openai_cache_usage() {
        let anthropic = json!({
            "input_tokens": 100,
            "cache_creation_input_tokens": 40,
            "cache_read_input_tokens": 60
        });
        let usage = usage_from_provider(&anthropic, 100, 5);
        assert_eq!(usage.prompt_cache_read_tokens, 60);
        assert_eq!(usage.prompt_cache_write_tokens, 40);

        let openai = json!({
            "prompt_tokens": 80,
            "completion_tokens": 3,
            "prompt_tokens_details": {"cached_tokens": 55}
        });
        let (read, write) = cache_tokens_from_usage(&openai);
        assert_eq!(read, 55);
        assert_eq!(write, 0);
    }

    #[test]
    fn anthropic_prompt_cache_marks_last_tool_system_and_tail() {
        let mut body = json!({
            "system": "sys",
            "tools": [{"name": "a"}, {"name": "b"}],
            "messages": [
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "two"},
                {"role": "user", "content": "three"}
            ]
        });
        apply_anthropic_prompt_cache(&mut body);
        assert!(body["tools"][0].get("cache_control").is_none());
        assert!(body["tools"][1].get("cache_control").is_some());
        assert!(body["system"][0].get("cache_control").is_some());
        let messages = body["messages"].as_array().unwrap();
        assert!(messages[0].get("cache_control").is_none());
        assert!(messages[1]["content"].is_string());
        assert!(messages[2]["content"][0].get("cache_control").is_some());
    }

    #[test]
    fn anthropic_conversation_mark_slides_and_strip_keeps_prefix() {
        use crate::prompt_wire::{common_prefix_len, snapshot_prompt};

        let block = |text: &str| json!([{"type": "text", "text": text}]);
        let mut first = json!({
            "system": "sys",
            "tools": [{"name": "a"}],
            "messages": [{"role": "user", "content": block("one")}]
        });
        apply_anthropic_prompt_cache(&mut first);
        let mut second = json!({
            "system": "sys",
            "tools": [{"name": "a"}],
            "messages": [
                {"role": "user", "content": block("one")},
                {"role": "assistant", "content": block("two")}
            ]
        });
        apply_anthropic_prompt_cache(&mut second);
        assert!(first["messages"][0]["content"][0]
            .get("cache_control")
            .is_some());
        assert!(second["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
        assert!(second["messages"][1]["content"][0]
            .get("cache_control")
            .is_some());
        let a = snapshot_prompt(&crate::prompt_wire::prompt_object_from_body(first));
        let b = snapshot_prompt(&crate::prompt_wire::prompt_object_from_body(second));
        assert_eq!(common_prefix_len(&a.bytes, &b.bytes), a.bytes.len());
    }
}
