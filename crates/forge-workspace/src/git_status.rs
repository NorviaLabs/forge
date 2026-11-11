//! Parsing `git status --porcelain=1 -z` into per-path staged/unstaged status.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatusKind {
    Modified,
    Added,
    Deleted,
    Untracked,
    Ignored,
    Conflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PathStatus {
    pub staged: Option<GitStatusKind>,
    pub unstaged: Option<GitStatusKind>,
}

impl PathStatus {
    pub fn primary(self) -> Option<GitStatusKind> {
        match (self.staged, self.unstaged) {
            (Some(a), Some(b)) => Some(if a.is_more_severe(b) { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    pub fn is_conflicted(self) -> bool {
        self.staged == Some(GitStatusKind::Conflicted)
            || self.unstaged == Some(GitStatusKind::Conflicted)
    }
}

impl GitStatusKind {
    pub fn is_more_severe(self, other: Self) -> bool {
        let rank = |k: Self| match k {
            Self::Conflicted => 3,
            Self::Added => 2,
            Self::Modified => 1,
            Self::Deleted => 1,
            Self::Untracked | Self::Ignored => 0,
        };
        rank(self) > rank(other)
    }
}

pub(crate) fn parse_null_terminated(data: &[u8]) -> Result<HashMap<PathBuf, PathStatus>, String> {
    let mut map = HashMap::new();
    let text = String::from_utf8_lossy(data);
    let parts: Vec<&str> = text.split('\0').collect();
    let mut i = 0;
    while i < parts.len() {
        let record = parts[i];
        if record.is_empty() {
            i += 1;
            continue;
        }
        if record.len() < 3 {
            i += 1;
            continue;
        }
        let xy = &record[..2];
        let path = record[2..].trim_start_matches(' ');
        let Some(status) = classify_path_status(xy) else {
            i += 1;
            continue;
        };

        let (target_path, consumed) = if xy.starts_with('R') || xy.starts_with('C') {
            // Rename/copy format: "XY old_path\0new_path\0".
            if i + 1 < parts.len() {
                i += 1;
                (parts[i], 2)
            } else {
                (path, 1)
            }
        } else {
            (path, 1)
        };

        let target = PathBuf::from(target_path);
        map.entry(target)
            .and_modify(|existing| *existing = merge_path_status(*existing, status))
            .or_insert(status);
        i += consumed;
    }
    Ok(map)
}

fn classify_letter(letter: char) -> Option<GitStatusKind> {
    match letter {
        'M' | 'T' | 'R' | 'C' => Some(GitStatusKind::Modified),
        'A' => Some(GitStatusKind::Added),
        'D' => Some(GitStatusKind::Deleted),
        'U' => Some(GitStatusKind::Conflicted),
        '?' => Some(GitStatusKind::Untracked),
        '!' => Some(GitStatusKind::Ignored),
        _ => None,
    }
}

fn classify_path_status(xy: &str) -> Option<PathStatus> {
    if xy.len() != 2 {
        return None;
    }
    let mut chars = xy.chars();
    let x = chars.next().unwrap();
    let y = chars.next().unwrap();
    if xy == "DD" || xy == "AU" || xy == "UA" || xy == "DU" || xy == "UD" || x == 'U' || y == 'U' {
        return Some(PathStatus {
            staged: Some(GitStatusKind::Conflicted),
            unstaged: Some(GitStatusKind::Conflicted),
        });
    }
    if x == '?' && y == '?' {
        return Some(PathStatus {
            staged: None,
            unstaged: Some(GitStatusKind::Untracked),
        });
    }
    Some(PathStatus {
        staged: if x == ' ' { None } else { classify_letter(x) },
        unstaged: if y == ' ' { None } else { classify_letter(y) },
    })
    .filter(|status| status.primary().is_some())
}

fn merge_path_status(left: PathStatus, right: PathStatus) -> PathStatus {
    PathStatus {
        staged: pick_more_severe(left.staged, right.staged),
        unstaged: pick_more_severe(left.unstaged, right.unstaged),
    }
}

fn pick_more_severe(
    left: Option<GitStatusKind>,
    right: Option<GitStatusKind>,
) -> Option<GitStatusKind> {
    match (left, right) {
        (Some(a), Some(b)) => Some(if a.is_more_severe(b) { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(test)]
fn classify_status(xy: &str) -> Option<GitStatusKind> {
    classify_path_status(xy).and_then(PathStatus::primary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn porcelain_splits_staged_and_unstaged() {
        let status = classify_path_status("MM").unwrap();
        assert_eq!(status.staged, Some(GitStatusKind::Modified));
        assert_eq!(status.unstaged, Some(GitStatusKind::Modified));
        let staged_only = classify_path_status("M ").unwrap();
        assert_eq!(staged_only.staged, Some(GitStatusKind::Modified));
        assert_eq!(staged_only.unstaged, None);
        let worktree_only = classify_path_status(" M").unwrap();
        assert_eq!(worktree_only.staged, None);
        assert_eq!(worktree_only.unstaged, Some(GitStatusKind::Modified));
    }

    #[test]
    fn classify_status_maps_common_codes() {
        assert_eq!(classify_status("M "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status(" M"), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("A "), Some(GitStatusKind::Added));
        assert_eq!(classify_status("??"), Some(GitStatusKind::Untracked));
        assert_eq!(classify_status("U "), Some(GitStatusKind::Conflicted));
        assert_eq!(classify_status("!!"), Some(GitStatusKind::Ignored));
    }

    #[test]
    fn parse_null_terminated_simple() {
        let data = b"M a.txt\0?? b.txt\0";
        let map = parse_null_terminated(data).unwrap();
        assert_eq!(
            map.get(Path::new("a.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
        );
        assert_eq!(
            map.get(Path::new("b.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Untracked)
        );
    }

    #[test]
    fn parse_null_terminated_rename() {
        let data = b"R old.txt\0new.txt\0";
        let map = parse_null_terminated(data).unwrap();
        assert_eq!(map.get(Path::new("old.txt")), None);
        assert_eq!(
            map.get(Path::new("new.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
        );
    }

    #[test]
    fn severity_ranks_conflicts_above_adds_above_edits() {
        use GitStatusKind::*;
        assert!(Conflicted.is_more_severe(Added));
        assert!(Added.is_more_severe(Modified));
        assert!(Modified.is_more_severe(Untracked));
        assert!(!Added.is_more_severe(Conflicted));
        // Modified and Deleted share a rank, as do Untracked and Ignored.
        assert!(!Modified.is_more_severe(Deleted));
        assert!(!Deleted.is_more_severe(Modified));
        assert!(!Untracked.is_more_severe(Ignored));
    }

    #[test]
    fn classify_status_rejects_codes_that_are_not_two_chars() {
        assert_eq!(classify_status(""), None);
        assert_eq!(classify_status("M"), None);
        assert_eq!(classify_status("MMM"), None);
    }

    #[test]
    fn classify_status_maps_remaining_codes() {
        assert_eq!(classify_status("D "), Some(GitStatusKind::Deleted));
        assert_eq!(classify_status(" D"), Some(GitStatusKind::Deleted));
        assert_eq!(classify_status("T "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("C "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("R "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("DD"), Some(GitStatusKind::Conflicted));
        assert_eq!(classify_status("AU"), Some(GitStatusKind::Conflicted));
        assert_eq!(classify_status("UD"), Some(GitStatusKind::Conflicted));
        // A clean entry and an unrecognised pair both classify as nothing.
        assert_eq!(classify_status("  "), None);
        assert_eq!(classify_status("XY"), None);
    }

    #[test]
    fn parse_null_terminated_skips_malformed_records() {
        // "M" is below the 3-byte "XY path" minimum and "ZZ x.txt" carries an
        // unrecognised status pair. Both are skipped without failing the parse.
        let map = parse_null_terminated(b"M\0ZZ x.txt\0M  good.txt\0").unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get(Path::new("good.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
        );
    }

    #[test]
    fn duplicate_paths_keep_the_more_severe_status() {
        // The severity comparison must win regardless of the order git reports in.
        let escalating = parse_null_terminated(b"M  dup.txt\0A  dup.txt\0").unwrap();
        assert_eq!(
            escalating
                .get(Path::new("dup.txt"))
                .and_then(|s| s.primary()),
            Some(GitStatusKind::Added)
        );
        let descending = parse_null_terminated(b"A  dup.txt\0M  dup.txt\0").unwrap();
        assert_eq!(
            descending
                .get(Path::new("dup.txt"))
                .and_then(|s| s.primary()),
            Some(GitStatusKind::Added)
        );
    }

    #[test]
    fn truncated_rename_record_falls_back_to_the_original_path() {
        // A rename record whose second (new path) entry never arrives must not
        // panic, and should fall back to the path carried by the first record.
        let map = parse_null_terminated(b"R  only.txt").unwrap();
        assert_eq!(
            map.get(Path::new("only.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
        );
    }
}
