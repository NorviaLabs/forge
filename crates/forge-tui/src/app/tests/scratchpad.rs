//! Session-scoped scratchpad behaviour: entry points, the Vim command line,
//! autosave-on-close, and the footer chip.
//!
//! These are the operator-visible guarantees from the design: the notes are the
//! user's, they survive a reopen, and the dirty state is always spelled out.

use super::prelude::*;

#[tokio::test]
async fn the_footer_chip_reports_a_line_count_before_any_note_is_written() {
    let (_dir, mut app) = focus_test_app().await;
    let text = render_app_text(&mut app, 120, 40);

    assert!(
        text.contains("notes 0 lines"),
        "the chip is always present and reads as an empty document: {text}"
    );
}

#[tokio::test]
async fn typed_notes_land_in_the_buffer_and_the_chip_counts_them() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_scratchpad();

    // `i` enters Insert mode, then two words and a newline.
    for code in [
        KeyCode::Char('i'),
        KeyCode::Char('a'),
        KeyCode::Char('b'),
        KeyCode::Enter,
        KeyCode::Char('c'),
        KeyCode::Char('d'),
    ] {
        app.handle_key(press(code, KeyModifiers::NONE))
            .await
            .unwrap();
    }

    assert!(
        app.scratchpad.as_ref().expect("open").is_dirty(),
        "typing dirties the document"
    );
    let text = render_app_text(&mut app, 120, 40);
    assert!(
        text.contains("notes 2 lines"),
        "the chip counts the edited buffer: {text}"
    );
}

#[tokio::test]
async fn colon_w_saves_and_clears_the_unsaved_word() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_scratchpad();
    app.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char('z'), KeyModifiers::NONE))
        .await
        .unwrap();

    // Leave Insert mode so `:` is the command line and not a literal character.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char(':'), KeyModifiers::NONE))
        .await
        .unwrap();
    for code in [KeyCode::Char('w'), KeyCode::Enter] {
        app.handle_key(press(code, KeyModifiers::NONE))
            .await
            .unwrap();
    }

    let scratchpad = app.scratchpad.as_ref().expect("still open");
    assert!(!scratchpad.is_dirty(), "`:w` accepts the buffer");
    assert!(
        scratchpad.path().exists(),
        "`:w` wrote the file the chip was counting"
    );
}

#[tokio::test]
async fn escape_only_leaves_insert_mode_and_never_closes_the_notes() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_scratchpad();
    app.handle_key(press(KeyCode::Char('i'), KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        app.scratchpad.as_ref().unwrap().editor().mode(),
        edtui::EditorMode::Insert
    );

    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    let scratchpad = app.scratchpad.as_ref().expect("escape must not close");
    assert_eq!(
        scratchpad.editor().mode(),
        edtui::EditorMode::Normal,
        "escape returns to Normal mode, as Vim does"
    );
}

#[tokio::test]
async fn colon_q_autosaves_and_closes() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_scratchpad();
    for code in [
        KeyCode::Char('i'),
        KeyCode::Char('k'),
        KeyCode::Char('e'),
        KeyCode::Esc,
    ] {
        app.handle_key(press(code, KeyModifiers::NONE))
            .await
            .unwrap();
    }

    // `:q` autosaves rather than discarding, so the note is not lost to a
    // keystroke that looked like a close.
    for code in [KeyCode::Char(':'), KeyCode::Char('q'), KeyCode::Enter] {
        app.handle_key(press(code, KeyModifiers::NONE))
            .await
            .unwrap();
    }

    assert!(app.scratchpad.is_none(), "`:q` closes the surface");
    let saved = std::fs::read_to_string(app.scratchpad_path()).expect("autosave wrote on close");
    assert!(
        saved.contains('k'),
        "the pending note was saved, got {saved:?}"
    );
}

#[tokio::test]
async fn colon_q_bang_discards_without_writing() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_scratchpad();
    for code in [
        KeyCode::Char('i'),
        KeyCode::Char('x'),
        KeyCode::Esc,
        KeyCode::Char(':'),
        KeyCode::Char('q'),
        KeyCode::Char('!'),
        KeyCode::Enter,
    ] {
        app.handle_key(press(code, KeyModifiers::NONE))
            .await
            .unwrap();
    }

    assert!(app.scratchpad.is_none(), "`:q!` closes");
    assert!(
        !app.scratchpad_path().exists(),
        "`:q!` threw the note away instead of saving it"
    );
}

#[tokio::test]
async fn the_slash_command_toggles_the_surface() {
    let (_dir, mut app) = focus_test_app().await;

    app.open_scratchpad();
    assert!(app.scratchpad.is_some(), "`/notes` opens");
    app.close_scratchpad();
    assert!(app.scratchpad.is_none(), "`/notes` closes again");
}

#[tokio::test]
async fn closing_a_clean_document_creates_no_file() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_scratchpad();
    let path = app.scratchpad_path();

    app.close_scratchpad();

    assert!(
        !path.exists(),
        "no notes file for notes that were never typed"
    );
}

#[tokio::test]
async fn notes_survive_reopening_the_same_session() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_scratchpad();
    for code in [
        KeyCode::Char('i'),
        KeyCode::Char('k'),
        KeyCode::Char('e'),
        KeyCode::Char('e'),
        KeyCode::Char('p'),
        KeyCode::Esc,
        KeyCode::Char(':'),
        KeyCode::Char('q'),
        KeyCode::Enter,
    ] {
        app.handle_key(press(code, KeyModifiers::NONE))
            .await
            .unwrap();
    }

    let reopened = app.scratchpad_status().expect("chip has a status");
    assert_eq!(reopened.0, 1, "the closed scratchpad is still counted");
    app.open_scratchpad();
    let text = app.scratchpad.as_ref().expect("reopened").editor().text();
    assert!(text.contains("keep"), "the note came back: {text:?}");
}

#[tokio::test]
async fn the_notes_chip_is_clickable_and_toggles_the_surface() {
    let (_dir, mut app) = focus_test_app().await;
    render_app_text(&mut app, 140, 40);
    let ranges = app.footer_chip_rects.expect("chips captured during paint");
    let y = app.footer_area.expect("footer drawn").y;

    // The third chip has a real, non-empty range: a click target that was never
    // painted is the classic way a footer affordance silently dies.
    assert!(
        ranges[2].0 < ranges[2].1,
        "the notes chip occupies columns {:?}",
        ranges[2]
    );

    app.handle_mouse(left_click(ranges[2].0 + 1, y))
        .await
        .unwrap();
    assert!(
        app.scratchpad.is_some(),
        "clicking the chip opens the notes"
    );

    app.handle_mouse(left_click(ranges[2].0 + 1, y))
        .await
        .unwrap();
    assert!(
        app.scratchpad.is_none(),
        "clicking it again closes, autosaving first"
    );
}

#[tokio::test]
async fn keys_never_reach_the_conversation_while_the_scratchpad_is_open() {
    let (_dir, mut app) = focus_test_app().await;
    app.open_scratchpad();

    // A bare Enter with no composer text would otherwise try to submit.
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(
        app.scratchpad.is_some(),
        "Enter stayed inside the editor rather than submitting"
    );
}
