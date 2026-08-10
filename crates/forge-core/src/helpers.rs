//! Free functions shared across the session modules: prompt assembly,
//! tool-call classification, and journal restoration.
//!
//! Split out of `lib.rs`; moved verbatim.

use crate::*;

pub(crate) const SYSTEM_PROMPT: &str = include_str!("system_prompt.md");

/// Discovery stage of progressive disclosure (issue #226): a skill with
/// frontmatter contributes only its `name` + `description` here — its full
/// `SKILL.md` body is fetched on demand via the `load_skill` tool once the
/// model judges the description relevant. A skill without frontmatter has no
/// `description` to show instead, so its whole body is injected eagerly,
/// matching pre-#226 behavior.
pub(crate) fn assemble_system_prompt(
    agents_md: &str,
    skills: &[forge_context::SkillManifest],
) -> String {
    let mut prompt = SYSTEM_PROMPT.trim_end().to_owned();

    if !agents_md.trim().is_empty() {
        prompt.push_str("\n\n# Project Instructions\n\nAGENTS.md:\n");
        prompt.push_str(agents_md);
    }

    if !skills.is_empty() {
        prompt.push_str(
            "\n\n# Skills\n\nEach skill below is listed by name and description. When a task \
matches a skill's description, call the `load_skill` tool with that name to load its full \
instructions before proceeding.",
        );
        for skill in skills {
            prompt.push_str("\n\n## ");
            prompt.push_str(&skill.name);
            prompt.push_str("\n\n");
            if skill.has_frontmatter {
                prompt.push_str(skill.description.trim());
            } else {
                prompt.push_str(skill.body.trim());
            }
        }
    }

    prompt
}

/// Durable marker for a terminal turn failure summary in session messages.
/// Presentation maps this to TurnFailure; it is never a user-facing answer.
pub const TURN_FAILED_MARKER: &str = "[forge.turn_failed]";

// --- Completion-evidence helpers -------------------------------------------
//
// These are pure/near-pure helpers used to classify a turn's expectation and
// to build `EvidenceEntry` values from real tool calls and filesystem state.
// None of them read the model's own text.

/// Lightweight, local mirror of `forge_tools::builtins::GitArgs` so this
/// module doesn't need to depend on that crate's private argument shape —
/// just enough to recover the subcommand and its arguments from a `ToolCall`.
#[derive(serde::Deserialize)]
pub(crate) struct GitCallArgsLite {
    pub(crate) subcommand: String,
    #[serde(default)]
    pub(crate) args: Vec<String>,
}

pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

pub(crate) fn search_result_count(content: &str) -> usize {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(hits) = value.get("hits").and_then(|hits| hits.as_array()) {
            return hits.len();
        }
        if value
            .get("message")
            .and_then(|message| message.as_str())
            .is_some_and(|message| message.contains("no matches found"))
        {
            return 0;
        }
    }
    if content.trim() == "no matches found" || content.contains("no matches found") {
        0
    } else {
        content.lines().count()
    }
}

/// Content hash of a workspace file. `None` means the path does not exist
/// (or isn't readable) — the convention `EvidenceEntry` documents.
pub(crate) async fn hash_file(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = tokio::fs::read(path).await.ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

/// Parse `*** Add/Update/Delete File: <path>` header lines out of an
/// `apply_patch` call's own `patch` argument. Deliberately does not
/// duplicate the tool's hunk-application logic — only enough to know which
/// paths a patch touched and whether the file should end up present or gone.
pub(crate) fn parse_patch_paths(patch: &str) -> Vec<(String, FileEffectKind)> {
    patch
        .lines()
        .filter_map(|line| {
            for (prefix, kind) in [
                ("*** Add File: ", FileEffectKind::Modified),
                ("*** Update File: ", FileEffectKind::Modified),
                ("*** Delete File: ", FileEffectKind::Deleted),
            ] {
                if let Some(p) = line.strip_prefix(prefix) {
                    return Some((p.to_string(), kind));
                }
            }
            None
        })
        .collect()
}

/// A short label for a bash call, e.g. `"cargo test"`, used both to classify
/// the turn and to name the operation in evidence/user-facing messages.
pub(crate) fn bash_label(arguments: &serde_json::Value) -> String {
    let command = arguments
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("bash")
        .trim();
    let first_line = command.lines().next().unwrap_or(command);
    truncate(first_line, 60)
}

pub(crate) fn git_effect_kind(subcommand: &str) -> GitEffectKind {
    match subcommand {
        "commit" => GitEffectKind::CommitCreated,
        "add" => GitEffectKind::Staged,
        "checkout" | "switch" => GitEffectKind::BranchChanged,
        "restore" => GitEffectKind::Restored,
        _ => GitEffectKind::CommandOnly,
    }
}

/// Collapse repeated attempts at the same target down to the last one, order
/// otherwise unspecified. A model that retries a failed write/command/git
/// call until it succeeds should only be judged on the final attempt, not
/// penalized for the earlier failures — this is what makes that distinction
/// from "5 required edits, 3 succeeded" (genuinely distinct targets).
pub(crate) fn dedup_keep_last<T: Clone>(items: Vec<T>, key_fn: impl Fn(&T) -> String) -> Vec<T> {
    let mut map: HashMap<String, T> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for item in items {
        let key = key_fn(&item);
        if !map.contains_key(&key) {
            order.push(key.clone());
        }
        map.insert(key, item);
    }
    order
        .into_iter()
        .filter_map(|key| map.remove(&key))
        .collect()
}

/// True when `text` contains what looks like an unparsed tool-call attempt —
/// a real, registered tool name used in call-shaped syntax (a JSON object
/// key, or a bare `{"tool_name", ...}` element) — rather than genuine prose.
/// This is exactly what a model emits when it tries to invoke a tool as plain
/// text instead of through the real structured tool-calling wire format (most
/// often a smaller/local model that doesn't reliably follow the function-call
/// API shape): the response has zero real `ToolCall`s, so without this check
/// it looks identical to a legitimate no-op chat answer and would otherwise
/// be marked `Completed` under `TaskExpectation::ReadOnly`.
///
/// Deliberately conservative: prose that merely *mentions* a tool by name
/// (e.g. "you can use the write_file tool") does not match, because the quoted
/// name isn't immediately followed by `:` or `,` the way a JSON key or a bare
/// call argument would be.
pub(crate) fn looks_like_dangling_tool_call(text: &str, tool_names: &[String]) -> bool {
    for name in tool_names {
        let needle = format!("\"{name}\"");
        let mut search_from = 0;
        while let Some(offset) = text[search_from..].find(needle.as_str()) {
            let match_end = search_from + offset + needle.len();
            let next_non_space = text[match_end..]
                .find(|c: char| !c.is_whitespace())
                .map(|i| match_end + i);
            if let Some(i) = next_non_space {
                if matches!(text.as_bytes()[i], b':' | b',') {
                    return true;
                }
            }
            search_from = search_from + offset + 1;
        }
    }
    false
}

/// Classify a finished turn's expectation from the tool calls the model
/// actually issued — not from natural-language intent inference over the
/// user's request. Precedence (a turn can only be one category):
/// `GitOperation > FileEdit > ToolExecution > Search > ReadOnly`.
pub(crate) fn classify_turn(calls: &[ToolCall]) -> TaskExpectation {
    let mut git_items: Vec<(String, String, GitEffectKind)> = Vec::new();
    let mut file_items: Vec<(String, String, FileEffectKind)> = Vec::new();
    let mut tool_items: Vec<(String, String)> = Vec::new();
    let mut search_count = 0usize;

    for call in calls {
        match call.name.as_str() {
            "git" => {
                if let Ok(a) = serde_json::from_value::<GitCallArgsLite>(call.arguments.clone()) {
                    let sub = a.subcommand.trim().to_ascii_lowercase();
                    git_items.push((call.id.clone(), sub.clone(), git_effect_kind(&sub)));
                }
            }
            "write_file" => {
                if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                    file_items.push((call.id.clone(), path.to_string(), FileEffectKind::Modified));
                }
            }
            "apply_patch" => {
                if let Some(patch) = call.arguments.get("patch").and_then(|v| v.as_str()) {
                    for (path, kind) in parse_patch_paths(patch) {
                        file_items.push((call.id.clone(), path, kind));
                    }
                }
            }
            "bash" => tool_items.push((call.id.clone(), bash_label(&call.arguments))),
            "fffind" | "ffgrep" => search_count += 1,
            _ => {}
        }
    }

    if !git_items.is_empty() {
        let deduped = dedup_keep_last(git_items, |(_, sub, _)| sub.clone());
        return TaskExpectation::GitOperation {
            expected_effects: deduped
                .into_iter()
                .map(|(operation_id, command, effect)| GitEffectExpectation {
                    operation_id,
                    command,
                    effect,
                })
                .collect(),
        };
    }
    if !file_items.is_empty() {
        let deduped = dedup_keep_last(file_items, |(_, path, _)| path.clone());
        return TaskExpectation::FileEdit {
            expected_effects: deduped
                .into_iter()
                .map(|(operation_id, path, kind)| FileEffectExpectation {
                    operation_id,
                    path,
                    kind,
                })
                .collect(),
        };
    }
    if !tool_items.is_empty() {
        let deduped = dedup_keep_last(tool_items, |(_, label)| label.clone());
        return TaskExpectation::ToolExecution {
            required_tools: deduped
                .into_iter()
                .map(|(operation_id, tool_name)| ToolExpectation {
                    operation_id,
                    tool_name,
                })
                .collect(),
        };
    }
    if search_count > 0 {
        return TaskExpectation::Search {
            required_operations: search_count,
        };
    }
    TaskExpectation::ReadOnly
}

/// Pre-call git state needed to verify a subcommand's repository effect
/// afterward. `None`-shaped variants mean "not practical to verify" per
/// subcommand.
#[derive(Clone)]
pub(crate) enum GitPre {
    Head(Option<String>),
    Branch(Option<String>),
    RestorePath(Option<String>),
    NotVerified,
}

/// Remove structural protocol control markers from final-answer text before
/// persistence. Not phrase filtering — only known control envelopes.
pub(crate) fn strip_protocol_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\\confidence{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "\\confidence{".len()..];
        if let Some(end) = after.find('}') {
            rest = &after[end + 1..];
        } else {
            // Unterminated marker, e.g. model output truncated mid-annotation.
            // Rewind to the marker so the tail is emitted exactly once: the
            // prefix was already pushed above, so leaving `rest` untouched
            // would duplicate it.
            rest = &rest[start..];
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Reconstruct the `WaitReason` a restored session was blocked on, from the
/// raw HITL payload the journal replayed. Only `Approval` is ever produced
/// today — the sole wait reason with a real runtime producer.
pub(crate) fn restored_wait_reason(pending_hitl: &Option<serde_json::Value>) -> Option<WaitReason> {
    let payload: HitlPayload = serde_json::from_value(pending_hitl.clone()?).ok()?;
    Some(WaitReason::Approval {
        request_id: payload.call_id.clone(),
        payload,
    })
}

/// Map forge-durable's lightweight replay mirror into forge-core's
/// `QueuedTask`, attaching the session id (not itself journaled per item).
pub(crate) fn restored_queue_items(
    session_id: SessionId,
    items: Vec<forge_durable::RestoredQueueItem>,
) -> Vec<QueuedTask> {
    items
        .into_iter()
        .map(|item| QueuedTask {
            id: item.id,
            session_id,
            text: item.text,
            created_at: item.created_at,
            status: item.status,
        })
        .collect()
}

/// A short, human-readable hint for a resumable session — its first user
/// message, truncated — so a `/resume` list can show more than a raw UUID
/// and timestamp. Cheap: opens and replays only the one session's journal,
/// independent of any live `AgentSession` (no tools/model/governance
/// needed). Returns `None` on any read/replay error or an empty journal —
/// callers should fall back to showing just the id/timestamp in that case,
/// never fail the whole listing over one unreadable session.
pub async fn session_title_hint(
    journal_dir: &Path,
    session_id: forge_types::SessionId,
) -> Option<String> {
    let journal = Journal::open(journal_dir, session_id).await.ok()?;
    let state = journal.replay(session_id).await.ok()?;
    let first = state.user_messages.into_iter().next()?;
    let mut title: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_LEN: usize = 60;
    if title.chars().count() > MAX_LEN {
        title = title.chars().take(MAX_LEN).collect::<String>() + "…";
    }
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}
