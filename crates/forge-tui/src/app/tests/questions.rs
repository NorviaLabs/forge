//! Inline `ask_user_question` prompt: option navigation, Other via composer.

use super::prelude::*;
use crossterm::event::{KeyCode, KeyModifiers};
use forge_types::{AskUserQuestionItem, AskUserQuestionOption, QuestionPayload};

fn db_question() -> QuestionPayload {
    QuestionPayload {
        call_id: "q-1".into(),
        tool: "ask_user_question".into(),
        questions: vec![AskUserQuestionItem {
            id: "db".into(),
            question: "Which database?".into(),
            header: "Database".into(),
            options: vec![
                AskUserQuestionOption {
                    label: "Postgres (Recommended)".into(),
                    description: "Relational default.".into(),
                },
                AskUserQuestionOption {
                    label: "SQLite".into(),
                    description: "Local file.".into(),
                },
            ],
            multi_select: false,
        }],
    }
}

fn set_pending_question_focused(app: &mut TuiApp, payload: QuestionPayload) {
    set_pending_question(app, payload);
    app.sync_question_focus();
    app.sync_question_menu();
}

fn multi_question() -> QuestionPayload {
    QuestionPayload {
        call_id: "q-multi".into(),
        tool: "ask_user_question".into(),
        questions: vec![AskUserQuestionItem {
            id: "checks".into(),
            question: "Which checks should I fix?".into(),
            header: "Checks".into(),
            options: vec![
                AskUserQuestionOption {
                    label: "Lint".into(),
                    description: "".into(),
                },
                AskUserQuestionOption {
                    label: "Tests".into(),
                    description: "".into(),
                },
                AskUserQuestionOption {
                    label: "Clippy".into(),
                    description: "".into(),
                },
            ],
            multi_select: true,
        }],
    }
}

fn last_tool_message(app: &TuiApp) -> String {
    app.session_runtime
        .messages
        .iter()
        .rev()
        .find(|m| m.role == forge_types::MessageRole::Tool)
        .map(|m| m.content.clone())
        .expect("tool message")
}

#[tokio::test]
async fn question_prompt_renders_inline() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, db_question());
    let text = render_app_text(&mut app, 100, 34);
    assert!(text.contains("Which database?"), "{text}");
    assert!(text.contains("1. Postgres (Recommended)"), "{text}");
    assert!(text.contains("Other"), "{text}");
    assert!(text.contains("Esc skip"), "{text}");
}

#[tokio::test]
async fn drawing_a_question_keeps_menu_focus_so_arrows_move() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, db_question());
    assert_eq!(app.focus.block(), FocusBlock::Approval);

    let first = render_app_text(&mut app, 100, 34);
    assert_eq!(app.focus.block(), FocusBlock::Approval);
    assert_eq!(app.question_menu_indexes(), (0, 0));
    assert!(first.contains("1. Postgres (Recommended)"), "{first}");

    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.question_menu_indexes(), (0, 1));

    let second = render_app_text(&mut app, 100, 34);
    assert_eq!(app.focus.block(), FocusBlock::Approval);
    assert!(second.contains("2. SQLite"), "{second}");
}

#[tokio::test]
async fn enter_on_an_option_submits_the_answer() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, db_question());
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.pending_interaction.has_question_submit());
    app.drain_pending_question(None).await.unwrap();
    assert!(app.session_runtime.pending_question().is_none());
    let tool_msg = app
        .session_runtime
        .messages
        .iter()
        .rev()
        .find(|m| m.role == forge_types::MessageRole::Tool)
        .expect("tool message");
    assert!(
        tool_msg.content.contains("Postgres (Recommended)"),
        "{}",
        tool_msg.content
    );
}

#[tokio::test]
async fn composer_text_answers_as_other() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, db_question());
    app.enter_chat_composer();
    app.input.set_text("MySQL");
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.pending_interaction.has_question_submit());
    app.drain_pending_question(None).await.unwrap();
    let tool_msg = app
        .session_runtime
        .messages
        .iter()
        .rev()
        .find(|m| m.role == forge_types::MessageRole::Tool)
        .expect("tool message");
    assert!(tool_msg.content.contains("MySQL"), "{}", tool_msg.content);
}

#[tokio::test]
async fn multi_select_renders_checkboxes_and_live_count() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, multi_question());

    let before = render_app_text(&mut app, 100, 34);
    assert!(before.contains("[ ] Lint"), "{before}");
    assert!(before.contains("[ ] Tests"), "{before}");
    assert!(before.contains("0 selected"), "{before}");

    app.handle_key(press(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();
    let after = render_app_text(&mut app, 100, 34);
    assert!(after.contains("[x] Lint"), "{after}");
    assert!(after.contains("[ ] Tests"), "{after}");
    assert!(after.contains("1 selected"), "{after}");
}

#[tokio::test]
async fn space_toggles_only_the_highlighted_option() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, multi_question());
    assert_eq!(app.question_menu_indexes(), (0, 0));

    app.handle_key(press(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.question_menu_indexes(), (0, 1));
    app.handle_key(press(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();

    let text = render_app_text(&mut app, 100, 34);
    assert!(text.contains("[x] Lint"), "{text}");
    assert!(text.contains("[x] Tests"), "{text}");
    assert!(text.contains("[ ] Clippy"), "{text}");

    app.handle_key(press(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();
    let text = render_app_text(&mut app, 100, 34);
    assert!(text.contains("[ ] Tests"), "{text}");
    assert!(text.contains("1 selected"), "{text}");
}

#[tokio::test]
async fn enter_confirms_every_selected_option() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, multi_question());
    app.handle_key(press(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.pending_interaction.has_question_submit());
    app.drain_pending_question(None).await.unwrap();

    let content = last_tool_message(&app);
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    let selected = value["answers"][0]["selected"].as_array().unwrap();
    assert_eq!(selected.len(), 2, "{content}");
    assert!(selected.iter().any(|v| v == "Lint"), "{content}");
    assert!(selected.iter().any(|v| v == "Tests"), "{content}");
}

#[tokio::test]
async fn multi_select_composes_with_free_text_other() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, multi_question());
    app.handle_key(press(KeyCode::Char(' '), KeyModifiers::NONE))
        .await
        .unwrap();

    app.enter_chat_composer();
    app.input.set_text("Formatting");
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.pending_interaction.has_question_submit());
    app.drain_pending_question(None).await.unwrap();

    let content = last_tool_message(&app);
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(value["answers"][0]["custom"], "Formatting", "{content}");
    let selected = value["answers"][0]["selected"].as_array().unwrap();
    assert_eq!(selected.len(), 1, "{content}");
    assert!(selected.iter().any(|v| v == "Lint"), "{content}");
}
