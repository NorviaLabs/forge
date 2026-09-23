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

#[tokio::test]
async fn git_command_opens_live_working_tree_and_stage_refreshes_status() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("tracked.txt", "one\n")],
        &[("tracked.txt", "two\n")],
    );

    app.open_git_view();
    settle_git(&mut app);
    assert!(app.diff_view_is_open());
    assert_eq!(app.diff_view.source, DiffSource::WorkingTree);
    assert_eq!(
        app.diff_view.selected_path().unwrap(),
        std::path::Path::new("tracked.txt")
    );
    settle_patch(&mut app);
    assert!(matches!(app.diff_view.patch, PatchState::Ready(_)));

    app.stage_selected_diff_file(true);
    settle_git(&mut app);
    assert!(app
        .diff_view
        .entries
        .iter()
        .any(|entry| entry.path == std::path::Path::new("tracked.txt")));
    app.close_diff_view();
    assert!(!app.diff_view_is_open());
}

/// The commit flow: refuse with nothing staged, then commit exactly the index
/// and leave the unstaged change alone.
#[tokio::test]
async fn commit_covers_only_staged_changes_and_refuses_an_empty_index() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("staged.txt", "one\n"), ("loose.txt", "one\n")],
        &[("staged.txt", "two\n"), ("loose.txt", "two\n")],
    );

    app.open_git_view();
    settle_git(&mut app);

    // Nothing staged yet: the prompt must not open, because `git commit` would
    // only fail after the operator had typed a message.
    assert_eq!(app.staged_change_count(), 0);
    app.handle_diff_key(event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(app.overlay.is_none(), "an empty index opens no prompt");
    assert!(
        app.status_state.message.contains("Nothing staged"),
        "and says why: {}",
        app.status_state.message
    );

    // Stage one of the two changes, then commit through the real overlay path.
    app.diff_view
        .select_path(std::path::Path::new("staged.txt"));
    app.stage_selected_diff_file(true);
    settle_git(&mut app);
    assert_eq!(app.staged_change_count(), 1);

    app.handle_diff_key(event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(matches!(app.overlay, Some(Overlay::GitCommit { .. })));

    // Enter on an empty message reports the error instead of dispatching.
    assert_eq!(
        handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Enter),
        OverlayAction::None
    );
    assert!(matches!(
        app.overlay,
        Some(Overlay::GitCommit { error: Some(_), .. })
    ));

    for ch in "add staged change".chars() {
        handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Char(ch));
    }
    let OverlayAction::CommitGit { message } =
        handle_overlay_key(app.overlay.as_mut().unwrap(), OverlayKey::Enter)
    else {
        panic!("a non-empty message must dispatch a commit");
    };
    app.commit_staged_changes(&message);
    settle_git(&mut app);

    assert!(app.overlay.is_none(), "the prompt closes on commit");
    // The commit landed, with only the staged file in it.
    let log = git_stdout(dir.path(), &["log", "-1", "--pretty=%s"]);
    assert_eq!(log.trim(), "add staged change");
    let committed = git_stdout(dir.path(), &["show", "--stat", "--pretty=", "HEAD"]);
    assert!(committed.contains("staged.txt"), "{committed}");
    assert!(
        !committed.contains("loose.txt"),
        "an unstaged file must not ride along: {committed}"
    );
    // `loose.txt` is still modified in the worktree, uncommitted.
    assert_eq!(
        git_stdout(dir.path(), &["status", "--short"]),
        " M loose.txt\n"
    );
}

/// The sync row and the two refusals, against a real repository and a real
/// local remote — so a push that reports success has actually pushed.
#[tokio::test]
async fn the_sync_row_reports_the_branch_and_pull_push_end_to_end() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(dir.path(), &[("tracked.txt", "one\n")], &[]);
    let remote = TempDir::new().unwrap();
    // Bare: a non-bare remote refuses a push to the branch it has checked out,
    // which would test git's policy rather than this code.
    git_run(remote.path(), &["init", "--bare", "-q", "-b", "main"]);
    let remote_path = remote.path().to_str().unwrap().to_string();

    app.open_git_view();
    settle_git(&mut app);

    // No upstream yet: the row must not claim to know a branch state it cannot
    // read, and both operations have to be refused before spawning anything.
    settle_sync(&mut app);
    assert!(app.git_sync.branch.is_none() || app.git_sync.upstream().is_none());
    assert_eq!(app.git_sync_tag().as_deref(), Some("main"));
    app.start_git_sync(forge_workspace::git_sync::SyncOperation::Push);
    assert!(
        app.git_sync.running.is_none(),
        "a push with no upstream must not start"
    );
    assert!(
        app.feedback.text.contains("no upstream"),
        "and must say why: {}",
        app.feedback.text
    );

    // Give the branch an upstream and a local commit to send.
    git_run(dir.path(), &["remote", "add", "origin", &remote_path]);
    git_run(dir.path(), &["push", "-q", "-u", "origin", "main"]);
    std::fs::write(dir.path().join("tracked.txt"), "two\n").unwrap();
    git_run(dir.path(), &["commit", "-qam", "second"]);
    settle_sync(&mut app);
    assert_eq!(app.git_sync.upstream(), Some("origin/main"));
    assert_eq!(
        app.git_sync_tag().as_deref(),
        Some("main ↑1"),
        "one commit is waiting to go out"
    );

    app.start_git_sync(forge_workspace::git_sync::SyncOperation::Push);
    assert!(
        app.git_sync_tag()
            .as_deref()
            .is_some_and(|tag| tag.starts_with("Pushing origin/main")),
        "the row says what is happening now: {:?}",
        app.git_sync_tag()
    );
    settle_sync(&mut app);
    assert_eq!(app.feedback.severity, FeedbackSeverity::Info);
    assert_eq!(app.feedback.text, "Pushed origin/main");
    assert_eq!(
        app.git_sync_tag().as_deref(),
        Some("main"),
        "the count clears because the commit is on the remote"
    );

    // And it really landed there.
    let sent = git_stdout(
        remote.path(),
        &["log", "-1", "--pretty=%s", "refs/heads/main"],
    );
    assert_eq!(sent.trim(), "second");
}

/// A repository with no branch state reads as unknown, never as in sync.
#[tokio::test]
async fn an_unreadable_branch_renders_as_unknown_rather_than_clean() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("tracked.txt", "one\n")],
        &[("tracked.txt", "two\n")],
    );
    app.open_git_view();
    settle_git(&mut app);
    settle_sync(&mut app);

    // A branch read that failed has to say `?`. An empty tag would read as
    // "nothing to report", which is the one answer that is certainly wrong.
    app.git_sync.branch = None;
    app.git_sync.error = Some("git is not usable here".into());
    assert_eq!(app.git_sync_tag().as_deref(), Some("branch ?"));
    app.start_git_sync(forge_workspace::git_sync::SyncOperation::Pull);
    assert!(app.git_sync.running.is_none());
    assert!(
        app.feedback.text.contains("could not be read"),
        "and must not guess an upstream: {}",
        app.feedback.text
    );
}

fn git_run(dir: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Pump the branch read and any operation until both settle.
fn settle_sync(app: &mut TuiApp) {
    let root = app.session_view.workspace_root().to_path_buf();
    app.git_sync.start_branch_refresh(root);
    for _ in 0..600 {
        app.poll_git_sync();
        if !app.git_sync.loading && app.git_sync.running.is_none() {
            // One more pass so a finished operation's follow-up (a fresh branch
            // read) also lands before the test asserts.
            app.poll_git_sync();
            if !app.git_sync.loading {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the sync state never settled");
}

fn git_stdout(dir: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
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
async fn reopening_diff_refreshes_external_changes_before_listing_files() {
    let (dir, mut app) = focus_test_app().await;
    repo_with_changes(
        dir.path(),
        &[("tracked.txt", "one\n")],
        &[("tracked.txt", "two\n")],
    );

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    assert_eq!(app.diff_view.entries.len(), 1);

    std::fs::write(dir.path().join("tracked.txt"), "three\n").unwrap();
    std::fs::write(dir.path().join("external.txt"), "new\n").unwrap();

    app.open_diff_view(DiffSource::WorkingTree);
    settle_git(&mut app);
    let paths: Vec<_> = app
        .diff_view
        .entries
        .iter()
        .map(|entry| entry.path.display().to_string())
        .collect();
    assert_eq!(paths, vec!["external.txt", "tracked.txt"]);
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
async fn diff_keymap_handles_every_owned_navigation_command_without_a_selection() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_diff_view(DiffSource::WorkingTree);

    for key in [
        KeyCode::Char('j'),
        KeyCode::Char('k'),
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::PageDown,
        KeyCode::PageUp,
        KeyCode::Char('g'),
        KeyCode::Char('G'),
        KeyCode::Char(']'),
        KeyCode::Char('['),
        KeyCode::Char('n'),
        KeyCode::Char('p'),
        KeyCode::Char('d'),
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Char('m'),
        KeyCode::Char('v'),
        KeyCode::Char('s'),
        KeyCode::Char('u'),
        KeyCode::Char('o'),
        KeyCode::Char('?'),
    ] {
        assert!(app.handle_diff_key(event::KeyEvent::new(key, KeyModifiers::NONE)));
    }
    assert!(app.overlay.is_some());
    assert!(!app.handle_diff_key(event::KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE,)));
    assert!(app.handle_diff_key(event::KeyEvent::new(
        KeyCode::Char('d'),
        KeyModifiers::CONTROL,
    )));
    assert!(app.handle_diff_key(event::KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )));

    app.overlay = None;
    assert!(app.handle_diff_key(event::KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE,)));
    assert!(app.handle_diff_key(event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE,)));
    assert!(app.handle_diff_key(event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE,)));
    assert!(app.handle_diff_key(event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE,)));
    assert!(app.handle_diff_key(event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,)));
    assert!(app.handle_diff_key(event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,)));
    assert!(!app.diff_view_is_open());
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

    // The round-2 insets cost the diff pane a few columns, so the comfortable
    // width where hints keep every pair is 126 now.
    let rendered = render_app_text(&mut app, 126, 35);
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
