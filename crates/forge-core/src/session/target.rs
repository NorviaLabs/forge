//! Where the session's tools run: this machine, or an SSH host.

use crate::*;
use forge_types::ExecutionTarget;

/// Starts the system prompt section that says where commands run.
const TARGET_SECTION: &str = "\n\n# Where this session runs\n\n";

/// The tools whose work happens on the session's target. An approval for
/// one of them names the host.
pub(crate) fn runs_on_target(tool: &str) -> bool {
    matches!(
        tool,
        "bash"
            | "exec_command"
            | "write_stdin"
            | "background_run"
            | "git"
            | "read_file"
            | "write_file"
            | "edit"
            | "apply_patch"
            | "ls"
            | "view_image"
            | "glob"
            | "grep"
    )
}

impl AgentSession {
    /// Run this session's workspace tools on `runtime`, the connected
    /// target `target` describes. For an SSH host: approvals name the host,
    /// "ask before each command" gates every command and `git` call, the
    /// unconfined-retry tool is withdrawn (nothing there is sandboxed), and
    /// the model is told where its commands run.
    pub fn set_execution_target(
        &mut self,
        target: ExecutionTarget,
        runtime: Arc<dyn forge_tools::target::Target>,
    ) {
        self.tool_ctx.target = runtime;
        self.execution_target = target;
        let governance = self.governance.clone();
        self.governance = self.governed_for_target(governance);
        self.describe_target_in_prompt();
    }

    /// Release what the target holds (for an SSH host: the connection and
    /// the temp directory there). Called when the session closes.
    pub async fn close_execution_target(&self) {
        self.tool_ctx.target.close().await;
    }

    /// Where this session's tools run.
    pub fn execution_target(&self) -> &ExecutionTarget {
        &self.execution_target
    }

    /// The SSH host an approval for `tool` would run on.
    pub(crate) fn approval_target(&self, tool: &str) -> Option<String> {
        self.execution_target
            .host()
            .filter(|_| runs_on_target(tool))
            .map(str::to_string)
    }

    /// `governance` with this session's target applied: on a host, nothing
    /// is sandboxed, so the unconfined retry is withdrawn; asking before each
    /// command gates commands there.
    pub(crate) fn governed_for_target(&self, mut governance: Governance) -> Governance {
        if self.execution_target.is_local() {
            return governance;
        }
        governance.acl.deny("request_unconfined_retry".into());
        if self.execution_target.asks_before_commands() {
            governance = governance.asking_before_remote_commands();
        }
        governance
    }

    /// Put a section saying where commands run into the system prompt (in
    /// place of an earlier one), before the custom agents section that must
    /// stay last, and name the host's temp directory instead of this
    /// machine's scratch directory.
    pub(crate) fn describe_target_in_prompt(&mut self) {
        let Some(system) = self
            .messages
            .first()
            .filter(|message| message.role == forge_types::MessageRole::System)
        else {
            return;
        };
        let (base, agents) = match system.content.strip_suffix(self.agents_section.as_str()) {
            Some(base) => (base.to_string(), self.agents_section.clone()),
            None => (system.content.clone(), String::new()),
        };
        let mut base = match base.find(TARGET_SECTION) {
            Some(start) => base[..start].to_string(),
            None => base,
        };
        let ExecutionTarget::Ssh { host, path, ask } = &self.execution_target else {
            self.messages[0].content = format!("{base}{agents}");
            return;
        };
        if let Some(tmp) = &self.tool_ctx.session_tmp {
            base = base.replace(&tmp.path().display().to_string(), "$TMPDIR");
        }
        let asking = if *ask {
            " The user approves each command before it runs."
        } else {
            ""
        };
        let section = format!(
            "{TARGET_SECTION}This session runs on the SSH host `{host}`, in `{path}`: every \
shell command, file tool, search and `git` call works there, not on the user's machine. \
Commands there run outside Forge's sandbox, with that machine's network and \
credentials.{asking} The sandbox, network and scratch-directory rules above describe the \
user's machine and do not apply on the host; use `$TMPDIR` there for temporary files."
        );
        self.messages[0].content = format!("{base}{section}{agents}");
        if let Ok(mut cache) = self.ctx_tokens_cache.lock() {
            *cache = None;
        }
    }
}
