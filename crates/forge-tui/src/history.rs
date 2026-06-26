//! Input command history (TUI-05 / tui-input-history.md).

/// In-session submitted-line history for the TUI input bar.
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
        Self::new(500)
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

    /// Move to older entry. `draft` is current input text when leaving live mode.
    /// Returns text to show in the input bar, or `None` if no history / already at oldest.
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
                // already oldest
                Some(self.entries[0].clone())
            }
            Some(i) => {
                let i = i - 1;
                self.browse_index = Some(i);
                Some(self.entries[i].clone())
            }
        }
    }

    /// Move toward newer; past newest restores stash and live mode.
    /// Returns `Some(text)` for the input bar. When leaving browse to live, returns stash
    /// (possibly empty).
    pub fn down(&mut self) -> Option<String> {
        let i = self.browse_index?;
        if i + 1 < self.entries.len() {
            let i = i + 1;
            self.browse_index = Some(i);
            Some(self.entries[i].clone())
        } else {
            // leave history → live
            let draft = self.stash.take().unwrap_or_default();
            self.browse_index = None;
            Some(draft)
        }
    }

    pub fn reset_browse(&mut self) {
        self.browse_index = None;
        self.stash = None;
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }
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

    #[test]
    fn push_and_up_down_cycle() {
        let mut h = InputHistory::new(100);
        h.push("one");
        h.push("two");
        h.push("three");
        assert_eq!(h.up("draft").as_deref(), Some("three"));
        assert_eq!(h.up("ignored").as_deref(), Some("two"));
        assert_eq!(h.up("").as_deref(), Some("one"));
        // stay at oldest
        assert_eq!(h.up("").as_deref(), Some("one"));
        assert_eq!(h.down().as_deref(), Some("two"));
        assert_eq!(h.down().as_deref(), Some("three"));
        assert_eq!(h.down().as_deref(), Some("draft"));
        assert!(!h.browsing());
        // further down is no-op (None)
        assert!(h.down().is_none());
    }

    #[test]
    fn empty_history_up_noop() {
        let mut h = InputHistory::default();
        assert!(h.up("x").is_none());
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
        assert!(!InputHistory::should_store("sk-abcdefghijklmnopqrstuvwxyz012345"));
        assert!(!InputHistory::should_store("export API_KEY=secretvaluehere"));
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
}
