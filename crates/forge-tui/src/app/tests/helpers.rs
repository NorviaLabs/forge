//! Shared fixtures and helpers for `app` integration tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::util::footer_provider_id;
use super::watch::path_is_under_dot_forge;
use super::*;
use crate::widgets::status::TurnLifecycle;
use forge_config::CommandConfig;
use forge_core::LoopConfig;
use forge_model::{MockModelClient, ModelClient};
use forge_tools::ToolRegistry;
use forge_types::{Message, MessageRole, ModelResponse};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tempfile::TempDir;

/// Returns (journal_workspace_guard, session). Keep the TempDir until the test ends.
pub(crate) async fn test_session() -> (TempDir, AgentSession) {
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
    let (dir, session) = test_session().await;
    let app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    (dir, app)
}

pub(crate) fn render_app_text(app: &mut TuiApp, width: u16, height: u16) -> String {
    use ratatui::backend::TestBackend;

    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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

pub(crate) fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

pub(crate) fn hit_point(app: &TuiApp, predicate: impl Fn(&HitTarget) -> bool) -> (u16, u16) {
    let region = app
        .hit_regions
        .iter()
        .find(|region| region.generation == app.frame_generation && predicate(&region.target))
        .expect("expected hit region");
    (region.area.x, region.area.y)
}

pub(crate) fn hit_point_for_path(
    app: &TuiApp,
    predicate: impl Fn(&HitTarget, &Path) -> bool,
    path: &Path,
) -> (u16, u16) {
    hit_point(app, |target| predicate(target, path))
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
    }
}

pub(crate) fn press(code: KeyCode, mods: KeyModifiers) -> event::KeyEvent {
    event::KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

/// Save-and-restore env vars so dev machine credentials don't leak into tests.
pub(crate) struct ScopedEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

impl ScopedEnvGuard {
    pub(crate) fn new(keys: &[&str]) -> Self {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("environment test lock poisoned");
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
/// Each answer needs its own preceding user message: `compose_turn_presentation`
/// keeps only one durable answer per turn, so consecutive assistant messages
/// collapse into the last one and the earlier blocks never render at all.
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
    app.splash_dismissed = true;
    push_code_transcript(&mut app, marker);
    (dir, app)
}
