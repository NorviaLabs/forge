//! Agent loop (agent-loop.md) — Phase 1.

use std::path::PathBuf;
use std::sync::Arc;

use forge_durable::{new_session_id, Journal};
use forge_model::{ModelClient, ModelRequest};
use forge_tools::{default_builtins, ToolContext, ToolError, ToolRegistry, ValidationBudget};
use forge_types::{
    Message, MessageRole, ModelResponse, SessionId, SessionStatus, ToolCall, ToolOutput,
};
use serde_json::json;
use thiserror::Error;
use tracing::{info, warn};

#[derive(Debug, Error)]
pub enum LoopError {
    #[error(transparent)]
    Journal(#[from] forge_durable::JournalError),
    #[error(transparent)]
    Model(#[from] forge_model::ModelError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_turns: u32,
    pub workspace: PathBuf,
    pub journal_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct TurnEvent {
    pub kind: String,
    pub detail: String,
}

pub struct AgentSession {
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub messages: Vec<Message>,
    pub events: Vec<TurnEvent>,
    journal: Journal,
    tools: ToolRegistry,
    model: Arc<dyn ModelClient>,
    tool_ctx: ToolContext,
    max_turns: u32,
}

impl AgentSession {
    pub async fn create(
        loop_cfg: LoopConfig,
        model: Arc<dyn ModelClient>,
        mut tools: ToolRegistry,
    ) -> Result<Self, LoopError> {
        for t in default_builtins() {
            if tools.get(t.name()).is_none() {
                tools.register(t);
            }
        }
        let session_id = new_session_id();
        let journal = Journal::open(&loop_cfg.journal_dir, session_id).await?;
        journal.append_session_created(session_id).await?;
        Ok(Self {
            session_id,
            status: SessionStatus::Running,
            messages: vec![Message {
                role: MessageRole::System,
                content: "You are Forge, a coding agent. Use tools when needed.".into(),
                tool_call_id: None,
                name: None,
            }],
            events: vec![],
            journal,
            tools,
            model,
            tool_ctx: ToolContext::new(loop_cfg.workspace),
            max_turns: loop_cfg.max_turns,
        })
    }

    pub async fn resume(
        loop_cfg: LoopConfig,
        model: Arc<dyn ModelClient>,
        mut tools: ToolRegistry,
        session_id: SessionId,
    ) -> Result<Self, LoopError> {
        for t in default_builtins() {
            if tools.get(t.name()).is_none() {
                tools.register(t);
            }
        }
        let journal = Journal::open(&loop_cfg.journal_dir, session_id).await?;
        let state = journal.replay(session_id).await?;
        let mut messages = vec![Message {
            role: MessageRole::System,
            content: "You are Forge, a coding agent. Session resumed from journal.".into(),
            tool_call_id: None,
            name: None,
        }];
        for u in &state.user_messages {
            messages.push(Message {
                role: MessageRole::User,
                content: u.clone(),
                tool_call_id: None,
                name: None,
            });
        }
        // Rehydrate tool results as tool messages (simplified)
        for (id, tr) in &state.tool_results {
            messages.push(Message {
                role: MessageRole::Tool,
                content: tr.output.content.clone(),
                tool_call_id: Some(id.clone()),
                name: Some(tr.name.clone()),
            });
        }
        for incomplete in &state.incomplete_intents {
            warn!(call_id = %incomplete, "incomplete tool intent on resume (fail-safe)");
        }
        Ok(Self {
            session_id,
            status: state.status,
            messages,
            events: vec![TurnEvent {
                kind: "resume".into(),
                detail: format!("seq={}", state.last_seq),
            }],
            journal,
            tools,
            model,
            tool_ctx: ToolContext::new(loop_cfg.workspace),
            max_turns: loop_cfg.max_turns,
        })
    }

    pub fn list_tools(&self) -> Vec<String> {
        self.tools.names()
    }

    /// Run until no tool calls or max turns.
    pub async fn run_user_message(&mut self, text: &str) -> Result<ModelResponse, LoopError> {
        self.journal
            .append_user_message(self.session_id, text)
            .await?;
        self.messages.push(Message {
            role: MessageRole::User,
            content: text.into(),
            tool_call_id: None,
            name: None,
        });

        for turn in 0..self.max_turns {
            info!(turn, "model step");
            self.journal
                .append_model_request(
                    self.session_id,
                    json!({ "turn": turn, "messages": self.messages.len() }),
                )
                .await?;

            let tools = self.tools.list_descriptors();
            let req = ModelRequest {
                messages: self.messages.clone(),
                tools,
                model: String::new(),
            };
            let last = self.model.complete(req).await?;

            self.journal
                .append_model_response(
                    self.session_id,
                    json!({
                        "turn": turn,
                        "text_len": last.text.len(),
                        "tool_calls": last.tool_calls.len(),
                    }),
                )
                .await?;

            if !last.text.is_empty() {
                self.messages.push(Message {
                    role: MessageRole::Assistant,
                    content: last.text.clone(),
                    tool_call_id: None,
                    name: None,
                });
                self.events.push(TurnEvent {
                    kind: "assistant".into(),
                    detail: last.text.clone(),
                });
            }

            if last.tool_calls.is_empty() {
                self.status = SessionStatus::Completed;
                self.journal
                    .append_status(self.session_id, SessionStatus::Completed)
                    .await?;
                return Ok(last);
            }

            // Sequential tools
            let mut budget = ValidationBudget::with_default_max();
            for call in &last.tool_calls {
                self.run_one_tool(call, &mut budget).await?;
            }
        }

        self.status = SessionStatus::Failed;
        self.journal
            .append_status(self.session_id, SessionStatus::Failed)
            .await?;
        Err(LoopError::Other("max_turns exceeded".into()))
    }

    async fn run_one_tool(
        &mut self,
        call: &ToolCall,
        budget: &mut ValidationBudget,
    ) -> Result<(), LoopError> {
        // DUR-01: journal intent before side effect
        self.journal
            .append_tool_intent(self.session_id, call)
            .await?;

        match self
            .tools
            .call(&self.tool_ctx, &call.name, call.arguments.clone(), budget)
            .await
        {
            Ok(output) => {
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: output.content.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                });
                self.events.push(TurnEvent {
                    kind: "tool".into(),
                    detail: format!("{} -> {} chars", call.name, output.content.len()),
                });
            }
            Err(ToolError::Validation(ve)) => {
                self.journal
                    .append_validation_failed(self.session_id, &call.name, &ve.to_string())
                    .await?;
                let msg = format!("Tool validation error: {ve}. Please correct arguments.");
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: msg.clone(),
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                });
                self.events.push(TurnEvent {
                    kind: "validation".into(),
                    detail: msg,
                });
            }
            Err(e) => {
                let output = ToolOutput {
                    content: e.to_string(),
                    is_error: true,
                };
                self.journal
                    .append_tool_result(self.session_id, call, &output)
                    .await?;
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    content: output.content,
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_model::MockModelClient;
    use forge_types::ToolCall;
    use tempfile::tempdir;

    #[tokio::test]
    async fn loop_text_only_completes() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "all done".into(),
            tool_calls: vec![],
            usage: None,
        }]));
        let mut s = AgentSession::create(
            LoopConfig {
                max_turns: 5,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        let r = s.run_user_message("hello").await.unwrap();
        assert_eq!(r.text, "all done");
        assert_eq!(s.status, SessionStatus::Completed);
    }

    #[tokio::test]
    async fn loop_runs_tool_then_finishes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "data").unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "f.txt"}),
                }],
                usage: None,
            },
            ModelResponse {
                text: "read ok".into(),
                tool_calls: vec![],
                usage: None,
            },
        ]));
        let mut s = AgentSession::create(
            LoopConfig {
                max_turns: 5,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        let r = s.run_user_message("read it").await.unwrap();
        assert_eq!(r.text, "read ok");
        assert!(s.events.iter().any(|e| e.kind == "tool"));
    }

    #[tokio::test]
    async fn invalid_tool_args_do_not_crash_loop() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![
            ModelResponse {
                text: "".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": 99}),
                }],
                usage: None,
            },
            ModelResponse {
                text: "fixed".into(),
                tool_calls: vec![],
                usage: None,
            },
        ]));
        let mut s = AgentSession::create(
            LoopConfig {
                max_turns: 5,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        let r = s.run_user_message("bad tool").await.unwrap();
        assert_eq!(r.text, "fixed");
        assert!(s.events.iter().any(|e| e.kind == "validation"));
    }
}
