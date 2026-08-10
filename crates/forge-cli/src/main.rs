//! Forge CLI — launches the full-screen TUI by default.

use std::io::{self, Write};

use clap::Parser;
use forge_config::{Config, ConfigOverrides};
use forge_core::AgentSession;
use forge_session::{
    open_session, resolve_journal_dir, ApprovalPolicy, SessionCommand, SessionEvent, SessionTarget,
};
use forge_tui::{resume_session_items, run_tui, ExitCode, TuiRuntimeConfig};
use forge_types::SessionId;
use tracing_subscriber::EnvFilter;

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

    /// Run one prompt without the TUI, stream the answer to stdout, and exit.
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    print: Option<String>,

    /// What to do when a tool call needs approval in `--print` mode.
    #[arg(long, value_enum, default_value_t = Approvals::Ask)]
    approvals: Approvals,
}

/// Mirrors `forge_session::ApprovalPolicy` as a CLI-facing enum, so the flag
/// vocabulary can differ from the library's without either constraining the
/// other.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum Approvals {
    /// Stop and report the request. Nothing runs without a human.
    Ask,
    /// Deny every request; the agent continues and may adapt.
    Deny,
    /// Approve every request. Runs model-authored commands unattended.
    Approve,
}

impl From<Approvals> for ApprovalPolicy {
    fn from(value: Approvals) -> Self {
        match value {
            Approvals::Ask => ApprovalPolicy::Ask,
            Approvals::Deny => ApprovalPolicy::DenyAll,
            Approvals::Approve => ApprovalPolicy::ApproveAll,
        }
    }
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

/// Turn `--resume` into a concrete target. A bare `--resume` means "the most
/// recent session", which requires looking one up; if there is none, fall back
/// to a new session and say so.
async fn resolve_target(
    cfg: &Config,
    resume: Option<Option<SessionId>>,
) -> anyhow::Result<(SessionTarget, Option<String>)> {
    match resume {
        Some(Some(session_id)) => Ok((SessionTarget::Resume(session_id), None)),
        Some(None) => {
            let (journal_dir, _) = resolve_journal_dir(cfg);
            let items = resume_session_items(&journal_dir, 10)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            match items.first() {
                Some(item) => {
                    let session_id = item
                        .id
                        .parse::<SessionId>()
                        .map_err(|e| anyhow::anyhow!(e))?;
                    Ok((SessionTarget::Resume(session_id), None))
                }
                None => Ok((
                    SessionTarget::New,
                    Some("No previous session found; creating a new session.".to_string()),
                )),
            }
        }
        None => Ok((SessionTarget::New, None)),
    }
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

    let (target, create_notice) = resolve_target(&cfg, cli.resume).await?;
    let opened = open_session(&cfg, target).await?;
    let mut startup_notices = opened.notices;
    if let Some(notice) = create_notice {
        startup_notices.push(notice);
    }

    if let Some(prompt) = cli.print {
        for notice in &startup_notices {
            eprintln!("note: {notice}");
        }
        return run_headless(opened.session, prompt, cli.approvals.into()).await;
    }

    let runtime = TuiRuntimeConfig {
        model_label: cfg.model.model.clone(),
        provider: cfg.model.provider.as_str().into(),
        cwd: cfg.workspace_root().to_path_buf(),
        version: env!("CARGO_PKG_VERSION").into(),
        startup_notices,
        file_icons: cfg.tui.file_icons,
        theme_id: cfg.tui.theme.clone(),
    };
    let summary = run_tui(opened.session, runtime)
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

/// Drive one prompt to completion with no terminal attached.
///
/// Assistant text goes to stdout as it streams; everything else (notices,
/// approval requests, errors) goes to stderr, so the stdout of a `--print`
/// run is just the answer and can be piped.
async fn run_headless(
    session: AgentSession,
    prompt: String,
    policy: ApprovalPolicy,
) -> anyhow::Result<ExitCode> {
    let mut handle = forge_session::spawn(session, policy);
    handle.send(SessionCommand::Prompt(prompt)).await?;

    let mut stdout = io::stdout();
    let exit = loop {
        let Some(event) = handle.next_event().await else {
            // The runner stopped without reporting an outcome.
            break ExitCode::Failed;
        };
        match event {
            SessionEvent::TextDelta(text) => {
                write!(stdout, "{text}")?;
                stdout.flush()?;
            }
            // Reasoning and tool activity are progress, not output.
            SessionEvent::ThinkingDelta(_) => {}
            SessionEvent::ToolCall { name, .. } => eprintln!("tool: {name}"),
            SessionEvent::TurnComplete { .. } => {
                writeln!(stdout)?;
                break ExitCode::Success;
            }
            SessionEvent::AwaitingApproval(payload) => {
                eprintln!(
                    "awaiting approval: {} — {}\n\
                     re-run with --approvals approve to allow, or --approvals deny to refuse",
                    payload.tool, payload.reason
                );
                break ExitCode::AwaitingHitl;
            }
            SessionEvent::Error(message) => {
                eprintln!("error: {message}");
                break ExitCode::Failed;
            }
        }
    };

    let _ = handle.send(SessionCommand::Shutdown).await;
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// An explicit session id needs no journal lookup, so this maps straight
    /// through without touching the filesystem.
    #[tokio::test]
    async fn explicit_resume_id_maps_to_a_resume_target() {
        let session_id = SessionId::new_v4();
        let cfg = Config::default();
        let (target, notice) = resolve_target(&cfg, Some(Some(session_id))).await.unwrap();
        assert_eq!(target, SessionTarget::Resume(session_id));
        assert!(notice.is_none());
    }

    /// Approval is the one flag where a wrong default is dangerous, so pin
    /// both the default and every mapping.
    #[test]
    fn approvals_defaults_to_ask_and_maps_to_the_library_policy() {
        let cli = Cli::try_parse_from(["forge", "-p", "hi"]).unwrap();
        assert_eq!(cli.approvals, Approvals::Ask);
        assert_eq!(ApprovalPolicy::from(cli.approvals), ApprovalPolicy::Ask);

        for (flag, expected) in [
            ("ask", ApprovalPolicy::Ask),
            ("deny", ApprovalPolicy::DenyAll),
            ("approve", ApprovalPolicy::ApproveAll),
        ] {
            let cli = Cli::try_parse_from(["forge", "-p", "hi", "--approvals", flag]).unwrap();
            assert_eq!(ApprovalPolicy::from(cli.approvals), expected, "flag {flag}");
        }
    }

    #[test]
    fn print_is_absent_unless_asked_for() {
        assert!(Cli::try_parse_from(["forge"]).unwrap().print.is_none());
        assert_eq!(
            Cli::try_parse_from(["forge", "-p", "do a thing"])
                .unwrap()
                .print
                .as_deref(),
            Some("do a thing")
        );
    }

    #[tokio::test]
    async fn no_resume_flag_maps_to_a_new_session() {
        let cfg = Config::default();
        let (target, notice) = resolve_target(&cfg, None).await.unwrap();
        assert_eq!(target, SessionTarget::New);
        assert!(notice.is_none());
    }
}
