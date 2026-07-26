//! Visual regression harness using ratatui TestBackend (no real TTY).

#[cfg(test)]
mod tests {
    use crate::app::{TuiApp, TuiRuntimeConfig};
    use crate::overlays::Overlay;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use forge_core::{AgentSession, LoopConfig};
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::ModelResponse;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn app() -> (TempDir, TuiApp) {
        let dir = TempDir::new().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: true,

                ..Default::default()
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        let app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("/tmp"),
                version: "forge 0.8.0".into(),
            },
        );
        (dir, app)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let area = buf.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        out
    }

    #[tokio::test]
    async fn visual_slash_autocomplete_shows_suggestions() {
        let (_d, mut app) = app().await;
        for c in "/re".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
        }
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        // Save for manual inspection
        let _ = std::fs::write("/tmp/forge_tui_visual_slash.txt", &text);
        assert!(
            text.contains("resume")
                || text.contains("/resume")
                || text.contains("commands")
                || text.contains("suggestions"),
            "frame missing autocomplete:\n{text}"
        );
        assert!(
            text.contains("/re") || text.contains("re"),
            "input missing:\n{text}"
        );
        assert!(
            text.contains('▶') || text.contains('█') || text.contains("/re"),
            "expected selection marker or input:\n{text}"
        );
        assert!(
            text.contains("Surface-local commands do not call the model"),
            "missing command safety note:\n{text}"
        );
        assert!(
            text.contains("Resume session by id"),
            "missing selected command help:\n{text}"
        );
        // Selected row must use solid teal background (theme::ACCENT)
        let buf = term.backend().buffer();
        let area = buf.area();
        let mut found_sel_bg = false;
        let mut found_caret_bg = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.style().bg == Some(crate::theme::ACCENT) {
                    found_sel_bg = true;
                }
                // Block cursor: solid TEXT background cell (space or inverted char)
                if cell.style().bg == Some(crate::theme::TEXT) {
                    found_caret_bg = true;
                }
            }
        }
        assert!(
            found_sel_bg,
            "selected suggestion must have ACCENT background"
        );
        assert!(
            found_caret_bg,
            "block cursor must have solid TEXT background"
        );
    }

    #[tokio::test]
    async fn visual_connect_picker_frame() {
        let (_d, mut app) = app().await;
        for c in "/connect".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
        }
        app.handle_key(press(KeyCode::Enter)).await.unwrap();
        assert!(matches!(app.overlay, Some(Overlay::ConnectPicker { .. })));
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        let _ = std::fs::write("/tmp/forge_tui_visual_connect.txt", &text);
        assert!(
            text.contains("Grok") || text.contains("xAI") || text.contains("connect"),
            "picker frame:\n{text}"
        );
        assert!(
            text.contains("OpenCode") || text.contains("opencode") || text.contains("Go"),
            "picker frame missing Go:\n{text}"
        );
    }

    #[tokio::test]
    async fn visual_status_command_in_textbox() {
        let (_d, mut app) = app().await;
        for c in "/status".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
        }
        app.handle_key(press(KeyCode::Enter)).await.unwrap();
        let backend = TestBackend::new(100, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        let _ = std::fs::write("/tmp/forge_tui_visual_status.txt", &text);
        assert!(
            text.contains("session")
                || text.contains("ctx")
                || app.status_message.contains("ctx")
                || app.notices.iter().any(|l| l.contains("model=")),
            "status frame:\n{text}\nstatus_msg={}",
            app.status_message
        );
    }

    #[tokio::test]
    async fn visual_wide_shell_shows_sidebar_activity() {
        let (_d, mut app) = app().await;
        app.push_activity(
            crate::activity::ActivityKind::Model,
            crate::widgets::FeedbackSeverity::Info,
            "model started",
        );
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains("FORGE"), "missing session chrome:\n{text}");
        assert!(text.contains("SESSION"), "missing sidebar:\n{text}");
        assert!(text.contains("RECENT JOURNAL"), "missing journal:\n{text}");
        assert!(
            text.contains("model started"),
            "missing activity item:\n{text}"
        );
    }

    #[tokio::test]
    async fn visual_idle_home_matches_reference_structure() {
        let (_d, mut app) = app().await;
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);

        for expected in [
            "FORGE",
            "SYSTEM",
            "Forge ready",
            "Waiting for your first message.",
            "Describe a task or paste an error",
            "CONTEXT BUDGET",
            "TOOLS (ACL)",
            "RECENT JOURNAL",
        ] {
            assert!(text.contains(expected), "missing {expected:?}:\n{text}");
        }
    }
}
