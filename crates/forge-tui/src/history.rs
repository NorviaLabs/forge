//! Input history (TUI-05 / tui-input-history.md).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_INPUT_HISTORY: usize = 500;

/// Submitted-line history for the TUI input bar.
#[derive(Debug, Clone)]
pub struct InputHistory {
    /// Oldest → newest.
    entries: Vec<String>,
    max_entries: usize,
    /// None = live draft; Some(i) = viewing entries[i].
    browse_index: Option<usize>,
    /// Draft text when user first pressed Up from live input.
    stash: Option<String>,
}

impl Default for InputHistory {
    fn default() -> Self {
        Self::new(MAX_INPUT_HISTORY)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredHistoryEntry {
    workspace: String,
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredHistory {
    #[serde(default)]
    entries: Vec<StoredHistoryEntry>,
}

/// User-level history store, partitioned by canonical workspace.
#[derive(Debug, Clone)]
pub struct HistoryStore {
    path: PathBuf,
    workspace: String,
}

impl HistoryStore {
    pub fn new(path: PathBuf, workspace: &Path) -> Self {
        let workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf())
            .display()
            .to_string();
        Self { path, workspace }
    }

    pub fn user_default(workspace: &Path) -> Self {
        let path = std::env::var_os("FORGE_INPUT_HISTORY_PATH")
            .map(PathBuf::from)
            .or_else(|| {
                Some(
                    dirs::data_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join("forge")
                        .join("input-history.json"),
                )
            })
            .expect("history path fallback");
        Self::new(path, workspace)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> StoredHistory {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn load(&self, max_entries: usize) -> Vec<String> {
        self.read()
            .entries
            .into_iter()
            .filter(|entry| {
                entry.workspace == self.workspace && InputHistory::should_store(&entry.text)
            })
            .map(|entry| entry.text)
            .rev()
            .take(max_entries.max(1))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn push(&self, line: &str, max_entries: usize) -> std::io::Result<()> {
        if !InputHistory::should_store(line) {
            return Ok(());
        }
        let text = line.trim();
        let mut history = self.read();
        if history
            .entries
            .iter()
            .rev()
            .find(|entry| entry.workspace == self.workspace)
            .is_some_and(|entry| entry.text == text)
        {
            return Ok(());
        }
        history.entries.push(StoredHistoryEntry {
            workspace: self.workspace.clone(),
            text: text.to_string(),
        });
        let keep = max_entries.max(1);
        let workspace_count = history
            .entries
            .iter()
            .filter(|entry| entry.workspace == self.workspace)
            .count();
        if workspace_count > keep {
            let remove = workspace_count - keep;
            let mut removed = 0;
            history.entries.retain(|entry| {
                if entry.workspace == self.workspace && removed < remove {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        }
        let Some(parent) = self.path.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "history path has no parent",
            ));
        };
        std::fs::create_dir_all(parent)?;
        let temp = self.path.with_extension("json.tmp");
        let encoded = serde_json::to_vec_pretty(&history).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        std::fs::write(&temp, encoded)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(temp, &self.path)
    }
}

impl InputHistory {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries: max_entries.max(1),
            browse_index: None,
            stash: None,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn browsing(&self) -> bool {
        self.browse_index.is_some()
    }

    /// Whether a line should be stored (non-empty, not secret-like).
    pub fn should_store(line: &str) -> bool {
        let t = line.trim();
        if t.is_empty() {
            return false;
        }
        !looks_like_secret(t)
    }

    /// Append a submitted line; reset browse to live. Skips empty, secrets, consecutive dups.
    pub fn push(&mut self, line: &str) {
        let t = line.trim();
        if !Self::should_store(t) {
            self.reset_browse();
            return;
        }
        if self.entries.last().map(|s| s.as_str()) == Some(t) {
            self.reset_browse();
            return;
        }
        self.entries.push(t.to_string());
        while self.entries.len() > self.max_entries {
            self.entries.remove(0);
        }
        self.reset_browse();
    }

    /// Move to older entry, wrapping from the oldest back to the newest.
    /// `draft` is current input text when leaving live mode. Returns text to
    /// show in the input bar, or `None` if there's no history at all.
    pub fn up(&mut self, draft: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        match self.browse_index {
            None => {
                self.stash = Some(draft.to_string());
                let i = self.entries.len() - 1;
                self.browse_index = Some(i);
                Some(self.entries[i].clone())
            }
            Some(0) => {
                // Wrap to the newest entry.
                let i = self.entries.len() - 1;
                self.browse_index = Some(i);
                Some(self.entries[i].clone())
            }
            Some(i) => {
                let i = i - 1;
                self.browse_index = Some(i);
                Some(self.entries[i].clone())
            }
        }
    }

    /// Move to newer entry, wrapping from the newest back to the oldest.
    /// Returns `None` when not currently browsing (nothing to cycle).
    pub fn down(&mut self) -> Option<String> {
        let i = self.browse_index?;
        if i + 1 < self.entries.len() {
            let i = i + 1;
            self.browse_index = Some(i);
            Some(self.entries[i].clone())
        } else {
            // Wrap to the oldest entry.
            self.browse_index = Some(0);
            Some(self.entries[0].clone())
        }
    }

    /// Explicitly leave browse mode (e.g. Esc), restoring the stashed live
    /// draft. Unlike `up`/`down`, which now wrap around indefinitely, this is
    /// the only way back to the live draft. Returns `None` when not
    /// currently browsing.
    pub fn leave_browse(&mut self) -> Option<String> {
        self.browse_index?;
        let draft = self.stash.take().unwrap_or_default();
        self.browse_index = None;
        Some(draft)
    }

    pub fn reset_browse(&mut self) {
        self.browse_index = None;
        self.stash = None;
    }

    /// Replace all entries with a resumed session's own past input lines
    /// (oldest → newest), for restoring Up/Down arrow-key recall on
    /// `/resume`. Reuses `push`'s existing policy (secret-line filtering,
    /// consecutive-dup skipping, `max_entries` trimming) rather than
    /// reimplementing it, so resumed history follows the same rules as
    /// history built from live typing.
    pub fn load_resumed(&mut self, entries: impl IntoIterator<Item = String>) {
        self.entries.clear();
        self.reset_browse();
        for entry in entries {
            self.push(&entry);
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }
}

pub fn search_entries(entries: &[String], query: &str) -> Vec<String> {
    let query = query.trim().to_ascii_lowercase();
    let mut matches = entries
        .iter()
        .rev()
        .filter_map(|entry| fuzzy_match_score(entry, &query).map(|score| (score, entry)))
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, _), (right_score, _)| right_score.cmp(left_score));
    matches
        .into_iter()
        .map(|(_, entry)| entry.clone())
        .collect()
}

fn fuzzy_match_score(candidate: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let candidate = candidate.to_ascii_lowercase();
    let candidate_chars = candidate.chars().collect::<Vec<_>>();
    let query_chars = query.chars().collect::<Vec<_>>();
    let mut candidate_index = 0;
    let mut previous_match = None;
    let mut score = 0;
    for query_char in query_chars {
        let relative_index = candidate_chars[candidate_index..]
            .iter()
            .position(|candidate_char| *candidate_char == query_char)?;
        let match_index = candidate_index + relative_index;
        score += 10 - relative_index.min(10) as i32;
        if previous_match == Some(match_index.saturating_sub(1)) {
            score += 12;
        }
        if match_index == 0
            || candidate_chars
                .get(match_index.saturating_sub(1))
                .is_some_and(|character| !character.is_ascii_alphanumeric())
        {
            score += 8;
        }
        previous_match = Some(match_index);
        candidate_index = match_index + 1;
    }
    Some(score - candidate_chars.len().min(40) as i32)
}

fn looks_like_secret(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("api_key=") || lower.contains("apikey=") {
        return true;
    }
    if lower.contains("sk-") && line.len() > 20 {
        return true;
    }
    // long token-like single word
    let trimmed = line.trim();
    if !trimmed.contains(char::is_whitespace)
        && trimmed.len() >= 40
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn push_and_up_down_cycle() {
        let mut h = InputHistory::new(100);
        h.push("one");
        h.push("two");
        h.push("three");
        assert_eq!(h.up("draft").as_deref(), Some("three"));
        assert_eq!(h.up("ignored").as_deref(), Some("two"));
        assert_eq!(h.up("").as_deref(), Some("one"));
        assert_eq!(h.down().as_deref(), Some("two"));
        assert_eq!(h.down().as_deref(), Some("three"));
        assert!(h.browsing());
        assert_eq!(
            h.leave_browse().as_deref(),
            Some("draft"),
            "explicit leave restores the stashed live draft"
        );
        assert!(!h.browsing());
    }

    #[test]
    fn persistent_history_survives_sessions_but_stays_workspace_scoped() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("history.json");
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let store = HistoryStore::new(path.clone(), &first);
        store.push("one", 500).unwrap();
        store.push("two", 500).unwrap();

        let reloaded = HistoryStore::new(path.clone(), &first);
        assert_eq!(reloaded.load(500), ["one", "two"]);
        let other = HistoryStore::new(path, &second);
        assert!(other.load(500).is_empty());
    }

    #[test]
    fn persistent_history_filters_secrets_and_trims_oldest_entry() {
        let dir = TempDir::new().unwrap();
        let store = HistoryStore::new(dir.path().join("history.json"), dir.path());
        store.push("one", 2).unwrap();
        store.push("api_key=secret", 2).unwrap();
        store.push("two", 2).unwrap();
        store.push("three", 2).unwrap();
        assert_eq!(store.load(2), ["two", "three"]);
    }

    #[test]
    fn up_wraps_from_oldest_to_newest() {
        let mut h = InputHistory::new(100);
        h.push("one");
        h.push("two");
        h.push("three");
        assert_eq!(h.up("draft").as_deref(), Some("three"));
        assert_eq!(h.up("").as_deref(), Some("two"));
        assert_eq!(h.up("").as_deref(), Some("one"));
        assert_eq!(
            h.up("").as_deref(),
            Some("three"),
            "Up past oldest wraps to newest"
        );
    }

    #[test]
    fn down_wraps_from_newest_to_oldest() {
        let mut h = InputHistory::new(100);
        h.push("one");
        h.push("two");
        h.push("three");
        h.up("draft");
        assert_eq!(
            h.down().as_deref(),
            Some("one"),
            "Down past newest wraps to oldest"
        );
    }

    #[test]
    fn leave_browse_is_noop_when_not_browsing() {
        let mut h = InputHistory::new(10);
        h.push("one");
        assert!(h.leave_browse().is_none());
    }

    #[test]
    fn load_resumed_replaces_entries_in_order() {
        let mut h = InputHistory::new(100);
        h.push("stale");
        h.load_resumed(["one".to_string(), "two".to_string(), "three".to_string()]);
        assert_eq!(
            h.entries(),
            &["one".to_string(), "two".to_string(), "three".to_string()]
        );
        assert_eq!(h.up("draft").as_deref(), Some("three"));
    }

    #[test]
    fn load_resumed_applies_the_same_filtering_as_push() {
        let mut h = InputHistory::new(100);
        h.load_resumed([
            "hello".to_string(),
            "".to_string(),
            "hello".to_string(), // consecutive dup, skipped
            "sk-abcdefghijklmnopqrstuvwxyz012345".to_string(), // secret-like, skipped
            "world".to_string(),
        ]);
        assert_eq!(h.entries(), &["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn load_resumed_resets_browse_state() {
        let mut h = InputHistory::new(100);
        h.push("one");
        h.push("two");
        h.up("draft");
        assert!(h.browsing());
        h.load_resumed(["a".to_string(), "b".to_string()]);
        assert!(!h.browsing());
    }

    #[test]
    fn empty_history_up_noop() {
        let mut h = InputHistory::default();
        assert!(h.up("x").is_none());
    }

    #[test]
    fn fuzzy_search_matches_subsequences_newest_first() {
        let entries = vec![
            "cargo test".to_string(),
            "git status --short".to_string(),
            "git diff --stat".to_string(),
        ];
        assert_eq!(search_entries(&entries, "gds"), vec!["git diff --stat"]);
    }

    #[test]
    fn fuzzy_search_prefers_consecutive_matches() {
        let entries = vec!["run cargo test".to_string(), "cargo test".to_string()];
        assert_eq!(
            search_entries(&entries, "ct"),
            vec!["cargo test", "run cargo test"]
        );
    }

    #[test]
    fn skips_empty_and_consecutive_dup() {
        let mut h = InputHistory::new(10);
        h.push("  ");
        h.push("");
        assert!(h.is_empty());
        h.push("a");
        h.push("a");
        assert_eq!(h.len(), 1);
        h.push("b");
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn rejects_secret_like_lines() {
        assert!(!InputHistory::should_store(
            "sk-abcdefghijklmnopqrstuvwxyz012345"
        ));
        assert!(!InputHistory::should_store(
            "export API_KEY=secretvaluehere"
        ));
        assert!(!InputHistory::should_store(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(InputHistory::should_store("/status"));
        assert!(InputHistory::should_store("fix the bug please"));
        let mut h = InputHistory::new(10);
        h.push("sk-abcdefghijklmnopqrstuvwxyz012345");
        assert!(h.is_empty());
    }

    #[test]
    fn max_entries_drops_oldest() {
        let mut h = InputHistory::new(2);
        h.push("1");
        h.push("2");
        h.push("3");
        assert_eq!(h.entries(), &["2".to_string(), "3".to_string()]);
    }

    #[test]
    fn push_resets_browse() {
        let mut h = InputHistory::new(10);
        h.push("a");
        h.push("b");
        h.up("");
        assert!(h.browsing());
        h.push("c");
        assert!(!h.browsing());
        assert_eq!(h.up("").as_deref(), Some("c"));
    }

    #[test]
    fn mixed_commands_and_text_keep_browsing_through_all_entries() {
        let mut h = InputHistory::new(10);
        h.push("hello");
        h.push("/status");
        h.push("world");

        assert_eq!(h.up("draft").as_deref(), Some("world"));
        assert_eq!(h.up("ignored").as_deref(), Some("/status"));
        assert_eq!(h.up("ignored").as_deref(), Some("hello"));
        assert_eq!(h.down().as_deref(), Some("/status"));
        assert_eq!(h.down().as_deref(), Some("world"));
        assert!(h.browsing());
        assert_eq!(h.leave_browse().as_deref(), Some("draft"));
        assert!(!h.browsing());
    }
}
