//! Targeted exact-string replace in an existing workspace file.
//!
//! Locate uses the shared FFF index (`grep`); the write then refreshes that
//! index so a following `grep` sees the new bytes without waiting on watch.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use forge_search::GrepQueryMode;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::builtins::{schema_for, unified_diff};
use crate::fast_file_tools::FastFileState;
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EditArgs {
    /// Path relative to workspace root (or absolute under workspace).
    #[serde(alias = "file_path")]
    pub path: String,
    /// Exact text to find. Must be unique in the file unless `replace_all`.
    #[serde(alias = "old", alias = "oldString")]
    pub old_string: String,
    /// Replacement text.
    #[serde(alias = "new", alias = "newString")]
    pub new_string: String,
    /// Replace every occurrence. Default: fail when `old_string` is not unique.
    #[serde(default)]
    pub replace_all: bool,
}

pub struct EditTool {
    state: Arc<FastFileState>,
}

impl EditTool {
    pub(crate) fn new(state: Arc<FastFileState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in an existing workspace file. \
Fails if the string is missing, or if it appears more than once unless `replace_all` is true. \
Use this for a focused change. Use `write_file` to create a file or replace it wholesale, \
and `apply_patch` for multi-hunk or multi-file diffs."
    }

    fn input_schema(&self) -> Value {
        schema_for::<EditArgs>()
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Write
    }

    fn warm_workspace(&self, root: &Path) {
        self.state.warm(root);
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: EditArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::Execution(format!("internal deserialize after validation: {e}"))
        })?;
        if a.old_string.is_empty() {
            return Ok(ToolOutput::failed_exit(
                "edit: old_string must not be empty",
                None,
            ));
        }
        if a.old_string == a.new_string {
            return Ok(ToolOutput::failed_exit(
                "edit: old_string and new_string are identical",
                None,
            ));
        }

        let path = ctx.resolve_write_path(&a.path)?;
        let original = tokio::fs::read_to_string(&path).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ToolError::Execution(format!("edit: `{}` does not exist", a.path))
            } else {
                ToolError::Execution(format!("edit: reading `{}`: {error}", a.path))
            }
        })?;

        let count = original.matches(&a.old_string).count();
        if count == 0 {
            let hint = locate_elsewhere(&self.state, &ctx.workspace_root, &a.path, &a.old_string);
            return Ok(ToolOutput::failed_exit(
                format!("edit: old_string not found in `{}`{hint}", a.path),
                None,
            ));
        }
        if count > 1 && !a.replace_all {
            return Ok(ToolOutput::failed_exit(
                format!(
                    "edit: old_string appears {count} times in `{}`; pass replace_all=true or include more surrounding context",
                    a.path
                ),
                None,
            ));
        }

        let updated = if a.replace_all {
            original.replace(&a.old_string, &a.new_string)
        } else {
            original.replacen(&a.old_string, &a.new_string, 1)
        };
        tokio::fs::write(&path, updated.as_bytes()).await?;
        let _ = self
            .state
            .index_for(&ctx.workspace_root)
            .and_then(|index| index.note_file_changed(&path).map_err(index_err));
        let diff = unified_diff(&a.path, Some(&original), &updated)?;
        Ok(ToolOutput {
            outcome: Default::default(),
            content: if diff.trim().is_empty() {
                format!("edited {}", a.path)
            } else {
                diff
            },
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

fn index_err(error: forge_search::SearchError) -> ToolError {
    ToolError::Execution(error.to_string())
}

/// When the target file does not contain `old_string`, ask FFF whether it
/// lives elsewhere so the model can retry with the right path.
fn locate_elsewhere(state: &FastFileState, root: &Path, current: &str, old_string: &str) -> String {
    if old_string.contains('\n') || old_string.chars().count() < 4 {
        return String::new();
    }
    let Ok(index) = state.index_for(root) else {
        return String::new();
    };
    let Ok(found) = index.grep_scoped(old_string, None, None, GrepQueryMode::Plain, 20) else {
        return String::new();
    };
    let current = current.trim_start_matches("./");
    let mut others: Vec<&str> = found
        .hits
        .iter()
        .map(|hit| hit.path.as_str())
        .filter(|path| *path != current)
        .collect();
    others.sort_unstable();
    others.dedup();
    if others.is_empty() {
        return String::new();
    }
    format!(
        "; also found in {}",
        others.into_iter().take(3).collect::<Vec<_>>().join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fast_file_tools::FffGrepTool;
    use serde_json::json;
    use tempfile::tempdir;

    fn tool() -> EditTool {
        EditTool::new(Arc::new(FastFileState::new()))
    }

    #[test]
    fn describes_itself() {
        let tool = tool();
        assert_eq!(tool.name(), "edit");
        assert_eq!(tool.side_effect_class(), SideEffectClass::Write);
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn replaces_a_unique_string() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\nfn bar() {}\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = tool()
            .call(
                &ctx,
                json!({
                    "path": "lib.rs",
                    "old_string": "fn foo() {}",
                    "new_string": "fn foo() { 1 }"
                }),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("-fn foo() {}"), "{}", out.content);
        assert!(out.content.contains("+fn foo() { 1 }"), "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
            "fn foo() { 1 }\nfn bar() {}\n"
        );
    }

    #[tokio::test]
    async fn accepts_claude_and_opencode_arg_aliases() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "alpha\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = tool()
            .call(
                &ctx,
                json!({
                    "file_path": "a.rs",
                    "oldString": "alpha",
                    "newString": "beta"
                }),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "beta\n"
        );
    }

    #[tokio::test]
    async fn fails_when_the_string_is_not_unique() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "foo\nfoo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = tool()
            .call(
                &ctx,
                json!({
                    "path": "a.rs",
                    "old_string": "foo",
                    "new_string": "bar"
                }),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("2 times"), "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "foo\nfoo\n"
        );
    }

    #[tokio::test]
    async fn replace_all_updates_every_occurrence() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "foo\nfoo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = tool()
            .call(
                &ctx,
                json!({
                    "path": "a.rs",
                    "old_string": "foo",
                    "new_string": "bar",
                    "replace_all": true
                }),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "bar\nbar\n"
        );
    }

    #[tokio::test]
    async fn edit_refreshes_the_shared_fff_index() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn before() {}\n").unwrap();
        let state = Arc::new(FastFileState::new());
        let edit = EditTool::new(state.clone());
        let grep = FffGrepTool::new(state, "grep");
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = edit
            .call(
                &ctx,
                json!({
                    "path": "lib.rs",
                    "old_string": "fn before() {}",
                    "new_string": "fn after() {}"
                }),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let found = grep
            .call(&ctx, json!({"pattern": "fn after", "mode": "plain"}))
            .await
            .unwrap();
        assert!(
            found.content.contains("lib.rs"),
            "grep must see the edited bytes, got {}",
            found.content
        );
        assert!(
            !found.content.contains("No matches found"),
            "{}",
            found.content
        );
    }

    #[tokio::test]
    async fn missing_string_hints_other_fff_hits() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("wrong.rs"), "hello\n").unwrap();
        std::fs::write(dir.path().join("right.rs"), "unique_token_xyz\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = tool()
            .call(
                &ctx,
                json!({
                    "path": "wrong.rs",
                    "old_string": "unique_token_xyz",
                    "new_string": "changed"
                }),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("not found"), "{}", out.content);
        assert!(
            out.content.contains("right.rs"),
            "FFF should point at the other file, got {}",
            out.content
        );
    }

    #[tokio::test]
    async fn fails_when_the_string_is_missing() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "foo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = tool()
            .call(
                &ctx,
                json!({
                    "path": "a.rs",
                    "old_string": "missing",
                    "new_string": "bar"
                }),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("not found"), "{}", out.content);
    }

    #[tokio::test]
    async fn refuses_empty_old_string_and_identical_replacement() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "foo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let empty = tool()
            .call(
                &ctx,
                json!({"path": "a.rs", "old_string": "", "new_string": "x"}),
            )
            .await
            .unwrap();
        assert!(empty.is_error);
        let same = tool()
            .call(
                &ctx,
                json!({"path": "a.rs", "old_string": "foo", "new_string": "foo"}),
            )
            .await
            .unwrap();
        assert!(same.is_error);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "foo\n"
        );
    }

    #[tokio::test]
    async fn does_not_create_a_missing_file() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let error = tool()
            .call(
                &ctx,
                json!({
                    "path": "missing.rs",
                    "old_string": "a",
                    "new_string": "b"
                }),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not exist"), "{error}");
        assert!(!dir.path().join("missing.rs").exists());
    }

    #[tokio::test]
    async fn refuses_git_directory() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let error = tool()
            .call(
                &ctx,
                json!({
                    "path": ".git/config",
                    "old_string": "[core]",
                    "new_string": "[diff]"
                }),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains(".git"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".git/config")).unwrap(),
            "[core]\n"
        );
    }

    #[tokio::test]
    async fn refuses_path_escaping_the_workspace() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let error = tool()
            .call(
                &ctx,
                json!({
                    "path": "../escape.rs",
                    "old_string": "a",
                    "new_string": "b"
                }),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("escapes workspace"), "{error}");
    }
}
