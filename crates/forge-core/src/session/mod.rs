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
mod create;
mod inspect;
mod tasks;
mod tools;
mod turn_ops;
