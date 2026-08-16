//! Tool protocol (tool-protocol.md) — CORE-01.
//! Phase 9: `web_search` (WEB-01) — see `web_search` module.

mod apply_patch;
mod builtins;
mod edit;
mod fast_file_tools;
mod invocation;
mod registry;
mod skills;
mod unified_exec;
mod validation;
mod view_image;
pub mod web_search;

pub use apply_patch::{ApplyPatchArgs, ApplyPatchTool};
pub use builtins::{
    default_builtins, default_builtins_with_web_search, run_shell_command, BashTool, GitTool,
    LsTool, ReadFileTool, UpdatePlanTool, WriteFileTool, PROVIDER_CREDENTIAL_ENV,
};
pub use edit::{EditArgs, EditTool};
pub use invocation::tool_invocation;
pub use registry::{canonical_tool_name, canonicalize_tool_call, ToolContext, ToolRegistry};
pub use skills::{LoadSkillArgs, LoadSkillTool};
pub use unified_exec::{unified_exec_tools, ExecCommandTool, WriteStdinTool};
pub use validation::{validate_args, validation_error_signature, ValidationBudget};
pub use view_image::{ViewImageArgs, ViewImageTool};
pub use web_search::{should_register_web_search, web_search_tool, WebSearchArgs, WebSearchTool};

use async_trait::async_trait;
use forge_types::{
    ExecutionOutcome, SideEffectClass, ToolDescriptor, ToolOutput, ToolValidationError,
};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ToolError {
    #[error(transparent)]
    Validation(#[from] ToolValidationError),
    #[error("unknown tool `{0}`")]
    Unknown(String),
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl ToolError {
    /// Maps an error surfacing from `ToolRegistry::call` (i.e. the tool
    /// never produced a `ToolOutput` at all) to the outcome that should be
    /// recorded. `Validation` errors are a protocol-shape retry mechanism
    /// handled separately by callers and are not expected here.
    pub fn as_outcome(&self) -> ExecutionOutcome {
        match self {
            ToolError::Validation(_) => ExecutionOutcome::Failed { exit_code: None },
            ToolError::Unknown(_) | ToolError::Execution(_) | ToolError::Io(_) => {
                ExecutionOutcome::SpawnFailed {
                    reason: self.to_string(),
                }
            }
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    fn side_effect_class(&self) -> SideEffectClass;
    fn idempotent(&self) -> bool {
        false
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError>;

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
            side_effect_class: self.side_effect_class(),
            idempotent: self.idempotent(),
        }
    }
}
