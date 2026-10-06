//! Fitting long text — paths above all — into a fixed number of columns.
//!
//! Clipping at the right edge is the wrong default for a path. The tail is the
//! part that identifies it: cutting `…/scratchpad/lab` off the end of a long
//! temp path leaves the caller staring at a UUID and no folder name. Every
//! comparable terminal tool elides the middle instead, and so does Forge.

/// Width of the ellipsis, in columns.
const ELLIPSIS: char = '…';

/// Middle-elide `text` so it occupies at most `max` columns.
///
/// Returns `text` unchanged when it already fits. Below the width needed to
/// show an ellipsis plus one character on each side, falls back to a plain
/// head-truncation, because an ellipsis alone carries less than a fragment.
pub fn elide_middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    if max < 3 {
        return text.chars().take(max).collect();
    }
    // One column goes to the ellipsis; split the rest, favouring the tail,
    // which is where a path keeps its name.
    let budget = max - 1;
    let head = budget / 2;
    let tail = budget - head;
    let start: String = text.chars().take(head).collect();
    let end: String = {
        let mut chars: Vec<char> = text.chars().collect();
        chars.drain(..count - tail);
        chars.into_iter().collect()
    };
    format!("{start}{ELLIPSIS}{end}")
}

/// Middle-elide a path, cutting on separators so whole segments survive.
///
/// Prefers `/private/tmp/claude-501/…/scratchpad/lab` over a cut that lands
/// inside a segment. Falls back to [`elide_middle`] when no separator split
/// fits — a single very long segment has no boundary to respect.
pub fn elide_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 4 {
        return elide_middle(path, max);
    }

    // Grow the kept tail as far as the budget allows, then the kept head.
    // The tail is grown first: the last segments name the thing.
    let width = |head: usize, tail: usize| -> usize {
        let head_str = segments[..head].join("/");
        let tail_str = segments[segments.len() - tail..].join("/");
        // head + "/…/" + tail
        head_str.chars().count() + 3 + tail_str.chars().count()
    };

    let mut best: Option<(usize, usize)> = None;
    for tail in 1..segments.len() {
        for head in 1..segments.len() - tail {
            if width(head, tail) <= max {
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
