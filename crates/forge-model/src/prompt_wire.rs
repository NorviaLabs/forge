//! Provider prompt-wire snapshots for prefix diagnostics.
//!
//! Compared object is tools + system/instructions + messages, never
//! `model` / effort / route, and never `cache_control`.

use serde::ser::{SerializeMap, SerializeSeq, Serializer};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::normalize::tools_to_openai_functions;
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

/// The comparable form of a prompt: the append-only encoding and its digest.
///
/// Deliberately does *not* carry the stripped `Value`. Materialising one costs
/// a deep clone of the entire prompt on every model step, and the only consumer
/// is the `first_json_pointer` diagnostic on the rare cache-invalidation
/// branch. That caller builds it with [`strip_cache_control`] when it actually
/// needs it.
#[derive(Debug, Clone)]
pub struct PromptSnapshot {
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
    openai_compat_prompt_with_messages(req, crate::normalize::forge_messages_to_wire_in)
}

pub(crate) fn openai_compat_prompt_cached(
    req: &ModelRequest,
    client: &crate::native::NativeModelClient,
) -> Value {
    openai_compat_prompt_with_messages(req, |messages, workspace| {
        crate::normalize::forge_messages_to_wire_in_cached(messages, workspace, client)
    })
}

fn openai_compat_prompt_with_messages(
    req: &ModelRequest,
    messages: impl Fn(&[forge_types::Message], &std::path::Path) -> Vec<Value>,
) -> Value {
    let mut wire = json!({
        "messages": messages(&req.messages, &req.workspace_root),
    });
    let tools = tools_to_openai_functions(&req.tools);
    if !tools.is_empty() {
        wire["tools"] = Value::Array(tools);
    }
    wire
}

/// Project the prompt-comparable fields out of a provider request body.
///
/// Takes the body by value: every caller has just built it and drops it
/// immediately after, so moving the fields out avoids deep-cloning the whole
/// prompt for the sake of a diagnostic.
pub fn prompt_object_from_body(body: Value) -> Value {
    let Value::Object(mut body) = body else {
        return json!({});
    };
    let mut wire = serde_json::Map::new();
    for key in ["tools", "system", "instructions", "messages", "input"] {
        if let Some(value) = body.remove(key) {
            wire.insert(key.to_string(), value);
        }
    }
    Value::Object(wire)
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
    let bytes = encode_prompt_parts(wire);
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    PromptSnapshot { bytes, sha256 }
}

/// Serialises a `Value` exactly as `serde_json` would, minus any
/// `cache_control` member at any depth.
///
/// This is what lets the encoding below run straight off the provider's own
/// wire object. Producing a stripped copy first cost a deep clone of the whole
/// prompt — on every model step — purely to drop one key per content block.
struct Stripped<'a>(&'a Value);

impl Serialize for Stripped<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(map) => {
                let mut out = serializer.serialize_map(None)?;
                for (key, child) in map {
                    if key == "cache_control" {
                        continue;
                    }
                    out.serialize_entry(key, &Stripped(child))?;
                }
                out.end()
            }
            Value::Array(items) => {
                let mut out = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    out.serialize_element(&Stripped(item))?;
                }
                out.end()
            }
            other => other.serialize(serializer),
        }
    }
}

/// Concatenate tools, system/instructions, then each message/input item.
/// A single JSON object cannot be a byte prefix when an array grows (`]`
/// becomes `,`), so diagnostics hash this append-only encoding instead.
///
/// Takes the raw provider wire object: `cache_control` is dropped during
/// serialisation, not by pre-stripping into a copy.
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
    // Serialise directly into the accumulator so a part costs one pass and no
    // intermediate buffer. On failure the partial write is rolled back, which
    // keeps the old `to_vec`-then-append semantics of emitting nothing.
    let mark = out.len();
    if serde_json::to_writer(&mut *out, &Stripped(part)).is_ok() {
        out.push(b'\n');
    } else {
        out.truncate(mark);
    }
}

pub fn common_prefix_len(previous: &[u8], current: &[u8]) -> usize {
    previous
        .iter()
        .zip(current.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

/// Names the append-only part that a byte offset falls inside.
///
/// `common_prefix_len` reports *where* two encodings diverge, in bytes. The
/// encoding is `tools\nsystem\nmsg0\nmsg1\n…` — one JSON document per part, not
/// a single document — so those bytes cannot be parsed back into one `Value`
/// and diffed against the current prompt. Counting the completed parts before
/// the offset gives the diverging part directly, without allocating or
/// retaining the previous prompt.
///
/// An offset sitting exactly on a part boundary names the part it opens, which
/// is what a divergence point means: everything before it matched. An offset
/// past the end therefore names one part past the last.
///
/// Part names come from `wire`, so a part present only in the older encoding is
/// named by position rather than by key.
pub fn part_pointer_at(wire: &Value, encoded: &[u8], offset: usize) -> String {
    let mut index = encoded[..offset.min(encoded.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();

    if wire.get("tools").is_some() {
        if index == 0 {
            return "/tools".into();
        }
        index -= 1;
    }

    let system = ["system", "instructions"]
        .into_iter()
        .find(|key| wire.get(key).is_some());
    if let Some(key) = system {
        if index == 0 {
            return format!("/{key}");
        }
        index -= 1;
    }

    match ["messages", "input"]
        .into_iter()
        .find(|key| wire.get(key).is_some())
    {
        Some(key) => format!("/{key}/{index}"),
        None => "/".into(),
    }
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
            thinking_enabled: true,
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

    /// The encoder drops `cache_control` inline instead of stripping into a
    /// copy first. That is only safe if it is byte-identical to the copy, at
    /// every nesting depth — the hash it feeds is journaled.
    #[test]
    fn inline_stripping_matches_stripping_into_a_copy() {
        let wire = json!({
            "tools": [{"name": "read_file", "input_schema": {"type": "object"}}],
            "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "a", "cache_control": {"type": "ephemeral"}},
                        {"type": "text", "text": "b", "extra": [1, 2.5, true, null, "s"]}
                    ],
                    "cache_control": {"type": "ephemeral"}
                },
                {"role": "assistant", "content": "plain"}
            ]
        });
        let inline = snapshot_prompt(&wire);
        let copied = snapshot_prompt(&strip_cache_control(&wire));
        assert_eq!(inline.bytes, copied.bytes);
        assert_eq!(inline.sha256, copied.sha256);
        assert!(!String::from_utf8_lossy(&inline.bytes).contains("cache_control"));
    }

    #[test]
    fn prompt_object_from_body_keeps_only_prompt_fields() {
        let body = json!({
            "model": "m",
            "temperature": 0.5,
            "system": "sys",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"name": "t"}]
        });
        let wire = prompt_object_from_body(body);
        assert!(wire.get("model").is_none());
        assert!(wire.get("temperature").is_none());
        assert_eq!(wire.get("system"), Some(&json!("sys")));
        assert!(wire.get("messages").is_some());
        assert!(wire.get("tools").is_some());
        assert!(prompt_object_from_body(json!("not an object"))
            .as_object()
            .is_some_and(serde_json::Map::is_empty));
    }

    /// The reason `part_pointer_at` exists: the snapshot encoding is several
    /// concatenated JSON documents, so parsing it back into one value — which
    /// the cache-invalidation diagnostic used to do — always fails once there
    /// is more than one part.
    #[test]
    fn the_snapshot_encoding_is_not_a_single_json_document() {
        let wire = json!({
            "system": "sys",
            "messages": [{"role": "user", "content": "a"}, {"role": "user", "content": "b"}],
        });
        let bytes = snapshot_prompt(&wire).bytes;
        assert!(serde_json::from_slice::<Value>(&bytes).is_err());
    }

    #[test]
    fn part_pointer_names_the_diverging_part() {
        let wire = json!({
            "tools": [{"name": "t"}],
            "system": "sys",
            "messages": [
                {"role": "user", "content": "a"},
                {"role": "assistant", "content": "b"},
                {"role": "user", "content": "c"},
            ],
        });
        let bytes = snapshot_prompt(&wire).bytes;

        // Every part starts at 0 or just past a newline; the trailing newline
        // ends the last part rather than starting another.
        let mut starts = vec![0usize];
        starts.extend(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        starts.pop();
        let seen: Vec<String> = starts
            .iter()
            .map(|offset| part_pointer_at(&wire, &bytes, *offset))
            .collect();
        assert_eq!(
            seen,
            vec![
                "/tools",
                "/system",
                "/messages/0",
                "/messages/1",
                "/messages/2"
            ]
        );
    }

    /// An offset sitting exactly on a boundary belongs to the part it opens,
    /// which is what a divergence point means: the bytes before it matched.
    #[test]
    fn part_pointer_treats_a_boundary_as_the_next_part() {
        let wire = json!({
            "system": "sys",
            "messages": [{"role": "user", "content": "a"}],
        });
        let bytes = snapshot_prompt(&wire).bytes;
        let boundary = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        assert_eq!(part_pointer_at(&wire, &bytes, boundary), "/system");
        assert_eq!(part_pointer_at(&wire, &bytes, boundary + 1), "/messages/0");
    }

    /// The real caller passes the byte offset from `common_prefix_len`, so the
    /// pointer must name the message that actually changed.
    #[test]
    fn part_pointer_locates_a_real_mutation() {
        let previous = json!({
            "system": "sys",
            "messages": [{"role": "user", "content": "a"}, {"role": "user", "content": "b"}],
        });
        let current = json!({
            "system": "sys",
            "messages": [{"role": "user", "content": "a"}, {"role": "user", "content": "CHANGED"}],
        });
        let old = snapshot_prompt(&previous).bytes;
        let new = snapshot_prompt(&current).bytes;
        let common = common_prefix_len(&old, &new);
        assert_eq!(part_pointer_at(&current, &old, common), "/messages/1");
    }

    /// Codex-shaped wires name their own keys.
    #[test]
    fn part_pointer_handles_instructions_and_input() {
        let wire = json!({
            "instructions": "sys",
            "input": [{"role": "user", "content": "a"}],
        });
        let bytes = snapshot_prompt(&wire).bytes;
        let boundary = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        assert_eq!(part_pointer_at(&wire, &bytes, 0), "/instructions");
        assert_eq!(part_pointer_at(&wire, &bytes, boundary + 1), "/input/0");
    }

    /// An offset past the end, and a wire with no parts at all, stay in range
    /// rather than panicking — this runs inside a diagnostic.
    #[test]
    fn part_pointer_is_total() {
        let wire = json!({"messages": [{"role": "user", "content": "a"}]});
        let bytes = snapshot_prompt(&wire).bytes;
        assert_eq!(part_pointer_at(&wire, &bytes, usize::MAX), "/messages/1");
        assert_eq!(part_pointer_at(&json!({}), &[], 0), "/");
        assert_eq!(part_pointer_at(&json!({}), &[], usize::MAX), "/");
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
