//! Outbound user-message queue (enqueue while busy; auto-dequeue when idle).

use std::collections::VecDeque;

/// FIFO queue of user messages waiting to be sent to the agent.
#[derive(Debug, Clone, Default)]
pub struct MessageQueue {
    items: VecDeque<String>,
}

impl MessageQueue {
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Push to the back. Returns 1-based position in the queue.
    pub fn enqueue(&mut self, text: impl Into<String>) -> usize {
        let t = text.into().trim().to_string();
        self.items.push_back(t);
        self.items.len()
    }

    /// Pop from the front (next to send).
    pub fn dequeue(&mut self) -> Option<String> {
        self.items.pop_front()
    }

    /// Drop by 1-based index. Returns removed text.
    pub fn drop_at(&mut self, one_based: usize) -> Option<String> {
        if one_based == 0 || one_based > self.items.len() {
            return None;
        }
        self.items.remove(one_based - 1)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Push to the front (used to restore a message if send is blocked).
    pub fn push_front(&mut self, text: impl Into<String>) {
        self.items.push_front(text.into());
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.items.iter()
    }

    /// Preview lines for notices (index + truncated text).
    pub fn list_lines(&self) -> Vec<String> {
        if self.items.is_empty() {
            return vec!["Message queue is empty.".into()];
        }
        let mut lines = vec![format!("{} message(s) queued:", self.items.len())];
        for (i, t) in self.items.iter().enumerate() {
            let preview: String = t.chars().take(80).collect();
            let ellipsis = if t.chars().count() > 80 { "…" } else { "" };
            lines.push(format!("  {}. {}{ellipsis}", i + 1, preview));
        }
        lines.push("Send next: empty Enter when idle · cancel: click a queue row".into());
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_dequeue_fifo() {
        let mut q = MessageQueue::new();
        assert_eq!(q.enqueue("a"), 1);
        assert_eq!(q.enqueue("b"), 2);
        assert_eq!(q.dequeue().as_deref(), Some("a"));
        assert_eq!(q.dequeue().as_deref(), Some("b"));
        assert!(q.dequeue().is_none());
    }

    #[test]
    fn drop_at_and_clear() {
        let mut q = MessageQueue::new();
        q.enqueue("a");
        q.enqueue("b");
        q.enqueue("c");
        assert_eq!(q.drop_at(2).as_deref(), Some("b"));
        assert_eq!(q.len(), 2);
        q.clear();
        assert!(q.is_empty());
    }
}
