//! Process startup: arguments, ownership and trust, the supervisor, then
//! the Tauri event loop. Every refusal happens before a window opens.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use forge_config::{Config, ConfigOverrides};
use forge_session::{LaunchPlan, LaunchTarget, RepositorySupervisor, SupervisorHandle};
use forge_types::SessionId;

use crate::bridge::{self, DesktopState};

/// How long quitting waits for the supervisor before exiting regardless.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub workspace: Option<PathBuf>,
    pub self_test: bool,
}

/// `forge-desktop [WORKSPACE]`, plus `--self-test` in self-test builds.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    for arg in args {
        match arg.as_str() {
            "--self-test" if cfg!(feature = "self-test") => parsed.self_test = true,
            flag if flag.starts_with('-') => return Err(format!("unknown argument `{flag}`")),
            path if parsed.workspace.is_none() => parsed.workspace = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument `{extra}`")),
        }
    }
    Ok(parsed)
}

struct Started {
    supervisor: RepositorySupervisor,
    handle: SupervisorHandle,
    session_id: SessionId,
    initial: forge_session::SessionRuntimeSnapshot,
    /// Self-test only: the temporary workspace, kept alive for the run.
    _scratch: Option<Box<dyn std::any::Any + Send>>,
}

pub fn run() -> i32 {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("forge-desktop: {message}");
            return 2;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("forge-desktop: {error}");
            return 1;
        }
    };
    let started = match runtime.block_on(start(&args)) {
        Ok(started) => started,
        Err(error) => {
            eprintln!("forge-desktop: {error:#}");
            return 1;
        }
    };
    tauri::async_runtime::set(runtime.handle().clone());

    #[cfg(feature = "self-test")]
    if args.self_test {
        // A scripted check must end on its own; never hang CI.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(90));
            eprintln!("SELF_TEST FAIL timed out");
            std::process::exit(1);
        });
    }

    let supervisor = std::sync::Arc::new(tokio::sync::RwLock::new(Some(started.supervisor)));
    #[allow(unused_mut)] // self-test builds attach their probe
    let mut state = DesktopState::new(
        started.handle,
        supervisor.clone(),
        started.session_id,
        &started.initial,
    );
    #[cfg(feature = "self-test")]
    let probe = args
        .self_test
        .then(|| std::sync::Arc::new(crate::self_test::Probe::new()));
    #[cfg(feature = "self-test")]
    {
        state.self_test = probe.clone();
    }
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .manage(state)
        .plugin(navigation_guard())
        .invoke_handler(invoke_handler());
    #[cfg(feature = "self-test")]
    if let Some(probe) = probe {
        let on_load = probe.clone();
        builder = builder
            .setup(move |app| {
                probe.watch_emit(app.handle());
                Ok(())
            })
            .on_page_load(move |webview, payload| {
                if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                    use tauri::Manager;
                    on_load.on_page_load(webview.app_handle(), payload.url());
                }
            });
    }
    let app = builder.build(context());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            eprintln!("forge-desktop: {error}");
            return 1;
        }
    };
    let _scratch = started._scratch;
    app.run(move |_, event| {
        if let tauri::RunEvent::Exit = event {
            let taken = runtime.block_on(async { supervisor.write().await.take() });
            if let Some(supervisor) = taken {
                // Release the repository explicitly, bounded so a provider
                // that never answers cannot keep the app from quitting.
                let shutdown = runtime.block_on(async move {
                    tokio::time::timeout(SHUTDOWN_TIMEOUT, supervisor.shutdown()).await
                });
                match shutdown {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("forge-desktop: shutdown: {error}"),
                    Err(_) => eprintln!("forge-desktop: shutdown timed out"),
                }
            }
        }
    });
    0
}

fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        bridge::subscribe,
        bridge::submit_prompt,
        bridge::stop_turn,
        bridge::approve_once,
        bridge::deny
    ]
}

/// The webview may only ever show Forge's own bundled page: any navigation
/// elsewhere (a link, `location = …`, a form) is refused. External links
/// will open in the system browser through a dedicated command (M2).
fn navigation_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("forge-navigation-guard")
        .on_navigation(|_, url| is_app_url(url))
        .build()
}

pub fn is_app_url(url: &tauri::Url) -> bool {
    match (url.scheme(), url.host_str()) {
        ("tauri", Some("localhost")) => true,
        // Windows and the http-scheme option serve the app from here.
        ("http" | "https", Some("tauri.localhost")) => true,
        _ => false,
    }
}

/// One configuration for every build; self-test builds patch it through
/// `TAURI_CONFIG` (see build.rs).
fn context() -> tauri::Context {
    tauri::generate_context!()
}

async fn start(args: &Args) -> anyhow::Result<Started> {
    if args.self_test {
        return start_self_test().await;
    }
    let workspace = match &args.workspace {
        Some(path) => path.clone(),
        None => std::env::current_dir().context("no current directory")?,
    };
    let cfg = Config::load(ConfigOverrides {
        config_path: None,
        workspace: Some(workspace),
        journal_path: None,
    })
    .map_err(|error| anyhow::anyhow!(error))?;
    let plan = LaunchPlan::prepare(&cfg).await?;
    if plan.needs_trust() {
        anyhow::bail!(
            "{} is not trusted yet. Open it once with `forge` in a terminal and trust it there; \
             the desktop trust prompt arrives in a later release.",
            plan.workspace().display()
        );
    }
    open(cfg, plan, None).await
}

async fn open(
    cfg: Config,
    plan: LaunchPlan,
    scratch: Option<Box<dyn std::any::Any + Send>>,
) -> anyhow::Result<Started> {
    let launched = plan.launch(LaunchTarget::Continue).await?;
    let Some(bootstrap) = launched.bootstrap else {
        anyhow::bail!(
            "the desktop app currently needs a git repository; use `forge` for other folders"
        );
    };
    let session_id = launched.opened.session.session_id;
    let (supervisor, handle) = bootstrap
        .open_with_primary(&cfg, launched.opened.session)
        .await?;
    let initial = supervisor
        .snapshot(session_id)
        .await
        .context("the session is missing from the supervisor")?;
    Ok(Started {
        supervisor,
        handle,
        session_id,
        initial,
        _scratch: scratch,
    })
}

#[cfg(not(feature = "self-test"))]
async fn start_self_test() -> anyhow::Result<Started> {
    unreachable!("--self-test is rejected by parse_args outside self-test builds")
}

/// A throwaway trusted git repository, the offline mock model, and a
/// private trust store: nothing of the user's is touched.
#[cfg(feature = "self-test")]
async fn start_self_test() -> anyhow::Result<Started> {
    let scratch = tempfile::tempdir()?;
    let repo = scratch.path().join("repo");
    std::fs::create_dir_all(&repo)?;
    std::fs::write(repo.join("README.md"), "self-test\n")?;
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "self-test@forge.invalid"][..],
        &["config", "user.name", "Forge Self-Test"][..],
        &["add", "README.md"][..],
        &["commit", "-q", "-m", "init"][..],
    ] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status()
            .context("self-test needs git")?;
        anyhow::ensure!(status.success(), "git {args:?} failed");
    }
    let store = scratch.path().join("trust.toml");
    forge_config::grant_trust_at(&store, &repo)?;
    let mut cfg = Config {
        resolved_workspace: repo.clone(),
        workspace_root: Some(repo.display().to_string()),
        ..Default::default()
    };
    cfg.model.mock = true;
    let plan = LaunchPlan::prepare_with_trust_store(&cfg, Some(store)).await?;
    open(cfg, plan, Some(Box::new(scratch))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn only_the_bundled_app_may_be_navigated_to() {
        for allowed in [
            "tauri://localhost/",
            "tauri://localhost/?selftest=1",
            "http://tauri.localhost/index.html",
        ] {
            assert!(
                is_app_url(&tauri::Url::parse(allowed).unwrap()),
                "{allowed}"
            );
        }
        for refused in [
            "https://example.com/",
            "http://127.0.0.1:8080/",
            "http://localhost/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "tauri://evil.com/",
            "data:text/html,hi",
        ] {
            assert!(
                !is_app_url(&tauri::Url::parse(refused).unwrap()),
                "{refused}"
            );
        }
    }

    #[test]
    fn workspace_argument_is_optional() {
        assert_eq!(parse(&[]).unwrap(), Args::default());
        assert_eq!(
            parse(&["/tmp/repo"]).unwrap().workspace,
            Some(PathBuf::from("/tmp/repo"))
        );
    }

    #[test]
    fn unknown_flags_and_extra_paths_are_refused() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["a", "b"]).is_err());
    }

    #[cfg(not(feature = "self-test"))]
    #[test]
    fn release_builds_reject_self_test() {
        assert!(parse(&["--self-test"]).is_err());
    }

    #[cfg(feature = "self-test")]
    #[test]
    fn self_test_builds_accept_self_test() {
        assert!(parse(&["--self-test"]).unwrap().self_test);
    }
}
