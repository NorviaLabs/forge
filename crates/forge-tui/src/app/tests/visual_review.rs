//! Temporary visual-review dumper (added for a UI audit; not a committed test).

use super::prelude::*;
use crate::widgets::NavigatorTab;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn out_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("FORGE_VISUAL_DUMP_DIR")
            .expect("set FORGE_VISUAL_DUMP_DIR to a writable directory"),
    )
}

fn color_tag(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("rgb{r:02x}{g:02x}{b:02x}"),
        Color::Reset => "reset".to_string(),
        Color::Indexed(i) => format!("idx{i:03}"),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// Render one frame and write `<name>.cells` (styled grid) + `<name>.txt`.
fn dump(app: &mut TuiApp, name: &str, w: u16, h: u16) {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    app.tick_render_state();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let cursor = terminal.backend().cursor_position();
    let visible = terminal.backend().cursor_visible();
    let buf = terminal.backend().buffer();
    let area = *buf.area();

    let mut cells = String::new();
    let _ = writeln!(
        cells,
        "# frame {name} {w}x{h} cursor {} {} visible {visible} focus {:?} navtab {:?}",
        cursor.x,
        cursor.y,
        app.focus.block(),
        app.navigator_tab
    );
    let mut text = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let style = cell.style();
            let mut mods = String::new();
            if style.add_modifier.contains(Modifier::BOLD) {
                mods.push('B');
            }
            if style.add_modifier.contains(Modifier::DIM) {
                mods.push('D');
            }
            if style.add_modifier.contains(Modifier::ITALIC) {
                mods.push('I');
            }
            if style.add_modifier.contains(Modifier::UNDERLINED) {
                mods.push('U');
            }
            if style.add_modifier.contains(Modifier::REVERSED) {
                mods.push('R');
            }
            let _ = writeln!(
                cells,
                "{y} {x} {:?} {} {} {}",
                cell.symbol(),
                color_tag(style.fg.unwrap_or(Color::Reset)),
                color_tag(style.bg.unwrap_or(Color::Reset)),
                mods
            );
            text.push_str(cell.symbol());
        }
        text.push('\n');
    }
    std::fs::write(out_dir().join(format!("{name}.cells")), cells).unwrap();
    std::fs::write(out_dir().join(format!("{name}.txt")), text).unwrap();
}

async fn type_str(app: &mut TuiApp, s: &str) {
    for c in s.chars() {
        let key = if c == '\n' {
            press(KeyCode::Enter, KeyModifiers::NONE)
        } else {
            press(KeyCode::Char(c), KeyModifiers::NONE)
        };
        app.handle_key(key).await.unwrap();
    }
}

fn seed_workspace(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("README.md"),
        "# Demo\n\nA repo for visual review.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/skinny.rs"), "fn x() {}\n").unwrap();
}

/// A longer, obviously-overflowing filename to see how the explorer truncates.
fn seed_long_names(dir: &Path) {
    let long = "a-very-long-module-name-that-should-be-truncated-somewhere";
    std::fs::write(dir.join(format!("{long}.rs")), "fn long_name() {}\n").unwrap();
    std::fs::write(
        dir.join("src").join(format!("{long}.rs")),
        "fn nested() {}\n",
    )
    .unwrap();
}

#[tokio::test]
async fn dump_home_sizes() {
    let dir = TempDir::new().unwrap();
    seed_workspace(dir.path());
    for (w, h) in [
        (80u16, 18u16),
        (80, 24),
        (100, 30),
        (116, 40),
        (120, 40),
        (160, 50),
        (200, 60),
    ] {
        let (td, mut app) = focus_test_app().await;
        app.runtime.cwd = dir.path().to_path_buf();
        dump(&mut app, &format!("home-{w}x{h}"), w, h);
        drop(td);
    }
}

#[tokio::test]
async fn dump_focus_cycle() {
    let dir = TempDir::new().unwrap();
    seed_workspace(dir.path());
    let (_td, mut app) = focus_test_app().await;
    app.runtime.cwd = dir.path().to_path_buf();
    app.navigator_tab = NavigatorTab::Files;
    dump(&mut app, "focus-00-start", 160, 50);
    for i in 1..=9 {
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        dump(&mut app, &format!("focus-{i:02}-tab"), 160, 50);
    }
}

#[tokio::test]
async fn dump_composer_states() {
    let dir = TempDir::new().unwrap();
    seed_workspace(dir.path());
    let (_td, mut app) = focus_test_app().await;
    app.runtime.cwd = dir.path().to_path_buf();
    dump(&mut app, "composer-00-empty", 120, 40);
    type_str(&mut app, "Explain the layout module in one paragraph.").await;
    dump(&mut app, "composer-01-typed", 120, 40);
    for _ in 0..6 {
        app.handle_key(press(KeyCode::Enter, KeyModifiers::ALT))
            .await
            .unwrap();
        type_str(&mut app, "another wrapped line of input").await;
    }
    dump(&mut app, "composer-02-multiline", 120, 40);
    for _ in 0..6 {
        app.handle_key(press(KeyCode::Enter, KeyModifiers::ALT))
            .await
            .unwrap();
        type_str(&mut app, "line").await;
    }
    dump(&mut app, "composer-03-overflow", 120, 40);
    // Long single line to force horizontal scrolling of the composer.
    type_str(
        &mut app,
        " and now a really long unbroken tail that keeps going and going and going",
    )
    .await;
    dump(&mut app, "composer-04-longline", 120, 40);
    dump(&mut app, "composer-05-narrow", 80, 18);
}

#[tokio::test]
async fn dump_slash_menu() {
    let dir = TempDir::new().unwrap();
    seed_workspace(dir.path());
    let (_td, mut app) = focus_test_app().await;
    app.runtime.cwd = dir.path().to_path_buf();
    type_str(&mut app, "/").await;
    dump(&mut app, "slash-00-root", 120, 40);
    type_str(&mut app, "re").await;
    dump(&mut app, "slash-01-re", 120, 40);
    dump(&mut app, "slash-02-re-100", 100, 30);
    dump(&mut app, "slash-03-re-80", 80, 18);
    // No-match query.
    let (_td2, mut app2) = focus_test_app().await;
    type_str(&mut app2, "/zzzz").await;
    dump(&mut app2, "slash-04-nomatch", 120, 40);
    // Unknown command submitted.
    let (_td3, mut app3) = focus_test_app().await;
    type_str(&mut app3, "/nope").await;
    app3.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    dump(&mut app3, "slash-05-unknown", 120, 40);
}

#[tokio::test]
async fn dump_overlays() {
    let dir = TempDir::new().unwrap();
    seed_workspace(dir.path());
    let cases: Vec<(&str, &str)> = vec![
        ("help", "/help"),
        ("theme", "/theme"),
        ("model", "/model"),
        ("connect", "/connect"),
        ("sessions", "/sessions"),
        ("status", "/status"),
        ("context", "/context"),
        ("diff", "/diff"),
        ("terminal", "/terminal"),
    ];
    for (name, cmd) in cases {
        let (_td, mut app) = focus_test_app().await;
        app.runtime.cwd = dir.path().to_path_buf();
        type_str(&mut app, cmd).await;
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        dump(&mut app, &format!("overlay-{name}"), 120, 40);
        dump(&mut app, &format!("overlay-{name}-wide"), 160, 50);
    }
    // Bare `?` shortcut sheet.
    let (_td, mut app) = focus_test_app().await;
    app.handle_key(press(KeyCode::Char('?'), KeyModifiers::NONE))
        .await
        .unwrap();
    dump(&mut app, "overlay-qmark", 120, 40);
}

#[tokio::test]
async fn dump_approval_and_question() {
    let dir = TempDir::new().unwrap();
    seed_workspace(dir.path());
    let (_td, mut app) = focus_test_app().await;
    app.runtime.cwd = dir.path().to_path_buf();
    set_pending_hitl(&mut app, direct_hitl_payload("call-1", "src/main.rs"));
    dump(&mut app, "hitl-approval", 120, 40);
    dump(&mut app, "hitl-approval-wide", 160, 50);
    dump(&mut app, "hitl-approval-min", 80, 18);

    let (_td2, mut app2) = focus_test_app().await;
    set_pending_question(
        &mut app2,
        forge_types::QuestionPayload {
            call_id: "q-1".into(),
            tool: "ask_user_question".into(),
            questions: vec![forge_types::AskUserQuestionItem {
                id: "q1".into(),
                header: "Copy".into(),
                question: "Which tone should the onboarding copy use?".into(),
                options: vec![
                    forge_types::AskUserQuestionOption {
                        label: "Direct".into(),
                        description: "Short sentences, no hedging.".into(),
                    },
                    forge_types::AskUserQuestionOption {
                        label: "Warm".into(),
                        description: "Friendlier, more explanation.".into(),
                    },
                ],
                multi_select: false,
            }],
        },
    );
    dump(&mut app2, "hitl-question", 120, 40);
}

#[tokio::test]
async fn dump_transcript() {
    let (_td, mut app) = app_with_code("visual-audit").await;
    dump(&mut app, "transcript-code", 120, 40);
    dump(&mut app, "transcript-code-wide", 160, 50);
    dump(&mut app, "transcript-code-min", 80, 18);

    let (_td2, mut app2) = focus_test_app().await;
    app2.stream.preview =
        "Here is a partial answer that is still streaming into the transcript.".into();
    app2.stream.thinking = "Weighing two approaches before committing to one.".into();
    app2.stream.reveal_everything_for_tests();
    dump(&mut app2, "transcript-streaming", 120, 40);
}

#[tokio::test]
async fn dump_workspace_views() {
    let dir = TempDir::new().unwrap();
    seed_workspace(dir.path());
    seed_long_names(dir.path());
    let (_td, mut app) = focus_test_app().await;
    app.runtime.cwd = dir.path().to_path_buf();
    app.navigator_tab = NavigatorTab::Files;
    app.focus_block(crate::app::FocusBlock::Workspace);
    dump(&mut app, "workspace-empty", 160, 50);
    app.navigate_to_workspace_view(crate::app::WorkspaceView::File(
        dir.path().join("src/main.rs"),
    ));
    dump(&mut app, "workspace-file", 160, 50);
    dump(&mut app, "workspace-file-120", 120, 40);
    dump(&mut app, "workspace-file-80", 80, 18);

    // Explorer with long names, narrow enough to force truncation.
    let (_td2, mut app2) = focus_test_app().await;
    app2.runtime.cwd = dir.path().to_path_buf();
    app2.navigator_tab = NavigatorTab::Files;
    app2.focus_block(crate::app::FocusBlock::Files);
    dump(&mut app2, "explorer-long-names", 120, 40);
    dump(&mut app2, "explorer-long-names-80", 80, 18);
}

#[tokio::test]
async fn dump_approve_all_strip() {
    let dir = TempDir::new().unwrap();
    seed_workspace(dir.path());
    let (_td, mut app) = focus_test_app().await;
    app.runtime.cwd = dir.path().to_path_buf();
    app.approve_all = true;
    dump(&mut app, "approve-all-strip", 120, 40);
}

#[tokio::test]
async fn probe_modal_dim_accumulation() {
    use forge_connect::CatalogSource;
    let (_td, mut app) = focus_test_app().await;
    let items = vec![crate::overlays::ModelItem {
        provider: "native".into(),
        model: "mock-model".into(),
        profile_id: Some("mock".into()),
        source: CatalogSource::Default,
        route_label: "Mock".into(),
    }];
    app.overlay = Some(crate::overlays::Overlay::connect_model_open(
        vec![],
        items,
        Some("mock"),
        "mock",
        crate::ReasoningEffort::default(),
        crate::overlays::ConnectModelColumn::Models,
    ));
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    for pass in 0..4 {
        app.tick_render_state();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buf = terminal.backend().buffer();
        // Frame border cells: find the modal's left border column on its title row.
        let mut sample = Vec::new();
        for y in 0..40 {
            for x in 0..120 {
                let c = &buf[(x, y)];
                if c.symbol() == "│" {
                    sample.push((
                        x,
                        y,
                        format!("{:?}", c.style().fg),
                        format!("{:?}", c.style().bg),
                    ));
                }
            }
        }
        println!(
            "pass {pass}: {} vertical bars; first 6: {:?}",
            sample.len(),
            &sample[..sample.len().min(6)]
        );
    }
}
