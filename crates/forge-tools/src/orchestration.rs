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
    /// Optional allow-list that narrows the child's inherited tool policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_allowlist: Option<Vec<String>>,
    /// Optional maximum number of model steps for this child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AgentMessageArgs {
    /// Opaque agent ID returned by spawn_agent.
    pub target: String,
    /// Message to place in the target's mailbox.
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WaitAgentArgs {
    /// Maximum time to wait in milliseconds. The coordinator clamps this to
    /// its configured minimum and maximum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Revision returned by the previous wait/list call. If omitted, the
    /// current revision is used and the call waits for a future change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_revision: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListAgentsArgs {
    /// Optional opaque-ID prefix used to narrow the descendant list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TargetAgentArgs {
    /// Opaque agent ID returned by spawn_agent.
    pub target: String,
}

pub struct SpawnAgentTool;
pub struct SendMessageTool;
pub struct FollowupTaskTool;
pub struct WaitAgentTool;
pub struct ListAgentsTool;
pub struct InterruptAgentTool;

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
    "Start a child agent in an isolated worktree without waiting for it to finish.",
    SpawnAgentArgs
);
orchestration_tool!(
    SendMessageTool,
    "send_message",
    "Place a message in a child agent's mailbox without waking it.",
    AgentMessageArgs
);
orchestration_tool!(
    FollowupTaskTool,
    "followup_task",
    "Deliver a message to an idle or completed child agent and start its next turn.",
    AgentMessageArgs
);
orchestration_tool!(
    WaitAgentTool,
    "wait_agent",
    "Wait for the next state change from one of this agent's descendants.",
    WaitAgentArgs
);
orchestration_tool!(
    ListAgentsTool,
    "list_agents",
    "List this agent's live and recently finished descendants.",
    ListAgentsArgs
);
orchestration_tool!(
    InterruptAgentTool,
    "interrupt_agent",
    "Interrupt a descendant agent. Already finished targets are treated as success.",
    TargetAgentArgs
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
            Box::new(FollowupTaskTool),
            Box::new(WaitAgentTool),
            Box::new(ListAgentsTool),
            Box::new(InterruptAgentTool),
        ];
        let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();
        assert_eq!(
            names,
            vec![
                "spawn_agent",
                "send_message",
                "followup_task",
                "wait_agent",
                "list_agents",
                "interrupt_agent"
            ]
        );
        for tool in tools {
            validate_args(
                tool.name(),
                &tool.input_schema(),
                &match tool.name() {
                    "spawn_agent" => json!({"task_name": "reviewer", "message": "Review the diff"}),
                    "send_message" | "followup_task" => {
                        json!({"target": "agent-id", "message": "Continue"})
                    }
                    "wait_agent" => json!({"timeout_ms": 1000}),
                    "list_agents" => json!({}),
                    "interrupt_agent" => json!({"target": "agent-id"}),
                    _ => unreachable!(),
                },
            )
            .unwrap();
        }
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

    #[tokio::test]
    async fn direct_calls_are_rejected_until_core_dispatches_them() {
        let error = SpawnAgentTool
            .call(&ToolContext::new(".".into()), json!({}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("agent coordinator"));
    }
}
