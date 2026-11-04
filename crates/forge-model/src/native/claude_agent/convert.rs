//! Translation between Forge's transcript and Claude Code's messages.

use std::collections::HashMap;

use forge_connect::claude_agent::TOOL_PREFIX;
use forge_types::{ExecutionOutcome, ImageRef, Message, MessageRole, ToolDescriptor};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Added to Claude Code's own system prompt for every Forge chat.
pub(super) const RUNTIME_NOTE: &str = "<runtime_info>You are running in Forge through the Claude Code harness. Use only Forge's tools. Mention this only when asked.</runtime_info>";

/// Claude Code cuts MCP tool descriptions at this many characters.
const DESCRIPTION_LIMIT: usize = 2048;

/// Loads an image attachment as `(media type, base64)`, or `None` when it
/// is gone.
pub(super) type ImageLoader<'a> = &'a (dyn Fn(&ImageRef) -> Option<(String, String)> + Sync);

/// The MCP name Claude sees for a Forge tool. Claude Code limits a full
/// tool name, `mcp__forge__` included, to 64 `[A-Za-z0-9_-]` characters;
/// any other name is sanitised and given a hash suffix.
pub(super) fn short_name(name: &str) -> String {
    let limit = 64 - TOOL_PREFIX.len();
    let valid = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if !name.is_empty() && name.len() <= limit && name.chars().all(valid) {
        return name.to_string();
    }
    let hash = hex(&Sha256::digest(name.as_bytes()));
    let stem: String = name
        .chars()
        .map(|c| if valid(c) { c } else { '_' })
        .take(limit - 11)
        .collect();
    format!("{stem}_{}", &hash[..10])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Maps the names Claude calls back to Forge's tool names.
#[derive(Debug, Clone, Default)]
pub(super) struct ToolNames {
    by_claude_name: HashMap<String, String>,
}

impl ToolNames {
    pub(super) fn new(tools: &[ToolDescriptor]) -> Self {
        Self {
            by_claude_name: tools
                .iter()
                .map(|tool| {
                    (
                        format!("{TOOL_PREFIX}{}", short_name(&tool.name)),
                        tool.name.clone(),
                    )
                })
                .collect(),
        }
    }

    /// The Forge tool behind Claude's `mcp__forge__…` name. `None` for a
    /// tool Forge did not offer; Claude Code answers those calls itself.
    pub(super) fn original(&self, claude_name: &str) -> Option<&str> {
        self.by_claude_name.get(claude_name).map(String::as_str)
    }
}

/// The MCP `tools/list` entries for Forge's tools.
pub(super) fn tool_definitions(tools: &[ToolDescriptor]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut schema = tool.input_schema.clone();
            if !schema.is_object() {
                schema = json!({"type": "object", "properties": {}});
            }
            if schema.get("type").is_none() {
                schema["type"] = json!("object");
            }
            json!({
                "name": short_name(&tool.name),
                "description": truncate(&tool.description, DESCRIPTION_LIMIT),
                "inputSchema": schema,
                // Claude runs non-read-only tools one at a time, in order.
                "annotations": {"readOnlyHint": false},
                "_meta": {"anthropic/alwaysLoad": true, "anthropic/maxResultSizeChars": 500000},
            })
        })
        .collect()
}

fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => text[..end].to_string(),
        None => text.to_string(),
    }
}

/// Forge's system messages at the head of the transcript, which become the
/// appended part of Claude Code's system prompt.
pub(super) fn system_prompt(messages: &[Message]) -> String {
    let forge: Vec<&str> = messages
        .iter()
        .take_while(|message| message.role == MessageRole::System)
        .map(|message| message.content.as_str())
        .filter(|content| !content.trim().is_empty())
        .collect();
    if forge.is_empty() {
        return RUNTIME_NOTE.to_string();
    }
    format!(
        "{RUNTIME_NOTE}\n\n# Instructions from Forge\n\n{}",
        forge.join("\n\n")
    )
}

/// The conversation after the leading system messages: what Claude must
/// see as history.
pub(super) fn conversation(messages: &[Message]) -> &[Message] {
    let start = messages
        .iter()
        .position(|message| message.role != MessageRole::System)
        .unwrap_or(messages.len());
    &messages[start..]
}

/// A digest of `messages`, used to tell whether Claude's copy of the
/// history still matches Forge's.
pub(super) fn fingerprint(messages: &[Message]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for message in messages {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
            _ => "other",
        };
        let calls: Vec<Value> = message
            .tool_calls
            .iter()
            .map(|call| json!([call.id, call.name, call.arguments]))
            .collect();
        let images: Vec<&str> = message
            .attachments
            .iter()
            .map(|image| image.path.as_str())
            .collect();
        let part = json!([role, message.content, message.tool_call_id, calls, images]);
        hasher.update(part.to_string().as_bytes());
        hasher.update([0]);
    }
    hasher.finalize().into()
}

fn image_block(image: &ImageRef, load: ImageLoader<'_>) -> Value {
    match load(image) {
        Some((media_type, data)) => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }),
        None => json!({
            "type": "text",
            "text": format!("image at `{}` is no longer available", image.path),
        }),
    }
}

fn text_block(text: impl Into<String>) -> Value {
    json!({"type": "text", "text": text.into()})
}

fn role_tag(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
        _ => "message",
    }
}

const CHECKPOINT_OPEN: &str = "<conversation-checkpoint>\nThe following is conversation history from Forge that you have not seen. Treat it as prior conversation, not a new request. Continue with the messages after this checkpoint.";
const CHECKPOINT_CLOSE: &str = "</conversation-checkpoint>";

/// The user message that brings Claude up to date with `messages`.
///
/// A single user message is sent as-is. Anything else — history Claude did
/// not write, tool results, or another model's replies — is sent as one
/// user message: the history wrapped in `<conversation-checkpoint>` as
/// tagged text (Claude cannot be given turns it did not take), followed by
/// the trailing user or system messages as the new request. Images stay
/// real image blocks.
pub(super) fn catch_up(messages: &[Message], load: ImageLoader<'_>) -> Vec<Value> {
    let request_start = messages
        .iter()
        .rposition(|message| !matches!(message.role, MessageRole::User | MessageRole::System))
        .map_or(0, |index| index + 1);
    let (history, request) = messages.split_at(request_start);
    let mut blocks = Vec::new();
    if !history.is_empty() {
        blocks.push(text_block(CHECKPOINT_OPEN));
        for message in history {
            push_tagged(&mut blocks, message, load);
        }
        blocks.push(text_block(CHECKPOINT_CLOSE));
    }
    for message in request {
        if !message.content.is_empty() {
            blocks.push(text_block(message.content.clone()));
        }
        for image in &message.attachments {
            blocks.push(image_block(image, load));
        }
    }
    if request.is_empty() {
        blocks.push(text_block(
            "Continue from where the conversation above left off.",
        ));
    }
    if blocks.is_empty() {
        blocks.push(text_block(" "));
    }
    blocks
}

fn push_tagged(blocks: &mut Vec<Value>, message: &Message, load: ImageLoader<'_>) {
    let tag = role_tag(message.role);
    blocks.push(text_block(format!("<{tag}>")));
    if message.role == MessageRole::Tool {
        let mut result = json!({
            "id": message.tool_call_id,
            "name": message.name,
            "output": message.content,
        });
        if !message.outcome.is_success() {
            result["error"] = json!(true);
        }
        blocks.push(text_block(format!(
            "<tool-result>\n{result}\n</tool-result>"
        )));
    } else if !message.content.is_empty() {
        blocks.push(text_block(message.content.clone()));
    }
    for call in &message.tool_calls {
        let call = json!({"id": call.id, "name": call.name, "input": call.arguments});
        blocks.push(text_block(format!("<tool-call>\n{call}\n</tool-call>")));
    }
    for image in &message.attachments {
        blocks.push(image_block(image, load));
    }
    blocks.push(text_block(format!("</{tag}>")));
}

/// The MCP `tools/call` result for a Forge tool message.
pub(super) fn tool_result(message: &Message, load: ImageLoader<'_>) -> Value {
    let mut content = vec![json!({"type": "text", "text": message.content})];
    for image in &message.attachments {
        match load(image) {
            Some((media_type, data)) => {
                content.push(json!({"type": "image", "data": data, "mimeType": media_type}))
            }
            None => content.push(json!({
                "type": "text",
                "text": format!("image at `{}` is no longer available", image.path),
            })),
        }
    }
    json!({"content": content, "isError": !message.outcome.is_success()})
}

/// True when a tool result says the user stopped the call.
pub(super) fn was_cancelled(message: &Message) -> bool {
    matches!(message.outcome, ExecutionOutcome::Cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::{SideEffectClass, ToolCall};

    fn no_images(_: &ImageRef) -> Option<(String, String)> {
        None
    }

    fn descriptor(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.into(),
            description: "d".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            side_effect_class: SideEffectClass::Read,
            idempotent: true,
        }
    }

    #[test]
    fn tool_names_fit_claude_codes_limit_and_map_back() {
        assert_eq!(short_name("read_file"), "read_file");
        let mcp = short_name("mcp:github:create_pull_request");
        assert!(mcp.starts_with("mcp_github_create_pull_request_"), "{mcp}");
        let long = short_name(&"x".repeat(80));
        assert!(TOOL_PREFIX.len() + long.len() <= 64, "{long}");
        assert_ne!(short_name(&"x".repeat(80)), short_name(&"x".repeat(81)));

        let names = ToolNames::new(&[descriptor("read_file"), descriptor("mcp:gh:issue")]);
        assert_eq!(names.original("mcp__forge__read_file"), Some("read_file"));
        assert_eq!(
            names.original(&format!("mcp__forge__{}", short_name("mcp:gh:issue"))),
            Some("mcp:gh:issue")
        );
        assert_eq!(names.original("glob"), None);
    }

    #[test]
    fn a_single_user_message_is_sent_as_is() {
        let blocks = catch_up(&[Message::new(MessageRole::User, "hello")], &no_images);
        assert_eq!(blocks, vec![json!({"type": "text", "text": "hello"})]);
    }

    #[test]
    fn unseen_history_is_wrapped_in_a_checkpoint() {
        let mut assistant = Message::new(MessageRole::Assistant, "looking");
        assistant.tool_calls = vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "a"}),
        }];
        let mut result = Message::new(MessageRole::Tool, "contents");
        result.tool_call_id = Some("call_1".into());
        result.name = Some("read_file".into());
        let messages = [
            Message::new(MessageRole::User, "read a"),
            assistant,
            result,
            Message::new(MessageRole::User, "now b"),
        ];
        let blocks = catch_up(&messages, &no_images);
        let text: Vec<&str> = blocks
            .iter()
            .map(|block| block["text"].as_str().unwrap())
            .collect();
        assert!(text[0].starts_with("<conversation-checkpoint>"));
        assert!(text.contains(&"read a"));
        assert!(text
            .iter()
            .any(|t| t.contains("<tool-call>") && t.contains("call_1")));
        assert!(text
            .iter()
            .any(|t| t.contains("<tool-result>") && t.contains("contents")));
        assert_eq!(text[text.len() - 2], CHECKPOINT_CLOSE);
        assert_eq!(text[text.len() - 1], "now b");
    }

    #[test]
    fn tool_results_carry_errors_and_images() {
        let mut message = Message::new(MessageRole::Tool, "failed");
        message.outcome = ExecutionOutcome::Failed { exit_code: Some(1) };
        message.attachments = vec![ImageRef::new("shot.png", "image/png", 1)];
        let loaded = |_: &ImageRef| Some(("image/png".to_string(), "AAAA".to_string()));
        let result = tool_result(&message, &loaded);
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["content"][1]["type"], json!("image"));
        assert_eq!(result["content"][1]["mimeType"], json!("image/png"));
    }

    #[test]
    fn the_system_prompt_appends_forge_instructions() {
        let messages = [
            Message::new(MessageRole::System, "Project rules"),
            Message::new(MessageRole::User, "hi"),
        ];
        let prompt = system_prompt(&messages);
        assert!(prompt.starts_with(RUNTIME_NOTE));
        assert!(prompt.ends_with("Project rules"));
        assert_eq!(conversation(&messages).len(), 1);
    }

    #[test]
    fn fingerprints_change_with_content() {
        let a = [Message::new(MessageRole::User, "a")];
        let b = [Message::new(MessageRole::User, "b")];
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }
}
