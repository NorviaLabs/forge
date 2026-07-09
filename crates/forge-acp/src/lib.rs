//! ACP surface (protocol-acp.md) — CORE-03. Phase 2.
//!
//! Minimal JSON-line ACP-like protocol: no direct model/MCP calls; all go through AgentSession.

use std::path::PathBuf;
use std::sync::Arc;

use forge_core::{AgentSession, LoopConfig, LoopError};
use forge_model::{MockModelClient, ModelClient};
use forge_tools::ToolRegistry;
use forge_types::{HitlDecision, SessionId, SessionStatus};
use forge_workspace::IsolationMode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;


#[derive(Debug, Error)]
pub enum AcpError {
    #[error(transparent)]
    Loop(#[from] LoopError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("session not found")]
    NoSession,
}

/// Handle exposing session control without model/MCP access (surface rule).
pub struct AgentHandle {
    session: Arc<Mutex<AgentSession>>,
}

impl AgentHandle {
    pub fn new(session: AgentSession) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
        }
    }

    pub async fn session_id(&self) -> SessionId {
        self.session.lock().await.session_id
    }

    pub async fn status(&self) -> SessionStatus {
        self.session.lock().await.status
    }

    pub async fn prompt(&self, text: &str) -> Result<serde_json::Value, AcpError> {
        let mut s = self.session.lock().await;
        let resp = s.run_user_message(text).await?;
        Ok(json!({
            "text": resp.text,
            "tool_calls": resp.tool_calls.len(),
            "status": s.status,
            "session_id": s.session_id,
            "pending_hitl": s.pending_hitl,
        }))
    }

    pub async fn list_tools(&self) -> Vec<String> {
        self.session.lock().await.list_tools()
    }

    pub async fn approve(&self, actor: &str) -> Result<(), AcpError> {
        self.session
            .lock()
            .await
            .resolve_hitl(HitlDecision::Approve, actor)
            .await?;
        Ok(())
    }

    pub async fn deny(&self, actor: &str) -> Result<(), AcpError> {
        self.session
            .lock()
            .await
            .resolve_hitl(HitlDecision::Deny, actor)
            .await?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AcpRequest {
    Initialize {},
    SessionNew {},
    SessionPrompt { text: String },
    SessionTools {},
    SessionStatus {},
    SessionApprove {},
    SessionDeny {},
}

#[derive(Debug, Serialize)]
pub struct AcpResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct AcpServer {
    handle: Option<AgentHandle>,
    workspace: PathBuf,
    journal_dir: PathBuf,
    model: Arc<dyn ModelClient>,
}

impl AcpServer {
    pub fn new(workspace: PathBuf, journal_dir: PathBuf, model: Arc<dyn ModelClient>) -> Self {
        Self {
            handle: None,
            workspace,
            journal_dir,
            model,
        }
    }

    pub async fn ensure_session(&mut self) -> Result<&AgentHandle, AcpError> {
        if self.handle.is_none() {
            let session = AgentSession::create(
                LoopConfig {
                    max_turns: 16,
                    workspace: self.workspace.clone(),
                    journal_dir: self.journal_dir.clone(),
                    isolation: IsolationMode::Off,
                    enable_context_lifecycle: true,
                    enable_governance: true,

                ..Default::default()
                },
                self.model.clone(),
                ToolRegistry::new(),
            )
            .await?;
            self.handle = Some(AgentHandle::new(session));
        }
        Ok(self.handle.as_ref().unwrap())
    }

    pub async fn handle_request(&mut self, req: AcpRequest) -> AcpResponse {
        match self.dispatch(req).await {
            Ok(v) => AcpResponse {
                ok: true,
                result: Some(v),
                error: None,
            },
            Err(e) => AcpResponse {
                ok: false,
                result: None,
                error: Some(e.to_string()),
            },
        }
    }

    async fn dispatch(&mut self, req: AcpRequest) -> Result<serde_json::Value, AcpError> {
        match req {
            AcpRequest::Initialize {} => Ok(json!({
                "protocol": "forge-acp-lite",
                "version": "0.1.0",
                "phase": 2
            })),
            AcpRequest::SessionNew {} => {
                self.handle = None;
                let h = self.ensure_session().await?;
                Ok(json!({ "session_id": h.session_id().await }))
            }
            AcpRequest::SessionPrompt { text } => {
                let h = self.ensure_session().await?;
                h.prompt(&text).await
            }
            AcpRequest::SessionTools {} => {
                let h = self.ensure_session().await?;
                Ok(json!({ "tools": h.list_tools().await }))
            }
            AcpRequest::SessionStatus {} => {
                let h = self.handle.as_ref().ok_or(AcpError::NoSession)?;
                Ok(json!({
                    "session_id": h.session_id().await,
                    "status": h.status().await,
                }))
            }
            AcpRequest::SessionApprove {} => {
                let h = self.handle.as_ref().ok_or(AcpError::NoSession)?;
                h.approve("acp").await?;
                Ok(json!({ "decision": "approve" }))
            }
            AcpRequest::SessionDeny {} => {
                let h = self.handle.as_ref().ok_or(AcpError::NoSession)?;
                h.deny("acp").await?;
                Ok(json!({ "decision": "deny" }))
            }
        }
    }

    /// Serve one JSON object per line on the given reader/writer (for tests & IDE bridge).
    pub async fn serve_lines<R, W>(&mut self, reader: R, mut writer: W) -> Result<(), AcpError>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let req: AcpRequest = serde_json::from_str(&line)
                .map_err(|e| AcpError::Protocol(e.to_string()))?;
            let resp = self.handle_request(req).await;
            let mut out = serde_json::to_string(&resp)?;
            out.push('\n');
            writer.write_all(out.as_bytes()).await?;
            writer.flush().await?;
        }
        Ok(())
    }
}

pub async fn open_mock_handle(workspace: PathBuf, journal_dir: PathBuf) -> Result<AgentHandle, AcpError> {
    let model = Arc::new(MockModelClient::script(vec![forge_types::ModelResponse {
        text: "acp ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let session = AgentSession::create(
        LoopConfig {
            max_turns: 8,
            workspace,
            journal_dir,
            isolation: IsolationMode::Off,
            enable_context_lifecycle: true,
            enable_governance: true,

        ..Default::default()
        },
        model,
        ToolRegistry::new(),
    )
    .await?;
    Ok(AgentHandle::new(session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn acp_session_prompt() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![forge_types::ModelResponse {
            text: "hello from acp".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
    }]));
        let mut server = AcpServer::new(
            dir.path().to_path_buf(),
            dir.path().join("j"),
            model,
        );
        let init = server.handle_request(AcpRequest::Initialize {}).await;
        assert!(init.ok);
        let created = server.handle_request(AcpRequest::SessionNew {}).await;
        assert!(created.ok);
        let prompt = server
            .handle_request(AcpRequest::SessionPrompt {
                text: "hi".into(),
            })
            .await;
        assert!(prompt.ok);
        let result = prompt.result.unwrap();
        assert_eq!(result["text"], "hello from acp");
        let tools = server.handle_request(AcpRequest::SessionTools {}).await;
        assert!(tools.ok);
        assert!(tools.result.unwrap()["tools"].as_array().unwrap().len() >= 1);
    }

    #[tokio::test]
    async fn handle_session_id_stable() {
        let dir = tempdir().unwrap();
        let h = open_mock_handle(dir.path().to_path_buf(), dir.path().join("j"))
            .await
            .unwrap();
        let id = h.session_id().await;
        assert_eq!(id, h.session_id().await);
    }
}
