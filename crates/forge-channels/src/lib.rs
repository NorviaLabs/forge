//! Multi-channel ingress (channels.md) — CH-01. Phase 3 only.
//!
//! Maps Slack/Telegram/webhook-shaped messages into sessions with a **restricted**
//! principal (no broad repo tools by default).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use forge_core::{AgentSession, LoopConfig, LoopError};
use forge_governance::{AclPolicy, Governance};
use forge_model::ModelClient;
use forge_tools::ToolRegistry;
use forge_types::{Principal, SessionId};
use forge_workspace::IsolationMode;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
#[derive(Debug, Error)]
pub enum ChannelError {
    #[error(transparent)]
    Loop(#[from] LoopError),
    #[error("unknown channel `{0}`")]
    UnknownChannel(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Slack,
    Telegram,
    Webhook,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub channel: ChannelKind,
    pub channel_id: String,
    pub user_id: String,
    pub text: String,
    /// Optional thread/conversation key
    #[serde(default)]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChannelSessionKey {
    pub channel: ChannelKind,
    pub channel_id: String,
    pub thread_id: String,
}

impl ChannelSessionKey {
    pub fn from_message(m: &ChannelMessage) -> Self {
        Self {
            channel: m.channel,
            channel_id: m.channel_id.clone(),
            thread_id: m.thread_id.clone().unwrap_or_else(|| m.user_id.clone()),
        }
    }

    pub fn as_string(&self) -> String {
        format!("{:?}:{}:{}", self.channel, self.channel_id, self.thread_id)
    }
}

/// Restricted ACL for channel principals: deny write/exec by default; allow read tools.
pub fn restricted_channel_governance(surface: &str) -> Governance {
    let mut acl = AclPolicy::new();
    // Allow read-only built-ins + namespaced mcp read-style if listed
    acl.allow("read_file".into());
    acl.allow("grep".into());
    acl.allow("mcp:demo:echo".into());
    // Explicitly deny dangerous tools
    acl.deny("bash".into());
    acl.deny("write_file".into());
    acl.deny("mcp:*".into()); // re-allow only demo echo above (last match: need order)
                              // Fix: last match wins — re-allow echo after deny mcp:*
    acl.allow("mcp:demo:echo".into());

    Governance::default()
        .with_principal(Principal::restricted(surface))
        .with_acl(acl)
}

pub struct ChannelGateway {
    workspace: PathBuf,
    journal_dir: PathBuf,
    model: Arc<dyn ModelClient>,
    sessions: Mutex<HashMap<String, Arc<Mutex<AgentSession>>>>,
}

impl ChannelGateway {
    pub fn new(workspace: PathBuf, journal_dir: PathBuf, model: Arc<dyn ModelClient>) -> Self {
        Self {
            workspace,
            journal_dir,
            model,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub async fn handle_message(
        &self,
        msg: ChannelMessage,
    ) -> Result<ChannelResponse, ChannelError> {
        // Never log secrets from channel text into our response path beyond truncated text
        let key = ChannelSessionKey::from_message(&msg);
        let key_s = key.as_string();

        let session = {
            let mut map = self.sessions.lock().await;
            if let Some(s) = map.get(&key_s) {
                s.clone()
            } else {
                let tools = ToolRegistry::new();
                // Built-ins registered by AgentSession::create
                let mut agent = AgentSession::create(
                    LoopConfig {
                        max_turns: 128,
                        workspace: self.workspace.clone(),
                        journal_dir: self.journal_dir.clone(),
                        isolation: IsolationMode::Off,
                        enable_context_lifecycle: true,
                        enable_governance: true,

                        ..Default::default()
                    },
                    self.model.clone(),
                    tools,
                )
                .await?;
                let surface = match msg.channel {
                    ChannelKind::Slack => "slack",
                    ChannelKind::Telegram => "telegram",
                    ChannelKind::Webhook => "webhook",
                };
                agent.set_governance(restricted_channel_governance(surface));
                let arc = Arc::new(Mutex::new(agent));
                map.insert(key_s.clone(), arc.clone());
                arc
            }
        };

        let mut guard = session.lock().await;
        // Ensure tools list is restricted
        let tools = guard.list_tools();
        if tools.iter().any(|t| t == "bash" || t == "write_file") {
            return Err(ChannelError::Other(
                "channel ACL misconfigured: broad tools visible".into(),
            ));
        }

        let resp = guard.run_user_message(&msg.text).await?;
        Ok(ChannelResponse {
            session_id: guard.session_id,
            text: resp.text,
            tools_visible: tools,
            channel_key: key_s,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelResponse {
    pub session_id: SessionId,
    pub text: String,
    pub tools_visible: Vec<String>,
    pub channel_key: String,
}

/// Webhook JSON body helper.
#[derive(Debug, Deserialize)]
pub struct WebhookIngress {
    pub text: String,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
}

impl WebhookIngress {
    pub fn into_message(self) -> ChannelMessage {
        ChannelMessage {
            channel: ChannelKind::Webhook,
            channel_id: "default".into(),
            user_id: self.user_id.unwrap_or_else(|| "webhook".into()),
            text: self.text,
            thread_id: self.thread_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_model::MockModelClient;
    use forge_types::ModelResponse;
    use tempfile::tempdir;

    #[test]
    fn restricted_acl_hides_bash_and_write() {
        let g = restricted_channel_governance("webhook");
        let tools = g.filter_tools(vec![
            forge_types::ToolDescriptor {
                name: "bash".into(),
                description: "".into(),
                input_schema: serde_json::json!({}),
                side_effect_class: forge_types::SideEffectClass::Exec,
                idempotent: false,
            },
            forge_types::ToolDescriptor {
                name: "write_file".into(),
                description: "".into(),
                input_schema: serde_json::json!({}),
                side_effect_class: forge_types::SideEffectClass::Write,
                idempotent: false,
            },
            forge_types::ToolDescriptor {
                name: "read_file".into(),
                description: "".into(),
                input_schema: serde_json::json!({}),
                side_effect_class: forge_types::SideEffectClass::Read,
                idempotent: true,
            },
        ]);
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(!names.contains(&"bash"));
        assert!(!names.contains(&"write_file"));
        assert!(names.contains(&"read_file"));
    }

    #[tokio::test]
    async fn channel_message_runs_with_restricted_tools() {
        let dir = tempdir().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "channel ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let gw = ChannelGateway::new(dir.path().to_path_buf(), dir.path().join("j"), model);
        let resp = gw
            .handle_message(ChannelMessage {
                channel: ChannelKind::Slack,
                channel_id: "C1".into(),
                user_id: "U1".into(),
                text: "hello".into(),
                thread_id: Some("T1".into()),
            })
            .await
            .unwrap();
        assert_eq!(resp.text, "channel ok");
        assert!(!resp.tools_visible.iter().any(|t| t == "bash"));
        assert!(!resp.tools_visible.iter().any(|t| t == "write_file"));
    }

    #[test]
    fn webhook_ingress_maps() {
        let m = WebhookIngress {
            text: "hi".into(),
            user_id: None,
            thread_id: None,
        }
        .into_message();
        assert_eq!(m.channel, ChannelKind::Webhook);
        assert_eq!(m.text, "hi");
    }
}
