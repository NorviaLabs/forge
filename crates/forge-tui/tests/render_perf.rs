//! Render-cost regression guards for the TUI draw path.
//!
//! # Why this exists
//!
//! A frame's cost must be bounded by the size of the **viewport**, not by the
//! length of the conversation. When that invariant breaks, the symptom is a TUI
//! that feels fine in a short session and degrades as the session grows —
//! exactly the kind of regression that no unit test notices and no reviewer
//! spots in a diff.
//!
//! Three fixes established that invariant (repo header moved off the render
//! path, cached transcript lines shared rather than deep-copied per frame, and
//! input drained before repainting). These guards keep it.
//!
//! # Why there is also an absolute budget, and a cache-miss guard
//!
//! Scaling guards compare two transcript lengths, so they say nothing about
//! work that is merely constant-but-large. Per-frame cost later grew 2.6x
//! underneath them from exactly that kind of work. The budget below is
//! therefore held close to the measured value rather than left loose.
//!
//! They also only measure cache *hits*. A frame that misses the conversation
//! cache rebuilds the whole transcript, and every render-key change (busy-phase
//! flip, banner, activity summary, resize, theme change) pays it mid-session.
//! `cache_miss_cost_per_message_is_bounded` covers that path.
//!
//! # Why allocation counts, not timings
//!
//! Wall-clock on a shared CI runner is noisy enough to be useless as a gate: the
//! same unmodified code measured 3.6ms and 9.0ms per empty frame in two sessions
//! on the same machine, while allocation counts for those runs differed by one.
//! A timing threshold would either flake or be set so loose it catches nothing.
//!
//! Allocation counts are deterministic and machine-independent, so they can live
//! in the ordinary test suite. Timings are available through the opt-in report
//! below, for humans, and are never asserted on.
//!
//! # Usage
//!
//! The guards run as part of `cargo test`. The measurement table is `#[ignore]`d
//! because it is a tool rather than a check:
//!
//! ```text
//! cargo test -p forge-tui --test render_perf --release -- --ignored --nocapture
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use forge_config::FileIconMode;
use forge_core::{AgentSession, LoopConfig};
use forge_model::MockModelClient;
use forge_tools::ToolRegistry;
use forge_tui::{TuiApp, TuiRuntimeConfig};
use forge_types::{Message, MessageRole, ModelResponse};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::TempDir;

// ----------------------------------------------------------- counting allocator

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicUsize = AtomicUsize::new(0);

/// Counts allocations while armed. Arming is global, so every measurement must
/// hold [`lock_measurement`] to keep another test's allocations out of the total.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Relaxed) == 1 {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size(), Relaxed);
        }
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Relaxed) == 1 {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(new_size, Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Serialises measurement. The allocation counters are process-global, so two
/// tests measuring at once would each see the other's allocations. Follows the
/// repo's pattern for process-global state (`lock_env` in `editor.rs`), and
/// recovers poisoning so one failing test does not cascade into the rest.
fn lock_measurement() -> MutexGuard<'static, ()> {
    static GUARD: Mutex<()> = Mutex::new(());
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy)]
struct FrameCost {
    allocs: usize,
    bytes: usize,
    p50_micros: f64,
}

// ------------------------------------------------------------------- fixtures

const CODE: &str = r#"pub fn process(items: &[Item]) -> Result<Summary, Error> {
    let mut total = 0usize;
    for item in items.iter().filter(|i| i.active) {
        total = total.checked_add(item.weight).ok_or(Error::Overflow)?;
    }
    Ok(Summary { total, count: items.len() })
}"#;

/// A realistic assistant turn: prose plus a fenced Rust block, so the syntax
/// highlighter is exercised too.
///
/// The block is distinct per turn. Reusing one identical block would let any
/// content-keyed caching dedupe within a single rebuild, flattering the numbers
/// relative to a real transcript where every turn shows different code.
fn assistant_body(turn: usize) -> String {
    let code = CODE.replace("process", &format!("process_{turn}"));
    format!(
        "Here is the change for step {turn}. I refactored the loop so the \
         accumulator cannot overflow, and threaded the error type through the \
         public signature.\n\n```rust\n{code}\n```\n\nThat keeps the hot path tight."
    )
}

async fn session_at(workspace: &Path) -> AgentSession {
    let model = Arc::new(MockModelClient::script(vec![ModelResponse {
        text: "ok".into(),
        tool_calls: vec![],
        usage: None,
        thinking: None,
    }]));
    AgentSession::create(
        LoopConfig {
            max_turns: 4,
            workspace: workspace.to_path_buf(),
            journal_dir: workspace.join("j"),
            ..Default::default()
        },
        model,
        ToolRegistry::new(),
    )
    .await
    .expect("session should be creatable in a temp workspace")
}

/// An app whose transcript holds `turns` user/assistant exchanges.
///
/// Each answer needs its own preceding user message: the transcript keeps one
/// durable answer per turn, so consecutive assistant messages collapse into the
/// last one and the earlier ones never render.
async fn app_with_turns(turns: usize) -> (TempDir, TuiApp) {
    let dir = TempDir::new().expect("temp dir");
    let mut session = session_at(dir.path()).await;
    for turn in 0..turns {
        session.messages.push(Message::new(
            MessageRole::User,
            format!("Please handle step {turn} of the migration."),
        ));
        session
            .messages
            .push(Message::new(MessageRole::Assistant, assistant_body(turn)));
    }
    // The splash banner is left as-is: it is only reachable from inside the
    // crate, and it contributes a constant to every frame, so it cancels out of
    // the growth comparisons these guards make.
    let app = TuiApp::new(
        session,
        TuiRuntimeConfig {
            model_label: "mock".into(),
            provider: "mock".into(),
            cwd: dir.path().to_path_buf(),
            version: "perf-guard".into(),
            startup_notices: Vec::new(),
            file_icons: FileIconMode::Unicode,
            theme_id: forge_config::DEFAULT_THEME_ID.to_string(),
        },
    );
    (dir, app)
}

/// Cost of one steady-state frame: the render key is unchanged between draws, so
/// this is the path taken while typing and while a response streams.
///
/// The caller must hold [`lock_measurement`].
fn steady_frame_cost(app: &mut TuiApp, iterations: usize) -> FrameCost {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test backend");
    // Warm: populate the conversation render cache so the measured draws are hits.
    terminal.draw(|frame| app.draw(frame)).expect("warm draw");

    let mut timings = Vec::with_capacity(iterations);
    ALLOCS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    COUNTING.store(1, Relaxed);
    for _ in 0..iterations {
        let started = Instant::now();
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        timings.push(started.elapsed().as_nanos());
    }
    COUNTING.store(0, Relaxed);

    timings.sort_unstable();
    FrameCost {
        allocs: ALLOCS.load(Relaxed) / iterations,
        bytes: BYTES.load(Relaxed) / iterations,
        p50_micros: timings[timings.len() / 2] as f64 / 1_000.0,
    }
}

/// Cost of the *first* frame an app ever draws, which must build the whole
/// transcript because the conversation cache is empty.
///
/// This is the path taken whenever the render key changes — a busy-phase flip,
/// a new banner, an activity summary, a resize, a theme change — so it is not a
/// startup-only cost. Unlike a steady-state frame it is expected to scale with
/// history; what must not regress is how much it costs *per message*.
///
/// The caller must hold [`lock_measurement`].
fn cold_frame_allocs(app: &mut TuiApp) -> usize {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test backend");
    ALLOCS.store(0, Relaxed);
    COUNTING.store(1, Relaxed);
    terminal.draw(|frame| app.draw(frame)).expect("cold draw");
    COUNTING.store(0, Relaxed);
    ALLOCS.load(Relaxed)
}

// --------------------------------------------------------------------- guards
//
// Bounds are set well clear of measured values so ordinary drift does not fail
// CI, while the regressions they exist to catch are orders of magnitude away.
// Reference measurements are recorded beside each bound.

/// Allocations per steady-state frame must not grow with transcript length.
///
/// This is the core invariant. When the transcript's rendered lines were
/// deep-copied on every frame, this delta was about 53 allocations per message —
/// roughly 16,000 additional allocations at 150 turns. Sharing the cached lines
/// instead of copying them made frame cost independent of history.
#[test]
fn frame_allocations_do_not_scale_with_transcript_length() {
    let _guard = lock_measurement();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let (_empty_dir, mut empty) = runtime.block_on(app_with_turns(0));
    let (_long_dir, mut long) = runtime.block_on(app_with_turns(150));

    let empty_cost = steady_frame_cost(&mut empty, 20);
    let long_cost = steady_frame_cost(&mut long, 20);
    let growth = long_cost.allocs.saturating_sub(empty_cost.allocs);

    // Reference after the skills-count and windowed-tail work: growth stays
    // well under a thousand. Before the original share-not-copy fix this was
    // ~15,960.
    const MAX_GROWTH: usize = 1_000;
    assert!(
        growth < MAX_GROWTH,
        "frame allocations must not scale with transcript length: \
         {} allocs empty vs {} allocs at 150 turns (growth {growth}, limit {MAX_GROWTH}). \
         A frame should cost the viewport, not the history.",
        empty_cost.allocs,
        long_cost.allocs
    );
}

/// Bytes allocated per steady-state frame must not grow with transcript length.
///
/// The allocation count and the byte volume can regress independently — copying
/// fewer, larger buffers would keep the count flat while the traffic grew.
#[test]
fn frame_allocated_bytes_do_not_scale_with_transcript_length() {
    let _guard = lock_measurement();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let (_empty_dir, mut empty) = runtime.block_on(app_with_turns(0));
    let (_long_dir, mut long) = runtime.block_on(app_with_turns(150));

    let empty_cost = steady_frame_cost(&mut empty, 20);
    let long_cost = steady_frame_cost(&mut long, 20);
    let growth_kib = long_cost.bytes.saturating_sub(empty_cost.bytes) / 1024;

    // Reference: ~35KiB empty, ~40KiB at 150 turns => growth ~5KiB.
    // Before the fix: ~183KiB -> ~940KiB => growth ~757KiB.
    const MAX_GROWTH_KIB: usize = 100;
    assert!(
        growth_kib < MAX_GROWTH_KIB,
        "frame allocation volume must not scale with transcript length: \
         {}KiB empty vs {}KiB at 150 turns (growth {growth_kib}KiB, limit {MAX_GROWTH_KIB}KiB).",
        empty_cost.bytes / 1024,
        long_cost.bytes / 1024
    );
}

/// An absolute ceiling on one steady-state frame at a long transcript.
///
/// The scaling guards above compare two points and would both pass if every
/// frame became uniformly expensive. This catches that, and also catches new
/// per-frame work that does not depend on history — a subprocess spawn, a fresh
/// clone of the composer, a rebuilt sidebar.
#[test]
fn frame_allocation_budget_holds_at_a_long_transcript() {
    let _guard = lock_measurement();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let (_dir, mut app) = runtime.block_on(app_with_turns(150));
    let cost = steady_frame_cost(&mut app, 20);

    // SLO: a steady-state frame at 150 turns stays under 600 allocations
    // (was 1,500; ~467 measured before the skills-count fix, ~380 after).
    // Kept close to the measured value so a new per-frame clone cannot hide.
    const BUDGET: usize = 600;
    assert!(
        cost.allocs < BUDGET,
        "a steady-state frame at 150 turns allocated {} times (budget {BUDGET}). \
         Something new is happening per frame.",
        cost.allocs
    );
}

/// The marginal cost of one more message on a cache-*miss* frame is bounded.
///
/// The guards above only measure cache hits, where cost is correctly flat. They
/// would all pass if rebuilding the transcript became arbitrarily expensive,
/// which is what users feel as a stall the moment anything invalidates the
/// cache mid-session — and the longer the session, the worse it gets.
///
/// Measuring the marginal cost between two lengths, rather than the total,
/// keeps the fixed per-frame chrome out of the number.
#[test]
fn cache_miss_cost_per_message_is_bounded() {
    let _guard = lock_measurement();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    // One throwaway app first: the very first draw in the process also pays
    // one-time lazy initialisation (theme registry, syntax sets) that would
    // otherwise be charged to whichever length happened to run first.
    let (_warm_dir, mut warm) = runtime.block_on(app_with_turns(4));
    cold_frame_allocs(&mut warm);

    let (_short_dir, mut short) = runtime.block_on(app_with_turns(40));
    let (_long_dir, mut long) = runtime.block_on(app_with_turns(150));
    let short_allocs = cold_frame_allocs(&mut short);
    let long_allocs = cold_frame_allocs(&mut long);

    let extra_messages = (150 - 40) * 2;
    let per_message = long_allocs.saturating_sub(short_allocs) / extra_messages;

    // Follow-mode cache misses only materialize a viewport-sized tail, so the
    // marginal cost of extra history is the projection of new messages, not a
    // full wrap of every line. SLO target is 80; 300 was the pre-window budget.
    const MAX_PER_MESSAGE: usize = 80;
    assert!(
        per_message < MAX_PER_MESSAGE,
        "rebuilding the transcript cost {per_message} allocations per message \
         ({short_allocs} allocs at 40 turns vs {long_allocs} at 150, limit \
         {MAX_PER_MESSAGE}). Every render-key change pays this, so it is what a \
         long session feels when the cache misses."
    );
}

// --------------------------------------------------------------------- report
//
// Not a check: a tool for humans, so it is ignored by default.

/// Print frame cost against transcript length.
///
/// All timings are microseconds. Treat them as corroboration only — see the
/// module docs on why they are never asserted on.
#[test]
#[ignore = "measurement tool, not a check: run with --ignored --nocapture"]
fn perf_report_frame_cost_by_transcript_length() {
    let _guard = lock_measurement();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    println!("\nsteady-state frame cost by transcript length (120x40)");
    println!("timings are MICROSECONDS (p50 of 20 draws); allocs and KiB are per frame\n");
    println!(
        "{:>6} {:>6} | {:>10} {:>10} {:>8} | {:>12}",
        "turns", "msgs", "p50 us", "allocs", "KiB", "cold allocs"
    );
    println!("{}", "-".repeat(64));

    // Absorb one-time lazy initialisation before the first measured cold draw.
    let (_warm_dir, mut warm) = runtime.block_on(app_with_turns(4));
    cold_frame_allocs(&mut warm);

    for turns in [0usize, 10, 40, 80, 150] {
        let (_dir, mut app) = runtime.block_on(app_with_turns(turns));
        let cost = steady_frame_cost(&mut app, 20);
        let (_cold_dir, mut cold) = runtime.block_on(app_with_turns(turns));
        let cold_allocs = cold_frame_allocs(&mut cold);
        println!(
            "{:>6} {:>6} | {:>10.1} {:>10} {:>8.1} | {:>12}",
            turns,
            turns * 2,
            cost.p50_micros,
            cost.allocs,
            cost.bytes as f64 / 1024.0,
            cold_allocs
        );
    }

    println!(
        "\nFlat allocations across rows is the invariant the guards in this file \
         protect: a frame should cost the viewport, not the history."
    );
    println!(
        "Wall-clock SLOs (session create < 50ms, cold frame @ 150 < 8ms, \
         forge --version RSS < 10MiB) are measured here, not asserted — \
         CI runners are too noisy. Allocation budgets above are the gates."
    );
}

/// One assistant answer's worth of streamed markdown, in chunks.
///
/// Mixed prose and a fenced code block, because the fence is what makes the
/// rebuild expensive: it is re-highlighted from scratch every time the preview
/// is rebuilt.
fn stream_chunks(count: usize) -> Vec<String> {
    let mut chunks = Vec::with_capacity(count);
    for i in 0..count {
        if i % 7 == 3 {
            chunks.push(format!(
                "\n```rust\nfn step_{i}(items: &[Item]) -> usize {{\n    items.iter().filter(|i| i.ready).count()\n}}\n```\n"
            ));
        } else if i % 4 == 3 {
            // End the paragraph. Real answers have block structure; without it
            // nothing ever settles and the cache has nothing to hold.
            chunks.push(format!("Sentence {i} closes the paragraph.\n\n"));
        } else {
            chunks.push(format!(
                "This is sentence {i} of the streamed answer, long enough to wrap in a sidebar pane. "
            ));
        }
    }
    chunks
}

/// Total draw cost of streaming one answer, as a function of its length.
///
/// The preview rebuild re-parses everything received so far, so the cost of a
/// whole turn is the sum of a growing series rather than a constant per chunk.
/// Doubling the answer length should roughly double the work; if it quadruples,
/// the growth is quadratic.
///
/// Reported, never asserted: the point is the shape, and the shape is what the
/// stable/unstable split would change.
#[test]
#[ignore = "measurement tool, not a gate"]
fn perf_report_stream_preview_cost_by_answer_length() {
    let _guard = lock_measurement();
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    println!(
        "\n{:>7}  {:>14}  {:>14}  {:>12}  {:>10}",
        "chunks", "total allocs", "total KiB", "p50 us/draw", "alloc/chunk"
    );
    let mut previous: Option<(usize, usize)> = None;
    for chunks in [16usize, 32, 64, 128, 256] {
        let (_dir, mut app) = rt.block_on(app_with_turns(4));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test backend");
        terminal.draw(|frame| app.draw(frame)).expect("warm draw");

        let pieces = stream_chunks(chunks);
        let mut timings = Vec::with_capacity(chunks);
        ALLOCS.store(0, Relaxed);
        BYTES.store(0, Relaxed);
        COUNTING.store(1, Relaxed);
        for piece in &pieces {
            app.stream_preview_for_tests(piece);
            let started = Instant::now();
            terminal.draw(|frame| app.draw(frame)).expect("draw");
            timings.push(started.elapsed().as_nanos());
        }
        COUNTING.store(0, Relaxed);
        timings.sort_unstable();

        let allocs = ALLOCS.load(Relaxed);
        let bytes = BYTES.load(Relaxed);
        let growth = previous
            .map(|(pc, pa)| {
                format!(
                    "  x{:.2} for x{:.0}",
                    allocs as f64 / pa as f64,
                    chunks as f64 / pc as f64
                )
            })
            .unwrap_or_default();
        println!(
            "{chunks:>7}  {allocs:>14}  {:>14}  {:>12.1}  {:>10}{growth}",
            bytes / 1024,
            timings[timings.len() / 2] as f64 / 1_000.0,
            allocs / chunks,
        );
        previous = Some((chunks, allocs));
    }
    println!();
}

/// Streaming one answer must cost roughly its length, not its length squared.
///
/// Before the settled-prefix cache, doubling the answer multiplied total
/// allocations by ~3.7 — converging on 4x, the signature of quadratic growth.
/// The guard is set at 3.0 so it fails on a return to that shape while leaving
/// room for the constant overhead that dominates the shorter samples.
#[test]
fn streaming_cost_does_not_grow_quadratically() {
    let _guard = lock_measurement();
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    let measure = |chunks: usize| -> usize {
        let (_dir, mut app) = rt.block_on(app_with_turns(4));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test backend");
        terminal.draw(|frame| app.draw(frame)).expect("warm draw");
        ALLOCS.store(0, Relaxed);
        COUNTING.store(1, Relaxed);
        for piece in &stream_chunks(chunks) {
            app.stream_preview_for_tests(piece);
            terminal.draw(|frame| app.draw(frame)).expect("draw");
        }
        COUNTING.store(0, Relaxed);
        ALLOCS.load(Relaxed)
    };

    let short = measure(64);
    let long = measure(128);
    let growth = long as f64 / short as f64;

    assert!(
        growth < 3.0,
        "doubling the answer multiplied allocations by {growth:.2} \
         ({short} -> {long}). Above ~3 the streaming preview is re-reading \
         work it has already done; see docs/streaming-preview-commit-boundary.md"
    );
}

/// Visual check: dump the streamed pane at two widths, mid-fence.
///
/// The cache is the only thing between a settled prefix and the screen, so a
/// human should be able to read the frame it produces.
#[test]
#[ignore = "visual check, not a gate"]
fn perf_report_streamed_frame_dump() {
    let _guard = lock_measurement();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let (_dir, mut app) = rt.block_on(app_with_turns(1));

    for piece in [
        "Here is the plan.\n\n",
        "- discover the boundary\n- cache the prefix\n\n",
        "Now the code:\n\n```rust\n",
        "fn apply(x: usize) -> usize {\n    x + 1\n",
    ] {
        app.stream_preview_for_tests(piece);
    }

    for (w, h) in [(120u16, 24u16), (90, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("backend");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        println!("\n===== {w}x{h} =====");
        let buf = terminal.backend().buffer();
        for y in 0..h {
            let row: String = (0..w).map(|x| buf[(x, y)].symbol()).collect();
            println!("{}", row.trim_end());
        }
    }
}
