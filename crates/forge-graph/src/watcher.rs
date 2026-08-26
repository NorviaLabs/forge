//! Live filesystem watching — keeps the graph current after the initial
//! build without ever blocking a caller on re-indexing.
//!
//! One `notify` watcher per `GraphHandle`, independent of forge-tui's own
//! (`crates/forge-tui/src/app/watch.rs`) and of `forge-search`'s (opaque,
//! inside the third-party `fff_search` crate) — deliberately not shared,
//! because this one has to run in headless sessions too, where the TUI's
//! watcher doesn't exist at all. See the design plan for why unifying the
//! three is an explicitly deferred follow-up, not this crate's job.
//!
//! Events are debounced per quiet period (not per path): a save that fires
//! several filesystem events in quick succession collapses to one
//! re-index, and a burst of saves across many files re-indexes each once
//! after the burst settles, not mid-burst.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::store::GraphStore;
use crate::{is_skipped_path, reindex_one_file};

const DEBOUNCE: Duration = Duration::from_millis(300);

/// Keeps the OS-level watch and the debounce task alive. Dropping it stops
/// both — the `notify` watcher unwatches on drop, and the background task
/// is aborted, which is how `GraphHandle::pause_watcher` works.
pub(crate) struct WatcherGuard {
    _watcher: RecommendedWatcher,
    task: JoinHandle<()>,
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts watching `workspace` and returns a guard keeping it alive.
/// `Err` only if the platform watcher itself fails to initialize (e.g. an
/// exhausted inotify instance limit) — `GraphHandle` treats that as "no
/// live updates for this session," not a fatal error, since the graph
/// still answers correctly from whatever the last build indexed.
pub(crate) fn start(workspace: PathBuf, store: Arc<GraphStore>) -> notify::Result<WatcherGuard> {
    let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();

    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else { return };
            if !matches!(
                event.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                return;
            }
            for path in event.paths {
                if is_skipped_path(&path) {
                    continue;
                }
                let _ = tx.send(path);
            }
        },
        Config::default(),
    )?;
    watcher.watch(&workspace, RecursiveMode::Recursive)?;

    let task = tokio::spawn(debounce_loop(rx_take(&mut rx), workspace, store));
    Ok(WatcherGuard {
        _watcher: watcher,
        task,
    })
}

// A tiny indirection so `debounce_loop` owns the receiver outright rather
// than borrowing it — keeps the `tokio::spawn` future `'static` without an
// extra `Option`/`mem::take` at the call site.
fn rx_take(rx: &mut mpsc::UnboundedReceiver<PathBuf>) -> mpsc::UnboundedReceiver<PathBuf> {
    std::mem::replace(rx, mpsc::unbounded_channel().1)
}

async fn debounce_loop(
    mut rx: mpsc::UnboundedReceiver<PathBuf>,
    workspace: PathBuf,
    store: Arc<GraphStore>,
) {
    let mut pending: HashSet<PathBuf> = HashSet::new();
    loop {
        let Some(first) = rx.recv().await else {
            return; // sender dropped — the watcher itself is gone
        };
        pending.insert(first);
        // Keep absorbing events, resetting the quiet-period timer on each
        // one, until nothing new arrives for a full `DEBOUNCE` window.
        loop {
            tokio::select! {
                maybe_path = rx.recv() => {
                    match maybe_path {
                        Some(p) => { pending.insert(p); }
                        None => break,
                    }
                }
                _ = tokio::time::sleep(DEBOUNCE) => break,
            }
        }
        for path in pending.drain() {
            // Best-effort: a failed re-index (a transient DB error, a file
            // that vanished between the event and this read) leaves that
            // one file's rows stale rather than crashing the watcher for
            // the rest of the session.
            let _ = reindex_one_file(&store, &workspace, &path).await;
        }
    }
}
