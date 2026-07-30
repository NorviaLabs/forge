//! Forge CLI — launches the full-screen TUI by default.

use std::sync::Arc;

use clap::Parser;
use forge_config::{Config, ConfigOverrides};
use forge_core::{AgentSession, LoopConfig};
use forge_mcp::{register_static_mcp, McpManager, StaticMcpTool};
use forge_model::{client_from_config, ModelClient};
use forge_tools::ToolRegistry;
use forge_tui::{run_tui, ExitCode, TuiRuntimeConfig};
use forge_types::SessionId;
use serde_json::json;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "forge",
    version,
    about = "Forge AI coding agent",
    long_about = "Open the full-screen TUI by default.\n\nUse --help or --version for CLI info."
)]
struct Cli {
    #[arg(long = "resume", value_name = "SESSION_ID")]
    resume: Option<SessionId>,
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

    let (session, startup_notices) = open_session(&cfg, cli.resume).await?;
    let runtime = TuiRuntimeConfig {
        model_label: cfg.model.model.clone(),
        provider: cfg.model.provider.as_str().into(),
        cwd: cfg.workspace_root().to_path_buf(),
        version: env!("CARGO_PKG_VERSION").into(),
        startup_notices,
        validation_command: cfg.validation.command.clone(),
        file_icons: cfg.tui.file_icons,
        mouse_capture: cfg.tui.mouse_capture,
        theme: cfg.tui.theme,
    };
    let summary = run_tui(session, runtime)
        .await
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
    resume: Option<SessionId>,
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

    let loop_cfg = LoopConfig {
        max_turns: 128,
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir: cfg.journal_dir(),
        enable_context_lifecycle: true,
        enable_governance: true,
        web_search: cfg.tools.web_search.clone(),
    };

    let mut session = AgentSession::create(loop_cfg, model, tools)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(session_id) = resume {
        session
            .resume_session(session_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
    }
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
        assert_eq!(cli.resume, Some(session_id));
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

    #[tokio::test]
    async fn open_session_with_mock_model_builds_a_session_without_notices() {
        let temp = TempDir::new().unwrap();
        let mut cfg = Config::default();
        cfg.model.provider = forge_config::ModelProviderKind::Mock;
        cfg.model.model = "mock".into();
        cfg.resolved_workspace = temp.path().to_path_buf();
        cfg.workspace_root = Some(temp.path().display().to_string());

        let (session, notices) = open_session(&cfg, None).await.unwrap();
        assert!(notices.is_empty());
        assert_eq!(session.session_id.to_string().len(), 36);
    }
}
