use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::builtins::schema_for;
use crate::{Tool, ToolContext, ToolError};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SpawnAgentArgs {
    /// Human-readable label for the child agent.
    pub task_name: String,
    /// First instruction sent to the child agent.
    pub message: String,
    /// Scheduling mode. Read-only agents may share the workspace concurrently;
    /// writers are serialized. Omitted values are writers for safety.
    #[serde(default)]
    pub mode: AgentMode,
    /// Optional allow-list that narrows the child's inherited tool policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    ReadOnly,
    #[default]
    Writer,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AgentMessageArgs {
    /// Opaque agent ID returned by spawn_agent.
    pub target: String,
    /// Message for the target. Required unless `interrupt` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Start the target's next turn with this message. Use it for an idle or
    /// completed agent; without it the message waits in the mailbox.
    #[serde(default)]
    pub wake: bool,
    /// Stop the target's current turn instead of messaging it. Already
    /// finished targets are treated as success.
    #[serde(default)]
    pub interrupt: bool,
}

/// What a `send_message` call asks the coordinator to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentMessageAction {
    /// Queue the message without waking the target.
    Queue(String),
    /// Deliver the message and start the target's next turn.
    Wake(String),
    /// Interrupt the target's current turn.
    Interrupt,
}

impl AgentMessageArgs {
    /// Resolve the flags into one action, rejecting contradictory input.
    pub fn action(&self) -> Result<AgentMessageAction, String> {
        let message = self
            .message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(str::to_string);
        match (self.interrupt, self.wake, message) {
            (true, true, _) => Err("`interrupt` and `wake` cannot both be set".into()),
            (true, false, Some(_)) => Err(
                "`interrupt` takes no message; interrupt first, then send the message with `wake`"
                    .into(),
            ),
            (true, false, None) => Ok(AgentMessageAction::Interrupt),
            (false, _, None) => Err("`message` is required unless `interrupt` is true".into()),
            (false, true, Some(message)) => Ok(AgentMessageAction::Wake(message)),
            (false, false, Some(message)) => Ok(AgentMessageAction::Queue(message)),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WaitAgentArgs {
    /// Maximum time to wait in milliseconds. The coordinator clamps this to
    /// its configured minimum and maximum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Revision returned by the previous wait call. If omitted, the current
    /// revision is used and the call waits for a future change. Pass 0 to
    /// list every descendant immediately without waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_revision: Option<u64>,
}

pub struct SpawnAgentTool;
pub struct SendMessageTool;
pub struct WaitAgentTool;

macro_rules! orchestration_tool {
    ($type:ty, $name:literal, $description:literal, $args:ty) => {
        #[async_trait]
        impl Tool for $type {
            fn name(&self) -> &str {
                $name
            }

            fn description(&self) -> &str {
                $description
            }

            fn input_schema(&self) -> Value {
                schema_for::<$args>()
            }

            fn side_effect_class(&self) -> SideEffectClass {
                SideEffectClass::Meta
            }

            async fn call(
                &self,
                _ctx: &ToolContext,
                _args: Value,
            ) -> Result<ToolOutput, ToolError> {
                Err(ToolError::Execution(format!(
                    "{} must be intercepted by the agent coordinator",
                    $name
                )))
            }
        }
    };
}

orchestration_tool!(
    SpawnAgentTool,
    "spawn_agent",
    "Start a child agent without waiting. Read-only agents share the workspace concurrently; writers are serialized.",
    SpawnAgentArgs
);
orchestration_tool!(
    SendMessageTool,
    "send_message",
    "Message a child agent. By default the message waits in its mailbox; set `wake` to start its next turn with it, or `interrupt` (no message) to stop its current turn.",
    AgentMessageArgs
);
orchestration_tool!(
    WaitAgentTool,
    "wait_agent",
    "Wait for the next state change from this agent's descendants and return all of them. Pass `since_revision: 0` to list them without waiting.",
    WaitAgentArgs
);
#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::validate_args;
    use serde_json::json;

    #[test]
    fn schemas_cover_the_full_orchestration_surface() {
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(SpawnAgentTool),
            Box::new(SendMessageTool),
            Box::new(WaitAgentTool),
        ];
        let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();
        assert_eq!(names, vec!["spawn_agent", "send_message", "wait_agent"]);
        for tool in tools {
            validate_args(
                tool.name(),
                &tool.input_schema(),
                &match tool.name() {
                    "spawn_agent" => json!({"task_name": "reviewer", "message": "Review the diff"}),
                    "send_message" => json!({"target": "agent-id", "message": "Continue"}),
                    "wait_agent" => json!({"timeout_ms": 1000}),
                    _ => unreachable!(),
                },
            )
            .unwrap();
        }
    }

    #[test]
    fn spawn_mode_defaults_to_writer_and_accepts_read_only() {
        let default: SpawnAgentArgs = serde_json::from_value(json!({
            "task_name": "reviewer",
            "message": "Review the diff"
        }))
        .unwrap();
        assert!(matches!(default.mode, AgentMode::Writer));

        let read_only: SpawnAgentArgs = serde_json::from_value(json!({
            "task_name": "reviewer",
            "message": "Review the diff",
            "mode": "read_only"
        }))
        .unwrap();
        assert!(matches!(read_only.mode, AgentMode::ReadOnly));
    }

    #[test]
    fn spawn_schema_rejects_missing_message() {
        let error = validate_args(
            "spawn_agent",
            &SpawnAgentTool.input_schema(),
            &json!({"task_name": "reviewer"}),
        )
        .unwrap_err();
        assert_eq!(error.tool, "spawn_agent");
    }

    #[test]
    fn send_message_flags_resolve_to_one_action() {
        let args = |value: Value| -> AgentMessageArgs { serde_json::from_value(value).unwrap() };
        assert_eq!(
            args(json!({"target": "a", "message": "hi"})).action(),
            Ok(AgentMessageAction::Queue("hi".into()))
        );
        assert_eq!(
            args(json!({"target": "a", "message": "hi", "wake": true})).action(),
            Ok(AgentMessageAction::Wake("hi".into()))
        );
        assert_eq!(
            args(json!({"target": "a", "interrupt": true})).action(),
            Ok(AgentMessageAction::Interrupt)
        );
        for bad in [
            json!({"target": "a"}),
            json!({"target": "a", "message": "  "}),
            json!({"target": "a", "wake": true}),
            json!({"target": "a", "interrupt": true, "wake": true}),
            json!({"target": "a", "interrupt": true, "message": "stop"}),
        ] {
            assert!(args(bad.clone()).action().is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn direct_calls_are_rejected_until_core_dispatches_them() {
        let error = SpawnAgentTool
            .call(&ToolContext::new(".".into()), json!({}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("agent coordinator"));
    }
}
