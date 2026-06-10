//! Tool protocol (tool-protocol.md) — CORE-01.

mod builtins;
mod registry;
mod validation;

pub use builtins::{default_builtins, BashTool, GrepTool, ReadFileTool, WriteFileTool};
pub use registry::{ToolContext, ToolRegistry};
pub use validation::{validate_args, ValidationBudget};

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
