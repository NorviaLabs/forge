//! Process-wide git preferences set from the user config at startup.
//!
//! Only the smart-commit flag lives here. It is read once when the app is
//! built and again when the operator picks "Always" in the no-staged-changes
//! prompt, which also persists it so the next launch starts with it on.

use std::cell::Cell;

thread_local! {
    static SMART_COMMIT: Cell<bool> = const { Cell::new(false) };
}

/// Install the configured preference (call once at startup).
pub fn install(smart_commit: bool) {
    SMART_COMMIT.with(|slot| slot.set(smart_commit));
}

/// Whether committing with nothing staged should stage everything and commit
/// without asking. Off by default: the first commit with nothing staged
/// prompts, and only "Always" turns this on.
pub fn smart_commit() -> bool {
    SMART_COMMIT.with(Cell::get)
}
