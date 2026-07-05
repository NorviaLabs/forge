//! Forge CLI — Phase 1 + Phase 2 surfaces.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use forge_config::{Config, ConfigOverrides};
use forge_core::{AgentSession, LoopConfig};
use forge_mcp::{register_static_mcp, McpManager, StaticMcpTool};
use forge_model::{client_from_config, ModelClient};
use forge_tools::ToolRegistry;
use forge_connect::{
    builtin_registry, handle_connect_action, ConnectAction, CredentialStore,
};
use forge_tui::{
    help_text, parse_slash, run_tui, ExitCode, SlashCommand, TuiRuntimeConfig, WorktreeAction,
};
use forge_types::{HitlDecision, ModelResponse, SessionStatus};
use forge_workspace::IsolationMode;
use serde_json::json;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(
    name = "forge",
    version,
    about = "Forge AI coding agent — default opens the full-screen TUI",
    long_about = "Run `forge` with no subcommand to open the full-screen terminal UI.\n\
Use subcommands for headless run, REPL, connect, status, and ops tools."
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    #[arg(long, global = true)]
    provider: Option<String>,
    #[arg(long, global = true)]
    model: Option<String>,
    /// Enable git worktree isolation (Phase 2 CTX-03)
    #[arg(long, global = true)]
    worktree: bool,
    /// Resume session (TUI default mode, or with `run` / `repl`)
    #[arg(long, global = true)]
    resume: Option<Uuid>,
    /// Max agent turns for default TUI (subcommand flags override when present)
    #[arg(long, default_value_t = 32)]
    max_turns: u32,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Run {
        prompt: String,
        #[arg(long)]
        resume: Option<Uuid>,
        #[arg(long, default_value_t = 8)]
        max_turns: u32,
    },
    Repl {
        #[arg(long)]
        resume: Option<Uuid>,
        #[arg(long, default_value_t = 16)]
        max_turns: u32,
    },
    Status,
    /// Phase 2: resolve HITL for a resumed session
    Approve {
        #[arg(long)]
        session: Uuid,
    },
    Deny {
        #[arg(long)]
        session: Uuid,
    },
    /// Phase 3: run feedback sensors in workspace (EVAL-01)
    Feedback {
        /// Shell command sensor (repeatable)
        #[arg(long = "sensor", default_value = "echo ok")]
        sensors: Vec<String>,
        /// Criteria text for evaluator
        #[arg(long, default_value = "all sensors pass")]
        criteria: String,
    },
    /// Phase 3: simulate channel ingress with restricted ACL (CH-01)
    Channel {
        /// slack | telegram | webhook
        #[arg(long, default_value = "webhook")]
        kind: String,
        text: String,
    },
    /// Phase 3: load fleet plugins / export demo SIEM (FLEET-01)
    Fleet {
        #[arg(long, default_value_t = true)]
        scim: bool,
        #[arg(long, default_value_t = true)]
        siem: bool,
    },
    /// Phase 6: connect a product provider profile (xai | opencode_go)
    Connect {
        /// Profile id, or list|status|disconnect
        profile: Option<String>,
        /// Optional API key (prefer env / interactive; do not log)
        #[arg(long)]
        key: Option<String>,
        /// Read API key from file (one line)
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .with_writer(io::stderr)
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
        model_provider: cli.provider.clone(),
        model_id: cli.model.clone(),
        api_key: None,
        journal_path: None,
    };
    let cfg = Config::load(overrides).map_err(|e| anyhow::anyhow!(e))?;

    match cli.command {
        // Default: full-screen TUI (`forge` / `forge --resume …`)
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
        Some(Commands::Connect {
            profile,
            key,
            key_file,
        }) => {
            let reg = builtin_registry();
            let store = CredentialStore::user_default();
            let mut active_profile = None;
            let mut active_model = Some(cfg.model.model.clone());
            let action = match profile.as_deref() {
                None | Some("list") => ConnectAction::List,
                Some("status") => ConnectAction::Status,
                Some("disconnect") => ConnectAction::Disconnect { profile_id: None },
                Some(id) => {
                    let api_key = if let Some(k) = key {
                        Some(k)
                    } else if let Some(path) = key_file {
                        Some(
                            std::fs::read_to_string(&path)
                                .map_err(|e| anyhow::anyhow!(e))?
                                .lines()
                                .next()
                                .unwrap_or("")
                                .trim()
                                .to_string(),
                        )
                    } else {
                        None
                    };
                    let oauth_fixture = std::env::var("FORGE_CONNECT_OAUTH_FIXTURE").is_ok();
                    ConnectAction::Connect {
                        profile_id: id.to_string(),
                        api_key,
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
                    // OAuth pending prints instructions and non-zero
                    eprintln!("{e}");
                    Err(anyhow::anyhow!(e))
                }
            }
        }
        Some(Commands::Run {
            prompt,
            resume,
            max_turns,
        }) => {
            let resume = resume.or(cli.resume);
            let mut session =
                open_session(&cfg, max_turns, resume, cli.worktree).await?;
            let _resp = session.run_user_message(&prompt).await?;
            print_session_tail(&session);
            println!("session_id={}", session.session_id);
            Ok(exit_for_status(session.status))
        }
        Some(Commands::Repl {
            resume,
            max_turns,
        }) => {
            let resume = resume.or(cli.resume);
            let mut session =
                open_session(&cfg, max_turns, resume, cli.worktree).await?;
            println!("Forge REPL — session {}", session.session_id);
            println!("{}", help_text());
            let connect_reg = builtin_registry();
            let connect_store = CredentialStore::user_default();
            let mut connect_profile: Option<String> = None;
            let mut connect_model: Option<String> = Some(cfg.model.model.clone());
            let stdin = io::stdin();
            let mut stdout = io::stdout();
            for line in stdin.lock().lines() {
                let line = line?;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(cmd_res) = parse_slash(line) {
                    match cmd_res {
                        Ok(SlashCommand::Quit) => break,
                        Ok(SlashCommand::Help { .. }) => print!("{}", help_text()),
                        Ok(SlashCommand::Status) => {
                            println!(
                                "session={} status={:?} tools={} ctx_usage={:.1}% hitl={:?}",
                                session.session_id,
                                session.status,
                                session.list_tools().len(),
                                session.context_usage_ratio() * 100.0,
                                session.pending_hitl.as_ref().map(|p| &p.tool)
                            );
                        }
                        Ok(SlashCommand::Tools) => {
                            for n in session.list_tools() {
                                println!("  {n}");
                            }
                        }
                        Ok(SlashCommand::Resume { session_id }) => {
                            session = open_session(
                                &cfg,
                                max_turns,
                                Some(session_id),
                                cli.worktree,
                            )
                            .await?;
                            println!("resumed {}", session.session_id);
                        }
                        Ok(SlashCommand::Cancel) => println!("cancel acknowledged"),
                        Ok(SlashCommand::Model { provider, model }) => {
                            println!("model switch requested provider={provider:?} model={model:?}");
                        }
                        Ok(SlashCommand::Connect(action)) => {
                            match handle_connect_action(
                                action,
                                &connect_reg,
                                &connect_store,
                                &mut connect_profile,
                                &mut connect_model,
                            ) {
                                Ok(msg) => println!("{msg}"),
                                Err(e) => println!("{e}"),
                            }
                        }
                        Ok(SlashCommand::Journal { tail }) => {
                            let n = tail.unwrap_or(10);
                            for e in session
                                .events
                                .iter()
                                .rev()
                                .take(n)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                            {
                                println!("  [{}] {}", e.kind, e.detail);
                            }
                        }
                        Ok(SlashCommand::Approve) => {
                            session
                                .resolve_hitl(HitlDecision::Approve, "tui")
                                .await?;
                            println!("approved");
                        }
                        Ok(SlashCommand::Deny) => {
                            session.resolve_hitl(HitlDecision::Deny, "tui").await?;
                            println!("denied");
                        }
                        Ok(SlashCommand::Reset) | Ok(SlashCommand::Compact) => {
                            session.force_context_reset_async().await?;
                            println!("context reset + progress.json written");
                        }
                        Ok(SlashCommand::Cost) => {
                            println!(
                                "context usage ratio: {:.2}%",
                                session.context_usage_ratio() * 100.0
                            );
                        }
                        Ok(SlashCommand::Worktree { action }) => match action {
                            WorktreeAction::Status => {
                                println!(
                                    "{}",
                                    session
                                        .worktree_status()
                                        .unwrap_or_else(|| "worktree off".into())
                                );
                            }
                            WorktreeAction::Merge => {
                                session.worktree_merge()?;
                                println!("worktree merged");
                            }
                            WorktreeAction::Discard { confirm } => {
                                if !confirm {
                                    println!("confirm with /worktree discard --yes");
                                } else {
                                    session.worktree_discard()?;
                                    println!("worktree discarded");
                                }
                            }
                        },
                        Err(e) => println!("{e}"),
                    }
                    stdout.flush()?;
                    continue;
                }
                match session.run_user_message(line).await {
                    Ok(resp) => {
                        print_response(&resp);
                        if session.status == SessionStatus::AwaitingHitl {
                            println!("(awaiting HITL — /approve or /deny)");
                        }
                    }
                    Err(e) => println!("error: {e}"),
                }
                stdout.flush()?;
            }
            Ok(ExitCode::Success)
        }
        Some(Commands::Approve { session }) => {
            let mut s = open_session(&cfg, 8, Some(session), cli.worktree).await?;
            s.resolve_hitl(HitlDecision::Approve, "cli").await?;
            println!("approved");
            Ok(ExitCode::Success)
        }
        Some(Commands::Deny { session }) => {
            let mut s = open_session(&cfg, 8, Some(session), cli.worktree).await?;
            s.resolve_hitl(HitlDecision::Deny, "cli").await?;
            println!("denied");
            Ok(ExitCode::Success)
        }
        Some(Commands::Feedback { sensors, criteria }) => {
            use forge_feedback::{
                CommandSensor, FeedbackConfig, FeedbackGate, SensorContext,
            };
            use std::sync::Arc;
            let mut gate = FeedbackGate::new(FeedbackConfig {
                enabled: true,
                evaluator_enabled: true,
                sensor_commands: sensors.clone(),
                ..Default::default()
            });
            for s in &sensors {
                gate = gate.with_sensor(Arc::new(CommandSensor::new(s.clone())));
            }
            let out = gate
                .run_gate(
                    &SensorContext {
                        workspace: cfg.workspace_root().to_path_buf(),
                    },
                    &criteria,
                )
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "feedback passed={} repairs={} sensors={}",
                out.passed,
                out.repairs.len(),
                out.sensor_reports.len()
            );
            for r in &out.sensor_reports {
                println!("  sensor={} status={:?} {}", r.sensor, r.status, r.summary);
            }
            for t in &out.repairs {
                println!("  repair: {} — {}", t.sensor, t.summary);
            }
            Ok(if out.passed {
                ExitCode::Success
            } else {
                ExitCode::Failed
            })
        }
        Some(Commands::Channel { kind, text }) => {
            use forge_channels::{ChannelGateway, ChannelKind, ChannelMessage};
            let kind = match kind.to_ascii_lowercase().as_str() {
                "slack" => ChannelKind::Slack,
                "telegram" => ChannelKind::Telegram,
                _ => ChannelKind::Webhook,
            };
            let model: Arc<dyn ModelClient> = Arc::from(
                client_from_config(&cfg).map_err(|e| anyhow::anyhow!(e))?,
            );
            let gw = ChannelGateway::new(
                cfg.workspace_root().to_path_buf(),
                cfg.journal_dir(),
                model,
            );
            let resp = gw
                .handle_message(ChannelMessage {
                    channel: kind,
                    channel_id: "cli".into(),
                    user_id: "operator".into(),
                    text,
                    thread_id: None,
                })
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("session_id={}", resp.session_id);
            println!("tools_visible={:?}", resp.tools_visible);
            println!("{}", resp.text);
            assert!(
                !resp.tools_visible.iter().any(|t| t == "bash" || t == "write_file"),
                "channel must not expose broad tools"
            );
            Ok(ExitCode::Success)
        }
        Some(Commands::Fleet { scim, siem }) => {
            use forge_fleet::{FleetConfig, FleetPluginRegistry, ScimUser, SiemEncoding};
            use forge_governance::{AuditEvent, AuditLog};
            use forge_types::PolicyDecision;
            let dir = cfg.workspace_root().join(".forge/fleet");
            let reg = FleetPluginRegistry::load(&FleetConfig {
                scim_enabled: scim,
                siem_enabled: siem,
                siem_path: Some(dir.join("siem.jsonl")),
                siem_encoding: SiemEncoding::JsonlOtlp,
            })
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("plugins={:?}", reg.list_plugins());
            if let Some(mut scim_p) = reg.scim {
                scim_p
                    .create_user(ScimUser {
                        id: "demo".into(),
                        user_name: "demo".into(),
                        active: true,
                        roles: vec!["dev".into()],
                    })
                    .map_err(|e| anyhow::anyhow!(e))?;
                println!("scim: provisioned user demo");
            }
            if let Some(siem_p) = reg.siem {
                let log = AuditLog::default();
                log.push(AuditEvent {
                    session_id: "demo".into(),
                    principal: "cli".into(),
                    tool: "status".into(),
                    args_redacted: json!({}),
                    decision: PolicyDecision::Allow,
                    policy_id: "default".into(),
                    result: "ok".into(),
                    duration_ms: 1,
                    trace_id: None,
                });
                let n = siem_p
                    .export_audit(&log.snapshot())
                    .map_err(|e| anyhow::anyhow!(e))?;
                println!("siem: exported {n} events");
            }
            // Optional obs export demo
            use forge_obs::{init, OtelConfig};
            let obs = init(&OtelConfig {
                enabled: true,
                endpoint: None,
                export_path: Some(dir.join("otel.jsonl")),
            });
            let sid = obs.start_session("demo", "cli", &cfg.model.model);
            let turn = obs.start_turn(&sid, 0);
            let m = obs.start_model(&turn, cfg.model.provider.as_str(), &cfg.model.model);
            obs.end_model(&m, 1, 0, 0);
            let path = dir.join("otel.jsonl");
            let n = obs.export_jsonl_file(&path).map_err(|e| anyhow::anyhow!(e))?;
            println!("obs: exported {n} records to {}", path.display());
            Ok(ExitCode::Success)
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
    for e in session.events.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev() {
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

    let session = if let Some(id) = resume {
        AgentSession::resume(loop_cfg, model, tools, id).await?
    } else {
        AgentSession::create(loop_cfg, model, tools).await?
    };
    Ok(session)
}

fn print_response(resp: &ModelResponse) {
    if !resp.text.is_empty() {
        println!("{}", resp.text);
    }
    for tc in &resp.tool_calls {
        println!("[tool_call] {} {}", tc.name, tc.arguments);
    }
}
