//! Recent-raw-tail selection.
//!
//! The checkpoint is never the whole replacement context: a slice of the most
//! recent conversation is carried over verbatim. Cutting at exactly N tokens
//! would routinely split an assistant tool call from its result, which no
//! provider accepts, so the selector walks backward to the token target and
//! then snaps to the nearest structurally valid boundary — slightly
//! overshooting the target rather than emitting invalid context.

use forge_types::{Message, MessageRole};

use crate::estimate_tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailSelection {
    /// Index into the body slice where the retained tail starts. `body.len()`
    /// means "no raw tail" (checkpoint only).
    pub start: usize,
    /// Estimated tokens in the retained tail.
    pub tokens: usize,
    /// Whether the tail begins on a user message (the preferred boundary).
    pub starts_on_user: bool,
}

/// Estimated tokens for one message, counting reasoning text — the context
/// budget pays for it on the wire, so tail selection must too.
pub fn message_tokens(message: &Message) -> usize {
    let mut tokens = estimate_tokens(&message.content);
    if let Some(thinking) = message.thinking.as_ref() {
        tokens = tokens.saturating_add(estimate_tokens(thinking));
    }
    for call in &message.tool_calls {
        tokens = tokens.saturating_add(estimate_tokens(&call.arguments.to_string()));
    }
    tokens
}

pub fn messages_tokens(messages: &[Message]) -> usize {
    messages.iter().map(message_tokens).sum()
}

/// Select the retained tail of `body` (the conversation with any leading
/// system messages already removed).
pub fn select_tail(body: &[Message], target_tokens: usize) -> TailSelection {
    let mut start = body.len();
    let mut tokens = 0usize;
    while start > 0 && tokens < target_tokens {
        start -= 1;
        tokens = tokens.saturating_add(message_tokens(&body[start]));
    }

    // Snap backward to a boundary that keeps tool calls with their results.
    while start > 0 && !is_valid_start(body, start) {
        start -= 1;
    }

    // Don't open the tail on an assistant reply whose user request is one
    // message behind: keeping the pair together costs a single message and
    // avoids handing the model an answer with no visible question.
    if start > 0
        && start < body.len()
        && body[start].role == MessageRole::Assistant
        && body[start - 1].role == MessageRole::User
        && is_valid_start(body, start - 1)
    {
        start -= 1;
    }

    let tokens = messages_tokens(&body[start..]);
    TailSelection {
        start,
        tokens,
        starts_on_user: body.get(start).is_some_and(|m| m.role == MessageRole::User),
    }
}

/// True when `body[start..]` is a self-contained conversation slice.
///
/// Two rules: the slice may not open on a tool result (its call would be
/// gone), and every tool result inside it must be answered by an assistant
/// tool call that is also inside it.
pub fn is_valid_start(body: &[Message], start: usize) -> bool {
    if start >= body.len() {
        return true;
    }
    match body[start].role {
        MessageRole::User => {}
        MessageRole::Assistant if body[start].tool_calls.is_empty() => {}
        _ => return false,
    }
    tool_calls_are_paired(&body[start..])
}

/// Every `Tool` message in `slice` has a preceding assistant call for it.
pub fn tool_calls_are_paired(slice: &[Message]) -> bool {
    let mut open: Vec<&str> = Vec::new();
    for message in slice {
        match message.role {
            MessageRole::Assistant => {
                open.extend(message.tool_calls.iter().map(|call| call.id.as_str()));
            }
            MessageRole::Tool => {
                let Some(id) = message.tool_call_id.as_deref() else {
                    return false;
                };
                if !open.contains(&id) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Every assistant tool call in `slice` has a matching result. Used by
/// candidate validation, which must not install a context whose final
/// exchange asks the provider to answer a call that was cut away.
pub fn tool_results_are_complete(slice: &[Message]) -> bool {
    let answered: Vec<&str> = slice
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    slice
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .flat_map(|m| m.tool_calls.iter())
        .all(|call| answered.contains(&call.id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::ToolCall;

    fn user(text: &str) -> Message {
        Message::new(MessageRole::User, text)
    }

    fn assistant(text: &str) -> Message {
        Message::new(MessageRole::Assistant, text)
    }

    fn assistant_call(id: &str, text: &str) -> Message {
        let mut message = Message::new(MessageRole::Assistant, text);
        message.tool_calls = vec![ToolCall {
            id: id.into(),
            name: "bash".into(),
            arguments: serde_json::json!({}),
        }];
        message
    }

    fn tool_result(id: &str, text: &str) -> Message {
        let mut message = Message::new(MessageRole::Tool, text);
        message.tool_call_id = Some(id.into());
        message.name = Some("bash".into());
        message
    }

    /// 40 chars ≈ 10 tokens under the ~4-chars/token estimate.
    fn filler(tag: &str) -> String {
        format!("{tag}{}", "x".repeat(39))
    }

    #[test]
    fn retains_the_most_recent_messages_up_to_the_target() {
        let body: Vec<Message> = (0..10)
            .map(|i| {
                if i % 2 == 0 {
                    user(&filler("u"))
                } else {
                    assistant(&filler("a"))
                }
            })
            .collect();
        // Each message is ~10 tokens; a 30-token target keeps the last few.
        let selection = select_tail(&body, 30);
        assert!(selection.start > 0, "must not retain the whole body");
        assert!(selection.tokens >= 30, "target is a floor, not a ceiling");
        assert!(selection.starts_on_user);
    }

    #[test]
    fn never_splits_a_tool_call_from_its_result() {
        let body = vec![
            user("older request"),
            assistant_call("c1", "calling"),
            tool_result("c1", &filler("r")),
            user("newer request"),
            assistant_call("c2", "calling again"),
            tool_result("c2", &filler("r")),
            assistant("done"),
        ];
        // A target that lands the naive cut in the middle of the c2 pair.
        for target in 1..=12 {
            let selection = select_tail(&body, target);
            let tail = &body[selection.start..];
            assert!(
                tool_calls_are_paired(tail),
                "target {target} produced an orphaned tool result at start {}",
                selection.start
            );
            assert_ne!(
                body[selection.start].role,
                MessageRole::Tool,
                "target {target} started the tail on a tool result"
            );
        }
    }

    #[test]
    fn prefers_a_user_boundary_when_one_is_close_behind() {
        let body = vec![
            user("first"),
            assistant("first answer"),
            user("second"),
            assistant("second answer"),
        ];
        // The naive walk stops on the final assistant message; the selector
        // should step back one to open on "second".
        let selection = select_tail(&body, 1);
        assert_eq!(selection.start, 2);
        assert!(selection.starts_on_user);
    }

    #[test]
    fn a_zero_target_selects_an_empty_tail() {
        let body = vec![user("a"), assistant("b")];
        let selection = select_tail(&body, 0);
        assert_eq!(selection.start, body.len());
        assert_eq!(selection.tokens, 0);
    }

    #[test]
    fn falls_back_to_the_whole_body_when_no_valid_boundary_exists() {
        // A body that is entirely one unbreakable tool exchange.
        let body = vec![
            user("go"),
            assistant_call("c1", ""),
            tool_result("c1", &filler("r")),
        ];
        let selection = select_tail(&body, 1);
        assert_eq!(selection.start, 0);
        assert!(tool_calls_are_paired(&body[selection.start..]));
    }

    #[test]
    fn empty_body_selects_an_empty_tail() {
        let selection = select_tail(&[], 1_000);
        assert_eq!(selection.start, 0);
        assert_eq!(selection.tokens, 0);
    }

    #[test]
    fn detects_an_unanswered_assistant_call() {
        let complete = vec![assistant_call("c1", ""), tool_result("c1", "ok")];
        assert!(tool_results_are_complete(&complete));
        let dangling = vec![assistant_call("c1", "")];
        assert!(!tool_results_are_complete(&dangling));
    }

    #[test]
    fn message_tokens_counts_thinking_and_tool_arguments() {
        let mut message = assistant("abcd");
        message.thinking = Some("x".repeat(400));
        message.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "notes.txt"}),
        }];
        let expected = estimate_tokens("abcd")
            + estimate_tokens(&"x".repeat(400))
            + estimate_tokens(&message.tool_calls[0].arguments.to_string());
        assert_eq!(message_tokens(&message), expected);
        assert_eq!(messages_tokens(&[message]), expected);
    }

    #[test]
    fn is_valid_start_rejects_opening_on_a_tool_result() {
        let body = vec![
            user("go"),
            assistant_call("c1", ""),
            tool_result("c1", "ok"),
        ];
        assert!(is_valid_start(&body, 0));
        assert!(
            !is_valid_start(&body, 1),
            "assistant-with-calls is not a start"
        );
        assert!(
            !is_valid_start(&body, 2),
            "a tool result cannot open the tail"
        );
        assert!(is_valid_start(&body, body.len()));
    }
}
