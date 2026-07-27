use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::builtins::unified_diff;
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ApplyPatchArgs {
    /// Patch in the `*** Begin Patch` format.
    pub patch: String,
}

pub struct ApplyPatchTool;

#[derive(Debug)]
enum PatchAction {
    Add { path: String, content: String },
    Update { path: String, hunks: Vec<Hunk> },
    Delete { path: String },
}

#[derive(Debug)]
struct Hunk {
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

#[derive(Debug)]
enum PreparedChange {
    Write {
        path: PathBuf,
        display_path: String,
        original: Option<String>,
        content: String,
    },
    Delete {
        path: PathBuf,
        display_path: String,
        original: String,
    },
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a validated patch to workspace files using the `*** Begin Patch` format"
    }

    fn input_schema(&self) -> Value {
        let schema = schemars::schema_for!(ApplyPatchArgs);
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({"type": "object"}))
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Write
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: ApplyPatchArgs = serde_json::from_value(args)
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let actions = parse_patch(&args.patch)?;
        let changes = prepare_changes(ctx, actions).await?;
        let mut diffs = Vec::with_capacity(changes.len());

        for change in &changes {
            let diff = match change {
                PreparedChange::Write {
                    display_path,
                    original,
                    content,
                    ..
                } => unified_diff(display_path, original.as_deref(), content).await?,
                PreparedChange::Delete {
                    display_path,
                    original,
                    ..
                } => unified_diff(display_path, Some(original), "").await?,
            };
            if !diff.trim().is_empty() {
                diffs.push(diff);
            }
        }

        for change in &changes {
            match change {
                PreparedChange::Write { path, content, .. } => {
                    if let Some(parent) = path.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::write(path, content).await?;
                }
                PreparedChange::Delete { path, .. } => tokio::fs::remove_file(path).await?,
            }
        }

        Ok(ToolOutput {
            content: diffs.join("\n"),
            is_error: false,
        })
    }
}

fn parse_patch(patch: &str) -> Result<Vec<PatchAction>, ToolError> {
    let normalized = patch.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        return execution_error(
            "patch must start with `*** Begin Patch` and end with `*** End Patch`",
        );
    }

    let mut actions = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let line = lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            index += 1;
            let mut content = Vec::new();
            while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                let added = lines[index].strip_prefix('+').ok_or_else(|| {
                    ToolError::Execution("add-file lines must start with `+`".into())
                })?;
                content.push(added);
                index += 1;
            }
            actions.push(PatchAction::Add {
                path: path.to_string(),
                content: join_patch_lines(&content),
            });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            actions.push(PatchAction::Delete {
                path: path.to_string(),
            });
            index += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            index += 1;
            let mut hunks = Vec::new();
            while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                if !lines[index].starts_with("@@") {
                    return execution_error(
                        "update-file content must begin with a `@@` hunk header",
                    );
                }
                index += 1;
                let mut old_lines = Vec::new();
                let mut new_lines = Vec::new();
                while index + 1 < lines.len()
                    && !lines[index].starts_with("@@")
                    && !lines[index].starts_with("*** ")
                {
                    let hunk_line = lines[index];
                    if hunk_line.is_empty() {
                        return execution_error("hunk lines must start with ` `, `+`, or `-`");
                    }
                    let (marker, text) = hunk_line.split_at(1);
                    match marker {
                        " " => {
                            old_lines.push(text.to_string());
                            new_lines.push(text.to_string());
                        }
                        "-" => old_lines.push(text.to_string()),
                        "+" => new_lines.push(text.to_string()),
                        _ => return execution_error("hunk lines must start with ` `, `+`, or `-`"),
                    }
                    index += 1;
                }
                if old_lines == new_lines {
                    continue;
                }
                hunks.push(Hunk {
                    old_lines,
                    new_lines,
                });
            }
            if hunks.is_empty() {
                return execution_error("update action does not change any content");
            }
            actions.push(PatchAction::Update {
                path: path.to_string(),
                hunks,
            });
        } else {
            return execution_error(format!("unknown patch directive `{line}`"));
        }
    }

    if actions.is_empty() {
        return execution_error("patch contains no file changes");
    }
    Ok(actions)
}

async fn prepare_changes(
    ctx: &ToolContext,
    actions: Vec<PatchAction>,
) -> Result<Vec<PreparedChange>, ToolError> {
    let mut seen = HashSet::new();
    let mut changes = Vec::with_capacity(actions.len());

    for action in actions {
        let path_text = match &action {
            PatchAction::Add { path, .. }
            | PatchAction::Update { path, .. }
            | PatchAction::Delete { path } => path.clone(),
        };
        if !seen.insert(path_text.clone()) {
            return execution_error(format!("patch changes `{path_text}` more than once"));
        }
        let path = safe_patch_path(ctx, &path_text)?;

        match action {
            PatchAction::Add { content, .. } => {
                if tokio::fs::try_exists(&path).await? {
                    return execution_error(format!("cannot add existing file `{path_text}`"));
                }
                changes.push(PreparedChange::Write {
                    path,
                    display_path: path_text,
                    original: None,
                    content,
                });
            }
            PatchAction::Update { hunks, .. } => {
                let original = tokio::fs::read_to_string(&path).await.map_err(|error| {
                    ToolError::Execution(format!("cannot update `{path_text}`: {error}"))
                })?;
                let content = apply_hunks(&original, &hunks, &path_text)?;
                changes.push(PreparedChange::Write {
                    path,
                    display_path: path_text,
                    original: Some(original),
                    content,
                });
            }
            PatchAction::Delete { .. } => {
                let original = tokio::fs::read_to_string(&path).await.map_err(|error| {
                    ToolError::Execution(format!("cannot delete `{path_text}`: {error}"))
                })?;
                changes.push(PreparedChange::Delete {
                    path,
                    display_path: path_text,
                    original,
                });
            }
        }
    }

    Ok(changes)
}

fn safe_patch_path(ctx: &ToolContext, path: &str) -> Result<PathBuf, ToolError> {
    let relative = Path::new(path);
    if path.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return execution_error(format!("patch path `{path}` must be workspace-relative"));
    }

    let target = ctx.workspace_root.join(relative);
    let root = ctx
        .workspace_root
        .canonicalize()
        .map_err(|error| ToolError::Execution(format!("cannot resolve workspace: {error}")))?;
    let mut ancestor = target.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| ToolError::Execution(format!("cannot resolve patch path `{path}`")))?;
    }
    let canonical = ancestor.canonicalize()?;
    if !canonical.starts_with(&root) {
        return execution_error(format!("patch path `{path}` escapes workspace"));
    }
    Ok(target)
}

fn apply_hunks(original: &str, hunks: &[Hunk], path: &str) -> Result<String, ToolError> {
    let had_trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    let mut cursor = 0;

    for hunk in hunks {
        let position = find_sequence(&lines, &hunk.old_lines, cursor)
            .ok_or_else(|| ToolError::Execution(format!("hunk did not match file `{path}`")))?;
        let old_len = hunk.old_lines.len();
        lines.splice(position..position + old_len, hunk.new_lines.clone());
        cursor = position + hunk.new_lines.len();
    }

    let mut content = lines.join("\n");
    if had_trailing_newline {
        content.push('\n');
    }
    Ok(content)
}

fn find_sequence(lines: &[String], needle: &[String], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(lines.len()));
    }
    if needle.len() > lines.len() || start > lines.len() - needle.len() {
        return None;
    }
    (start..=lines.len() - needle.len())
        .find(|&index| lines[index..index + needle.len()] == *needle)
}

fn join_patch_lines(lines: &[&str]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn execution_error<T>(message: impl Into<String>) -> Result<T, ToolError> {
    Err(ToolError::Execution(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn applies_add_update_and_delete() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("update.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.path().join("delete.txt"), "gone\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Add File: nested/new.txt\n+new\n+file\n*** Update File: update.txt\n@@\n one\n-two\n+second\n three\n*** Delete File: delete.txt\n*** End Patch";

        let output = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert!(!output.is_error);
        for expected in [
            "diff --git a/nested/new.txt b/nested/new.txt",
            "diff --git a/update.txt b/update.txt",
            "diff --git a/delete.txt b/delete.txt",
            "+second",
            "-gone",
        ] {
            assert!(
                output.content.contains(expected),
                "missing {expected} in:\n{}",
                output.content
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nested/new.txt")).unwrap(),
            "new\nfile\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("update.txt")).unwrap(),
            "one\nsecond\nthree\n"
        );
        assert!(!dir.path().join("delete.txt").exists());
    }

    #[tokio::test]
    async fn validates_all_actions_before_writing() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Add File: created.txt\n+created\n*** Update File: missing.txt\n@@\n-old\n+new\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("missing.txt"));
        assert!(!dir.path().join("created.txt").exists());
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Add File: ../escape.txt\n+nope\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("workspace-relative"));
    }

    #[tokio::test]
    async fn rejects_non_matching_hunk_without_writing() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "actual\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n-expected\n+replacement\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "actual\n"
        );
    }
}

#[cfg(test)]
mod noop_hunk_regression_tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn ignores_noop_hunks_when_other_hunks_change_content() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "one\ntwo\nthree\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n one\n two\n@@\n two\n-three\n+done\n*** End Patch";

        let output = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert!(!output.is_error);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "one\ntwo\ndone\n"
        );
    }

    #[tokio::test]
    async fn rejects_update_when_all_hunks_are_noops() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "one\ntwo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n one\n two\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("update action does not change any content"));
    }
}
