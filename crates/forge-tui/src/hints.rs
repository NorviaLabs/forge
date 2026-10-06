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
/// Model/provider picker. No `←→` pair: the picker opens directly on one
/// standalone column (providers, models, or effort) since the modal
/// restructure, so there is no section to switch between — advertising one
/// would fail the displayed-hints-match-routing invariant.
pub const PICKER: &[Hint] = &[("↑↓", "move"), ("Enter", "select"), ("Esc", "close")];
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
/// Multi-select with more than one question: both the tab keys and `Space`
/// are live, so both must be shown (the displayed-binding invariant).
pub const QUESTION_TABS_MULTI: &[Hint] = &[
    ("←→", "questions"),
    ("↑↓", "move"),
    ("Space", "toggle"),
    ("Enter", "answer"),
    ("Esc", "skip"),
];
pub const THEME: &[Hint] = &[("↑↓", "preview"), ("Enter", "apply"), ("Esc", "cancel")];
/// First-run trust prompt. Enter confirms the highlighted choice — which is
/// "exit" when the No row is selected — so the verb is `confirm`, not `trust`.
pub const TRUST: &[Hint] = &[("↑↓", "move"), ("Enter", "confirm"), ("Esc", "quit")];
/// Quit-all confirm. The highlighted row defaults to `Cancel`, so `Enter`
/// confirms whatever is selected rather than always quitting.
pub const QUIT_ALL: &[Hint] = &[("↑↓", "move"), ("Enter", "confirm"), ("Esc", "cancel")];
pub const COMMANDS: &[Hint] = &[("↑↓", "move"), ("Tab", "complete"), ("Enter", "run")];
pub const BROWSE: &[Hint] = &[
    ("↑↓", "move"),
    ("Enter", "open"),
    ("←", "up"),
    ("Esc", "close"),
];
pub const SCROLL_BACK_CLOSE: &[Hint] = &[("↑↓", "scroll"), ("←", "back"), ("Esc", "close")];
/// `/diff`. There are thirteen bindings; this row holds five, chosen so the
/// verbs still fit at the width the patch pane actually gets with all three
/// panes open (~50 columns). Everything else — `/` search, `o` open, `v`
/// split, `s`/`u` stage, `d` source, the scroll keys — lives behind `?`,
/// which is why `?` never leaves this row.
pub const DIFF: &[Hint] = &[
    ("] [", "hunk"),
    ("n p", "file"),
    ("m", "done"),
    ("?", "keys"),
    ("Esc", "close"),
];
