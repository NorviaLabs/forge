//! Forge CLI — TUI by default; headless `run`, `status`, and `connect`.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use forge_config::{Config, ConfigOverrides};
use forge_connect::{
    builtin_registry, handle_connect_action, ConnectAction, CredentialStore,
};
use forge_core::{AgentSession, LoopConfig};
use forge_mcp::{register_static_mcp, McpManager, StaticMcpTool};
use forge_model::{client_from_config, ModelClient};
use forge_tools::ToolRegistry;
use forge_tui::{run_tui, ExitCode, TuiRuntimeConfig};
use forge_types::SessionStatus;
use forge_workspace::IsolationMode;
use serde_json::json;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(
    name = "forge",
    version,
    about = "Forge AI coding agent",
    long_about = "Open the full-screen TUI with no subcommand.\n\
Headless: forge run \"…\" · forge status · forge connect"
)]
struct Cli {
    /// Config file (else project/user forge.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Workspace root (default: cwd)
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    /// Model id (LiteLLM string), e.g. openai/gpt-4.1-mini
    #[arg(long, global = true)]
    model: Option<String>,
    /// Git worktree isolation for file edits
    #[arg(long, global = true)]
    worktree: bool,
    /// Resume a session by id
    #[arg(long, global = true)]
    resume: Option<Uuid>,
    /// Max agent turns per run/TUI session
    #[arg(long, default_value_t = 32)]
    max_turns: u32,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Headless one-shot (or multi-turn) agent run
    Run {
        /// User prompt
        prompt: String,
    },
    /// Print version, workspace, and model
    Status,
    /// Connect a provider profile (xai | opencode_go | list | status)
    Connect {
        /// Profile id, or list|status|disconnect
        profile: Option<String>,
        /// API key (prefer env; never logged)
        #[arg(long)]
        key: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
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
        config_path: cli.config.clone(),
        workspace: cli.workspace.clone(),
        model_provider: None,
        model_id: cli.model.clone(),
        api_key: None,
        journal_path: None,
    };
    let cfg = Config::load(overrides).map_err(|e| anyhow::anyhow!(e))?;

    match cli.command {
        None => {
            let session =
                open_session(&cfg, cli.max_turns, cli.resume, cli.worktree).await?;
            let runtime = TuiRuntimeConfig {
                model_label: cfg.model.model.clone(),
                provider: cfg.model.provider.as_str().into(),
                cwd: cfg.workspace_root().to_path_buf(),
                version: env!("CARGO_PKG_VERSION").into(),
            };
            let code = run_tui(session, runtime)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            Ok(code)
        }
        Some(Commands::Status) => {
            println!(
                "forge {}\nworkspace {}\nprovider {} model {}",
                env!("CARGO_PKG_VERSION"),
                cfg.workspace_root().display(),
                cfg.model.provider.as_str(),
                cfg.model.model
            );
            Ok(ExitCode::Success)
        }
        Some(Commands::Run { prompt }) => {
            let mut session =
                open_session(&cfg, cli.max_turns, cli.resume, cli.worktree).await?;
            let _resp = session.run_user_message(&prompt).await?;
            print_session_tail(&session);
            println!("session_id={}", session.session_id);
            Ok(exit_for_status(session.status))
        }
        Some(Commands::Connect { profile, key }) => {
            let reg = builtin_registry();
            let store = CredentialStore::user_default();
            let mut active_profile = None;
            let mut active_model = Some(cfg.model.model.clone());
            let action = match profile.as_deref() {
                None | Some("list") => ConnectAction::List,
                Some("status") => ConnectAction::Status,
                Some("disconnect") => ConnectAction::Disconnect { profile_id: None },
                Some(id) => {
                    let oauth_fixture = std::env::var("FORGE_CONNECT_OAUTH_FIXTURE").is_ok();
                    ConnectAction::Connect {
                        profile_id: id.to_string(),
                        api_key: key,
                        oauth_fixture,
                    }
                }
            };
            match handle_connect_action(
                action,
                &reg,
                &store,
                &mut active_profile,
                &mut active_model,
            ) {
                Ok(msg) => {
                    println!("{msg}");
                    if active_profile.is_some() {
                        if let Some(m) = active_model {
                            println!("hint: set FORGE_MODEL_ID={m} or model in forge.toml");
                        }
                    }
                    Ok(ExitCode::Success)
                }
                Err(e) => {
                    eprintln!("{e}");
                    Err(anyhow::anyhow!(e))
                }
            }
        }
    }
}

fn exit_for_status(status: SessionStatus) -> ExitCode {
    match status {
        SessionStatus::Completed => ExitCode::Success,
        SessionStatus::AwaitingHitl => ExitCode::AwaitingHitl,
        SessionStatus::Failed => ExitCode::Failed,
        SessionStatus::Running => ExitCode::Success,
    }
}

fn print_session_tail(session: &AgentSession) {
    for e in session
        .events
        .iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        eprintln!("[{}] {}", e.kind, e.detail);
    }
    if let Some(ref h) = session.pending_hitl {
        println!("pending_hitl tool={} reason={}", h.tool, h.reason);
    }
}

async fn open_session(
    cfg: &Config,
    max_turns: u32,
    resume: Option<Uuid>,
    worktree: bool,
) -> anyhow::Result<AgentSession> {
    let model: Arc<dyn ModelClient> =
        Arc::from(client_from_config(cfg).map_err(|e| anyhow::anyhow!(e))?);

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

    if !cfg.mcp.servers.is_empty() {
        let mut mgr = McpManager::new();
        let errors = mgr.connect_all(&cfg.mcp.servers).await;
        for e in errors {
            eprintln!("mcp: {e}");
        }
        let _ = mgr.register_into(&mut tools).await;
    }

    let loop_cfg = LoopConfig {
        max_turns,
        workspace: cfg.workspace_root().to_path_buf(),
        journal_dir: cfg.journal_dir(),
        isolation: if worktree {
            IsolationMode::Worktree
        } else {
            IsolationMode::Off
        },
        enable_context_lifecycle: true,
        enable_governance: true,
        web_search: cfg.tools.web_search.clone(),
        ..Default::default()
    };

    if let Some(id) = resume {
        AgentSession::resume(loop_cfg, model, tools, id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    } else {
        AgentSession::create(loop_cfg, model, tools)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }
}
