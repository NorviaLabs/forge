//! Forge CLI — launches the full-screen TUI by default.

use clap::Parser;
use forge_config::{Config, ConfigOverrides};
use forge_session::{open_session, resolve_journal_dir, SessionTarget};
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

    #[tokio::test]
    async fn no_resume_flag_maps_to_a_new_session() {
        let cfg = Config::default();
        let (target, notice) = resolve_target(&cfg, None).await.unwrap();
        assert_eq!(target, SessionTarget::New);
        assert!(notice.is_none());
    }
}
