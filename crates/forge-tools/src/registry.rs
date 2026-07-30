use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use forge_types::{ToolDescriptor, ToolOutput};
use serde_json::Value;

use crate::validation::{validate_args, ValidationBudget};
use crate::{Tool, ToolError};

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace_root: PathBuf,
    pub principal: String,
}

impl ToolContext {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            principal: "local-dev".into(),
        }
    }

    pub fn resolve_path(&self, rel: &str) -> Result<PathBuf, ToolError> {
        let p = PathBuf::from(rel);
        let full = if p.is_absolute() {
            p
        } else {
            self.workspace_root.join(p)
        };
        let root = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        // Best-effort containment: reject `..` escape when possible.
        if let (Ok(c), Ok(r)) = (full.canonicalize(), root.canonicalize()) {
            if !c.starts_with(&r) {
                return Err(ToolError::Execution(format!(
                    "path `{}` escapes workspace",
                    rel
                )));
            }
            return Ok(c);
        }
        // File may not exist yet (writes).
        let normalized = full;
        if normalized
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            // Allow relative parent only if still under root after join — check prefix string-wise
            let root_s = root.to_string_lossy();
            let norm_s = normalized.to_string_lossy();
            if !norm_s.starts_with(root_s.as_ref()) {
                return Err(ToolError::Execution(format!(
                    "path `{}` escapes workspace",
                    rel
                )));
            }
        }
        Ok(normalized)
    }
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn list_descriptors(&self) -> Vec<ToolDescriptor> {
        let mut v: Vec<_> = self.tools.values().map(|t| t.descriptor()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<_> = self.tools.keys().cloned().collect();
        n.sort();
        n
    }

    /// Validate then execute. Never calls handler on validation failure.
    pub async fn call(
        &self,
        ctx: &ToolContext,
        name: &str,
        args: Value,
        budget: &mut ValidationBudget,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;

        if let Err(ve) = validate_args(name, &tool.input_schema(), &args) {
            let signature =
                crate::validation::validation_error_signature(name, &ve.path, &ve.message);
            budget
                .record_failure_with_signature(name, Some(&signature))
                .map_err(ToolError::Execution)?;
            return Err(ToolError::Validation(ve));
        }

        tool.call(ctx, args).await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::ReadFileTool;
    use crate::ValidationBudget;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn unknown_tool() {
        let reg = ToolRegistry::new();
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let mut b = ValidationBudget::with_default_max();
        let err = reg.call(&ctx, "nope", json!({}), &mut b).await.unwrap_err();
        assert!(matches!(err, ToolError::Unknown(_)));
    }

    #[tokio::test]
    async fn validation_blocks_side_effects() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("x.txt");
        std::fs::write(&path, "hello").unwrap();

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let mut b = ValidationBudget::with_default_max();
        let err = reg
            .call(&ctx, "read_file", json!({"path": 1}), &mut b)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn read_file_ok() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let mut b = ValidationBudget::with_default_max();
        let out = reg
            .call(&ctx, "read_file", json!({"path": "a.txt"}), &mut b)
            .await
            .unwrap();
        assert_eq!(out.content, "hi");
        assert!(!out.is_error);
    }

    #[test]
    fn tool_context_resolves_relative_absolute_and_missing_paths() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hi").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        assert_eq!(
            ctx.resolve_path("a.txt").unwrap(),
            file.canonicalize().unwrap()
        );
        assert_eq!(
            ctx.resolve_path(file.to_str().unwrap())
                .unwrap()
                .canonicalize()
                .unwrap(),
            file.canonicalize().unwrap()
        );

        let missing = ctx.resolve_path("new.txt").unwrap();
        assert_eq!(missing, dir.path().join("new.txt"));
    }

    #[test]
    fn tool_context_rejects_canonical_escape() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "no").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        let err = ctx
            .resolve_path(outside_file.to_str().unwrap())
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Execution(message) if message.contains("escapes workspace"))
        );
    }

    #[test]
    fn registry_descriptors_and_names_are_sorted() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        assert_eq!(reg.names(), vec!["read_file"]);
        let descriptors = reg.list_descriptors();
        assert_eq!(descriptors[0].name, "read_file");
    }
}
