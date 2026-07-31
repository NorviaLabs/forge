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

/// Index of the penultimate message — the stable prefix breakpoint for multi-turn agents.
fn prefix_breakpoint_index(len: usize) -> Option<usize> {
    if len >= 2 {
        Some(len - 2)
    } else if len == 1 {
        Some(0)
    } else {
        None
    }
}

fn attach_openai_cache_control(message: &mut Value) {
    if let Some(obj) = message.as_object_mut() {
        obj.insert("cache_control".into(), ephemeral_cache_control());
    }
}

/// OpenAI-compatible chat bodies: cache the system prompt and the stable prefix.
pub fn apply_openai_prompt_cache(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(first) = messages.first_mut() {
        if first.get("role").and_then(Value::as_str) == Some("system") {
            attach_openai_cache_control(first);
        }
    }
    if let Some(index) = prefix_breakpoint_index(messages.len()) {
        attach_openai_cache_control(&mut messages[index]);
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

/// Anthropic messages bodies: cache the system prompt and the stable prefix.
pub fn apply_anthropic_prompt_cache(body: &mut Value) {
    attach_anthropic_cache_to_system(body);
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(index) = prefix_breakpoint_index(messages.len()) {
        attach_anthropic_cache_to_message(&mut messages[index]);
    }
}

fn attach_codex_cache_to_input_item(item: &mut Value) {
    let Some(obj) = item.as_object_mut() else {
        return;
    };
    if let Some(content) = obj.get_mut("content") {
        attach_anthropic_cache_to_content(content);
    }
}

/// Codex responses bodies: cache instructions and the stable input prefix.
pub fn apply_codex_prompt_cache(body: &mut Value) {
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        if !instructions.is_empty() {
            body["instructions"] = json!([{
                "type": "input_text",
                "text": instructions,
                "cache_control": ephemeral_cache_control()
            }]);
        }
    }
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(index) = prefix_breakpoint_index(input.len()) {
        attach_codex_cache_to_input_item(&mut input[index]);
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
    fn openai_prompt_cache_marks_system_and_prefix() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "two"},
                {"role": "user", "content": "three"}
            ]
        });
        apply_openai_prompt_cache(&mut body);
        let messages = body["messages"].as_array().unwrap();
        assert!(messages[0].get("cache_control").is_some());
        assert!(messages[2].get("cache_control").is_some());
        assert!(messages[3].get("cache_control").is_none());
    }

    #[test]
    fn anthropic_prompt_cache_marks_system_and_prefix() {
        let mut body = json!({
            "system": "sys",
            "messages": [
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "two"},
                {"role": "user", "content": "three"}
            ]
        });
        apply_anthropic_prompt_cache(&mut body);
        assert!(body["system"][0].get("cache_control").is_some());
        let messages = body["messages"].as_array().unwrap();
        assert!(messages[1]["content"][0].get("cache_control").is_some());
        assert!(messages[2]["content"].is_string());
    }
}
