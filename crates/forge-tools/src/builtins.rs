use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use crate::registry::ToolContext;
use crate::{Tool, ToolError};

fn schema_for<T: JsonSchema>() -> Value {
    let s = schemars::schema_for!(T);
    serde_json::to_value(s).unwrap_or_else(|_| json!({"type": "object"}))
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadFileArgs {
    /// Path relative to workspace root (or absolute under workspace).
    pub path: String,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
}

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a text file from the workspace"
    }
    fn input_schema(&self) -> Value {
        schema_for::<ReadFileArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn idempotent(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: ReadFileArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::Execution(format!("internal deserialize after validation: {e}"))
        })?;
        let path = ctx.resolve_path(&a.path)?;
        let text = tokio::fs::read_to_string(&path).await?;
        let content = slice_lines(&text, a.offset, a.limit);
        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

fn slice_lines(text: &str, offset: Option<u64>, limit: Option<u64>) -> String {
    let start = offset.unwrap_or(1).saturating_sub(1) as usize;
    let lines: Vec<&str> = text.lines().collect();
    let end = limit
        .map(|l| start + l as usize)
        .unwrap_or(lines.len())
        .min(lines.len());
    if start >= lines.len() {
        return String::new();
    }
    lines[start..end].join("\n")
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write a text file in the workspace (creates parent dirs)"
    }
    fn input_schema(&self) -> Value {
        schema_for::<WriteFileArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Write
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: WriteFileArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        let path = ctx.resolve_path(&a.path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, a.content.as_bytes()).await?;
        Ok(ToolOutput {
            content: format!("wrote {} bytes to {}", a.content.len(), a.path),
            is_error: false,
        })
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BashArgs {
    pub command: String,
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Run a shell command in the workspace directory"
    }
    fn input_schema(&self) -> Value {
        schema_for::<BashArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Exec
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: BashArgs =
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?;
        let out = Command::new("bash")
            .arg("-lc")
            .arg(&a.command)
            .current_dir(&ctx.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        let mut content = String::from_utf8_lossy(&out.stdout).into_owned();
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&err);
        }
        Ok(ToolOutput {
            content,
            is_error: !out.status.success(),
        })
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GrepArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search for a regex pattern in the workspace (ripgrep if available, else grep)"
    }
    fn input_schema(&self) -> Value {
        schema_for::<GrepArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn idempotent(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: GrepArgs =
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?;
        let search_path = a
            .path
            .as_deref()
            .map(|p| ctx.resolve_path(p))
            .transpose()?
            .unwrap_or_else(|| ctx.workspace_root.clone());

        let mut cmd = Command::new("rg");
        cmd.arg("-n")
            .arg("--no-heading")
            .arg(&a.pattern)
            .arg(&search_path)
            .current_dir(&ctx.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = match cmd.output().await {
            Ok(o) => o,
            Err(_) => {
                // Fallback to grep -R
                Command::new("grep")
                    .arg("-Rn")
                    .arg(&a.pattern)
                    .arg(&search_path)
                    .current_dir(&ctx.workspace_root)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await
                    .map_err(|e| ToolError::Execution(e.to_string()))?
            }
        };
        let content = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

pub fn default_builtins() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ReadFileTool),
        std::sync::Arc::new(WriteFileTool),
        std::sync::Arc::new(BashTool),
        std::sync::Arc::new(GrepTool),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::validate_args;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn read_schema_rejects_number_path() {
        let t = ReadFileTool;
        let err = validate_args("read_file", &t.input_schema(), &json!({"path": 1})).unwrap_err();
        assert_eq!(err.tool, "read_file");
    }

    #[tokio::test]
    async fn write_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        WriteFileTool
            .call(
                &ctx,
                json!({"path": "n/a.txt", "content": "xyz"}),
            )
            .await
            .unwrap();
        let out = ReadFileTool
            .call(&ctx, json!({"path": "n/a.txt"}))
            .await
            .unwrap();
        assert_eq!(out.content, "xyz");
    }
}
