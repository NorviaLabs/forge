//! Opt-in, identical-workload comparison across the UI rehaul stack.
//! Reports dispatch + TestBackend draw time; no wall-clock CI assertion.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use forge_core::{AgentSession, LoopConfig};
use forge_model::MockModelClient;
use forge_tools::ToolRegistry;
use forge_tui::{TuiApp, TuiRuntimeConfig};
use forge_types::{Message, MessageRole};
use ratatui::{backend::TestBackend, Terminal};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

async fn fixture(workload: &str) -> (TempDir, TuiApp) {
    let directory = TempDir::new().unwrap();
    let mut session = AgentSession::create(
        LoopConfig {
            workspace: directory.path().into(),
            journal_dir: directory.path().join("j"),
            ..Default::default()
        },
        Arc::new(MockModelClient::script(vec![])),
        ToolRegistry::new(),
    )
    .await
    .unwrap();
    let turns = if workload == "history" { 150 } else { 1 };
    for index in 0..turns {
        session.messages.push(Message::new(
            MessageRole::User,
            format!("Inspect migration step {index}."),
        ));
        session.messages.push(Message::new(
            MessageRole::Assistant,
            format!(
                "Evidence for step {index}. {}\n\n```rust\nfn step_{index}() {{}}\n```",
                "Retain the original error. ".repeat(20)
            ),
        ));
    }
    if workload == "jobs" {
        for index in 0..40 {
            session
                .spawn_background_shell(
                    "printf 'Actual benchmark evidence λ/東京\\n'".into(),
                    format!("Benchmark job {index}"),
                )
                .await
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            session.poll_background_tasks().await.unwrap();
            if session
                .background()
                .list()
                .all(|task| task.status.is_terminal())
            {
                break;
            }
            assert!(Instant::now() < deadline, "benchmark jobs did not settle");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            provider: "mock".into(),
            model_label: "mock".into(),
            cwd: directory.path().into(),
            ..Default::default()
        },
    );
    (directory, app)
}

#[test]
#[ignore = "manual latency comparison, not a timing gate"]
fn report_rehaul_key_to_buffer_latency() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut results = Vec::new();
        let selected = std::env::var("FORGE_LATENCY_WORKLOAD").ok();
        let rounds_count = std::env::var("FORGE_LATENCY_ROUNDS")
            .map(|value| value.parse::<usize>().expect("positive round count"))
            .unwrap_or(5);
        assert!((1..=100).contains(&rounds_count));
        for workload in ["typing", "stream", "jobs", "history", "draft"] {
            if selected.as_ref().is_some_and(|value| value != workload) {
                continue;
            }
            let mut rounds = Vec::new();
            for _round in 0..rounds_count {
                let (_directory, mut app) = fixture(workload).await;
                let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
                if workload == "draft" {
                    for _ in 0..8000 {
                        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
                            .await
                            .unwrap();
                    }
                }
                if workload == "stream" {
                    app.stream_preview_for_tests("Available provider evidence.\n\n");
                }
                for _ in 0..5 {
                    terminal.draw(|frame| app.draw(frame)).unwrap();
                }
                let mut samples = Vec::new();
                for index in 0..100 {
                    let began = Instant::now();
                    if workload == "stream" {
                        app.stream_preview_for_tests(&format!(
                            "Chunk {index}: preserve the original error in λ/東京.rs.\n\n"
                        ));
                    }
                    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE))
                        .await
                        .unwrap();
                    terminal.draw(|frame| app.draw(frame)).unwrap();
                    samples.push(began.elapsed().as_micros() as u64);
                }
                samples.sort_unstable();
                rounds.push(serde_json::json!({"median_us":samples[50], "p95_us":samples[95]}));
            }
            println!("{workload}: {}", serde_json::to_string(&rounds).unwrap());
            results.push(
                serde_json::json!({"workload":workload,"rounds":rounds,"keys_per_round":100}),
            );
        }
        let path = std::env::var("FORGE_LATENCY_REPORT").expect("set FORGE_LATENCY_REPORT");
        assert!(!results.is_empty(), "unknown latency workload");
        std::fs::write(
            Path::new(&path),
            serde_json::to_vec_pretty(&results).unwrap(),
        )
        .unwrap();
    });
}
