//! Explicit FIFO queue of future-task instructions. A queue item is never
//! itself `Working`/`Completed`/etc. — only the task it gets promoted into
//! is. Owned by `AgentSession` (not the TUI) so promotion/journaling/
//! restoration all go through one store with direct `Journal` access.

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

    /// Visible queue depth — `Queued` + `Promoting` only. A `Promoted`/
    /// `Removed` item is gone from the visible count, matching "a queued
    /// item is never simultaneously visible as queued and active."
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

    /// Visible items (`Queued`/`Promoting`) in FIFO order. Redraws must call
    /// this rather than mutating the underlying deque — rendering never
    /// recreates or removes items.
    pub fn visible(&self) -> impl Iterator<Item = &QueuedTask> {
        self.items.iter().filter(|i| {
            matches!(
                i.status,
                QueueItemStatus::Queued | QueueItemStatus::Promoting
            )
        })
    }

    /// The oldest still-`Queued` item, if any — the promotion candidate.
    pub fn peek_next_queued(&self) -> Option<&QueuedTask> {
        self.items
            .iter()
            .find(|i| i.status == QueueItemStatus::Queued)
    }

    /// First step of atomic promotion: `Queued -> Promoting`. Returns
    /// `false` if the item isn't in a promotable state (already promoted,
    /// removed, or unknown id) — a queue item can be promoted at most once.
    pub fn mark_promoting(&mut self, id: QueueItemId) -> bool {
        self.transition_status(id, QueueItemStatus::Queued, QueueItemStatus::Promoting)
    }

    /// Final step of atomic promotion, called only after the task/attempt
    /// were actually created.
    pub fn mark_promoted(&mut self, id: QueueItemId) -> bool {
        let changed =
            self.transition_status(id, QueueItemStatus::Promoting, QueueItemStatus::Promoted);
        if changed {
            self.compact_terminal();
        }
        changed
    }

    /// Roll a `Promoting` item back to `Queued` — used when promotion fails
    /// partway (task creation errored) or on restore when a crash left the
    /// item stuck mid-promotion with no matching `Promoted`/`Removed`
    /// record. Failed promotion must never lose the item.
    pub fn revert_promoting(&mut self, id: QueueItemId) -> bool {
        self.transition_status(id, QueueItemStatus::Promoting, QueueItemStatus::Queued)
    }

    /// Explicit user cancellation of a still-queued item. Only legal while
    /// `Queued` — an item already `Promoting` cannot be cancelled out from
    /// under an in-flight promotion.
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
        self.items.retain(|item| {
            matches!(
                item.status,
                QueueItemStatus::Queued | QueueItemStatus::Promoting
            )
        });
    }

    /// Cancel by 1-based position in the *visible* list — matches the
    /// existing keyboard cancel-by-row UX.
    pub fn remove_at_visible_position(&mut self, one_based: usize) -> Option<QueuedTask> {
        let id = self.visible().nth(one_based.checked_sub(1)?)?.id;
        self.remove(id)
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

    /// Rebuild a queue from persisted items (session restore). Any item
    /// left `Promoting` — no matching `Promoted`/`Removed` event followed
    /// it — reconciles back to `Queued`, since a crash between "mark
    /// Promoting" and "mark Promoted" must not silently drop the
    /// instruction, and the task it may or may not have started is handled
    /// separately by task-lifecycle restoration, not by the queue.
    pub fn from_restored(items: Vec<QueuedTask>) -> Self {
        let next_id = items
            .iter()
            .map(|i| i.id.0)
            .max()
            .map(|m| m + 1)
            .unwrap_or(1);
        let mut items: VecDeque<QueuedTask> = items.into_iter().collect();
        for item in items.iter_mut() {
            if item.status == QueueItemStatus::Promoting {
                item.status = QueueItemStatus::Queued;
            }
        }
        Self {
            items,
            next_id,
            revision: 0,
        }
    }

    /// Preview lines for notices (index + truncated text) — same shape as
    /// the queue preview UI already renders.
    pub fn list_lines(&self) -> Vec<String> {
        let visible: Vec<&QueuedTask> = self.visible().collect();
        if visible.is_empty() {
            return vec!["Message queue is empty.".into()];
        }
        let mut lines = vec![format!("{} message(s) queued:", visible.len())];
        for (i, item) in visible.iter().enumerate() {
            let preview: String = item.text.chars().take(80).collect();
            let ellipsis = if item.text.chars().count() > 80 {
                "…"
            } else {
                ""
            };
            lines.push(format!("  {}. {}{ellipsis}", i + 1, preview));
        }
        lines.push("Send next: empty Enter when idle · cancel: click a queue row".into());
        lines
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
    fn item_remains_visible_until_promotion_or_removal() {
        let mut q = TaskQueue::new();
        let item = q.enqueue(sid(), "a");
        assert_eq!(q.len(), 1);
        assert!(q.mark_promoting(item.id));
        // Still visible while Promoting.
        assert_eq!(q.len(), 1);
        assert!(q.mark_promoted(item.id));
        // Gone from the visible list once Promoted.
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn promotion_removes_exactly_one_item() {
        let mut q = TaskQueue::new();
        let a = q.enqueue(sid(), "a");
        q.enqueue(sid(), "b");
        assert!(q.mark_promoting(a.id));
        assert!(q.mark_promoted(a.id));
        assert_eq!(q.len(), 1);
        let visible: Vec<_> = q.visible().map(|i| i.text.clone()).collect();
        assert_eq!(visible, vec!["b"]);
    }

    #[test]
    fn promotion_cannot_happen_twice() {
        let mut q = TaskQueue::new();
        let item = q.enqueue(sid(), "a");
        assert!(q.mark_promoting(item.id));
        assert!(q.mark_promoted(item.id));
        // Already Promoted — a second attempt must fail, not re-promote.
        assert!(!q.mark_promoting(item.id));
        assert!(!q.mark_promoted(item.id));
    }

    #[test]
    fn failed_promotion_preserves_the_item_via_revert() {
        let mut q = TaskQueue::new();
        let item = q.enqueue(sid(), "a");
        assert!(q.mark_promoting(item.id));
        assert!(q.revert_promoting(item.id));
        assert_eq!(q.len(), 1);
        assert_eq!(q.peek_next_queued().map(|i| i.id), Some(item.id));
    }

    #[test]
    fn remove_only_legal_while_queued() {
        let mut q = TaskQueue::new();
        let item = q.enqueue(sid(), "a");
        assert!(q.mark_promoting(item.id));
        // Cannot cancel an item mid-promotion.
        assert!(q.remove(item.id).is_none());
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn remove_at_visible_position_matches_keyboard_cancel_ux() {
        let mut q = TaskQueue::new();
        q.enqueue(sid(), "a");
        let b = q.enqueue(sid(), "b");
        q.enqueue(sid(), "c");
        let removed = q.remove_at_visible_position(2).unwrap();
        assert_eq!(removed.id, b.id);
        let visible: Vec<_> = q.visible().map(|i| i.text.clone()).collect();
        assert_eq!(visible, vec!["a", "c"]);
    }

    #[test]
    fn remove_at_visible_position_zero_or_out_of_range_is_none() {
        let mut q = TaskQueue::new();
        q.enqueue(sid(), "a");
        assert!(q.remove_at_visible_position(0).is_none());
        assert!(q.remove_at_visible_position(2).is_none());
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn restoration_reconciles_promoting_item_without_duplication() {
        let session = sid();
        let items = vec![
            QueuedTask {
                id: QueueItemId(1),
                session_id: session,
                text: "stuck-mid-promotion".into(),
                created_at: Utc::now(),
                status: QueueItemStatus::Promoting,
            },
            QueuedTask {
                id: QueueItemId(2),
                session_id: session,
                text: "already-promoted".into(),
                created_at: Utc::now(),
                status: QueueItemStatus::Promoted,
            },
            QueuedTask {
                id: QueueItemId(3),
                session_id: session,
                text: "still-queued".into(),
                created_at: Utc::now(),
                status: QueueItemStatus::Queued,
            },
        ];
        let q = TaskQueue::from_restored(items);
        // Exactly the Promoting-reconciled-to-Queued and the still-Queued
        // item are visible; the already-Promoted one is not duplicated in.
        let visible: Vec<_> = q.visible().map(|i| (i.id, i.text.clone())).collect();
        assert_eq!(
            visible,
            vec![
                (QueueItemId(1), "stuck-mid-promotion".to_string()),
                (QueueItemId(3), "still-queued".to_string()),
            ]
        );
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
    fn list_lines_formats_correctly() {
        let mut q = TaskQueue::new();
        assert!(q.list_lines()[0].contains("empty"));
        q.enqueue(sid(), "hello world");
        let lines = q.list_lines();
        assert!(lines[0].contains("1 message"));
        assert!(lines[1].contains("1. hello world"));
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
