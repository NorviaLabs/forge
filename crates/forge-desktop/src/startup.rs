//! Process startup: arguments, the launch project (through the project
//! service: registry, trust, owner lock), then the Tauri event loop.
//! Untrusted or unregistered folders wait behind the trust screen.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
#[cfg(feature = "self-test")]
use forge_config::Config;
use forge_session::{LaunchResolution, ProjectError, ProjectService, ProjectServiceOptions};

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

pub(crate) struct Started {
    pub(crate) projects: forge_session::ProjectServiceHandle,
    /// The project the window opens on.
    pub(crate) active: forge_session::OpenProjectRef,
    /// Self-test only: the temporary workspace, kept alive for the run.
    pub(crate) _scratch: Option<Box<dyn std::any::Any + Send>>,
    /// Self-test only: the throwaway repository the journey acts on.
    #[cfg(feature = "self-test")]
    pub(crate) self_test_repo: Option<PathBuf>,
}

/// What startup produced: an open project, or something the operator must
/// decide first in the window. Nothing is written into a project folder
/// before they do.
pub enum Startup {
    Ready(Box<Started>),
    /// Trust a registered project, or confirm adding a plain folder.
    NeedsTrust {
        projects: forge_session::ProjectServiceHandle,
        workspace: PathBuf,
        pending: bridge::Pending,
    },
    /// Launched with no path from `/` or the home folder (the Dock, Finder):
    /// neither is a project, so the page asks for a folder first.
    NeedsFolder {
        projects: forge_session::ProjectServiceHandle,
    },
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
    let startup = match runtime.block_on(start(&args)) {
        Ok(startup) => startup,
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

    // The page asks `trust_status` first: an untrusted workspace gets the
    // trust screen, and `trust_workspace` starts the session in place.
    let (trust, ready) = match startup {
        Startup::Ready(started) => (
            bridge::TrustState::ready(
                started.projects.clone(),
                started.active.project.root.clone(),
            ),
            Some(started),
        ),
        Startup::NeedsTrust {
            projects,
            workspace,
            pending,
        } => (
            bridge::TrustState::pending(projects, workspace, pending),
            None,
        ),
        Startup::NeedsFolder { projects } => (bridge::TrustState::needs_folder(projects), None),
    };
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .manage(trust)
        .plugin(navigation_guard())
        .invoke_handler(invoke_handler());
    #[cfg(feature = "self-test")]
    let mut probe = None;
    let mut _scratch: Option<Box<dyn std::any::Any + Send>> = None;
    if let Some(started) = ready {
        let started = *started;
        _scratch = started._scratch;
        #[allow(unused_mut)] // self-test builds attach their probe
        let mut state = DesktopState::new(started.projects, started.active);
        #[cfg(feature = "self-test")]
        {
            probe = started
                .self_test_repo
                .clone()
                .map(|repo| std::sync::Arc::new(crate::self_test::Probe::new(repo)));
            state.self_test = probe.clone();
        }
        builder = builder.manage(state);
    }
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
    let runtime_handle = runtime.handle().clone();
    app.run(move |handle, event| {
        match &event {
            tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "main" && quit_risk(handle) => {
                api.prevent_close();
                ask_before_quitting(handle);
            }
            tauri::RunEvent::ExitRequested { api, .. } if quit_risk(handle) => {
                api.prevent_exit();
                ask_before_quitting(handle);
            }
            _ => {}
        }
        if let tauri::RunEvent::Exit = event {
            use tauri::Manager;
            // No `DesktopState` yet means the trust screen never got past
            // itself; there is no session and nothing to release.
            let Some(state) = handle.try_state::<DesktopState>() else {
                return;
            };
            state.shells.close_all();
            // Release every project explicitly, bounded so a provider that
            // never answers cannot keep the app from quitting.
            let projects = state.projects.clone();
            let shutdown = runtime_handle.block_on(async move {
                tokio::time::timeout(SHUTDOWN_TIMEOUT, projects.shutdown()).await
            });
            match shutdown {
                Ok(Ok(notices)) => {
                    for notice in notices {
                        eprintln!("forge-desktop: shutdown: {notice}");
                    }
                }
                Ok(Err(error)) => eprintln!("forge-desktop: shutdown: {error}"),
                Err(_) => eprintln!("forge-desktop: shutdown timed out"),
            }
        }
    });
    // Provider/device-login workers must not hold process exit for minutes.
    // The supervisor's durable work was already shut down above.
    runtime.shutdown_background();
    0
}

/// Unsaved editor buffers or live turns, and no confirmation yet: a close
/// must not discard either silently.
fn quit_risk(handle: &tauri::AppHandle) -> bool {
    use std::sync::atomic::Ordering;
    use tauri::Manager;
    let Some(state) = handle.try_state::<DesktopState>() else {
        return false;
    };
    // `busy` from the page covers only the project on screen; agents in
    // background projects are stopped by quitting too, so they count.
    let busy = state.busy.load(Ordering::Relaxed)
        || state.projects.workspace().borrow().has_busy_sessions();
    (state.unsaved.load(Ordering::Relaxed) || busy) && !state.quitting.load(Ordering::Relaxed)
}

/// The page shows its discard dialog; `quit_app` ends the process if the
/// user goes through with it.
fn ask_before_quitting(handle: &tauri::AppHandle) {
    use tauri::Emitter;
    let _ = handle.emit("close_requested", ());
}

fn invoke_handler() -> impl Fn(tauri::ipc::Invoke) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        bridge::trust_status,
        bridge::trust_workspace,
        bridge::choose_folder,
        bridge::list_projects,
        bridge::open_project,
        bridge::project_switch,
        bridge::project_add,
        bridge::project_trust,
        bridge::project_close,
        bridge::project_remove,
        bridge::project_relocate,
        bridge::subscribe,
        bridge::current_session,
        bridge::submit_prompt,
        bridge::submit_images,
        bridge::decide_approval,
        bridge::answer_question,
        bridge::answer_host_question,
        bridge::set_approve_all,
        bridge::clear_approvals,
        bridge::continue_turn,
        bridge::stop_session,
        bridge::close_session,
        bridge::fork_session,
        bridge::resume_session,
        bridge::list_resumable_sessions,
        bridge::rename_session,
        bridge::pin_session,
        bridge::cancel_queued,
        bridge::compact_context,
        bridge::set_thinking,
        bridge::cancel_background,
        bridge::resolve_child_approval,
        bridge::child_transcript,
        bridge::message_detail,
        bridge::create_session,
        bridge::select_session,
        bridge::archive_session,
        bridge::attach_worktree,
        bridge::remove_worktree,
        bridge::list_dir,
        bridge::find_files,
        bridge::read_file,
        bridge::read_image,
        bridge::git_status,
        bridge::github_issues,
        bridge::github_details,
        bridge::github_logs,
        bridge::github_create_pr,
        bridge::github_refresh_pr,
        bridge::github_apply_feedback,
        bridge::git_diff,
        bridge::git_operation,
        bridge::write_file,
        bridge::create_file,
        bridge::create_dir,
        bridge::rename_path,
        bridge::delete_path,
        bridge::search_files,
        bridge::git_stage,
        bridge::git_unstage,
        bridge::git_commit,
        bridge::git_branches,
        bridge::git_switch_branch,
        bridge::git_create_branch,
        bridge::git_merge_branch,
        bridge::git_rename_branch,
        bridge::set_unsaved,
        bridge::quit_app,
        bridge::git_restore,
        bridge::git_discard_hunk,
        bridge::git_last_turn_files,
        bridge::list_models,
        bridge::session_details,
        bridge::list_providers,
        bridge::list_themes,
        bridge::sandbox_grants,
        bridge::revoke_sandbox_grant,
        bridge::list_agents,
        bridge::save_agent,
        bridge::delete_agent,
        bridge::read_agent_source,
        bridge::save_agent_source,
        bridge::list_agent_tools,
        bridge::connect_provider,
        bridge::import_provider_login,
        bridge::disconnect_provider,
        bridge::add_model,
        bridge::save_custom_provider,
        bridge::update_custom_provider,
        bridge::remove_custom_provider,
        bridge::set_model,
        bridge::shell_open,
        bridge::shell_write,
        bridge::shell_resize,
        bridge::shell_close
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

/// A GUI launch (Dock, Finder) starts in `/` or the home folder. Neither is a
/// project, and opening either would hand the agent the whole disk or home.
fn is_not_a_project(cwd: &std::path::Path, home: Option<&std::path::Path>) -> bool {
    cwd.parent().is_none() || home.is_some_and(|home| cwd == home)
}

async fn start(args: &Args) -> anyhow::Result<Startup> {
    if args.self_test {
        return Ok(Startup::Ready(Box::new(start_self_test().await?)));
    }
    let projects = ProjectService::spawn(ProjectServiceOptions::user()?)?;
    let folder = match &args.workspace {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => std::env::current_dir()
            .context("no current directory")?
            .join(path),
        None => {
            let cwd = std::env::current_dir().context("no current directory")?;
            if is_not_a_project(&cwd, dirs::home_dir().as_deref()) {
                return Ok(Startup::NeedsFolder { projects });
            }
            cwd
        }
    };
    match projects.resolve_launch(folder).await? {
        LaunchResolution::Project(project) => open_or_trust(projects, project, None).await,
        // Registering a plain folder lets agents edit everything in it, so
        // the window asks first (D5), on the same screen as trust.
        LaunchResolution::NeedsConfirmation { root } => Ok(Startup::NeedsTrust {
            projects,
            workspace: root.clone(),
            pending: bridge::Pending::Confirm(root),
        }),
    }
}

/// Open `project`, or hand it to the trust screen when it isn't trusted.
async fn open_or_trust(
    projects: forge_session::ProjectServiceHandle,
    project: forge_session::Project,
    scratch: Option<Box<dyn std::any::Any + Send>>,
) -> anyhow::Result<Startup> {
    match projects.open(project.id).await {
        Ok(_) => Ok(Startup::Ready(Box::new(Started {
            active: projects.project(project.id).await?,
            projects,
            _scratch: scratch,
            #[cfg(feature = "self-test")]
            self_test_repo: None,
        }))),
        Err(ProjectError::NeedsTrust(_)) => Ok(Startup::NeedsTrust {
            projects,
            workspace: project.root,
            pending: bridge::Pending::Trust(project.id),
        }),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(feature = "self-test"))]
async fn start_self_test() -> anyhow::Result<Started> {
    unreachable!("--self-test is rejected by parse_args outside self-test builds")
}

/// A throwaway trusted git repository, the scripted fake provider, and a
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
    // Fixtures for the approval journey (see `self_test::SCENARIO`): the
    // approved `git reset --hard` restores README.md, and the denied
    // `git clean -f` must leave the untracked file alone.
    std::fs::write(repo.join("README.md"), "modified\n")?;
    std::fs::write(repo.join(crate::self_test::UNTRACKED), "keep\n")?;
    let scenario = scratch.path().join("scenario.json");
    std::fs::write(&scenario, crate::self_test::SCENARIO)?;
    let store = scratch.path().join("trust.toml");
    forge_config::grant_trust_at(&store, &repo)?;
    let mut cfg = Config {
        resolved_workspace: repo.clone(),
        workspace_root: Some(repo.display().to_string()),
        ..Default::default()
    };
    cfg.model.mock = true;
    cfg.model.mock_script = Some(scenario);
    let projects = ProjectService::spawn(ProjectServiceOptions {
        registry: forge_session::ProjectRegistry::at(scratch.path().join("projects.toml")),
        trust_store: Some(store),
        load_config: std::sync::Arc::new(move |_root| Ok(cfg.clone())),
    })?;
    let project = projects.add(repo.clone()).await?;
    let Startup::Ready(mut started) =
        open_or_trust(projects, project, Some(Box::new(scratch))).await?
    else {
        anyhow::bail!("the self-test repository should be trusted");
    };
    started.self_test_repo = Some(repo);
    Ok(*started)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn root_and_home_are_not_opened_without_a_path() {
        let home = std::path::Path::new("/Users/someone");
        assert!(is_not_a_project(std::path::Path::new("/"), Some(home)));
        assert!(is_not_a_project(home, Some(home)));
        assert!(!is_not_a_project(&home.join("notes"), Some(home)));
        assert!(!is_not_a_project(std::path::Path::new("/work/repo"), None));
    }

    #[test]
    fn config_comes_from_the_workspace_not_the_launch_directory() {
        let project =
            std::env::temp_dir().join(format!("forge-desktop-config-{}", uuid_like_suffix()));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("forge.toml"),
            "[model]\nrequest_timeout_secs = 91\n",
        )
        .unwrap();

        // Read no real user config: it is the developer's, not the fixture's.
        let home = project.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let saved = [
            ("HOME", std::env::var_os("HOME")),
            ("XDG_CONFIG_HOME", std::env::var_os("XDG_CONFIG_HOME")),
        ];
        std::env::set_var("HOME", &home);
        std::env::remove_var("XDG_CONFIG_HOME");
        // The loader the window uses for every project it opens.
        let load = ProjectServiceOptions::user().map(|options| options.load_config);
        let cfg = load
            .map_err(|error| error.to_string())
            .and_then(|load| load(&project));
        for (key, value) in saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let cfg = cfg.unwrap();
        let _ = std::fs::remove_dir_all(&project);

        assert_eq!(cfg.model.request_timeout_secs, 91);
        assert_eq!(cfg.workspace_root(), project.as_path());
    }

    fn uuid_like_suffix() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{}-{nanos}", std::process::id())
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
