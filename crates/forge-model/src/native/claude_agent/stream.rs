//! One Forge model step read from Claude Code's output.
//!
//! A Claude turn can span several Forge steps: when Claude calls Forge's
//! tools, the step ends at that API message's `message_stop` and Forge runs
//! the tools while Claude Code waits for their MCP results. The turn's
//! `result` message ends the last step.

use std::collections::{HashMap, HashSet};

use forge_types::{ModelResponse, ModelStreamEvent, ToolCall, Usage};
use serde_json::Value;

use super::convert::ToolNames;
use crate::{ModelError, StreamEventTx};

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StepEnd {
    /// Claude called Forge's tools and waits for their results.
    Tools,
    /// The Claude turn finished.
    Done,
}

#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    input: u32,
    read: u32,
    write: u32,
    output: u32,
}

/// Accumulates one step.
pub(super) struct Step<'a> {
    names: &'a ToolNames,
    tx: Option<&'a StreamEventTx>,
    /// Fail if Forge's MCP server is not connected when the turn starts.
    expect_tools: bool,
    text: String,
    thinking: String,
    tool_calls: Vec<ToolCall>,
    /// API message ids seen as stream events; their complete copies repeat
    /// the streamed content.
    streamed: HashSet<String>,
    message_id: String,
    /// Content-block index → Forge tool-call id, for argument deltas.
    tool_blocks: HashMap<u64, String>,
    /// Claude Code reports one API message's usage several times while it
    /// streams; keep the highest count per message.
    usage: Vec<(String, Counts)>,
    result_usage: Option<Value>,
    error_code: Option<String>,
    rejected: Option<Value>,
}

impl<'a> Step<'a> {
    pub(super) fn new(
        names: &'a ToolNames,
        tx: Option<&'a StreamEventTx>,
        expect_tools: bool,
    ) -> Self {
        Self {
            names,
            tx,
            expect_tools,
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            streamed: HashSet::new(),
            message_id: String::new(),
            tool_blocks: HashMap::new(),
            usage: Vec::new(),
            result_usage: None,
            error_code: None,
            rejected: None,
        }
    }

    fn emit(&self, event: ModelStreamEvent) {
        if let Some(tx) = self.tx {
            let _ = tx.send(event);
        }
    }

    fn push_text(&mut self, piece: &str, thinking: bool) {
        if piece.is_empty() {
            return;
        }
        if thinking {
            self.thinking.push_str(piece);
            self.emit(ModelStreamEvent::ThinkingDelta { text: piece.into() });
        } else {
            self.text.push_str(piece);
            self.emit(ModelStreamEvent::TextDelta { text: piece.into() });
        }
    }

    /// Separate a new text block from earlier text in the same step.
    fn start_block(&mut self, thinking: bool) {
        let existing = if thinking { &self.thinking } else { &self.text };
        if !existing.is_empty() {
            self.push_text("\n\n", thinking);
        }
    }

    fn update_usage(&mut self, id: &str, usage: &Value) {
        let read = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0) as u32;
        let index = match self.usage.iter().position(|(seen, _)| seen == id) {
            Some(index) => index,
            None => {
                self.usage.push((id.to_string(), Counts::default()));
                self.usage.len() - 1
            }
        };
        let counts = &mut self.usage[index].1;
        counts.input = counts.input.max(read("input_tokens"));
        counts.read = counts.read.max(read("cache_read_input_tokens"));
        counts.write = counts.write.max(read("cache_creation_input_tokens"));
        counts.output = counts.output.max(read("output_tokens"));
    }

    /// Consume one message. `Some` when the step is over.
    pub(super) fn consume(&mut self, message: &Value) -> Result<Option<StepEnd>, ModelError> {
        let top_level = message.get("parent_tool_use_id").is_none_or(Value::is_null);
        match message.get("type").and_then(Value::as_str) {
            Some("system") if message.get("subtype").and_then(Value::as_str) == Some("init") => {
                if self.expect_tools && !forge_server_connected(message) {
                    return Err(ModelError::Provider(
                        "Forge's tool server did not connect to Claude Code".into(),
                    ));
                }
            }
            Some("rate_limit_event") => {
                let info = message.get("rate_limit_info").cloned().unwrap_or_default();
                self.rejected = (info.get("status").and_then(Value::as_str) == Some("rejected"))
                    .then_some(info);
            }
            Some("stream_event") if top_level => return Ok(self.stream_event(&message["event"])),
            Some("assistant") if top_level => self.assistant(message),
            Some("result") => {
                if let Some(error) =
                    result_error(message, self.error_code.as_deref(), self.rejected.as_ref())
                {
                    return Err(error);
                }
                self.result_usage = message.get("usage").cloned();
                return Ok(Some(StepEnd::Done));
            }
            _ => {}
        }
        Ok(None)
    }

    fn stream_event(&mut self, event: &Value) -> Option<StepEnd> {
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let message = &event["message"];
                self.message_id = message
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.streamed.insert(self.message_id.clone());
                self.tool_blocks.clear();
                let id = self.message_id.clone();
                self.update_usage(&id, &message["usage"]);
            }
            Some("content_block_start") => {
                let block = &event["content_block"];
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        self.start_block(false);
                        let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                        self.push_text(text, false);
                    }
                    Some("thinking") => {
                        self.start_block(true);
                        let text = block.get("thinking").and_then(Value::as_str).unwrap_or("");
                        self.push_text(text, true);
                    }
                    Some("tool_use") => {
                        let claude_name = block.get("name").and_then(Value::as_str).unwrap_or("");
                        let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                        if let Some(name) = self.names.original(claude_name) {
                            self.tool_blocks.insert(index, id.to_string());
                            self.emit(ModelStreamEvent::ToolCallStart {
                                id: id.to_string(),
                                name: name.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        self.push_text(text, false);
                    }
                    Some("thinking_delta") => {
                        let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        self.push_text(text, true);
                    }
                    Some("input_json_delta") => {
                        if let Some(id) = self.tool_blocks.get(&index) {
                            let piece = delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            if !piece.is_empty() {
                                self.emit(ModelStreamEvent::ToolCallDelta {
                                    id: id.clone(),
                                    arguments_delta: piece.into(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                let id = self.message_id.clone();
                self.update_usage(&id, &event["usage"]);
            }
            // The API message that called Forge's tools is complete: Forge
            // runs them while Claude Code waits.
            Some("message_stop") if !self.tool_calls.is_empty() => return Some(StepEnd::Tools),
            _ => {}
        }
        None
    }

    fn assistant(&mut self, message: &Value) {
        // Synthetic API errors are diagnostics for the result, not replies.
        if let Some(code) = message.get("error").and_then(Value::as_str) {
            self.error_code = Some(code.to_string());
            return;
        }
        let body = &message["message"];
        let id = body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.update_usage(&id, &body["usage"]);
        let streamed = self.streamed.contains(&id);
        let blocks = body
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for block in &blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let claude_name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let Some(name) = self.names.original(claude_name) else {
                        continue;
                    };
                    let call = ToolCall {
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: name.to_string(),
                        arguments: block
                            .get("input")
                            .cloned()
                            .filter(Value::is_object)
                            .unwrap_or_else(|| serde_json::json!({})),
                    };
                    if self.tool_calls.iter().any(|seen| seen.id == call.id) {
                        continue;
                    }
                    if !streamed {
                        self.emit(ModelStreamEvent::ToolCallStart {
                            id: call.id.clone(),
                            name: call.name.clone(),
                        });
                    }
                    self.emit(ModelStreamEvent::ToolCallEnd { call: call.clone() });
                    self.tool_calls.push(call);
                }
                // Complete messages repeat streamed content; use them only
                // when nothing was streamed.
                Some("text") if !streamed => {
                    self.start_block(false);
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    self.push_text(text, false);
                }
                Some("thinking") if !streamed => {
                    self.start_block(true);
                    let text = block.get("thinking").and_then(Value::as_str).unwrap_or("");
                    self.push_text(text, true);
                }
                _ => {}
            }
        }
    }

    /// The step as a Forge response. Context use is the last API call's
    /// input; output adds up every call in the step.
    pub(super) fn finish(self) -> ModelResponse {
        let usage = match self.usage.last() {
            Some((_, last)) => Some(Usage {
                prompt_tokens: last
                    .input
                    .saturating_add(last.read)
                    .saturating_add(last.write),
                completion_tokens: self
                    .usage
                    .iter()
                    .map(|(_, counts)| counts.output)
                    .fold(0u32, u32::saturating_add),
                prompt_cache_read_tokens: last.read,
                prompt_cache_write_tokens: last.write,
            }),
            None => self.result_usage.as_ref().map(|usage| {
                let read = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0) as u32;
                let cache_read = read("cache_read_input_tokens");
                let cache_write = read("cache_creation_input_tokens");
                Usage {
                    prompt_tokens: read("input_tokens")
                        .saturating_add(cache_read)
                        .saturating_add(cache_write),
                    completion_tokens: read("output_tokens"),
                    prompt_cache_read_tokens: cache_read,
                    prompt_cache_write_tokens: cache_write,
                }
            }),
        };
        if let Some(tx) = self.tx {
            if let Some(usage) = &usage {
                let _ = tx.send(ModelStreamEvent::Usage {
                    usage: usage.clone(),
                });
            }
            let _ = tx.send(ModelStreamEvent::MessageEnd);
        }
        ModelResponse {
            text: self.text,
            tool_calls: self.tool_calls,
            usage,
            thinking: (!self.thinking.is_empty()).then_some(self.thinking),
        }
    }
}

fn forge_server_connected(init: &Value) -> bool {
    init.get("mcp_servers")
        .and_then(Value::as_array)
        .is_some_and(|servers| {
            servers.iter().any(|server| {
                server.get("name").and_then(Value::as_str)
                    == Some(forge_connect::claude_agent::MCP_SERVER)
                    && server.get("status").and_then(Value::as_str) == Some("connected")
            })
        })
}

/// The error a failed turn's `result` describes. Claude Code has already
/// retried, so none of these is retryable.
fn result_error(
    result: &Value,
    code: Option<&str>,
    rejected: Option<&Value>,
) -> Option<ModelError> {
    let success = result.get("subtype").and_then(Value::as_str) == Some("success");
    let status = result
        .get("api_error_status")
        .and_then(Value::as_u64)
        .filter(|status| *status >= 400);
    let is_error = result
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if success && !is_error && status.is_none() {
        return None;
    }
    let detail = if success {
        result
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        result
            .get("errors")
            .and_then(Value::as_array)
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    };
    if code == Some("authentication_failed") || status == Some(401) {
        return Some(ModelError::ProviderAuth {
            status: 401,
            message: "Claude Code is not signed in or its login expired. Run `claude auth login` in a terminal.".into(),
        });
    }
    let limited = code == Some("rate_limit") || (rejected.is_some() && status == Some(429));
    let mut message = if let Some(status @ 500..) = status {
        let what = if status == 529 {
            "Anthropic is overloaded (529).".to_string()
        } else {
            format!("Anthropic returned HTTP {status}.")
        };
        format!("{what} Claude Code exhausted its retries. Try again shortly.")
    } else if limited && detail.is_empty() {
        let window = rejected
            .and_then(|info| info.get("rateLimitType"))
            .and_then(Value::as_str)
            .map(|kind| format!(" ({})", kind.replace('_', " ")))
            .unwrap_or_default();
        format!("Claude Code usage limit reached{window}.")
    } else {
        detail
    };
    if limited {
        if let Some(resets) = rejected
            .and_then(|info| info.get("resetsAt"))
            .and_then(Value::as_i64)
        {
            message.push_str(&format!(" Resets at unix time {resets}."));
        }
    }
    if code == Some("billing_error") {
        message.push_str(" Check your Claude plan and extra-usage settings.");
    }
    if message.trim().is_empty() {
        message = match status {
            Some(status) => format!("Claude Code could not finish its reply (HTTP {status})"),
            None => "Claude Code could not finish its reply".into(),
        };
    }
    Some(ModelError::Provider(message.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::{SideEffectClass, ToolDescriptor};
    use serde_json::json;

    fn names() -> ToolNames {
        ToolNames::new(&[ToolDescriptor {
            name: "lookup".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
            side_effect_class: SideEffectClass::Read,
            idempotent: true,
        }])
    }

    fn event(event: Value) -> Value {
        json!({"type": "stream_event", "event": event, "parent_tool_use_id": null})
    }

    #[test]
    fn a_tool_call_ends_the_step_at_message_stop() {
        let names = names();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut step = Step::new(&names, Some(&tx), true);
        let messages = [
            json!({"type": "system", "subtype": "init", "mcp_servers": [{"name": "forge", "status": "connected"}]}),
            event(
                json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 2, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 10, "output_tokens": 1}}}),
            ),
            event(
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            ),
            event(
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "plan"}}),
            ),
            event(
                json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            ),
            event(
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Looking."}}),
            ),
            event(
                json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "mcp__forge__lookup", "input": {}}}),
            ),
            event(
                json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"key\":\"a\"}"}}),
            ),
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": "msg_1", "content": [{"type": "tool_use", "id": "toolu_1", "name": "mcp__forge__lookup", "input": {"key": "a"}}], "usage": {"input_tokens": 2, "output_tokens": 5}}}),
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": "msg_1", "content": [{"type": "tool_use", "id": "toolu_x", "name": "Bash", "input": {}}]}}),
            event(json!({"type": "message_delta", "usage": {"output_tokens": 40}})),
        ];
        for message in &messages {
            assert_eq!(step.consume(message).unwrap(), None);
        }
        assert_eq!(
            step.consume(&event(json!({"type": "message_stop"})))
                .unwrap(),
            Some(StepEnd::Tools)
        );
        let response = step.finish();
        assert_eq!(response.text, "Looking.");
        assert_eq!(response.thinking.as_deref(), Some("plan"));
        assert_eq!(
            response.tool_calls.len(),
            1,
            "a tool Forge did not offer is ignored"
        );
        assert_eq!(response.tool_calls[0].name, "lookup");
        assert_eq!(response.tool_calls[0].arguments, json!({"key": "a"}));
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 112);
        assert_eq!(usage.completion_tokens, 40);
        assert_eq!(usage.prompt_cache_read_tokens, 100);
        let events: Vec<ModelStreamEvent> = rx.try_iter().collect();
        assert!(matches!(
            events.first(),
            Some(ModelStreamEvent::ThinkingDelta { .. })
        ));
        assert!(events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::ToolCallDelta { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::ToolCallEnd { .. })));
        assert!(matches!(events.last(), Some(ModelStreamEvent::MessageEnd)));
    }

    #[test]
    fn a_result_ends_the_turn_and_unstreamed_text_is_used() {
        let names = names();
        let mut step = Step::new(&names, None, false);
        step.consume(&json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": "m", "content": [{"type": "text", "text": "done"}], "usage": {"input_tokens": 3, "output_tokens": 2}}})).unwrap();
        let end = step
            .consume(&json!({"type": "result", "subtype": "success", "is_error": false, "result": "done"}))
            .unwrap();
        assert_eq!(end, Some(StepEnd::Done));
        assert_eq!(step.finish().text, "done");
    }

    #[test]
    fn a_missing_tool_server_fails_the_turn() {
        let names = names();
        let mut step = Step::new(&names, None, true);
        let error = step
            .consume(&json!({"type": "system", "subtype": "init", "mcp_servers": []}))
            .unwrap_err();
        assert!(error.to_string().contains("tool server"), "{error}");
    }

    #[test]
    fn failed_results_map_to_errors() {
        let auth = result_error(
            &json!({"subtype": "success", "is_error": true, "result": "Invalid API key"}),
            Some("authentication_failed"),
            None,
        )
        .unwrap();
        assert!(auth.is_auth_failure());
        assert!(auth.to_string().contains("claude auth login"));

        let limit = result_error(
            &json!({"subtype": "success", "is_error": true, "result": "", "api_error_status": 429}),
            Some("rate_limit"),
            Some(&json!({"status": "rejected", "rateLimitType": "five_hour", "resetsAt": 1792213800})),
        )
        .unwrap();
        let text = limit.to_string();
        assert!(text.contains("usage limit reached (five hour)"), "{text}");
        assert!(!limit.is_retryable(), "Claude Code already retried");

        let overloaded = result_error(
            &json!({"subtype": "success", "is_error": true, "api_error_status": 529}),
            None,
            None,
        )
        .unwrap();
        assert!(overloaded.to_string().contains("overloaded"));

        let max_turns = result_error(
            &json!({"subtype": "error_max_turns", "is_error": true, "errors": ["too many turns"]}),
            None,
            None,
        )
        .unwrap();
        assert!(max_turns.to_string().contains("too many turns"));

        assert!(result_error(
            &json!({"subtype": "success", "is_error": false, "result": "ok"}),
            None,
            None
        )
        .is_none());
    }
}
