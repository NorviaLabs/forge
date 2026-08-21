//! One grammar for the key hints that sit on modals and prompts.
//!
//! Every surface used to invent its own. Across five of them Forge shipped
//! `↑↓  Enter confirm  Esc quit`, `commands 1–8/31 · Tab · ↑↓`, `Theme · ↑↓
//! preview · Enter confirm · Esc cancel`, `↑↓  Select    Enter  Confirm    Esc
//! Close` and `↑↓ Enter Esc don't run` — three separators, two capitalisations,
//! and single, double and quadruple spaces. None of it is wrong on its own;
//! together it is what makes the app read as assembled rather than designed.
//!
//! The grammar: `key verb` pairs, keys at bold weight, verbs in sentence case,
//! joined by ` · `.

use crate::theme;
use ratatui::style::Modifier;
use ratatui::text::Span;

/// A key and what it does.
pub type Hint = (&'static str, &'static str);

/// Separator between pairs.
const SEP: &str = " · ";

/// Render pairs as plain text, for a block title.
pub fn hint_text(pairs: &[Hint]) -> String {
    pairs
        .iter()
        .map(|(key, verb)| format!("{key} {verb}"))
        .collect::<Vec<_>>()
        .join(SEP)
}

/// Render pairs as styled spans, keys at bold weight.
///
/// Degrades within `budget` columns: first the verbs are dropped, leaving the
/// bare keys, then trailing pairs are dropped from the right. Never wraps — a
/// hint that reflows onto a second row breaks its container's height budget.
pub fn hint_spans(pairs: &[Hint], budget: usize) -> Vec<Span<'static>> {
    fn build(pairs: &[Hint], verbs: bool) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (key, verb) in pairs {
            if !spans.is_empty() {
                spans.push(Span::styled(
                    if verbs {
                        SEP.to_string()
                    } else {
                        " ".to_string()
                    },
                    theme::metadata_style(),
                ));
            }
            spans.push(Span::styled(
                (*key).to_string(),
                theme::metadata_style().add_modifier(Modifier::BOLD),
            ));
            if verbs {
                spans.push(Span::raw(" "));
                spans.push(Span::styled((*verb).to_string(), theme::metadata_style()));
            }
        }
        spans
    }
    let width = |spans: &[Span<'static>]| spans.iter().map(Span::width).sum::<usize>();

    let full = build(pairs, true);
    if width(&full) <= budget {
        return full;
    }
    let keys_only = build(pairs, false);
    if width(&keys_only) <= budget {
        return keys_only;
    }
    for take in (1..pairs.len()).rev() {
        let trimmed = build(&pairs[..take], false);
        if width(&trimmed) <= budget {
            return trimmed;
        }
    }
    Vec::new()
}

/// Move, choose, leave — the shape almost every list-shaped surface needs.
pub const MOVE_SELECT_CLOSE: &[Hint] = &[("↑↓", "move"), ("Enter", "select"), ("Esc", "close")];
pub const APPROVAL: &[Hint] = &[("↑↓", "move"), ("Enter", "confirm"), ("Esc", "don't run")];
pub const QUESTION: &[Hint] = &[("↑↓", "move"), ("Enter", "answer"), ("Esc", "skip")];
pub const QUESTION_MULTI: &[Hint] = &[
    ("↑↓", "move"),
    ("Space", "toggle"),
    ("Enter", "answer"),
    ("Esc", "skip"),
];
pub const QUESTION_TABS: &[Hint] = &[
    ("←→", "questions"),
    ("↑↓", "move"),
    ("Enter", "answer"),
    ("Esc", "skip"),
];
pub const THEME: &[Hint] = &[("↑↓", "preview"), ("Enter", "apply"), ("Esc", "cancel")];
pub const TRUST: &[Hint] = &[("↑↓", "move"), ("Enter", "trust"), ("Esc", "quit")];
pub const COMMANDS: &[Hint] = &[("↑↓", "move"), ("Tab", "complete"), ("Enter", "run")];
pub const BROWSE: &[Hint] = &[
    ("↑↓", "move"),
    ("Enter", "open"),
    ("←", "up"),
    ("Esc", "close"),
];
pub const SCROLL_BACK_CLOSE: &[Hint] = &[("↑↓", "scroll"), ("←", "back"), ("Esc", "close")];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_uses_one_separator_and_sentence_case() {
        assert_eq!(
            hint_text(MOVE_SELECT_CLOSE),
            "↑↓ move · Enter select · Esc close"
        );
    }

    /// The point of the module: no surface may drift into its own grammar.
    #[test]
    fn every_shipped_hint_shares_the_grammar() {
        for pairs in [
            MOVE_SELECT_CLOSE,
            APPROVAL,
            QUESTION,
            QUESTION_MULTI,
            QUESTION_TABS,
            THEME,
            TRUST,
            COMMANDS,
            BROWSE,
            SCROLL_BACK_CLOSE,
        ] {
            for (key, verb) in pairs {
                assert!(!key.is_empty() && !verb.is_empty(), "{key:?}/{verb:?}");
                assert!(
                    verb.chars().next().is_some_and(|c| !c.is_uppercase()),
                    "verb {verb:?} must be sentence case"
                );
                assert!(!verb.contains("  "), "verb {verb:?} has doubled spaces");
            }
        }
    }

    #[test]
    fn spans_drop_verbs_before_pairs_when_short_of_room() {
        let full = hint_spans(MOVE_SELECT_CLOSE, 100);
        let text: String = full.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("select"), "{text}");

        let squeezed = hint_spans(MOVE_SELECT_CLOSE, 14);
        let text: String = squeezed.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("select"), "{text}");
        assert!(text.contains("Esc"), "{text}");
        assert!(text.chars().count() <= 14, "{text}");
    }

    #[test]
    fn spans_never_exceed_their_budget() {
        for budget in 0..60 {
            let spans = hint_spans(QUESTION_TABS, budget);
            let width: usize = spans.iter().map(Span::width).sum();
            assert!(width <= budget, "budget {budget} produced {width}");
        }
    }
}
