//! `/diff` — opening, closing, keys and empty/error states.

use super::prelude::*;
use crate::diff_view::{DiffSource, DiffStatus, PatchState};

/// Commit `files` as the baseline, then apply `changes` on top so the
/// workspace has a real diff against `HEAD`.
fn repo_with_changes(dir: &std::path::Path, baseline: &[(&str, &str)], changes: &[(&str, &str)]) {
    init_repo(dir);
    for (name, body) in baseline {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }
    for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "baseline"]] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(&args)
            .status()
            .unwrap()
            .success());
    }
    for (name, body) in changes {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }
}

/// Drive the async git-status cache to completion, the way the event loop
/// tick does, so a test can assert on a settled file list.
fn settle_git(app: &mut TuiApp) {
    let root = app.session_view.workspace_root().to_path_buf();
    // `TuiApp::new` may already have a refresh in flight from before the test
    // wrote its files, and `start_refresh` coalesces onto it — so one `poll`
    // returning `true` can be the *stale* answer. Keep pumping until the
    // queue drains and nothing is left running.
    app.workspace_files
        .explorer
        .git_status
        .start_refresh(root.clone());
    for _ in 0..400 {
        app.workspace_files.explorer.git_status.poll();
        app.workspace_files.explorer.git_status.poll_diff();
        if !app.workspace_files.explorer.git_status.loading {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.refresh_diff_entries();
}

/// Pump `pump_diff_view` until the selected file's patch lands.
fn settle_patch(app: &mut TuiApp) {
    for _ in 0..400 {
        app.workspace_files.explorer.git_status.poll_diff();
        app.pump_diff_view();
        if matches!(app.diff_view.patch, PatchState::Ready(_)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[tokio::test]
async fn diff_lists_modified_and_untracked_files_in_a_session_that_edited_nothing() {
    // Claude Code only shows an untracked file when the same session created
    // it. Ours must not depend on session history at all.
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("tracked.txt", "one\n")],
        &[("tracked.txt", "two\n"), ("brand_new.txt", "hello\n")],
    );

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);

    let paths: Vec<String> = app
        .diff_view
        .entries
        .iter()
        .map(|entry| entry.path.display().to_string())
        .collect();
    assert!(paths.contains(&"tracked.txt".to_string()), "{paths:?}");
    assert!(paths.contains(&"brand_new.txt".to_string()), "{paths:?}");
    let untracked = app
        .diff_view
        .entries
        .iter()
        .find(|entry| entry.path.ends_with("brand_new.txt"))
        .unwrap();
    assert_eq!(untracked.marker, "A");
}

#[tokio::test]
async fn diff_filters_the_explorer_and_esc_restores_it() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("kept.txt", "one\n")],
        &[("kept.txt", "two\n")],
    );
    app.workspace_files.visible = true;

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    assert!(
        app.workspace_files.explorer.diff_filter_is_active(),
        "the explorer becomes the changed-file list"
    );

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        !app.workspace_files.explorer.diff_filter_is_active(),
        "one Esc restores the full tree — no level-by-level unwinding"
    );
    assert!(!app.diff_view_is_open());
}

#[tokio::test]
async fn diff_on_a_clean_tree_says_so_instead_of_doing_nothing() {
    // Claude Code's `/diff` on a clean tree is a silent no-op; you cannot tell
    // whether the command ran. The mode must open and explain itself.
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("only.txt", "one\n")], &[]);

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);

    assert!(app.diff_view_is_open(), "the pane stays open");
    assert_eq!(app.diff_view.status, DiffStatus::NoChanges);
}

#[tokio::test]
async fn diff_outside_a_git_repository_explains_and_stays_closed_to_git() {
    let (_dir, mut app) = focus_test_app().await;
    // No `init_repo` — the fixture directory is a plain folder.
    app.open_diff_view(DiffSource::WorkingTree);
    assert_eq!(app.diff_view.status, DiffStatus::NotARepo);
    assert!(
        app.diff_view_is_open(),
        "the message needs somewhere to live"
    );
}

#[tokio::test]
async fn enter_in_the_patch_pane_does_not_leave_review() {
    // Enter means "show this file's patch" in the explorer half of this mode.
    // It must not also mean "leave review and open the editor" in the patch
    // half — that is what `o` is for.
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("a.txt", "one\n")], &[("a.txt", "two\n")]);

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(
        app.diff_view_is_open(),
        "Enter must leave the diff view where it is"
    );
}

#[tokio::test]
async fn file_and_hunk_keys_move_within_the_pane() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("a.txt", "one\n"), ("b.txt", "one\n")],
        &[("a.txt", "two\n"), ("b.txt", "two\n")],
    );

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    assert_eq!(app.diff_view.entries.len(), 2);

    let first = app.diff_view.selected;
    app.handle_key(press(KeyCode::Char('n'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_ne!(app.diff_view.selected, first, "n moves to the next file");
    app.handle_key(press(KeyCode::Char('p'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.diff_view.selected, first, "p moves back");
}

#[tokio::test]
async fn d_toggles_the_source_and_resets_the_patch() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("a.txt", "one\n")], &[("a.txt", "two\n")]);

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.diff_view.source, DiffSource::LastTurn);
    assert_eq!(app.diff_view.patch, PatchState::Loading);
    assert!(
        app.diff_view.loaded_for.is_none(),
        "no stale patch survives"
    );
}

#[tokio::test]
async fn unhandled_keys_fall_through_instead_of_being_swallowed() {
    // Keys the diff pane does not use must reach the composer. Silently
    // eating them is the failure that loses a pasted message.
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("a.txt", "one\n")], &[("a.txt", "two\n")]);

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    app.handle_key(press(KeyCode::Char('z'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.input.text, "z", "the keystroke reached the composer");
}

#[tokio::test]
async fn the_patch_pane_loads_the_selected_file() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("a.txt", "one\ntwo\nthree\n")],
        &[("a.txt", "one\nCHANGED\nthree\n")],
    );

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    settle_patch(&mut app);

    let PatchState::Ready(patch) = &app.diff_view.patch else {
        panic!("patch never landed: {:?}", app.diff_view.patch);
    };
    assert_eq!(patch.added, 1);
    assert_eq!(patch.removed, 1);
    assert!(
        app.diff_view.header().contains("+1 -1"),
        "{}",
        app.diff_view.header()
    );
}

#[tokio::test]
async fn o_opens_the_file_at_the_line_under_the_cursor() {
    // The payoff of not being a full-screen modal: jump from a hunk into the
    // editable file at the right place, conversation still on screen.
    let (dir, mut app) = focus_test_app().await;
    let baseline: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    let changed = baseline.replace("line 30\n", "line 30 CHANGED\n");
    repo_with_changes(dir.path(), &[("a.txt", &baseline)], &[("a.txt", &changed)]);

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    settle_patch(&mut app);

    // Walk to the added line so the cursor sits on new-file line 30.
    let PatchState::Ready(patch) = &app.diff_view.patch else {
        panic!("patch never landed");
    };
    let added = patch
        .lines
        .iter()
        .position(|line| line.contains("line 30 CHANGED"))
        .expect("the added line is in the patch");
    app.diff_view.scroll = added;
    assert_eq!(app.diff_view.new_file_line_at_scroll(), Some(30));

    app.handle_key(press(KeyCode::Char('o'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.current_workspace_is_file(), "o leaves the diff view");
    assert!(
        !app.workspace_files.explorer.diff_filter_is_active(),
        "the full tree comes back with the file"
    );
    assert_eq!(
        app.source_viewer.current_line, 29,
        "the viewer lands on new-file line 30 (0-based 29)"
    );
    if let Some(editor) = app.editor_session.as_ref() {
        assert_eq!(editor.cursor_row(), 29, "the editor cursor moves too");
    }
}

#[tokio::test]
async fn the_pane_shows_its_own_keymap() {
    // `?` is not discoverable on its own; the keys have to be on screen.
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    app.workspace_files.visible = true;

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    settle_patch(&mut app);

    let rendered = render_app_text(&mut app, 120, 35);
    assert!(rendered.contains("] [ hunk"), "{rendered}");
    assert!(rendered.contains("m done"), "{rendered}");
    assert!(rendered.contains("Esc close"), "{rendered}");
}

#[tokio::test]
async fn a_narrow_pane_keeps_every_key_even_when_the_verbs_go() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    app.workspace_files.visible = true;

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    settle_patch(&mut app);

    let rendered = render_app_text(&mut app, 80, 24);
    assert!(
        !rendered.contains("] [ hunk"),
        "verbs drop first:\n{rendered}"
    );
    for key in ["] [", "n p", "m", "?", "Esc"] {
        assert!(rendered.contains(key), "lost {key:?} from:\n{rendered}");
    }
}

#[tokio::test]
async fn the_search_prompt_owns_the_keyboard_while_it_is_open() {
    // `n` is "next file" normally and a letter while searching. Nothing may
    // fire underneath the prompt.
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("a.txt", "one\n"), ("b.txt", "one\n")],
        &[("a.txt", "two\n"), ("b.txt", "two\n")],
    );
    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    settle_patch(&mut app);

    let selected = app.diff_view.selected;
    app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
        .await
        .unwrap();
    for ch in "no".chars() {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE))
            .await
            .unwrap();
    }
    assert_eq!(app.diff_view.search.query, "no");
    assert_eq!(
        app.diff_view.selected, selected,
        "`n` was query text, not a file jump"
    );

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.diff_view.search.open);
    assert!(
        app.diff_view_is_open(),
        "Esc closed the prompt, not the view"
    );
}

#[tokio::test]
async fn m_marks_reviewed_and_moves_on() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("a.txt", "one\n"), ("b.txt", "one\n")],
        &[("a.txt", "two\n"), ("b.txt", "two\n")],
    );
    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);

    let first = app.diff_view.selected_path().unwrap().to_path_buf();
    app.handle_key(press(KeyCode::Char('m'), KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.diff_view.reviewed.contains(&first));
    assert_ne!(
        app.diff_view.selected_path().unwrap(),
        first,
        "marking one done moves to the next unreviewed file"
    );
    assert!(app.status_state.message.contains("1 of 2 reviewed"));
}

#[tokio::test]
async fn s_stages_the_selected_file_and_u_puts_it_back() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);

    let staged = |root: &std::path::Path| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(staged(dir.path()), "a.txt", "s stages");

    app.handle_key(press(KeyCode::Char('u'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(staged(dir.path()), "", "u unstages");
}

#[tokio::test]
async fn staging_is_refused_on_the_last_turn_source() {
    // The last-turn view is a reading of the transcript, not of the index.
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    app.diff_view.source = DiffSource::LastTurn;

    app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
        .await
        .unwrap();

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "nothing was staged"
    );
}
