//! VS Code–inspired Quick Open path scoring for the TUI.
//!
//! Agent `fffind` keeps permissive fuzzy matching; Quick Open uses word-boundary
//! subsequence scoring with basename and path-segment bonuses.

use crate::types::FileSearchHit;

const SEPARATOR_BONUS: i32 = 10;
const CONSECUTIVE_BONUS: i32 = 5;
const CAMEL_BONUS: i32 = 10;
const LEADING_PENALTY: i32 = -3;
const MAX_LEADING_PENALTY: i32 = -9;
const UNMATCHED_PENALTY: i32 = -1;
const BASENAME_BONUS: i32 = 40;
const PATH_SEGMENT_BONUS: i32 = 25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickOpenScore {
    pub score: i32,
    pub match_ranges: Vec<(u32, u32)>,
}

/// Score a workspace-relative path for Quick Open and return highlight ranges.
pub fn score_quick_open(path: &str, query: &str) -> Option<QuickOpenScore> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }

    if query.contains('/') {
        return score_path_aware(path, query);
    }

    if query.split_whitespace().count() > 1 {
        return score_multi_piece(path, query);
    }

    let mut best = score_fuzzy_word_boundary(path, query, true)?;

    if let Some((filename, offset)) = filename_in_path(path) {
        if let Some(mut base) = score_fuzzy_word_boundary(filename, query, false) {
            base.score += BASENAME_BONUS;
            base.match_ranges = offset_ranges(base.match_ranges, offset);
            if base.score > best.score {
                best = base;
            }
        }
    }

    Some(best)
}

/// Re-rank fff candidates with Quick Open scoring.
pub fn rerank_quick_open_hits(hits: Vec<FileSearchHit>, query: &str) -> Vec<FileSearchHit> {
    let query = query.trim();
    if query.is_empty() {
        return hits;
    }

    let mut scored: Vec<FileSearchHit> = hits
        .into_iter()
        .filter_map(|hit| {
            let scored = score_quick_open(&hit.path, query)?;
            Some(FileSearchHit {
                path: hit.path,
                score: scored.score,
                relevance: 0.0,
                match_ranges: scored.match_ranges,
            })
        })
        .collect();

    scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    if let Some(top) = scored.first().map(|hit| hit.score.max(1)) {
        for hit in &mut scored {
            hit.relevance = (hit.score as f32 / top as f32).clamp(0.0, 1.0);
        }
    }
    scored
}

fn filename_in_path(path: &str) -> Option<(&str, u32)> {
    let (prefix, filename) = path.rsplit_once('/')?;
    let offset = (prefix.len() + 1) as u32;
    Some((filename, offset))
}

fn offset_ranges(ranges: Vec<(u32, u32)>, offset: u32) -> Vec<(u32, u32)> {
    ranges
        .into_iter()
        .map(|(start, end)| (start + offset, end + offset))
        .collect()
}

fn is_separator(byte: u8) -> bool {
    matches!(byte, b'/' | b'-' | b'_' | b'.' | b' ' | b'\\')
}

fn is_word_boundary(path: &str, byte_idx: usize) -> bool {
    if byte_idx == 0 {
        return true;
    }
    let bytes = path.as_bytes();
    if is_separator(bytes[byte_idx - 1]) {
        return true;
    }
    if byte_idx < bytes.len() {
        let prev = bytes[byte_idx - 1];
        let cur = bytes[byte_idx];
        if prev.is_ascii_lowercase() && cur.is_ascii_uppercase() {
            return true;
        }
    }
    false
}

fn score_multi_piece(path: &str, query: &str) -> Option<QuickOpenScore> {
    let mut total_score = 0;
    let mut ranges = Vec::new();
    for piece in query.split_whitespace() {
        let mut piece_score = score_fuzzy_word_boundary(path, piece, false)?;
        if let Some((filename, offset)) = filename_in_path(path) {
            if let Some(mut base) = score_fuzzy_word_boundary(filename, piece, false) {
                base.score += BASENAME_BONUS;
                base.match_ranges = offset_ranges(base.match_ranges, offset);
                if base.score > piece_score.score {
                    piece_score = base;
                }
            }
        }
        total_score += piece_score.score;
        ranges.extend(piece_score.match_ranges);
    }
    ranges.sort_unstable_by_key(|range| range.0);
    Some(QuickOpenScore {
        score: total_score,
        match_ranges: ranges,
    })
}

fn score_path_aware(path: &str, query: &str) -> Option<QuickOpenScore> {
    let query_parts: Vec<&str> = query.split('/').filter(|part| !part.is_empty()).collect();
    let path_parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if query_parts.is_empty() || query_parts.len() > path_parts.len() {
        return None;
    }

    let offset = path_parts.len() - query_parts.len();
    let mut byte_offsets = Vec::with_capacity(path_parts.len());
    let mut cursor = 0usize;
    for part in &path_parts {
        byte_offsets.push(cursor);
        cursor += part.len() + 1;
    }

    let mut total_score = 0;
    let mut ranges = Vec::new();
    for (index, query_part) in query_parts.iter().enumerate() {
        let part = path_parts[offset + index];
        let part_score = score_fuzzy_word_boundary(part, query_part, false)?;
        total_score += part_score.score + PATH_SEGMENT_BONUS;
        let part_offset = byte_offsets[offset + index] as u32;
        ranges.extend(offset_ranges(part_score.match_ranges, part_offset));
    }

    ranges.sort_unstable_by_key(|range| range.0);
    Some(QuickOpenScore {
        score: total_score,
        match_ranges: ranges,
    })
}

fn score_fuzzy_word_boundary(
    haystack: &str,
    needle: &str,
    penalize_suffix: bool,
) -> Option<QuickOpenScore> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }

    let h_bytes = haystack.as_bytes();
    let n_bytes = needle.as_bytes();
    let h_len = h_bytes.len();
    let n_len = n_bytes.len();

    let mut best: Option<(i32, Vec<usize>)> = None;
    let mut matches = Vec::with_capacity(n_len);

    struct VisitCtx<'a> {
        haystack: &'a str,
        h_bytes: &'a [u8],
        n_bytes: &'a [u8],
        h_len: usize,
        n_len: usize,
        penalize_suffix: bool,
    }

    fn visit(
        ctx: &VisitCtx<'_>,
        h_start: usize,
        n_idx: usize,
        last_match: Option<usize>,
        score: i32,
        matches: &mut Vec<usize>,
        best: &mut Option<(i32, Vec<usize>)>,
    ) {
        if n_idx == ctx.n_len {
            let mut final_score = score;
            if ctx.penalize_suffix {
                let tail = ctx
                    .h_len
                    .saturating_sub(matches.last().copied().unwrap_or(0) + 1)
                    as i32;
                final_score += tail * UNMATCHED_PENALTY;
            }
            if best
                .as_ref()
                .map(|(best_score, _)| final_score > *best_score)
                .unwrap_or(true)
            {
                *best = Some((final_score, matches.clone()));
            }
            return;
        }

        let n_lower = ctx.n_bytes[n_idx].to_ascii_lowercase();
        for h_idx in h_start..ctx.h_len {
            if ctx.h_bytes[h_idx].to_ascii_lowercase() != n_lower {
                continue;
            }
            if let Some(prev) = last_match {
                if h_idx > prev + 1 && !is_word_boundary(ctx.haystack, h_idx) {
                    continue;
                }
            } else if !is_word_boundary(ctx.haystack, h_idx) {
                continue;
            }

            let mut next_score = score;
            if let Some(prev) = last_match {
                if h_idx == prev + 1 {
                    next_score += CONSECUTIVE_BONUS;
                }
                if h_idx > 0 && is_separator(ctx.h_bytes[h_idx - 1]) {
                    next_score += SEPARATOR_BONUS;
                }
                if h_idx > 0
                    && ctx.h_bytes[h_idx - 1].is_ascii_lowercase()
                    && ctx.h_bytes[h_idx].is_ascii_uppercase()
                {
                    next_score += CAMEL_BONUS;
                }
            } else {
                let leading_penalty = ((h_idx as i32) * LEADING_PENALTY).max(MAX_LEADING_PENALTY);
                next_score += leading_penalty;
            }

            matches.push(h_idx);
            visit(
                ctx,
                h_idx + 1,
                n_idx + 1,
                Some(h_idx),
                next_score,
                matches,
                best,
            );
            matches.pop();
        }
    }

    let ctx = VisitCtx {
        haystack,
        h_bytes,
        n_bytes,
        h_len,
        n_len,
        penalize_suffix,
    };
    visit(&ctx, 0, 0, None, 0, &mut matches, &mut best);

    let (score, indices) = best?;
    if score <= 0 {
        return None;
    }

    let match_ranges = indices_to_ranges(&indices, h_bytes);
    Some(QuickOpenScore {
        score,
        match_ranges,
    })
}

fn indices_to_ranges(indices: &[usize], h_bytes: &[u8]) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    for &idx in indices {
        let start = idx as u32;
        let width = utf8_char_width(h_bytes, idx) as u32;
        ranges.push((start, start + width));
    }
    ranges
}

fn utf8_char_width(bytes: &[u8], index: usize) -> usize {
    let b = bytes[index];
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_prefers_filename_over_scattered_path_letters() {
        let direct = score_quick_open("crates/forge-tui/src/app/search.rs", "search").unwrap();
        let weak = score_quick_open(
            "docs/forge-v31-redesign-prompts/07-supporting-surfaces-and-chrome.md",
            "search",
        );
        assert!(weak.is_none() || direct.score > weak.unwrap().score);
    }

    #[test]
    fn path_segment_query_matches_directory_and_file() {
        let scored = score_quick_open(
            "docs/forge-v31-redesign-prompts/07-supporting-surfaces-and-chrome.md",
            "forge-v31/07",
        )
        .unwrap();
        assert!(!scored.match_ranges.is_empty());
        assert!(scored.score > 0);
    }

    #[test]
    fn supporting_matches_path_segment() {
        let scored = score_quick_open(
            "docs/forge-v31-redesign-prompts/07-supporting-surfaces-and-chrome.md",
            "supporting",
        )
        .unwrap();
        assert!(scored.score > 0);
    }

    #[test]
    fn multi_piece_query_matches_path_fragments() {
        let scored = score_quick_open(
            "docs/forge-v31-redesign-prompts/07-supporting-surfaces-and-chrome.md",
            "v31 supporting",
        )
        .unwrap();
        assert!(scored.score > 0);
    }

    #[test]
    fn rerank_drops_weak_quick_open_matches() {
        let hits = rerank_quick_open_hits(
            vec![
                FileSearchHit {
                    path: "crates/forge-tui/src/app/search.rs".into(),
                    score: 10,
                    relevance: 1.0,
                    match_ranges: vec![],
                },
                FileSearchHit {
                    path: "docs/forge-v31-redesign-prompts/07-supporting-surfaces-and-chrome.md"
                        .into(),
                    score: 9,
                    relevance: 0.9,
                    match_ranges: vec![],
                },
            ],
            "search",
        );
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("search.rs"));
        assert!(!hits[0].match_ranges.is_empty());
    }
}
