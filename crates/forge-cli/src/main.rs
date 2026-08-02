//! Forge CLI — launches the full-screen TUI by default.

use std::sync::Arc;

use clap::Parser;
use forge_config::{Config, ConfigOverrides};
use forge_core::{AgentSession, LoopConfig};
use forge_durable::latest_session_id;
use forge_mcp::{register_static_mcp, McpManager, StaticMcpTool};
use forge_model::{client_from_config, ModelClient};
use forge_storage::{LocalRuntimeStorage, RuntimeDataKind, RuntimeStorage};
use forge_tools::ToolRegistry;
use forge_tui::{
    resume_session_items, run_tui, run_tui_with_resume_picker, ExitCode, TuiRuntimeConfig,
};
use forge_types::SessionId;
use serde_json::json;
use tracing_subscriber::EnvFilter;

/// Resolve where the session journal lives, and any startup notices the
/// resolution itself produced. An explicit `journal.path` override (via
/// `forge.toml`/env/CLI) is respected as-is — advanced use, not managed by
/// the storage resolver. Otherwise, route through the centralized
/// runtime-storage resolver: `.forge/local/sessions` inside a Git
/// repository (natively excluded from `git status`), or the platform
/// application-data directory outside one — surfacing a notice if
/// repository-local storage fell back, or if legacy runtime files were
/// found already tracked by Git (never silently migrated or altered).
fn resolve_journal_dir(cfg: &Config) -> (std::path::PathBuf, Vec<String>) {
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

#[derive(Parser, Debug)]
#[command(
    name = "forge",
    version,
    about = "Forge AI coding agent",
    long_about = "Open the full-screen TUI by default.\n\nUse --help or --version for CLI info."
)]
struct Cli {
    #[arg(long = "resume", value_name = "SESSION_ID", num_args = 0..=1)]
    resume: Option<Option<SessionId>>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_ansi(false)
        .init();

    let code = match run(cli).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::Failed
        }
    };
    std::process::exit(code.code());
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let overrides = ConfigOverrides {
        config_path: None,
        workspace: None,
        model_provider: None,
        model_id: None,
        api_key: None,
        journal_path: None,
    };
    let cfg = Config::load(overrides).map_err(|e| anyhow::anyhow!(e))?;

    let (startup_resume_items, create_notice) = if cli.resume == Some(None) {
        let (journal_dir, _) = resolve_journal_dir(&cfg);
        let items = resume_session_items(&journal_dir, 10)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if items.is_empty() {
            (
                None,
                Some("No previous session found; creating a new session.".to_string()),
            )
        } else {
            (Some(items), None)
        }
    } else {
        (None, None)
    };
    let (session, mut startup_notices) = open_session(
        &cfg,
        if startup_resume_items.is_some() {
            None
        } else if create_notice.is_some() {
            None
        } else {
            cli.resume
        },
    )
    .await?;
    if let Some(notice) = create_notice {
        startup_notices.push(notice);
    }
    let runtime = TuiRuntimeConfig {
        model_label: cfg.model.model.clone(),
        provider: cfg.model.provider.as_str().into(),
        cwd: cfg.workspace_root().to_path_buf(),
        version: env!("CARGO_PKG_VERSION").into(),
        startup_notices,
        validation_command: cfg.validation.command.clone(),
        file_icons: cfg.tui.file_icons,
        mouse_capture: cfg.tui.mouse_capture,
        theme_id: cfg.tui.theme.clone(),
    };
    let summary = match startup_resume_items {
        Some(items) => run_tui_with_resume_picker(session, runtime, items).await,
        None => run_tui(session, runtime).await,
    }
    .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(token_usage) = summary.token_usage {
        println!("{token_usage}");
        println!(
            "To continue this session, run forge --resume {}",
            summary.session_id
        );
    }
    Ok(summary.exit_code)
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
fn connect_credentials() -> Vec<(String, String)> {
    let reg = forge_connect::builtin_registry();
    let store = forge_connect::CredentialStore::user_default();
    let svc = forge_connect::ConnectService {
        registry: &reg,
        store: &store,
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

async fn open_session(
    cfg: &Config,
    resume: Option<Option<SessionId>>,
) -> anyhow::Result<(AgentSession, Vec<String>)> {
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
            handler: Box::new(|args| forge_types::ToolOutput {
                content: args
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
                is_error: false,
                exit_code: None,
            }),
        }],
    );

    let mut startup_notices = cfg.refused_key_notices();
    if !cfg.mcp.servers.is_empty() {
        let mut mgr = McpManager::new();
        let errors = mgr.connect_all(&cfg.mcp.servers).await;
        for e in errors {
            startup_notices.push(format!("mcp: {e}"));
        }
        let _ = mgr.register_into(&mut tools).await;
    }

    let (journal_dir, storage_notices) = resolve_journal_dir(cfg);
    startup_notices.extend(storage_notices);
    let resume_id = match resume {
        Some(Some(session_id)) => Some(session_id),
        Some(None) => None,
        None => None,
    };
    let loop_cfg = LoopConfig {
        max_turns: 128,
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir,
        enable_context_lifecycle: true,
        enable_governance: true,
        web_search: cfg.tools.web_search.clone(),
    };

    let mut session = if let Some(session_id) = resume_id {
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
    }
    Ok((session, startup_notices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn cli_resume_parses_session_id() {
        let session_id = SessionId::new_v4();
        let cli = Cli::try_parse_from(["forge", "--resume", &session_id.to_string()]).unwrap();
        assert_eq!(cli.resume, Some(Some(session_id)));
    }

    #[test]
    fn cli_resume_without_session_id_is_accepted() {
        let cli = Cli::try_parse_from(["forge", "--resume"]).unwrap();
        assert_eq!(cli.resume, Some(None));
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

        let (session, notices) = open_session(&cfg, None).await.unwrap();
        assert!(notices.is_empty());
        assert_eq!(session.session_id.to_string().len(), 36);
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
