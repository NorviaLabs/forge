use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::builtins::unified_diff;
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

/// Models sometimes dump an entire file into a patch. Cap the argument so a
/// single call cannot pin hundreds of megabytes in the agent loop.
const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ApplyPatchArgs {
    /// Patch in the `*** Begin Patch` format.
    pub patch: String,
}

pub struct ApplyPatchTool;

#[derive(Debug)]
enum PatchAction {
    Add {
        path: String,
        content: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<Hunk>,
    },
    Delete {
        path: String,
    },
}

#[derive(Debug)]
struct Hunk {
    /// Optional text after `@@`, used as a location hint when the same old
    /// lines appear more than once.
    header: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    /// When set, the old lines must be a suffix of the file (or empty, which
    /// means append).
    end_of_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Newline {
    Lf,
    Crlf,
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
        "Apply a validated patch to workspace files using the `*** Begin Patch` format. \
Supports Add/Update/Delete File, optional `*** Move to:`, `@@` hunks, and `*** End of File`."
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
        if args.patch.len() > MAX_PATCH_BYTES {
            return execution_error(format!(
                "patch is {} bytes; maximum is {MAX_PATCH_BYTES} bytes",
                args.patch.len()
            ));
        }
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
                } => unified_diff(display_path, original.as_deref(), content)?,
                PreparedChange::Delete {
                    display_path,
                    original,
                    ..
                } => unified_diff(display_path, Some(original), "")?,
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
            outcome: Default::default(),
            content: diffs.join("\n"),
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

fn parse_patch(patch: &str) -> Result<Vec<PatchAction>, ToolError> {
    let lines = extract_marked_patch(patch)?;

    let mut actions = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let line = lines[index];
        if let Some(path) = strip_file_directive(line, "Add File") {
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
        } else if let Some(path) = strip_file_directive(line, "Delete File") {
            actions.push(PatchAction::Delete {
                path: path.to_string(),
            });
            index += 1;
        } else if let Some(path) = strip_file_directive(line, "Update File")
            .or_else(|| strip_file_directive(line, "Change File"))
        {
            index += 1;
            let mut move_to = None;
            if index + 1 < lines.len() {
                if let Some(dest) = strip_file_directive(lines[index], "Move to") {
                    move_to = Some(dest.to_string());
                    index += 1;
                }
            }
            let mut hunks = Vec::new();
            while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                if !lines[index].starts_with("@@") {
                    return execution_error(
                        "update-file content must begin with a `@@` hunk header",
                    );
                }
                let header = hunk_header(lines[index]);
                index += 1;
                let mut old_lines = Vec::new();
                let mut new_lines = Vec::new();
                while index + 1 < lines.len()
                    && !lines[index].starts_with("@@")
                    && !lines[index].starts_with("*** ")
                {
                    parse_hunk_line(lines[index], &mut old_lines, &mut new_lines)?;
                    index += 1;
                }
                let mut end_of_file = false;
                if index + 1 < lines.len() && lines[index].trim() == "*** End of File" {
                    end_of_file = true;
                    index += 1;
                }
                if old_lines == new_lines && !end_of_file {
                    continue;
                }
                hunks.push(Hunk {
                    header,
                    old_lines,
                    new_lines,
                    end_of_file,
                });
            }
            if hunks.is_empty() && move_to.is_none() {
                return execution_error("update action does not change any content");
            }
            actions.push(PatchAction::Update {
                path: path.to_string(),
                move_to,
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

/// Locate the `*** Begin Patch` … `*** End Patch` envelope even when the
/// model wrapped it in a markdown fence or added trailing blank lines.
fn extract_marked_patch(patch: &str) -> Result<Vec<&str>, ToolError> {
    let lines: Vec<&str> = patch.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim() == "*** Begin Patch");
    let end = lines
        .iter()
        .rposition(|line| line.trim() == "*** End Patch");
    match (start, end) {
        (Some(start), Some(end)) if start < end => Ok(lines[start..=end].to_vec()),
        _ => execution_error("patch must contain `*** Begin Patch` and `*** End Patch`"),
    }
}

fn strip_file_directive<'a>(line: &'a str, kind: &str) -> Option<&'a str> {
    let rest = line.strip_prefix("*** ")?;
    let rest = rest.strip_prefix(kind)?;
    let rest = rest.strip_prefix(':')?;
    let path = rest.trim();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn hunk_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("@@")?.trim();
    // Unified-diff coordinates (`-12,3 +12,4`) are not a location hint.
    if rest.is_empty() || rest.starts_with('-') || rest.starts_with('+') {
        None
    } else {
        Some(rest.to_string())
    }
}

fn parse_hunk_line(
    hunk_line: &str,
    old_lines: &mut Vec<String>,
    new_lines: &mut Vec<String>,
) -> Result<(), ToolError> {
    // Models often omit the required leading space on a blank context line.
    if hunk_line.is_empty() {
        old_lines.push(String::new());
        new_lines.push(String::new());
        return Ok(());
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
    Ok(())
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
            PatchAction::Update { hunks, move_to, .. } => {
                let original = read_text_file(&path, "update", &path_text).await?;
                let content = if hunks.is_empty() {
                    original.clone()
                } else {
                    apply_hunks(&original, &hunks, &path_text)?
                };
                if let Some(dest_text) = move_to {
                    if !seen.insert(dest_text.clone()) {
                        return execution_error(format!(
                            "patch changes `{dest_text}` more than once"
                        ));
                    }
                    let dest = safe_patch_path(ctx, &dest_text)?;
                    if tokio::fs::try_exists(&dest).await? {
                        return execution_error(format!(
                            "cannot move `{path_text}` to existing file `{dest_text}`"
                        ));
                    }
                    changes.push(PreparedChange::Write {
                        path: dest,
                        display_path: dest_text,
                        original: None,
                        content,
                    });
                    changes.push(PreparedChange::Delete {
                        path,
                        display_path: path_text,
                        original,
                    });
                } else {
                    changes.push(PreparedChange::Write {
                        path,
                        display_path: path_text,
                        original: Some(original),
                        content,
                    });
                }
            }
            PatchAction::Delete { .. } => {
                let original = read_text_file(&path, "delete", &path_text).await?;
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
    // Same confinement as `write_file`: workspace-only, no `.git`, no dangling
    // or escaping symlinks. Absolute paths are accepted when they resolve
    // inside the workspace — models sometimes pass those.
    ctx.resolve_write_path(path)
}

async fn read_text_file(path: &Path, verb: &str, display: &str) -> Result<String, ToolError> {
    tokio::fs::read_to_string(path).await.map_err(|error| {
        let detail = if error.kind() == std::io::ErrorKind::InvalidData {
            "file is not valid UTF-8".to_string()
        } else {
            error.to_string()
        };
        ToolError::Execution(format!("cannot {verb} `{display}`: {detail}"))
    })
}

fn apply_hunks(original: &str, hunks: &[Hunk], path: &str) -> Result<String, ToolError> {
    let newline = detect_newline(original);
    let had_trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    // Precompute once; the rolling window makes each hunk O(N) instead of
    // O(N*M) per-position string comparisons.
    let mut line_hashes = lines.iter().map(|line| hash_line(line)).collect::<Vec<_>>();
    let mut cursor = 0;

    for hunk in hunks {
        let position = locate_hunk(&lines, &line_hashes, hunk, cursor, path)?;
        let old_len = hunk.old_lines.len();
        lines.splice(position..position + old_len, hunk.new_lines.clone());
        line_hashes.splice(
            position..position + old_len,
            hunk.new_lines.iter().map(|line| hash_line(line)),
        );
        cursor = position + hunk.new_lines.len();
    }

    Ok(join_file_lines(&lines, newline, had_trailing_newline))
}

fn locate_hunk(
    lines: &[String],
    line_hashes: &[u64],
    hunk: &Hunk,
    cursor: usize,
    path: &str,
) -> Result<usize, ToolError> {
    if hunk.end_of_file {
        if hunk.old_lines.is_empty() {
            return Ok(lines.len());
        }
        let start = lines.len().saturating_sub(hunk.old_lines.len());
        if lines.get(start..) == Some(hunk.old_lines.as_slice()) {
            return Ok(start);
        }
        return execution_error(format!(
            "hunk marked End of File did not match the end of `{path}`"
        ));
    }

    // Addition-only hunks have no old lines to search for. Inserting at the
    // cursor would prepend on the first hunk; models almost always mean append.
    if hunk.old_lines.is_empty() {
        return Ok(lines.len());
    }

    let mut search_from = cursor;
    if let Some(hint) = hunk.header.as_deref() {
        if let Some(offset) = lines[cursor..].iter().position(|line| line.contains(hint)) {
            search_from = cursor + offset;
        }
    }

    find_sequence(lines, line_hashes, &hunk.old_lines, search_from)
        .or_else(|| {
            if search_from != cursor {
                find_sequence(lines, line_hashes, &hunk.old_lines, cursor)
            } else {
                None
            }
        })
        .ok_or_else(|| ToolError::Execution(format!("hunk did not match file `{path}`")))
}

fn detect_newline(text: &str) -> Newline {
    if text.contains("\r\n") {
        Newline::Crlf
    } else {
        Newline::Lf
    }
}

fn join_file_lines(lines: &[String], newline: Newline, trailing: bool) -> String {
    let sep = match newline {
        Newline::Lf => "\n",
        Newline::Crlf => "\r\n",
    };
    let mut content = lines.join(sep);
    if trailing {
        content.push_str(sep);
    }
    content
}

/// FNV-1a over the line bytes. Collisions are harmless: a match is always
/// confirmed with an exact string compare, so a hash collision at worst costs
/// one extra compare.
fn hash_line(line: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in line.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Locate `needle` at or after `start` via a rolling hash window over
/// `line_hashes`, confirmed by an exact slice compare on candidate positions.
fn find_sequence(
    lines: &[String],
    line_hashes: &[u64],
    needle: &[String],
    start: usize,
) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(lines.len()));
    }
    if needle.len() > lines.len() || start > lines.len() - needle.len() {
        return None;
    }
    let needle_hash: u64 = needle
        .iter()
        .fold(0u64, |hash, line| hash.wrapping_add(hash_line(line)));
    let end = lines.len() - needle.len();
    let mut window_hash: u64 = line_hashes[start..start + needle.len()]
        .iter()
        .fold(0u64, |hash, &value| hash.wrapping_add(value));
    for index in start..=end {
        if window_hash == needle_hash && lines[index..index + needle.len()] == *needle {
            return Some(index);
        }
        if index < end {
            window_hash = window_hash
                .wrapping_sub(line_hashes[index])
                .wrapping_add(line_hashes[index + needle.len()]);
        }
    }
    None
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

        assert!(error.to_string().contains("escapes workspace"), "{error}");
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
    async fn treats_blank_hunk_line_as_empty_context() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "one\n\ntwo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        // Models often omit the required leading space on a blank context line.
        let patch =
            "*** Begin Patch\n*** Update File: file.txt\n@@\n one\n\n-two\n+done\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "one\n\ndone\n"
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
        let hashes = lines.iter().map(|line| hash_line(line)).collect::<Vec<_>>();
        assert_eq!(find_sequence(&lines, &hashes, &[], 1), Some(1));
        // A start position past the end of `lines` is clamped, not returned raw.
        assert_eq!(find_sequence(&lines, &hashes, &[], 50), Some(2));
    }

    #[test]
    fn find_sequence_returns_none_when_needle_cannot_fit() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let hashes = lines.iter().map(|line| hash_line(line)).collect::<Vec<_>>();
        let needle = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(find_sequence(&lines, &hashes, &needle, 0), None);

        // Needle fits in `lines` overall but not from this late a start index.
        let short_needle = vec!["b".to_string()];
        assert_eq!(find_sequence(&lines, &hashes, &short_needle, 5), None);
    }

    #[test]
    fn find_sequence_locates_a_matching_run() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let hashes = lines.iter().map(|line| hash_line(line)).collect::<Vec<_>>();
        let needle = vec!["b".to_string(), "c".to_string()];
        assert_eq!(find_sequence(&lines, &hashes, &needle, 0), Some(1));
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
            dir.path().join("existing.txt").canonicalize().unwrap()
        );
        assert!(safe_patch_path(&ctx, ".gitignore").is_ok());
    }

    #[test]
    fn allows_absolute_path_inside_workspace() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let abs = dir.path().join("inside.txt");
        assert_eq!(safe_patch_path(&ctx, abs.to_str().unwrap()).unwrap(), abs);
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

#[cfg(test)]
mod robustness_tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn accepts_patch_wrapped_in_markdown_and_trailing_blank_lines() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "```\n*** Begin Patch\n*** Add File: wrapped.txt\n+hi\n*** End Patch\n```\n\n";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("wrapped.txt")).unwrap(),
            "hi\n"
        );
    }

    #[tokio::test]
    async fn preserves_crlf_line_endings() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("win.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: win.txt\n@@\n one\n-two\n+second\n three\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("win.txt")).unwrap(),
            "one\r\nsecond\r\nthree\r\n"
        );
    }

    #[tokio::test]
    async fn addition_only_hunk_appends_instead_of_prepending() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "keep\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n+tail\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "keep\ntail\n"
        );
    }

    #[tokio::test]
    async fn end_of_file_marker_requires_a_suffix_match() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "alpha\nomega\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n-omega\n+done\n*** End of File\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "alpha\ndone\n"
        );
    }

    #[tokio::test]
    async fn end_of_file_marker_rejects_non_suffix() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "alpha\nomega\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n-alpha\n+nope\n*** End of File\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("End of File"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "alpha\nomega\n"
        );
    }

    #[tokio::test]
    async fn hunk_header_disambiguates_repeated_old_lines() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("file.py"),
            "def first():\n    pass\ndef second():\n    pass\n",
        )
        .unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: file.py\n@@ def second():\n-    pass\n+    return 2\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.py")).unwrap(),
            "def first():\n    pass\ndef second():\n    return 2\n"
        );
    }

    #[tokio::test]
    async fn moves_a_file_and_applies_hunks() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "one\ntwo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: old.txt\n*** Move to: nested/new.txt\n@@\n one\n-two\n+moved\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert!(!dir.path().join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nested/new.txt")).unwrap(),
            "one\nmoved\n"
        );
    }

    #[tokio::test]
    async fn rename_only_move_is_allowed() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "keep\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch =
            "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert!(!dir.path().join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "keep\n"
        );
    }

    #[tokio::test]
    async fn accepts_change_file_as_update_alias() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "old\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Change File: file.txt\n@@\n-old\n+new\n*** End Patch";

        ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "new\n"
        );
    }

    #[tokio::test]
    async fn rejects_oversized_patch_without_touching_disk() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let huge = format!(
            "*** Begin Patch\n*** Add File: huge.txt\n+{}\n*** End Patch",
            "x".repeat(MAX_PATCH_BYTES)
        );

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": huge}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("maximum is"), "{error}");
        assert!(!dir.path().join("huge.txt").exists());
    }

    #[tokio::test]
    async fn rejects_binary_update_with_utf8_message() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("bin.dat"), [0xff, 0xfe, 0x00]).unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let patch = "*** Begin Patch\n*** Update File: bin.dat\n@@\n-x\n+y\n*** End Patch";

        let error = ApplyPatchTool
            .call(&ctx, json!({"patch": patch}))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("UTF-8"), "{error}");
    }

    #[test]
    fn extract_marked_patch_requires_both_markers() {
        assert!(extract_marked_patch("no markers").is_err());
        assert!(extract_marked_patch("*** Begin Patch\n*** Add File: a\n").is_err());
    }
}
