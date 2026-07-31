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
            exit_code: None,
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

    // A patch writes, and Git takes executable behaviour from its own config
    // and hook files, so `.git` is off limits to this tool entirely.
    if relative
        .components()
        .any(|component| matches!(component, Component::Normal(name) if name == ".git"))
    {
        return execution_error(format!(
            "refusing to patch `{path}`: paths under `.git` are not writable by tools"
        ));
    }

    let target = ctx.workspace_root.join(relative);
    let root = ctx
        .workspace_root
        .canonicalize()
        .map_err(|error| ToolError::Execution(format!("cannot resolve workspace: {error}")))?;

    // Walk with `symlink_metadata`, not `exists()`. `exists()` follows symlinks
    // and so reports false for a *dangling* one, which stepped the walk past the
    // link and left its target unchecked — a link committed in a repository
    // could then redirect the write outside the workspace.
    let mut ancestor = target.as_path();
    while ancestor.symlink_metadata().is_err() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| ToolError::Execution(format!("cannot resolve patch path `{path}`")))?;
    }

    // `canonicalize` resolves links, so an ancestor pointing outside the
    // workspace is caught below and one pointing inside still works. A dangling
    // link cannot be resolved, so it cannot be shown to be contained.
    let canonical = ancestor.canonicalize().map_err(|_| {
        ToolError::Execution(format!(
            "patch path `{path}` resolves through a broken symlink"
        ))
    })?;
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

    #[test]
    fn describes_itself() {
        let tool = ApplyPatchTool;
        assert_eq!(tool.name(), "apply_patch");
        assert!(tool.description().contains("*** Begin Patch"));
        assert_eq!(tool.side_effect_class(), SideEffectClass::Write);
        let schema = tool.input_schema();
        assert!(schema.get("properties").is_some(), "{schema}");
    }

    #[tokio::test]
    async fn call_reports_deserialize_failure_for_non_string_patch() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": 12345}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("invalid type"), "{error}");
    }

    #[tokio::test]
    async fn rejects_patch_missing_begin_or_end_markers() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Add File: created.txt\n+created\n";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("Begin Patch"), "{error}");
    }

    #[tokio::test]
    async fn rejects_add_file_line_missing_plus_prefix() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Add File: created.txt\nno-prefix\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("add-file lines must start with `+`"),
            "{error}"
        );
        assert!(!dir.path().join("created.txt").exists());
    }

    #[tokio::test]
    async fn rejects_update_file_content_not_starting_with_hunk_header() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "one\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.txt\nnot-a-hunk-header\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("must begin with a `@@` hunk header"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rejects_empty_hunk_line() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "one\ntwo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        // A hunk line that is exactly empty is neither ` `, `+`, nor `-` prefixed.
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n one\n\n-two\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("hunk lines must start with ` `, `+`, or `-`"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rejects_hunk_line_with_unrecognized_marker() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "one\ntwo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n#bad\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("hunk lines must start with ` `, `+`, or `-`"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rejects_unknown_patch_directive() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Rename File: a.txt\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unknown patch directive `*** Rename File: a.txt`"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rejects_patch_with_no_file_changes() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("patch contains no file changes"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rejects_patch_that_touches_the_same_path_twice() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch =
            "*** Begin Patch\n*** Add File: dup.txt\n+one\n*** Delete File: dup.txt\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("patch changes `dup.txt` more than once"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rejects_adding_a_file_that_already_exists() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "already here\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Add File: existing.txt\n+new content\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("cannot add existing file `existing.txt`"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("existing.txt")).unwrap(),
            "already here\n"
        );
    }

    #[tokio::test]
    async fn rejects_deleting_a_file_that_does_not_exist() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("cannot delete `missing.txt`"),
            "{error}"
        );
    }

    #[test]
    fn find_sequence_with_empty_needle_returns_clamped_start() {
        let lines = vec!["a".to_string(), "b".to_string()];
        assert_eq!(find_sequence(&lines, &[], 1), Some(1));
        // A start position past the end of `lines` is clamped, not returned raw.
        assert_eq!(find_sequence(&lines, &[], 50), Some(2));
    }

    #[test]
    fn find_sequence_returns_none_when_needle_cannot_fit() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let needle = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(find_sequence(&lines, &needle, 0), None);

        // Needle fits in `lines` overall but not from this late a start index.
        let short_needle = vec!["b".to_string()];
        assert_eq!(find_sequence(&lines, &short_needle, 5), None);
    }

    #[test]
    fn find_sequence_locates_a_matching_run() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let needle = vec!["b".to_string(), "c".to_string()];
        assert_eq!(find_sequence(&lines, &needle, 0), Some(1));
    }

    #[test]
    fn join_patch_lines_returns_empty_string_for_no_lines() {
        assert_eq!(join_patch_lines(&[]), "");
    }

    #[test]
    fn join_patch_lines_joins_and_terminates_with_newline() {
        assert_eq!(join_patch_lines(&["a", "b"]), "a\nb\n");
    }
}

#[cfg(test)]
mod path_confinement_tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    /// `safe_patch_path` already rejected absolute paths and every non-`Normal`
    /// component, so the only way past it was a link the walk stepped over:
    /// `exists()` reports false for a dangling symlink.
    #[cfg(unix)]
    #[test]
    fn rejects_dangling_symlink_target() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let escape_target = outside.join("absent.txt");
        std::os::unix::fs::symlink(&escape_target, workspace.join("docs/latest")).unwrap();
        let ctx = ToolContext::new(workspace);

        let error = safe_patch_path(&ctx, "docs/latest").unwrap_err();
        assert!(error.to_string().contains("symlink"));
        assert!(!escape_target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_directory_escaping_the_workspace() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("linked")).unwrap();
        let ctx = ToolContext::new(workspace);

        let error = safe_patch_path(&ctx, "linked/new.txt").unwrap_err();
        assert!(error.to_string().contains("escapes workspace"));
    }

    /// Symlinks are not inherently refused — only ones that cannot be shown to
    /// stay inside the workspace. A link pointing within it still resolves.
    #[cfg(unix)]
    #[test]
    fn still_allows_symlinked_directory_inside_the_workspace() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(workspace.join("real")).unwrap();
        std::os::unix::fs::symlink(workspace.join("real"), workspace.join("linked")).unwrap();
        let ctx = ToolContext::new(workspace.clone());

        assert_eq!(
            safe_patch_path(&ctx, "linked/new.txt").unwrap(),
            workspace.join("linked/new.txt")
        );
    }

    #[test]
    fn refuses_paths_under_git() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        for path in [".git/config", ".git/hooks/pre-commit", "nested/.git/config"] {
            let error = safe_patch_path(&ctx, path).unwrap_err();
            assert!(
                error.to_string().contains(".git"),
                "expected `{path}` to be refused"
            );
        }
    }

    /// End-to-end: the tool reports the refusal and writes nothing.
    #[tokio::test]
    async fn patching_git_config_is_refused_end_to_end() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let config = dir.path().join(".git/config");
        std::fs::write(&config, "[core]\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch =
            "*** Begin Patch\n*** Update File: .git/config\n@@\n-[core]\n+[diff]\n*** End Patch";

        let result = ApplyPatchTool.call(&ctx, json!({"patch": patch})).await;

        match result {
            Ok(output) => assert!(output.is_error, "expected an error output"),
            Err(error) => assert!(error.to_string().contains(".git")),
        }
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "[core]\n");
    }

    #[test]
    fn still_allows_ordinary_new_and_existing_paths() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "hi\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        assert_eq!(
            safe_patch_path(&ctx, "nested/deeper/new.txt").unwrap(),
            dir.path().join("nested/deeper/new.txt")
        );
        assert_eq!(
            safe_patch_path(&ctx, "existing.txt").unwrap(),
            dir.path().join("existing.txt")
        );
        assert!(safe_patch_path(&ctx, ".gitignore").is_ok());
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
