//! Tool protocol (tool-protocol.md) — CORE-01.
//! Phase 9: `web_search` (WEB-01) — see `web_search` module.

mod apply_patch;
mod refactor;
mod builtins;
mod fff;
mod registry;
mod validation;
pub mod web_search;

pub use apply_patch::{ApplyPatchArgs, ApplyPatchTool};
pub use builtins::{
    default_builtins, default_builtins_with_web_search, BashTool, GitTool, ReadFileTool,
    WriteFileTool,
};
pub use refactor::RefactorTool;
pub use registry::{ToolContext, ToolRegistry};
pub use validation::{validate_args, ValidationBudget};
pub use web_search::{should_register_web_search, web_search_tool, WebSearchArgs, WebSearchTool};

use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolDescriptor, ToolOutput, ToolValidationError};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
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
