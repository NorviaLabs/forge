use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;

use crate::registry::ToolContext;
use crate::{Tool, ToolError};

pub(crate) fn schema_for<T: JsonSchema>() -> Value {
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

fn unique_temp_path(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "forge-write-{label}-{stamp}-{}",
        std::process::id()
    ))
}

pub(crate) async fn unified_diff(
    path: &str,
    old: Option<&str>,
    new: &str,
) -> Result<String, ToolError> {
    let old_path = unique_temp_path("old");
    let new_path = unique_temp_path("new");
    let old_content = old.unwrap_or("");
    tokio::fs::write(&old_path, old_content).await?;
    tokio::fs::write(&new_path, new).await?;

    let out = Command::new("git")
        .arg("diff")
        .arg("--no-index")
        .arg("--no-color")
        .arg("--unified=3")
        .arg(&old_path)
        .arg(&new_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| ToolError::Execution(e.to_string()))?;

    let _ = tokio::fs::remove_file(&old_path).await;
    let _ = tokio::fs::remove_file(&new_path).await;

    if !matches!(out.status.code(), Some(0 | 1)) {
        let error = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(ToolError::Execution(if error.is_empty() {
            format!("git diff failed with {}", out.status)
        } else {
            error
        }));
    }

    let old_name = old_path.to_string_lossy();
    let new_name = new_path.to_string_lossy();
    let old_name = old_name.trim_start_matches('/');
    let new_name = new_name.trim_start_matches('/');
    let diff = String::from_utf8_lossy(&out.stdout)
        .replace(&format!("a/{old_name}"), &format!("a/{path}"))
        .replace(&format!("b/{new_name}"), &format!("b/{path}"));
    Ok(diff)
}

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
        let a: WriteFileArgs =
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?;
        let path = ctx.resolve_path(&a.path)?;
        let old = tokio::fs::read_to_string(&path).await.ok();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, a.content.as_bytes()).await?;
        let diff = unified_diff(&a.path, old.as_deref(), &a.content).await?;
        let content = if diff.trim().is_empty() {
            format!("wrote {} bytes to {}", a.content.len(), a.path)
        } else {
            diff
        };
        Ok(ToolOutput {
            content,
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

/// Allowlisted git subcommands (not a free-form shell).
const GIT_ALLOWED_SUBCOMMANDS: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "branch",
    "add",
    "commit",
    "checkout",
    "switch",
    "restore",
    "stash",
    "rev-parse",
    "ls-files",
    "remote",
    "fetch",
    "pull",
    "push",
    "merge",
    "rebase",
    "cherry-pick",
    "tag",
    "blame",
    "init",
    "clone",
];

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GitArgs {
    /// Git subcommand (e.g. status, diff, log, add, commit, push).
    pub subcommand: String,
    /// Additional arguments after the subcommand (e.g. ["--stat"], ["-m", "msg"]).
    #[serde(default)]
    pub args: Vec<String>,
}

pub struct GitTool;

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }
    fn description(&self) -> &str {
        "Run an allowlisted git subcommand in the workspace (status, diff, log, add, commit, branch, push, …). Not a free-form shell."
    }
    fn input_schema(&self) -> Value {
        schema_for::<GitArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        // Reads are common; writes/push also go through this tool — classify as Write for ACL.
        SideEffectClass::Write
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: GitArgs =
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?;
        let sub = a.subcommand.trim().to_ascii_lowercase();
        if sub.is_empty() {
            return Err(ToolError::Execution(
                "git: subcommand is required (e.g. status, diff, commit)".into(),
            ));
        }
        if !GIT_ALLOWED_SUBCOMMANDS.contains(&sub.as_str()) {
            return Err(ToolError::Execution(format!(
                "git: subcommand `{sub}` is not allowlisted; allowed: {}",
                GIT_ALLOWED_SUBCOMMANDS.join(", ")
            )));
        }
        // Reject args that look like option injectors for a second git command
        for arg in &a.args {
            if arg.starts_with("--git-dir")
                || arg.starts_with("--work-tree")
                || arg == "-C"
                || arg.starts_with("-c")
            {
                return Err(ToolError::Execution(format!(
                    "git: argument `{arg}` is not allowed"
                )));
            }
        }

        let mut cmd = Command::new("git");
        cmd.arg(&sub)
            .args(&a.args)
            .current_dir(&ctx.workspace_root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = cmd
            .output()
            .await
            .map_err(|e| ToolError::Execution(format!("failed to run git: {e}")))?;

        let mut content = String::from_utf8_lossy(&out.stdout).into_owned();
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&err);
        }
        if content.trim().is_empty() && out.status.success() {
            content = format!("git {sub}: ok");
        }
        Ok(ToolOutput {
            content,
            is_error: !out.status.success(),
        })
    }
}

/// Phase 1 workspace tools only (no web_search). Prefer
/// [`default_builtins_with_web_search`] when config is available.
pub fn default_builtins() -> Vec<std::sync::Arc<dyn Tool>> {
    let mut tools: Vec<std::sync::Arc<dyn Tool>> = vec![
        std::sync::Arc::new(ReadFileTool),
        std::sync::Arc::new(WriteFileTool),
        std::sync::Arc::new(crate::ApplyPatchTool),
        std::sync::Arc::new(BashTool),
        std::sync::Arc::new(GitTool),
    ];
    tools.extend(crate::fast_file_tools::fff_tools());
    tools
}

/// Phase 1 built-ins plus optional Phase 9 `web_search` when config allows.
pub fn default_builtins_with_web_search(
    web_search: &forge_config::WebSearchConfig,
) -> Vec<std::sync::Arc<dyn Tool>> {
    let mut tools = default_builtins();
    if let Some(t) = crate::web_search::web_search_tool(web_search) {
        tools.push(t);
    }
    tools
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
            .call(&ctx, json!({"path": "n/a.txt", "content": "xyz"}))
            .await
            .unwrap();
        let out = ReadFileTool
            .call(&ctx, json!({"path": "n/a.txt"}))
            .await
            .unwrap();
        assert_eq!(out.content, "xyz");
    }

    #[tokio::test]
    async fn write_file_returns_diff_without_git_help() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = WriteFileTool
            .call(&ctx, json!({"path": "sample.txt", "content": "hello\n"}))
            .await
            .unwrap();

        assert!(!out.is_error);
        assert!(out.content.contains("--- a/sample.txt"), "{}", out.content);
        assert!(out.content.contains("+++ b/sample.txt"), "{}", out.content);
        assert!(!out.content.contains("usage: git diff"), "{}", out.content);
    }

    #[test]
    fn default_builtins_with_web_search_includes_mock() {
        let cfg = forge_config::WebSearchConfig::default();
        let tools = default_builtins_with_web_search(&cfg);
        assert!(tools.iter().any(|t| t.name() == "web_search"));
        assert!(tools.iter().any(|t| t.name() == "read_file"));
        assert!(tools.iter().any(|t| t.name() == "git"));
    }

    #[test]
    fn default_builtins_omits_web_search_when_disabled() {
        let cfg = forge_config::WebSearchConfig {
            enabled: false,
            ..Default::default()
        };
        let tools = default_builtins_with_web_search(&cfg);
        assert!(!tools.iter().any(|t| t.name() == "web_search"));
        assert_eq!(tools.len(), default_builtins().len());
    }

    #[test]
    fn default_builtins_includes_git() {
        let tools = default_builtins();
        assert!(tools.iter().any(|t| t.name() == "git"));
        assert!(tools.iter().any(|t| t.name() == "apply_patch"));
        assert!(tools.iter().any(|t| t.name() == "fffind"));
        assert!(tools.iter().any(|t| t.name() == "ffgrep"));
    }

    #[tokio::test]
    async fn git_status_in_repo() {
        let dir = tempdir().unwrap();
        // init repo
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "forge@test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Forge Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();

        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = GitTool
            .call(
                &ctx,
                json!({"subcommand": "status", "args": ["--porcelain"]}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("a.txt") || out.content.contains("??"),
            "got {}",
            out.content
        );
    }

    #[tokio::test]
    async fn git_rejects_unknown_subcommand() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let err = GitTool
            .call(&ctx, json!({"subcommand": "daemon"}))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("allowlisted") || err.to_string().contains("daemon"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn git_add_and_commit() {
        let dir = tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "forge@test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Forge Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("b.txt"), "content").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        GitTool
            .call(&ctx, json!({"subcommand": "add", "args": ["b.txt"]}))
            .await
            .unwrap();
        let out = GitTool
            .call(
                &ctx,
                json!({"subcommand": "commit", "args": ["-m", "add b"]}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let log = GitTool
            .call(
                &ctx,
                json!({"subcommand": "log", "args": ["-1", "--oneline"]}),
            )
            .await
            .unwrap();
        assert!(log.content.contains("add b"), "{}", log.content);
    }

    #[test]
    fn fff_find_schema_rejects_empty_args() {
        let t = crate::fast_file_tools::FffFindTool::new(std::sync::Arc::new(
            crate::fast_file_tools::FastFileState::new(),
        ));
        let err =
            crate::validation::validate_args("fffind", &t.input_schema(), &json!({})).unwrap_err();
        assert_eq!(err.tool, "fffind");
    }

    #[test]
    fn fff_grep_schema_rejects_empty_args() {
        let state = std::sync::Arc::new(crate::fast_file_tools::FastFileState::new());
        let t = crate::fast_file_tools::FffGrepTool::new(state);
        let err =
            crate::validation::validate_args("ffgrep", &t.input_schema(), &json!({})).unwrap_err();
        assert_eq!(err.tool, "ffgrep");
    }

    #[test]
    fn fff_find_schema_accepts_query() {
        let t = crate::fast_file_tools::FffFindTool::new(std::sync::Arc::new(
            crate::fast_file_tools::FastFileState::new(),
        ));
        crate::validation::validate_args("fffind", &t.input_schema(), &json!({"query": "main.rs"}))
            .unwrap();
    }

    #[test]
    fn fff_grep_schema_accepts_pattern() {
        let state = std::sync::Arc::new(crate::fast_file_tools::FastFileState::new());
        let t = crate::fast_file_tools::FffGrepTool::new(state);
        crate::validation::validate_args("ffgrep", &t.input_schema(), &json!({"pattern": "TODO"}))
            .unwrap();
    }
}
