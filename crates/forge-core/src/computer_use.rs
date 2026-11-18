//! Session-side rules for the computer-use tools (`browser_*`, `desktop_*`,
//! and the web app tools `app_start`, `app_stop`, `web_check`).
//!
//! The tools themselves live in `forge-browser` and `forge-screen`; the
//! session only needs to know them by name: who may be offered them, and how
//! much of their screenshot history is sent back to the model.

use std::collections::BTreeSet;

use forge_types::Message;

/// The fewest recent computer-use screenshots a request carries (and the
/// size of the batches older ones are dropped in). Older ones are replaced by
/// a note: a screenshot is stale once the screen has moved on, and each costs
/// over a thousand tokens on every request.
pub(crate) const SCREENSHOTS_KEPT: usize = 3;

/// The note an older screenshot is replaced by.
pub(crate) const SCREENSHOT_OMITTED_NOTE: &str = "[earlier screenshot omitted]";

/// Whether `tool` is one of the computer-use tools. The web app tools
/// count: a child agent never gets them, and `web_check` screenshots age
/// out like the others.
pub(crate) fn is_computer_use_tool(tool: &str) -> bool {
    tool.starts_with(forge_config::agents::BROWSER_TOOL_PREFIX)
        || tool.starts_with(forge_config::agents::DESKTOP_TOOL_PREFIX)
        || matches!(tool, "app_start" | "app_stop" | "web_check")
}

/// Whether a session may be offered, or run, the computer-use tool `tool`.
///
/// A top-level session may. A child agent may only when its tool limit
/// names the tool explicitly, which happens only for a custom agent whose
/// capabilities include it; a built-in child (no limit) never may.
pub(crate) fn computer_use_tool_permitted(
    is_subagent: bool,
    tool_limit: Option<&BTreeSet<String>>,
    tool: &str,
) -> bool {
    if !is_subagent || !is_computer_use_tool(tool) {
        return true;
    }
    tool_limit.is_some_and(|limit| limit.contains(tool))
}

/// Drop the image attachments of old computer-use tool results, noting the
/// omission in each result's text, in batches of `keep`: with `n`
/// screenshots the oldest `keep * ((n - keep) / keep)` go, so a request
/// carries between `keep` and `2 * keep - 1` of the newest. Dropping one per
/// new screenshot would rewrite an earlier message on every step, and the
/// provider's prompt cache would miss from there each time; in batches the
/// cut moves once every `keep` screenshots. Returns `None` when nothing
/// needs dropping, so the caller keeps sharing the transcript.
pub(crate) fn prune_screenshots(messages: &[Message], keep: usize) -> Option<Vec<Message>> {
    let is_screenshot_result = |message: &Message| {
        message.role == forge_types::MessageRole::Tool
            && !message.attachments.is_empty()
            && message.name.as_deref().is_some_and(is_computer_use_tool)
    };
    let keep = keep.max(1);
    let total = messages.iter().filter(|m| is_screenshot_result(m)).count();
    let mut to_drop = total.saturating_sub(keep) / keep * keep;
    if to_drop == 0 {
        return None;
    }
    let mut pruned = messages.to_vec();
    for message in &mut pruned {
        if to_drop == 0 {
            break;
        }
        if is_screenshot_result(message) {
            message.attachments.clear();
            if !message.content.is_empty() {
                message.content.push('\n');
            }
            message.content.push_str(SCREENSHOT_OMITTED_NOTE);
            to_drop -= 1;
        }
    }
    Some(pruned)
}

/// Why a computer-use tool cannot run for this session right now, when it
/// cannot: what the model is told instead of a result.
pub(crate) fn unavailable_reason(
    tool: &dyn forge_tools::Tool,
    ctx: &forge_tools::ToolContext,
) -> Option<String> {
    if tool.requires_image_input() && !ctx.image_input {
        let model = if ctx.active_model.is_empty() {
            "the active model"
        } else {
            ctx.active_model.as_str()
        };
        return Some(format!(
            "`{}` is not available: it returns screenshots, and {model} cannot see images",
            tool.name()
        ));
    }
    if !tool.is_available(ctx) {
        return Some(format!(
            "`{}` is not available: the user has turned it off in Settings, or this machine \
             does not support it",
            tool.name()
        ));
    }
    None
}
