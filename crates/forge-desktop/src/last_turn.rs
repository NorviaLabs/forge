//! The diffs the agent's most recent turn produced, read from the session's
//! messages. A tool result that is a unified diff becomes one card per file,
//! so "last turn" stays honest after the working tree moves on.

use forge_types::{Message, MessageRole};

/// `(path, diff lines)` for every file the last turn changed, sorted by
/// path. The turn starts after the operator's last message. A file changed
/// more than once keeps its latest diff.
pub fn diff_cards(messages: &[Message]) -> Vec<(String, Vec<String>)> {
    let start = messages
        .iter()
        .rposition(is_operator_message)
        .map(|index| index + 1)
        .unwrap_or(0);
    let mut cards: Vec<(String, Vec<String>)> = Vec::new();
    for message in &messages[start..] {
        if message.role != MessageRole::Tool || is_validation_failure(&message.content) {
            continue;
        }
        if !looks_like_diff(&message.content) {
            continue;
        }
        let name = message.name.as_deref().unwrap_or("tool");
        for (path, lines) in split_diff_sections(name, &message.content) {
            match cards.iter_mut().find(|(seen, _)| *seen == path) {
                Some(entry) => entry.1 = lines,
                None => cards.push((path, lines)),
            }
        }
    }
    cards.sort_by(|left, right| left.0.cmp(&right.0));
    cards
}

/// A message the operator sent. A repair task is written in the user role
/// by the evaluator, not by the operator, so it does not start a turn.
fn is_operator_message(message: &Message) -> bool {
    message.role == MessageRole::User && !message.content.starts_with("[REPAIR TASK")
}

/// A rejected tool call echoes its arguments, which may contain a diff that
/// was never applied.
fn is_validation_failure(content: &str) -> bool {
    content.starts_with("Tool validation error:")
        || content.contains("validation retry budget exceeded")
}

fn looks_like_diff(content: &str) -> bool {
    let has_hunk = content.lines().any(|line| {
        line.strip_prefix("@@ -")
            .and_then(|line| line.split_once(" +"))
            .is_some()
    });
    has_hunk
        && (content.starts_with("diff --git")
            || (content.lines().any(|line| line.starts_with("--- "))
                && content.lines().any(|line| line.starts_with("+++ "))))
}

/// The file a diff section is about, falling back to the tool's name.
fn extract_path_hint(name: &str, content: &str) -> String {
    for line in content.lines().take(8) {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            return rest.trim().to_string();
        }
        if let Some(rest) = line.strip_prefix("--- a/") {
            return rest.trim().to_string();
        }
        if line.contains("path") && line.contains(':') {
            if let Some(path) = line.split('"').nth(1) {
                if path.contains('/') || path.contains('.') {
                    return path.to_string();
                }
            }
        }
    }
    name.to_string()
}

/// One section per `diff --git` header, each with the path it names.
fn split_diff_sections(name: &str, content: &str) -> Vec<(String, Vec<String>)> {
    let mut sections = Vec::new();
    let mut current = Vec::new();

    for line in content.lines() {
        if line.starts_with("diff --git ") && !current.is_empty() {
            while current.last().is_some_and(String::is_empty) {
                current.pop();
            }
            let body = current.join("\n");
            sections.push((extract_path_hint(name, &body), std::mem::take(&mut current)));
        }
        current.push(line.to_string());
    }

    if !current.is_empty() {
        let body = current.join("\n");
        sections.push((extract_path_hint(name, &body), current));
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: MessageRole, name: Option<&str>, content: &str) -> Message {
        Message {
            outcome: Default::default(),
            role,
            content: content.into(),
            tool_call_id: None,
            name: name.map(str::to_string),
            thinking: None,
            thinking_duration_secs: None,
            tool_calls: vec![],
            attachments: Vec::new(),
        }
    }

    fn user(text: &str) -> Message {
        message(MessageRole::User, None, text)
    }

    fn tool(name: &str, content: &str) -> Message {
        message(MessageRole::Tool, Some(name), content)
    }

    fn diff(path: &str, old: &str, new: &str) -> String {
        format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-{old}\n+{new}\n"
        )
    }

    fn paths(cards: &[(String, Vec<String>)]) -> Vec<&str> {
        cards.iter().map(|(path, _)| path.as_str()).collect()
    }

    #[test]
    fn a_multi_file_diff_becomes_one_card_per_file_sorted_by_path() {
        let both = format!(
            "{}\n{}",
            diff("src/b.rs", "old_b", "new_b"),
            diff("src/a.rs", "old_a", "new_a")
        );
        let cards = diff_cards(&[user("change both"), tool("apply_patch", &both)]);
        assert_eq!(paths(&cards), ["src/a.rs", "src/b.rs"]);
        assert_eq!(cards[0].1.last().map(String::as_str), Some("+new_a"));
        assert_eq!(
            cards[1].1.last().map(String::as_str),
            Some("+new_b"),
            "the blank line between sections is not part of the first"
        );
    }

    #[test]
    fn only_the_turn_after_the_operators_last_message_counts() {
        let messages = [
            user("first"),
            tool("edit", &diff("old.rs", "a", "b")),
            user("second"),
            message(MessageRole::Assistant, None, "on it"),
            tool("edit", &diff("new.rs", "a", "b")),
            // The evaluator's repair task is not the operator speaking: the
            // repair's edits still belong to this turn.
            user("[REPAIR TASK 1] fix the test"),
            tool("edit", &diff("repaired.rs", "a", "b")),
        ];
        assert_eq!(paths(&diff_cards(&messages)), ["new.rs", "repaired.rs"]);
        assert!(diff_cards(&[user("nothing yet")]).is_empty());
        assert!(diff_cards(&[]).is_empty());
    }

    #[test]
    fn a_file_changed_twice_keeps_its_latest_diff() {
        let messages = [
            user("go"),
            tool("edit", &diff("lib.rs", "one", "two")),
            tool("edit", &diff("lib.rs", "two", "three")),
        ];
        let cards = diff_cards(&messages);
        assert_eq!(paths(&cards), ["lib.rs"]);
        assert!(cards[0].1.contains(&"+three".to_string()));
        assert!(!cards[0].1.contains(&"+two".to_string()));
    }

    #[test]
    fn results_that_are_not_applied_diffs_are_ignored() {
        let applied = diff("lib.rs", "a", "b");
        let messages = [
            user("go"),
            tool("bash", "ok\n3 tests passed"),
            // A rejected call echoes the diff it never applied.
            tool(
                "apply_patch",
                &format!("Tool validation error: bad patch\n{applied}"),
            ),
            // Headers without a hunk are not a diff.
            tool("read_file", "--- a/x\n+++ b/x\nplain text"),
            // The model's own prose is never a tool result.
            message(MessageRole::Assistant, None, &applied),
        ];
        assert!(diff_cards(&messages).is_empty());
    }

    #[test]
    fn a_diff_is_recognised_by_its_hunk_and_headers() {
        let git = diff("a.rs", "old", "new");
        assert!(looks_like_diff(&git));
        assert!(looks_like_diff(
            "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new"
        ));
        assert!(!looks_like_diff("@@ -1 +1 @@\n-old\n+new"), "a hunk alone");
        assert!(!looks_like_diff("diff --git a/a.rs b/a.rs\nno hunk"));
        assert_eq!(extract_path_hint("write_file", &git), "a.rs");
        assert_eq!(extract_path_hint("edit", "no headers here"), "edit");
        assert_eq!(
            extract_path_hint("edit", "path: \"src/main.rs\"\n@@ -1 +1 @@"),
            "src/main.rs"
        );
        assert_eq!(
            split_diff_sections(
                "edit",
                &format!("{git}\ndiff --git a/b.rs b/b.rs\n+++ b/b.rs")
            )
            .len(),
            2
        );
    }
}
