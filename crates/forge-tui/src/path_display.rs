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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_that_fits_is_returned_untouched() {
        assert_eq!(elide_middle("short", 10), "short");
        assert_eq!(elide_middle("exact", 5), "exact");
    }

    #[test]
    fn eliding_never_exceeds_the_budget() {
        for max in 1..40 {
            let out = elide_middle("abcdefghijklmnopqrstuvwxyz", max);
            assert!(
                out.chars().count() <= max,
                "max {max} produced {out:?} ({} chars)",
                out.chars().count()
            );
        }
    }

    #[test]
    fn eliding_keeps_both_ends() {
        let out = elide_middle("abcdefghijklmnop", 7);
        assert!(out.starts_with("abc"), "{out}");
        assert!(out.ends_with("nop"), "{out}");
        assert!(out.contains('…'), "{out}");
    }

    /// The bug this exists to fix: right-edge truncation kept the meaningless
    /// hash and threw away the folder name.
    #[test]
    fn a_path_keeps_its_last_segment() {
        let path =
            "/private/tmp/claude-501/-Users-mohitranka-Projects-forge/ac5a5dcf-403d/scratchpad/lab";
        let out = elide_path(path, 46);
        assert!(out.chars().count() <= 46, "{out}");
        assert!(out.ends_with("/lab"), "the folder name must survive: {out}");
        assert!(out.starts_with("/private"), "{out}");
        assert!(out.contains('…'), "{out}");
    }

    #[test]
    fn a_path_elides_on_separators() {
        let path = "/aaa/bbb/ccc/ddd/eee/fff";
        let out = elide_path(path, 16);
        assert!(out.chars().count() <= 16, "{out}");
        for segment in out.split('/').filter(|s| !s.is_empty() && *s != "…") {
            assert!(
                path.split('/').any(|original| original == segment),
                "segment {segment:?} was cut in half: {out}"
            );
        }
    }

    #[test]
    fn a_path_with_no_usable_separator_still_fits() {
        let path = "/averyveryverylongsinglesegmentwithnobreaks";
        let out = elide_path(path, 20);
        assert!(out.chars().count() <= 20, "{out}");
    }

    #[test]
    fn a_short_path_is_untouched() {
        assert_eq!(elide_path("~/demo", 40), "~/demo");
    }
}
