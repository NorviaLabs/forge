use async_trait::async_trait;
use forge_types::{ExecutionOutcome, SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::registry::ToolContext;
use crate::{Tool, ToolError};

pub(crate) fn schema_for<T: JsonSchema>() -> Value {
    let s = schemars::schema_for!(T);
    serde_json::to_value(s).unwrap_or_else(|_| json!({"type": "object"}))
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadFileArgs {
    /// Path relative to workspace root (or absolute under the workspace or
    /// session temp).
    pub path: String,
    /// 1-based start line (integer or null). Separate field from `limit`.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Max lines to return (integer or null). Separate field from `offset`.
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
        "Read a text file from the workspace or session temp"
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
        const MAX_UNBOUNDED_READ_BYTES: u64 = 2 * 1024 * 1024;
        if a.offset.is_none() && a.limit.is_none() {
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                if meta.len() > MAX_UNBOUNDED_READ_BYTES {
                    return Ok(ToolOutput::failed_exit(
                        format!(
                            "`{}` is {} bytes; pass `offset` and `limit` to read a slice (max {MAX_UNBOUNDED_READ_BYTES} bytes without a range)",
                            a.path,
                            meta.len()
                        ),
                        None,
                    ));
                }
            }
        }
        let mut header = [0_u8; 32];
        {
            let mut probe = tokio::fs::File::open(&path).await?;
            let n = probe.read(&mut header).await?;
            if forge_types::sniff_allowed_image(&header[..n]) {
                let hint = if ctx.image_input {
                    "use view_image on this path"
                } else {
                    "the active model does not support image inputs"
                };
                return Ok(ToolOutput::failed_exit(
                    format!("`{}` is an image; {hint}", a.path),
                    None,
                ));
            }
        }
        let file = tokio::fs::File::open(&path).await?;
        let mut lines = BufReader::new(file).lines();
        let start = a.offset.unwrap_or(1).saturating_sub(1);
        for _ in 0..start {
            if lines.next_line().await?.is_none() {
                break;
            }
        }
        let mut content = String::new();
        let mut count = 0;
        while a.limit.is_none_or(|limit| count < limit) {
            let Some(line) = lines.next_line().await? else {
                break;
            };
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&line);
            count += 1;
        }
        Ok(ToolOutput {
            outcome: Default::default(),
            content,
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LsArgs {
    /// Directory or file to list, relative to workspace root. Defaults to `.`.
    #[serde(default)]
    pub path: Option<String>,
    /// Include hidden entries (names starting with `.`).
    #[serde(default)]
    pub all: bool,
}

pub struct LsTool;

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List files and directories in the workspace. Prefer this over `bash(ls …)`."
    }
    fn input_schema(&self) -> Value {
        schema_for::<LsArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn idempotent(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: LsArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::Execution(format!("internal deserialize after validation: {e}"))
        })?;
        let requested = a.path.as_deref().unwrap_or(".");
        let path = ctx.resolve_path(requested)?;
        let metadata = tokio::fs::metadata(&path).await?;
        let content = if metadata.is_dir() {
            let mut names = Vec::new();
            let mut entries = tokio::fs::read_dir(&path).await?;
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !a.all && name.starts_with('.') {
                    continue;
                }
                let suffix = if entry.file_type().await?.is_dir() {
                    "/"
                } else {
                    ""
                };
                names.push(format!("{name}{suffix}"));
            }
            names.sort();
            names.join("\n")
        } else {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| requested.to_string())
        };
        Ok(ToolOutput {
            outcome: Default::default(),
            content,
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

pub struct WriteFileTool;

/// Unified diff between `old` and `new` for `path`, in the same shape
/// `git diff` emits (header + `---`/`+++` + `@@` hunks). In-process via
/// `similar` — a pure-Rust Myers diff — replacing the old `git diff --no-index`
/// subprocess that wrote two temp files and spawned a process per call.
pub(crate) fn unified_diff(path: &str, old: Option<&str>, new: &str) -> Result<String, ToolError> {
    let old = old.unwrap_or("");
    if old == new {
        return Ok(String::new());
    }
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = format!("diff --git a/{path} b/{path}\n");
    let mut unified = diff.unified_diff();
    unified.context_radius(3);
    unified.header(&format!("a/{path}"), &format!("b/{path}"));
    out.push_str(&unified.to_string());
    Ok(out)
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
        let path = ctx.resolve_write_path(&a.path)?;
        let old = tokio::fs::read_to_string(&path).await.ok();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, a.content.as_bytes()).await?;
        let diff = unified_diff(&a.path, old.as_deref(), &a.content)?;
        let content = if diff.trim().is_empty() {
            format!("wrote {} bytes to {}", a.content.len(), a.path)
        } else {
            diff
        };
        Ok(ToolOutput {
            outcome: Default::default(),
            content,
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BashArgs {
    pub command: String,
}

pub struct BashTool;

/// Provider credential environment variables, stripped from any child process
/// the shell tool starts.
///
/// These belong to Forge and to the user, not to a model-authored command. A
/// command has no reason to read a provider key, and `$ANTHROPIC_API_KEY` in a
/// `curl` is the shortest exfiltration path there is.
///
/// Mirrors the `api_key_env` names on `forge_connect`'s built-in profiles plus
/// the tokens exported for OAuth providers. `forge-tools` does not depend on
/// `forge-connect`, so `credential_env_names_cover_every_connect_profile` in
/// `forge-cli` — which sees both crates — asserts this list stays complete.
pub const PROVIDER_CREDENTIAL_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OLLAMA_API_KEY",
    "OPENCODE_API_KEY",
    "OPENCODE_GO_API_KEY",
    "OPENCODE_ZEN_API_KEY",
    "XAI_API_KEY",
    "FORGE_CODEX_ACCESS_TOKEN",
    "FORGE_CODEX_ACCOUNT_ID",
];

pub async fn run_shell_command(
    command: &str,
    workspace_root: &Path,
) -> Result<ToolOutput, ToolError> {
    run_shell_command_with_egress(command, workspace_root, None).await
}

/// As [`run_shell_command`], with an explicit network grant.
///
/// `None` denies the network, which is the default. Callers that hold a
/// session's [`crate::sandbox::EgressGrant`] pass it here so the confined
/// command can reach the allowlisted hosts and nothing else.
pub async fn run_shell_command_with_egress(
    command: &str,
    workspace_root: &Path,
    egress: Option<&crate::sandbox::EgressGrant>,
) -> Result<ToolOutput, ToolError> {
    run_shell_command_inner(command, workspace_root, egress, None, true).await
}

pub async fn run_shell_command_with_egress_and_temp(
    command: &str,
    workspace_root: &Path,
    egress: Option<&crate::sandbox::EgressGrant>,
    session_tmp: Option<&Path>,
) -> Result<ToolOutput, ToolError> {
    run_shell_command_inner(command, workspace_root, egress, session_tmp, true).await
}

async fn run_shell_command_inner(
    command: &str,
    workspace_root: &Path,
    egress: Option<&crate::sandbox::EgressGrant>,
    session_tmp: Option<&Path>,
    confined: bool,
) -> Result<ToolOutput, ToolError> {
    // Do not use a login shell: `bash -l` sources profile files, which can
    // re-export credentials after the explicit removals below.
    //
    // Confinement is applied here, at spawn, because that is the only moment
    // it can be: a process that starts unconfined stays unconfined for its
    // whole life. The supported CLI never starts when the host cannot confine.
    let mut policy =
        crate::sandbox::SandboxPolicy::for_workspace(workspace_root).with_egress(egress);
    if let Some(session_tmp) = session_tmp {
        policy = policy.with_session_tmp(session_tmp);
    }
    let wrapped = confined
        .then(|| crate::sandbox::wrap_shell_command("bash", command, &policy))
        .flatten();

    // `wrap_shell_command` returns `None` for two very different reasons.
    // "This host has no sandbox" is a launch-time refusal in the supported
    // CLI. "This host has a sandbox but this policy could not be built" is
    // an anomaly — a workspace path that is not valid UTF-8, a root that
    // cannot be canonicalised — and nothing upstream knows it happened, so
    // falling back would drop confinement with no prompt and no message.
    // Refuse instead.
    if confined && wrapped.is_none() && crate::sandbox::availability().is_ok() {
        return Err(ToolError::Execution(format!(
            "refusing to run unconfined: this host can sandbox, but no sandbox \
             could be built for workspace {}",
            workspace_root.display()
        )));
    }

    let confined_run = wrapped.is_some();
    let mut shell = match wrapped {
        Some((program, args)) => {
            let mut confined = Command::new(program);
            confined.args(args);
            confined
        }
        None => {
            let mut plain = Command::new("bash");
            plain.arg("-c").arg(command);
            plain
        }
    };
    shell
        .current_dir(workspace_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in PROVIDER_CREDENTIAL_ENV {
        shell.env_remove(name);
    }
    for (name, value) in crate::sandbox::egress_env(&policy) {
        shell.env(name, value);
    }
    for (name, value) in crate::sandbox::temp_env(&policy) {
        shell.env(name, value);
    }

    let mut child = shell
        .spawn()
        .map_err(|e| ToolError::Execution(e.to_string()))?;
    let (status, stdout, stderr) = collect_bounded_output(&mut child).await?;
    let mut content = String::from_utf8_lossy(&stdout).into_owned();
    let err = String::from_utf8_lossy(&stderr);
    if !err.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&err);
    }

    // A denial does not announce itself: blocked egress arrives as "Could not
    // resolve host", which reads as a DNS outage, and a blocked write arrives
    // as "Operation not permitted", which reads as a file-permission problem
    // on disk. Neither is true, and a model that believes them retries or
    // chases the wrong fix. Say which boundary stopped it.
    if confined_run && !status.success() {
        if let Some(explanation) = crate::sandbox::explain_denial(&content, workspace_root) {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(explanation);
            return Err(ToolError::SandboxDenied {
                content,
                reason: explanation.to_string(),
            });
        }
    }

    let is_error = !status.success();
    let exit_code = status.code();
    let outcome = if !is_error {
        ExecutionOutcome::Success
    } else if exit_code == Some(127) {
        ExecutionOutcome::SpawnFailed {
            reason: "command not found".into(),
        }
    } else {
        ExecutionOutcome::Failed { exit_code }
    };

    Ok(ToolOutput {
        outcome: Some(outcome),
        content,
        is_error,
        exit_code,
        attachments: Vec::new(),
    })
}

const MAX_CAPTURED_COMMAND_BYTES: usize = 512 * 1024;

async fn read_bounded<R: AsyncRead + Unpin>(mut reader: R) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(output);
        }
        if output.len() < MAX_CAPTURED_COMMAND_BYTES {
            let remaining = MAX_CAPTURED_COMMAND_BYTES - output.len();
            output.extend_from_slice(&buffer[..count.min(remaining)]);
        }
    }
}

async fn collect_bounded_output(
    child: &mut tokio::process::Child,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), ToolError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::Execution("missing command stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::Execution("missing command stderr".into()))?;
    let stdout_task = tokio::spawn(read_bounded(stdout));
    let stderr_task = tokio::spawn(read_bounded(stderr));
    let status = child.wait().await?;
    let stdout = stdout_task
        .await
        .map_err(|e| ToolError::Execution(e.to_string()))??;
    let stderr = stderr_task
        .await
        .map_err(|e| ToolError::Execution(e.to_string()))??;
    Ok((status, stdout, stderr))
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Run a shell command in the workspace directory. \
Do not use this for listing, file search, content search, file reads, or git. \
Use `ls`, `glob`, `grep`, `read_file`, or `git` instead."
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
        run_shell_command_inner(
            &a.command,
            &ctx.workspace_root,
            ctx.egress.as_deref(),
            ctx.session_tmp.as_deref().map(|temp| temp.path()),
            !ctx.unconfined_shell,
        )
        .await
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BackgroundRunArgs {
    /// Shell command to run in the background, without blocking this turn.
    pub command: String,
    /// Short human-readable label shown in the Tasks panel (e.g. "cargo test").
    #[serde(default)]
    pub label: Option<String>,
}

/// Starts a shell command running in the background instead of executing it
/// inline. The agent loop (`forge-core`'s `run_one_tool`) special-cases this
/// tool name and intercepts the call *before* it ever reaches
/// `ToolRegistry::call` — routing it to `AgentSession::spawn_background_shell`
/// instead, so this `Tool::call` impl is a defensive fallback that should
/// never actually run in production; it exists so the tool has a normal,
/// schema-validated, governance-gated identity like any other tool.
pub struct BackgroundRunTool;

#[async_trait]
impl Tool for BackgroundRunTool {
    fn name(&self) -> &str {
        "background_run"
    }
    fn description(&self) -> &str {
        "Run a shell command in the background (e.g. compile, test, index) without blocking this turn. Reports back when finished."
    }
    fn input_schema(&self) -> Value {
        schema_for::<BackgroundRunArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Exec
    }

    async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Execution(
            "background_run must be intercepted by the agent loop, not executed directly".into(),
        ))
    }
}

/// Model-callable TODO/checklist tool. Parses args and returns a static ack;
/// the agent loop emits a `plan_update` turn event so the TUI can render it.
/// There is no server-side plan state — each call fully replaces what clients show.
pub struct UpdatePlanTool;

#[async_trait]
impl Tool for UpdatePlanTool {
    fn name(&self) -> &str {
        "update_plan"
    }

    fn description(&self) -> &str {
        "Updates the task plan. \
Provide an optional explanation and a list of plan items, each with a step and status \
(pending, in_progress, or completed). \
At most one step can be in_progress at a time. \
Use this as a structured checklist the user can see — not as a substitute for doing the work."
    }

    fn input_schema(&self) -> Value {
        schema_for::<forge_types::UpdatePlanArgs>()
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Meta
    }

    fn idempotent(&self) -> bool {
        true
    }

    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        // Validation already ran against the schema; deserialize is belt-and-suspenders.
        let _: forge_types::UpdatePlanArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::Execution(format!("internal deserialize after validation: {e}"))
        })?;
        Ok(ToolOutput {
            outcome: Default::default(),
            content: "Plan updated".into(),
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

/// Allowlisted git subcommands (not a free-form shell).
///
/// Every name here must have a [`git_policy`] entry; `git_policy_covers_every_subcommand`
/// enforces that.
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

/// Options refused for every subcommand, whatever the per-subcommand policy
/// says. Matched by name, so `--opt=value`, `--opt value` and `-Xvalue` are all
/// covered.
///
/// The second group is why an allowlist of *subcommands* is not sufficient on
/// its own: git passes each of these values to a shell, so any allowlisted
/// subcommand that accepts one is arbitrary command execution. `--output`/`-o`
/// writes to a caller-chosen file instead of going through the pathspec
/// machinery.
const GIT_DENIED_OPTIONS: &[&str] = &[
    // Redirect which repository or worktree is operated on. Inert after a
    // subcommand, but there is no reason for a tool call to carry them.
    "--git-dir",
    "--work-tree",
    "-C",
    "--config-env",
    // Executed by git via a shell.
    "--upload-pack",
    "--receive-pack",
    "--exec",
    "--upload-archive",
    // Writes to an arbitrary path.
    "--output",
    "-o",
];

/// How a subcommand's non-option operands are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitOperands {
    /// Refs and pathspecs. Git resolves these itself and refuses a pathspec
    /// that leaves the repository, so they are passed through.
    Free,
    /// `git clone <source> [<directory>]`. The source is only read and may be a
    /// URL, so it is not confined. The directory is created, so it is.
    CloneSourceThenDirectory,
    /// `git init [<directory>]` — the directory is created, so it is confined.
    OptionalDirectory,
}

/// Options permitted for one subcommand.
///
/// An option may appear in both lists when it is usable either way, such as
/// `--porcelain` and `--porcelain=v2`; the bare form then consumes no value.
struct GitPolicy {
    /// Options taking no value.
    flags: &'static [&'static str],
    /// Options taking a value as `--opt=value`, `--opt value`, or `-Xvalue`.
    valued: &'static [&'static str],
    operands: GitOperands,
}

impl GitPolicy {
    fn permits(&self, name: &str) -> bool {
        self.flags.contains(&name) || self.valued.contains(&name)
    }

    /// True when a bare `--opt` consumes the following token. An option that is
    /// also a flag does not.
    fn consumes_following_token(&self, name: &str) -> bool {
        self.valued.contains(&name) && !self.flags.contains(&name)
    }
}

/// Per-subcommand option policy.
///
/// Deliberately absent, because the allowlist is positive and silence is a
/// refusal:
///
/// - `--template` on `init`/`clone`, which copies hook scripts out of a
///   caller-chosen directory — the same class of hazard as `--exec`.
/// - `--separate-git-dir`, which points `.git` outside the workspace.
/// - Interactive options such as `-i`, `--interactive` and `-e`, which would
///   wait on a terminal that is not attached and hang the turn.
/// - `--recurse-submodules`, which fetches and checks out submodule content.
fn git_policy(subcommand: &str) -> Option<GitPolicy> {
    let policy = match subcommand {
        "status" => GitPolicy {
            flags: &[
                "-s",
                "--short",
                "--long",
                "-b",
                "--branch",
                "--show-stash",
                "--porcelain",
                "-v",
                "--verbose",
                "--ignored",
                "-z",
                "--no-renames",
                "--ahead-behind",
                "--no-ahead-behind",
            ],
            valued: &["-u", "--untracked-files", "--porcelain"],
            operands: GitOperands::Free,
        },
        "diff" => GitPolicy {
            flags: &[
                "--cached",
                "--staged",
                "-p",
                "--patch",
                "--no-patch",
                "-s",
                "--stat",
                "--compact-summary",
                "--numstat",
                "--shortstat",
                "--name-only",
                "--name-status",
                "--summary",
                "--raw",
                "--no-color",
                "--color",
                "-w",
                "--ignore-all-space",
                "-b",
                "--ignore-space-change",
                "--ignore-blank-lines",
                "-M",
                "--find-renames",
                "--no-renames",
                "-R",
                "--text",
                "--binary",
                "--exit-code",
                "--quiet",
                "--no-index",
            ],
            valued: &[
                "-U",
                "--unified",
                "--diff-filter",
                "--stat",
                "-M",
                "--find-renames",
                "--find-copies",
            ],
            operands: GitOperands::Free,
        },
        "log" => GitPolicy {
            flags: &[
                "--oneline",
                "--graph",
                "--decorate",
                "--no-decorate",
                "--stat",
                "--numstat",
                "--shortstat",
                "--name-only",
                "--name-status",
                "--all",
                "--no-merges",
                "--merges",
                "--first-parent",
                "--reverse",
                "-p",
                "--patch",
                "--no-patch",
                "--no-color",
                "--color",
                "--abbrev-commit",
                "--no-abbrev-commit",
                "--follow",
                "--topo-order",
                "--date-order",
            ],
            valued: &[
                "-n",
                "--max-count",
                "--skip",
                "--since",
                "--after",
                "--until",
                "--before",
                "--author",
                "--committer",
                "--grep",
                "--format",
                "--pretty",
                "--date",
                "--abbrev",
                "-L",
                "-S",
                "-G",
                "--diff-filter",
                "--stat",
            ],
            operands: GitOperands::Free,
        },
        "show" => GitPolicy {
            flags: &[
                "--stat",
                "--numstat",
                "--name-only",
                "--name-status",
                "--no-color",
                "--color",
                "--oneline",
                "-p",
                "--patch",
                "--no-patch",
                "-s",
                "--abbrev-commit",
            ],
            valued: &[
                "--format",
                "--pretty",
                "--date",
                "-U",
                "--unified",
                "--stat",
            ],
            operands: GitOperands::Free,
        },
        "branch" => GitPolicy {
            flags: &[
                "-a",
                "--all",
                "-r",
                "--remotes",
                "-v",
                "--verbose",
                "-l",
                "--list",
                "--show-current",
                "--merged",
                "--no-merged",
                "-d",
                "--delete",
                "-D",
                "-f",
                "--force",
                "-q",
                "--quiet",
            ],
            valued: &[
                "--contains",
                "--no-contains",
                "--sort",
                "--format",
                "-u",
                "--set-upstream-to",
                "--points-at",
                "--merged",
                "--no-merged",
            ],
            operands: GitOperands::Free,
        },
        "add" => GitPolicy {
            flags: &[
                "-A",
                "--all",
                "-u",
                "--update",
                "-f",
                "--force",
                "-n",
                "--dry-run",
                "-v",
                "--verbose",
                "--ignore-errors",
                "--renormalize",
                "-N",
                "--intent-to-add",
                "--ignore-removal",
            ],
            valued: &[],
            operands: GitOperands::Free,
        },
        "commit" => GitPolicy {
            flags: &[
                "-a",
                "--all",
                "--amend",
                "--no-edit",
                "-n",
                "--no-verify",
                "--allow-empty",
                "--allow-empty-message",
                "-s",
                "--signoff",
                "-v",
                "--verbose",
                "-q",
                "--quiet",
                "--no-post-rewrite",
            ],
            valued: &["-m", "--message", "--author", "--date"],
            operands: GitOperands::Free,
        },
        "checkout" => GitPolicy {
            flags: &[
                "-f",
                "--force",
                "--detach",
                "-q",
                "--quiet",
                "--ours",
                "--theirs",
                "-m",
                "--merge",
                "--no-track",
                "--track",
                "-t",
                "--",
            ],
            valued: &["-b", "-B", "--orphan"],
            operands: GitOperands::Free,
        },
        "switch" => GitPolicy {
            flags: &[
                "-f",
                "--force",
                "--detach",
                "-q",
                "--quiet",
                "--discard-changes",
                "--no-track",
                "--track",
                "-t",
                "-m",
                "--merge",
            ],
            // `-c` here is switch's "create branch". Top-level `-c` injects
            // configuration, but arguments are always placed after the
            // subcommand, so git parses this one as switch's own option.
            valued: &["-c", "--orphan"],
            operands: GitOperands::Free,
        },
        "restore" => GitPolicy {
            flags: &[
                "-S",
                "--staged",
                "-W",
                "--worktree",
                "-q",
                "--quiet",
                "--ours",
                "--theirs",
                "-m",
                "--merge",
                "--overlay",
                "--no-overlay",
            ],
            valued: &["-s", "--source"],
            operands: GitOperands::Free,
        },
        "stash" => GitPolicy {
            flags: &[
                "-u",
                "--include-untracked",
                "-k",
                "--keep-index",
                "--no-keep-index",
                "-q",
                "--quiet",
                "--staged",
                "-a",
                "--all",
            ],
            valued: &["-m", "--message"],
            operands: GitOperands::Free,
        },
        "rev-parse" => GitPolicy {
            flags: &[
                "--short",
                "--abbrev-ref",
                "--verify",
                "-q",
                "--quiet",
                "--show-toplevel",
                "--is-inside-work-tree",
                "--show-cdup",
                "--all",
                "--symbolic",
                "--symbolic-full-name",
            ],
            valued: &["--short", "--abbrev-ref"],
            operands: GitOperands::Free,
        },
        "ls-files" => GitPolicy {
            flags: &[
                "--cached",
                "--deleted",
                "--modified",
                "--others",
                "--ignored",
                "--stage",
                "--unmerged",
                "--killed",
                "-z",
                "--exclude-standard",
                "--full-name",
                "--error-unmatch",
            ],
            valued: &["-x", "--exclude"],
            operands: GitOperands::Free,
        },
        "remote" => GitPolicy {
            flags: &["-v", "--verbose"],
            valued: &[],
            operands: GitOperands::Free,
        },
        "fetch" => GitPolicy {
            flags: &[
                "--all",
                "--tags",
                "--no-tags",
                "--prune",
                "-p",
                "--prune-tags",
                "-f",
                "--force",
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--dry-run",
                "-n",
                "--no-recurse-submodules",
            ],
            valued: &["--depth", "--deepen", "--shallow-since", "-j", "--jobs"],
            operands: GitOperands::Free,
        },
        "pull" => GitPolicy {
            flags: &[
                "--rebase",
                "--no-rebase",
                "--ff",
                "--ff-only",
                "--no-ff",
                "--autostash",
                "--no-autostash",
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--tags",
                "--no-tags",
                "--prune",
                "--no-recurse-submodules",
            ],
            valued: &[
                "--depth",
                "-j",
                "--jobs",
                "-s",
                "--strategy",
                "-X",
                "--strategy-option",
            ],
            operands: GitOperands::Free,
        },
        "push" => GitPolicy {
            flags: &[
                "--all",
                "--tags",
                "--follow-tags",
                "-f",
                "--force",
                "--force-with-lease",
                "--no-verify",
                "-u",
                "--set-upstream",
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "-n",
                "--dry-run",
                "--delete",
                "-d",
                "--prune",
                "--atomic",
            ],
            valued: &["--force-with-lease"],
            operands: GitOperands::Free,
        },
        "merge" => GitPolicy {
            flags: &[
                "--no-ff",
                "--ff",
                "--ff-only",
                "--squash",
                "--no-commit",
                "--commit",
                "--abort",
                "--continue",
                "--quit",
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--no-edit",
                "--no-verify",
                "--allow-unrelated-histories",
            ],
            valued: &[
                "-m",
                "--message",
                "-s",
                "--strategy",
                "-X",
                "--strategy-option",
            ],
            operands: GitOperands::Free,
        },
        "rebase" => GitPolicy {
            flags: &[
                "--continue",
                "--abort",
                "--skip",
                "--quit",
                "--autostash",
                "--no-autostash",
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "--no-verify",
                "--keep-empty",
                "--root",
                "--no-ff",
                "--ff",
            ],
            valued: &["--onto", "-s", "--strategy", "-X", "--strategy-option"],
            operands: GitOperands::Free,
        },
        "cherry-pick" => GitPolicy {
            flags: &[
                "--continue",
                "--abort",
                "--skip",
                "--quit",
                "-n",
                "--no-commit",
                "-x",
                "--ff",
                "--allow-empty",
                "--allow-empty-message",
                "-s",
                "--signoff",
                "--no-verify",
            ],
            valued: &["-m", "--mainline", "--strategy", "-X", "--strategy-option"],
            operands: GitOperands::Free,
        },
        "tag" => GitPolicy {
            flags: &[
                "-l",
                "--list",
                "-d",
                "--delete",
                "-f",
                "--force",
                "-a",
                "--annotate",
                "--merged",
                "--no-merged",
            ],
            valued: &[
                "-m",
                "--message",
                "-n",
                "--contains",
                "--no-contains",
                "--points-at",
                "--sort",
                "--format",
                "--merged",
                "--no-merged",
            ],
            operands: GitOperands::Free,
        },
        "blame" => GitPolicy {
            flags: &[
                "-l",
                "-t",
                "-s",
                "-f",
                "--show-name",
                "-n",
                "--show-number",
                "-w",
                "-e",
                "--show-email",
                "--line-porcelain",
                "--porcelain",
                "--root",
            ],
            valued: &["-L", "--since", "--abbrev"],
            operands: GitOperands::Free,
        },
        "init" => GitPolicy {
            flags: &["--bare", "-q", "--quiet"],
            valued: &["-b", "--initial-branch"],
            operands: GitOperands::OptionalDirectory,
        },
        "clone" => GitPolicy {
            flags: &[
                "--bare",
                "-q",
                "--quiet",
                "-v",
                "--verbose",
                "-n",
                "--no-checkout",
                "--no-tags",
                "--single-branch",
                "--no-single-branch",
                "--no-recurse-submodules",
            ],
            valued: &["--depth", "-b", "--branch", "--origin", "-j", "--jobs"],
            operands: GitOperands::CloneSourceThenDirectory,
        },
        _ => return None,
    };
    Some(policy)
}

/// Subcommands where a bare `-<n>` is git's shorthand for `--max-count=<n>`.
/// `git log -1` is idiomatic enough that refusing it would make the tool feel
/// broken, and the token carries no value beyond the number itself.
const GIT_NUMERIC_COUNT_SUBCOMMANDS: &[&str] = &["log", "show"];

fn is_git_numeric_count(token: &str) -> bool {
    match token.strip_prefix('-') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// The option name a token carries, or `None` when the token is an operand.
///
/// `--opt=value` yields `--opt`. A single-dash token yields its first two
/// characters, so `-U3` yields `-U`.
fn git_option_name(token: &str) -> Option<&str> {
    if token == "-" || token == "--" || !token.starts_with('-') {
        return None;
    }
    if let Some(long) = token.strip_prefix("--") {
        let name_len = long.find('=').map(|i| i + 2).unwrap_or(token.len());
        return Some(&token[..name_len]);
    }
    Some(&token[..2])
}

/// Validate the arguments for an allowlisted subcommand.
///
/// Replaces an earlier deny-list of four option prefixes. That approach could
/// not work: it enumerated what to refuse, so anything not thought of was
/// permitted, and the options that actually matter — the ones git hands to a
/// shell — were not among them.
fn validate_git_args(
    ctx: &ToolContext,
    subcommand: &str,
    args: &[String],
) -> Result<(), ToolError> {
    let policy = git_policy(subcommand).ok_or_else(|| {
        ToolError::Execution(format!(
            "git: no argument policy for subcommand `{subcommand}`"
        ))
    })?;

    let mut operands: Vec<&str> = Vec::new();
    let mut index = 0;
    let mut options_ended = false;

    while index < args.len() {
        let token = args[index].as_str();
        if options_ended {
            operands.push(token);
            index += 1;
            continue;
        }
        if token == "--" {
            options_ended = true;
            index += 1;
            continue;
        }

        if is_git_numeric_count(token) && GIT_NUMERIC_COUNT_SUBCOMMANDS.contains(&subcommand) {
            index += 1;
            continue;
        }

        let Some(name) = git_option_name(token) else {
            operands.push(token);
            index += 1;
            continue;
        };

        if GIT_DENIED_OPTIONS.contains(&name) {
            return Err(ToolError::Execution(format!(
                "git: option `{name}` is never allowed"
            )));
        }
        if !policy.permits(name) {
            return Err(ToolError::Execution(format!(
                "git: option `{name}` is not allowed for `{subcommand}`"
            )));
        }

        // A single-dash token longer than two characters is either a valued
        // option carrying its value (`-U3`) or a cluster (`-am`). Clusters are
        // refused rather than parsed, so that the trailing letters cannot be a
        // second option that slipped past the checks above.
        let long = token.starts_with("--");
        if !long && token.len() > 2 {
            if !policy.valued.contains(&name) {
                return Err(ToolError::Execution(format!(
                    "git: `{token}` bundles short options; pass them separately"
                )));
            }
            index += 1;
            continue;
        }

        let has_inline_value = long && token.contains('=');
        if !has_inline_value && policy.consumes_following_token(name) {
            index += 1;
            if index >= args.len() {
                return Err(ToolError::Execution(format!(
                    "git: option `{name}` requires a value"
                )));
            }
        }
        index += 1;
    }

    match policy.operands {
        GitOperands::Free => {}
        // The directory git creates is confined; the source it reads is not,
        // because it is commonly a URL.
        GitOperands::CloneSourceThenDirectory => {
            if operands.len() > 2 {
                return Err(ToolError::Execution(
                    "git: clone takes at most a source and a directory".into(),
                ));
            }
            if let Some(directory) = operands.get(1) {
                ctx.resolve_write_path(directory)?;
            }
        }
        GitOperands::OptionalDirectory => {
            if operands.len() > 1 {
                return Err(ToolError::Execution(format!(
                    "git: {subcommand} takes at most one directory"
                )));
            }
            if let Some(directory) = operands.first() {
                ctx.resolve_write_path(directory)?;
            }
        }
    }

    Ok(())
}

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
        "Run an allowlisted git subcommand in the workspace (status, diff, log, add, commit, branch, push, …). \
Not a free-form shell. Prefer this over `bash(git …)`."
    }
    fn input_schema(&self) -> Value {
        schema_for::<GitArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
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
        validate_git_args(ctx, &sub, &a.args)?;

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
            outcome: Default::default(),
            content,
            is_error: !out.status.success(),
            exit_code: out.status.code(),
            attachments: Vec::new(),
        })
    }
}

/// Phase 1 workspace tools only (no web_search). Prefer
/// [`default_builtins_with_web_search`] when config is available.
pub fn default_builtins() -> Vec<std::sync::Arc<dyn Tool>> {
    let (exec_command, write_stdin) = crate::unified_exec_tools();
    let mut tools: Vec<std::sync::Arc<dyn Tool>> = vec![
        std::sync::Arc::new(ReadFileTool),
        std::sync::Arc::new(LsTool),
        std::sync::Arc::new(crate::ViewImageTool),
        std::sync::Arc::new(WriteFileTool),
        std::sync::Arc::new(crate::ApplyPatchTool),
        std::sync::Arc::new(BashTool),
        std::sync::Arc::new(GitTool),
        std::sync::Arc::new(BackgroundRunTool),
        std::sync::Arc::new(exec_command),
        std::sync::Arc::new(write_stdin),
        std::sync::Arc::new(UpdatePlanTool),
        std::sync::Arc::new(crate::skills::LoadSkillTool),
        crate::web_fetch::web_fetch_tool(),
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

    #[tokio::test]
    async fn bash_tool_uses_private_session_temp() {
        let workspace = tempfile::tempdir().unwrap();
        let session_tmp = crate::SessionTempDir::create("bash-temp-test").unwrap();
        let ctx = ToolContext::new(workspace.path().to_path_buf())
            .with_session_tmp(session_tmp.clone())
            .with_unconfined_shell();
        let output = BashTool
            .call(
                &ctx,
                serde_json::json!({
                    "command": "printf '%s\\n%s\\n%s' \"$TMPDIR\" \"$TMP\" \"$TEMP\"; touch \"$TMPDIR/probe\""
                }),
            )
            .await
            .unwrap();

        let expected = session_tmp.path().to_string_lossy();
        assert_eq!(
            output.content,
            format!("{expected}\n{expected}\n{expected}")
        );
        assert!(session_tmp.path().join("probe").exists());
        assert!(!workspace.path().join("probe").exists());
    }

    #[test]
    fn read_schema_rejects_number_path() {
        let t = ReadFileTool;
        let err = validate_args("read_file", &t.input_schema(), &json!({"path": 1})).unwrap_err();
        assert_eq!(err.tool, "read_file");
    }

    #[test]
    fn read_file_describes_itself() {
        let t = ReadFileTool;
        assert_eq!(t.name(), "read_file");
        assert_eq!(
            t.description(),
            "Read a text file from the workspace or session temp"
        );
        assert_eq!(t.side_effect_class(), SideEffectClass::Read);
        assert!(t.idempotent());
    }

    #[tokio::test]
    async fn ls_lists_workspace_entries_and_hides_dotfiles_by_default() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("README.md"), "hi").unwrap();
        std::fs::write(dir.path().join(".hidden"), "secret").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = LsTool.call(&ctx, json!({})).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("README.md"), "{}", out.content);
        assert!(out.content.contains("src/"), "{}", out.content);
        assert!(!out.content.contains(".hidden"), "{}", out.content);
        let all = LsTool.call(&ctx, json!({"all": true})).await.unwrap();
        assert!(all.content.contains(".hidden"), "{}", all.content);
    }

    #[test]
    fn ls_describes_itself() {
        let t = LsTool;
        assert_eq!(t.name(), "ls");
        assert_eq!(t.side_effect_class(), SideEffectClass::Read);
        assert!(t.idempotent());
    }

    #[tokio::test]
    async fn read_file_reports_internal_deserialize_failure() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let error = ReadFileTool
            .call(&ctx, json!({"path": 5}))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("internal deserialize"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn read_file_refuses_unbounded_read_of_huge_file() {
        let dir = tempdir().unwrap();
        let huge = dir.path().join("huge.txt");
        {
            let file = std::fs::File::create(&huge).unwrap();
            file.set_len(2 * 1024 * 1024 + 1).unwrap();
        }
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = ReadFileTool
            .call(&ctx, json!({"path": "huge.txt"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("offset"), "{}", out.content);
    }

    #[tokio::test]
    async fn read_file_offset_past_end_of_file_returns_empty() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("short.txt"), "one\ntwo\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = ReadFileTool
            .call(&ctx, json!({"path": "short.txt", "offset": 50}))
            .await
            .unwrap();
        assert_eq!(out.content, "");
    }

    #[tokio::test]
    async fn read_file_rejects_image_and_names_view_image() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), forge_types::sample_png_bytes()).unwrap();
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.image_input = true;
        let out = ReadFileTool
            .call(&ctx, json!({"path": "shot.png"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("view_image"), "{}", out.content);
    }

    #[tokio::test]
    async fn read_file_rejects_image_when_vision_unavailable() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), forge_types::sample_png_bytes()).unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = ReadFileTool
            .call(&ctx, json!({"path": "shot.png"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            out.content.contains("does not support image inputs"),
            "{}",
            out.content
        );
    }

    #[test]
    fn default_builtins_includes_view_image() {
        let names: Vec<_> = default_builtins()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        assert!(names.contains(&"view_image".to_string()), "{names:?}");
    }

    #[test]
    fn write_file_describes_itself() {
        let t = WriteFileTool;
        assert_eq!(t.name(), "write_file");
        assert_eq!(
            t.description(),
            "Write a text file in the workspace (creates parent dirs)"
        );
        assert_eq!(t.side_effect_class(), SideEffectClass::Write);
    }

    #[tokio::test]
    async fn write_file_reports_deserialize_failure() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let error = WriteFileTool
            .call(&ctx, json!({"path": "a.txt", "content": 5}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid type"), "{error}");
    }

    #[test]
    fn bash_describes_itself() {
        let t = BashTool;
        assert_eq!(t.name(), "bash");
        assert_eq!(
            t.description(),
            "Run a shell command in the workspace directory. \
Do not use this for listing, file search, content search, file reads, or git. \
Use `ls`, `glob`, `grep`, `read_file`, or `git` instead."
        );
        assert_eq!(t.side_effect_class(), SideEffectClass::Exec);
    }

    #[tokio::test]
    async fn bash_reports_deserialize_failure() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let error = BashTool
            .call(&ctx, json!({"command": 12345}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid type"), "{error}");
    }

    #[tokio::test]
    async fn approved_bash_context_runs_the_exact_command_unconfined() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let target = outside.path().join("approved.txt");
        let ctx = ToolContext::new(workspace.path().to_path_buf()).with_unconfined_shell();

        let output = BashTool
            .call(
                &ctx,
                json!({"command": format!("printf approved > {}", target.display())}),
            )
            .await
            .unwrap();

        assert!(!output.is_error, "{}", output.content);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "approved");
    }

    /// Restores an environment variable on drop, so a test that has to touch the
    /// process environment does not leak into the rest of the suite.
    struct EnvVarGuard {
        name: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    /// A model-authored command must not be able to read a provider credential
    /// out of its environment. `OPENCODE_ZEN_API_KEY` is used because nothing
    /// else in the workspace reads it, so setting it here cannot perturb another
    /// test.
    #[tokio::test]
    async fn bash_does_not_inherit_provider_credentials() {
        let _guard = EnvVarGuard::set("OPENCODE_ZEN_API_KEY", "sk-must-not-reach-the-child");
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        let out = BashTool
            .call(
                &ctx,
                json!({"command": "printf '[%s]' \"$OPENCODE_ZEN_API_KEY\""}),
            )
            .await
            .unwrap();

        assert_eq!(
            out.content.trim(),
            "[]",
            "credential reached the child: {}",
            out.content
        );
    }

    /// This strips credentials, not the environment. An unrelated variable must
    /// still reach the command, or ordinary builds break.
    #[tokio::test]
    async fn bash_still_inherits_unrelated_environment() {
        let _guard = EnvVarGuard::set("FORGE_TEST_BASH_PASSTHROUGH", "visible");
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        let out = BashTool
            .call(
                &ctx,
                json!({"command": "printf '[%s]' \"$FORGE_TEST_BASH_PASSTHROUGH\""}),
            )
            .await
            .unwrap();

        assert_eq!(out.content.trim(), "[visible]", "{}", out.content);
    }

    #[test]
    fn provider_credential_env_list_is_well_formed() {
        for name in PROVIDER_CREDENTIAL_ENV {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "`{name}` is not an environment variable name"
            );
        }
        let mut sorted = PROVIDER_CREDENTIAL_ENV.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "duplicate entry in the list");
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

    /// Git executes what it finds in its own config, so `write_file` must not be
    /// able to reach it even though `.git` sits inside the workspace.
    #[tokio::test]
    async fn write_file_refuses_git_directory() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let config = dir.path().join(".git/config");
        std::fs::write(&config, "[core]\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        let error = WriteFileTool
            .call(
                &ctx,
                json!({"path": ".git/config", "content": "[diff]\n\texternal = payload\n"}),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains(".git"));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "[core]\n");
    }

    /// A write target that does not exist yet used to skip containment entirely
    /// when it was absolute, so nothing stopped it being created outside.
    #[tokio::test]
    async fn write_file_refuses_absent_target_outside_workspace() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let target = outside.path().join("created.txt");

        let error = WriteFileTool
            .call(
                &ctx,
                json!({"path": target.to_str().unwrap(), "content": "payload"}),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("escapes workspace"));
        assert!(!target.exists());
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
    fn default_builtins_with_web_search_omits_mock() {
        let cfg = forge_config::WebSearchConfig::default();
        let tools = default_builtins_with_web_search(&cfg);
        assert!(
            !tools.iter().any(|t| t.name() == "web_search"),
            "mock web_search must not be advertised"
        );
        assert!(tools.iter().any(|t| t.name() == "read_file"));
        assert!(tools.iter().any(|t| t.name() == "git"));
        assert_eq!(tools.len(), default_builtins().len());
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
        assert!(tools.iter().any(|t| t.name() == "edit"));
        assert!(
            !tools.iter().any(|t| t.name() == "search_replace"),
            "search_replace is a silent synonym for edit"
        );
        assert!(tools.iter().any(|t| t.name() == "glob"));
        assert!(tools.iter().any(|t| t.name() == "grep"));
        assert!(
            !tools.iter().any(|t| t.name() == "rg"),
            "rg is a silent synonym for grep, not a second advertised tool"
        );
        assert!(tools.iter().any(|t| t.name() == "update_plan"));
        assert!(tools.iter().any(|t| t.name() == "ls"));
    }

    #[tokio::test]
    async fn update_plan_returns_static_ack() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let args = json!({
            "explanation": "starting",
            "plan": [
                {"step": "read code", "status": "completed"},
                {"step": "implement", "status": "in_progress"},
                {"step": "test", "status": "pending"}
            ]
        });
        validate_args("update_plan", &UpdatePlanTool.input_schema(), &args).unwrap();
        let out = UpdatePlanTool.call(&ctx, args).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content, "Plan updated");
        assert_eq!(UpdatePlanTool.side_effect_class(), SideEffectClass::Meta);
    }

    #[test]
    fn update_plan_schema_rejects_unknown_status() {
        let err = validate_args(
            "update_plan",
            &UpdatePlanTool.input_schema(),
            &json!({
                "plan": [{"step": "x", "status": "blocked"}]
            }),
        )
        .unwrap_err();
        assert_eq!(err.tool, "update_plan");
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

    fn git_ctx() -> (tempfile::TempDir, ToolContext) {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        (dir, ctx)
    }

    fn reject(subcommand: &str, args: &[&str]) -> String {
        let (_dir, ctx) = git_ctx();
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        match validate_git_args(&ctx, subcommand, &owned) {
            Ok(()) => panic!("expected `git {subcommand} {args:?}` to be refused"),
            Err(error) => error.to_string(),
        }
    }

    fn accept(subcommand: &str, args: &[&str]) {
        let (_dir, ctx) = git_ctx();
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        if let Err(error) = validate_git_args(&ctx, subcommand, &owned) {
            panic!("expected `git {subcommand} {args:?}` to be allowed: {error}");
        }
    }

    /// Every option git hands to a shell, in both `--opt=value` and
    /// `--opt value` form. These are the reason a subcommand allowlist alone is
    /// not enough — each of these was previously accepted.
    #[test]
    fn git_refuses_shell_executing_options_in_both_forms() {
        let cases: &[(&str, &str)] = &[
            ("fetch", "--upload-pack"),
            ("pull", "--upload-pack"),
            ("clone", "--upload-pack"),
            ("push", "--receive-pack"),
            ("push", "--exec"),
            ("rebase", "--exec"),
            ("fetch", "--upload-archive"),
        ];
        for (subcommand, option) in cases {
            let inline = reject(subcommand, &[&format!("{option}=payload")]);
            assert!(
                inline.contains("never allowed"),
                "inline form of {option} on {subcommand}: {inline}"
            );
            let separate = reject(subcommand, &[option, "payload"]);
            assert!(
                separate.contains("never allowed"),
                "separate form of {option} on {subcommand}: {separate}"
            );
        }
    }

    #[test]
    fn git_refuses_output_redirection_and_repository_redirection() {
        for (subcommand, option) in [
            ("log", "--output"),
            ("log", "-o"),
            ("diff", "--output"),
            ("status", "--git-dir"),
            ("status", "--work-tree"),
            ("diff", "-C"),
            ("log", "--config-env"),
        ] {
            let message = reject(subcommand, &[option, "value"]);
            assert!(
                message.contains("never allowed"),
                "{option} on {subcommand}: {message}"
            );
        }
    }

    /// The allowlist is positive, so options nobody enumerated are refused —
    /// including two that are the same class of hazard as `--exec` but were not
    /// on anyone's deny-list.
    #[test]
    fn git_refuses_options_absent_from_the_allowlist() {
        for (subcommand, args) in [
            // Copies hook scripts out of a caller-chosen directory.
            ("init", vec!["--template=/tmp/hooks"]),
            ("clone", vec!["--template=/tmp/hooks", "src"]),
            // Points `.git` outside the workspace.
            ("clone", vec!["--separate-git-dir=/tmp/elsewhere", "src"]),
            // Would wait on a terminal that is not attached.
            ("rebase", vec!["-i"]),
            ("add", vec!["--interactive"]),
            ("commit", vec!["--edit"]),
            // Simply not a real option.
            ("status", vec!["--make-coffee"]),
        ] {
            let message = reject(subcommand, &args);
            assert!(
                message.contains("not allowed"),
                "{subcommand} {args:?}: {message}"
            );
        }
    }

    /// An option permitted for one subcommand is not permitted everywhere.
    #[test]
    fn git_option_policy_is_per_subcommand() {
        accept("checkout", &["-b", "feature"]);
        accept("switch", &["-c", "feature"]);
        accept("status", &["--porcelain"]);
        accept("status", &["--porcelain=v2"]);

        // `--porcelain` is fine for status, meaningless for commit.
        assert!(reject("commit", &["--porcelain"]).contains("not allowed"));
        // `-b` creates a branch for checkout; for status it is `--branch`, a
        // flag, so it must not swallow the next token as a value.
        accept("status", &["-b"]);
    }

    /// A value that happens to look like a refused option is a value, not an
    /// option. Getting this wrong would make ordinary commit messages fail.
    #[test]
    fn git_treats_option_values_as_values_not_options() {
        accept("commit", &["-m", "--upload-pack=not actually an option"]);
        accept("commit", &["--message=--exec=still just text"]);
        accept("log", &["--grep", "--receive-pack"]);
        accept("log", &["-n", "5"]);
        accept("log", &["-n5"]);
        accept("diff", &["-U3"]);
    }

    /// `git log -1` is idiomatic; the count shorthand is accepted where git
    /// supports it and refused elsewhere rather than allowed everywhere.
    #[test]
    fn git_accepts_numeric_count_shorthand_only_where_git_supports_it() {
        accept("log", &["-1", "--oneline"]);
        accept("log", &["-20"]);
        accept("show", &["-3"]);
        assert!(reject("status", &["-1"]).contains("not allowed"));
        assert!(reject("push", &["-1"]).contains("not allowed"));
    }

    #[test]
    fn git_refuses_bundled_short_options() {
        let message = reject("commit", &["-am", "msg"]);
        assert!(message.contains("separately"), "{message}");
    }

    #[test]
    fn git_requires_a_value_for_valued_options() {
        assert!(reject("commit", &["-m"]).contains("requires a value"));
    }

    #[test]
    fn git_treats_tokens_after_double_dash_as_operands() {
        // Without end-of-options handling this would be read as an option.
        accept("add", &["--", "--weird-filename"]);
        accept("log", &["--", "src/main.rs"]);
    }

    /// `git clone <src> <dir>` and `git init <dir>` create the directory, so the
    /// destination is confined. This escaped previously.
    #[test]
    fn git_confines_clone_and_init_destinations() {
        for (subcommand, args) in [
            ("clone", vec!["https://example.invalid/r.git", "../escaped"]),
            (
                "clone",
                vec!["https://example.invalid/r.git", "/tmp/escaped"],
            ),
            ("init", vec!["../escaped"]),
            ("init", vec!["/tmp/escaped"]),
        ] {
            let message = reject(subcommand, &args);
            assert!(
                message.contains("escapes workspace"),
                "{subcommand} {args:?}: {message}"
            );
        }

        // A destination inside the workspace still works, including when a
        // valued option precedes the operands — the parser must not mistake the
        // option's value for the source.
        accept("clone", &["https://example.invalid/r.git", "nested/copy"]);
        accept(
            "clone",
            &[
                "--depth",
                "1",
                "https://example.invalid/r.git",
                "nested/copy",
            ],
        );
        accept("init", &["nested/repo"]);
        accept("init", &[]);
    }

    #[test]
    fn git_rejects_extra_clone_and_init_operands() {
        assert!(reject("clone", &["a", "b", "c"]).contains("at most"));
        assert!(reject("init", &["a", "b"]).contains("at most"));
    }

    /// A positive allowlist fails closed, which means it can break ordinary use
    /// as easily as it blocks an attack. These are commands an agent actually
    /// issues; if a future tightening breaks one, that should be a decision
    /// rather than a surprise.
    #[test]
    fn git_allows_ordinary_workflows() {
        for (subcommand, args) in [
            ("status", vec!["--porcelain"]),
            ("status", vec!["-s", "-b"]),
            ("diff", vec!["--cached", "--stat"]),
            ("diff", vec!["--name-only", "HEAD~1"]),
            ("log", vec!["-5", "--oneline", "--graph"]),
            ("log", vec!["--pretty=format:%h %s", "-n", "10"]),
            ("show", vec!["HEAD", "--stat"]),
            ("add", vec!["-A"]),
            ("add", vec!["src/main.rs", "README.md"]),
            ("commit", vec!["-m", "Fix the thing"]),
            ("commit", vec!["--amend", "--no-edit"]),
            ("branch", vec!["--show-current"]),
            ("branch", vec!["-a"]),
            ("checkout", vec!["-b", "feature/x"]),
            ("checkout", vec!["main"]),
            ("switch", vec!["-c", "feature/y"]),
            ("restore", vec!["--staged", "src/main.rs"]),
            ("stash", vec!["push", "-m", "wip"]),
            ("stash", vec!["pop"]),
            ("rev-parse", vec!["--short", "HEAD"]),
            ("rev-parse", vec!["--abbrev-ref", "HEAD"]),
            ("ls-files", vec!["--modified"]),
            ("remote", vec!["-v"]),
            ("fetch", vec!["origin", "--prune"]),
            ("pull", vec!["--rebase"]),
            ("push", vec!["-u", "origin", "feature/x"]),
            ("push", vec!["--force-with-lease"]),
            ("merge", vec!["--no-ff", "feature/x"]),
            ("rebase", vec!["--onto", "main", "HEAD~2"]),
            ("rebase", vec!["--continue"]),
            ("cherry-pick", vec!["-x", "abc1234"]),
            ("tag", vec!["-a", "v1.0.0", "-m", "release"]),
            ("blame", vec!["-L", "10,20", "src/main.rs"]),
        ] {
            accept(subcommand, &args);
        }
    }

    /// The subcommand allowlist and the policy table must not drift apart.
    #[test]
    fn git_policy_covers_every_subcommand() {
        for subcommand in GIT_ALLOWED_SUBCOMMANDS {
            assert!(
                git_policy(subcommand).is_some(),
                "`{subcommand}` is allowlisted but has no argument policy"
            );
        }
    }

    /// A subcommand with no policy entry falls through to `None`, matching the
    /// intent that the allowlist is positive: anything not enumerated is
    /// refused rather than silently permitted.
    #[test]
    fn git_policy_is_none_for_an_unlisted_subcommand() {
        assert!(git_policy("daemon").is_none());
        assert!(git_policy("").is_none());
        assert!(!GIT_ALLOWED_SUBCOMMANDS.contains(&"daemon"));
    }

    /// `validate_git_args` is called directly here (bypassing `GitTool::call`'s
    /// allowlist gate, as `reject`/`accept` already do above) to exercise its
    /// own defence against an unlisted subcommand reaching the policy lookup.
    #[test]
    fn validate_git_args_reports_missing_policy_for_unlisted_subcommand() {
        let message = reject("daemon", &[]);
        assert!(
            message.contains("no argument policy for subcommand `daemon`"),
            "{message}"
        );
    }

    /// End to end: the refusal happens before git is invoked, and nothing is
    /// created on disk.
    #[tokio::test]
    async fn git_tool_refuses_upload_pack_before_running_git() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let marker = dir.path().join("PWNED");

        let error = GitTool
            .call(
                &ctx,
                json!({
                    "subcommand": "fetch",
                    "args": [format!("--upload-pack=touch {}; git-upload-pack", marker.display()), "."]
                }),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("never allowed"), "{error}");
        assert!(!marker.exists(), "payload must not have run");
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
        let t = crate::fast_file_tools::FffFindTool::new(
            std::sync::Arc::new(crate::fast_file_tools::FastFileState::new()),
            "glob",
        );
        let err =
            crate::validation::validate_args("glob", &t.input_schema(), &json!({})).unwrap_err();
        assert_eq!(err.tool, "glob");
    }

    #[test]
    fn fff_grep_schema_rejects_empty_args() {
        let state = std::sync::Arc::new(crate::fast_file_tools::FastFileState::new());
        let t = crate::fast_file_tools::FffGrepTool::new(state, "grep");
        let err =
            crate::validation::validate_args("grep", &t.input_schema(), &json!({})).unwrap_err();
        assert_eq!(err.tool, "grep");
    }

    #[test]
    fn fff_find_schema_accepts_query() {
        let t = crate::fast_file_tools::FffFindTool::new(
            std::sync::Arc::new(crate::fast_file_tools::FastFileState::new()),
            "glob",
        );
        crate::validation::validate_args("glob", &t.input_schema(), &json!({"pattern": "main.rs"}))
            .unwrap();
    }

    #[test]
    fn fff_grep_schema_accepts_pattern() {
        let state = std::sync::Arc::new(crate::fast_file_tools::FastFileState::new());
        let t = crate::fast_file_tools::FffGrepTool::new(state, "grep");
        crate::validation::validate_args("grep", &t.input_schema(), &json!({"pattern": "TODO"}))
            .unwrap();
    }
}
