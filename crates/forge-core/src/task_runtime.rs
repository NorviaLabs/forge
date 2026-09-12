//! Private runtime handles for background task execution.
//!
//! The queue and background stores are session-owned here. `AgentSession`
//! exposes read-only accessors for the TUI, while the channels that drive
//! those stores remain private runtime state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use forge_types::{BackgroundTaskId, HitlDecision, SessionId};
use tokio::sync::mpsc::UnboundedSender;

use crate::background::{BackgroundControl, BackgroundTaskOutcome, BackgroundTaskRegistry};
use crate::TaskQueue;

pub(crate) struct TaskRuntime {
    pub(crate) queue: TaskQueue,
    pub(crate) background: BackgroundTaskRegistry,
    pub(crate) receivers:
        HashMap<BackgroundTaskId, std::sync::Mutex<Receiver<BackgroundTaskOutcome>>>,
    pub(crate) retained_subagents: HashMap<SessionId, RetainedSubagent>,
}

pub(crate) struct RetainedSubagent {
    pub(crate) label: String,
    pub(crate) workspace: PathBuf,
    pub(crate) result_sink: Arc<Mutex<Option<std::sync::mpsc::Sender<BackgroundTaskOutcome>>>>,
    pub(crate) hitl_sender: UnboundedSender<HitlDecision>,
    pub(crate) latest_message: Arc<Mutex<Option<String>>>,
}

impl TaskRuntime {
    pub(crate) fn new() -> Self {
        Self {
            queue: TaskQueue::new(),
            background: BackgroundTaskRegistry::new(),
            receivers: HashMap::new(),
            retained_subagents: HashMap::new(),
        }
    }

    /// Shared control handles for this session's background work, so an owner
    /// (the repository supervisor) can cancel a task or answer a subagent's
    /// approval while a foreground turn holds the session lock.
    pub(crate) fn background_control(&self) -> Arc<BackgroundControl> {
        self.background.control()
    }

    pub(crate) fn with_queue(queue: TaskQueue) -> Self {
        Self {
            queue,
            ..Self::new()
        }
    }
}
