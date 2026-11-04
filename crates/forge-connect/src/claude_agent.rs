//! The Claude Agent provider's process conventions.
//!
//! Forge runs the user's own installed `claude` program over the Claude
//! Agent SDK's stdio protocol (`--input-format stream-json`). Forge never
//! reads Claude Code's saved login and never sits between Claude Code and
//! Anthropic: the user signs in with `claude auth login`, and Claude Code
//! makes every API call itself. `/connect` only records the choice.
//!
//! This module owns what both sides of Forge need: finding `claude`, the
//! flags and environment that turn Claude Code's own features off in favour
//! of Forge's (tools, instructions, compaction, memory), and the startup
//! probe that reports the version, the login, and the models. The
//! long-running chat process lives in `forge-model`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Overrides which `claude` Forge runs.
pub const EXECUTABLE_ENV: &str = "FORGE_CLAUDE_PATH";

/// The name Forge's in-process MCP server registers under. Claude sees
/// Forge's tools as `mcp__forge__<name>`.
pub const MCP_SERVER: &str = "forge";

/// Prefix Claude Code puts on every tool of [`MCP_SERVER`].
pub const TOOL_PREFIX: &str = "mcp__forge__";

/// Older builds ignore `client_composed` and may treat user text as Claude
/// Code commands.
pub const MINIMUM_VERSION: &str = "2.1.248";

/// How long `claude` may take to answer `initialize`.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Where to get Claude Code.
pub const INSTALL_HINT: &str =
    "Install Claude Code (https://claude.com/claude-code) and run `claude auth login`, or set FORGE_CLAUDE_PATH.";

/// The `claude` to run: `FORGE_CLAUDE_PATH`, else `claude` on `PATH`.
pub fn executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(EXECUTABLE_ENV).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    find_on_path(name, std::env::var_os("PATH")?.as_os_str())
}

fn find_on_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Environment Forge applies over the inherited one: `Some` sets a
/// variable, `None` removes it.
///
/// Each switch turns off a Claude Code feature Forge has its own version
/// of, so the model sees one set of instructions, one tool system, and one
/// compactor.
pub fn isolation_env() -> Vec<(&'static str, Option<&'static str>)> {
    vec![
        // Forge sends AGENTS.md and the rest of its instructions itself.
        ("CLAUDE_CODE_DISABLE_CLAUDE_MDS", Some("1")),
        // Notes only Claude models would see, outside Forge's control.
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", Some("1")),
        // Forge compacts; a second compactor would desync the histories.
        ("DISABLE_AUTO_COMPACT", Some("1")),
        ("CLAUDE_CODE_ENABLE_AWAY_SUMMARY", Some("0")),
        // Every Forge tool reaches Claude as an MCP tool; tool search would
        // hide all of them.
        ("ENABLE_TOOL_SEARCH", Some("false")),
        // Server-side calls Forge could neither show nor control.
        ("CLAUDE_CODE_DISABLE_ADVISOR_TOOL", Some("1")),
        // Forge truncates tool output itself.
        ("MAX_MCP_OUTPUT_TOKENS", Some("200000")),
        // Identifies the embedding client in Claude Code's own telemetry.
        ("CLAUDE_CODE_ENTRYPOINT", Some("sdk-forge")),
        // Skips OAuth, so a subscription login would not be used.
        ("CLAUDE_CODE_SIMPLE", None),
        // Would override the effort chosen in Forge.
        ("CLAUDE_CODE_EFFORT_LEVEL", None),
        // Would cap every Forge tool call, failing long permission prompts.
        ("MCP_TOOL_TIMEOUT", None),
    ]
}

/// Flags every `claude` Forge starts shares: stream-json both ways, no
/// built-in tools, no settings files, hooks, or MCP servers of the user's,
/// and only Forge's MCP tools allowed without a prompt.
pub fn base_args() -> Vec<String> {
    [
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--tools=",
        "--setting-sources=",
        "--strict-mcp-config",
        "--permission-mode=dontAsk",
    ]
    .into_iter()
    .map(str::to_string)
    .chain([
        format!("--allowedTools={TOOL_PREFIX}*"),
        "--settings".into(),
        r#"{"disableAllHooks":true}"#.into(),
    ])
    .collect()
}

/// A `claude` command with [`base_args`] and [`isolation_env`] applied.
pub fn command(executable: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(executable);
    command.args(base_args()).current_dir(cwd);
    for (name, value) in isolation_env() {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    command
}

/// The SDK's `initialize` control request with no tools and no prompt.
pub fn probe_initialize(request_id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "initialize", "systemPrompt": [""], "skills": []},
    })
}

/// One model `claude` offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeModel {
    /// What `--model` takes: an alias (`opus`) or a full id.
    pub value: String,
    /// The full id the alias currently means.
    pub resolved: Option<String>,
    pub display_name: String,
}

impl ProbeModel {
    /// The id Forge lists: the resolved full id, so models.dev metadata
    /// (context window, images, effort) applies.
    pub fn catalog_id(&self) -> &str {
        self.resolved.as_deref().unwrap_or(&self.value)
    }
}

/// What a `claude` reports when it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub version: Option<String>,
    pub logged_in: bool,
    /// `Claude Max`, `Claude Team`, …; absent for an API key.
    pub subscription: Option<String>,
    pub models: Vec<ProbeModel>,
}

impl Probe {
    /// Model ids for the catalog, without the `default` row and duplicates.
    pub fn catalog_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for model in &self.models {
            if model.value == "default" {
                continue;
            }
            let id = model.catalog_id().to_string();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    /// Why this `claude` cannot run a chat, if it cannot.
    pub fn readiness_error(&self) -> Option<String> {
        if !self.logged_in {
            return Some(
                "Claude Code is not signed in. Run `claude auth login` in a terminal, then connect again."
                    .into(),
            );
        }
        version_error(self.version.as_deref())
    }
}

/// `None` when `version` is new enough for Forge.
pub fn version_error(version: Option<&str>) -> Option<String> {
    match version {
        Some(version) if version_at_least(version, MINIMUM_VERSION) => None,
        Some(version) => Some(format!(
            "Claude Code {version} is too old; {MINIMUM_VERSION} or later is required. Run `claude update`."
        )),
        None => Some(
            "Claude Code did not report its version, so compatibility could not be checked. Run `claude update`."
                .into(),
        ),
    }
}

/// Compares release numbers like `2.1.248`, ignoring a pre-release or
/// build suffix.
pub fn version_at_least(version: &str, minimum: &str) -> bool {
    let parse = |value: &str| -> Vec<u64> {
        value
            .split(['-', '+'])
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|part| part.trim().parse().unwrap_or(0))
            .collect()
    };
    let current = parse(version);
    let required = parse(minimum);
    for (index, needed) in required.iter().enumerate() {
        let have = current.get(index).copied().unwrap_or(0);
        if have != *needed {
            return have > *needed;
        }
    }
    true
}

/// Read the `initialize` response's version, login, and models.
pub fn parse_initialize_response(response: &Value) -> Probe {
    let account = response.get("account").cloned().unwrap_or(Value::Null);
    // A logged-out Claude Code reports `tokenSource: "none"` and no API key.
    let logged_out = account.get("tokenSource").and_then(Value::as_str) == Some("none")
        && account.get("apiKeySource").is_none();
    let models = response
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    let value = model.get("value")?.as_str()?.trim();
                    (!value.is_empty()).then(|| ProbeModel {
                        value: value.to_string(),
                        resolved: model
                            .get("resolvedModel")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        display_name: model
                            .get("displayName")
                            .and_then(Value::as_str)
                            .unwrap_or(value)
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Probe {
        version: response
            .get("claude_code_version")
            .and_then(Value::as_str)
            .map(str::to_string),
        logged_in: !account.is_null() && !logged_out,
        subscription: account
            .get("subscriptionType")
            .and_then(Value::as_str)
            .map(str::to_string),
        models,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("Claude Code was not found. {INSTALL_HINT}")]
    Missing,
    #[error("{0}")]
    Failed(String),
}

/// Start `claude`, ask for its startup report, and stop it. Sends no
/// prompt, so no model call is made and no usage is spent. Blocking.
pub fn probe() -> Result<Probe, ProbeError> {
    let executable = executable().ok_or(ProbeError::Missing)?;
    let cwd = std::env::temp_dir();
    let mut command = command(&executable, &cwd);
    command
        .arg("--no-session-persistence")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|error| ProbeError::Failed(format!("could not start Claude Code: {error}")))?;
    probe_child(child, STARTUP_TIMEOUT)
}

fn probe_child(mut child: Child, timeout: Duration) -> Result<Probe, ProbeError> {
    let result = probe_io(&mut child, timeout);
    // Closing stdin ends `claude`; kill it if it lingers.
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return result;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn probe_io(child: &mut Child, timeout: Duration) -> Result<Probe, ProbeError> {
    const REQUEST_ID: &str = "forge-probe";
    let stdout = child.stdout.take().ok_or_else(|| failed("no stdout"))?;
    let stderr = child.stderr.take();
    let stderr_text = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    });
    let (lines_tx, lines_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| failed("no stdin"))?;
        let line = probe_initialize(REQUEST_ID).to_string();
        writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .map_err(|error| failed(&format!("could not write to Claude Code: {error}")))?;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match lines_rx.recv_timeout(remaining) {
            Ok(line) => {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("type").and_then(Value::as_str) != Some("control_response") {
                    continue;
                }
                let response = &message["response"];
                if response.get("request_id").and_then(Value::as_str) != Some(REQUEST_ID) {
                    continue;
                }
                if response.get("subtype").and_then(Value::as_str) == Some("error") {
                    let error = response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error");
                    return Err(failed(&format!("Claude Code failed to start: {error}")));
                }
                return Ok(parse_initialize_response(
                    response.get("response").unwrap_or(&Value::Null),
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(failed(&format!(
                    "Claude Code did not start within {} seconds",
                    timeout.as_secs()
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.wait();
                let stderr = stderr_text.join().unwrap_or_default();
                let detail = stderr.trim();
                return Err(failed(&if detail.is_empty() {
                    "Claude Code exited before it started".to_string()
                } else {
                    format!("Claude Code exited before it started: {detail}")
                }));
            }
        }
    }
}

fn failed(message: &str) -> ProbeError {
    ProbeError::Failed(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_release_number() {
        assert!(version_at_least("2.1.318", "2.1.248"));
        assert!(version_at_least("2.1.248", "2.1.248"));
        assert!(version_at_least("3.0.0", "2.1.248"));
        assert!(version_at_least("2.1.248-beta.1", "2.1.248"));
        assert!(!version_at_least("2.1.247", "2.1.248"));
        assert!(!version_at_least("2.0.999", "2.1.248"));
        assert!(!version_at_least("1", "2.1.248"));
    }

    #[test]
    fn initialize_response_reports_login_and_models() {
        let probe = parse_initialize_response(&json!({
            "claude_code_version": "2.1.318",
            "account": {"email": "a@example.com", "subscriptionType": "Claude Max", "apiProvider": "firstParty"},
            "models": [
                {"value": "default", "resolvedModel": "claude-opus-5-5", "displayName": "Default"},
                {"value": "opus", "resolvedModel": "claude-opus-5-5", "displayName": "Opus 5.5"},
                {"value": "haiku", "resolvedModel": "claude-haiku-5-5", "displayName": "Haiku 5.5"},
                {"value": "claude-sonnet-4-6", "displayName": "Sonnet 4.6"}
            ]
        }));
        assert!(probe.logged_in);
        assert_eq!(probe.subscription.as_deref(), Some("Claude Max"));
        assert_eq!(probe.readiness_error(), None);
        assert_eq!(
            probe.catalog_ids(),
            ["claude-opus-5-5", "claude-haiku-5-5", "claude-sonnet-4-6"]
        );
    }

    #[test]
    fn a_logged_out_or_old_claude_is_not_ready() {
        let logged_out = parse_initialize_response(&json!({
            "claude_code_version": "2.1.318",
            "account": {"tokenSource": "none", "apiProvider": "firstParty"},
            "models": []
        }));
        assert!(!logged_out.logged_in);
        assert!(logged_out
            .readiness_error()
            .unwrap()
            .contains("claude auth login"));

        let api_key = parse_initialize_response(&json!({
            "claude_code_version": "2.1.318",
            "account": {"tokenSource": "none", "apiKeySource": "ANTHROPIC_API_KEY"},
        }));
        assert!(api_key.logged_in);

        let old = parse_initialize_response(&json!({
            "claude_code_version": "2.1.100",
            "account": {"email": "a@example.com"},
        }));
        assert!(old.readiness_error().unwrap().contains("claude update"));
    }

    #[test]
    fn the_launch_turns_off_claude_code_features_forge_replaces() {
        let args = base_args();
        for flag in [
            "--tools=",
            "--setting-sources=",
            "--strict-mcp-config",
            "--permission-mode=dontAsk",
            "--allowedTools=mcp__forge__*",
        ] {
            assert!(args.iter().any(|arg| arg == flag), "missing {flag}");
        }
        let env = isolation_env();
        assert!(env.contains(&("CLAUDE_CODE_DISABLE_CLAUDE_MDS", Some("1"))));
        assert!(env.contains(&("DISABLE_AUTO_COMPACT", Some("1"))));
        assert!(env.contains(&("CLAUDE_CODE_SIMPLE", None)));
    }

    #[test]
    fn path_search_finds_only_executables() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("claude");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            assert_eq!(find_on_path("claude", dir.path().as_os_str()), None);
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            find_on_path("claude", dir.path().as_os_str()),
            Some(plain.clone())
        );
    }

    /// A stand-in `claude` that answers `initialize` like a signed-in one.
    #[cfg(unix)]
    #[test]
    fn probe_reads_a_scripted_claude() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("claude");
        std::fs::write(
            &script,
            "#!/bin/sh\nread line\necho '{\"type\":\"system\",\"subtype\":\"noise\"}'\necho '{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"forge-probe\",\"response\":{\"claude_code_version\":\"2.1.318\",\"account\":{\"email\":\"a@example.com\"},\"models\":[{\"value\":\"opus\",\"resolvedModel\":\"claude-opus-5-5\",\"displayName\":\"Opus\"}]}}}'\ncat >/dev/null\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let probe = probe_child(child, Duration::from_secs(10)).unwrap();
        assert!(probe.logged_in);
        assert_eq!(probe.catalog_ids(), ["claude-opus-5-5"]);

        let silent = dir.path().join("silent");
        std::fs::write(&silent, "#!/bin/sh\necho 'boom' >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&silent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new(&silent)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let error = probe_child(child, Duration::from_secs(10)).unwrap_err();
        assert!(error.to_string().contains("boom"), "{error}");
    }
}
