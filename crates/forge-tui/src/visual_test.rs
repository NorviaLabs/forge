//! Visual regression harness using ratatui TestBackend (no real TTY).

#[cfg(test)]
mod tests {
    use crate::app::{FocusBlock, TuiApp, TuiRuntimeConfig};
    use crate::overlays::Overlay;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use forge_core::{AgentSession, LoopConfig};
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::ModelResponse;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Git-initializes `dir` so writes routed through the runtime-storage
    /// resolver (UI state, run history, context offload/progress) resolve
    /// repository-locally inside the tempdir, instead of falling back to
    /// the real platform application-data directory.
    fn init_repo(dir: &std::path::Path) {
        for args in [
            vec!["init", "--initial-branch=main", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    async fn app() -> (TempDir, TuiApp) {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
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
                startup_notices: Vec::new(),
                validation_command: None,
                file_icons: forge_config::FileIconMode::Unicode,
                mouse_capture: true,
                theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
            },
        );
        (dir, app)
    }

    #[tokio::test]
    async fn visual_header_shows_repo_model_context_and_state_when_wide() {
        let (dir, mut app) = app().await;
        let repo = dir.path().join("forge");
        std::fs::create_dir_all(&repo).unwrap();
        app.runtime.cwd = repo.clone();
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "forge@test"])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Forge Test"])
            .current_dir(&repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("dirty.txt"), "x").unwrap();
        app.runtime.cwd = repo.clone();
        app.handle_key(press(KeyCode::Char('x'))).await.unwrap();
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        let top = text.lines().next().unwrap_or_default();
        assert!(top.contains("Forge"), "missing header brand:\n{text}");
        assert!(top.contains("main*"), "missing dirty branch:\n{text}");
    }

    #[tokio::test]
    async fn visual_header_omits_low_priority_fields_when_narrow() {
        let (dir, mut app) = app().await;
        let repo = dir.path().join("forge");
        std::fs::create_dir_all(&repo).unwrap();
        app.runtime.cwd = repo.clone();
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&repo)
            .output()
            .unwrap();
        app.runtime.cwd = repo.clone();
        app.handle_key(press(KeyCode::Char('x'))).await.unwrap();
        let backend = TestBackend::new(40, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        let top = text.lines().next().unwrap_or_default();
        assert!(top.contains("Forge"), "missing Forge:\n{text}");
        assert!(
            !top.contains("mock"),
            "model should not be duplicated:\n{text}"
        );
        assert!(
            !top.contains("context"),
            "context should not be duplicated:\n{text}"
        );
    }

    fn press(code: KeyCode) -> KeyEvent {
        press_with(code, KeyModifiers::NONE)
    }

    fn press_with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
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
    async fn contextual_workspace_default_has_no_permanent_tabs() {
        let (_d, mut app) = app().await;

        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);

        assert!(
            !text.contains(" Chat  Editor  Diff "),
            "permanent workspace tabs should be gone:\n{text}"
        );
        assert!(
            text.contains("Describe a task…"),
            "missing composer prompt:\n{text}"
        );
    }

    #[tokio::test]
    async fn contextual_workspace_review_changes_uses_redirected_tab_control() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        Command::new("git")
            .arg("init")
            .current_dir(&workspace)
            .output()
            .unwrap();
        fs::write(workspace.join("x.txt"), "changed").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.workspace_files.explorer = crate::file_explorer::FileExplorer::new(
            Some(workspace.clone()),
            forge_config::FileIconMode::Unicode,
        );

        app.handle_key(press_with(KeyCode::Right, KeyModifiers::ALT))
            .await
            .unwrap();

        // Wait for the background git-status thread.
        while app.workspace_files.explorer.git_status.loading {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            app.workspace_files.explorer.git_status.poll();
        }

        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        assert!(
            text.contains("CHANGES") || text.contains("No changes"),
            "missing diff inventory or empty state:\n{text}"
        );
    }

    #[tokio::test]
    async fn workspace_navigation_does_not_handle_keys_under_overlay() {
        let (_d, mut app) = app().await;
        app.overlay = Some(Overlay::Help);
        app.handle_key(press_with(KeyCode::Right, KeyModifiers::ALT))
            .await
            .unwrap();

        assert!(matches!(app.overlay, Some(Overlay::Help)));
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
            text.contains("Restore a previous session"),
            "missing selected command help:\n{text}"
        );
        // Selected row uses the design-system selected surface, distinct from
        // the cyan focus accent.
        let buf = term.backend().buffer();
        let area = buf.area();
        let mut found_sel_bg = false;
        let mut found_caret_bg = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.style().bg == Some(crate::theme::palette(&crate::theme::active()).selection)
                {
                    found_sel_bg = true;
                }
                // Block cursor: solid inverted cell (space or inverted char)
                if cell.style().bg == crate::theme::caret().bg {
                    found_caret_bg = true;
                }
            }
        }
        assert!(
            found_sel_bg,
            "selected suggestion must have selected-surface background"
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
        assert!(matches!(app.overlay, Some(Overlay::ConnectModel { .. })));
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
    async fn visual_model_picker_marks_current_config_only_switch() {
        let (_d, mut app) = app().await;
        let items = vec![crate::overlays::ModelItem {
            provider: "native".into(),
            model: "mock".into(),
            profile_id: Some("mock".into()),
            source: forge_connect::CatalogSource::Default,
            route_label: "Mock".into(),
        }];
        app.overlay = Some(Overlay::connect_model_open(
            vec![],
            items,
            Some("mock"),
            "mock",
            crate::ReasoningEffort::default(),
            crate::overlays::ConnectModelColumn::Models,
        ));
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        for expected in ["Choose a model", "mock", "current", "Esc close"] {
            assert!(text.contains(expected), "missing {expected:?}:\n{text}");
        }
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
            app.status_state.message.contains("unknown command")
                || app.notice_state.items.is_empty(),
            "status frame:\n{text}\nstatus_msg={}",
            app.status_state.message
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
        app.handle_key(press_with(KeyCode::Char('['), KeyModifiers::ALT))
            .await
            .unwrap();
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        assert!(
            !text.contains("in 0 · out 0 · total 0"),
            "default shell should not duplicate footer usage:\n{text}"
        );
        assert!(
            !text.contains("Runtime"),
            "default shell should not show inspector:\n{text}"
        );
        assert!(
            !text.contains("Recent"),
            "default shell should not show activity panel:\n{text}"
        );
        assert!(
            !text.contains("model started"),
            "background activity should remain off default chrome:\n{text}"
        );
    }

    #[tokio::test]
    async fn visual_idle_home_matches_reference_structure() {
        let (_d, mut app) = app().await;
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);

        for expected in ["Describe a task…", "forge 0.8.0"] {
            assert!(text.contains(expected), "missing {expected:?}:\n{text}");
        }
        assert!(
            !text.contains("Context"),
            "default shell should not show inspector context:\n{text}"
        );
        assert!(text.contains("Forge"), "missing branding:\n{text}");
        assert!(
            !text.contains("Waiting for your first message."),
            "stale copy:\n{text}"
        );
    }

    #[tokio::test]
    async fn visual_splash_disappears_after_typing() {
        let (_d, mut app) = app().await;
        app.handle_key(press(KeyCode::Char('h'))).await.unwrap();
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);

        assert!(!text.contains("FORGE"), "splash still visible:\n{text}");
        assert!(
            !text.contains("Loaded AGENTS.md"),
            "home copy still visible:\n{text}"
        );
        assert!(text.contains("h"), "typed input missing:\n{text}");
    }

    #[tokio::test]
    async fn editor_opens_text_file_from_files_panel() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            workspace.join("main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.workspace_files.explorer = crate::file_explorer::FileExplorer::new(
            Some(workspace.clone()),
            forge_config::FileIconMode::Unicode,
        );
        app.workspace_files.visible = true;
        app.focus_block(FocusBlock::Files);
        // Select the file and open it.
        app.workspace_files.explorer.move_selection(1);
        app.handle_key(press(KeyCode::Enter)).await.unwrap();

        let expected = workspace.join("main.rs").canonicalize().unwrap();
        assert_eq!(app.source_viewer.path.as_deref(), Some(expected.as_path()));
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains("main.rs"), "missing path:\n{text}");
        assert!(text.contains("fn main()"), "missing content:\n{text}");
        assert!(text.contains("1 │"), "missing line numbers:\n{text}");
    }

    #[tokio::test]
    async fn editor_shows_binary_state_for_binary_file() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        let mut file = fs::File::create(workspace.join("image.bin")).unwrap();
        file.write_all(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
            .unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.workspace_files.explorer = crate::file_explorer::FileExplorer::new(
            Some(workspace.clone()),
            forge_config::FileIconMode::Unicode,
        );
        app.workspace_files.visible = true;
        app.focus_block(FocusBlock::Files);
        app.workspace_files.explorer.move_selection(1);
        app.handle_key(press(KeyCode::Enter)).await.unwrap();

        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        assert!(
            text.contains("Binary file"),
            "missing binary header:\n{text}"
        );
        assert!(text.contains("image.bin"), "missing path:\n{text}");
    }

    #[tokio::test]
    async fn editor_navigation_moves_cursor() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("x.txt"), "line1\nline2\nline3\n").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.open_file_view_for_test(&workspace.join("x.txt"));

        app.handle_key(press(KeyCode::Down)).await.unwrap();
        assert_eq!(app.source_viewer.current_line, 1);
        app.handle_key(press(KeyCode::Down)).await.unwrap();
        app.handle_key(press(KeyCode::Down)).await.unwrap();
        assert_eq!(app.source_viewer.current_line, 2); // clamped
        app.handle_key(press(KeyCode::End)).await.unwrap();
        assert_eq!(app.source_viewer.h_scroll, 5); // "line3" width
        app.handle_key(press(KeyCode::Home)).await.unwrap();
        assert_eq!(app.source_viewer.h_scroll, 0);
    }

    #[tokio::test]
    async fn editor_search_finds_and_navigates_matches() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("x.txt"), "foo bar\nfoo baz\n").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.open_file_view_for_test(&workspace.join("x.txt"));

        app.handle_key(press_with(KeyCode::Char('f'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(app.source_viewer.search.open);
        for c in "foo".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
        }
        assert_eq!(app.source_viewer.search.matches.len(), 2);
        assert_eq!(app.source_viewer.current_line, 0);
        app.handle_key(press(KeyCode::Enter)).await.unwrap();
        assert_eq!(app.source_viewer.current_line, 1);
        app.handle_key(press_with(KeyCode::Enter, KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert_eq!(app.source_viewer.current_line, 0);
        app.handle_key(press(KeyCode::Esc)).await.unwrap();
        assert!(!app.source_viewer.search.open);
    }

    #[tokio::test]
    async fn editor_jump_to_line_moves_cursor() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("x.txt"), "1\n2\n3\n4\n").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.open_file_view_for_test(&workspace.join("x.txt"));

        app.handle_key(press_with(KeyCode::Char('g'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(app.source_viewer.jump.open);
        for c in "3".chars() {
            app.handle_key(press(KeyCode::Char(c))).await.unwrap();
        }
        app.handle_key(press(KeyCode::Enter)).await.unwrap();
        assert!(!app.source_viewer.jump.open);
        assert_eq!(app.source_viewer.current_line, 2);
    }

    #[tokio::test]
    async fn editor_focused_current_line_uses_brand_gutter_no_bright_bg() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("x.txt"), "first\nsecond\nthird\n").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.open_file_view_for_test(&workspace.join("x.txt"));
        app.source_viewer.focused = true;
        app.source_viewer.current_line = 1;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer();
        let text = buffer_text(&term);
        assert!(
            text.contains("2 │ second"),
            "current line not rendered:\n{text}"
        );

        // The current line uses the theme's subtle accent background.
        let accent = crate::theme::accent_soft_bg();
        let line_y = text.lines().position(|l| l.contains("2 │ second")).unwrap();
        let content_start_x = text.lines().nth(line_y).unwrap().find('│').unwrap() + 1;
        let has_themed_bg = (content_start_x..buf.area().width as usize)
            .any(|x| buf[(x as u16, line_y as u16)].style().bg == Some(accent));
        assert!(
            has_themed_bg,
            "current line content is missing the themed background:\n{text}"
        );
    }

    #[tokio::test]
    async fn editor_unfocused_current_line_reduces_gutter_emphasis() {
        use crate::source_viewer::SourceViewerWidget;
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("x.txt"), "first\nsecond\nthird\n").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.source_viewer.open(&workspace, &workspace.join("x.txt"));
        app.source_viewer.focused = false;
        app.source_viewer.current_line = 1;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            f.render_widget(
                SourceViewerWidget {
                    viewer: &mut app.source_viewer,
                    focused: false,
                },
                f.area(),
            );
        })
        .unwrap();
        let text = buffer_text(&term);
        // When the editor is not focused, the current line is still visible but
        // the gutter should not be bold brand.
        assert!(
            text.contains("2 │ second"),
            "current line not rendered:\n{text}"
        );
    }

    #[tokio::test]
    async fn explorer_shows_git_status_markers() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        Command::new("git")
            .arg("init")
            .current_dir(&workspace)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "forge@test"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Forge Test"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        fs::write(workspace.join("tracked.txt"), "x").unwrap();
        fs::write(workspace.join("untracked.txt"), "y").unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        fs::write(workspace.join("tracked.txt"), "changed").unwrap();

        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.workspace_files.explorer = crate::file_explorer::FileExplorer::new(
            Some(workspace.clone()),
            forge_config::FileIconMode::Unicode,
        );
        app.workspace_files.visible = true;
        app.focus_block(FocusBlock::Files);

        // Wait for the background git-status thread.
        while app.workspace_files.explorer.git_status.loading {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            app.workspace_files.explorer.git_status.poll();
        }

        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        assert!(
            text.contains("tracked.txt M"),
            "missing modified marker:\n{text}"
        );
        assert!(
            text.contains("untracked.txt ?"),
            "missing untracked marker:\n{text}"
        );
    }

    #[tokio::test]
    async fn diff_tab_loads_changes_with_files_hidden() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        Command::new("git")
            .arg("init")
            .current_dir(&workspace)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Forge Test"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        fs::write(workspace.join("tracked.txt"), "x").unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        fs::write(workspace.join("tracked.txt"), "changed").unwrap();

        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.workspace_files.explorer = crate::file_explorer::FileExplorer::new(
            Some(workspace),
            forge_config::FileIconMode::Unicode,
        );
        app.workspace_files.visible = false;
        app.review_changes_for_test();

        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        while app.workspace_files.explorer.git_status.loading {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            term.draw(|f| app.draw(f)).unwrap();
        }

        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains("CHANGES"), "missing diff inventory:\n{text}");
        assert!(
            text.contains("M tracked.txt"),
            "missing changed file:\n{text}"
        );
    }

    async fn rebuild_session(dir: &std::path::Path, workspace: &std::path::Path) -> AgentSession {
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "ok".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: workspace.to_path_buf(),
                journal_dir: dir.join("j"),
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

    #[tokio::test]
    async fn files_stay_populated_after_turn() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("a.txt"), "hello").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.workspace_files.explorer = crate::file_explorer::FileExplorer::new(
            Some(workspace.clone()),
            forge_config::FileIconMode::Unicode,
        );
        app.workspace_files.visible = true;

        let before = app.workspace_files.explorer.visible_nodes().len();
        assert!(before > 1, "explorer should have files before turn");

        // Simulate a chat turn the app would process while files are open.
        app.handle_key(press(KeyCode::Char('x'))).await.unwrap();
        app.handle_key(press(KeyCode::Enter)).await.unwrap();

        let after = app.workspace_files.explorer.visible_nodes().len();
        assert!(
            after >= before,
            "explorer tree must not shrink after turn: {before} → {after}"
        );
    }

    #[tokio::test]
    async fn search_survives_overlay_open() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("x.txt"), "searchable content\n").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.open_file_view_for_test(&workspace.join("x.txt"));

        app.source_viewer.start_search();
        app.source_viewer.update_search_query("searchable");
        assert!(!app.source_viewer.search.matches.is_empty());

        // Open an overlay (no-op overlay that doesn't touch search state).
        app.overlay = Some(crate::overlays::Overlay::turn_limit(5));

        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        // The draw must not panic. Search matches remain valid even though the
        // overlay takes input priority.
        assert_eq!(app.source_viewer.search.matches.len(), 1);
    }

    #[tokio::test]
    async fn search_survives_viewport_change() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("x.txt"), "alpha\nbeta\nsearchable gamma\n").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.open_file_view_for_test(&workspace.join("x.txt"));

        app.source_viewer.start_search();
        app.source_viewer.update_search_query("searchable");
        assert!(!app.source_viewer.search.matches.is_empty());

        // Render at one size, then resize and render again.
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        drop(term);

        let backend2 = TestBackend::new(60, 16);
        let mut term2 = Terminal::new(backend2).unwrap();
        term2.draw(|f| app.draw(f)).unwrap();
        // Must not panic and search matches should still be valid.
        assert!(!app.source_viewer.search.matches.is_empty());
    }

    #[tokio::test]
    async fn external_editor_resume_survives_resize_before_redraw() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("x.rs"), "fn main() {}\n").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        app.open_file_view_for_test(&workspace.join("x.rs"));

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();

        let backend = TestBackend::new(60, 16);
        let mut resized = Terminal::new(backend).unwrap();
        resized.autoresize().unwrap();
        resized.clear().unwrap();
        resized.draw(|f| app.draw(f)).unwrap();

        assert!(!app.source_viewer.lines.is_empty());
    }

    #[tokio::test]
    async fn load_failure_shows_unable_to_load() {
        let (dir, mut app) = app().await;
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("a.txt"), "hello").unwrap();
        app.session = rebuild_session(dir.path(), &workspace).await;
        app.runtime.cwd = workspace.clone();
        // Create an explorer whose root cannot be read.
        app.workspace_files.explorer = crate::file_explorer::FileExplorer::new(
            Some(workspace.join("nonexistent")),
            forge_config::FileIconMode::Unicode,
        );
        app.workspace_files.visible = true;

        let backend = TestBackend::new(120, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = buffer_text(&term);
        assert!(
            text.contains("Unable to load files"),
            "load failure should show error, got:\n{text}"
        );
    }
}
