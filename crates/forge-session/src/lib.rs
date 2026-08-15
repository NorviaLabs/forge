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

mod snapshot;

pub use snapshot::{SessionSnapshot, TranscriptSnapshot};

use std::sync::Arc;

use forge_config::Config;
use forge_core::{AgentSession, LoopConfig};
use forge_governance::{parse_pattern_rules, Governance};
use forge_mcp::{register_static_mcp, McpManager, StaticMcpTool};
use forge_model::{client_from_config, ModelClient};
use forge_storage::{LocalRuntimeStorage, RuntimeDataKind, RuntimeStorage};
use forge_tools::ToolRegistry;
use forge_types::{SessionId, SideEffectClass};
use serde_json::json;

/// Which session to open: a new one, or a specific existing one to resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionTarget {
    /// Create a fresh session.
    #[default]
    New,
    /// Resume the session with this id.
    Resume(SessionId),
}

impl SessionTarget {
    fn resume_id(self) -> Option<SessionId> {
        match self {
            SessionTarget::New => None,
            SessionTarget::Resume(id) => Some(id),
        }
    }
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
        let storage = LocalRuntimeStorage::new(cfg.workspace_root());
        if let Ok(dir) = storage.path_for(RuntimeDataKind::Session) {
            let mut notices = Vec::new();
            if let Some(reason) = storage.fallback_reason() {
                notices.push(reason);
            }
            let tracked = storage.tracked_migration_conflicts();
            if !tracked.is_empty() {
                let paths = tracked
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                notices.push(format!(
                    "Some Forge runtime files are already tracked by Git ({paths}). \
                     Forge did not modify the Git index; review the tracked files before migration."
                ));
            }
            return (dir, notices);
        }
    }
    (cfg.journal_dir(), Vec::new())
}

/// Collect stored OAuth / API-key material for the native model client.
///
/// Returns the pairs instead of exporting them. `NativeModelClient` reads its
/// injected map ahead of the process environment, so the client never needed
/// them in `std::env` — and putting them there handed a copy to every child
/// process Forge starts, including MCP servers and shell commands.
///
/// An explicitly exported variable still wins, which is the precedence
/// `forge_connect::resolve_key` already uses.
pub fn connect_credentials() -> Vec<(String, String)> {
    let reg = forge_connect::loaded_registry();
    let store = forge_connect::CredentialStore::user_default();
    let preferences = forge_connect::PreferenceStore::user_default();
    let svc = forge_connect::ConnectService {
        registry: &reg,
        store: &store,
        preferences: &preferences,
        active_profile_id: None,
        active_model: None,
    };
    let mut pairs = Vec::new();
    for profile in reg.profiles() {
        let Ok(profile_pairs) = svc.provider_env_for_profile(&profile.id) else {
            continue;
        };
        for (name, value) in profile_pairs {
            let already_exported = std::env::var(&name)
                .ok()
                .is_some_and(|existing| !existing.trim().is_empty());
            if !already_exported {
                pairs.push((name, value));
            }
        }
    }
    pairs
}

/// Assemble a session from configuration, independent of any frontend.
pub async fn open_session(cfg: &Config, target: SessionTarget) -> anyhow::Result<OpenedSession> {
    let model: Arc<dyn ModelClient> =
        Arc::from(client_from_config(cfg).map_err(|e| anyhow::anyhow!(e))?);
    // After construction, because credentials are resolved per request rather
    // than at build time.
    model.apply_provider_env(&connect_credentials());

    let mut tools = ToolRegistry::new();
    register_static_mcp(
        &mut tools,
        "demo",
        vec![StaticMcpTool {
            server_id: "demo".into(),
            tool_name: "echo".into(),
            description: "Echo text (static MCP demo)".into(),
            schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
            side_effect_class: SideEffectClass::Meta,
            handler: Box::new(|args| forge_types::ToolOutput {
                outcome: Default::default(),
                content: args
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                is_error: false,
                exit_code: None,
                attachments: Vec::new(),
            }),
        }],
    );

    let mut notices = cfg.refused_key_notices();
    if !cfg.mcp.servers.is_empty() {
        let mut mgr = McpManager::new();
        let errors = mgr.connect_all(&cfg.mcp.servers).await;
        for e in errors {
            notices.push(format!("mcp: {e}"));
        }
        let _ = mgr.register_into(&mut tools).await;
    }

    let (journal_dir, storage_notices) = resolve_journal_dir(cfg);
    notices.extend(storage_notices);
    let loop_cfg = LoopConfig {
        max_turns: 128,
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir,
        enable_context_lifecycle: true,
        enable_governance: true,
        web_search: cfg.tools.web_search.clone(),
    };

    let mut session = if let Some(session_id) = target.resume_id() {
        AgentSession::resume(loop_cfg, model, tools, session_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?
    } else {
        AgentSession::create(loop_cfg, model, tools)
            .await
            .map_err(|e| anyhow::anyhow!(e))?
    };
    if !cfg.model.model.is_empty() {
        session.set_active_model(cfg.model.model.clone());
        let cache = forge_connect::ModelCatalogCache::user_default();
        if !cfg!(test) && !cache.image_input_ready() {
            let _ = forge_connect::refresh_models_dev_registry(
                forge_connect::loaded_registry().profiles(),
                &cache,
            );
        }
        session.set_image_input_supported(cache.model_accepts_image_input(&cfg.model.model));
    }

    let (permissions, permission_notices) = forge_config::load_permissions(cfg.workspace_root());
    notices.extend(permission_notices);
    session.set_governance(Governance::default().with_pattern_rules(
        parse_pattern_rules(&permissions.allow),
        parse_pattern_rules(&permissions.deny),
    ));

    Ok(OpenedSession { session, notices })
}

#[cfg(test)]
mod tests {
    use super::*;
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

        // Tokens exported for OAuth providers do not appear in `api_key_env`.
        for name in [
            "XAI_API_KEY",
            forge_connect::OPENAI_CODEX_ACCESS_TOKEN_ENV,
            forge_connect::OPENAI_CODEX_ACCOUNT_ID_ENV,
        ] {
            assert!(
                stripped.contains(&name),
                "`{name}` is exported for an OAuth provider but would not be stripped"
            );
        }
    }

    /// Git-initializes `dir` so the journal-dir resolver exercises
    /// repository-local storage hermetically, inside the tempdir — without
    /// this, an unconfigured journal path falls back to the platform
    /// application-data directory (correct real-world behavior outside a
    /// repository, but not what a test should touch on the host machine).
    fn init_repo(dir: &std::path::Path) {
        for args in [
            vec!["init", "--initial-branch=main", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

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
        assert_eq!(dir, temp.path().join("custom/journal"));
        assert!(notices.is_empty());
    }

    #[test]
    fn resolve_journal_dir_reports_tracked_legacy_files_as_a_notice() {
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

        let (_dir, notices) = resolve_journal_dir(&cfg);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("already tracked by Git"));
    }
}
