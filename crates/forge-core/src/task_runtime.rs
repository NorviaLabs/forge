//! Private runtime handles for background task execution.
//!
//! The queue and background stores are session-owned here. `AgentSession`
//! exposes read-only accessors for the TUI, while the channels that drive
//! those stores remain private runtime state.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use forge_types::{BackgroundTaskId, HitlDecision};
use tokio::sync::mpsc::UnboundedSender;

use crate::background::{BackgroundTaskOutcome, BackgroundTaskRegistry};
use crate::TaskQueue;

pub(crate) struct TaskRuntime {
    pub(crate) queue: TaskQueue,
    pub(crate) background: BackgroundTaskRegistry,
    pub(crate) receivers:
        HashMap<BackgroundTaskId, std::sync::Mutex<Receiver<BackgroundTaskOutcome>>>,
    pub(crate) subagent_hitl_senders: HashMap<BackgroundTaskId, UnboundedSender<HitlDecision>>,
}

impl TaskRuntime {
    pub(crate) fn new() -> Self {
        Self {
            queue: TaskQueue::new(),
            background: BackgroundTaskRegistry::new(),
            receivers: HashMap::new(),
            subagent_hitl_senders: HashMap::new(),
        }
    }

    pub(crate) fn with_queue(queue: TaskQueue) -> Self {
        Self {
            queue,
            ..Self::new()
        }
    }
}
