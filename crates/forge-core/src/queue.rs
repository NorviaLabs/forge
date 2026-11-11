//! Explicit FIFO queue of future-task instructions, such as an approve-all
//! background result waiting for the next turn. Owned by `AgentSession` so
//! journaling and restoration go through one store with direct `Journal`
//! access. The supervisor moves queued items into its own prompt queue,
//! which is what runs them.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use forge_types::{QueueItemId, QueueItemStatus, SessionId};

#[derive(Debug, Clone)]
pub struct QueuedTask {
    pub id: QueueItemId,
    pub session_id: SessionId,
    pub text: String,
    pub created_at: DateTime<Utc>,
    pub status: QueueItemStatus,
}

/// FIFO queue of user instructions waiting to become a task. IDs are stable
/// and monotonic for the lifetime of the queue (including across restore,
/// via `from_restored`), so a rendered position never gets silently
/// reassigned to a different item.
#[derive(Debug, Clone, Default)]
pub struct TaskQueue {
    items: VecDeque<QueuedTask>,
    next_id: u64,
    revision: u64,
}

impl TaskQueue {
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
            next_id: 1,
            revision: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Number of items still queued.
    pub fn len(&self) -> usize {
        self.visible().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Add a new queued instruction at the back. Returns the stable item
    /// (with its assigned id) so the caller can journal/confirm it before
    /// telling the user it was queued.
    pub fn enqueue(&mut self, session_id: SessionId, text: impl Into<String>) -> QueuedTask {
        let id = QueueItemId(self.next_id);
        self.next_id += 1;
        let item = QueuedTask {
            id,
            session_id,
            text: text.into().trim().to_string(),
            created_at: Utc::now(),
            status: QueueItemStatus::Queued,
        };
        self.items.push_back(item.clone());
        self.revision += 1;
        item
    }

    /// Queued items in FIFO order.
    pub fn visible(&self) -> impl Iterator<Item = &QueuedTask> {
        self.items
            .iter()
            .filter(|i| i.status == QueueItemStatus::Queued)
    }

    /// Remove a still-queued item.
    pub fn remove(&mut self, id: QueueItemId) -> Option<QueuedTask> {
        if self.transition_status(id, QueueItemStatus::Queued, QueueItemStatus::Removed) {
            let removed = self.items.iter().find(|i| i.id == id).cloned();
            self.compact_terminal();
            removed
        } else {
            None
        }
    }

    fn compact_terminal(&mut self) {
        self.items
            .retain(|item| item.status == QueueItemStatus::Queued);
    }

    fn transition_status(
        &mut self,
        id: QueueItemId,
        expected: QueueItemStatus,
        next: QueueItemStatus,
    ) -> bool {
        if let Some(item) = self
            .items
            .iter_mut()
            .find(|i| i.id == id && i.status == expected)
        {
            item.status = next;
            self.revision += 1;
            true
        } else {
            false
        }
    }

    /// Rebuild a queue from persisted items (session restore). Removed items
    /// are dropped; ids continue after the highest one seen.
    pub fn from_restored(items: Vec<QueuedTask>) -> Self {
        let next_id = items
            .iter()
            .map(|i| i.id.0)
            .max()
            .map(|m| m + 1)
            .unwrap_or(1);
        let items: VecDeque<QueuedTask> = items
            .into_iter()
            .filter(|item| item.status == QueueItemStatus::Queued)
            .collect();
        Self {
            items,
            next_id,
            revision: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn sid() -> SessionId {
        Uuid::new_v4()
    }

    #[test]
    fn enqueue_assigns_stable_ids_in_fifo_order() {
        let mut q = TaskQueue::new();
        let a = q.enqueue(sid(), "a");
        let b = q.enqueue(sid(), "b");
        assert_eq!(a.id, QueueItemId(1));
        assert_eq!(b.id, QueueItemId(2));
        let visible: Vec<_> = q.visible().map(|i| i.text.clone()).collect();
        assert_eq!(visible, vec!["a", "b"]);
    }

    #[test]
    fn enqueue_trims_whitespace() {
        let mut q = TaskQueue::new();
        let item = q.enqueue(sid(), "  spaced  ");
        assert_eq!(item.text, "spaced");
    }

    #[test]
    fn remove_takes_the_item_out_and_only_once() {
        let mut q = TaskQueue::new();
        let a = q.enqueue(sid(), "a");
        q.enqueue(sid(), "b");
        assert_eq!(q.remove(a.id).map(|item| item.text), Some("a".into()));
        assert!(
            q.remove(a.id).is_none(),
            "a removed item cannot be removed again"
        );
        let visible: Vec<_> = q.visible().map(|i| i.text.clone()).collect();
        assert_eq!(visible, vec!["b"]);
    }

    #[test]
    fn restoration_keeps_only_queued_items() {
        let session = sid();
        let item = |id, text: &str, status| QueuedTask {
            id: QueueItemId(id),
            session_id: session,
            text: text.into(),
            created_at: Utc::now(),
            status,
        };
        let q = TaskQueue::from_restored(vec![
            item(1, "removed", QueueItemStatus::Removed),
            item(2, "queued", QueueItemStatus::Queued),
        ]);
        let visible: Vec<_> = q.visible().map(|i| (i.id, i.text.clone())).collect();
        assert_eq!(visible, vec![(QueueItemId(2), "queued".to_string())]);
    }

    #[test]
    fn restoration_continues_id_sequence_without_collision() {
        let items = vec![QueuedTask {
            id: QueueItemId(5),
            session_id: sid(),
            text: "x".into(),
            created_at: Utc::now(),
            status: QueueItemStatus::Queued,
        }];
        let mut q = TaskQueue::from_restored(items);
        let next = q.enqueue(sid(), "y");
        assert_eq!(next.id, QueueItemId(6));
    }

    #[test]
    fn deterministic_ordering_for_sequential_submissions() {
        let mut q = TaskQueue::new();
        for i in 0..5 {
            q.enqueue(sid(), format!("msg-{i}"));
        }
        let visible: Vec<_> = q.visible().map(|i| i.text.clone()).collect();
        assert_eq!(visible, vec!["msg-0", "msg-1", "msg-2", "msg-3", "msg-4"]);
    }
}
