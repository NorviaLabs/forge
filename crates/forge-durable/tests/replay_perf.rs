//! Allocation-cost guards for journal replay.
//!
//! # Why this exists
//!
//! `Journal::replay` runs at session resume, and its cost is proportional to
//! the *whole history* of the session — not to a viewport or a window. A long
//! session therefore pays for every redundant pass over every payload it ever
//! wrote, on every resume.
//!
//! The loop used to deep-clone each row's payload once to build the
//! `JournalEvent` it always pushes, and then a second time for every arm that
//! deserialises a typed payload (`ModelResponse`, `ToolResultPayload`,
//! `ContextCompacted`'s message array). For a `ContextCompacted` event that is
//! two extra copies of the entire compacted conversation.
//!
//! # Why allocation counts, not timings
//!
//! Same rationale as `forge-tui/tests/render_perf.rs`: wall-clock on this
//! hardware varies by more than 2x run to run; allocation counts are
//! deterministic. Guards assert on counts only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

use forge_durable::{new_session_id, Journal};
use forge_types::{Message, MessageRole, ModelResponse, SessionId, ToolCall, ToolOutput};
use tempfile::TempDir;

// ----------------------------------------------------------- counting allocator

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicUsize = AtomicUsize::new(0);

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

/// The counters are process-global, so measurements must be serialised.
fn lock_measurement() -> MutexGuard<'static, ()> {
    static GUARD: Mutex<()> = Mutex::new(());
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runs an async measurement body under the serialising lock.
///
/// The lock is taken here, in a synchronous frame, rather than at the top of an
/// `#[tokio::test]`: a `std` guard held across an `await` is a clippy denial,
/// and an async-aware mutex would not help because the allocator counters are
/// global to the *process*, not to the task.
fn measured<T>(body: impl std::future::Future<Output = T>) -> T {
    let _guard = lock_measurement();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(body)
}

#[derive(Debug, Clone, Copy)]
struct Cost {
    allocs: usize,
    bytes: usize,
}

// ------------------------------------------------------------------- fixtures

const CODE: &str = "pub fn process(items: &[Item]) -> Result<Summary, Error> {\n    \
                    let mut total = 0usize;\n    for item in items { total += item.weight; }\n    \
                    Ok(Summary { total, count: items.len() })\n}";

/// A journal shaped like a real session: a user message, an assistant response
/// carrying a tool call, and the tool result, repeated `turns` times.
async fn journal_with_turns(dir: &TempDir, turns: usize) -> (Journal, SessionId) {
    let sid = new_session_id();
    let journal = Journal::open(dir.path(), sid).await.expect("open journal");
    journal
        .append_session_created(sid)
        .await
        .expect("session created");
    for turn in 0..turns {
        journal
            .append_user_message(sid, &format!("Please handle step {turn} of the migration."))
            .await
            .expect("user message");
        let call = ToolCall {
            id: format!("call_{turn}"),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": format!("src/module_{turn}.rs")}),
        };
        journal
            .append_model_response(
                sid,
                serde_json::to_value(ModelResponse {
                    text: format!("Reading the file for step {turn} before editing it in place."),
                    tool_calls: vec![call.clone()],
                    usage: None,
                    thinking: Some(format!("The caller wants step {turn}; read first.")),
                })
                .expect("response serialises"),
            )
            .await
            .expect("model response");
        journal
            .append_tool_intent(sid, &call)
            .await
            .expect("tool intent");
        journal
            .append_tool_result(
                sid,
                &call,
                &ToolOutput {
                    outcome: Default::default(),
                    content: CODE.replace("process", &format!("process_{turn}")),
                    is_error: false,
                    exit_code: None,
                    attachments: Vec::new(),
                },
            )
            .await
            .expect("tool result");
    }
    (journal, sid)
}

/// Appends a compaction event carrying a whole replacement conversation — the
/// newest replay arm, and the one that moves the most bytes per event.
async fn append_compaction(journal: &Journal, sid: SessionId, messages: usize) {
    let replacement: Vec<Message> = (0..messages)
        .map(|index| {
            Message::new(
                if index % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                format!("Compacted summary line {index} covering the earlier migration steps."),
            )
        })
        .collect();
    journal
        .append_context_compacted(
            sid,
            serde_json::json!({
                "messages": replacement,
                "context_state": {"tokens": 1234, "reason": "auto"},
            }),
        )
        .await
        .expect("compaction event");
}

async fn measure_replay(journal: &Journal, sid: SessionId) -> Cost {
    // Warm up: the pool, the prepared statement and any lazies must not be
    // billed to the measured run.
    let warm = journal.replay(sid).await.expect("warm replay");
    drop(warm);
    ALLOCS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    COUNTING.store(1, Relaxed);
    let state = journal.replay(sid).await.expect("measured replay");
    COUNTING.store(0, Relaxed);
    let cost = Cost {
        allocs: ALLOCS.load(Relaxed),
        bytes: BYTES.load(Relaxed),
    };
    drop(state);
    cost
}

// --------------------------------------------------------------------- guards

/// Replay cost per journal event must stay bounded. Each event legitimately
/// costs a JSON parse plus the typed value it projects into; what it must not
/// cost is extra whole-payload copies on top.
#[test]
fn replay_cost_per_event_is_bounded() {
    measured(async {
        let dir = TempDir::new().expect("temp dir");
        let turns = 120;
        let (journal, sid) = journal_with_turns(&dir, turns).await;
        let events = turns * 4 + 1;
        let cost = measure_replay(&journal, sid).await;
        let per_event = cost.allocs as f64 / events as f64;
        assert!(
            per_event < 26.0,
            "replay cost {per_event:.1} allocs/event ({} total over {events} events, {} bytes); \
             budget is 26 (was 38.5 before the payload move + borrowed deserialisation)",
            cost.allocs,
            cost.bytes
        );
    });
}

/// Replay must be linear in journal length, not superlinear.
#[test]
fn replay_cost_scales_linearly() {
    measured(async {
        let small_dir = TempDir::new().expect("temp dir");
        let large_dir = TempDir::new().expect("temp dir");
        let (small_journal, small_sid) = journal_with_turns(&small_dir, 40).await;
        let (large_journal, large_sid) = journal_with_turns(&large_dir, 160).await;
        let small = measure_replay(&small_journal, small_sid).await;
        let large = measure_replay(&large_journal, large_sid).await;
        let ratio = large.allocs as f64 / small.allocs.max(1) as f64;
        assert!(
            ratio < 5.0,
            "4x the journal cost {ratio:.2}x the allocations ({} -> {})",
            small.allocs,
            large.allocs
        );
    });
}

/// A `ContextCompacted` event carries an entire replacement conversation. It
/// must be deserialised once, not copied first and deserialised after.
///
/// Measured as the marginal cost of adding one compaction event to an
/// otherwise identical journal, so the surrounding turns cancel out.
#[test]
fn compaction_replay_does_not_copy_the_conversation_twice() {
    measured(async {
        let plain_dir = TempDir::new().expect("temp dir");
        let compacted_dir = TempDir::new().expect("temp dir");
        let compacted_messages = 200;

        let (plain_journal, plain_sid) = journal_with_turns(&plain_dir, 20).await;
        let (compacted_journal, compacted_sid) = journal_with_turns(&compacted_dir, 20).await;
        append_compaction(&compacted_journal, compacted_sid, compacted_messages).await;

        let plain = measure_replay(&plain_journal, plain_sid).await;
        let compacted = measure_replay(&compacted_journal, compacted_sid).await;
        let marginal = compacted.allocs.saturating_sub(plain.allocs);
        let per_message = marginal as f64 / compacted_messages as f64;
        assert!(
            per_message < 12.0,
            "one compaction event carrying {compacted_messages} messages cost {marginal} extra \
             allocations ({per_message:.1}/message); budget is 12/message — the payload is being \
             copied before it is deserialised"
        );
    });
}

/// Measurement table. Not a check — run it by hand:
/// `cargo test -p forge-durable --test replay_perf -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_report_replay_cost_by_journal_length() {
    measured(async {
        println!("\nturns | events | allocs | KiB | allocs/event");
        for turns in [10, 40, 120, 240] {
            let dir = TempDir::new().expect("temp dir");
            let (journal, sid) = journal_with_turns(&dir, turns).await;
            let cost = measure_replay(&journal, sid).await;
            let events = turns * 4 + 1;
            println!(
                "{turns:>5} | {events:>6} | {:>6} | {:>7.1} | {:>6.1}",
                cost.allocs,
                cost.bytes as f64 / 1024.0,
                cost.allocs as f64 / events as f64
            );
        }
        println!("\ncompaction event carrying N messages (marginal over a 20-turn journal)");
        for messages in [50, 200, 800] {
            let plain_dir = TempDir::new().expect("temp dir");
            let compacted_dir = TempDir::new().expect("temp dir");
            let (plain_journal, plain_sid) = journal_with_turns(&plain_dir, 20).await;
            let (compacted_journal, compacted_sid) = journal_with_turns(&compacted_dir, 20).await;
            append_compaction(&compacted_journal, compacted_sid, messages).await;
            let plain = measure_replay(&plain_journal, plain_sid).await;
            let compacted = measure_replay(&compacted_journal, compacted_sid).await;
            let marginal = compacted.allocs.saturating_sub(plain.allocs);
            println!(
                "{messages:>5} messages | marginal allocs {marginal:>6} | {:>5.1}/message",
                marginal as f64 / messages as f64
            );
        }
    });
}
