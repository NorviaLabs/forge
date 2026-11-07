//! Forge CLI — launches the desktop app by default; `--tui` opens the
//! full-screen terminal UI.

mod egress_connect;
mod launch;

use std::{io::Read, path::PathBuf, time::Duration};

use clap::{Args, Parser, Subcommand};

use forge_config::{Config, ConfigOverrides};
use forge_session::{
    open_session, run_headless, ApprovalPolicy, ApprovalRequired, LaunchPlan, LaunchTarget,
    SessionTarget,
};
use forge_tui::{
    decide_launch, run_setup, run_tui_supervised, run_tui_with_launch, ExitCode, SetupRequest,
    SetupResult, TuiLaunch, TuiRuntimeConfig,
};
use forge_types::SessionId;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "forge",
    version,
    about = "Forge — an AI coding agent with a desktop app and a terminal workspace.",
    long_about = "Forge — an AI coding agent with a desktop app and a terminal workspace.\n\n\
                  Running `forge` with no arguments opens the desktop app in the current \
                  directory. Pass --tui for the full-screen terminal UI. Sessions are \
                  journalled to .forge/local/sessions in the workspace, so an interrupted one \
                  can be reopened with --continue or --resume.",
    after_help = "`forge` opens the desktop app; `forge --tui` opens the terminal UI. \
                  Press ? inside the terminal UI for keyboard shortcuts."
)]
struct Cli {
    /// Open the terminal UI instead of the desktop app.
    #[arg(long)]
    tui: bool,

    /// Reopen a previous session. Omit the id to pick from a list. Requires --tui.
    #[arg(long = "resume", value_name = "SESSION_ID", num_args = 0..=1)]
    resume: Option<Option<SessionId>>,

    /// Continue the most recent previous session in this workspace. Requires --tui.
    #[arg(long, conflicts_with_all = ["resume", "fork"])]
    r#continue: bool,

    /// Fork a previous session into a new conversation. Requires --tui.
    #[arg(long, value_name = "SESSION_ID", conflicts_with_all = ["resume", "continue"])]
    fork: Option<SessionId>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Args, Debug)]
struct BenchArgs {
    /// Repository the agent is allowed to modify.
    #[arg(long, default_value = ".", value_name = "PATH")]
    workspace: PathBuf,

    /// Directory for the durable Forge session journal.
    #[arg(long, value_name = "PATH")]
    journal: Option<PathBuf>,

    /// Model id as `<provider>/<model>`, e.g. `openai_codex/gpt-5.6-luna`.
    /// The prefix is the provider profile.
    #[arg(long, value_name = "MODEL")]
    model: String,

    /// Wire-level reasoning effort sent to the provider.
    #[arg(long, default_value = "max", value_name = "EFFORT")]
    effort: String,

    /// Approve tool prompts and grant blocked network hosts for the session.
    /// Commands stay sandboxed: a request to run outside the sandbox is
    /// refused. Use only with an isolated evaluation workspace.
    #[arg(long)]
    approve_all: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run one prompt through Forge without starting the terminal UI.
    Bench(BenchArgs),
}

fn main() {
    // Internal helpers run before clap so they stay out of `--help` and
    // skip the logging and runtime setup the TUI needs.
    let mut args = std::env::args_os().skip(1);
    if args
        .next()
        .is_some_and(|arg| arg == egress_connect::SUBCOMMAND)
    {
        let rest: Vec<String> = args.map(|arg| arg.to_string_lossy().into_owned()).collect();
        std::process::exit(egress_connect::run(&rest));
    }

    let cli = Cli::parse();

    // The desktop app is the default frontend. Resume flags are keyed to the
    // TUI, so name the missing flag instead of silently ignoring it.
    if cli.command.is_none() && !cli.tui {
        if needs_tui(&cli) {
            eprintln!(
                "error: --resume, --continue and --fork belong to the terminal frontend; add --tui"
            );
            std::process::exit(2);
        }
        if !launch::display_available() {
            eprintln!(
                "No display available; the Forge GUI needs a desktop session. \
                 Run `forge --tui` for the terminal frontend."
            );
            std::process::exit(1);
        }
        std::process::exit(launch::run(None));
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_ansi(false)
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime");
    let code = runtime.block_on(async move {
        match run(cli).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::Failed
            }
        }
    });
    runtime.shutdown_timeout(Duration::ZERO);
    std::process::exit(code.code());
}

/// A flag only the terminal frontend understands.
fn needs_tui(cli: &Cli) -> bool {
    cli.resume.is_some() || cli.r#continue || cli.fork.is_some()
}

/// The session the flags ask for. First-run launches (setup still pending
/// at start) always open a new session.
fn launch_target(cli: &Cli, allow_resume_picker: bool) -> LaunchTarget {
    if !allow_resume_picker {
        return LaunchTarget::New;
    }
    if let Some(session_id) = cli.fork {
        return LaunchTarget::Fork(session_id);
    }
    if cli.r#continue {
        return LaunchTarget::Continue;
    }
    match cli.resume {
        Some(Some(session_id)) => LaunchTarget::Resume(session_id),
        Some(None) => LaunchTarget::Latest,
        None => LaunchTarget::New,
    }
}

/// How long quitting waits for the supervisor to stop its sessions and
/// release the repository lease before exiting regardless.
const SUPERVISOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a supervisor shutdown, giving up after `timeout`. Returns what to
/// report on stderr, if anything; it never changes the exit code.
async fn bounded_shutdown<E: std::fmt::Display>(
    shutdown: impl std::future::Future<Output = Result<(), E>>,
    timeout: Duration,
) -> Option<String> {
    match tokio::time::timeout(timeout, shutdown).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("shutdown: {error}")),
        Err(_) => Some(format!(
            "shutdown: sessions did not stop within {}s; exiting anyway",
            timeout.as_secs()
        )),
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Some(Command::Bench(args)) => run_bench(args).await,
        None => run_tui(cli).await,
    }
}

async fn run_bench(args: BenchArgs) -> anyhow::Result<ExitCode> {
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    if prompt.trim().is_empty() {
        anyhow::bail!("forge bench expects a non-empty prompt on stdin");
    }

    let cfg = Config::load(ConfigOverrides {
        workspace: Some(args.workspace),
        journal_path: args.journal.as_ref().map(|path| path.display().to_string()),
        ..Default::default()
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    // Config never selects the model. The bench command takes it explicitly
    // so a run is reproducible.
    let profile_id = resolve_profile_id(&args.model)?;

    let opened = open_session(&cfg, SessionTarget::New).await?;
    let notices = opened.notices;
    let mut session = opened.session;
    session.set_active_model(args.model.clone());
    session.set_active_profile_id(profile_id.clone());
    session.set_reasoning_effort(Some(args.effort.clone()));

    let response = match run_headless(
        session,
        &prompt,
        if args.approve_all {
            ApprovalPolicy::ApproveAll
        } else {
            ApprovalPolicy::Ask
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            let Some(approval) = error.downcast_ref::<ApprovalRequired>() else {
                return Err(error);
            };
            let (kind, retry_scope) = match approval.payload.kind() {
                forge_types::ApprovalKind::NetworkHost => (
                    "network_host_denial",
                    "host_grant_stays_filesystem_sandboxed",
                ),
                forge_types::ApprovalKind::SandboxEscalation => (
                    "sandbox_filesystem_denial",
                    "approved_command_retries_unconfined",
                ),
                forge_types::ApprovalKind::Governance => (
                    "governance_approval",
                    "tool_call_requires_operator_approval",
                ),
            };
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "status": "approval_required",
                    "policy": "sandboxed",
                    "approval": {
                        "kind": kind,
                        "tool": approval.payload.tool,
                        "args_redacted": approval.payload.args_redacted,
                        "reason": approval.payload.reason,
                        "failure": approval.payload.failure,
                        "denied_host": approval.payload.denied_host,
                        "sandbox_escalation": approval.payload.sandbox_escalation,
                        "retry_scope": retry_scope,
                    },
                }))?
            );
            return Ok(ExitCode::Failed);
        }
    };

    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "model": args.model,
            "profile_id": profile_id,
            "effort": args.effort,
            "response": response,
            "notices": notices,
        }))?
    );
    Ok(ExitCode::Success)
}

/// The provider profile a `<provider>/<model>` id names. An old prefix
/// (`openai-codex/…`) is refused with the id to use instead.
fn resolve_profile_id(model: &str) -> anyhow::Result<String> {
    if let Some(current) = forge_connect::old_prefix_hint(model) {
        anyhow::bail!("model `{model}` uses an old provider prefix; use `{current}`");
    }
    let Some((prefix, _)) = model.split_once('/') else {
        anyhow::bail!("model `{model}` needs a provider prefix: `<provider>/<model>`");
    };
    let registry = forge_connect::loaded_registry();
    match registry.get(prefix) {
        Some(profile) => Ok(profile.id.clone()),
        None => anyhow::bail!(
            "no provider `{prefix}`; known providers: {}",
            registry.ids().join(", ")
        ),
    }
}

async fn run_tui(cli: Cli) -> anyhow::Result<ExitCode> {
    let overrides = ConfigOverrides {
        config_path: None,
        workspace: None,
        journal_path: None,
    };
    let cfg = Config::load(overrides).map_err(|e| anyhow::anyhow!(e))?;

    let cwd = cfg.workspace_root().to_path_buf();
    // Ownership first: a launch another Forge would make a second writer is
    // refused here, before any setup screen and any trust or theme write.
    let plan = LaunchPlan::prepare(&cfg).await?;
    let trusted_at_start = !plan.needs_trust();
    // One provider registry for the launch decisions below.
    let registry = forge_connect::loaded_registry();
    let provider_connected = forge_connect::has_connected_profile(&registry);
    let decision = decide_launch(
        cfg.tui.theme_committed,
        trusted_at_start,
        provider_connected,
    );

    if decision.run_theme_setup || decision.run_trust_setup {
        match run_setup(SetupRequest {
            run_theme: decision.run_theme_setup,
            run_trust: decision.run_trust_setup,
            cwd: cwd.clone(),
        })
        .map_err(|e| anyhow::anyhow!(e))?
        {
            SetupResult::Canceled => return Ok(ExitCode::Canceled),
            SetupResult::Completed => {}
        }
    }

    let launched = plan
        .launch(launch_target(&cli, decision.allow_resume_picker))
        .await?;
    let opened_session_id = launched.opened.session.session_id;
    let runtime_cwd = launched.opened.session.workspace_root().to_path_buf();
    let startup_notices = launched.notices;
    let bootstrap = launched.bootstrap;
    let fork_from = launched.fork_from;
    let opened = launched.opened;

    let cfg = Config::load(ConfigOverrides {
        config_path: None,
        workspace: None,
        journal_path: None,
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    let last = forge_connect::PreferenceStore::user_default()
        .last_selection_struct(&registry)
        .ok()
        .flatten()
        .filter(|selection| {
            selection
                .profile_id
                .as_deref()
                .and_then(|id| registry.get(id))
                .is_some()
                && provider_connected
        });
    forge_tui::notify::install(cfg.tui.notify);
    forge_tui::git_prefs::install(cfg.tui.smart_commit);
    let runtime = TuiRuntimeConfig {
        model_label: last
            .as_ref()
            .map(|selection| selection.model.clone())
            .unwrap_or_default(),
        provider: last
            .as_ref()
            .map(|_| "native".to_string())
            .unwrap_or_default(),
        cwd: runtime_cwd,
        version: env!("CARGO_PKG_VERSION").into(),
        startup_notices,
        file_icons: cfg.tui.file_icons,
        theme_id: cfg.tui.theme.clone(),
        reduced_motion: cfg.tui.reduced_motion,
    };
    let launch = TuiLaunch {
        startup_items: None,
        onboarding_connect: decision.require_connect,
        ready_placeholder: decision.show_ready_placeholder,
    };
    let summary = match bootstrap {
        Some(bootstrap) => {
            let (supervisor, handle) = bootstrap.open_with_primary(&cfg, opened.session).await?;
            if let Some(source_session_id) = fork_from {
                handle
                    .command(forge_session::SupervisorCommand::ForkSession {
                        session_id: source_session_id,
                    })
                    .await?;
            }
            let session_id = if fork_from.is_some() {
                supervisor
                    .selected_session_id()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("forked session was not selected"))?
            } else {
                opened_session_id
            };
            let initial = supervisor
                .snapshot(session_id)
                .await
                .ok_or_else(|| anyhow::anyhow!("primary session missing from supervisor"))?;
            let summary = run_tui_supervised(initial, runtime, handle, launch).await;
            // Release the repository explicitly rather than relying on
            // process exit, so the lease is provably free once we return.
            // Bounded: a provider that never answers must not keep the
            // terminal from quitting; process exit then frees the lease.
            if let Some(problem) =
                bounded_shutdown(supervisor.shutdown(), SUPERVISOR_SHUTDOWN_TIMEOUT).await
            {
                eprintln!("{problem}");
            }
            summary
        }
        None => run_tui_with_launch(opened.session, runtime, launch).await,
    }
    .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(quit_failure) = &summary.quit_failure {
        eprintln!("quit all sessions: {quit_failure}");
    }
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
    fn the_desktop_app_is_the_default_and_tui_is_opt_in() {
        let cli = Cli::try_parse_from(["forge"]).unwrap();
        assert!(!cli.tui);
        assert!(!needs_tui(&cli));
        assert!(Cli::try_parse_from(["forge", "--tui"]).unwrap().tui);
    }

    #[test]
    fn resume_flags_are_terminal_only() {
        for args in [
            vec!["forge", "--continue"],
            vec!["forge", "--resume"],
            vec!["forge", "--fork", "00000000-0000-0000-0000-000000000000"],
        ] {
            let cli = Cli::try_parse_from(args.iter().copied()).unwrap();
            assert!(needs_tui(&cli), "{args:?} should require --tui");
        }
    }

    #[test]
    fn cli_continue_and_fork_parse() {
        let cli = Cli::try_parse_from(["forge", "--continue"]).unwrap();
        assert!(cli.r#continue);
        let session_id = SessionId::new_v4();
        let cli = Cli::try_parse_from(["forge", "--fork", &session_id.to_string()]).unwrap();
        assert_eq!(cli.fork, Some(session_id));
    }

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

    #[test]
    fn cli_bench_parses_the_native_headless_options() {
        let cli = Cli::try_parse_from([
            "forge",
            "bench",
            "--workspace",
            "/tmp/repo",
            "--journal",
            "/tmp/journal",
            "--model",
            "openai_codex/gpt-5.6-luna",
            "--effort",
            "max",
            "--approve-all",
        ])
        .unwrap();
        let Some(Command::Bench(args)) = cli.command else {
            panic!("expected forge bench subcommand");
        };
        assert_eq!(args.workspace, PathBuf::from("/tmp/repo"));
        assert_eq!(args.journal, Some(PathBuf::from("/tmp/journal")));
        assert_eq!(args.model, "openai_codex/gpt-5.6-luna");
        assert_eq!(args.effort, "max");
        assert!(args.approve_all);
    }

    #[test]
    fn bench_takes_the_profile_from_the_model_prefix() {
        assert_eq!(
            resolve_profile_id("openai_codex/gpt-5.6-luna").unwrap(),
            "openai_codex"
        );
        let old = resolve_profile_id("openai-codex/gpt-5.6-luna").unwrap_err();
        assert!(
            old.to_string().contains("openai_codex/gpt-5.6-luna"),
            "{old}"
        );
        assert!(resolve_profile_id("gpt-5").is_err());
        assert!(resolve_profile_id("nobody/model").is_err());
    }

    #[test]
    fn print_and_approvals_are_not_offered() {
        assert!(Cli::try_parse_from(["forge", "-p", "hi"]).is_err());
        assert!(Cli::try_parse_from(["forge", "--print", "hi"]).is_err());
        assert!(Cli::try_parse_from(["forge", "--approvals", "ask"]).is_err());
    }

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("tracked.txt"), "one\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "forge@example.test"][..],
            &["config", "user.name", "Forge Test"][..],
            &["add", "tracked.txt"][..],
            &["commit", "-q", "-m", "init"][..],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(temp.path())
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        temp
    }

    fn workspace_config(workspace: &std::path::Path) -> Config {
        let mut cfg = Config {
            resolved_workspace: workspace.to_path_buf(),
            workspace_root: Some(workspace.display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = workspace.join("journals").display().to_string();
        cfg
    }

    /// Every file under `dir`, with its bytes, for before/after comparisons.
    fn snapshot(dir: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        files
    }

    fn find_file(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
        snapshot(dir)
            .into_keys()
            .find(|path| path.file_name().is_some_and(|file| file == name))
    }

    const LEASE_HOLDER_ENV: &str = "FORGE_TEST_LEASE_HOLDER_WORKSPACE";

    /// Child half of the two-process test: when the parent sets
    /// `LEASE_HOLDER_ENV`, take the repository lease, say so on stdout and
    /// hold it until stdin closes. Without the variable it does nothing.
    #[tokio::test]
    async fn lease_holder_child() {
        let Ok(workspace) = std::env::var(LEASE_HOLDER_ENV) else {
            return;
        };
        let cfg = workspace_config(std::path::Path::new(&workspace));
        let _owner = forge_session::RepositoryBootstrap::acquire(&cfg)
            .await
            .expect("child takes the lease");
        println!("LEASE_HELD {}", std::process::id());
        let mut sink = String::new();
        let _ = std::io::stdin().read_to_string(&mut sink);
    }

    #[tokio::test]
    async fn second_process_on_an_owned_repository_is_refused_naming_the_owner() {
        use std::io::BufRead;

        let repo = git_repo();
        let cfg = workspace_config(repo.path());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::lease_holder_child", "--nocapture"])
            .env(LEASE_HOLDER_ENV, repo.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let child_pid = loop {
            let line = lines
                .next()
                .expect("child exited before taking the lease")
                .unwrap();
            if let Some(pid) = line.strip_prefix("LEASE_HELD ") {
                break pid.trim().to_string();
            }
        };
        assert_eq!(child_pid, child.id().to_string());
        let control = repo.path().join(".forge");
        let before = snapshot(&control);

        let error = LaunchPlan::prepare(&cfg)
            .await
            .err()
            .expect("an owned repository must refuse, not fall back to a lease-less session");
        let message = format!("{error:#}");
        // Compare while the owner is still running: its own exit legitimately
        // touches the control database.
        let after = snapshot(&control);

        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(
            message.contains("already owns this repository"),
            "refusal must name the ownership conflict: {message}"
        );
        assert!(
            message.contains(&format!("pid {child_pid},")),
            "refusal must name the owning process {child_pid}: {message}"
        );
        assert!(
            !message.contains(&format!("pid {},", std::process::id())),
            "refusal must not name this process: {message}"
        );
        let changed: Vec<_> = before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .collect();
        assert!(
            changed.is_empty(),
            "a refused launch must not change the owner's control directory: {changed:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_lock_file_in_a_repository_is_refused_not_lease_less() {
        use std::os::unix::fs::PermissionsExt;

        let repo = git_repo();
        let cfg = workspace_config(repo.path());
        let owner = forge_session::RepositoryBootstrap::acquire(&cfg)
            .await
            .expect("first owner takes the lease");
        let lock = find_file(&repo.path().join(".forge"), "owner.lock").expect("owner.lock");
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&lock).is_ok() {
            // Running as root: permissions do not apply, nothing to test.
            std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        // The read-only probe in `prepare` cannot read the lock either and
        // defers to the real acquisition, which runs after trust.
        let trust = tempfile::tempdir().unwrap();
        let store = trust.path().join("trust.toml");
        forge_config::grant_trust_at(&store, repo.path()).unwrap();
        let result =
            match forge_session::LaunchPlan::prepare_with_trust_store(&cfg, Some(store)).await {
                Ok(plan) => plan.launch(LaunchTarget::New).await.map(|_| ()),
                Err(error) => Err(error),
            };
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error =
            result.expect_err("an ownership failure other than AlreadyOwned must also refuse");
        let message = format!("{error:#}");
        assert!(message.contains("could not take ownership"), "{message}");
        assert!(
            message.contains(".forge/local"),
            "the refusal must name where the problem is: {message}"
        );
        assert!(
            !repo.path().join("journals").exists(),
            "no lease-less session was opened"
        );
        drop(owner);
    }

    #[tokio::test]
    async fn bounded_shutdown_gives_up_after_the_timeout() {
        let problem = bounded_shutdown(
            std::future::pending::<Result<(), String>>(),
            Duration::from_millis(10),
        )
        .await
        .expect("a shutdown that never finishes must be reported");
        assert!(problem.contains("did not stop"), "{problem}");
        assert_eq!(
            bounded_shutdown(async { Ok::<(), String>(()) }, Duration::from_secs(10)).await,
            None
        );
        assert_eq!(
            bounded_shutdown(async { Err::<(), _>("boom") }, Duration::from_secs(10)).await,
            Some("shutdown: boom".to_string())
        );
    }

    fn target_for(args: &[&str], allow_resume_picker: bool) -> LaunchTarget {
        let cli = Cli::try_parse_from(args).unwrap();
        launch_target(&cli, allow_resume_picker)
    }

    #[test]
    fn flags_map_to_launch_targets() {
        let session_id = SessionId::new_v4();
        let id = session_id.to_string();
        assert_eq!(target_for(&["forge"], true), LaunchTarget::New);
        assert_eq!(
            target_for(&["forge", "--continue"], true),
            LaunchTarget::Continue
        );
        assert_eq!(
            target_for(&["forge", "--resume"], true),
            LaunchTarget::Latest
        );
        assert_eq!(
            target_for(&["forge", "--resume", &id], true),
            LaunchTarget::Resume(session_id)
        );
        assert_eq!(
            target_for(&["forge", "--fork", &id], true),
            LaunchTarget::Fork(session_id)
        );
    }

    #[test]
    fn first_run_launches_ignore_resume_flags() {
        let id = SessionId::new_v4().to_string();
        for args in [
            &["forge", "--continue"][..],
            &["forge", "--resume"][..],
            &["forge", "--resume", &id][..],
            &["forge", "--fork", &id][..],
        ] {
            assert_eq!(target_for(args, false), LaunchTarget::New, "{args:?}");
        }
    }

    /// Bare `--resume` must pick exactly what the in-app resume picker lists
    /// first. Both order by the journal's `.db` modification time, so a
    /// write that only touched the WAL moves neither.
    #[tokio::test]
    async fn latest_matches_the_resume_picker_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config {
            resolved_workspace: dir.path().to_path_buf(),
            workspace_root: Some(dir.path().display().to_string()),
            ..Default::default()
        };
        let journals = dir.path().join("journals");
        cfg.journal.path = journals.display().to_string();
        cfg.model.mock = true;
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(
                open_session(&cfg, SessionTarget::New)
                    .await
                    .unwrap()
                    .session
                    .session_id,
            );
        }
        // Dropped sessions close their journals asynchronously, and the close
        // rewrites the `.db`. Wait (bounded) until every journal is closed
        // (its WAL and shared-memory files are gone) before setting times.
        let closed = |id: &SessionId| {
            !journals.join(format!("{id}.db-wal")).exists()
                && !journals.join(format!("{id}.db-shm")).exists()
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !ids.iter().all(closed) {
            assert!(
                std::time::Instant::now() < deadline,
                "journals never closed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Journal times disagree with creation order: the middle one is newest.
        for (id, age) in [(ids[0], 7200), (ids[1], 0), (ids[2], 3600)] {
            std::fs::File::options()
                .write(true)
                .open(journals.join(format!("{id}.db")))
                .unwrap()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
        // A WAL-only write on the oldest session: an (empty, valid) WAL file
        // newer than every journal.
        std::fs::write(journals.join(format!("{}.db-wal", ids[0])), b"").unwrap();
        let trust = tempfile::tempdir().unwrap();
        let store = trust.path().join("trust.toml");
        forge_config::grant_trust_at(&store, dir.path()).unwrap();

        // Launch first: the picker reads each journal for a title, which can
        // itself move journal times, so it must not run before the choice.
        let launched = forge_session::LaunchPlan::prepare_with_trust_store(&cfg, Some(store))
            .await
            .unwrap()
            .launch(LaunchTarget::Latest)
            .await
            .unwrap();
        let picker_first = forge_tui::resume_session_items(&journals, 10)
            .await
            .unwrap()
            .first()
            .map(|item| item.id.clone())
            .expect("picker lists sessions");
        assert_eq!(launched.opened.session.session_id.to_string(), picker_first);
        assert_eq!(launched.opened.session.session_id, ids[1]);
    }
}
