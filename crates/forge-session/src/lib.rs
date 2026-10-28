//! Frontend-independent session assembly.
//!
//! Building a usable `AgentSession` means resolving credentials, registering
//! MCP servers, locating the journal, and composing governance from workspace
//! permissions. That work used to live in `forge-cli`'s `main.rs`, which made
//! the terminal binary the only thing in the workspace that could produce a
//! session. Anything else wanting one — a background agent host or a test
//! harness — had to duplicate it.
//!
//! This crate sits above `forge-core` because assembly needs `forge-mcp` and
//! `forge-connect`, neither of which `forge-core` depends on and neither of
//! which belongs in it.

mod control;
pub mod headless;
mod snapshot;
mod supervisor;

pub use forge_storage::RepositoryRuntimeStorage;

pub use control::{
    CompletedCreation, LeaseOwner, NewRepositorySession, PendingCreation, RepositoryControl,
    RepositoryLease, RepositorySession, RepositorySessionError, SessionLifecycle, StaleCreation,
    SupervisorTurnState, WorktreeOwnership,
};
pub use headless::{run_headless, ApprovalPolicy, ApprovalRequired};
pub use snapshot::{
    replayed_transcript, BackgroundTaskSnapshot, SessionDetailsSnapshot, SessionSnapshot,
    ShellExecutionSnapshot, TranscriptSnapshot,
};
pub use supervisor::{
    QueuedPromptId, RepositoryBootstrap, RepositorySupervisor, RepositorySupervisorError,
    SessionRuntimeSnapshot, SupervisorCommand, SupervisorEvent, SupervisorHandle,
};

use std::sync::Arc;

use forge_config::Config;
use forge_core::{AgentSession, LoopConfig};
use forge_governance::{parse_pattern_rules, Governance};
use forge_mcp::McpManager;
use forge_model::{client_from_config, ModelClient};
use forge_storage::{RuntimeDataKind, RuntimeStorage};
use forge_tools::ToolRegistry;
use forge_types::SessionId;

/// Which session to open: a new one, an existing one, or a fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionTarget {
    /// Create a fresh session.
    #[default]
    New,
    /// Resume the session with this id.
    Resume(SessionId),
    /// Create a new session seeded from this session's model-visible context.
    Fork(SessionId),
}

/// A session plus any non-fatal notices produced while assembling it
/// (storage fallbacks, refused config keys, MCP connection failures).
/// Notices are informational — the session is usable regardless.
pub struct OpenedSession {
    pub session: AgentSession,
    pub notices: Vec<String>,
}

/// Resolve where the session journal lives, and any startup notices the
/// resolution itself produced. An explicit `journal.path` override (via
/// `forge.toml`/env/CLI) is respected as-is — advanced use, not managed by
/// the storage resolver. Otherwise, route through the centralized
/// runtime-storage resolver: `.forge/local/sessions` inside a Git
/// repository (natively excluded from `git status`), or the platform
/// application-data directory outside one — surfacing a notice if
/// repository-local storage fell back, or if legacy runtime files were
/// found already tracked by Git (never silently migrated or altered).
pub fn resolve_journal_dir(cfg: &Config) -> (std::path::PathBuf, Vec<String>) {
    if cfg.journal.path == forge_config::default_journal_path() {
        let storage = RepositoryRuntimeStorage::new(cfg.workspace_root());
        if let Ok(storage) = storage {
            let Ok(dir) = storage.path_for(RuntimeDataKind::Session) else {
                return (cfg.journal_dir(), Vec::new());
            };
            let mut notices = Vec::new();
            if let Some(reason) = storage.fallback_reason() {
                notices.push(reason);
            }
            return (dir, notices);
        }
    }
    let configured = std::path::PathBuf::from(&cfg.journal.path);
    if configured.is_absolute() {
        return (configured, Vec::new());
    }
    match forge_storage::main_worktree(cfg.workspace_root()) {
        Ok(main) => (main.join(configured), Vec::new()),
        Err(_) => (cfg.journal_dir(), Vec::new()),
    }
}

/// The routing snapshot for a session: the loaded provider registry over
/// the process credentials handle. Credentials are read per request, so a
/// new key needs no rebuild; a definition change does (`ReloadProviders`).
pub fn route_table() -> forge_model::RouteTable {
    forge_connect::resolve(
        &forge_connect::loaded_registry(),
        &forge_connect::Credentials::shared(),
    )
}

/// Assemble a session from configuration, independent of any frontend.
pub async fn open_session(cfg: &Config, target: SessionTarget) -> anyhow::Result<OpenedSession> {
    let model: Arc<dyn ModelClient> =
        Arc::from(client_from_config(cfg).map_err(|e| anyhow::anyhow!(e))?);

    open_session_with_model(cfg, target, model).await
}

async fn open_session_with_model(
    cfg: &Config,
    target: SessionTarget,
    model: Arc<dyn ModelClient>,
) -> anyhow::Result<OpenedSession> {
    let mut tools = ToolRegistry::new();

    let mut notices = cfg.refused_key_notices();
    // One live egress state for the whole session, built before the MCP
    // servers start (they start before the session exists) and adopted by
    // the session gateway below. Sandboxed servers route through endpoints of
    // their own on this state, so a host granted for the session — or
    // persistently — reaches their traffic immediately, and their
    // connections appear in the session event log as `mcp:<server>`.
    let egress_state = forge_tools::egress::EgressShared::new(
        forge_core::egress_policy_for_workspace(cfg.workspace_root()),
        Default::default(),
    );
    if !cfg.mcp.servers.is_empty() {
        let mut mgr = McpManager::new().with_egress_shared(egress_state.clone());
        let errors = mgr
            .connect_all_in_workspace(&cfg.mcp.servers, cfg.workspace_root())
            .await;
        for e in errors {
            notices.push(format!("mcp: {e}"));
        }
        let _ = mgr.register_into(&mut tools).await;
        forge_mcp::install_search_tools(&mut tools);
    }

    let (journal_dir, storage_notices) = resolve_journal_dir(cfg);
    notices.extend(storage_notices);
    let loop_cfg = LoopConfig {
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir,
        enable_context_lifecycle: true,
        enable_governance: true,
        web_search: cfg.tools.web_search.clone(),
        agents: cfg.tools.agents.clone(),
    };

    let mut session = match target {
        SessionTarget::New => AgentSession::create(loop_cfg, model, tools)
            .await
            .map_err(|e| anyhow::anyhow!(e))?,
        SessionTarget::Resume(session_id) => {
            AgentSession::resume(loop_cfg, model, tools, session_id)
                .await
                .map_err(|e| anyhow::anyhow!(e))?
        }
        SessionTarget::Fork(session_id) => {
            let source = AgentSession::resume(loop_cfg, model.clone(), tools.clone(), session_id)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            source.fork().await.map_err(|e| anyhow::anyhow!(e))?
        }
    };
    if !cfg.model.model.is_empty() {
        session.set_active_model(cfg.model.model.clone());
        let cache = forge_connect::ModelCatalogCache::user_default();
        session.set_image_input_supported(cache.model_accepts_image_input(&cfg.model.model));
        // Compaction reads the model's real context window from the registry
        // cache. An unknown id keeps `CompactionPolicy`'s conservative
        // default rather than claiming a window Forge cannot verify.
        if let Some(limits) = cache.model_limits(&cfg.model.model) {
            session
                .set_context_window(limits.context, (limits.output > 0).then_some(limits.output));
        }
    }

    // Requests route through the snapshot, and provider keys never reach
    // model-triggered child processes: the built-in vendor aliases are
    // stripped by forge-tools, the snapshot adds every generated name and
    // every key name the loaded profiles read (custom `api_key_env` too).
    session.set_route_table(route_table());

    let (permissions, permission_notices) = forge_config::load_permissions(cfg.workspace_root());
    notices.extend(permission_notices);
    session.set_governance(Governance::default().with_pattern_rules(
        parse_pattern_rules(&permissions.allow),
        parse_pattern_rules(&permissions.deny),
    ));
    // Serve the state the MCP servers share. It already holds the personal
    // `host(...)` policy; later `apply_egress_policy` calls replace that
    // policy in place, so the servers keep following it.
    session.adopt_egress_state(egress_state).await;

    Ok(OpenedSession { session, notices })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// Exercises the exact composition `open_session` wires at startup —
    /// `load_permissions` output feeding `Governance::with_pattern_rules` —
    /// without needing a real model client. A workspace-scope `deny` rule
    /// must actually affect `authorize()`, proving the plumbing is live, not
    /// just parsed and discarded.
    #[test]
    fn workspace_deny_rule_reaches_governance_authorize() {
        let dir = TempDir::new().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(
            forge_dir.join("permissions.toml"),
            "allow = [\"bash(*)\"]\ndeny = [\"bash(rm -rf*)\"]\n",
        )
        .unwrap();

        let (permissions, _notices) = forge_config::load_permissions(dir.path());
        let governance = Governance::default().with_pattern_rules(
            parse_pattern_rules(&permissions.allow),
            parse_pattern_rules(&permissions.deny),
        );

        let call = forge_types::ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": "rm -rf /"}),
        };
        assert_eq!(
            governance.authorize(&call, forge_types::SideEffectClass::Exec),
            forge_types::PolicyDecision::Hitl,
            "repo-scope allow rules must never be honored, and deny always wins anyway"
        );
    }

    /// `forge-tools` cannot see `forge-connect`, so the list of credential
    /// variables the shell tool strips is maintained by hand. This crate depends
    /// on both, so it is where the two can be checked against each other: a new
    /// provider whose key is not on that list would otherwise be readable by any
    /// model-authored command, silently.
    #[test]
    fn credential_env_names_cover_every_connect_profile() {
        let registry = forge_connect::builtin_registry();
        let stripped = forge_tools::PROVIDER_CREDENTIAL_ENV;

        for profile in registry.profiles() {
            for name in &profile.api_key_env {
                assert!(
                    stripped.contains(&name.as_str()),
                    "`{name}` (profile `{}`) is a provider credential that the shell tool would \
                     not strip — add it to forge_tools::PROVIDER_CREDENTIAL_ENV",
                    profile.id
                );
            }
        }
    }

    /// Custom profiles declare their own key names. Those cannot be in the
    /// fixed list, so the session hands them to the tool layer instead.
    #[test]
    fn custom_profile_key_names_are_stripped_from_child_processes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("providers.toml");
        std::fs::write(
            &path,
            r#"[[providers]]
id = "groq"
title = "Groq"
description = "Groq gateway"
route_id = "groq"
model_provider_prefix = "groq"
default_base_url = "https://api.groq.com/openai/v1"
api_key_env = ["GROQ_API_KEY"]
transport = "openai-compat"
catalog_mode = "live"
auth_mode = { api_key = { tui_always_prompt = true } }
"#,
        )
        .unwrap();
        let registry = forge_connect::loaded_registry_from(&path);
        assert!(
            registry.get("groq").is_some(),
            "fixture must load: {:?}",
            registry.load_warnings()
        );
        let names = forge_connect::credential_env_names(&registry);
        assert!(names.iter().any(|name| name == "GROQ_API_KEY"));
        assert!(!forge_tools::PROVIDER_CREDENTIAL_ENV.contains(&"GROQ_API_KEY"));
        assert!(forge_tools::is_provider_credential_env(
            "GROQ_API_KEY",
            &names
        ));
        assert!(forge_tools::is_provider_credential_env(
            "FORGE_GROQ_API_KEY",
            &[]
        ));
    }

    /// `init_repo` git-initializes a tempdir so the journal-dir resolver
    /// exercises repository-local storage hermetically, inside that tempdir —
    /// without it, an unconfigured journal path falls back to the platform
    /// application-data directory (correct real-world behavior outside a
    /// repository, but not what a test should touch on the host machine).
    use forge_test_support::init_repo;

    #[tokio::test]
    async fn open_session_with_mock_model_builds_a_session_without_notices() {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        let mut cfg = Config::default();
        cfg.model.provider = forge_config::ModelProviderKind::Mock;
        cfg.model.model = "mock".into();
        cfg.resolved_workspace = temp.path().to_path_buf();
        cfg.workspace_root = Some(temp.path().display().to_string());

        let opened = open_session(&cfg, SessionTarget::New).await.unwrap();
        assert!(opened.notices.is_empty());
        assert_eq!(opened.session.session_id.to_string().len(), 36);
    }

    #[test]
    fn resolve_journal_dir_uses_the_storage_resolver_for_the_unconfigured_default() {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        let cfg = Config {
            resolved_workspace: temp.path().to_path_buf(),
            workspace_root: Some(temp.path().display().to_string()),
            ..Default::default()
        };

        let (dir, notices) = resolve_journal_dir(&cfg);
        assert_eq!(
            dir.canonicalize().unwrap(),
            temp.path()
                .join(".forge")
                .join("local")
                .join("sessions")
                .canonicalize()
                .unwrap()
        );
        assert!(notices.is_empty());
    }

    #[test]
    fn resolve_journal_dir_respects_an_explicit_override() {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        let cfg = Config {
            resolved_workspace: temp.path().to_path_buf(),
            workspace_root: Some(temp.path().display().to_string()),
            journal: forge_config::JournalConfig {
                path: "custom/journal".into(),
                ..Default::default()
            },
            ..Default::default()
        };

        let (dir, notices) = resolve_journal_dir(&cfg);
        assert_eq!(
            dir,
            temp.path().canonicalize().unwrap().join("custom/journal")
        );
        assert!(notices.is_empty());
    }

    #[test]
    fn resolve_journal_dir_does_not_import_or_report_legacy_files() {
        let temp = TempDir::new().unwrap();
        init_repo(temp.path());
        std::fs::create_dir_all(temp.path().join(".forge")).unwrap();
        std::fs::write(temp.path().join(".forge/ui-state.json"), "{}").unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(temp.path())
            .args(["add", ".forge/ui-state.json"])
            .status()
            .unwrap();
        assert!(status.success());

        let cfg = Config {
            resolved_workspace: temp.path().to_path_buf(),
            workspace_root: Some(temp.path().display().to_string()),
            ..Default::default()
        };

        let (dir, notices) = resolve_journal_dir(&cfg);
        assert!(notices.is_empty());
        assert!(dir.ends_with(".forge/local/sessions"));
        assert!(temp.path().join(".forge/ui-state.json").exists());
    }
}
