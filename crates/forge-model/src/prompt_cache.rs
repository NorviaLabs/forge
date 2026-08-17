//! Prompt-cache breakpoints and usage parsing shared across native transports.

use forge_types::Usage;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

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
        // GPT-5.6+ reports writes as `cache_write_tokens`, in
        // `input_tokens_details` on the Responses API and
        // `prompt_tokens_details` on Chat Completions. Without these two the
        // write count silently reads zero on the newest models, which is the
        // half of the picture that explains an expensive low hit rate.
        .or_else(|| raw.pointer("/input_tokens_details/cache_write_tokens"))
        .or_else(|| raw.pointer("/prompt_tokens_details/cache_write_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    (read, write)
}

/// Whether a provider's input-token count already accounts for cached tokens.
///
/// Providers disagree, and the disagreement is invisible at the call site.
/// Anthropic reports `input_tokens` as the *uncached remainder*, with
/// `cache_read_input_tokens` and `cache_creation_input_tokens` alongside it.
/// OpenAI reports a total that already contains `cached_tokens`.
///
/// Storing either verbatim makes `prompt_tokens` mean two different things, so
/// `cache_read / prompt_tokens` is a fraction of the whole prompt on one
/// provider and a multiple of the uncached part on the other — the same
/// caching reads as 90% or 900% depending on who answered. Normalising here
/// makes [`Usage::prompt_tokens`] one quantity everywhere: total input tokens
/// for the request, cached or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTokens {
    /// The count is the whole input, cached portion included (OpenAI).
    Total,
    /// The count excludes cached tokens, reported separately (Anthropic).
    UncachedOnly,
}

pub fn usage_from_provider(
    raw: &Value,
    input_tokens: u32,
    completion_tokens: u32,
    semantics: InputTokens,
) -> Usage {
    let (prompt_cache_read_tokens, prompt_cache_write_tokens) = cache_tokens_from_usage(raw);
    let prompt_tokens = match semantics {
        InputTokens::Total => input_tokens,
        // Cache *writes* are input tokens too: they are the tokens being
        // stored, and they are billed as input.
        InputTokens::UncachedOnly => input_tokens
            .saturating_add(prompt_cache_read_tokens)
            .saturating_add(prompt_cache_write_tokens),
    };
    Usage {
        prompt_tokens,
        completion_tokens,
        prompt_cache_read_tokens,
        prompt_cache_write_tokens,
    }
}

/// Routing hint that keeps a workspace's requests on one cache.
///
/// OpenAI documents `prompt_cache_key` as steering requests to the same cache
/// to improve hit rates. Without it, two requests with a byte-identical prefix
/// can land on different backends and the second simply misses — which looks
/// exactly like a caching bug while the prompt is provably stable.
///
/// Keyed on the workspace rather than the session, deliberately: forge's
/// prefix (system prompt, sorted tools, sorted skills) is identical for every
/// session in the same workspace, so a session-scoped key would throw away a
/// warm cache each time you restart. The path is hashed rather than sent, so
/// no directory name leaves the machine.
pub fn prompt_cache_key(workspace_root: &std::path::Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace_root.as_os_str().as_encoded_bytes());
    format!("forge-{:x}", hasher.finalize())[..24].to_string()
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
        let usage = usage_from_provider(&anthropic, 100, 5, InputTokens::UncachedOnly);
        assert_eq!(usage.prompt_cache_read_tokens, 60);
        assert_eq!(usage.prompt_cache_write_tokens, 40);
        assert_eq!(
            usage.prompt_tokens, 200,
            "Anthropic reports the uncached remainder, so the cached and \
             written tokens must be added back to get the whole input"
        );

        let openai = json!({
            "prompt_tokens": 80,
            "completion_tokens": 3,
            "prompt_tokens_details": {"cached_tokens": 55}
        });
        let (read, write) = cache_tokens_from_usage(&openai);
        assert_eq!(read, 55);
        assert_eq!(write, 0);
    }

    /// The whole point of `InputTokens`: identical caching must produce an
    /// identical ratio whoever reported it.
    ///
    /// Before normalising, `cache_read / prompt_tokens` was 9000/1000 = 900%
    /// on Anthropic and 9000/10000 = 90% on OpenAI for the same 10k prompt
    /// with 9k cached — so switching provider looked like a tenfold cache
    /// regression with nothing having changed.
    #[test]
    fn the_cache_ratio_means_the_same_thing_on_every_provider() {
        let anthropic = usage_from_provider(
            &json!({
                "input_tokens": 1000,
                "cache_read_input_tokens": 9000,
                "cache_creation_input_tokens": 0
            }),
            1000,
            7,
            InputTokens::UncachedOnly,
        );
        let openai = usage_from_provider(
            &json!({
                "prompt_tokens": 10000,
                "prompt_tokens_details": {"cached_tokens": 9000}
            }),
            10000,
            7,
            InputTokens::Total,
        );

        assert_eq!(anthropic.prompt_tokens, openai.prompt_tokens);
        assert_eq!(
            anthropic.prompt_cache_read_tokens,
            openai.prompt_cache_read_tokens
        );

        let ratio = |u: &Usage| u.prompt_cache_read_tokens as f64 / u.prompt_tokens as f64;
        assert!(
            (ratio(&anthropic) - 0.9).abs() < 1e-9,
            "{}",
            ratio(&anthropic)
        );
        assert!((ratio(&openai) - 0.9).abs() < 1e-9, "{}", ratio(&openai));
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

    /// The key must be identical for the same workspace and different across
    /// workspaces — that is the entire mechanism.
    #[test]
    fn the_cache_key_is_stable_per_workspace() {
        use std::path::Path;
        let a = prompt_cache_key(Path::new("/home/dev/forge"));
        let b = prompt_cache_key(Path::new("/home/dev/forge"));
        let c = prompt_cache_key(Path::new("/home/dev/other"));

        assert_eq!(a, b, "a restart in the same workspace must reuse the cache");
        assert_ne!(a, c, "separate workspaces must not share a cache shard");
        assert!(a.starts_with("forge-"), "{a}");
        assert_eq!(a.len(), 24, "{a}");
        assert!(
            !a.contains("forge/") && !a.contains("home"),
            "the path is hashed, never sent: {a}"
        );
    }

    /// GPT-5.6 renamed the write counter. Reading only the Anthropic spelling
    /// reported zero writes on the newest models, hiding the expensive half of
    /// a low hit rate.
    #[test]
    fn cache_writes_are_read_from_every_provider_spelling() {
        let cases = [
            (json!({"cache_creation_input_tokens": 40}), 40, "anthropic"),
            (
                json!({"input_tokens_details": {"cache_write_tokens": 41}}),
                41,
                "responses api",
            ),
            (
                json!({"prompt_tokens_details": {"cache_write_tokens": 42}}),
                42,
                "chat completions",
            ),
        ];
        for (raw, expected, who) in cases {
            let (_, write) = cache_tokens_from_usage(&raw);
            assert_eq!(write, expected, "{who} write count went unread");
        }
    }
}
