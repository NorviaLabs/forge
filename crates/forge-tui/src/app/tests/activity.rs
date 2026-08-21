//! Activity feed and agent-turn presentation tests.
//!
//! Split out of `app/tests/mod.rs` per #19. Moved verbatim.

use super::prelude::*;

#[tokio::test]
async fn agent_streaming_while_viewing_file_does_not_navigate() {
    let (dir, mut app) = focus_test_app().await;
    let path = dir.path().join("main.rs");
    fs::write(&path, "fn main() {}\n").unwrap();
    app.execute_semantic_command(SemanticCommand::OpenFile(path.clone()))
        .await
        .unwrap();
    let before = app.workspace_navigation.clone();

    app.busy_state.activate();
    app.busy_state.set_phase(BusyPhase::Model);
    app.pending_turn.clear();
    app.stream.preview = "partial answer".into();
    let rendered = render_app_text(&mut app, 100, 30);

    assert_eq!(app.workspace_navigation, before);
    assert_eq!(
        app.workspace_navigation.current(),
        Some(WorkspaceView::File(path))
    );
    assert!(rendered.contains("fn main()"), "{rendered}");
    // The sidebar always shows the conversation now, regardless of what the
    // center pane displays — streaming preview text is expected here.
    assert!(
        rendered.contains("partial answer"),
        "Sidebar should keep streaming visible while File view is primary:\n{rendered}"
    );
}

#[tokio::test]
async fn agent_thinking_keeps_composer_usable() {
    let (_dir, mut app) = focus_test_app().await;
    app.busy_state.activate();
    app.busy_state.set_phase(BusyPhase::Model);
    app.stream.thinking = "planning".into();
    app.focus_block(FocusBlock::Composer);

    app.handle_key(press(KeyCode::Char('x'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.input.text, "x");
    assert_eq!(app.workspace_navigation.current(), None);
    assert!(app.activity_summary().is_none());
}

#[tokio::test]
async fn changed_files_do_not_appear_in_footer_or_as_review_cta() {
    let (dir, mut app) = focus_test_app().await;
    init_repo(dir.path());
    let status = std::process::Command::new("git")
        .args(["-C", dir.path().to_str().unwrap(), "add", "-A"])
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new("git")
        .args([
            "-C",
            dir.path().to_str().unwrap(),
            "commit",
            "-qm",
            "initial",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(dir.path().join("changed.rs"), "changed\n").unwrap();
    app.workspace_files.explorer.refresh_git_status();
    for _ in 0..20 {
        let _ = app.workspace_files.explorer.git_status.poll();
        if !app.workspace_files.explorer.git_status.status.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !app.workspace_files.explorer.git_status.status.is_empty(),
        "expected a dirty worktree for the footer count"
    );

    assert!(app.activity_summary().is_none());
    let rendered = render_app_text(&mut app, 140, 30);
    assert!(
        rendered.contains("0 tokens"),
        "footer last segment is session usage, not a change count:\n{rendered}"
    );
    assert!(
        !rendered.contains("1 changes"),
        "workspace change count must not appear in the footer:\n{rendered}"
    );
    assert!(
        !rendered.contains("Review"),
        "Review CTA must not appear in conversation or footer:\n{rendered}"
    );
}

#[tokio::test]
async fn alt_right_and_workspace_right_do_not_open_review() {
    let (_dir, mut app) = focus_test_app().await;
    app.workspace_files
        .explorer
        .git_status
        .status
        .insert(PathBuf::from("changed.rs"), GitStatusKind::Modified);
    app.focus_block(FocusBlock::Workspace);

    assert_eq!(
        app.semantic_command_for_global_key(press(KeyCode::Right, KeyModifiers::ALT)),
        None
    );
    assert_eq!(
        app.semantic_command_for_workspace_key(press(KeyCode::Right, KeyModifiers::NONE)),
        None
    );

    app.handle_key(press(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.workspace_navigation.current(), None);
}
