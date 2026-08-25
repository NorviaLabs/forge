//! Shared fixtures and helpers for `app` integration tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::super::*;
use forge_core::LoopConfig;
use forge_model::{MockModelClient, ModelClient};
use forge_tools::ToolRegistry;
use forge_types::ModelResponse;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tempfile::TempDir;

/// Git-initializes `dir` so any write routed through the centralized
/// runtime-storage resolver (UI state, run history, context offload/
/// progress) resolves to repository-local storage hermetically inside the
/// tempdir — without this, those writers fall back to the platform
/// application-data directory (correct real-world behavior outside a
/// repository, but not something a test should touch on the host machine).
/// Point skill discovery at an empty directory for the whole test process.
///
/// Sessions splice globally installed skills (`~/.agents/skills`) into their
/// system prompt. On a developer machine that is whatever they happen to have
/// installed; in CI it is nothing. That difference is enough to change
/// behaviour — a bigger prompt pushed the context lifecycle into an extra
/// model call, which consumed a scripted mock's first response and made an
/// approval test fail locally while passing in CI.
///
/// Every caller sets the same value, so the repeated writes are benign.
pub(crate) fn isolate_global_skills() {
    static EMPTY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = EMPTY.get_or_init(|| tempfile::tempdir().expect("temp dir for skill isolation"));
    std::env::set_var("FORGE_GLOBAL_SKILLS_DIR", dir.path());
}

/// Points the platform application-data directory (`dirs::data_dir()`, which
/// derives from `$HOME`) at a throwaway home for one test. Holds the
/// test-env lock until dropped, so env-sensitive tests serialize, and
/// restores the previous `$HOME` on drop.
///
/// Needed by tests that exercise runtime-storage fallbacks outside a Git
/// repository (UI state, clipboard attachments): those land in the platform
/// application-data directory, which is not writable on every host.
pub(crate) fn fake_home_guard() -> (TempDir, HomeGuard) {
    let lock = lock_test_env();
    let home = TempDir::new().unwrap();
    let saved_home = std::env::var_os("HOME");
    let saved_userprofile = std::env::var_os("USERPROFILE");
    std::env::set_var("HOME", home.path());
    std::env::set_var("USERPROFILE", home.path());
    (
        home,
        HomeGuard {
            saved_home,
            saved_userprofile,
            _lock: lock,
        },
    )
}

/// Restores the pre-test environment when dropped.
pub(crate) struct HomeGuard {
    saved_home: Option<std::ffi::OsString>,
    saved_userprofile: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.saved_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        match &self.saved_userprofile {
            Some(path) => std::env::set_var("USERPROFILE", path),
            None => std::env::remove_var("USERPROFILE"),
        }
    }
}

pub(crate) fn init_repo(dir: &Path) {
    isolate_global_skills();
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

/// Returns (journal_workspace_guard, session). Keep the TempDir until the
/// test ends. Deliberately does *not* git-init `dir` — some tests
/// (repo-header display) rely on it being a plain, non-Git directory.
/// Tests that trigger a write through the runtime-storage resolver (UI
/// state save, run-history save, context offload/progress) should call
/// `init_repo` themselves first, to avoid falling back to the real
/// platform application-data directory.
/// The runtime config every `TuiApp` fixture uses. Kept in one place so a
/// new field is added once rather than in every test that builds an app.
pub(crate) fn test_runtime_config() -> TuiRuntimeConfig {
    TuiRuntimeConfig {
        model_label: "mock".into(),
        provider: "mock".into(),
        cwd: PathBuf::from("."),
        version: "0.12.0".into(),
        startup_notices: Vec::new(),
        file_icons: FileIconMode::Unicode,
        theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
    }
}

pub(crate) async fn test_session() -> (TempDir, AgentSession) {
    isolate_global_skills();
    let dir = TempDir::new().unwrap();
    let session = session_for_workspace(dir.path()).await;
    (dir, session)
}

pub(crate) async fn session_for_workspace(workspace: &Path) -> AgentSession {
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "hello tui".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    let session = AgentSession::create(
        LoopConfig {
            max_turns: 4,
            workspace: workspace.to_path_buf(),
            journal_dir: workspace.join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,

            ..Default::default()
        },
        model,
        ToolRegistry::new(),
    )
    .await
    .unwrap();
    session
}

pub(crate) async fn session_for_workspace_with_model(
    workspace: &Path,
    model: Arc<dyn ModelClient>,
) -> AgentSession {
    AgentSession::create(
        LoopConfig {
            max_turns: 4,
            workspace: workspace.to_path_buf(),
            journal_dir: workspace.join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,

            ..Default::default()
        },
        model,
        ToolRegistry::new(),
    )
    .await
    .unwrap()
}

pub(crate) async fn focus_test_app() -> (TempDir, TuiApp) {
    focus_test_app_with_theme(forge_config::DEFAULT_THEME_ID).await
}

/// Like `focus_test_app`, but built under the given theme id. Callers that
/// exercise a specific shipped palette must also `crate::theme::install` that
/// theme on the test thread (the app records the id; rendering resolves it
/// through the thread-local theme registry).
pub(crate) async fn focus_test_app_with_theme(theme_id: &str) -> (TempDir, TuiApp) {
    let (dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "test".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: theme_id.to_string(),
        },
    );
    // `TuiApp::new` restores any real, ambient connect credentials from the
    // host's credential store (correct in production) — isolate that here so
    // rendering assertions never depend on whichever provider the machine
    // running the suite happens to have connected.
    app.connect.profile = None;
    app.connect.store = CredentialStore::new(
        tempfile::TempDir::new()
            .unwrap()
            .path()
            .join("empty-creds.toml"),
    );
    (dir, app)
}

pub(crate) fn render_app_text(app: &mut TuiApp, width: u16, height: u16) -> String {
    use ratatui::backend::TestBackend;

    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    app.tick_render_state();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let area = buffer.area();
    let mut text = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

pub(crate) fn draw_app(app: &mut TuiApp, width: u16, height: u16) {
    use ratatui::backend::TestBackend;

    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
}

pub(crate) fn assert_buffer_fully_themed(buf: &ratatui::buffer::Buffer) {
    use ratatui::style::Color;
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            let bg = buf[(x, y)].style().bg;
            assert!(bg.is_some(), "unpainted cell at {x},{y}");
            assert_ne!(bg, Some(Color::Black), "terminal-default black at {x},{y}");
        }
    }
}

pub(crate) fn direct_hitl_payload(call_id: &str, path: &str) -> HitlPayload {
    HitlPayload {
        call_id: call_id.into(),
        tool: "read_file".into(),
        args_redacted: json!({"path": path}),
        reason: "test approval".into(),
        failure: None,
        sandbox_escalation: false,
        denied_host: None,
    }
}

/// Directly install a HITL wait onto the session's `active_task` for test
/// setup. Bypasses the normal transition validator (as session restoration
/// does) — appropriate here since these tests simulate "there's a pending
/// approval" without driving a real tool call through governance.
pub(crate) fn set_pending_hitl(app: &mut TuiApp, payload: HitlPayload) {
    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Waiting;
    app.session.active_task.wait_reason = Some(forge_types::WaitReason::Approval {
        request_id: payload.call_id.clone(),
        payload,
    });
}

pub(crate) fn set_pending_question(app: &mut TuiApp, payload: forge_types::QuestionPayload) {
    app.session.active_task.lifecycle = forge_types::TaskLifecycle::Waiting;
    app.session.active_task.wait_reason = Some(forge_types::WaitReason::Question {
        request_id: payload.call_id.clone(),
        payload,
    });
}

pub(crate) fn press(code: KeyCode, mods: KeyModifiers) -> event::KeyEvent {
    event::KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

/// Serialise every test that reads or writes process environment, including
/// `HOME` / `dirs::home_dir()`. Recover poison so one failing test does not
/// cascade into the rest of the suite.
pub(crate) fn lock_test_env() -> MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Save-and-restore env vars so dev machine credentials don't leak into tests.
pub(crate) struct ScopedEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

impl ScopedEnvGuard {
    pub(crate) fn new(keys: &[&str]) -> Self {
        let lock = lock_test_env();
        let mut saved = Vec::new();
        for key in keys {
            saved.push((key.to_string(), std::env::var(key).ok()));
            std::env::remove_var(key);
        }
        Self { _lock: lock, saved }
    }
}

impl Drop for ScopedEnvGuard {
    fn drop(&mut self) {
        for (key, val) in &self.saved {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Isolate `HOME`/`XDG_CONFIG_HOME`/provider API-key env vars to an empty temp
/// dir so `CredentialStore::user_default()` (read automatically by
/// `TuiApp::new`'s `restore_saved_auth()`, before a test can override
/// `app.connect.store`) can never discover this dev machine's real
/// credentials and silently overwrite `TuiRuntimeConfig`'s model/provider.
pub(crate) fn isolated_home_guard() -> (TempDir, ScopedEnvGuard) {
    let temp_home = TempDir::new().unwrap();
    let cred_dir = temp_home.path().join("Library/Application Support/forge");
    std::fs::create_dir_all(&cred_dir).unwrap_or_default();
    let _ = std::fs::write(cred_dir.join("credentials.toml"), "");
    let guard = ScopedEnvGuard::new(&[
        "HOME",
        "XDG_CONFIG_HOME",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENCODE_API_KEY",
        "OPENCODE_GO_API_KEY",
        "OPENCODE_ZEN_API_KEY",
        "OLLAMA_API_KEY",
        "XAI_API_KEY",
    ]);
    std::env::set_var("HOME", temp_home.path());
    (temp_home, guard)
}

// ---- highlight cache invalidation -------------------------------------
//
// The highlight cache is process-global and it is NOT exclusive to these
// tests: `source_viewer` highlights raw source files, so several of its tests
// move the same counters concurrently. Exact equality on those counters is
// therefore flaky by construction.
//
// These assertions are written so concurrent activity can never *falsify*
// them: "reuse" is asserted as a lower bound on hits and "invalidation" as a
// lower bound on misses, and other tests can only ever add to both. Exact
// hit/miss semantics are pinned separately in `forge-syntax`'s own unit
// tests, where the cache genuinely is exclusive.

pub(crate) const CACHED_BLOCKS: usize = 4;

/// Serialises these four tests against each other so their windows do not
/// overlap. Follows the repo's pattern for process-global state (`lock_env`
/// in `editor.rs`, `ScopedEnvGuard` in `app.rs`), recovering poisoning so one
/// failing test does not cascade into the rest.
pub(crate) fn lock_highlight_cache() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Assistant turns each carrying a distinct fenced Rust block, so a full
/// re-highlight costs `CACHED_BLOCKS` misses and a fully cached render costs
/// `CACHED_BLOCKS` hits.
///
/// Each answer needs its own preceding user message so highlight-cache
/// measurements treat the fenced blocks as separate turns rather than one
/// long assistant message.
pub(crate) fn push_code_transcript(app: &mut TuiApp, marker: &str) {
    for i in 0..CACHED_BLOCKS {
        app.session.messages.push(forge_types::Message::new(
            forge_types::MessageRole::User,
            format!("Please do step {i} of {marker}."),
        ));
        app.session.messages.push(forge_types::Message::new(
            forge_types::MessageRole::Assistant,
            format!(
                "Step {i} for {marker}.\n\n```rust\n\
                 pub fn {marker}_{i}(items: &[usize]) -> usize {{\n\
                 \x20   let mut total = 0usize;\n\
                 \x20   for item in items {{ total += *item; }}\n\
                 \x20   total\n\
                 }}\n```\n\nDone."
            ),
        ));
    }
}

pub(crate) async fn app_with_code(marker: &str) -> (TempDir, TuiApp) {
    let (dir, mut app) = focus_test_app().await;
    app.conversation_view.splash_dismissed = true;
    push_code_transcript(&mut app, marker);
    (dir, app)
}
