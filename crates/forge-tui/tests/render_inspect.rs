//! Exhaustive render-inspection harness for the Forge TUI.
//!
//! # Why this exists
//!
//! Two user-visible rendering concerns to characterise:
//!   1. missing blank lines before headings (and similar block separations)
//!      in streaming/static markdown answers;
//!   2. a small layout shift when a streaming turn finishes (the transcript
//!      re-renders statically and a turn-summary banner is inserted).
//!
//! This harness renders the *exact* public draw path (`TuiApp::draw` on a
//! ratatui `TestBackend`, plus `ConversationModel::lines_for_width` for
//! transcript-only inspection) and dumps every frame as plain text into
//! `$FORGE_RENDER_DUMP_DIR` (default `artifacts/pty-test/`) for eyeballing
//! and diffing.
//!
//! # Usage
//!
//! ```text
//! cargo test -p forge-tui --test render_inspect -- --ignored --nocapture
//! ```
//!
//! Frames are also sized like a real terminal (120x40 cells).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_config::FileIconMode;
use forge_core::{AgentSession, LoopConfig};
use forge_model::MockModelClient;
use forge_tools::ToolRegistry;
use forge_tui::{
    ChatItem, ConversationModel, ConversationRender, ConversationViewOpts, TuiApp,
    TuiRuntimeConfig,
};
use forge_types::{Message, MessageRole, ModelResponse, TaskLifecycle, Usage};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::TempDir;

// ---------------------------------------------------------------- dump helpers

fn dump_dir() -> PathBuf {
    std::env::var_os("FORGE_RENDER_DUMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("artifacts/pty-test"))
}

fn write_text(dir: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(dir).expect("create dump dir");
    let path = dir.join(name);
    std::fs::write(&path, text).expect("write dump");
    eprintln!("wrote {} ({} bytes)", path.display(), text.len());
}

/// Dump one full `TestBackend` frame (the real app chrome + conversation).
fn dump_app_frame(
    terminal: &mut Terminal<TestBackend>,
    app: &mut TuiApp,
    dir: &Path,
    name: &str,
) {
    terminal
        .draw(|frame| app.draw(frame))
        .expect("draw frame");
    let buf = terminal.backend().buffer();
    let (w, h) = (buf.area.width as usize, buf.area.height as usize);
    let mut out = String::new();
    for y in 0..h {
        let mut row = String::new();
        for x in 0..w {
            if let Some(cell) = buf.cell((x as u16, y as u16)) {
                row.push_str(cell.symbol());
            } else {
                row.push(' ');
            }
        }
        out.push_str(&row);
        out.push('\n');
    }
    // Blank-line census: rows whose visible cells are all blank.
    let blank_rows = out
        .lines()
        .filter(|l| l.trim().is_empty())
        .count();
    out.push_str(&format!("-- frame {name}: {w}x{h}, blank rows: {blank_rows}\n"));
    write_text(dir, &format!("{name}.txt"), &out);
}

/// Rendered transcript lines (conversation area only) for a set of messages.
fn transcript_lines(messages: Vec<Message>, width: usize) -> Vec<String> {
    let model = ConversationModel::from_messages(
        &messages,
        &[],
        TaskLifecycle::Working,
        ConversationViewOpts::default(),
    );
    model
        .lines_for_width(width)
        .into_iter()
        .map(|line| {
            let mut s = String::new();
            for span in &line.spans {
                s.push_str(span.content.as_ref());
            }
            s
        })
        .collect::<Vec<_>>()
}

/// Transcript lines plus the same lines annotated with trailing-blank state.
fn dump_transcript(dir: &Path, name: &str, lines: Vec<String>, width: usize) {
    let mut out = String::new();
    let mut blanks = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let is_blank = line.trim().is_empty();
        if is_blank {
            blanks += 1;
        }
        out.push_str(&format!(
            "{:>3} {} {}\n",
            i,
            if is_blank { "·" } else { " " },
            line
        ));
    }
    out.push_str(&format!(
        "-- {} lines at width {width}, blanks: {blanks}\n",
        lines.len()
    ));
    write_text(dir, &format!("{name}.txt"), &out);
}

fn user(text: &str) -> Message {
    Message::new(MessageRole::User, text)
}

fn assistant(text: &str) -> Message {
    Message::new(MessageRole::Assistant, text)
}

// ------------------------------------------------------------ A: spacing battery

/// Markdown answers exercising heading placement relative to every other
/// top-level block. Each name becomes a dump file name.
const SPACING_CASES: &[(&str, &str)] = &[
    (
        "heading_first",
        "## Starting heading\n\nBody paragraph right after the heading.\n\nSecond paragraph.",
    ),
    (
        "para_then_heading",
        "Opening paragraph of the answer.\n\n## Second heading\n\nClosing paragraph.",
    ),
    (
        "heading_then_code",
        "## Setup\n\n```rust\nfn f() -> u32 { 41 + 1 }\n```\n\nText after the code.",
    ),
    (
        "code_then_heading",
        "```rust\nfn f() -> u32 { 41 + 1 }\n```\n\n## After code\n\nText after the heading.",
    ),
    (
        "list_then_heading",
        "- first item\n- second item\n\n## After list\n\nText after the list.",
    ),
    (
        "heading_then_list",
        "## Items\n\n- alpha\n- beta\n\nText after the list.",
    ),
    (
        "table_then_heading",
        "| a | b |\n|---|---|\n| 1 | 2 |\n\n## After table\n\nText after the table.",
    ),
    (
        "heading_then_table",
        "## Table\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nText after the table.",
    ),
    (
        "rule_then_heading",
        "Lead text.\n\n---\n\n## After rule\n\nTail text.",
    ),
    (
        "h1_to_h6",
        "# One\n\n## Two\n\n### Three\n\n#### Four\n\n##### Five\n\n###### Six\n\nThe end.",
    ),
    (
        "heading_in_list",
        "- intro\n  ## Heading in item\n  body of item\n- outro",
    ),
    (
        "quote_then_heading",
        "> quoted line\n\n## After quote\n\nText after.",
    ),
    (
        "heading_after_quote",
        "## Before quote\n\n> quoted line\n\nText after.",
    ),
    (
        "sink",
        "Lead.\n\n| c1 | c2 |\n|---|---|\n| x | y |\n\n```sh\necho hi\n```\n\n- one\n\n## Sink heading\n\nEnd.",
    ),
];

#[test]
#[ignore]
fn spacing_battery() {
    let dir = dump_dir().join("spacing");
    for &(name, text) in SPACING_CASES {
        let msgs = vec![user("Build me the thing."), assistant(text)];
        for &width in &[60usize, 100, 140] {
            dump_transcript(
                &dir,
                &format!("{name}-w{width}"),
                transcript_lines(msgs.clone(), width),
                width,
            );
        }
        // Two-turn version: does a heading that follows a completed turn's
        // rule line get its blank?
        let two_turn = vec![
            user("First request."),
            assistant(text),
            user("Second request."),
            assistant("## Second answer\n\nBody of the second answer."),
        ];
        dump_transcript(
            &dir,
            &format!("{name}-2turn-w100"),
            transcript_lines(two_turn, 100),
            100,
        );
    }
}

#[test]
#[ignore]
fn empty_and_minimal() {
    let dir = dump_dir().join("spacing");
    // Nothing but a user message.
    dump_transcript(
        &dir,
        "just-user-w100",
        transcript_lines(vec![user("Hello.")], 100),
        100,
    );
    // Answer that is only a heading, then only a code fence.
    dump_transcript(
        &dir,
        "only-heading-w100",
        transcript_lines(vec![user("Q."), assistant("## Only heading")], 100),
        100,
    );
    dump_transcript(
        &dir,
        "only-code-w100",
        transcript_lines(vec![user("Q."), assistant("```rust\nfn f() {}\n```")], 100),
        100,
    );
}

// ------------------------------------------------------- B: streaming vs done

/// Long, block-rich answer so the transition is visible in a 40-row viewport.
const STREAM_TEXT: &str = r#"## Analysis

I looked at the rendering pipeline end to end. The transcript renderer walks
semantic blocks and lets the markdown renderer decide intra-block spacing, so
the places where a blank can be missing are exactly the block boundaries where
neither layer takes ownership.

### Where the blank goes

```rust
fn blank_before_block(out: &mut Vec<Line>) {
    match out.last() {
        None => {}
        Some(last) if last.width() == 0 => {}
        Some(_) => out.push(Line::from("")),
    }
}
```

That guards every heading. But a heading that ends with rule then code:

- looks tight
- right here

| case      | blank before heading |
|-----------|----------------------|
| after para | yes |
| after code | yes |
| after list | yes |

## Conclusion

The layout shift after streaming is explained by the turn summary banner
appearing at the tail plus the static re-render of the final answer, which
changes the number of visible lines below the last stable row.

### Checklist

1. reproduce the missing blank
2. capture mid-stream frames
3. capture the settled frame
4. diff the rows"#;

/// The same text split into streaming chunks (prefixes that grow monotonically).
fn stream_prefixes() -> Vec<&'static str> {
    let cuts = [110, 260, 460];
    let mut out = Vec::new();
    let mut prev = 0;
    for &c in &cuts {
        let cut = STREAM_TEXT
            .char_indices()
            .map(|(i, _)| i)
            .filter(|&i| i <= c)
            .last()
            .unwrap_or(c);
        out.push(&STREAM_TEXT[prev..cut]);
        prev = cut;
    }
    out.push(STREAM_TEXT);
    out
}

async fn app_with_messages(messages: Vec<Message>) -> (TempDir, TuiApp) {
    let dir = TempDir::new().expect("temp dir");
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: STREAM_TEXT.into(),
        tool_calls: vec![],
        usage: Some(Usage {
            prompt_tokens: 512,
            completion_tokens: 712,
            prompt_cache_read_tokens: 0,
            prompt_cache_write_tokens: 0,
        }),
        thinking: None,
    }]));
    let session = AgentSession::create(
        LoopConfig {
            max_turns: 4,
            workspace: dir.path().to_path_buf(),
            journal_dir: dir.path().join("j"),
            ..Default::default()
        },
        model,
        ToolRegistry::new(),
    )
    .await
    .expect("session creatable");
    for m in messages {
        session.messages.push(m);
    }
    let app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "render-inspect".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    (dir, app)
}

#[tokio::test]
#[ignore]
async fn streaming_then_done_layout_transition() {
    let dir = dump_dir().join("transition");
    let (_dir, mut app) = app_with_messages(vec![]).await;
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test backend");

    // F0: empty app (home card).
    dump_app_frame(&mut terminal, &mut app, &dir, "F0-idle-home");

    // Submit a prompt (queues the pending turn; the event loop would drain it).
    app.dispatch_line("Walk me through the renderer spacing.").await.unwrap();
    dump_app_frame(&mut terminal, &mut app, &dir, "F1-submitted");

    // Simulate live streaming: prefixes of the final answer, exactly as the
    // real draw path paints them (StreamMarkdownCache + streaming caret).
    for (i, prefix) in stream_prefixes().into_iter().enumerate() {
        app.stream_preview_for_tests(prefix);
        dump_app_frame(&mut terminal, &mut app, &dir, &format!("F{}-streaming-{}", i + 2, i));
    }

    // Run the turn to completion: same text, usage, and the turn-summary
    // banner that lands when streaming ends.
    app.drain_pending_prompt(None).await.expect("turn completes");
    dump_app_frame(&mut terminal, &mut app, &dir, "F6-done");
    dump_app_frame(&mut terminal, &mut app, &dir, "F7-done-settle");

    // Transcript-only comparison for the same content: static answer vs
    // static answer + TurnSummary banner (what the final frame adds).
    let msgs = vec![user("Walk me through the renderer spacing."), assistant(STREAM_TEXT)];
    dump_transcript(
        &dir,
        "T-static-answer",
        transcript_lines(msgs.clone(), 100),
        100,
    );
    let mut model = ConversationModel::from_messages(
        &msgs,
        &[],
        TaskLifecycle::Working,
        ConversationViewOpts::default(),
    );
    model = model.with_extra_banners([ChatItem::TurnSummary {
        secs: 23.4,
        chars: STREAM_TEXT.chars().count(),
        tools: 0,
        output_tokens: Some(712),
    }]);
    let lines = model
        .lines_for_width(100)
        .into_iter()
        .map(|line| {
            let mut s = String::new();
            for span in &line.spans {
                s.push_str(span.content.as_ref());
            }
            s
        })
        .collect::<Vec<_>>();
    dump_transcript(&dir, "T-static-answer-plus-summary", lines, 100);
}

// ------------------------------------------------------------------ utilities

#[test]
#[ignore]
fn dump_raw_transcripts() {
    // Full app frames for representative transcripts (draws the whole chrome).
    let dir = dump_dir().join("chrome");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let (_, mut app) = app_with_messages(vec![
            user("Build me the thing."),
            assistant(
                "## Plan\n\n- step one\n- step two\n\n```rust\nfn main() {}\n```\n\nDone.",
            ),
        ])
        .await;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("backend");
        dump_app_frame(&mut terminal, &mut app, &dir, "C-answer");
    });
}

#[test]
#[ignore]
fn report_width_sensitivity() {
    // The same answer at a range of widths: does spacing before headings
    // change with wrap?
    let dir = dump_dir().join("widths");
    let msgs = vec![
        user("Q."),
        assistant(
            "Some prose here.\n\n## Wide heading that wraps at narrow widths\n\nCode:\n\n```rust\nfn p() {}\n```\n\nDone.",
        ),
    ];
    for &width in &[32usize, 40, 48, 56, 64, 80, 100, 120, 140] {
        dump_transcript(
            &dir,
            &format!("w{width}"),
            transcript_lines(msgs.clone(), width),
            width,
        );
    }
}