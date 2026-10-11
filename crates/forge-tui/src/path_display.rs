//! Fitting long text — paths above all — into a fixed number of columns.
//!
//! Clipping at the right edge is the wrong default for a path. The tail is the
//! part that identifies it: cutting `…/scratchpad/lab` off the end of a long
//! temp path leaves the caller staring at a UUID and no folder name. Every
//! comparable terminal tool elides the middle instead, and so does Forge.
//!
//! Every budget here is in terminal cells, not chars: a CJK character or an
//! emoji occupies two cells, and a fit measured in chars overflows by up to
//! twice its budget. Cuts land on grapheme boundaries so a combined emoji or
//! an accented letter is never split.

use ratatui::style::Style;
use ratatui::text::Span;

/// Width of the ellipsis, in columns.
const ELLIPSIS: char = '…';

/// Display width of `text` in terminal cells.
pub fn width(text: &str) -> usize {
    Span::raw(text).width()
}

/// Display width of one char in terminal cells.
pub fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    width(ch.encode_utf8(&mut buf))
}

fn graphemes(text: &str) -> Vec<String> {
    Span::raw(text)
        .styled_graphemes(Style::default())
        .map(|grapheme| grapheme.symbol.to_string())
        .collect()
}

/// Longest prefix of `text` that fits in `max` cells, with no ellipsis.
pub fn head(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for grapheme in graphemes(text) {
        let cells = width(&grapheme);
        if used + cells > max {
            break;
        }
        out.push_str(&grapheme);
        used += cells;
    }
    out
}

/// Longest suffix of `text` that fits in `max` cells, with no ellipsis.
pub fn tail(text: &str, max: usize) -> String {
    let mut kept = Vec::new();
    let mut used = 0;
    for grapheme in graphemes(text).into_iter().rev() {
        let cells = width(&grapheme);
        if used + cells > max {
            break;
        }
        kept.push(grapheme);
        used += cells;
    }
    kept.into_iter().rev().collect()
}

/// Cut `text` to at most `max` cells, ending in an ellipsis when it was cut.
pub fn truncate_end(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    format!("{}{ELLIPSIS}", head(text, max - 1))
}

/// Middle-elide `text` so it occupies at most `max` columns.
///
/// Returns `text` unchanged when it already fits. Below the width needed to
/// show an ellipsis plus one character on each side, falls back to a plain
/// head-truncation, because an ellipsis alone carries less than a fragment.
pub fn elide_middle(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max < 3 {
        return head(text, max);
    }
    // One column goes to the ellipsis; split the rest, favouring the tail,
    // which is where a path keeps its name.
    let budget = max - 1;
    let head_w = budget / 2;
    format!(
        "{}{ELLIPSIS}{}",
        head(text, head_w),
        tail(text, budget - head_w)
    )
}

/// Middle-elide a path, cutting on separators so whole segments survive.
///
/// Prefers `/private/tmp/claude-501/…/scratchpad/lab` over a cut that lands
/// inside a segment. Falls back to [`elide_middle`] when no separator split
/// fits — a single very long segment has no boundary to respect.
pub fn elide_path(path: &str, max: usize) -> String {
    if width(path) <= max {
        return path.to_string();
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 4 {
        return elide_middle(path, max);
    }

    // Grow the kept tail as far as the budget allows, then the kept head.
    // The tail is grown first: the last segments name the thing.
    let fitted = |head: usize, tail: usize| -> usize {
        let head_str = segments[..head].join("/");
        let tail_str = segments[segments.len() - tail..].join("/");
        // head + "/…/" + tail
        width(&head_str) + 3 + width(&tail_str)
    };

    let mut best: Option<(usize, usize)> = None;
    for tail in 1..segments.len() {
        for head in 1..segments.len() - tail {
            if fitted(head, tail) <= max {
                best = Some(match best {
                    Some((bh, bt)) if bt + bh >= tail + head => (bh, bt),
                    _ => (head, tail),
                });
            }
        }
    }

    match best {
        Some((head, tail)) => {
            let head_str = segments[..head].join("/");
            let tail_str = segments[segments.len() - tail..].join("/");
            format!("{head_str}/{ELLIPSIS}/{tail_str}")
        }
        None => elide_middle(path, max),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_are_measured_in_cells() {
        assert_eq!(width("東京"), 4);
        assert_eq!(truncate_end("東京都庁", 5), "東京…");
        assert_eq!(truncate_end("abc", 3), "abc");
        assert_eq!(truncate_end("abcd", 1), "…");
        assert_eq!(elide_middle("東京都庁舎ビル", 9), "東京…ビル");
        assert!(width(&elide_middle("東京都庁舎ビル", 9)) <= 9);
    }

    #[test]
    fn grapheme_clusters_are_never_split() {
        // "e" + combining acute is one grapheme, one cell.
        let accented = "e\u{301}e\u{301}e\u{301}e\u{301}";
        assert_eq!(truncate_end(accented, 3), "e\u{301}e\u{301}…");
    }

    #[test]
    fn wide_path_segments_keep_the_tail() {
        let path = "/home/ユーザー/プロジェクト/ソース/メイン.rs";
        let fitted = elide_path(path, 24);
        assert!(width(&fitted) <= 24, "{fitted}");
        assert!(fitted.ends_with("メイン.rs"), "{fitted}");
    }
}
