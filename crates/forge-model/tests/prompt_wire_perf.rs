//! Allocation-cost guards for the prompt-snapshot diagnostic path.
//!
//! # Why this exists
//!
//! `record_prompt_snapshot` runs once per model step and exists only to feed
//! `tracing::debug!` prefix diagnostics plus a sha/byte-count in the journal.
//! It is adjacent to a multi-second network call, so it is not a user-visible
//! bottleneck — but it used to do four full passes over the whole prompt
//! (build the wire, deep-clone it out of the provider body, deep-clone it
//! again to strip `cache_control`, then encode and hash). These guards pin the
//! pass count down so it cannot silently grow back.
//!
//! # Why allocation counts, not timings
//!
//! Same rationale as `forge-tui/tests/render_perf.rs`: wall-clock on this
//! hardware varies by more than 2x run to run, while allocation counts are
//! deterministic. Guards assert on counts; timings are never asserted.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

use forge_model::{ModelRequest, PromptTransport, SharedMessages};
use forge_types::{Message, MessageRole, ToolCall, ToolDescriptor};
use serde_json::json;

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

#[derive(Debug, Clone, Copy)]
struct Cost {
    allocs: usize,
    bytes: usize,
}

fn measure<T>(mut body: impl FnMut() -> T) -> Cost {
    // Warm up so first-touch lazies are not billed to the measured run.
    let warm = body();
    ALLOCS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    COUNTING.store(1, Relaxed);
    let held = body();
    COUNTING.store(0, Relaxed);
    let cost = Cost {
        allocs: ALLOCS.load(Relaxed),
        bytes: BYTES.load(Relaxed),
    };
    drop(held);
    drop(warm);
    cost
}

// ------------------------------------------------------------------- fixtures

fn tool(name: &str) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        description: "Reads a file from the workspace and returns its contents.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "workspace-relative path"},
                "limit": {"type": "integer"}
            },
            "required": ["path"]
        }),
        side_effect_class: forge_types::SideEffectClass::Read,
        idempotent: true,
    }
}

/// A transcript shaped like a real agent session: user prose, an assistant turn
/// carrying a tool call, and the tool result.
fn transcript(messages: usize) -> Vec<Message> {
    let mut out = Vec::with_capacity(messages);
    for index in 0..messages {
        match index % 3 {
            0 => out.push(Message::new(
                MessageRole::User,
                format!("Please handle step {index} of the migration and report back."),
            )),
            1 => {
                let mut message = Message::new(
                    MessageRole::Assistant,
                    format!("Reading the file for step {index} before editing."),
                );
                message.tool_calls = vec![ToolCall {
                    id: format!("call_{index}"),
                    name: "read_file".into(),
                    arguments: json!({"path": format!("src/module_{index}.rs")}),
                }];
                out.push(message);
            }
            _ => {
                let mut message = Message::new(
                    MessageRole::Tool,
                    format!("pub fn step_{index}() -> Result<(), Error> {{ Ok(()) }}"),
                );
                message.tool_call_id = Some(format!("call_{}", index - 1));
                message.name = Some("read_file".into());
                out.push(message);
            }
        }
    }
    out
}

fn request(messages: usize) -> ModelRequest {
    ModelRequest {
        messages: SharedMessages::from(transcript(messages)),
        tools: vec![tool("read_file"), tool("apply_patch"), tool("shell")],
        model: "test-model".into(),
        workspace_root: std::path::PathBuf::from("/tmp/workspace"),
        route_id: None,
        reasoning_effort: Some("high".into()),
        prompt_cache: true,
    }
}

/// The whole diagnostic step as `record_prompt_snapshot` performs it: build the
/// wire object, then snapshot (strip + encode + hash) it.
fn snapshot_cost(messages: usize) -> Cost {
    let request = request(messages);
    measure(|| {
        let wire = forge_model::prompt_wire(&request, PromptTransport::OpenaiCompat);
        forge_model::snapshot_prompt(&wire)
    })
}

/// Snapshotting alone, with the wire already built — this is the part the
/// four-pass structure was paying twice.
fn snapshot_only_cost(messages: usize) -> Cost {
    let wire = forge_model::prompt_wire(&request(messages), PromptTransport::OpenaiCompat);
    measure(|| forge_model::snapshot_prompt(&wire))
}

// --------------------------------------------------------------------- guards

/// Snapshotting an already-built wire allocates a *constant* number of times,
/// not one-per-message.
///
/// The old strip-into-a-copy-then-encode structure cost ~12.6 allocations per
/// message (3807 at 300 messages). Serialising straight off the source value
/// leaves only the output buffer's doubling growth and the hex digest: 15 at
/// 300 messages, 16 at 600. Asserting an absolute ceiling is what makes this a
/// clone guard — any per-message allocation at all blows straight through it.
#[test]
fn snapshot_of_a_built_wire_allocates_a_constant_number_of_times() {
    let _guard = lock_measurement();
    let cost = snapshot_only_cost(300);
    assert!(
        cost.allocs < 40,
        "snapshot_prompt allocated {} times for 300 messages ({} bytes); budget is 40 — \
         it should be constant in transcript length, so a deep clone has come back",
        cost.allocs,
        cost.bytes
    );
}

/// The end-to-end diagnostic (build the wire + snapshot it) stays linear with a
/// small constant. Guards the whole `record_prompt_snapshot` inner cost; what
/// remains is the wire build itself, which is inherent.
#[test]
fn prompt_snapshot_cost_per_message_is_bounded() {
    let _guard = lock_measurement();
    let messages = 300;
    let cost = snapshot_cost(messages);
    let per_message = cost.allocs as f64 / messages as f64;
    assert!(
        per_message < 34.0,
        "build+snapshot cost {per_message:.2} allocs/message ({} total, {} bytes); budget is 34",
        cost.allocs,
        cost.bytes
    );
}

/// Cost must grow with the transcript, not faster than it.
#[test]
fn prompt_snapshot_cost_scales_linearly() {
    let _guard = lock_measurement();
    let small = snapshot_cost(100);
    let large = snapshot_cost(400);
    let ratio = large.allocs as f64 / small.allocs.max(1) as f64;
    assert!(
        ratio < 5.0,
        "4x the transcript cost {ratio:.2}x the allocations ({} -> {})",
        small.allocs,
        large.allocs
    );
}

/// Measurement table. Not a check — run it by hand:
/// `cargo test -p forge-model --test prompt_wire_perf -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_report_prompt_snapshot_cost_by_transcript_length() {
    let _guard = lock_measurement();
    println!("\nmsgs | build+snapshot allocs / KiB | snapshot-only allocs / KiB");
    for messages in [1, 81, 301, 601] {
        let full = snapshot_cost(messages);
        let only = snapshot_only_cost(messages);
        println!(
            "{messages:>4} | {:>8} / {:>7.1} | {:>8} / {:>7.1}",
            full.allocs,
            full.bytes as f64 / 1024.0,
            only.allocs,
            only.bytes as f64 / 1024.0
        );
    }
}
