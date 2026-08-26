//! Context compaction — a controlled transition between cache epochs.
//!
//! Forge's model-visible context is append-only during normal operation, so
//! the provider's cached prefix keeps growing and every turn reuses it.
//! Compaction is the one operation allowed to replace historical
//! model-visible context: when pressure gets high, the conversation is
//! collapsed into a structured state checkpoint plus a recent raw tail, and
//! append-only behaviour resumes on top of that new prefix.
//!
//! ```text
//! EPOCH 0                        EPOCH 1
//! stable prefix                  stable prefix
//! + conversation      COMPACT    + checkpoint
//! + turn         ───────────►    + recent raw tail
//! + turn                         + turn …
//! ```
//!
//! Canonical session history is never touched by anything in this module: it
//! lives in the durable journal, and compaction only ever proposes a new
//! *projection* of it. Everything here is pure — no I/O, no provider
//! knowledge — so the transactional install lives in `forge-core`, which owns
//! the journal and the model client.

mod checkpoint;
mod engine;
mod facts;
mod policy;
mod prompt;
mod tail;

pub use checkpoint::{
    Checkpoint, CheckpointError, CHECKPOINT_ROOT, CHECKPOINT_SECTIONS, CHECKPOINT_VERSION,
    REQUIRED_SECTIONS,
};
pub use engine::{
    is_checkpoint_message, plan_compaction, CompactionError, CompactionPlan, CompactionRecord,
    CompactionTelemetry, SessionContextState,
};
pub use facts::{
    collect_protected_facts, protected_fact, recent_facts, ProtectedFact, ProtectedFactKind,
    MAX_SUPPLIED_FACTS,
};
pub use policy::{
    CompactionPolicy, CompactionTrigger, DEFAULT_EXPECTED_TURN_TOKENS, DEFAULT_OUTPUT_RESERVE,
    DEFAULT_SAFETY_RESERVE, POST_COMPACTION_TARGET, TAIL_FRACTION, TAIL_MAX_TOKENS,
    TAIL_MIN_TOKENS, TOOL_SCHEMA_DEFERRAL_FRACTION, TRIGGER_UTILIZATION,
};
pub use prompt::{checkpoint_message, compaction_instruction, compaction_message};
pub use tail::{
    message_tokens, messages_tokens, select_tail, tool_calls_are_paired, tool_results_are_complete,
    TailSelection,
};
