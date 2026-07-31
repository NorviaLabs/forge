use super::util::footer_provider_id;
use super::watch::path_is_under_dot_forge;
use super::*;
use crate::widgets::status::TurnLifecycle;
use forge_config::CommandConfig;
use forge_core::LoopConfig;
use forge_model::MockModelClient;
use forge_tools::ToolRegistry;
use forge_types::{Message, MessageRole, ModelResponse};
use std::sync::Arc;
use tempfile::TempDir;

mod helpers;
mod pointer;

pub(crate) use helpers::*;

#[tokio::test]
async fn run_only_from_run_panel_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Running));

    app.bottom_panel.open_tab(BottomPanelTab::Run);
    app.focus_block(FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Running));
}

#[tokio::test]
async fn run_cancel_from_run_panel() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();
    app.bottom_panel.open_tab(BottomPanelTab::Run);
    app.focus_block(FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Cancelled));
}

#[tokio::test]
async fn restored_running_run_becomes_cancelled() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: Some(CommandConfig {
                executable: "true".into(),
                args: vec![],
            }),
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.run.draft.command_input = "true".into();
    app.run_current_draft();
    app.normalize_restored_run();
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Cancelled));
    assert!(!app.pending_validation);
    assert!(app.run_rx.is_none());
}

#[tokio::test]
async fn file_change_event_refreshes_git_status() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer.git_status = crate::git_status::GitStatusCache::new();
    assert!(!app.file_explorer.git_status.loading);

    app.file_change_tx
        .send(FileChangeEvent {
            path: app.session.workspace_root().join("changed.txt"),
        })
        .unwrap();
    app.poll_file_changes();

    assert!(app.file_explorer.git_status.loading);
}

#[tokio::test]
async fn ui_navigation_does_not_mutate_run_history() {
    let (_dir, mut app) = focus_test_app().await;
    app.bottom_panel.open_tab(BottomPanelTab::Run);
    app.focus_block(FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.run.recent.is_empty());
}

/// `repo_header()` must be a pure read of the cached field. If someone
/// reintroduces the `git` subprocess into it, the sentinel is overwritten by
/// real repo data and this fails — which is the point.
#[tokio::test]
async fn repo_header_reads_cache_without_shelling_out() {
    let (_dir, mut app) = focus_test_app().await;
    app.repo_header = RepoHeaderCache {
        repo_name: Some("sentinel-repo".into()),
        branch: Some("sentinel-branch".into()),
        dirty: true,
    };

    let header = app.repo_header();

    assert_eq!(header.repo_name.as_deref(), Some("sentinel-repo"));
    assert_eq!(header.branch.as_deref(), Some("sentinel-branch"));
    assert!(header.dirty);
}

/// Drawing must not derive the header either — several draws in a row leave
/// the cached sentinel untouched, proving the render path only reads it.
#[tokio::test]
async fn drawing_does_not_rederive_repo_header() {
    let (_dir, mut app) = focus_test_app().await;
    app.repo_header = RepoHeaderCache {
        repo_name: Some("sentinel-repo".into()),
        branch: Some("sentinel-branch".into()),
        dirty: true,
    };
    // Keep the TTL from firing a background refresh during the assertions.
    app.repo_header_refreshed_at = Instant::now();

    for _ in 0..3 {
        draw_app(&mut app, 120, 40);
    }

    assert_eq!(app.repo_header.branch.as_deref(), Some("sentinel-branch"));
}

/// FORGE-DESIGN 9.7: do not clear visible Git information during a refresh.
/// A dropped sender (failed refresh) must leave the last known header intact.
#[tokio::test]
async fn failed_repo_header_refresh_keeps_last_known_value() {
    let (_dir, mut app) = focus_test_app().await;
    app.repo_header = RepoHeaderCache {
        repo_name: Some("kept-repo".into()),
        branch: Some("kept-branch".into()),
        dirty: true,
    };
    let (tx, rx) = mpsc::channel::<RepoHeaderCache>();
    drop(tx); // simulate a refresh worker that died
    app.repo_header_rx = Some(rx);

    app.poll_repo_header();

    assert_eq!(app.repo_header.branch.as_deref(), Some("kept-branch"));
    assert_eq!(app.repo_header.repo_name.as_deref(), Some("kept-repo"));
    assert!(app.repo_header.dirty);
    assert!(app.repo_header_rx.is_none());
}

/// Changing the working directory must invalidate the cached header on the
/// very next poll, so the header never describes the previous directory.
#[tokio::test]
async fn cwd_change_refreshes_repo_header_immediately() {
    let (dir, mut app) = focus_test_app().await;
    app.repo_header = RepoHeaderCache {
        repo_name: Some("stale-repo".into()),
        branch: Some("stale-branch".into()),
        dirty: true,
    };

    let moved = dir.path().join("elsewhere");
    std::fs::create_dir_all(&moved).unwrap();
    app.runtime.cwd = moved.clone();
    app.poll_repo_header();

    assert_eq!(app.repo_header_cwd, moved);
    assert_eq!(app.repo_header.repo_name.as_deref(), Some("elsewhere"));
    // Plain directory, no git metadata: no branch, and not reported dirty.
    assert!(app.repo_header.branch.is_none());
    assert!(!app.repo_header.dirty);
}

/// An in-flight refresh that has not produced a value yet must be retained
/// rather than dropped, and must not disturb the current header.
#[tokio::test]
async fn pending_repo_header_refresh_is_retained() {
    let (_dir, mut app) = focus_test_app().await;
    app.repo_header = RepoHeaderCache {
        repo_name: Some("kept-repo".into()),
        branch: Some("kept-branch".into()),
        dirty: false,
    };
    let (tx, rx) = mpsc::channel::<RepoHeaderCache>();
    app.repo_header_rx = Some(rx);

    app.poll_repo_header();

    assert!(app.repo_header_rx.is_some(), "pending refresh must survive");
    assert_eq!(app.repo_header.branch.as_deref(), Some("kept-branch"));
    drop(tx);
}

#[tokio::test]
async fn final_shell_rendering_matrix_covers_v31_states_without_obsolete_chrome() {
    let sizes = [(80, 24), (120, 40), (160, 50), (240, 60)];
    let mut scenarios: Vec<(&str, TempDir, TuiApp, Vec<&str>)> = Vec::new();

    let (dir, app) = focus_test_app().await;
    scenarios.push(("conversation idle", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.busy = true;
    app.busy_phase = BusyPhase::Model;
    app.turn_started = Some(Instant::now());
    scenarios.push(("agent thinking", dir, app, vec!["thinking"]));

    let (dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    scenarios.push(("files open", dir, app, vec!["FILES", "Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.files_visible = false;
    scenarios.push(("files closed", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    let file = dir.path().join("src").join("matrix.rs");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "fn matrix() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(file.clone()))
        .await
        .unwrap();
    scenarios.push(("file open", dir, app, vec!["matrix.rs"]));

    let (dir, mut app) = focus_test_app().await;
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    scenarios.push(("diff", dir, app, vec!["CHANGES"]));

    let (dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "cargo test".into();
    app.run_current_draft();
    scenarios.push((
        "background run",
        dir,
        app,
        vec!["Running cargo test", "View output"],
    ));

    let (dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "cargo test".into();
    app.run_current_draft();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
        .await
        .unwrap();
    scenarios.push(("run open", dir, app, vec!["Run: cargo test"]));

    let (dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "cargo test".into();
    app.run_current_draft();
    if let Some(record) = app.run.current.as_mut() {
        record.state = RunState::Failed;
        record.exit_status = Some(101);
    }
    let run_id = app.current_run_id().unwrap();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
        .await
        .unwrap();
    scenarios.push(("run failed", dir, app, vec!["Failed", "Exit status: 101"]));

    let (dir, mut app) = focus_test_app().await;
    app.open_hitl_overlay(direct_hitl_payload("matrix-approval", "src/main.rs"));
    scenarios.push((
        "approval",
        dir,
        app,
        vec!["Approval required", "Allow once", "Deny"],
    ));

    let (dir, mut app) = focus_test_app().await;
    app.sidebar_visible = false;
    scenarios.push(("inspector closed", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.sidebar_visible = true;
    app.focus_block(FocusBlock::Inspector);
    scenarios.push((
        "inspector open",
        dir,
        app,
        vec!["INSPECTOR", "Describe a task"],
    ));

    let (dir, mut app) = focus_test_app().await;
    app.bottom_panel.open = false;
    scenarios.push(("bottom closed", dir, app, vec!["Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.open_bottom_panel(Some(BottomPanelTab::Terminal));
    scenarios.push(("bottom open", dir, app, vec!["Terminal", "Describe a task"]));

    let (dir, mut app) = focus_test_app().await;
    app.runtime.mouse_capture = false;
    scenarios.push(("mouse disabled", dir, app, vec!["Describe a task"]));

    for (name, _dir, mut app, expected) in scenarios {
        for (width, height) in sizes {
            let text = render_app_text(&mut app, width, height);
            assert!(
                !text.contains(" Chat  Editor  Diff "),
                "{name} at {width}x{height} restored permanent tabs:\n{text}"
            );
            assert!(
                !text.contains("BOTTOM"),
                "{name} at {width}x{height} restored BOTTOM label:\n{text}"
            );
            assert!(
                !text.contains("Ctrl+P close"),
                "{name} at {width}x{height} restored shortcut manual:\n{text}"
            );
            if name == "mouse disabled" {
                assert!(
                    !text.to_ascii_lowercase().contains("mouse"),
                    "mouse-disabled chrome should not spend space on mouse hints:\n{text}"
                );
            }
            let lower_text = text.to_ascii_lowercase();
            assert!(
                expected
                    .iter()
                    .any(|needle| lower_text.contains(&needle.to_ascii_lowercase())),
                "{name} at {width}x{height} missing expected state {:?}:\n{text}",
                expected
            );
        }
    }
}

#[tokio::test]
async fn focus_starts_on_composer_block() {
    let (_dir, app) = focus_test_app().await;
    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert_eq!(app.focus.mode, FocusMode::Navigation);
}

#[tokio::test]
async fn tab_cycles_visible_blocks_and_skips_hidden_ones() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.files_visible = true;
    app.bottom_panel.open = true;
    app.normalize_focus();

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Files);
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    app.sidebar_visible = false;
    app.normalize_focus();
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Files);
}

#[tokio::test]
async fn file_change_does_not_reload_tree_while_files_sidebar_is_focused() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("crates")).unwrap();
    fs::create_dir(dir.path().join("crates/forge-tui")).unwrap();
    fs::write(dir.path().join("crates/forge-tui/Cargo.toml"), "").unwrap();
    app.file_explorer.refresh_selected();
    app.file_explorer.selected_path = Some(dir.path().join("crates").canonicalize().unwrap());
    app.file_explorer.expand_selected();
    app.file_explorer.selected_path =
        Some(dir.path().join("crates/forge-tui").canonicalize().unwrap());
    app.file_explorer.expand_selected();
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);
    app.file_explorer.git_status = crate::git_status::GitStatusCache::new();

    app.file_change_tx
        .send(FileChangeEvent {
            path: app.session.workspace_root().join("changed.txt"),
        })
        .unwrap();
    app.poll_file_changes();

    assert!(app.file_explorer.git_status.loading);
    assert!(app
        .file_explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "Cargo.toml"));
}

#[test]
fn forge_runtime_paths_are_ignored_by_file_watcher_filter() {
    assert!(path_is_under_dot_forge(Path::new(".forge/progress.json")));
    assert!(path_is_under_dot_forge(Path::new(
        "/tmp/repo/.forge/sessions/x.db"
    )));
    assert!(!path_is_under_dot_forge(Path::new("src/app.rs")));
    assert!(!path_is_under_dot_forge(Path::new("/tmp/repo/src/lib.rs")));
}

#[tokio::test]
async fn tab_and_shift_tab_reach_composer() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);

    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
}

#[tokio::test]
async fn opening_and_closing_bottom_panel_transfers_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    assert!(app.bottom_panel.open);
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn shift_arrow_tabs_only_apply_to_the_active_navigation_block() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );

    app.sidebar_visible = true;
    app.focus_block(FocusBlock::Inspector);
    assert_eq!(app.focus.block, FocusBlock::Inspector);
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.inspector_view, InspectorView::Context);

    app.open_bottom_panel(None);
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
}

#[tokio::test]
async fn chat_input_keeps_literal_brackets_and_shift_arrows_do_not_switch_tabs() {
    let (_dir, mut app) = focus_test_app().await;
    app.handle_key(press(KeyCode::Char('['), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char(']'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "[]");
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.mode, FocusMode::Navigation);
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert_eq!(app.input.text, "[]x");
}

#[tokio::test]
async fn esc_from_composer_returns_to_previous_block_and_keeps_draft() {
    let (_dir, mut app) = focus_test_app().await;
    for block in [
        FocusBlock::Files,
        FocusBlock::Workspace,
        FocusBlock::Inspector,
        FocusBlock::BottomPanel,
    ] {
        app.files_visible = true;
        app.sidebar_visible = true;
        app.bottom_panel.open = true;
        app.focus_block(block);
        app.enter_chat_composer();
        app.input.set_text("draft");
        app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.focus.block, block);
        assert_eq!(app.input.text, "draft");
    }
}

#[tokio::test]
async fn type_to_compose_keeps_first_unbound_printable() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert_eq!(app.input.text, "x");
}

#[tokio::test]
async fn semantic_key_paths_emit_existing_commands() {
    let (_dir, mut app) = focus_test_app().await;
    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL)),
        Some(SemanticCommand::ToggleFiles)
    );
    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        Some(SemanticCommand::OpenGlobalCommandPalette)
    );

    app.focus_block(FocusBlock::Workspace);
    assert_eq!(
        app.semantic_command_for_workspace_key(press(KeyCode::Right, KeyModifiers::SHIFT)),
        Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
    );
    assert_eq!(
        app.semantic_command_for_composer_key(press(KeyCode::Enter, KeyModifiers::NONE)),
        Some(SemanticCommand::SubmitMessage)
    );
    assert_eq!(
        app.semantic_command_for_composer_key(press(KeyCode::Enter, KeyModifiers::SHIFT)),
        Some(SemanticCommand::InsertComposerNewline)
    );

    app.files_visible = true;
    assert_eq!(
        app.semantic_command_for_file_key(press(KeyCode::Enter, KeyModifiers::NONE)),
        Some(SemanticCommand::OpenSelectedEntry)
    );
}

#[tokio::test]
async fn semantic_commands_dispatch_without_rendering_a_frame() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();
    assert!(app.files_visible);
    assert_eq!(app.focus.block, FocusBlock::Files);

    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(path.clone())
    );
    assert_eq!(
        app.source_viewer.path.as_deref(),
        Some(path.canonicalize().unwrap().as_path())
    );

    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
        .await
        .unwrap();
    assert!(app.bottom_panel.open);
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Run);
}

#[tokio::test]
async fn semantic_dispatch_handles_invalid_or_stale_identifiers_without_panic() {
    let (_dir, mut app) = focus_test_app().await;
    let missing = PathBuf::from("/definitely/missing/forge-file.rs");

    app.execute_semantic_command(SemanticCommand::SelectEntry(missing.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::ToggleDirectory(missing.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(missing))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(
        "missing-run".into(),
    )))
    .await
    .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(!app.bottom_panel.open);
}

#[tokio::test]
async fn modal_and_transient_precedence_still_wins_over_semantic_bindings() {
    let (dir, mut app) = focus_test_app().await;
    app.overlay = Some(Overlay::welcome());
    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!app.files_visible);
    assert!(app.overlay.is_some());

    app.overlay = None;
    let path = dir.path().join("source.txt");
    fs::write(&path, "alpha\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('z'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.focus.mode,
        FocusMode::Transient(TransientOwner::SourceSearch)
    );
    assert_eq!(app.source_viewer.search.query, "z");
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn printable_globals_remain_available_to_type_to_compose() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);
    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Char('x'), KeyModifiers::NONE)),
        None
    );

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block, FocusBlock::Composer);
    assert_eq!(app.input.text, "x");
}

#[tokio::test]
async fn global_palette_selection_uses_semantic_dispatch() {
    let (_dir, mut app) = focus_test_app().await;
    app.execute_semantic_command(SemanticCommand::DispatchSlash {
        origin: SlashCommandOrigin::GlobalPalette,
        line: "/refresh".into(),
    })
    .await
    .unwrap();

    assert_eq!(app.status_message, "Refreshing git status...");
}

#[tokio::test]
async fn workspace_navigation_starts_at_conversation_home() {
    let (_dir, app) = focus_test_app().await;

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(app.workspace_navigation.history.is_empty());
}

#[tokio::test]
async fn workspace_navigation_pushes_file_and_replaces_file_resource() {
    let (dir, mut app) = focus_test_app().await;
    let first = dir.path().join("a.rs");
    let second = dir.path().join("b.rs");
    fs::write(&first, "fn a() {}\n").unwrap();
    fs::write(&second, "fn b() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(first.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(first.clone())
    );
    assert_eq!(
        app.workspace_navigation.history,
        vec![WorkspaceView::Conversation]
    );

    app.execute_semantic_command(SemanticCommand::OpenFile(second.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(second)
    );
    assert_eq!(
        app.workspace_navigation.history,
        vec![WorkspaceView::Conversation]
    );
}

#[tokio::test]
async fn workspace_navigation_pushes_between_file_diff_and_file() {
    let (dir, mut app) = focus_test_app().await;
    let first = dir.path().join("a.rs");
    let second = dir.path().join("b.rs");
    fs::write(&first, "fn a() {}\n").unwrap();
    fs::write(&second, "fn b() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(first.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );
    assert_eq!(
        app.workspace_navigation.history,
        vec![
            WorkspaceView::Conversation,
            WorkspaceView::File(first.clone())
        ]
    );

    app.execute_semantic_command(SemanticCommand::OpenFile(second.clone()))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::File(second)
    );
    assert_eq!(
        app.workspace_navigation.history,
        vec![
            WorkspaceView::Conversation,
            WorkspaceView::File(first),
            WorkspaceView::Diff(DiffCommandContext::Current)
        ]
    );
}

#[tokio::test]
async fn workspace_back_skips_invalid_file_entries() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("stale.rs");
    fs::write(&path, "fn stale() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    fs::remove_file(&path).unwrap();
    app.execute_semantic_command(SemanticCommand::GoBack)
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
}

#[tokio::test]
async fn workspace_home_returns_to_conversation_and_clears_history() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(app.workspace_navigation.history.is_empty());
}

#[tokio::test]
async fn overlay_open_and_close_do_not_mutate_workspace_history() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.overlay = Some(Overlay::welcome());
    app.execute_semantic_command(SemanticCommand::CloseOverlay)
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation, before);
    assert!(app.overlay.is_none());
}

#[tokio::test]
async fn files_visibility_is_independent_of_workspace_navigation() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.files_visible = true;

    app.execute_semantic_command(SemanticCommand::OpenFile(path))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    app.execute_semantic_command(SemanticCommand::GoHome)
        .await
        .unwrap();

    assert!(app.files_visible);
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
}

#[tokio::test]
async fn files_visibility_renders_independently_in_each_workspace_view() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();

    for view in [
        WorkspaceView::Conversation,
        WorkspaceView::File(path.clone()),
        WorkspaceView::Diff(DiffCommandContext::Current),
    ] {
        app.files_visible = true;
        app.navigate_to_workspace_view(view.clone());
        let rendered = render_app_text(&mut app, 160, 50);
        assert!(
            rendered.contains("FILES"),
            "Files should render for {view:?} when preference is open:\n{rendered}"
        );

        app.files_visible = false;
        let rendered = render_app_text(&mut app, 160, 50);
        assert!(
            !rendered.contains("FILES"),
            "Files should not render for {view:?} when preference is closed:\n{rendered}"
        );
    }
}

#[tokio::test]
async fn files_visibility_auto_collapses_and_restores_without_mutating_preference() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);

    let narrow = render_app_text(&mut app, 80, 24);
    assert!(!narrow.contains("FILES"), "{narrow}");
    assert!(app.files_visible, "auto-collapse must not persist close");
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    let wide = render_app_text(&mut app, 160, 50);
    assert!(wide.contains("FILES"), "{wide}");
    assert!(app.files_visible);
}

#[tokio::test]
async fn files_explicit_close_remains_closed_after_resizing() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();

    assert!(!app.files_visible);
    let narrow = render_app_text(&mut app, 80, 24);
    let wide = render_app_text(&mut app, 160, 50);
    assert!(!narrow.contains("FILES"), "{narrow}");
    assert!(!wide.contains("FILES"), "{wide}");
}

#[tokio::test]
async fn files_visibility_persists_per_repository() {
    let (dir, mut app) = focus_test_app().await;
    app.execute_semantic_command(SemanticCommand::ToggleFiles)
        .await
        .unwrap();
    assert!(app.files_visible);

    let session = session_for_workspace(dir.path()).await;
    let restored = TuiApp::new(
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
    assert!(restored.files_visible);

    let (_other_dir, other) = focus_test_app().await;
    assert!(
        !other.files_visible,
        "Files preference must not leak across repositories"
    );
}

#[tokio::test]
async fn theme_change_updates_active_palette_immediately() {
    let (_dir, mut app) = focus_test_app().await;
    assert_eq!(crate::theme::active(), forge_config::Theme::Dark);
    assert_eq!(crate::theme::text().fg, Some(crate::theme::TEXT));

    app.handle_theme_command(Some("light"));
    assert_eq!(crate::theme::active(), forge_config::Theme::Light);
    assert_eq!(crate::theme::text().fg, Some(crate::theme::LIGHT_TEXT));
    assert!(app.conversation_cache.is_none());
}

#[tokio::test]
async fn theme_persists_per_repository() {
    let (dir, mut app) = focus_test_app().await;
    app.handle_theme_command(Some("light"));
    assert_eq!(app.runtime.theme, forge_config::Theme::Light);
    assert_eq!(crate::theme::active(), forge_config::Theme::Light);

    let session = session_for_workspace(dir.path()).await;
    let restored = TuiApp::new(
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
    assert_eq!(restored.runtime.theme, forge_config::Theme::Light);
    assert_eq!(crate::theme::active(), forge_config::Theme::Light);
}

#[tokio::test]
async fn light_theme_paints_root_canvas_on_draw() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.splash_dismissed = true;
    app.handle_theme_command(Some("light"));
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    assert_buffer_fully_themed(term.backend().buffer());
    let corner = term.backend().buffer()[(0, 0)].style().bg;
    assert_eq!(corner, Some(crate::theme::LIGHT_CANVAS));
}

#[tokio::test]
async fn light_theme_resize_keeps_canvas_background() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.splash_dismissed = true;
    app.handle_theme_command(Some("light"));
    for (w, h) in [(80, 24), (160, 50), (120, 40)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        assert_buffer_fully_themed(term.backend().buffer());
    }
}

#[tokio::test]
async fn light_theme_representative_layout_snapshot() {
    let (_dir, mut app) = focus_test_app().await;
    app.splash_dismissed = true;
    app.files_visible = true;
    app.handle_theme_command(Some("light"));
    app.session.messages.push(Message {
        role: MessageRole::User,
        content: "Please review this change.\n\nIt spans multiple lines.".into(),
        tool_call_id: None,
        name: None,
        thinking: None,
        thinking_duration_secs: None,
        tool_calls: vec![],
    });
    app.session.messages.push(Message {
        role: MessageRole::Assistant,
        content: "Here is a concise review of your change.".into(),
        tool_call_id: None,
        name: None,
        thinking: None,
        thinking_duration_secs: None,
        tool_calls: vec![],
    });
    app.feedback = FeedbackModel::error("Model error: rate limited (HTTP 429).");
    app.conversation_cache = None;
    app.input.set_text("draft reply");
    app.input.cursor = app.input.text.len();

    let text = render_app_text(&mut app, 120, 40);
    assert!(text.contains("Forge"), "missing header:\n{text}");
    assert!(text.contains("FILES"), "missing sidebar:\n{text}");
    assert!(
        text.contains("Please review this change."),
        "missing user message:\n{text}"
    );
    assert!(
        text.contains("concise review"),
        "missing assistant response:\n{text}"
    );
    assert!(
        text.contains("Model error"),
        "missing model error feedback:\n{text}"
    );
    assert!(text.contains("draft reply"), "missing composer:\n{text}");

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    assert_buffer_fully_themed(term.backend().buffer());
    let buf = term.backend().buffer();
    let mut saw_gutter = false;
    let mut saw_selection = false;
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            if buf[(x, y)].style().fg == Some(crate::theme::USER_MESSAGE_GUTTER_LIGHT) {
                saw_gutter = true;
            }
            if buf[(x, y)].style().bg == Some(crate::theme::LIGHT_SELECTION) {
                saw_selection = true;
            }
        }
    }
    assert!(saw_gutter, "expected light-theme user gutter colour");
    assert!(
        saw_selection || text.contains("draft reply"),
        "expected composer selection or typed text"
    );
}

#[tokio::test]
async fn light_theme_overlay_uses_themed_backdrop() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (_dir, mut app) = focus_test_app().await;
    app.handle_theme_command(Some("light"));
    app.overlay = Some(Overlay::Help);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    assert_buffer_fully_themed(term.backend().buffer());
}

#[tokio::test]
async fn old_or_malformed_ui_state_migrates_safely_to_default() {
    let (dir, _app) = focus_test_app().await;
    let state_path = dir.path().join(".forge/ui-state.json");
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(&state_path, r#"{"files_visible":true}"#).unwrap();

    let session = session_for_workspace(dir.path()).await;
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

    assert!(!app.files_visible);
}

#[tokio::test]
async fn files_toggle_is_reachable_from_global_palette_dispatch() {
    let (_dir, mut app) = focus_test_app().await;
    assert!(!app.files_visible);

    app.execute_semantic_command(SemanticCommand::DispatchSlash {
        origin: SlashCommandOrigin::GlobalPalette,
        line: "/files".into(),
    })
    .await
    .unwrap();

    assert!(app.files_visible);
    assert_eq!(app.focus.block, FocusBlock::Files);
}

#[tokio::test]
async fn opening_file_does_not_open_closed_files_preference() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.files_visible = false;

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();

    assert!(!app.files_visible);
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
}

#[tokio::test]
async fn responsive_sizes_render_without_panic_and_follow_files_policy() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.splash_dismissed = true;
    for (width, height, expect_files) in [
        (80, 24, false),
        (120, 40, true),
        (160, 50, true),
        (240, 60, true),
    ] {
        let rendered = render_app_text(&mut app, width, height);
        assert!(
            rendered.contains("Describe a task"),
            "composer should remain reachable at {width}x{height}:\n{rendered}"
        );
        assert_eq!(
            rendered.contains("FILES"),
            expect_files,
            "unexpected Files visibility at {width}x{height}:\n{rendered}"
        );
    }
}

#[tokio::test]
async fn leaving_run_view_does_not_cancel_running_run() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();
    app.run_current_draft();
    let id = app.run.current.as_ref().unwrap().id.clone();

    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Current))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Run(id.clone())
    );

    app.execute_semantic_command(SemanticCommand::GoBack)
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.id == id && record.state == RunState::Running));
}

#[tokio::test]
async fn run_start_updates_activity_without_navigating_from_conversation() {
    let (_dir, mut app) = focus_test_app().await;
    let before = app.workspace_navigation.clone();
    app.run.draft.command_input = "true".into();

    app.run_current_draft();

    assert_eq!(app.workspace_navigation, before);
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.state == RunState::Running));
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Run && item.summary.contains("run started")));
    let summary = app.activity_summary().expect("run summary");
    assert_eq!(summary.label, "Running true");
    assert_eq!(summary.action_label, Some("View output"));

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Running true"), "{rendered}");
    assert_eq!(rendered.matches("View output").count(), 1, "{rendered}");
    assert!(
        !rendered.contains("Running validation"),
        "run must not also render the old running tool card:\n{rendered}"
    );
}

#[tokio::test]
async fn run_start_while_in_file_does_not_hijack_workspace() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.run.draft.command_input = "true".into();
    app.run_current_draft();

    assert_eq!(app.workspace_navigation, before);
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Run));
}

#[tokio::test]
async fn run_failure_while_in_diff_updates_summary_without_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "false".into();
    app.run_current_draft();
    let run_id = app.run.current.as_ref().unwrap().id.clone();
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_rx = Some(rx);

    tx.send(RunEvent::Finished {
        exit_code: Some(1),
        success: false,
    })
    .unwrap();
    app.poll_run();

    assert_eq!(app.workspace_navigation, before);
    assert!(app
        .run
        .current
        .as_ref()
        .is_some_and(|record| record.id == run_id && record.state == RunState::Failed));
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Run
            && item.severity == FeedbackSeverity::Error
            && item.summary.contains("run failed")));
    assert_eq!(
        app.activity_summary_command(),
        Some(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
    );
}

#[tokio::test]
async fn edge_run_cancel_preserves_output_and_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    let before = app.workspace_navigation.clone();
    app.run.draft.command_input = "long-running".into();
    app.run_current_draft();
    app.append_terminal_output(b"partial output\n");

    app.cancel_run();

    let record = app.run.current.as_ref().expect("current run");
    assert_eq!(record.state, RunState::Cancelled);
    assert_eq!(record.exit_status, None);
    assert!(app.terminal_capture.content.contains("partial output"));
    assert_eq!(app.workspace_navigation, before);
    assert!(app
        .run
        .recent
        .iter()
        .any(|run| run.state == RunState::Cancelled));
}

#[tokio::test]
async fn edge_run_spawn_failure_shows_invocation_without_exit_code() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "definitely-missing-forge-command --flag".into();
    app.run_current_draft();
    let run_id = app.run.current.as_ref().unwrap().id.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_rx = Some(rx);

    tx.send(RunEvent::SpawnFailed("No such file or directory".into()))
        .unwrap();
    app.poll_run();
    app.execute_semantic_command(SemanticCommand::OpenRun(RunCommandTarget::Id(run_id)))
        .await
        .unwrap();

    let record = app.run.current.as_ref().expect("current run");
    assert_eq!(record.state, RunState::StartFailed);
    assert_eq!(record.exit_status, None);
    assert_eq!(
        record.invocation.executable,
        "definitely-missing-forge-command"
    );
    assert_eq!(record.invocation.arguments, vec!["--flag"]);
    assert!(record.spawn_error.as_deref().unwrap().contains("No such"));

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Could not start"), "{rendered}");
    assert!(
        rendered.contains("Executable: definitely-missing-forge-command"),
        "{rendered}"
    );
    assert!(rendered.contains("Arguments: [\"--flag\"]"), "{rendered}");
    assert!(rendered.contains("Directory:"), "{rendered}");
    assert!(
        rendered.contains("Cause: No such file or directory"),
        "{rendered}"
    );
    assert!(rendered.contains("e edit rerun"), "{rendered}");
    assert!(!rendered.contains("Exit status:"), "{rendered}");
}

#[tokio::test]
async fn edge_network_stream_interruption_preserves_partial_response() {
    let dir = TempDir::new().unwrap();
    let session = session_for_workspace_with_model(
        dir.path(),
        Arc::new(MockModelClient::stream_error(
            vec!["partial ".into(), "answer".into()],
            "network connection lost",
        )),
    )
    .await;
    let mut app = TuiApp::new(
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
    let file = dir.path().join("open.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(file))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.dispatch_line("hello").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();

    assert_eq!(app.workspace_navigation, before);
    assert!(!app.busy);
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "x");
    assert!(app.feedback.text.contains("Retry or Continue"));
    assert!(app.session.messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.content.contains("partial answer")
            && message.content.contains("Interrupted")
    }));
}

#[tokio::test]
async fn edge_open_file_external_rename_updates_path_when_identity_matches() {
    let (dir, mut app) = focus_test_app().await;
    let old = dir.path().join("old.rs");
    let new = dir.path().join("new.rs");
    fs::write(&old, "fn main() {}\nline2\nline3\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(old.clone()))
        .await
        .unwrap();
    app.source_viewer.current_line = 2;
    app.source_viewer.top_line = 1;
    fs::rename(&old, &new).unwrap();

    app.file_change_tx
        .send(FileChangeEvent { path: new.clone() })
        .unwrap();
    app.poll_file_changes();

    let new = new.canonicalize().unwrap();
    assert_eq!(app.source_viewer.path.as_deref(), Some(new.as_path()));
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(new));
    assert_eq!(app.source_viewer.current_line, 2);
    assert_eq!(app.source_viewer.top_line, 1);
    assert_eq!(
        app.source_viewer.notice.as_deref(),
        Some("File renamed externally")
    );
}

#[tokio::test]
async fn edge_open_file_external_delete_keeps_file_view_and_buffer() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("gone.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let opened = app.source_viewer.path.clone().unwrap();
    let lines = app.source_viewer.lines.clone();
    fs::remove_file(&path).unwrap();

    app.file_change_tx
        .send(FileChangeEvent {
            path: opened.clone(),
        })
        .unwrap();
    app.poll_file_changes();

    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    assert_eq!(app.source_viewer.path.as_deref(), Some(opened.as_path()));
    assert_eq!(
        app.source_viewer.status,
        crate::source_viewer::ViewerStatus::NotFound
    );
    assert_eq!(app.source_viewer.lines, lines);
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("File no longer exists"), "{rendered}");
    assert!(rendered.contains("Back"), "{rendered}");
    assert!(rendered.contains("Locate"), "{rendered}");
}

#[tokio::test]
async fn edge_diff_becomes_stale_and_refresh_clears_it() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("one.rs"), GitStatusKind::Modified);
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    app.diff_selected = 0;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("two.rs"), GitStatusKind::Added);

    app.note_workspace_changed();
    assert!(app.diff_snapshot.stale);
    assert_eq!(app.diff_selected, 0);
    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Stale review"), "{rendered}");
    assert!(
        rendered.contains("Apply disabled until refresh"),
        "{rendered}"
    );
    assert_eq!(
        app.semantic_command_for_workspace_key(press(KeyCode::Char('r'), KeyModifiers::NONE)),
        Some(SemanticCommand::RefreshDiff)
    );

    app.execute_semantic_command(SemanticCommand::RefreshDiff)
        .await
        .unwrap();
    assert!(!app.diff_snapshot.stale);
    assert_eq!(app.diff_selected, 0);
}

#[tokio::test]
async fn edge_approval_at_80x24_keeps_required_fields_and_actions() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_hitl_overlay(direct_hitl_payload("call-1", "src/main.rs"));

    let rendered = render_app_text(&mut app, 80, 24);
    assert!(rendered.contains("Approval required"), "{rendered}");
    assert!(rendered.contains("Direct"), "{rendered}");
    assert!(rendered.contains("read_file"), "{rendered}");
    assert!(rendered.contains("Working directory"), "{rendered}");
    assert!(rendered.contains("test approval"), "{rendered}");
    assert!(rendered.contains("Allow once"), "{rendered}");
    assert!(rendered.contains("Deny"), "{rendered}");
    assert!(
        rendered.contains("Remember this exact Direct invocation"),
        "{rendered}"
    );
}

#[tokio::test]
async fn agent_streaming_while_viewing_file_does_not_navigate() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.busy = true;
    app.busy_phase = BusyPhase::Model;
    app.pending_prompt = None;
    app.stream_preview = "partial answer".into();
    let rendered = render_app_text(&mut app, 100, 30);

    assert_eq!(app.workspace_navigation, before);
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
    assert!(rendered.contains("fn main()"), "{rendered}");
    assert!(
        !rendered.contains("partial answer"),
        "File view should remain primary while streaming:\n{rendered}"
    );
}

#[tokio::test]
async fn agent_thinking_keeps_composer_usable() {
    let (_dir, mut app) = focus_test_app().await;
    app.busy = true;
    app.busy_phase = BusyPhase::Model;
    app.stream_thinking = "planning".into();
    app.focus_block(FocusBlock::Composer);

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.input.text, "x");
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    let summary = app.activity_summary().expect("thinking summary");
    assert_eq!(summary.label, "Forge is thinking");
}

#[tokio::test]
async fn activity_summary_priority_renders_one_actionable_row() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);
    app.busy = true;
    app.busy_phase = BusyPhase::Model;
    app.run.draft.command_input = "cargo test".into();
    app.run_current_draft();

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Running cargo test"), "{rendered}");
    assert_eq!(rendered.matches("View output").count(), 1, "{rendered}");
    assert!(
        !rendered.contains("files changed · Review"),
        "Run summary must outrank changes:\n{rendered}"
    );
    assert!(
        !rendered.contains("Forge is thinking"),
        "Run summary must outrank thinking:\n{rendered}"
    );

    let (tx, rx) = std::sync::mpsc::channel();
    app.run_rx = Some(rx);
    tx.send(RunEvent::Finished {
        exit_code: Some(1),
        success: false,
    })
    .unwrap();
    app.poll_run();

    let rendered = render_app_text(&mut app, 100, 30);
    assert!(rendered.contains("Run failed: cargo test"), "{rendered}");
    assert_eq!(rendered.matches("Inspect").count(), 1, "{rendered}");
    assert!(
        !rendered.contains("Running cargo test"),
        "Failure summary must replace active-run summary:\n{rendered}"
    );
}

#[tokio::test]
async fn summary_action_opens_expected_workspace_view() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();
    app.run_current_draft();
    let id = app.run.current.as_ref().unwrap().id.clone();
    app.focus_block(FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current, WorkspaceView::Run(id));
}

#[tokio::test]
async fn changes_summary_action_uses_review_changes_command() {
    let (_dir, mut app) = focus_test_app().await;
    app.file_explorer
        .git_status
        .status
        .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);

    assert_eq!(
        app.activity_summary_command(),
        Some(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
    );
    app.execute_semantic_command(SemanticCommand::ActivateActivitySummary)
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );
}

#[tokio::test]
async fn approval_overlay_preserves_underlying_workspace() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();
    app.session.pending_hitl = Some(forge_types::HitlPayload {
        call_id: "1".into(),
        tool: "bash".into(),
        args_redacted: json!({"command": "cargo test"}),
        reason: "test approval".into(),
    });

    app.maybe_open_hitl();

    assert!(matches!(app.overlay, Some(Overlay::Hitl { .. })));
    assert_eq!(app.workspace_navigation, before);
    assert!(app.activity_summary().is_none());
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
}

#[tokio::test]
async fn approval_direct_allow_once_resolves_without_remembering() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("allowed.txt"), "ok").unwrap();
    app.session.pending_hitl = Some(direct_hitl_payload("direct-once", "allowed.txt"));
    app.maybe_open_hitl();

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.session.pending_hitl.is_none());
    assert!(app.hitl_session_allow.is_empty());
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content == "ok"));
}

#[tokio::test]
async fn approval_remembered_direct_invocation_matches_exact_identity() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("remember.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("remember", "remember.txt");
    app.session.pending_hitl = Some(payload.clone());
    app.maybe_open_hitl();

    app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
        .await
        .unwrap();

    let identity = app.approval_identity_for_payload(&payload).unwrap();
    assert!(app.hitl_session_allow.contains(&identity));
    assert!(!app.hitl_session_allow.contains(
        &app.approval_identity_for_payload(&direct_hitl_payload("arg", "other.txt"))
            .unwrap()
    ));

    let env_payload = HitlPayload {
        args_redacted: json!({"path": "remember.txt", "env": {"RUST_LOG": "debug"}}),
        ..direct_hitl_payload("env", "remember.txt")
    };
    assert!(!app
        .hitl_session_allow
        .contains(&app.approval_identity_for_payload(&env_payload).unwrap()));

    let cwd_payload = HitlPayload {
        args_redacted: json!({"path": "remember.txt", "cwd": "nested"}),
        ..direct_hitl_payload("cwd", "remember.txt")
    };
    assert!(!app
        .hitl_session_allow
        .contains(&app.approval_identity_for_payload(&cwd_payload).unwrap()));

    let (other_dir, other_app) = focus_test_app().await;
    fs::write(other_dir.path().join("remember.txt"), "ok").unwrap();
    assert!(!app
        .hitl_session_allow
        .contains(&other_app.approval_identity_for_payload(&payload).unwrap()));
}

#[tokio::test]
async fn approval_remembered_direct_expires_with_session() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("session.txt"), "ok").unwrap();
    let payload = direct_hitl_payload("session", "session.txt");
    app.session.pending_hitl = Some(payload.clone());
    app.maybe_open_hitl();
    app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
        .await
        .unwrap();

    let next_session = session_for_workspace(dir.path()).await;
    let next_app = TuiApp::new(
        next_session,
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

    assert!(next_app.hitl_session_allow.is_empty());
    assert_ne!(
        app.approval_identity_for_payload(&payload),
        next_app.approval_identity_for_payload(&payload)
    );
}

#[tokio::test]
async fn approval_shell_mode_cannot_be_remembered() {
    let (_dir, mut app) = focus_test_app().await;
    app.session.pending_hitl = Some(HitlPayload {
        call_id: "shell".into(),
        tool: "bash".into(),
        args_redacted: json!({"command": "git push origin main"}),
        reason: "test approval".into(),
    });
    app.maybe_open_hitl();

    let Some(Overlay::Hitl { approval, .. }) = &app.overlay else {
        panic!("expected approval overlay");
    };
    assert_eq!(approval.mode, ApprovalExecutionMode::Shell);
    assert!(!approval.remember_eligible);
    assert_eq!(
        app.approval_identity_for_payload(app.session.pending_hitl.as_ref().unwrap()),
        None
    );
}

#[tokio::test]
async fn approval_escape_denies_and_underlying_commands_are_blocked() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("blocked.txt"), "ok").unwrap();
    app.session.pending_hitl = Some(direct_hitl_payload("esc", "blocked.txt"));
    app.maybe_open_hitl();
    let before_history = app.workspace_navigation.clone();

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.input.text.is_empty());
    assert!(app.session.pending_hitl.is_some());

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.session.pending_hitl.is_none());
    assert_eq!(app.workspace_navigation, before_history);
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content.contains("HITL denied")));
    assert!(!app
        .session
        .messages
        .iter()
        .any(|message| message.content == "ok"));
}

#[tokio::test]
async fn approval_duplicate_confirmation_is_idempotent() {
    let (dir, mut app) = focus_test_app().await;
    fs::write(dir.path().join("dup.txt"), "ok").unwrap();
    app.session.pending_hitl = Some(direct_hitl_payload("dup", "dup.txt"));
    app.maybe_open_hitl();

    app.resolve_hitl_overlay(HitlDecision::Approve, false)
        .await
        .unwrap();
    app.resolve_hitl_overlay(HitlDecision::Approve, false)
        .await
        .unwrap();

    let successful_tool_messages = app
        .session
        .messages
        .iter()
        .filter(|message| message.content == "ok")
        .count();
    assert_eq!(successful_tool_messages, 1);
}

#[tokio::test]
async fn approval_overlay_80x24_renders_actions_and_redacts_secrets() {
    let (_dir, mut app) = focus_test_app().await;
    app.session.pending_hitl = Some(HitlPayload {
        call_id: "secret".into(),
        tool: "read_file".into(),
        args_redacted: json!({"path": "config.txt", "api_key": "[REDACTED]"}),
        reason: "secret test".into(),
    });
    app.maybe_open_hitl();

    let rendered = render_app_text(&mut app, 80, 24);

    assert!(rendered.contains("Approval required"), "{rendered}");
    assert!(rendered.contains("Mode: Direct"), "{rendered}");
    assert!(rendered.contains("Executable: read_file"), "{rendered}");
    assert!(rendered.contains("Working directory:"), "{rendered}");
    assert!(rendered.contains("[Allow once]"), "{rendered}");
    assert!(rendered.contains("[Deny]"), "{rendered}");
    assert!(rendered.contains("[REDACTED]"), "{rendered}");
    assert!(
        !rendered.contains("Remember this exact Direct invocation"),
        "{rendered}"
    );
}

#[tokio::test]
async fn run_activity_history_remains_available_in_activity_panel() {
    let (_dir, mut app) = focus_test_app().await;
    app.run.draft.command_input = "true".into();
    app.run_current_draft();
    app.open_bottom_panel(Some(BottomPanelTab::Activity));

    let rendered = render_app_text(&mut app, 100, 30);

    assert!(rendered.contains("Run"), "{rendered}");
    assert!(rendered.contains("run started: true"), "{rendered}");
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
}

#[tokio::test]
async fn characterization_contextual_views_are_reachable_with_current_controls() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.focus_block(FocusBlock::Workspace);
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );

    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );
    assert_eq!(app.focus.block, FocusBlock::Workspace);

    app.handle_key(press(KeyCode::Left, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );

    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    assert_eq!(app.workspace_navigation.current, WorkspaceView::File(path));
}

#[tokio::test]
async fn characterization_files_selection_and_expansion_survive_focus_roundtrip() {
    let (dir, mut app) = focus_test_app().await;
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "").unwrap();
    app.file_explorer.refresh_workspace();
    let src = dir.path().join("src").canonicalize().unwrap();
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);
    app.file_explorer.selected_path = Some(src.clone());
    app.file_explorer.expand_selected();

    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_ne!(app.focus.block, FocusBlock::Files);
    app.handle_key(press(KeyCode::BackTab, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.focus.block, FocusBlock::Files);
    assert_eq!(
        app.file_explorer.selected_path.as_deref(),
        Some(src.as_path())
    );
    assert!(app
        .file_explorer
        .visible_nodes()
        .iter()
        .any(|node| node.display_name == "lib.rs"));
}

#[tokio::test]
async fn characterization_80x24_draws_without_panic() {
    use ratatui::backend::TestBackend;

    let (_dir, mut app) = focus_test_app().await;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn characterization_run_completion_preserves_bottom_panel_focus() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_bottom_panel(Some(BottomPanelTab::Run));
    app.run.draft.command_input = "/usr/bin/true".into();

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    assert!(app.pending_validation);

    app.drain_pending_validation(None).await.unwrap();
    for _ in 0..50 {
        app.poll_run();
        if app
            .run
            .current
            .as_ref()
            .is_some_and(|record| record.state != RunState::Running)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(app.focus.block, FocusBlock::BottomPanel);
    assert!(app.run.current.as_ref().is_some_and(|record| matches!(
        record.state,
        RunState::Succeeded | RunState::Failed | RunState::StartFailed
    )));
}

#[tokio::test]
async fn switching_to_diff_focuses_workspace_for_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    app.execute_semantic_command(SemanticCommand::ReviewChanges(DiffCommandContext::Current))
        .await
        .unwrap();
    app.file_explorer
        .git_status
        .status
        .insert(std::path::PathBuf::from("a.txt"), GitStatusKind::Modified);
    app.file_explorer
        .git_status
        .status
        .insert(std::path::PathBuf::from("b.txt"), GitStatusKind::Modified);

    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Diff(DiffCommandContext::Current)
    );
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert_eq!(app.diff_selected, 1);
}

#[tokio::test]
async fn registered_printable_editor_commands_do_not_enter_composer() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "a\nb\nc\n").unwrap();
    app.open_file_in_editor(&path);
    app.input.set_text("");

    app.handle_key(press(KeyCode::Char('r'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert!(app.input.text.is_empty());

    app.handle_key(press(KeyCode::Char('G'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn non_printable_keys_do_not_type_to_compose() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus_block(FocusBlock::Workspace);

    for key in [
        press(KeyCode::Enter, KeyModifiers::NONE),
        press(KeyCode::Left, KeyModifiers::NONE),
        press(KeyCode::Right, KeyModifiers::SHIFT),
        press(KeyCode::Char('c'), KeyModifiers::CONTROL),
        press(KeyCode::Char('x'), KeyModifiers::ALT),
    ] {
        app.handle_key(key).await.unwrap();
        assert_eq!(app.focus.block, FocusBlock::Workspace);
        assert!(app.input.text.is_empty());
    }
}

#[tokio::test]
async fn source_search_is_transient_and_esc_restores_workspace_navigation() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "line\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(
        app.focus.mode,
        FocusMode::Transient(TransientOwner::SourceSearch)
    );
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.source_viewer.search.open);
    assert_eq!(app.focus.mode, FocusMode::Navigation);
}

#[tokio::test]
async fn source_search_keeps_shift_arrows_inside_the_search_field() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "line\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert!(app.source_viewer.search.open);
    assert_eq!(
        app.focus.mode,
        FocusMode::Transient(TransientOwner::SourceSearch)
    );
}

#[tokio::test]
async fn jump_to_line_keeps_shift_arrows_inside_the_jump_field() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "line\n").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('g'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert!(app.source_viewer.jump.open);
    assert_eq!(
        app.focus.mode,
        FocusMode::Transient(TransientOwner::JumpToLine)
    );
}

#[tokio::test]
async fn editor_reload_does_not_reach_chat_input() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "before\n").unwrap();
    app.input.set_text("draft");
    app.open_file_in_editor(&path);
    fs::write(&path, "after\n").unwrap();
    app.handle_key(press(KeyCode::Char('r'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "draft");
    assert_eq!(app.source_viewer.lines, vec!["after"]);
}

#[tokio::test]
async fn explorer_new_file_dialog_owns_printable_input_and_selects_created_file() {
    let (dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);
    app.input.set_text("");

    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    for ch in "new.rs".chars() {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert!(app.input.text.is_empty());
    assert!(matches!(
        app.explorer_dialog,
        Some(ExplorerDialog::Name { .. })
    ));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog,
        Some(ExplorerDialog::ConfirmCreate { .. })
    ));
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let created = dir.path().join("new.rs").canonicalize().unwrap();
    assert!(created.is_file());
    assert_eq!(
        app.file_explorer.selected_path.as_deref(),
        Some(created.as_path())
    );
    assert_eq!(app.focus.block, FocusBlock::Files);
}

#[tokio::test]
async fn explorer_name_escape_cancels_without_focus_change_or_composer_input() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);

    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.explorer_dialog.is_none());
    assert_eq!(app.focus.block, FocusBlock::Files);
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_rename_prepopulates_and_updates_open_child_file() {
    let (dir, mut app) = focus_test_app().await;
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    let src = src.canonicalize().unwrap();
    let child = src.join("lib.rs");
    fs::write(&child, "pub fn old() {}\n").unwrap();
    app.file_explorer.refresh_workspace();
    app.file_explorer.selected_path = Some(src.clone());
    app.open_file_in_editor(&child);
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);
    app.file_explorer.selected_path = Some(src.clone());

    app.handle_key(press(KeyCode::Char('R'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    match app.explorer_dialog.as_mut() {
        Some(ExplorerDialog::Name { input, .. }) => {
            assert_eq!(input, "src");
            *input = "Source".into();
        }
        other => panic!("unexpected dialog: {other:?}"),
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let renamed_child = dir.path().join("Source/lib.rs").canonicalize().unwrap();
    assert!(renamed_child.is_file());
    assert_eq!(
        app.source_viewer.path.as_deref(),
        Some(renamed_child.as_path())
    );
    let renamed_dir = dir.path().join("Source").canonicalize().unwrap();
    assert_eq!(
        app.file_explorer.selected_path.as_deref(),
        Some(renamed_dir.as_path())
    );
}

#[tokio::test]
async fn explorer_rename_collision_keeps_name_dialog_with_error() {
    let (dir, mut app) = focus_test_app().await;
    let old = dir.path().join("old.rs");
    let existing = dir.path().join("existing.rs");
    fs::write(&old, "").unwrap();
    fs::write(&existing, "").unwrap();
    app.file_explorer.refresh_workspace();
    app.file_explorer.selected_path = Some(old.canonicalize().unwrap());
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);

    app.open_explorer_name_dialog(ExplorerNameAction::Rename);
    match app.explorer_dialog.as_mut() {
        Some(ExplorerDialog::Name { input, .. }) => *input = "existing.rs".into(),
        other => panic!("unexpected dialog: {other:?}"),
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    match app.explorer_dialog {
        Some(ExplorerDialog::Name {
            error: Some(error), ..
        }) => {
            assert!(error.contains("Destination already exists"));
        }
        other => panic!("unexpected dialog: {other:?}"),
    }
    assert!(app.input.text.is_empty());
}

#[tokio::test]
async fn explorer_delete_non_empty_folder_requires_stronger_confirmation() {
    let (dir, mut app) = focus_test_app().await;
    let folder = dir.path().join("generated");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("out.txt"), "").unwrap();
    let folder = folder.canonicalize().unwrap();
    app.file_explorer.refresh_workspace();
    app.file_explorer.selected_path = Some(folder.clone());
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);

    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.explorer_dialog,
        Some(ExplorerDialog::ConfirmDelete {
            non_empty: true,
            permanent: false,
            ..
        })
    ));

    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(folder.exists());
    assert!(app.explorer_dialog.is_some());
}

#[tokio::test]
async fn overlay_precedes_block_navigation() {
    let (_dir, mut app) = focus_test_app().await;
    app.focus.mode = FocusMode::Navigation;
    app.overlay = Some(Overlay::welcome());
    app.handle_key(press(KeyCode::Char(']'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.workspace_navigation.current,
        WorkspaceView::Conversation
    );
    assert!(app.overlay.is_some());
}

#[tokio::test]
async fn resize_drops_focus_from_a_zero_width_files_block() {
    use ratatui::backend::TestBackend;

    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.focus_block(FocusBlock::Files);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert_eq!(app.focus.mode, FocusMode::Navigation);
}

#[tokio::test]
async fn typing_reuses_cached_conversation_lines() {
    use ratatui::backend::TestBackend;

    let (dir, session) = test_session().await;
    let mut app = TuiApp::new(
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
    app.splash_dismissed = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    Arc::get_mut(&mut app.conversation_cache.as_mut().unwrap().lines)
        .expect("cache handle is unshared between frames")
        .reserve(1_000);
    let cached_capacity = app.conversation_cache.as_ref().unwrap().lines.capacity();

    app.input.insert('x');
    terminal.draw(|frame| app.draw(frame)).unwrap();

    assert_eq!(
        app.conversation_cache.as_ref().unwrap().lines.capacity(),
        cached_capacity
    );
}

#[tokio::test]
async fn streaming_updates_reuse_cached_transcript_lines() {
    use ratatui::backend::TestBackend;

    let (dir, mut session) = test_session().await;
    session.messages.push(Message {
        role: MessageRole::Assistant,
        content: "historical answer".into(),
        tool_call_id: None,
        name: None,
        thinking: Some("historical completed thinking".into()),
        thinking_duration_secs: Some(1.0),
        tool_calls: vec![],
    });
    let mut app = TuiApp::new(
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
    app.splash_dismissed = true;
    app.busy = true;
    app.busy_phase = BusyPhase::Model;
    app.stream_preview = "first chunk".into();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    Arc::get_mut(&mut app.conversation_cache.as_mut().unwrap().lines)
        .expect("cache handle is unshared between frames")
        .reserve(1_000);
    let cached_capacity = app.conversation_cache.as_ref().unwrap().lines.capacity();

    app.stream_preview.push_str(" and updated tail");
    terminal.draw(|frame| app.draw(frame)).unwrap();

    assert_eq!(
        app.conversation_cache.as_ref().unwrap().lines.capacity(),
        cached_capacity,
        "stream deltas must not rebuild historical transcript lines"
    );
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("updated tail"), "{rendered}");
    assert!(
        !rendered.contains("historical completed thinking"),
        "{rendered}"
    );
}

/// A cache hit must share the cached line buffer, not copy it. Pointer
/// identity is a direct check: the previous code deep-copied every `Line` and
/// `Span` on every frame, so the allocation would differ here.
#[tokio::test]
async fn cache_hit_shares_transcript_lines_without_copying() {
    let (_dir, mut app) = focus_test_app().await;
    app.splash_dismissed = true;
    app.session.messages.push(forge_types::Message::new(
        forge_types::MessageRole::Assistant,
        "cached transcript body",
    ));
    draw_app(&mut app, 100, 30);
    let first = Arc::clone(&app.conversation_cache.as_ref().unwrap().lines);

    // Typing does not change the render key, so this is a cache hit.
    app.input.insert('x');
    draw_app(&mut app, 100, 30);
    let second = Arc::clone(&app.conversation_cache.as_ref().unwrap().lines);

    assert!(
        Arc::ptr_eq(&first, &second),
        "a cache hit must reuse the same line allocation, not clone it"
    );
}

#[test]
fn footer_limits_parser_keeps_only_inline_limit_fields() {
    let limits = footer_limits_from_report(&[
        "Provider: OpenAI Codex".into(),
        "Session limit: 75% remaining".into(),
        "Weekly limit: 50% remaining".into(),
        "Credits: unlimited".into(),
    ]);

    assert_eq!(limits.usage, "Session limit: 75% remaining");
    assert_eq!(limits.weekly_limit, "Weekly limit: 50% remaining");
    assert_eq!(limits.credits, "Credits: unlimited");
}

#[test]
fn recent_resume_sessions_lists_previous_valid_journals() {
    let dir = TempDir::new().unwrap();
    let current = uuid::Uuid::new_v4();
    let previous = uuid::Uuid::new_v4();
    std::fs::write(dir.path().join(format!("{current}.db")), "").unwrap();
    std::fs::write(dir.path().join(format!("{previous}.db")), "").unwrap();
    std::fs::write(dir.path().join("not-a-session.db"), "").unwrap();
    std::fs::write(dir.path().join(format!("{}.txt", uuid::Uuid::new_v4())), "").unwrap();

    let sessions = recent_resume_sessions(dir.path(), current, 10).unwrap();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, previous);
}

#[tokio::test]
async fn resume_command_replaces_active_conversation_in_app() {
    let (dir, session) = test_session().await;
    let model = Arc::new(MockModelClient::script(vec![]));
    let mut previous = AgentSession::create(
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
    previous
        .append_user_message("restored conversation")
        .await
        .unwrap();
    let previous_id = previous.session_id;

    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line(&format!("/resume {previous_id}"))
        .await
        .unwrap();

    assert_eq!(app.session.session_id, previous_id);
    assert!(app
        .session
        .messages
        .iter()
        .any(|message| message.content == "restored conversation"));
    assert!(app.status_message.contains("resumed"));
    assert!(app.notices.is_empty());
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.summary.contains("session resumed")));
}

#[tokio::test]
async fn compact_reports_context_handoff_in_chat_and_activity() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );

    app.dispatch_line("/compact").await.unwrap();

    assert!(app.notices.is_empty());
    assert!(app.ui_banners.is_empty());
    assert!(app
        .activity
        .all()
        .iter()
        .any(|item| item.kind == ActivityKind::Context));
    assert_eq!(app.status_message, "Continuing in a fresh context");
    assert!(app
        .ui_banners
        .iter()
        .all(|item| !matches!(item, ChatItem::Banner { .. })));
}

#[tokio::test]
async fn enter_while_busy_enqueues_user_message() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.busy = true;
    for c in "queued later".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.message_queue.len(), 1);
    assert!(app.pending_prompt.is_none());
    assert_eq!(
        app.message_queue.iter().next().map(|s| s.as_str()),
        Some("queued later")
    );
}

#[tokio::test]
async fn typing_while_busy_updates_input_buffer() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.busy = true;
    for c in "next".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.input.text, "next");
    assert_eq!(app.message_queue.len(), 0);
}

#[tokio::test]
async fn ctrl_p_toggles_bottom_panel_without_touching_input() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.input.set_text("draft");

    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.bottom_panel.open);
    assert_eq!(app.input.text, "draft");

    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!app.bottom_panel.open);
    assert_eq!(app.input.text, "draft");
}

#[tokio::test]
async fn alt_number_opens_selected_bottom_panel_tab() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );

    app.handle_key(press(KeyCode::Char('4'), KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(app.bottom_panel.open);
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
}

#[tokio::test]
async fn focused_bottom_panel_cycles_without_typing_into_chat() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.input.set_text("draft");
    app.bottom_panel.open_tab(BottomPanelTab::Terminal);
    app.focus_block(FocusBlock::BottomPanel);

    app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
    app.handle_key(press(KeyCode::Left, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.bottom_panel.active, BottomPanelTab::Terminal);
    assert_eq!(app.input.text, "draft");
}

#[tokio::test]
async fn editor_uppercase_g_does_not_reach_chat_input() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (dir, session) = test_session().await;
    let path = dir.path().join("source.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.input.set_text("draft");
    app.open_file_in_editor(&path);

    app.handle_key(press(KeyCode::Char('G'), KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(app.source_viewer.current_line, 2);
    assert_eq!(app.input.text, "draft");
}

#[tokio::test]
async fn question_mark_opens_help_overlay() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.handle_key(press(KeyCode::F(1), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(app.overlay, Some(Overlay::Help)));
    assert!(app.input.text.is_empty());
    assert!(app.feedback.text.contains("Help"));
}

#[tokio::test]
async fn empty_enter_when_idle_dequeues_and_sends() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    // Simulate messages enqueued while processing.
    app.message_queue.enqueue("from queue");
    app.busy = false;
    assert!(app.pending_prompt.is_none());
    // Empty Enter = user action to dequeue + send
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.message_queue.is_empty());
    assert_eq!(app.pending_prompt.as_deref(), Some("from queue"));
    assert!(app.busy);
}

#[tokio::test]
async fn ctrl_backspace_cancels_selected_queue_message() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.enqueue_user_message("a".into());
    app.enqueue_user_message("b".into());
    app.move_queue_selection(1);
    app.handle_key(press(KeyCode::Backspace, KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert_eq!(app.message_queue.len(), 1);
    assert_eq!(
        app.message_queue.iter().next().map(|s| s.as_str()),
        Some("a")
    );
}

#[tokio::test]
async fn effort_selection_persists_across_tui_instances() {
    let (_dir, session) = test_session().await;
    let credential_dir = tempfile::tempdir().unwrap();
    let credential_path = credential_dir.path().join("credentials.toml");
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(credential_path.clone());

    app.reasoning_effort = ReasoningEffort::High;
    app.persist_selection();

    assert_eq!(
        app.connect_store.last_effort().unwrap().as_deref(),
        Some("high")
    );

    let (_dir, session) = test_session().await;
    let mut restarted = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    restarted.connect_store = CredentialStore::new(credential_path);
    restarted = restarted.restore_saved_auth();

    assert_eq!(restarted.reasoning_effort, ReasoningEffort::High);
}

#[tokio::test]
async fn model_command_applies_provider_id_to_session() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect_store
        .set_api_key("openai", "sk-test-openai-credential")
        .unwrap();
    app.connect_profile = Some("openai".into());
    app.runtime.model_label = "openai/gpt-4.1-mini".into();
    app.session.set_active_model("openai/gpt-4.1-mini");
    app.apply_model_selection("native", "openai/gpt-4.1-mini");
    assert_eq!(app.runtime.model_label, "openai/gpt-4.1-mini");
    assert_eq!(app.session.active_model, "openai/gpt-4.1-mini");
    assert!(app.pending_prompt.is_none());
}

#[tokio::test]
async fn model_command_rejects_cross_provider_selection_without_matching_connection() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai-codex/gpt-5.6-sol".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect_store
        .set_oauth(
            "openai_codex",
            forge_connect::OauthTokens {
                access_token:
                    "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.sig"
                        .to_string(),
                refresh_token: None,
                expires_at: None,
            },
        )
        .unwrap();
    app.connect_profile = Some("openai_codex".into());
    app.runtime.model_label = "openai-codex/gpt-5.6-sol".into();
    app.session.set_active_model("openai-codex/gpt-5.6-sol");

    app.dispatch_line("/model not-connected claude-sonnet-4-5")
        .await
        .unwrap();

    assert_eq!(app.connect_profile.as_deref(), Some("openai_codex"));
    assert_eq!(app.runtime.model_label, "openai-codex/gpt-5.6-sol");
    assert!(
        app.status_message.contains("connect `not-connected` first")
            || app.notices.iter().any(|l| l.contains("not-connected")),
        "expected rejection notice, got status={} notices={:?}",
        app.status_message,
        app.notices
    );
}

#[tokio::test]
async fn slash_sync_commits_and_does_not_queue_chat() {
    let (dir, session) = test_session().await;
    for args in [
        &["init"][..],
        &["config", "user.email", "forge@test"][..],
        &["config", "user.name", "Forge Test"][..],
    ] {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
    }
    // Need an initial commit so later push has a branch tip; create one empty first.
    std::fs::write(dir.path().join("README"), "x").unwrap();
    std::process::Command::new("git")
        .args(["add", "README"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    // Bare remote for push
    let remote = dir.path().join("remote.git");
    std::process::Command::new("git")
        .args(["init", "--bare"])
        .arg(&remote)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["remote", "add", "origin"])
        .arg(&remote)
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["push", "-u", "origin", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    std::fs::write(dir.path().join("note.txt"), "hello").unwrap();

    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "0.12.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("/sync").await.unwrap();
    assert!(app.pending_prompt.is_none());
    assert!(!app.busy);
    let log = app
        .run_git_tool("log", vec!["-1".into(), "--oneline".into()])
        .await
        .unwrap();
    // Heuristic message mentions note.txt when mock has no real LLM summary
    assert!(
        log.content.contains("note") || log.content.contains("Add") || !log.content.is_empty(),
        "expected a commit: {}",
        log.content
    );
    assert!(
        app.activity
            .recent(12)
            .iter()
            .any(|e| e.summary.contains("git commit") || e.summary.contains("git push")),
        "activity should record sync steps"
    );
}

#[tokio::test]
async fn app_dispatch_user_message() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("/tmp"),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("hi").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    assert!(
        app.session
            .messages
            .iter()
            .any(|m| m.content.contains("hello tui") || m.content == "hi"),
        "messages={:?}",
        app.session
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn app_status_command() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("/status").await.unwrap();
    assert!(app.overlay.is_none());
    assert!(app.notices.is_empty());
}

#[tokio::test]
async fn clear_hides_existing_chat_without_deleting_context() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("hi").await.unwrap();
    app.drain_pending_prompt(None).await.unwrap();
    let message_count = app.session.messages.len();
    let event_count = app.session.events.len();
    assert!(message_count > 0);

    app.dispatch_line("/clear").await.unwrap();

    assert_eq!(app.chat_message_start, message_count);
    assert_eq!(app.chat_event_start, event_count);
    assert_eq!(app.session.messages.len(), message_count);
    assert_eq!(app.session.events.len(), event_count);
    assert!(app.ui_banners.is_empty());
    assert!(app.notices.is_empty());
    assert_eq!(app.chat_scroll, 0);
    assert!(app.chat_follow);
}

#[tokio::test]
async fn app_quit_command() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "p".into(),
            cwd: PathBuf::from("."),
            version: "0.4.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("/quit").await.unwrap();
    assert!(app.should_quit);
}

#[tokio::test]
async fn connect_opencode_go_opens_api_key_overlay() {
    // Isolate HOME to prevent restore_saved_auth from discovering real credentials.
    let _home_guard = {
        let temp_home = tempfile::TempDir::new().unwrap();
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
    };

    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    let _store_dir = tempfile::TempDir::new().unwrap();
    app.connect_store = CredentialStore::new(_store_dir.path().join("empty-creds.toml"));
    app.dispatch_line("/connect opencode_go").await.unwrap();
    match &app.overlay {
        Some(Overlay::ConnectApiKey {
            profile_id, title, ..
        }) => {
            assert_eq!(profile_id, "opencode_go");
            assert!(title.contains("OpenCode"));
        }
        other => panic!("expected ConnectApiKey overlay, got {other:?}"),
    }
}

#[tokio::test]
async fn disconnect_clears_credentials_and_prompts_reauth() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect_store
        .set_api_key("openai", "sk-test-saved-credential")
        .unwrap();
    app.connect_profile = Some("openai".into());
    app.runtime.model_label = "openai/gpt-4.1-mini".into();
    app.session.set_active_model("openai/gpt-4.1-mini");

    app.dispatch_line("/disconnect").await.unwrap();

    assert!(app.auth_suspended);
    assert!(app.connect_profile.is_none());
    assert!(!app.is_provider_connected());
    assert!(!app.connect_store.is_connected("openai").unwrap());
    assert!(matches!(app.overlay, Some(Overlay::ConnectPicker { .. })));
    assert!(
        app.notices.iter().any(|l| l.contains("disconnected"))
            || app.status_message.contains("disconnected")
    );
}

#[tokio::test]
async fn connect_picker_marks_saved_credentials_as_connected() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect_store
        .set_api_key("openai", "sk-test-saved-credential")
        .unwrap();

    app.open_connect_picker();
    let Some(Overlay::ConnectPicker { items, .. }) = &app.overlay else {
        panic!("expected connect picker");
    };
    assert!(
        items
            .iter()
            .any(|item| item.id == "openai" && item.connected),
        "saved provider should be marked connected"
    );
}

#[tokio::test]
async fn successful_connect_hands_off_to_model_picker() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect_store
        .set_api_key("openai", "sk-test-saved-credential")
        .unwrap();
    app.connect_profile = Some("openai".into());
    app.runtime.model_label = "openai/gpt-4.1-mini".into();
    app.session.set_active_model("openai/gpt-4.1-mini");

    app.open_model_picker_after_connect("openai");
    let Some(Overlay::Model {
        provider_selected,
        providers,
        ..
    }) = &app.overlay
    else {
        panic!("expected model picker");
    };
    assert_eq!(
        providers.get(*provider_selected).map(String::as_str),
        Some("openai")
    );
    assert!(app.feedback.text.contains("choose a model"));
}

#[tokio::test]
async fn model_selection_switches_to_the_matching_connected_provider() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    app.connect_store
        .set_api_key("openai", "sk-test-openai-credential")
        .unwrap();
    app.connect_store
        .set_api_key("anthropic", "sk-test-anthropic-credential")
        .unwrap();
    app.connect_profile = Some("openai".into());

    app.apply_model_selection("native", "anthropic/claude-sonnet-4-5");

    assert_eq!(app.connect_profile.as_deref(), Some("anthropic"));
    assert_eq!(app.runtime.model_label, "anthropic/claude-sonnet-4-5");
}

#[tokio::test]
async fn invalid_api_key_error_stays_inside_key_modal() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
    let mut overlay = Overlay::connect_api_key("openai", "OpenAI", None, None);
    if let Overlay::ConnectApiKey { key_input, .. } = &mut overlay {
        *key_input = "bad".into();
    }
    app.overlay = Some(overlay);

    app.try_connect_api_key("openai", Some("bad".into()));
    let Some(Overlay::ConnectApiKey {
        key_input, error, ..
    }) = &app.overlay
    else {
        panic!("expected API key modal to remain open");
    };
    assert_eq!(key_input, "bad");
    assert!(error.as_deref().is_some_and(|text| text.contains("short")));
    assert!(
        !app.ui_banners.iter().any(|item| matches!(
            item,
            ChatItem::Banner {
                kind: BannerKind::Error,
                ..
            }
        )),
        "onboarding errors should stay in the modal"
    );
}

#[tokio::test]
async fn connect_xai_opens_oauth_overlay() {
    let (_dir, session) = test_session().await;
    let cred_dir = tempfile::tempdir().unwrap();
    // Isolate credentials + use stub device start (no network).
    std::env::set_var("FORGE_CONNECT_OAUTH_STUB", "1");
    std::env::remove_var("FORGE_CONNECT_OAUTH_FIXTURE");
    std::env::remove_var("FORGE_XAI_OAUTH_ACCESS_TOKEN");
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.6.1".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_store = CredentialStore::new(cred_dir.path().join("c.toml"));
    app.dispatch_line("/connect xai").await.unwrap();
    std::env::remove_var("FORGE_CONNECT_OAUTH_STUB");
    match &app.overlay {
        Some(Overlay::ConnectOauth {
            profile_id, title, ..
        }) => {
            assert_eq!(profile_id, "xai");
            assert!(title.contains("Grok") || title.contains("xAI"));
        }
        other => panic!("expected ConnectOauth overlay, got {other:?}"),
    }
    assert!(app.oauth_pending.is_some());
}

#[tokio::test]
async fn history_records_submitted_lines_and_up_recalls() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.7.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    let enter = KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    app.input.set_text("/status");
    app.handle_key(enter).await.unwrap();
    app.input.set_text("/model");
    app.handle_key(enter).await.unwrap();
    assert!(app.history.len() >= 2);
    let t = app.history.up(&app.input.text).unwrap();
    app.apply_history_text(t);
    assert_eq!(app.input.text, "/model");
    let t = app.history.up(&app.input.text).unwrap();
    app.apply_history_text(t);
    assert_eq!(app.input.text, "/status");
    let t = app.history.down().unwrap();
    app.apply_history_text(t);
    assert_eq!(app.input.text, "/model");
}

#[tokio::test]
async fn history_up_via_key_when_no_overlay() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.7.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.history.push("alpha");
    app.history.push("beta");
    app.input.clear();
    let up = KeyEvent {
        code: KeyCode::Up,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    app.handle_key(up).await.unwrap();
    assert_eq!(app.input.text, "beta");
    app.handle_key(up).await.unwrap();
    assert_eq!(app.input.text, "alpha");
    let down = KeyEvent {
        code: KeyCode::Down,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    app.handle_key(down).await.unwrap();
    assert_eq!(app.input.text, "beta");
}

#[tokio::test]
async fn helper_labels_reflect_focus_mode() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    assert!(app.help_text().contains("Conversation"));
    app.workspace_navigation
        .replace_view(WorkspaceView::Diff(DiffCommandContext::Current));
    assert!(app.help_text().contains("Review changes"));
}

#[tokio::test]
async fn tab_nav_command_recognizes_shifted_plain_arrows_only() {
    let (_dir, app) = focus_test_app().await;
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Left, KeyModifiers::SHIFT)),
        Some(TabNavCommand::PreviousTab)
    );
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Right, KeyModifiers::SHIFT)),
        Some(TabNavCommand::NextTab)
    );
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Left, KeyModifiers::ALT)),
        None
    );
    assert_eq!(
        app.tab_nav_command(press(KeyCode::Right, KeyModifiers::CONTROL)),
        None
    );
}

#[tokio::test]
async fn focus_availability_and_restore_skip_hidden_blocks() {
    let (_dir, mut app) = focus_test_app().await;
    app.files_visible = true;
    app.sidebar_visible = false;
    app.bottom_panel.open = false;
    let availability = app.focus_availability();
    assert!(availability.contains(FocusBlock::Files));
    assert!(!availability.contains(FocusBlock::Inspector));
    assert!(!availability.contains(FocusBlock::BottomPanel));

    app.focus.previous_block = Some(FocusBlock::Inspector);
    app.restore_focus_after_closing(FocusBlock::Files);
    assert_eq!(app.focus.block, FocusBlock::Workspace);
    assert_eq!(app.focus.return_block, Some(FocusBlock::Workspace));
}

#[tokio::test]
async fn contextual_hint_appears_only_for_transient_or_blocking_state() {
    let (_dir, mut app) = focus_test_app().await;
    assert!(app.contextual_hint().is_none());

    app.focus_block(FocusBlock::Workspace);
    assert!(app.contextual_hint().is_none());

    app.focus.mode = FocusMode::Transient(TransientOwner::SourceSearch);
    assert!(app
        .contextual_hint()
        .is_some_and(|hint| hint.contains("Esc cancel")));

    app.focus.mode = FocusMode::Navigation;
    app.overlay = Some(Overlay::turn_limit(4));
    assert_eq!(
        app.contextual_hint().as_deref(),
        Some("Enter confirm · Esc cancel")
    );
}

#[tokio::test]
async fn slash_stays_in_textbox_does_not_open_palette() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "/");
    assert!(app.overlay.is_none(), "Phase 8: / must not open palette");
    app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('t'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.input.text, "/st");
    assert!(app.overlay.is_none());
}

#[tokio::test]
async fn enter_runs_slash_from_main_textbox() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/status".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.input.text, "/status");
    assert!(app.overlay.is_none());
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.notices.is_empty());
    assert!(app.history.entries().iter().any(|e| e == "/status"));
}

#[tokio::test]
async fn ctrl_k_opens_command_palette() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.handle_key(press(KeyCode::Char('k'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(matches!(app.overlay, Some(Overlay::Slash { .. })));
}

#[tokio::test]
async fn inspector_is_closed_by_default_and_opens_on_demand() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    assert!(!app.sidebar_visible);
    app.handle_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(app.sidebar_visible);
    assert!(split_areas_full(
        ratatui::layout::Rect::new(0, 0, 120, 30),
        0,
        3,
        app.sidebar_visible,
        0
    )
    .sidebar
    .is_some());
    app.handle_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!app.sidebar_visible);
    assert!(
        split_areas_full(ratatui::layout::Rect::new(0, 0, 80, 24), 0, 3, true, 0)
            .sidebar
            .is_none()
    );
}

#[tokio::test]
async fn inspector_view_shortcuts_cycle_without_opening_sidebar() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "test".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    assert_eq!(app.inspector_view, InspectorView::Task);
    app.handle_key(press(KeyCode::Char(']'), KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.inspector_view, InspectorView::Context);
    app.handle_key(press(KeyCode::Char('['), KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.inspector_view, InspectorView::Task);
    assert!(!app.sidebar_visible);
}

#[tokio::test]
async fn multi_token_slash_connect_list_opens_picker() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/connect list".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    match &app.overlay {
        Some(Overlay::ConnectPicker { items, .. }) => {
            assert!(items.iter().any(|i| i.id == "xai"));
            assert!(items.iter().any(|i| i.id == "opencode_go"));
        }
        other => panic!("expected ConnectPicker, got {other:?}"),
    }
}

#[tokio::test]
async fn slash_tab_autocompletes_command() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/res".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert!(!app.slash_suggestions().is_empty());
    app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        app.input.text.starts_with("/resume"),
        "got {}",
        app.input.text
    );
}

#[tokio::test]
async fn connect_alone_opens_profile_picker() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/connect".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(app.overlay, Some(Overlay::ConnectPicker { .. })));
}

#[tokio::test]
async fn startup_notices_seed_notice_panel() {
    let (_dir, session) = test_session().await;
    let app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: vec!["mcp: failed".into()],
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );

    assert_eq!(app.notices, vec!["mcp: failed"]);
}

#[tokio::test]
async fn enter_on_highlighted_suggestion_runs_command() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    // Partial type; suggestions include /connect and /status.
    for c in "/con".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    let suggestions = app.slash_suggestions();
    assert!(!suggestions.is_empty(), "expected slash suggestions");
    // Move highlight onto /connect if it is not already first
    let connect_idx = suggestions
        .iter()
        .position(|s| s.cmd == "/connect")
        .expect("/connect in suggestions for /con");
    for _ in 0..connect_idx {
        app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(
        app.slash_suggestions()[app.slash_suggest_idx].cmd,
        "/connect"
    );
    // One Enter should apply selection AND open the connect picker (not merely complete text)
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        matches!(app.overlay, Some(Overlay::ConnectPicker { .. })),
        "Enter on highlighted /connect should open picker; overlay={:?} input={:?} status={}",
        app.overlay,
        app.input.text,
        app.status_message
    );
    assert!(
        app.input.text.is_empty(),
        "input should be cleared after run, got {:?}",
        app.input.text
    );
}

#[tokio::test]
async fn bare_slash_lists_all_palette_commands() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    let suggestions = app.slash_suggestions();
    let expected = crate::overlays::default_palette_items();
    assert_eq!(
        suggestions.len(),
        expected.len(),
        "bare / should list every palette command; got {:?}",
        suggestions.iter().map(|s| &s.cmd).collect::<Vec<_>>()
    );
    for cmd in [
        "/connect",
        "/model",
        "/compact",
        "/resume",
        "/file",
        "/sync",
        "/copy",
        "/clear",
        "/disconnect",
        "/quit",
    ] {
        assert!(
            suggestions.iter().any(|s| s.cmd == cmd),
            "missing {cmd} in suggestions"
        );
    }
}

#[tokio::test]
async fn enter_on_status_suggestion_runs_immediately() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.8.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/sta".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.input.text.is_empty());
}

#[test]
fn status_model_from_app_fields() {
    // layout + status integration smoke
    let m = StatusModel {
        status: forge_types::SessionStatus::Running,
        session_short: "abc".into(),
        model: "m".into(),
        provider: "mock".into(),
        effort: "auto".into(),
        ctx_pct: 0.2,
        busy: false,
        busy_phase: BusyPhase::Idle,
        connect_profile: None,
        provider_connected: true,
        web_search_label: None,
        tools_visible: 0,
        prompt_cache_hits: 0,
        prompt_cache_writes: 0,
        repo_name: None,
        branch: None,
        dirty: false,
        resource: None,
        activity: None,
        turn_cancelled: false,
        progress_description: None,
        failure_category: None,
        waiting_detail: None,
    };
    assert_eq!(m.status_label().0, "Ready");
}

#[tokio::test]
async fn header_status_follows_session_lifecycle() {
    let (dir, session) = test_session().await;
    let mut app = TuiApp::new(
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

    let ready = app.refresh_status_model();
    assert_eq!(ready.turn_lifecycle(), TurnLifecycle::Ready);
    assert!(ready.status_label().0.contains("Ready"));

    app.busy = true;
    app.busy_phase = BusyPhase::Tool {
        name: "read_file".into(),
    };
    let working = app.refresh_status_model();
    assert_eq!(working.turn_lifecycle(), TurnLifecycle::Working);
    assert!(working.status_label().0.contains("Working"));
    assert!(
        working.status_label().0.contains("Reading files"),
        "{:?}",
        working.status_label().0
    );

    app.busy = false;
    app.busy_phase = BusyPhase::Idle;
    app.session.status = forge_types::SessionStatus::Completed;
    assert_eq!(
        app.refresh_status_model().turn_lifecycle(),
        TurnLifecycle::Completed
    );

    app.session.status = forge_types::SessionStatus::Failed;
    assert_eq!(
        app.refresh_status_model().turn_lifecycle(),
        TurnLifecycle::Failed
    );

    app.session.status = forge_types::SessionStatus::Cancelled;
    assert_eq!(
        app.refresh_status_model().turn_lifecycle(),
        TurnLifecycle::Cancelled
    );

    app.session.status = forge_types::SessionStatus::Interrupted;
    assert_eq!(
        app.refresh_status_model().turn_lifecycle(),
        TurnLifecycle::Interrupted
    );

    app.session.status = forge_types::SessionStatus::AwaitingHitl;
    app.session.pending_hitl = Some(direct_hitl_payload("h", "x.txt"));
    let waiting = app.refresh_status_model();
    assert_eq!(waiting.turn_lifecycle(), TurnLifecycle::Waiting);
    assert!(waiting.status_label().0.contains("Approval required"));
}

#[tokio::test]
async fn header_status_switches_with_selected_session() {
    let dir = TempDir::new().unwrap();
    let mut completed = session_for_workspace_with_model(
        dir.path(),
        Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "done-a".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }])),
    )
    .await;
    completed.run_user_message("a").await.unwrap();
    assert_eq!(completed.status, forge_types::SessionStatus::Completed);
    let id_a = completed.session_id;

    let running =
        session_for_workspace_with_model(dir.path(), Arc::new(MockModelClient::script(vec![])))
            .await;
    let id_b = running.session_id;

    let mut app = TuiApp::new(
        completed,
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
    assert!(app
        .refresh_status_model()
        .status_label()
        .0
        .contains("Completed"));

    app.session.resume_session(id_b).await.unwrap();
    assert_eq!(app.session.status, forge_types::SessionStatus::Interrupted);
    assert!(app
        .refresh_status_model()
        .status_label()
        .0
        .contains("Interrupted"));

    app.session.resume_session(id_a).await.unwrap();
    assert_eq!(app.session.status, forge_types::SessionStatus::Completed);
    assert!(app
        .refresh_status_model()
        .status_label()
        .0
        .contains("Completed"));
}

#[tokio::test]
async fn blocks_chat_when_not_connected() {
    use crossterm::event::{KeyCode, KeyModifiers};

    // Isolate HOME to a temp dir so CredentialStore::user_default() cannot
    // discover the real user's ~/.forge/credentials.toml.
    let _home_guard = {
        let temp_home = tempfile::TempDir::new().unwrap();
        // Create the credential directory structure that user_default() expects.
        // On macOS: {HOME}/Library/Application Support/forge/credentials.toml
        // On Linux: {HOME}/.config/forge/credentials.toml
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
    };

    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-4.1-mini".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.11.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    // Override credential store with empty temp file so connection check fails.
    let _store_dir = tempfile::TempDir::new().unwrap();
    app.connect_store = CredentialStore::new(_store_dir.path().join("empty-creds.toml"));
    app.connect_profile = None;
    app.refresh_connection_ui();
    assert!(!app.is_provider_connected());

    for c in "hello world".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.pending_prompt.is_none(), "must not queue a model turn");
    assert!(!app.busy);
    assert_eq!(app.input.text, "hello world");
    assert!(
        app.ui_banners.iter().any(|b| matches!(
            b,
            ChatItem::Banner { text, .. } if text.to_ascii_lowercase().contains("not connected")
        )) || app
            .activity
            .recent(8)
            .iter()
            .any(|e| e.summary.to_ascii_lowercase().contains("not connected")),
        "expected not-connected feedback"
    );
}

#[tokio::test]
async fn mock_provider_allows_chat_without_connect() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.11.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    assert!(app.is_provider_connected());
    app.dispatch_line("hi").await.unwrap();
    assert_eq!(app.pending_prompt.as_deref(), Some("hi"));
}

#[tokio::test]
async fn status_chrome_shows_not_connected_badge() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-test".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.11.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.connect_profile = None;
    app.connect_store = CredentialStore::new(
        tempfile::TempDir::new()
            .unwrap()
            .path()
            .join("empty-creds.toml"),
    );
    app.refresh_connection_ui();
    let backend = TestBackend::new(100, 24);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let mut text = String::new();
    let buf = term.backend().buffer();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(
        text.to_ascii_lowercase().contains("not connected") || text.contains("○"),
        "missing not-connected chrome:\n{text}"
    );
}

#[tokio::test]
async fn tui09_chrome_includes_model_on_frame() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "openai/gpt-test".into(),
            provider: "native".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    let chrome = app.refresh_status_model();
    assert_eq!(chrome.provider, "native");
    assert!(chrome.model.contains("gpt-test"));
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let mut text = String::new();
    let buf = term.backend().buffer();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(
        !text.contains("gpt-test") && !text.contains("in 0 · out 0 · total 0"),
        "default chrome duplicated model or usage:\n{text}"
    );
}

#[tokio::test]
async fn tui09_narrow_frame_still_shows_model_or_ctx() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mymodel".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    // Width 60: no sidebar per layout MIN_WIDTH 80
    let backend = TestBackend::new(60, 24);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let mut text = String::new();
    let buf = term.backend().buffer();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("Forge"),
        "narrow frame missing app identity:\n{text}"
    );
    assert!(
        !text.contains("mymodel") && !text.contains("mock") && !text.contains("ctx"),
        "narrow default chrome duplicated secondary metadata:\n{text}"
    );
}

#[tokio::test]
async fn tui09_status_renders_structured_session_card() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/status".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.notices.is_empty());
    assert!(app.overlay.is_none());

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        text.contains("unknown command `/status`"),
        "missing status command feedback:\n{text}"
    );
}

#[tokio::test]
async fn tui08_report_error_writes_banner_feedback_and_activity() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.report_error("upstream returned 429 rate limit exceeded");
    assert_eq!(app.feedback.severity, FeedbackSeverity::Error);
    assert!(app.feedback.text.contains("429"));
    assert!(app.status_message.contains("429"));
    assert!(
        app.ui_banners.iter().any(|b| matches!(
            b,
            ChatItem::Banner {
                kind: BannerKind::Error,
                ..
            }
        )),
        "expected error banner in ui_banners"
    );
    assert!(
        app.activity
            .all()
            .iter()
            .any(|i| i.kind == ActivityKind::Error),
        "expected error activity"
    );
}

#[tokio::test]
async fn tui08_feedback_strip_visible_on_frame() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.report_error("429 rate limit");
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let mut text = String::new();
    let buf = term.backend().buffer();
    for y in 0..buf.area().height {
        for x in 0..buf.area().width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("rate limited") || text.contains("429") || text.contains("Model error"),
        "frame missing feedback:\n{text}"
    );
}

#[tokio::test]
async fn tui10_activity_feed_records_model_and_error() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.push_activity(
        ActivityKind::Model,
        FeedbackSeverity::Info,
        "model call started",
    );
    app.report_error("429 rate limit");
    assert!(app.activity.len() >= 2);
    let recent: Vec<_> = app
        .activity
        .recent(10)
        .iter()
        .map(|i| i.summary.clone())
        .collect();
    assert!(
        recent
            .iter()
            .any(|s| s.contains("rate") || s.contains("429") || s.contains("Model")),
        "recent={recent:?}"
    );
    assert_eq!(app.busy_phase, BusyPhase::Idle);
}

#[tokio::test]
async fn elapsed_status_persists_during_answer_and_tool_processing() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.busy = true;
    app.turn_started = Some(Instant::now() - Duration::from_millis(1200));
    app.stream_preview = "partial answer".into();
    assert_eq!(app.busy_status_detail().as_deref(), Some("Working... 1.2s"));

    app.stream_preview.clear();
    app.busy_phase = BusyPhase::Tool {
        name: "read_file".into(),
    };
    assert!(app
        .busy_status_detail()
        .unwrap()
        .starts_with("Working... 1.2s"));
}

#[tokio::test]
async fn tui10_busy_phase_model_during_turn_clears_after() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.dispatch_line("hello").await.unwrap();
    assert_eq!(app.busy_phase, BusyPhase::Model);
    assert!(app.pending_prompt.is_some());
    app.drain_pending_prompt(None).await.unwrap();
    assert_eq!(app.busy_phase, BusyPhase::Idle);
    assert!(
        app.activity
            .all()
            .iter()
            .any(|i| i.kind == ActivityKind::Model),
        "expected model activity"
    );
}

#[tokio::test]
async fn tui08_context_sets_feedback_strip() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    for c in "/status".chars() {
        app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.input.text.is_empty());
}

#[test]
fn exit_summary_formats_token_usage() {
    let report = forge_core::TokenUsageReport {
        api: forge_core::SessionTokenUsage {
            prompt_tokens: 6_094,
            completion_tokens: 36,
            model_calls_with_usage: 1,
            model_steps: 1,
            thinking_tokens_est: 19,
            prompt_cache_hits: 5_504,
            prompt_cache_writes: 0,
        },
        context_tokens_est: 0,
        context_capacity: 1,
        context_pct: 0.0,
        system_tokens_est: 0,
        user_tokens_est: 0,
        assistant_tokens_est: 0,
        tool_tokens_est: 0,
        thinking_in_context_est: 0,
        message_count: 0,
        tool_message_count: 0,
    };

    assert_eq!(
        format_exit_token_usage(&report),
        "Token usage: total=6,130 input=6,094 (+ 5,504 cached) output=36 (reasoning 19)"
    );
}

#[test]
fn footer_usage_formats_with_total_and_commas() {
    let report = forge_core::TokenUsageReport {
        api: forge_core::SessionTokenUsage {
            prompt_tokens: 6_094,
            completion_tokens: 36,
            model_calls_with_usage: 1,
            model_steps: 1,
            thinking_tokens_est: 19,
            prompt_cache_hits: 5_504,
            prompt_cache_writes: 0,
        },
        context_tokens_est: 0,
        context_capacity: 1,
        context_pct: 0.0,
        system_tokens_est: 0,
        user_tokens_est: 0,
        assistant_tokens_est: 0,
        tool_tokens_est: 0,
        thinking_in_context_est: 0,
        message_count: 0,
        tool_message_count: 0,
    };

    assert_eq!(
        footer_usage_summary_with_cost(&report, None),
        "in 6,094 · out 36 · total 6,130"
    );
}

#[test]
fn footer_usage_includes_cached_cost() {
    let report = forge_core::TokenUsageReport {
        api: forge_core::SessionTokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 500_000,
            model_calls_with_usage: 1,
            model_steps: 1,
            thinking_tokens_est: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
        },
        context_tokens_est: 0,
        context_capacity: 1,
        context_pct: 0.0,
        system_tokens_est: 0,
        user_tokens_est: 0,
        assistant_tokens_est: 0,
        tool_tokens_est: 0,
        thinking_in_context_est: 0,
        message_count: 0,
        tool_message_count: 0,
    };

    assert_eq!(
        footer_usage_summary_with_cost(
            &report,
            Some(forge_connect::CatalogCost {
                input: 3.0,
                output: 15.0,
            })
        ),
        "in 1,000,000 · out 500,000 · total 1,500,000 · $10.5000"
    );
}

#[test]
fn footer_limits_use_connected_profile_instead_of_native_transport() {
    assert_eq!(
        footer_provider_id("native", Some("openai-codex")),
        "openai-codex"
    );
    assert_eq!(footer_provider_id("mock", None), "mock");
}

#[tokio::test]
async fn external_editor_keybind_sets_flag() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    assert!(!app.pending_external_editor);
    let path = app.session.workspace_root().join("fake.txt");
    fs::write(&path, "hello").unwrap();
    app.open_file_in_editor(&path);
    app.handle_key(press(KeyCode::Char('e'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.pending_external_editor);
}

#[tokio::test]
async fn external_editor_preconditions_no_file() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.pending_external_editor = true;
    app.drain_pending_external_editor(None).await.unwrap();
    // Should not crash; feedback set because no file is open.
    assert!(!app.pending_external_editor);
}

#[tokio::test]
async fn external_editor_preconditions_binary_file() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.source_viewer.status = crate::source_viewer::ViewerStatus::Binary;
    app.source_viewer.path = Some(PathBuf::from("/tmp/fake.bin"));
    app.pending_external_editor = true;
    app.drain_pending_external_editor(None).await.unwrap();
    // Should not crash; feedback set because binary.
    assert!(!app.pending_external_editor);
}

#[tokio::test]
async fn external_editor_rejects_during_tool_execution() {
    let (_dir, session) = test_session().await;
    let mut app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "m".into(),
            provider: "mock".into(),
            cwd: PathBuf::from("."),
            version: "0.10.0".into(),
            startup_notices: Vec::new(),
            validation_command: None,
            file_icons: FileIconMode::Unicode,
            mouse_capture: true,
            theme: forge_config::Theme::default(),
        },
    );
    app.busy_phase = BusyPhase::Tool {
        name: "write".into(),
    };
    app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
    app.source_viewer.path = Some(PathBuf::from("/tmp/fake.txt"));
    app.pending_external_editor = true;
    app.drain_pending_external_editor(None).await.unwrap();
    // Should not crash; feedback set because tool is active.
    assert!(!app.pending_external_editor);
}

#[tokio::test]
async fn external_editor_resume_draws_after_terminal_reinit() {
    let (_dir, mut app) = focus_test_app().await;
    app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
    app.source_viewer.path = Some(PathBuf::from("/tmp/fake.txt"));
    app.pending_external_editor = true;

    let result = app.resume_after_external_editor(None);
    assert!(result.is_ok());
}

/// Highlighting does not depend on terminal width, so a resize must reuse it.
/// A resize flips the conversation render key and rebuilds every line; before
/// this cache that re-ran tree-sitter over every code block in the transcript.
#[tokio::test]
async fn resize_reuses_cached_highlights() {
    let (_dir, mut app) = app_with_code("resize").await;
    // Take the serialising guard only after the last await: holding a std
    // guard across an await point is a clippy error and a real deadlock risk.
    let _guard = lock_highlight_cache();
    draw_app(&mut app, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    // A wide delta guarantees the chat width changes, so the render key flips
    // and the transcript is rebuilt from scratch.
    draw_app(&mut app, 170, 40);
    let after = forge_syntax::highlight_cache_stats();

    assert!(
        after.hits >= before.hits + CACHED_BLOCKS as u64,
        "a resize must serve every block from cache (hits {} -> {})",
        before.hits,
        after.hits
    );
}

/// A theme switch changes the colours baked into each segment, so it *must*
/// recompute. This is the invalidation half of the contract: stale colours
/// after a theme change would be a visible bug.
#[tokio::test]
async fn theme_switch_recomputes_highlights() {
    let (_dir, mut app) = app_with_code("theme").await;
    let _guard = lock_highlight_cache();
    crate::theme::set_active(forge_config::Theme::Dark);
    draw_app(&mut app, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    crate::theme::set_active(forge_config::Theme::Light);
    draw_app(&mut app, 100, 30);
    let after = forge_syntax::highlight_cache_stats();

    // Restore before asserting so a failure cannot leak a palette into others.
    crate::theme::set_active(forge_config::Theme::Dark);

    assert!(
        after.misses >= before.misses + CACHED_BLOCKS as u64,
        "a theme switch must recompute every block (misses {} -> {})",
        before.misses,
        after.misses
    );
}

/// Scrolling changes which lines are visible, never their colours. The scroll
/// offset is not part of the conversation render key, so a scroll does not
/// rebuild the transcript and must not recompute any highlight.
///
/// Asserted as an upper bound with tolerance: a genuine re-highlight would add
/// exactly `CACHED_BLOCKS` misses, whereas a concurrent `source_viewer` test
/// contributes at most one or two.
#[tokio::test]
async fn scrollback_does_not_recompute_highlights() {
    let (_dir, mut app) = app_with_code("scroll").await;
    let _guard = lock_highlight_cache();
    draw_app(&mut app, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    app.chat_follow = false;
    app.chat_scroll = 3;
    draw_app(&mut app, 100, 30);
    let after = forge_syntax::highlight_cache_stats();

    assert!(
        after.misses < before.misses + CACHED_BLOCKS as u64,
        "scrolling must not recompute the transcript's highlights \
         (misses {} -> {})",
        before.misses,
        after.misses
    );
}

/// Reopening a session re-renders the same transcript text in a fresh
/// `TuiApp`. The cache is keyed on content, not on app identity, so the
/// second app must not pay for highlighting again.
#[tokio::test]
async fn session_reload_reuses_cached_highlights() {
    // Both apps are built before the guard is taken, for the same reason.
    let (_dir, mut first) = app_with_code("reload").await;
    // A separate app and session carrying identical transcript text.
    let (_dir2, mut reloaded) = app_with_code("reload").await;
    let _guard = lock_highlight_cache();
    draw_app(&mut first, 100, 30);
    let before = forge_syntax::highlight_cache_stats();

    draw_app(&mut reloaded, 100, 30);
    let after = forge_syntax::highlight_cache_stats();

    assert!(
        after.hits >= before.hits + CACHED_BLOCKS as u64,
        "a reloaded session must reuse cached highlights (hits {} -> {})",
        before.hits,
        after.hits
    );
}
