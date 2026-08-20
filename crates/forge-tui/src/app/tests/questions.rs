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

#[tokio::test]
async fn question_prompt_renders_inline() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, db_question());
    let text = render_app_text(&mut app, 100, 24);
    assert!(text.contains("Which database?"), "{text}");
    assert!(text.contains("Postgres (Recommended)"), "{text}");
    assert!(text.contains("Other"), "{text}");
    assert!(text.contains("Esc skip"), "{text}");
}

#[tokio::test]
async fn drawing_a_question_keeps_menu_focus_so_arrows_move() {
    let (_dir, mut app) = focus_test_app().await;
    set_pending_question_focused(&mut app, db_question());
    assert_eq!(app.focus.block(), FocusBlock::Approval);

    let first = render_app_text(&mut app, 100, 24);
    assert_eq!(app.focus.block(), FocusBlock::Approval);
    assert_eq!(app.question_menu_indexes(), (0, 0));
    assert!(first.contains("› Postgres (Recommended)"), "{first}");

    app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.question_menu_indexes(), (0, 1));

    let second = render_app_text(&mut app, 100, 24);
    assert_eq!(app.focus.block(), FocusBlock::Approval);
    assert!(second.contains("› SQLite"), "{second}");
    assert!(!second.contains("› Postgres (Recommended)"), "{second}");
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
    assert!(app.session.pending_question().is_none());
    let tool_msg = app
        .session
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
        .session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == forge_types::MessageRole::Tool)
        .expect("tool message");
    assert!(tool_msg.content.contains("MySQL"), "{}", tool_msg.content);
}
