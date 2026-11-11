//! Tool names the wire accepts.
//!
//! Chat Completions and Anthropic Messages restrict a tool name to letters,
//! digits, `_` and `-`, at most 64 characters. Forge's MCP tools are named
//! `mcp:<server>:<tool>`, which breaks both rules, so a request that carried
//! one failed for as long as the server was configured. The request goes out
//! under a stable alias, and the reply (and the live stream) is mapped back,
//! so the rest of Forge only ever sees the real name. The Responses wire does
//! the same in its own module.

use std::collections::BTreeMap;

use forge_types::{MessageRole, ModelResponse, ModelStreamEvent};

use super::responses::{digest, tool_aliases};
use crate::{ModelRequest, StreamEventTx};

pub(super) struct ToolNames {
    to_wire: BTreeMap<String, String>,
    to_forge: BTreeMap<String, String>,
}

fn accepted(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// The alias for a name that is not among the request's declared tools (a
/// call in the transcript to a tool that is hidden this turn). Stable, so it
/// matches what earlier requests sent.
fn standalone_alias(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .take(55)
        .collect();
    format!("{}_{}", stem.trim_matches('_'), digest(name, 8))
}

impl ToolNames {
    pub(super) fn for_request(req: &ModelRequest) -> Self {
        // Declared tools first, with the collision handling the Responses
        // wire uses; then any other name the transcript mentions.
        let mut to_wire = tool_aliases(req);
        for message in req.messages.iter() {
            let named = message
                .tool_calls
                .iter()
                .map(|call| call.name.as_str())
                .chain(
                    (message.role == MessageRole::Tool)
                        .then_some(message.name.as_deref())
                        .flatten(),
                );
            for name in named {
                if !to_wire.contains_key(name) {
                    let alias = if accepted(name) {
                        name.to_string()
                    } else {
                        standalone_alias(name)
                    };
                    to_wire.insert(name.to_string(), alias);
                }
            }
        }
        to_wire.retain(|name, alias| name != alias);
        let to_forge = to_wire
            .iter()
            .map(|(name, alias)| (alias.clone(), name.clone()))
            .collect();
        Self { to_wire, to_forge }
    }

    fn wire<'a>(&'a self, name: &'a str) -> &'a str {
        self.to_wire.get(name).map_or(name, String::as_str)
    }

    fn forge<'a>(&'a self, name: &'a str) -> &'a str {
        self.to_forge.get(name).map_or(name, String::as_str)
    }

    /// `req` with every tool name the wire would reject replaced by its alias.
    pub(super) fn apply(&self, mut req: ModelRequest) -> ModelRequest {
        if self.to_wire.is_empty() {
            return req;
        }
        for tool in &mut req.tools {
            tool.name = self.wire(&tool.name).to_string();
        }
        for message in req.messages.iter_mut() {
            for call in &mut message.tool_calls {
                call.name = self.wire(&call.name).to_string();
            }
            if let Some(name) = message.name.as_mut() {
                *name = self.wire(name).to_string();
            }
        }
        req
    }

    pub(super) fn restore(&self, response: &mut ModelResponse) {
        for call in &mut response.tool_calls {
            call.name = self.forge(&call.name).to_string();
        }
    }

    /// A sender that maps tool names in streamed events back before passing
    /// them on. `None` in, `None` out; unchanged when nothing was aliased.
    pub(super) fn restoring(&self, tx: Option<StreamEventTx>) -> Option<StreamEventTx> {
        let tx = tx?;
        if self.to_forge.is_empty() {
            return Some(tx);
        }
        let to_forge = self.to_forge.clone();
        let (inner, events) = std::sync::mpsc::channel::<ModelStreamEvent>();
        std::thread::spawn(move || {
            while let Ok(mut event) = events.recv() {
                if let ModelStreamEvent::ToolCallStart { name, .. } = &mut event {
                    if let Some(real) = to_forge.get(name.as_str()) {
                        *name = real.clone();
                    }
                }
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
        Some(inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::{Message, ToolCall, ToolDescriptor};

    fn request(tool: &str) -> ModelRequest {
        let mut call = Message::new(MessageRole::Assistant, "");
        call.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: tool.into(),
            arguments: serde_json::json!({}),
        }];
        let mut result = Message::new(MessageRole::Tool, "ok");
        result.tool_call_id = Some("c1".into());
        result.name = Some(tool.into());
        ModelRequest {
            messages: vec![call, result].into(),
            tools: vec![ToolDescriptor {
                name: tool.into(),
                description: String::new(),
                input_schema: serde_json::json!({}),
                side_effect_class: forge_types::SideEffectClass::Read,
                idempotent: true,
            }],
            model: "m".into(),
            workspace_root: std::path::PathBuf::new(),
            session_id: None,
            reasoning_effort: None,
            thinking_enabled: false,
            prompt_cache: false,
            side_call: false,
        }
    }

    #[test]
    fn an_mcp_tool_goes_out_under_an_accepted_name_and_comes_back() {
        let real = "mcp:github:create_issue";
        let names = ToolNames::for_request(&request(real));
        let sent = names.apply(request(real));

        let alias = sent.tools[0].name.clone();
        assert!(accepted(&alias), "{alias}");
        assert_eq!(sent.messages[0].tool_calls[0].name, alias);
        assert_eq!(sent.messages[1].name.as_deref(), Some(alias.as_str()));

        let mut reply = ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "c2".into(),
                name: alias,
                arguments: serde_json::json!({}),
            }],
            usage: None,
            thinking: None,
        };
        names.restore(&mut reply);
        assert_eq!(reply.tool_calls[0].name, real);
    }

    #[test]
    fn a_hidden_tool_in_the_transcript_is_aliased_the_same_way_every_time() {
        let mut hidden = request("mcp:github:create_issue");
        hidden.tools.clear();
        let names = ToolNames::for_request(&hidden);
        let sent = names.apply(hidden);
        let alias = sent.messages[0].tool_calls[0].name.clone();
        assert!(accepted(&alias), "{alias}");
        assert_eq!(alias, standalone_alias("mcp:github:create_issue"));
    }

    #[test]
    fn accepted_names_pass_through_untouched() {
        let names = ToolNames::for_request(&request("bash"));
        assert!(names.to_wire.is_empty());
        assert_eq!(names.apply(request("bash")).tools[0].name, "bash");
    }

    #[test]
    fn streamed_tool_names_are_restored() {
        let real = "mcp:github:create_issue";
        let names = ToolNames::for_request(&request(real));
        let alias = names.wire(real).to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let inner = names.restoring(Some(tx)).unwrap();
        inner
            .send(ModelStreamEvent::ToolCallStart {
                id: "c1".into(),
                name: alias,
            })
            .unwrap();
        drop(inner);
        match rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap() {
            ModelStreamEvent::ToolCallStart { name, .. } => assert_eq!(name, real),
            other => panic!("unexpected event {other:?}"),
        }
    }
}
