//! `AgentSession`'s inherent methods, grouped by concern.
//!
//! The struct itself stays in `lib.rs`: its fields are private, and
//! private fields are visible to descendants of the module declaring
//! them. Every module in this crate descends from the crate root, so
//! leaving the struct there keeps the sibling modules (`stream`, `turn`,
//! `completion`, `background`, ...) compiling against it unchanged.
//! Moving it here would mean widening ~15 private fields to
//! `pub(crate)`.

mod approval;
pub(crate) mod compaction;
mod create;
mod inspect;
pub(crate) mod question;
mod tasks;
pub(crate) mod tools;
mod turn_ops;
