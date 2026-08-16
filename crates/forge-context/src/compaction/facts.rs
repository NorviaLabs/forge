//! Protected facts — the handful of things in a conversation that cannot be
//! rediscovered from the repository.
//!
//! Forge can re-read `src/parser.rs` at any time; it cannot re-derive "do not
//! change the public API" from anything but the conversation. Those
//! statements are collected from canonical history, supplied to the
//! compaction prompt as must-keep material, and checked against the resulting
//! checkpoint before it is installed.
//!
//! Extraction is deliberately shallow (§13): user-authored text is the source,
//! and no classifier decides what a user "really meant".

use serde::{Deserialize, Serialize};

/// Longest a single fact is carried at. Long pasted material (logs, files)
/// is context, not a constraint, and would swamp the compaction prompt.
const MAX_FACT_CHARS: usize = 600;

/// Most recent facts supplied to a compaction prompt. Older user turns are
/// already represented by the previous checkpoint.
pub const MAX_SUPPLIED_FACTS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedFactKind {
    UserConstraint,
    UserCorrection,
    ExplicitDecision,
    IrreversibleAction,
}

impl ProtectedFactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserConstraint => "user constraint",
            Self::UserCorrection => "user correction",
            Self::ExplicitDecision => "explicit decision",
            Self::IrreversibleAction => "irreversible action",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedFact {
    pub kind: ProtectedFactKind,
    pub text: String,
    /// Which user turn this came from: the zero-based ordinal of the message
    /// in canonical session history. Forge has no per-message id type, and it
    /// does not need one here — canonical history is append-only, so a turn's
    /// ordinal never changes, and it survives compaction (which only rewrites
    /// the projection) and journal replay alike.
    pub source_message_index: usize,
}

/// Phrases that mark a user turn as a correction rather than a fresh request.
const CORRECTION_MARKERS: &[&str] = &[
    "no,", "not ", "don't", "do not", "never", "stop ", "revert", "undo", "instead", "actually",
    "wrong", "mistake", "avoid",
];

/// The protected fact carried by one user turn, if it carries one.
///
/// Every non-blank user turn contributes: a user turn is, by construction, the
/// only place an explicit instruction can come from. Kind classification is
/// the one piece of interpretation applied, and it only affects how the fact
/// is labelled in the compaction prompt.
pub fn protected_fact(turn_index: usize, text: &str) -> Option<ProtectedFact> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let lowered = text.to_ascii_lowercase();
    let kind = if CORRECTION_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        ProtectedFactKind::UserCorrection
    } else {
        ProtectedFactKind::UserConstraint
    };
    Some(ProtectedFact {
        kind,
        text: truncate_chars(text, MAX_FACT_CHARS),
        source_message_index: turn_index,
    })
}

/// Rebuild the protected-fact set from canonical user turns, oldest → newest.
/// This is what session resume replays: the journal keeps every user message
/// regardless of how many times the projection has been compacted.
pub fn collect_protected_facts<S: AsRef<str>>(user_turns: &[S]) -> Vec<ProtectedFact> {
    user_turns
        .iter()
        .enumerate()
        .filter_map(|(index, text)| protected_fact(index, text.as_ref()))
        .collect()
}

/// The most recent `MAX_SUPPLIED_FACTS` facts, oldest → newest.
pub fn recent_facts(facts: &[ProtectedFact]) -> &[ProtectedFact] {
    let start = facts.len().saturating_sub(MAX_SUPPLIED_FACTS);
    &facts[start..]
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_non_blank_user_turn_becomes_a_fact_keyed_by_its_turn_ordinal() {
        let turns = ["Do not change the public API.", "   ", "Also add tests."];
        let facts = collect_protected_facts(&turns);
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].source_message_index, 0);
        assert_eq!(facts[1].source_message_index, 2);
        assert_eq!(facts[1].text, "Also add tests.");
    }

    #[test]
    fn classifies_a_corrective_turn_as_a_correction() {
        let facts =
            collect_protected_facts(&["Add a cache layer.", "No, don't use a background thread."]);
        assert_eq!(facts[0].kind, ProtectedFactKind::UserConstraint);
        assert_eq!(facts[1].kind, ProtectedFactKind::UserCorrection);
    }

    #[test]
    fn pasted_material_is_truncated_rather_than_swamping_the_prompt() {
        let fact = protected_fact(0, &"y".repeat(MAX_FACT_CHARS + 50)).unwrap();
        assert_eq!(fact.text.chars().count(), MAX_FACT_CHARS + 1);
        assert!(fact.text.ends_with('\u{2026}'));
    }

    #[test]
    fn a_blank_turn_carries_no_fact() {
        assert!(protected_fact(0, "  \n ").is_none());
    }

    #[test]
    fn recent_facts_keeps_the_newest_window() {
        let turns: Vec<String> = (0..MAX_SUPPLIED_FACTS + 5)
            .map(|i| format!("request {i}"))
            .collect();
        let facts = collect_protected_facts(&turns);
        let recent = recent_facts(&facts);
        assert_eq!(recent.len(), MAX_SUPPLIED_FACTS);
        assert_eq!(
            recent.last().unwrap().text,
            format!("request {}", MAX_SUPPLIED_FACTS + 4)
        );
    }
}
