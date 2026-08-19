//! Isolation for work that must never occupy an application's async executor.

use std::future::Future;

/// Poll an async operation from Tokio's blocking pool.
///
/// `tokio::spawn` only schedules a future on another async worker. If that
/// future calls a synchronous provider SDK, tool implementation, filesystem
/// API, or subprocess wait, it can still starve the runtime that owns terminal
/// input and rendering. Polling it from the blocking pool makes the boundary
/// real even when the future is not cooperative.
pub struct IsolatedTask<T> {
    handle: Option<tokio::task::JoinHandle<Option<T>>>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl<T: Send + 'static> IsolatedTask<T> {
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Self {
        let runtime = tokio::runtime::Handle::current();
        let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
        let handle = tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
                tokio::select! {
                    result = future => Some(result),
                    _ = &mut cancelled => None,
                }
            })
        });
        Self {
            handle: Some(handle),
            cancel: Some(cancel),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
    }

    pub async fn join(mut self) -> Result<Option<T>, tokio::task::JoinError> {
        // Keep the sender alive while awaiting normal completion. Dropping it
        // early would select the cancellation branch inside the worker.
        let cancel = self.cancel.take();
        let result = self
            .handle
            .take()
            .expect("isolated task handle missing")
            .await;
        drop(cancel);
        result
    }

    pub fn abort(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(handle) = self.handle.as_ref() {
            // Effective before `spawn_blocking` starts. Once running, the
            // oneshot above drops the polled future at its next yield point.
            handle.abort();
        }
    }
}

impl<T> Drop for IsolatedTask<T> {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(handle) = self.handle.as_ref() {
            handle.abort();
        }
    }
}
