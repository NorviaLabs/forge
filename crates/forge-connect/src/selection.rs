//! A single, structured carrier for "which model/route/effort is active" —
//! replaces the pattern of setting several independent fields (provider,
//! model, profile id, effort) together by hand at every call site that
//! changes the active selection, which is exactly the kind of drift that let
//! a picker-confirm bug silently discard a correctly-resolved catalog row.

/// A fully-resolved model/route/effort choice, threaded end-to-end from a
/// catalog row (picker confirm, default-model application, quick switch, or
/// restore-on-restart) to the caller that applies it as the active
/// selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    /// Stable offering identity, e.g. `openai-chatgpt`.
    pub route_id: String,
    /// Dispatch/route family, e.g. `"native"`.
    pub provider: String,
    /// Full canonical catalog id, e.g. `"openai-codex/gpt-5.6-luna"`. Never
    /// truncated or re-derived from a substring of user input.
    pub model: String,
    /// The connect profile that owns this route, when known.
    pub profile_id: Option<String>,
    /// Transport-level effort string (e.g. `"high"`), not a UI-layer enum —
    /// this crate has no dependency on the TUI's `ReasoningEffort` type, and
    /// this is exactly the string shape already persisted via
    /// `CredentialStore::set_last_effort`/`last_effort`.
    pub effort: String,
}
