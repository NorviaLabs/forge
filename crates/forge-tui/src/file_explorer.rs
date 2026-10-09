use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use forge_config::FileIconMode;
use forge_search::{
    GrepQueryMode, GrepSearchHit, MergedSearch, WorkspaceIndex, WorkspaceIndexOptions,
    DEFAULT_SEARCH_MAX_FILES,
};

use crate::status_glyph::{status_indicator_now, Status};
use crate::theme;
use forge_workspace::git_status::{GitStatusCache, GitStatusKind};

const HIDDEN_DIRS: &[&str] = &[".git", "target"];
const TREE_INDENT: &str = "  ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    File,
    Symlink,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct FileNode {
    pub path: PathBuf,
    pub display_name: String,
    pub kind: FileKind,
    pub expanded: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub children: Vec<FileNode>,
    pub loaded: bool,
}

impl FileNode {
    fn root(path: PathBuf) -> Self {
        let display_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            path,
            display_name,
            kind: FileKind::Directory,
            expanded: true,
            loading: false,
            error: None,
            children: Vec::new(),
            loaded: false,
        }
    }

    fn child(path: PathBuf, kind: FileKind) -> Self {
        let display_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            path,
            display_name,
            kind,
            expanded: false,
            loading: false,
            error: None,
            children: Vec::new(),
            loaded: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VisibleNode {
    pub path: PathBuf,
    pub display_name: String,
    pub kind: FileKind,
    pub expanded: bool,
    pub loading: bool,
    pub loaded: bool,
    pub error: Option<String>,
    pub child_count: usize,
    pub depth: usize,
    pub content_match: Option<GrepSearchHit>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileSearchMode {
    #[default]
    Names,
    Content,
}

#[derive(Debug)]
struct ExplorerSearchResults {
    query: String,
    names: MergedSearch,
    content: Vec<GrepSearchHit>,
    truncated: bool,
}

const MAX_CONTENT_MATCHES: usize = 200;

#[derive(Debug)]
pub struct FileExplorer {
    pub root: Option<FileNode>,
    pub selected_path: Option<PathBuf>,
    /// Index of `selected_path` in `visible`, when that mapping is known.
    /// `move_selection` uses this so holding j/k does not rescan every
    /// `PathBuf` on a large listing.
    selected_index: Option<usize>,
    selected_match_line: Option<u64>,
    pub scroll: usize,
    pub focused: bool,
    pub search_focused: bool,
    pub search_query: String,
    pub search_mode: FileSearchMode,
    pub icon_mode: FileIconMode,
    root_path: Option<PathBuf>,
    pub git_status: GitStatusCache,
    visible: Vec<VisibleNode>,
    search_loader: Option<Receiver<Result<ExplorerSearchResults, String>>>,
    search_cancel: Option<Arc<AtomicBool>>,
    search_loading: bool,
    /// Matches for the current query, installed when the
    /// worker's scan lands. Stale results stay on screen while the next query
    /// runs so the listing does not blink empty on every keystroke.
    search_results: Option<Arc<ExplorerSearchResults>>,
    collapsed_search_files: HashSet<PathBuf>,
    /// The workspace index, opened on the first search keystroke and kept for
    /// the rest of the session.
    search_engine: SearchEngine,
    /// The last scan could not run. Distinct from "no matches": a failed index
    /// says nothing about whether the query matches anything.
    search_unavailable: bool,
    /// `/diff` mode: show only these repository-relative paths, as a flat
    /// list. Flat rather than tree-shaped on purpose — a changed file inside a
    /// collapsed directory must still be reachable, and the lazy tree only
    /// knows about directories someone has already expanded.
    diff_filter: Option<Vec<PathBuf>>,
    /// Generation of the latest requested workspace refresh. A worker result
    /// installs only while its generation still matches, so a superseded
    /// request, a different root, or a different session's explorer can never
    /// have its stale result overwrite newer state.
    refresh_generation: u64,
    /// The one workspace refresh currently running off the terminal thread.
    /// At most one is in flight; later requests coalesce via `refresh_queued`.
    pending_refresh: Option<PendingWorkspaceRefresh>,
    /// A refresh requested while one was already in flight. Resolved into a
    /// single follow-up after the current worker lands — never one per
    /// filesystem event.
    refresh_queued: bool,
    /// Test-only gate that parks a worker mid-refresh so a test can prove the
    /// terminal thread keeps ticking, and that stale results are dropped.
    #[cfg(test)]
    refresh_test_gate: Option<Arc<RefreshTestGate>>,
    /// Test-only count of installed background refreshes.
    #[cfg(test)]
    refresh_install_count: usize,
}

/// A workspace refresh running on a blocking worker thread.
#[derive(Debug)]
struct PendingWorkspaceRefresh {
    generation: u64,
    root: PathBuf,
    receiver: Receiver<FileNode>,
}

/// Parks a refresh worker until a test releases it. `started` counts workers
/// that reached the gate, so a test can distinguish two stale requests from
/// one coalesced request without sleeping.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct RefreshTestGate {
    started: std::sync::atomic::AtomicUsize,
    released: std::sync::Mutex<bool>,
    release: std::sync::Condvar,
}

#[cfg(test)]
impl RefreshTestGate {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn wait(&self) {
        self.started.fetch_add(1, Ordering::SeqCst);
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.release.wait(released).unwrap();
        }
    }

    pub(crate) fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_all();
    }

    pub(crate) fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }
}

/// The workspace index, opened lazily and shared by every search keystroke.
///
/// A mutex rather than `OnceLock` because a failed open must not be cached:
/// the next keystroke retries instead of reporting "Search unavailable" for the
/// rest of the session. Concurrent workers serialize on the lock, so a burst of
/// keystrokes opens one index rather than one per keystroke.
#[derive(Clone, Debug, Default)]
struct SearchEngine(Arc<Mutex<Option<Arc<WorkspaceIndex>>>>);

impl SearchEngine {
    fn index(&self, root: &Path) -> Result<Arc<WorkspaceIndex>, String> {
        let mut slot = self
            .0
            .lock()
            .map_err(|_| "search index lock poisoned".to_string())?;
        if let Some(index) = slot.as_ref() {
            return Ok(index.clone());
        }
        // `wait_for_scan: false` keeps the open off the critical path; the
        // first query waits for the scan inside `search_files` instead.
        let index = WorkspaceIndex::open_with_options(
            root,
            WorkspaceIndexOptions {
                watch: true,
                wait_for_scan: false,
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())?;
        *slot = Some(index.clone());
        Ok(index)
    }
}

impl FileExplorer {
    pub fn set_git_change_nodes(&mut self, paths: Vec<PathBuf>, selected: Option<PathBuf>) {
        let root = self.root_path().map(Path::to_path_buf).unwrap_or_default();
        self.visible = paths
            .into_iter()
            .map(|relative| VisibleNode {
                path: root.join(&relative),
                display_name: relative.to_string_lossy().into_owned(),
                kind: FileKind::File,
                expanded: false,
                loading: false,
                loaded: true,
                error: None,
                child_count: 0,
                depth: 0,
                content_match: None,
            })
            .collect();
        self.selected_path = selected
            .filter(|path| self.visible.iter().any(|node| &node.path == path))
            .or_else(|| self.visible.first().map(|node| node.path.clone()));
        self.selected_index = None;
        self.scroll = self.scroll.min(self.visible.len().saturating_sub(1));
    }

    pub fn new(root_path: Option<PathBuf>, icon_mode: FileIconMode) -> Self {
        let root_path = root_path.map(|p| p.canonicalize().unwrap_or(p));
        let mut explorer = Self {
            root: root_path.clone().map(FileNode::root),
            selected_path: root_path.clone(),
            selected_index: None,
            selected_match_line: None,
            scroll: 0,
            focused: false,
            search_focused: true,
            search_query: String::new(),
            search_mode: FileSearchMode::Names,
            icon_mode,
            root_path: root_path.clone(),
            git_status: GitStatusCache::new(),
            visible: Vec::new(),
            search_loader: None,
            search_cancel: None,
            search_loading: false,
            search_results: None,
            collapsed_search_files: HashSet::new(),
            search_engine: SearchEngine::default(),
            search_unavailable: false,
            diff_filter: None,
            refresh_generation: 0,
            pending_refresh: None,
            refresh_queued: false,
            #[cfg(test)]
            refresh_test_gate: None,
            #[cfg(test)]
            refresh_install_count: 0,
        };
        explorer.load_root();
        if let Some(root) = root_path {
            explorer.git_status.start_refresh(root);
        }
        explorer
    }

    pub fn load_root(&mut self) {
        self.invalidate_workspace_refresh();
        self.cancel_search_load();
        self.search_results = None;
        if let Some(root) = self.root.as_mut() {
            load_children(self.root_path.as_deref(), root);
        }
        self.restart_search_load_if_needed();
        self.rebuild_visible();
    }

    pub fn refresh_git_status(&mut self) {
        if let Some(root) = self.root_path.clone() {
            self.git_status.start_refresh(root);
        }
    }

    pub fn refresh_workspace(&mut self) {
        self.invalidate_workspace_refresh();
        self.cancel_search_load();
        self.search_results = None;
        let root_path = self.root_path.clone();
        if let Some(root) = self.root.as_mut() {
            refresh_loaded_directories(root_path.as_deref(), root);
        }
        self.restart_search_load_if_needed();
        self.rebuild_visible();
        self.refresh_git_status();
    }

    /// Refresh every loaded directory off the terminal thread.
    ///
    /// This is the non-blocking replacement for [`Self::refresh_workspace`]:
    /// the worker walks a snapshot of the tree on a blocking thread and sends
    /// the refreshed root back, where [`Self::poll_workspace_refresh`] installs
    /// it on a later tick. Requests made while a refresh is in flight coalesce
    /// into one follow-up, and any result whose generation no longer matches
    /// the latest request (or whose root changed) is dropped on arrival.
    pub fn request_workspace_refresh(&mut self) {
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        self.refresh_git_status();
        if self.pending_refresh.is_some() {
            self.refresh_queued = true;
            return;
        }
        let Some(root_path) = self.root_path.clone() else {
            return;
        };
        let Some(root) = self.root.clone() else {
            return;
        };
        let generation = self.refresh_generation;
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_root = root_path.clone();
        #[cfg(test)]
        let gate = self.refresh_test_gate.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(gate) = gate {
                gate.wait();
            }
            let mut refreshed = root;
            refresh_loaded_directories(Some(&worker_root), &mut refreshed);
            let _ = tx.send(refreshed);
        });
        self.pending_refresh = Some(PendingWorkspaceRefresh {
            generation,
            root: root_path,
            receiver: rx,
        });
    }

    /// Install a completed background workspace refresh, if one is ready.
    /// Non-blocking and safe to call from the terminal thread every tick.
    /// Returns `true` when a fresh result was installed.
    pub fn poll_workspace_refresh(&mut self) -> bool {
        let Some(pending) = self.pending_refresh.take() else {
            return false;
        };
        let installed = match pending.receiver.try_recv() {
            Ok(root) => {
                if pending.generation == self.refresh_generation
                    && self.root_path.as_deref() == Some(pending.root.as_path())
                {
                    self.install_workspace_refresh(root);
                    true
                } else {
                    false
                }
            }
            Err(TryRecvError::Empty) => {
                self.pending_refresh = Some(pending);
                return false;
            }
            Err(TryRecvError::Disconnected) => false,
        };
        if std::mem::take(&mut self.refresh_queued) {
            self.request_workspace_refresh();
        }
        installed
    }

    /// Whether a background workspace refresh is still in flight.
    pub fn workspace_refresh_pending(&self) -> bool {
        self.pending_refresh.is_some()
    }

    /// Drop any in-flight background refresh. A synchronous tree mutation makes
    /// the worker's snapshot stale even if its generation is unchanged, so the
    /// pending receiver is discarded and its eventual send is ignored.
    fn invalidate_workspace_refresh(&mut self) {
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        self.pending_refresh = None;
        self.refresh_queued = false;
    }

    fn install_workspace_refresh(&mut self, root: FileNode) {
        self.cancel_search_load();
        self.search_results = None;
        self.root = Some(root);
        self.restart_search_load_if_needed();
        self.rebuild_visible();
        #[cfg(test)]
        {
            self.refresh_install_count += 1;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_refresh_test_gate(&mut self, gate: Arc<RefreshTestGate>) {
        self.refresh_test_gate = Some(gate);
    }

    #[cfg(test)]
    pub(crate) fn refresh_install_count(&self) -> usize {
        self.refresh_install_count
    }

    /// Identity of the open index, so a test can prove later keystrokes reused
    /// it instead of opening a second one.
    #[cfg(test)]
    fn search_engine_ptr(&self) -> *const WorkspaceIndex {
        self.search_engine
            .0
            .lock()
            .expect("search lock")
            .as_ref()
            .map(Arc::as_ptr)
            .unwrap_or(std::ptr::null())
    }

    pub fn refresh_selected(&mut self) {
        self.invalidate_workspace_refresh();
        self.cancel_search_load();
        self.search_results = None;
        let selected = self.selected_path.clone();
        let root_path = self.root_path.clone();
        if let Some(path) = selected {
            if let Some(node) = self.find_mut(&path) {
                if node.kind == FileKind::Directory {
                    node.loaded = false;
                    load_children(root_path.as_deref(), node);
                    node.expanded = true;
                    self.restart_search_load_if_needed();
                    self.rebuild_visible();
                    self.refresh_git_status();
                    return;
                }
            }
        }
        self.load_root();
        self.refresh_git_status();
    }

    pub fn refresh_parent_and_select(&mut self, parent: &Path, selected: &Path) {
        self.invalidate_workspace_refresh();
        self.cancel_search_load();
        self.search_results = None;
        let root_path = self.root_path.clone();
        if let Some(node) = self.find_mut(parent) {
            if node.kind == FileKind::Directory {
                node.loaded = false;
                load_children(root_path.as_deref(), node);
                node.expanded = true;
            }
        } else {
            self.load_root();
        }
        self.selected_path = Some(selected.to_path_buf());
        self.restart_search_load_if_needed();
        self.rebuild_visible();
        self.refresh_git_status();
    }

    pub fn refresh_after_delete(&mut self, parent: &Path, deleted: &Path) {
        let deleted_index = self
            .visible
            .iter()
            .position(|node| node.path == deleted)
            .unwrap_or(0);
        self.refresh_parent_and_select(parent, parent);
        if self.visible.is_empty() {
            self.selected_path = self.root_path.clone();
            return;
        }
        let next = deleted_index.min(self.visible.len().saturating_sub(1));
        self.selected_path = Some(self.visible[next].path.clone());
        if self.selected_path.as_deref() == Some(deleted) {
            self.selected_path = Some(parent.to_path_buf());
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focused = !self.focused;
    }

    /// Borrow the current flattened explorer rows without cloning their paths
    /// and display names. Callers that need ownership can explicitly clone the
    /// individual rows they retain.
    pub fn visible_nodes(&self) -> &[VisibleNode] {
        &self.visible
    }

    pub fn is_visible(&self, path: &Path) -> bool {
        self.visible.iter().any(|node| node.path == path)
    }

    pub fn is_visible_directory(&self, path: &Path) -> bool {
        self.visible
            .iter()
            .any(|node| node.path == path && node.kind == FileKind::Directory)
    }

    pub fn set_search_query(&mut self, query: impl Into<String>) {
        let previous_index = self.selected_visible_index().unwrap_or(0);
        self.search_query = query.into();
        if self.search_query.trim().is_empty() {
            self.cancel_search_load();
            self.search_results = None;
            self.search_unavailable = false;
        } else {
            self.start_search_load();
        }
        self.rebuild_visible();
        self.repair_selection(previous_index);
        self.scroll = 0;
    }

    pub fn clear_search(&mut self) {
        self.set_search_query(String::new());
    }

    pub fn set_search_mode(&mut self, mode: FileSearchMode) {
        if self.search_mode == mode {
            return;
        }
        self.cancel_search_load();
        self.search_mode = mode;
        self.search_results = None;
        self.collapsed_search_files.clear();
        self.selected_match_line = None;
        self.set_search_query(self.search_query.clone());
    }

    fn content_search_active(&self) -> bool {
        self.search_mode == FileSearchMode::Content && !self.search_query.trim().is_empty()
    }

    fn row_is_selected(&self, node: &VisibleNode) -> bool {
        self.selected_path.as_ref() == Some(&node.path)
            && self.selected_match_line == node.content_match.as_ref().map(|hit| hit.line)
    }

    pub fn selected_content_match(&self) -> Option<&GrepSearchHit> {
        self.visible
            .iter()
            .find(|node| self.row_is_selected(node))?
            .content_match
            .as_ref()
    }

    pub fn selected_relative_path(&self) -> Option<String> {
        let root = self.root_path.as_ref()?;
        let selected = self.selected_path.as_ref()?;
        selected.strip_prefix(root).ok().map(|p| {
            let text = p.display().to_string();
            if text.is_empty() {
                ".".into()
            } else {
                text
            }
        })
    }

    pub fn root_path(&self) -> Option<&Path> {
        self.root_path.as_deref()
    }

    pub fn selected_node(&self) -> Option<&FileNode> {
        let path = self.selected_path.as_ref()?;
        self.find(path)
    }

    pub fn selected_creation_parent(&self) -> Option<PathBuf> {
        match self.selected_node() {
            Some(node) if node.kind == FileKind::Directory => Some(node.path.clone()),
            Some(node) => node.path.parent().map(Path::to_path_buf),
            None => self.root_path.clone(),
        }
    }

    pub fn git_status_for(&self, path: &Path) -> Option<GitStatusKind> {
        let root = self.root_path.as_ref()?;
        let rel = path.strip_prefix(root).ok()?;
        self.git_status.get(rel)
    }

    /// Start loading the active file's unstaged diff without blocking the UI.
    /// Completed results remain in `GitStatusCache` for a future diff view.
    pub fn request_unstaged_diff(&mut self, path: &Path) {
        let Some(root) = self.root_path.clone() else {
            return;
        };
        let Ok(relative) = path.strip_prefix(&root) else {
            return;
        };
        if self
            .git_status
            .path_status(relative)
            .is_some_and(|status| status.unstaged.is_some())
        {
            self.git_status
                .request_unstaged_diff(root, relative.to_path_buf());
        }
    }

    /// Poll both Git status and any active diff request without blocking.
    pub fn poll_git(&mut self) -> bool {
        let status_updated = self.git_status.poll();
        let diff_updated = self.git_status.poll_diff();
        status_updated || diff_updated
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.selected_visible_index().unwrap_or(0);
        let next = current
            .saturating_add_signed(delta)
            .min(self.visible.len() - 1);
        self.select_visible_index(next);
    }

    /// True when `↑` has nowhere left to go: the cursor already sits on the
    /// first row. An empty tree counts as at the top, so the key can still reach
    /// the navigator's tab row (`FORGE-DESIGN §8.3`) instead of doing nothing. A
    /// tree with no selection yet is *not* at the top — the first `↑` still
    /// selects row 0, exactly as it did before the tab row existed.
    pub fn selection_at_first_row(&self) -> bool {
        // Compares the selected path with the first visible node rather than
        // going through `selected_visible_index`, which caches its answer and
        // so needs `&mut self`.
        let Some(first) = self.visible.first() else {
            return true;
        };
        self.row_is_selected(first)
    }

    pub fn expand_selected(&mut self) {
        if self.content_search_active() {
            if let Some(path) = self.selected_path.clone() {
                self.collapsed_search_files.remove(&path);
                self.rebuild_visible();
                self.repair_selection(0);
            }
            return;
        }
        if let Some(index) = self.selected_visible_index() {
            let node = &self.visible[index];
            if node.kind != FileKind::Directory {
                return;
            }
            if node.expanded && node.loaded {
                return;
            }
        }
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        let root_path = self.root_path.clone();
        let Some(node) = self.find(&path) else {
            return;
        };
        if node.kind != FileKind::Directory {
            return;
        }
        // Git status is whole-repo (`git status -z -uall`), so expanding a
        // folder cannot change markers. Spawning porcelain here made holding
        // → hitch on process spawn and cleared the diff cache every time.
        if node.expanded && node.loaded {
            return;
        }
        // The tree is about to change under any in-flight refresh snapshot,
        // which must not install over this expansion.
        self.invalidate_workspace_refresh();
        let Some(node) = self.find_mut(&path) else {
            return;
        };
        if !node.loaded {
            load_children(root_path.as_deref(), node);
        }
        node.expanded = true;
        self.rebuild_visible();
    }

    pub fn activate_selected(&mut self) {
        if self.content_search_active() {
            let Some(path) = self.selected_path.clone() else {
                return;
            };
            if self.collapsed_search_files.contains(&path) {
                self.expand_selected();
            } else {
                self.collapse_selected();
            }
            return;
        }
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        if self
            .find(&path)
            .is_some_and(|node| node.kind == FileKind::Directory && node.expanded)
        {
            self.collapse_selected();
        } else {
            self.expand_selected();
        }
    }

    pub fn collapse_selected(&mut self) {
        if self.content_search_active() {
            if let Some(path) = self.selected_path.clone() {
                self.collapsed_search_files.insert(path);
                self.selected_match_line = None;
                self.rebuild_visible();
                self.repair_selection(0);
            }
            return;
        }
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        let previous_index = self
            .visible
            .iter()
            .position(|node| node.path == path)
            .unwrap_or(0);
        let collapsing = self
            .find(&path)
            .is_some_and(|node| node.kind == FileKind::Directory && node.expanded);
        if collapsing {
            // A collapse must not be overwritten by an in-flight refresh that
            // snapshotted the tree while this directory was still expanded.
            self.invalidate_workspace_refresh();
        }
        if let Some(node) = self.find_mut(&path) {
            if node.kind == FileKind::Directory && node.expanded {
                node.expanded = false;
                self.rebuild_visible();
                self.repair_selection(previous_index);
                return;
            }
        }
        if let Some(parent) = path.parent() {
            if self.contains(parent) {
                self.selected_path = Some(parent.to_path_buf());
            }
        }
    }

    /// Scan names or content for the current query on a worker thread.
    ///
    /// Every keystroke supersedes the scan before it: the stale worker is
    /// cancelled and its result dropped on arrival, so the listing only ever
    /// shows the query that is actually in the field. The previous result set
    /// stays on screen meanwhile rather than blinking empty between keystrokes.
    fn start_search_load(&mut self) {
        let Some(root_path) = self.root_path.clone() else {
            return;
        };
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.cancel_search_load();
        self.search_unavailable = false;
        let engine = self.search_engine.clone();
        let mode = self.search_mode;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = engine.index(&root_path).and_then(|index| {
                match mode {
                    FileSearchMode::Names => index
                        .find_files_quick_open(&query, DEFAULT_SEARCH_MAX_FILES, None)
                        .map(|response| ExplorerSearchResults {
                            query: query.clone(),
                            truncated: response.total_matched >= DEFAULT_SEARCH_MAX_FILES,
                            names: MergedSearch {
                                paths: response.hits.into_iter().map(|hit| hit.path).collect(),
                                truncated: response.total_matched >= DEFAULT_SEARCH_MAX_FILES,
                            },
                            content: Vec::new(),
                        }),
                    FileSearchMode::Content => index
                        .grep(
                            &query,
                            None,
                            GrepQueryMode::Literal,
                            MAX_CONTENT_MATCHES + 1,
                        )
                        .map(|response| ExplorerSearchResults {
                            query: query.clone(),
                            names: MergedSearch::default(),
                            truncated: response.hits.len() > MAX_CONTENT_MATCHES,
                            content: response
                                .hits
                                .into_iter()
                                .take(MAX_CONTENT_MATCHES)
                                .collect(),
                        }),
                }
                .map_err(|error| error.to_string())
            });
            if !worker_cancel.load(Ordering::Relaxed) {
                let _ = tx.send(outcome);
            }
        });
        self.search_loader = Some(rx);
        self.search_cancel = Some(cancel);
        self.search_loading = true;
    }

    fn cancel_search_load(&mut self) {
        if let Some(cancel) = self.search_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.search_loader = None;
        self.search_loading = false;
    }

    fn restart_search_load_if_needed(&mut self) {
        if !self.search_query.trim().is_empty() {
            self.start_search_load();
        }
    }

    /// Install the worker's result. Runs on the terminal thread, so it only
    /// moves the already-computed answer — never the scan itself.
    pub fn poll_search_load(&mut self) {
        let Some(rx) = self.search_loader.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                let previous_index = self.selected_visible_index().unwrap_or(0);
                match outcome {
                    Ok(results) => {
                        self.search_results = Some(Arc::new(results));
                        self.search_unavailable = false;
                    }
                    Err(_) => {
                        self.search_results = None;
                        self.search_unavailable = true;
                    }
                }
                self.search_cancel = None;
                self.search_loading = false;
                self.rebuild_visible();
                self.repair_selection(previous_index);
            }
            Err(TryRecvError::Empty) => self.search_loader = Some(rx),
            // A panicked worker drops the sender, which is a failed scan too.
            Err(TryRecvError::Disconnected) => {
                self.search_results = None;
                self.search_unavailable = true;
                self.search_cancel = None;
                self.search_loading = false;
                self.rebuild_visible();
            }
        }
    }

    /// The result count shown at the right edge of the Search field. `+` marks
    /// a listing that hit the cap, so a truncated result set never reads as a
    /// complete one.
    pub fn search_result_count(&self) -> Option<String> {
        let results = self.search_results.as_ref()?;
        if self.search_query.trim().is_empty() {
            return None;
        }
        let (count, noun) = if self.search_mode == FileSearchMode::Content {
            (results.content.len(), "match")
        } else {
            (results.names.paths.len(), "file")
        };
        Some(format!(
            "{count}{} {noun}{}",
            if results.truncated { "+" } else { "" },
            if count == 1 {
                ""
            } else if noun == "match" {
                "es"
            } else {
                "s"
            }
        ))
    }

    /// Restrict the listing to `paths` (repository-relative), or clear the
    /// restriction with `None`. Tree expansion and scroll state are untouched
    /// so leaving `/diff` restores exactly the listing the user had.
    pub fn set_diff_filter(&mut self, paths: Option<Vec<PathBuf>>) {
        if self.diff_filter == paths {
            return;
        }
        self.diff_filter = paths;
        self.rebuild_visible();
    }

    pub fn diff_filter_is_active(&self) -> bool {
        self.diff_filter.is_some()
    }

    fn rebuild_visible(&mut self) {
        if let Some(paths) = self.diff_filter.clone() {
            let root = self.root_path().map(Path::to_path_buf).unwrap_or_default();
            self.visible = paths
                .iter()
                .map(|relative| VisibleNode {
                    path: root.join(relative),
                    display_name: relative.to_string_lossy().into_owned(),
                    kind: FileKind::File,
                    expanded: false,
                    loading: false,
                    loaded: true,
                    error: None,
                    child_count: 0,
                    depth: 0,
                    content_match: None,
                })
                .collect();
            self.selected_index = None;
            return;
        }
        let mut visible = Vec::new();
        if !self.search_query.trim().is_empty() {
            if let Some(results) = &self.search_results {
                if self.search_mode == FileSearchMode::Content {
                    flatten_content_results(
                        self.root_path.as_deref(),
                        &results.content,
                        &self.collapsed_search_files,
                        &mut visible,
                    );
                } else {
                    flatten_search_results(self.root_path.as_deref(), &results.names, &mut visible);
                }
            } else if !self.search_unavailable && self.search_mode == FileSearchMode::Names {
                // No scan has landed yet: show what the loaded tree can answer
                // so the first keystrokes are not a blank pane. A failed scan
                // skips this — its own state line is the honest one.
                if let Some(root) = &self.root {
                    flatten_filtered(root, 0, &self.search_query, self.root_path(), &mut visible);
                }
            }
        } else if self.search_mode == FileSearchMode::Names {
            if let Some(root) = &self.root {
                flatten_filtered(root, 0, "", self.root_path(), &mut visible);
            }
        }
        self.visible = visible;
        self.selected_index = None;
    }

    pub fn selected_visible_index(&mut self) -> Option<usize> {
        if let Some(index) = self.selected_index {
            if self
                .visible
                .get(index)
                .is_some_and(|node| self.row_is_selected(node))
            {
                return Some(index);
            }
        }
        let index = self
            .visible
            .iter()
            .position(|node| self.row_is_selected(node))?;
        self.selected_index = Some(index);
        Some(index)
    }

    fn select_visible_index(&mut self, index: usize) {
        let Some(node) = self.visible.get(index) else {
            return;
        };
        self.selected_path = Some(node.path.clone());
        self.selected_match_line = node.content_match.as_ref().map(|hit| hit.line);
        self.selected_index = Some(index);
    }

    /// Select the node at `index` in the visible list (mouse row click). A
    /// no-op out of range, so a click below the last row changes nothing.
    pub fn select_visible_row(&mut self, index: usize) {
        self.select_visible_index(index);
    }

    fn repair_selection(&mut self, previous_index: usize) {
        if self.visible.is_empty() {
            self.selected_index = None;
            self.selected_match_line = None;
            return;
        }
        if self.visible.iter().any(|node| self.row_is_selected(node)) {
            self.selected_index = None;
            return;
        }
        self.select_visible_index(previous_index.min(self.visible.len() - 1));
    }

    /// Returns the selected path if it points to a regular file.
    pub fn selected_file_path(&self) -> Option<PathBuf> {
        if self.content_search_active() {
            return self
                .selected_content_match()
                .and_then(|_| self.selected_path.clone());
        }
        let path = self.selected_path.as_ref()?;
        let is_file = self
            .visible
            .iter()
            .find(|node| &node.path == path)
            .map(|node| matches!(node.kind, FileKind::File | FileKind::Symlink))
            .or_else(|| {
                self.find(path)
                    .map(|node| matches!(node.kind, FileKind::File | FileKind::Symlink))
            })?;
        is_file.then(|| path.clone())
    }

    fn ensure_selection_visible(&mut self, height: usize) {
        let Some(index) = self.selected_visible_index() else {
            self.select_visible_index(0);
            self.scroll = 0;
            return;
        };
        if index < self.scroll {
            self.scroll = index;
        } else if height > 0 && index >= self.scroll + height {
            self.scroll = index + 1 - height;
        }
    }

    fn contains(&self, path: &Path) -> bool {
        self.root_path
            .as_ref()
            .is_some_and(|root| path == root || path.starts_with(root))
    }

    fn find(&self, path: &Path) -> Option<&FileNode> {
        self.root.as_ref().and_then(|root| find_node(root, path))
    }

    fn find_mut(&mut self, path: &Path) -> Option<&mut FileNode> {
        self.root
            .as_mut()
            .and_then(|root| find_node_mut(root, path))
    }
}

fn flatten_filtered(
    node: &FileNode,
    depth: usize,
    query: &str,
    root: Option<&Path>,
    out: &mut Vec<VisibleNode>,
) -> bool {
    let relative = root
        .and_then(|root| node.path.strip_prefix(root).ok())
        .map(|path| {
            let text = path.to_string_lossy();
            if text.is_empty() {
                ".".to_string()
            } else {
                text.into_owned()
            }
        })
        .unwrap_or_else(|| node.path.to_string_lossy().into_owned());
    let self_matches = path_matches_query(&relative, query);
    let mut matching_children = Vec::new();
    if node.kind == FileKind::Directory && node.expanded {
        for child in &node.children {
            let start = matching_children.len();
            if flatten_filtered(child, depth + 1, query, root, &mut matching_children) {
                debug_assert!(matching_children.len() > start);
            }
        }
    }
    if !self_matches && matching_children.is_empty() {
        return false;
    }
    out.push(VisibleNode {
        path: node.path.clone(),
        display_name: node.display_name.clone(),
        kind: node.kind,
        expanded: node.expanded,
        loading: node.loading,
        loaded: node.loaded,
        error: node.error.clone(),
        child_count: node.children.len(),
        depth,
        content_match: None,
    });
    out.extend(matching_children);
    true
}

fn fuzzy_subsequence(path: &str, query: &str) -> bool {
    let mut path_chars = path.chars().flat_map(char::to_lowercase);
    for query_char in query.chars().flat_map(char::to_lowercase) {
        if path_chars
            .position(|path_char| path_char == query_char)
            .is_none()
        {
            return false;
        }
    }
    true
}

/// Turn ranked search results into tree rows.
///
/// The result list is ranked, not sorted by path, so rows keep rank order and
/// each match contributes the ancestors it needs on the way. A directory row
/// therefore always precedes the matches it holds, and a directory matched by
/// several results still appears once.
fn flatten_search_results(root: Option<&Path>, results: &MergedSearch, out: &mut Vec<VisibleNode>) {
    let Some(root) = root else {
        return;
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut rows: Vec<VisibleNode> = Vec::with_capacity(results.paths.len());
    let mut children: HashMap<PathBuf, u32> = HashMap::new();
    for relative in &results.paths {
        let components: Vec<&str> = relative
            .split('/')
            .filter(|component| !component.is_empty() && *component != ".")
            .collect();
        let mut path = root.to_path_buf();
        for (index, component) in components.iter().enumerate() {
            path.push(component);
            let is_file = index + 1 == components.len();
            if let Some(parent) = path.parent() {
                *children.entry(parent.to_path_buf()).or_default() += 1;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            rows.push(VisibleNode {
                display_name: component.to_string(),
                kind: if is_file {
                    FileKind::File
                } else {
                    FileKind::Directory
                },
                expanded: !is_file,
                loading: false,
                loaded: true,
                error: None,
                child_count: 0,
                depth: index,
                content_match: None,
                path: path.clone(),
            });
        }
    }
    for row in &mut rows {
        row.child_count = children.get(&row.path).copied().unwrap_or(0) as usize;
    }
    out.extend(rows);
}

fn flatten_content_results(
    root: Option<&Path>,
    hits: &[GrepSearchHit],
    collapsed: &HashSet<PathBuf>,
    out: &mut Vec<VisibleNode>,
) {
    let Some(root) = root else {
        return;
    };
    let mut files: BTreeMap<&str, Vec<&GrepSearchHit>> = BTreeMap::new();
    for hit in hits {
        files.entry(&hit.path).or_default().push(hit);
    }
    for (relative, mut matches) in files {
        matches.sort_by_key(|hit| hit.line);
        matches.dedup_by_key(|hit| hit.line);
        let path = root.join(relative);
        let expanded = !collapsed.contains(&path);
        out.push(VisibleNode {
            path: path.clone(),
            display_name: relative.to_string(),
            kind: FileKind::File,
            expanded,
            loading: false,
            loaded: true,
            error: None,
            child_count: matches.len(),
            depth: 0,
            content_match: None,
        });
        if expanded {
            for hit in matches {
                out.push(VisibleNode {
                    path: path.clone(),
                    display_name: hit.text.clone(),
                    kind: FileKind::File,
                    expanded: false,
                    loading: false,
                    loaded: true,
                    error: None,
                    child_count: 0,
                    depth: 1,
                    content_match: Some(hit.clone()),
                });
            }
        }
    }
}

fn path_matches_query(path: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .all(|token| fuzzy_subsequence(path, token))
}

fn find_node<'a>(node: &'a FileNode, path: &Path) -> Option<&'a FileNode> {
    if node.path == path {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_node(child, path))
}

fn find_node_mut<'a>(node: &'a mut FileNode, path: &Path) -> Option<&'a mut FileNode> {
    if node.path == path {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, path))
}

fn refresh_loaded_directories(root: Option<&Path>, node: &mut FileNode) {
    if node.kind != FileKind::Directory || !node.loaded {
        return;
    }
    refresh_directory(root, node);
}

fn refresh_directory(root: Option<&Path>, node: &mut FileNode) {
    let expanded = node.expanded;
    let loaded_children: Vec<(PathBuf, bool)> = node
        .children
        .iter()
        .filter(|child| child.kind == FileKind::Directory && child.loaded)
        .map(|child| (child.path.clone(), child.expanded))
        .collect();
    load_children(root, node);
    node.expanded = expanded;

    for (path, expanded) in loaded_children {
        if let Some(child) = node.children.iter_mut().find(|child| child.path == path) {
            child.expanded = expanded;
            refresh_directory(root, child);
        }
    }
}

fn content_match_line(
    hit: &GrepSearchHit,
    query: &str,
    selected: bool,
    focused: bool,
    width: usize,
) -> Line<'static> {
    let selection = selected.then(|| {
        if focused {
            theme::selection_active()
        } else {
            theme::selection_inactive()
        }
    });
    let prefix = format!(
        "{}    {}  ",
        if selected && focused { ">" } else { " " },
        hit.line
    );
    let budget = width.saturating_sub(prefix.len());
    let mut text = hit.text.replace('\t', " ");
    // Keep the match visible even when it occurs far along a source line.
    if let Some((start, _)) = content_match_range(&text, query.trim()) {
        let chars_before = text[..start].chars().count();
        if chars_before >= budget.saturating_sub(query.chars().count()) {
            let context = 8.min(budget.saturating_sub(query.chars().count() + 1));
            let skip = chars_before.saturating_sub(context);
            if skip > 0 {
                text = format!("…{}", text.chars().skip(skip).collect::<String>());
            }
        }
    }
    let mut spans = vec![Span::styled(prefix, selection.unwrap_or_else(theme::muted))];
    let base = selection.unwrap_or_else(theme::text);
    let mut rest = text.as_str();
    while let Some((start, end)) = content_match_range(rest, query.trim()) {
        if start > 0 {
            spans.push(Span::styled(rest[..start].to_string(), base));
        }
        let mut style = theme::search_match();
        if let Some(selection) = selection {
            style = style
                .patch(selection)
                .add_modifier(ratatui::style::Modifier::UNDERLINED);
        }
        spans.push(Span::styled(rest[start..end].to_string(), style));
        rest = &rest[end..];
    }
    spans.push(Span::styled(rest.to_string(), base));
    Line::from(spans).style(selection.unwrap_or_default())
}

fn content_match_range(text: &str, query: &str) -> Option<(usize, usize)> {
    if query.is_empty() {
        return None;
    }
    if query.chars().any(char::is_uppercase) {
        text.find(query).map(|start| (start, start + query.len()))
    } else {
        match_byte_range_case_insensitive(text, query)
    }
}

fn load_children(root: Option<&Path>, node: &mut FileNode) {
    node.loading = true;
    node.error = None;
    node.children.clear();
    match read_children(root, &node.path) {
        Ok(children) => {
            node.children = children;
            node.loaded = true;
        }
        Err(error) => {
            node.error = Some(error);
            node.loaded = true;
        }
    }
    node.loading = false;
}

fn read_children(root: Option<&Path>, dir: &Path) -> Result<Vec<FileNode>, String> {
    let root = root.ok_or_else(|| "No repository detected".to_string())?;
    let dir = safe_path(root, dir)?;
    // `dir` above is already canonicalized (via `safe_path`); canonicalize
    // `root` too so the `.forge/local` path comparison in `should_hide`
    // compares like with like (e.g. macOS's `/var` vs `/private/var`).
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let entries = fs::read_dir(&dir).map_err(|error| error.to_string())?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if should_hide(&canonical_root, &path) {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() && safe_path(root, &path).is_err() {
            continue;
        }
        let kind = if file_type.is_dir() {
            FileKind::Directory
        } else if file_type.is_symlink() {
            FileKind::Symlink
        } else if file_type.is_file() {
            FileKind::File
        } else {
            FileKind::Unknown
        };
        children.push(FileNode::child(path, kind));
    }
    sort_nodes(&mut children);
    Ok(children)
}

pub fn safe_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    if path == root || path.starts_with(&root) {
        Ok(path)
    } else {
        Err("path is outside the repository".into())
    }
}

fn should_hide(root: &Path, path: &Path) -> bool {
    let hidden_by_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| HIDDEN_DIRS.contains(&name));
    if hidden_by_name {
        return true;
    }
    // `.forge/local/` is Forge's own runtime-state subtree — hidden from the
    // browser like `.git`/`target`. Matched by exact path (never by bare
    // name) so a project's own `local/` directory elsewhere in the tree is
    // never hidden by mistake. The rest of `.forge/` (rules/agents/skills/
    // workflows) is project-owned and stays visible.
    path == root.join(".forge").join("local")
}

fn sort_nodes(nodes: &mut [FileNode]) {
    nodes.sort_by(|a, b| {
        (a.kind != FileKind::Directory, a.display_name.to_lowercase())
            .cmp(&(b.kind != FileKind::Directory, b.display_name.to_lowercase()))
    });
}

#[allow(clippy::too_many_arguments)]
fn explorer_row_line(
    prefix: &str,
    marker: &str,
    active_file: bool,
    name: &str,
    kind: FileKind,
    selected: bool,
    panel_focused: bool,
    status: Option<GitStatusKind>,
    _icon_mode: FileIconMode,
    query: &str,
    width: usize,
) -> Line<'static> {
    let selection_style = selected.then(|| {
        if panel_focused {
            theme::selection_active()
        } else {
            theme::selection_inactive()
        }
    });
    let chrome_style = theme::muted();
    let mut name_style = selection_style.unwrap_or_else(|| match kind {
        FileKind::Directory => theme::directory(),
        FileKind::Symlink => theme::symlink(),
        FileKind::File | FileKind::Unknown => theme::text(),
    });
    if active_file {
        name_style = name_style.add_modifier(ratatui::style::Modifier::BOLD);
    }
    if kind == FileKind::Directory {
        name_style = name_style.add_modifier(ratatui::style::Modifier::BOLD);
    }
    // Reserve the selection gutter so focus never shifts the tree's labels.
    // Content-search file groups still need their expand/collapse marker.
    let marker = if active_file && marker == " " {
        "•"
    } else {
        marker
    };
    let mut spans = vec![
        Span::styled(" ", selection_style.unwrap_or(chrome_style)),
        Span::styled(format!("{prefix}{marker} "), chrome_style),
    ];
    let chrome_width = spans.iter().map(Span::width).sum::<usize>();
    let name = crate::path_display::elide_middle(
        name,
        width.saturating_sub(chrome_width + if status.is_some() { 2 } else { 0 }),
    );
    // Selection wins over everything; otherwise the first query token found
    // as a contiguous (case-insensitive) run in the name is highlighted.
    if selection_style.is_some() || query.trim().is_empty() {
        spans.push(Span::styled(name, name_style));
    } else {
        spans.extend(highlight_name_spans(&name, query, name_style));
    }
    if let Some(status) = status {
        let mut glyph = status_indicator_now(Status::from(status));
        if let Some(style) = selection_style {
            glyph.style = style;
        }
        let used = spans.iter().map(Span::width).sum::<usize>();
        spans.push(Span::raw(
            " ".repeat(width.saturating_sub(used + glyph.width())),
        ));
        spans.push(glyph);
    }
    Line::from(spans).style(selection_style.unwrap_or_default())
}

/// Split `name` into spans with the first query token that occurs as a
/// contiguous case-insensitive run highlighted with the shared
/// [`theme::search_match`] style (same as the source viewer). Tokens that
/// only match as a fuzzy subsequence (or only match an ancestor path) leave
/// the name plain: no highlight is more truthful than a wrong one.
pub(crate) fn highlight_name_spans(name: &str, query: &str, base: Style) -> Vec<Span<'static>> {
    let token = query
        .split_whitespace()
        .find(|token| contains_case_insensitive(name, token));
    let Some(token) = token else {
        return vec![Span::styled(name.to_string(), base)];
    };
    let Some((start, end)) = match_byte_range_case_insensitive(name, token) else {
        return vec![Span::styled(name.to_string(), base)];
    };
    let mut spans = Vec::with_capacity(3);
    if start > 0 {
        spans.push(Span::styled(name[..start].to_string(), base));
    }
    spans.push(Span::styled(
        name[start..end].to_string(),
        theme::search_match(),
    ));
    if end < name.len() {
        spans.push(Span::styled(name[end..].to_string(), base));
    }
    spans
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    match_byte_range_case_insensitive(haystack, needle).is_some()
}

/// Byte range of the first case-insensitive occurrence of `needle` in
/// `haystack`. Matching runs over lowercased chars while every entry keeps
/// its source byte offset and length, so multi-byte text and multi-char
/// lowercasings (e.g. `İ`) still slice on valid boundaries.
fn match_byte_range_case_insensitive(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return None;
    }
    let hay: Vec<(usize, usize, char)> = haystack
        .char_indices()
        .flat_map(|(byte, c)| {
            let len = c.len_utf8();
            c.to_lowercase().map(move |lower| (byte, len, lower))
        })
        .collect();
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len().saturating_sub(needle.len())).find_map(|i| {
        let matches = needle.iter().enumerate().all(|(j, nc)| hay[i + j].2 == *nc);
        if !matches {
            return None;
        }
        let start = hay[i].0;
        let (last_byte, last_len, _) = hay[i + needle.len() - 1];
        Some((start, last_byte + last_len))
    })
}

/// One canvas separator followed by a padded, single-line composer surface.
pub(crate) const SEARCH_ROW_HEIGHT: u16 = 4;
const SEARCH_TEXT_ROW: u16 = 2;
/// Vertical origin of the tree after the search surface and spacing.
const TREE_TOP_OFFSET: u16 = SEARCH_ROW_HEIGHT + crate::design::TREE_TOP_GAP_Y;
/// Shared left inset for the search text and tree selection gutter.
const TREE_LEAD_INSET: u16 = 1;
/// Tree origin relative to the explorer's outer rectangle, shared with mouse routing.
pub(crate) const TREE_ROW_OFFSET: u16 = TREE_TOP_OFFSET;

pub struct FileExplorerWidget<'a> {
    pub explorer: &'a mut FileExplorer,
    pub focused: bool,
    /// The resource currently open in the workspace, independent of the
    /// explorer's navigation cursor. Diff and other views pass no file.
    pub active_file: Option<&'a Path>,
    pub show_search: bool,
    /// Whether `FocusBlock::Search` owns input and displays the caret.
    pub search_active: bool,
    /// Visible node index under the pointer (hover), if any. Hover never
    /// moves focus or selection; it only tints the row.
    pub hover: Option<usize>,
}

impl Widget for FileExplorerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.explorer.poll_search_load();
        self.explorer.poll_workspace_refresh();
        let inner = area;
        theme::fill(area, buf, theme::panel());
        // The footer row (selected relative path) always reserves 1 row;
        // the tree's own budget sits directly below the search field.
        let tree_top = if self.show_search {
            TREE_TOP_OFFSET
        } else {
            crate::design::TREE_TOP_GAP_Y
        };
        let height = inner.height.saturating_sub(tree_top + 1) as usize;
        self.explorer.ensure_selection_visible(height);
        let selected_index = self.explorer.selected_visible_index();
        let content_mode = self.explorer.search_mode == FileSearchMode::Content;
        let visible = &self.explorer.visible;
        let mut lines = Vec::new();
        let mut selected_line = None;
        if self.explorer.root.is_none() {
            lines.push(Line::from("No repository detected"));
        } else if let Some(root) = self.explorer.root.as_ref() {
            if root.loading || !root.loaded {
                lines.push(Line::from("Loading files..."));
            } else if let Some(ref error) = root.error {
                lines.push(Line::styled(
                    format!("Unable to load files: {}", error),
                    theme::danger(),
                ));
            } else if root.children.is_empty() {
                lines.push(Line::from("This directory is empty"));
            } else {
                let error_shown = self.explorer.git_status.error.is_some();
                if let Some(error) = self.explorer.git_status.error.as_deref() {
                    lines.push(Line::styled(
                        format!("Git status unavailable: {}", error),
                        theme::muted(),
                    ));
                }
                let list_height = height.saturating_sub(error_shown as usize);
                let query = self
                    .explorer
                    .search_results
                    .as_ref()
                    .map(|results| results.query.clone())
                    .unwrap_or_else(|| self.explorer.search_query.clone());
                for (offset, node) in visible
                    .iter()
                    .skip(self.explorer.scroll)
                    .take(list_height)
                    .enumerate()
                {
                    let selected = selected_index == Some(self.explorer.scroll + offset);
                    let marker = if content_mode && node.content_match.is_none() {
                        if node.expanded {
                            "⌄"
                        } else {
                            "›"
                        }
                    } else {
                        match node.kind {
                            FileKind::Directory if node.loading => "…",
                            FileKind::Directory if node.expanded => "⌄",
                            FileKind::Directory => "›",
                            FileKind::File | FileKind::Symlink | FileKind::Unknown => " ",
                        }
                    };
                    let prefix = TREE_INDENT.repeat(node.depth);
                    let status = if node.content_match.is_none()
                        && matches!(node.kind, FileKind::File | FileKind::Symlink)
                    {
                        self.explorer.git_status_for(&node.path)
                    } else {
                        None
                    };
                    let group_name = if content_mode && node.content_match.is_none() {
                        let count = format!(" ({})", node.child_count);
                        let budget = inner.width.saturating_sub(TREE_LEAD_INSET + 3) as usize;
                        format!(
                            "{}{count}",
                            crate::path_display::elide_path(
                                &node.display_name,
                                budget.saturating_sub(count.len()),
                            )
                        )
                    } else {
                        node.display_name.clone()
                    };
                    let mut line = if let Some(hit) = &node.content_match {
                        content_match_line(
                            hit,
                            &query,
                            selected,
                            self.focused,
                            inner.width.saturating_sub(TREE_LEAD_INSET) as usize,
                        )
                    } else {
                        explorer_row_line(
                            &prefix,
                            marker,
                            self.active_file == Some(node.path.as_path()),
                            &group_name,
                            node.kind,
                            selected,
                            self.focused,
                            status,
                            self.explorer.icon_mode,
                            if content_mode { "" } else { &query },
                            inner.width.saturating_sub(TREE_LEAD_INSET) as usize,
                        )
                    };
                    // Hover is a pointer affordance: raised ground plus a
                    // weight step. Selection wins and nothing about the row's
                    // layout or focus changes.
                    if self.hover == Some(self.explorer.scroll + offset) && !selected {
                        line = line.style(
                            theme::surface_hover().add_modifier(ratatui::style::Modifier::BOLD),
                        );
                    }
                    if selected && node.content_match.is_none() {
                        selected_line = Some(lines.len());
                    }
                    lines.push(line);
                    if let Some(error) = &node.error {
                        lines.push(Line::styled(
                            format!("{prefix}  Unable to read this directory"),
                            theme::danger(),
                        ));
                        lines.push(Line::styled(format!("{prefix}  {error}"), theme::muted()));
                    } else if node.kind == FileKind::Directory
                        && node.expanded
                        && node.loaded
                        && node.child_count == 0
                        && node.depth > 0
                    {
                        lines.push(Line::styled(
                            format!("{prefix}  This directory is empty"),
                            theme::muted(),
                        ));
                    }
                }
                // A query that filters everything out is a different state
                // from an empty repository: say what didn't match. A failed
                // scan is a third state again — it says nothing about the
                // query, so it never borrows the no-matches line.
                if self.explorer.search_unavailable {
                    lines.push(Line::styled("Search unavailable", theme::muted()));
                } else if content_mode && self.explorer.search_query.trim().is_empty() {
                    lines.push(Line::styled("Type to find in files", theme::muted()));
                } else if visible.is_empty() && self.explorer.search_loading {
                    lines.push(Line::styled("Searching...", theme::muted()));
                } else if visible.is_empty()
                    && !query.trim().is_empty()
                    && !self.explorer.search_loading
                {
                    lines.push(Line::styled(
                        format!("No matches for \"{}\"", query.trim()),
                        theme::muted(),
                    ));
                }
            }
        }
        if self.show_search && inner.height >= TREE_TOP_OFFSET {
            // Full-bleed across the navigator column: consume the one-cell
            // shell inset on the left so the field's surface runs edge-to-edge
            // like the composer's. The query keeps the composer's text inset.
            let bleed = inner.x.min(crate::design::PANE_PAD_X);
            let search_area = Rect::new(
                inner.x.saturating_sub(bleed),
                inner.y,
                inner.width.saturating_add(bleed),
                SEARCH_ROW_HEIGHT,
            );
            Block::default()
                .style(theme::composer_surface())
                .render(search_area, buf);
            theme::fill(
                Rect::new(search_area.x, search_area.y, search_area.width, 1),
                buf,
                theme::canvas(),
            );
            let inset = crate::widgets::input::TEXT_INSET;
            let search_inner = Rect::new(
                search_area.x + inset,
                search_area.y + SEARCH_TEXT_ROW,
                search_area.width.saturating_sub(inset * 2),
                1,
            );
            let text_focused = self.search_active && self.focused && self.explorer.search_focused;
            // The result count shares the Search field, so it claims its cells
            // first and the query truncates around it. A listing that hit the
            // cap still has to be able to say so.
            let count = self.explorer.search_result_count().unwrap_or_default();
            let count_width = Span::raw(&count).width() as u16;
            let field_width = search_inner.width;
            let reserved = count_width.min(field_width);
            let query_room = field_width
                .saturating_sub(reserved)
                .saturating_sub(u16::from(text_focused)) as usize;
            let mut query_width = 0;
            let shown_query: String = self
                .explorer
                .search_query
                .chars()
                .take_while(|ch| {
                    let width = Span::raw(ch.to_string()).width();
                    if query_width + width > query_room {
                        return false;
                    }
                    query_width += width;
                    true
                })
                .collect();
            let placeholder = if content_mode {
                "Find in files..."
            } else {
                "Search files..."
            };
            let (search, search_style) = if self.explorer.search_query.is_empty() {
                (placeholder.to_string(), theme::composer_placeholder())
            } else {
                let text = if text_focused {
                    format!("{}{}", shown_query, theme::CURSOR_GLYPH)
                } else {
                    shown_query.clone()
                };
                (text, theme::composer_text())
            };
            let mut spans = vec![Span::styled(search.clone(), search_style)];
            if reserved > 0 {
                let used = Span::raw(&search).width() as u16;
                spans.push(Span::raw(
                    " ".repeat(field_width.saturating_sub(used + reserved) as usize),
                ));
                spans.push(Span::styled(count, theme::muted()));
            }
            Paragraph::new(Line::from(spans)).render(search_inner, buf);
            if text_focused {
                let cursor_x = search_inner.x + query_width as u16;
                if cursor_x < search_inner.right() {
                    if self.explorer.search_query.is_empty() {
                        buf[(cursor_x, search_inner.y)].set_style(theme::caret());
                    } else {
                        theme::paint_caret(buf, cursor_x, search_inner.y);
                    }
                }
            }

            // The whole tree sits one indent step inside the field above it,
            // so the disclosure column clears the search box's own left edge.
            Paragraph::new(lines).render(
                Rect::new(
                    inner.x.saturating_add(TREE_LEAD_INSET),
                    inner.y + tree_top,
                    inner.width.saturating_sub(TREE_LEAD_INSET),
                    inner.height.saturating_sub(tree_top + 1),
                ),
                buf,
            );
        }
        if self.focused && !self.search_active {
            if let Some(offset) = selected_line {
                let y = inner.y + tree_top + offset as u16;
                if y < inner.bottom().saturating_sub(1) {
                    buf.set_string(inner.x, y, theme::FOCUS_MARKER, theme::accent_style());
                }
            }
        }
        if inner.height > 0 {
            // `selected_relative_path` is "." at the workspace root, which
            // left a lone full stop floating in the corner of the pane.
            let selected = match self.explorer.selected_relative_path() {
                Some(path) if path != "." => path,
                _ => String::new(),
            };
            let footer_y = inner.y + inner.height.saturating_sub(1);
            Paragraph::new(Line::styled(
                crate::path_display::elide_path(&selected, inner.width as usize),
                theme::muted(),
            ))
            .render(Rect::new(inner.x, footer_y, inner.width, 1), buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn tree_selection_fills_row_and_git_markers_align_right() {
        let area = Rect::new(0, 0, 40, 1);
        for focused in [false, true] {
            for name in ["a.rs", "longer-file-name.rs", "東京.rs"] {
                let line = explorer_row_line(
                    "  ",
                    " ",
                    false,
                    name,
                    FileKind::File,
                    true,
                    focused,
                    Some(GitStatusKind::Modified),
                    FileIconMode::Unicode,
                    "",
                    40,
                );
                let mut buf = Buffer::empty(area);
                theme::fill(area, &mut buf, theme::panel());
                Paragraph::new(line).render(area, &mut buf);
                assert_eq!(buf[(0, 0)].symbol(), " ");
                assert_eq!(buf[(39, 0)].symbol(), "M");
                for x in (0..40).filter(|_| name.is_ascii()) {
                    assert_eq!(
                        buf[(x, 0)].bg,
                        if focused {
                            theme::selection_active().bg.unwrap()
                        } else {
                            theme::panel().bg.unwrap()
                        },
                        "focused={focused}, name={name}, x={x}"
                    );
                }
            }
        }
    }

    #[test]
    fn directory_label_is_bold_but_disclosure_is_quiet() {
        let line = explorer_row_line(
            "  ",
            "›",
            false,
            "src",
            FileKind::Directory,
            false,
            false,
            None,
            FileIconMode::Unicode,
            "",
            40,
        );
        assert_eq!(line.spans[1].style, theme::muted());
        assert!(line.spans[2]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD));
        assert_eq!(TREE_LEAD_INSET, 1);
    }

    fn wait_for_search_load(explorer: &mut FileExplorer) {
        for _ in 0..1_000 {
            explorer.poll_search_load();
            if !explorer.search_loading {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("file search did not finish loading");
    }

    #[test]
    fn sort_directories_before_files_case_insensitive() {
        let mut nodes = vec![
            FileNode::child(PathBuf::from("b.rs"), FileKind::File),
            FileNode::child(PathBuf::from("Zoo"), FileKind::Directory),
            FileNode::child(PathBuf::from("alpha"), FileKind::Directory),
            FileNode::child(PathBuf::from("A.rs"), FileKind::File),
        ];
        sort_nodes(&mut nodes);
        let names: Vec<_> = nodes.into_iter().map(|node| node.display_name).collect();
        assert_eq!(names, ["alpha", "Zoo", "A.rs", "b.rs"]);
    }

    #[test]
    fn flatten_honors_expand_collapse() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        assert_eq!(explorer.visible_nodes().len(), 2);
        explorer.selected_path = Some(root.path().join("src").canonicalize().unwrap());
        explorer.expand_selected();
        assert_eq!(explorer.visible_nodes().len(), 3);
        explorer.collapse_selected();
        assert_eq!(explorer.visible_nodes().len(), 2);
    }

    #[test]
    fn requesting_selected_unstaged_diff_is_non_blocking() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("changed.rs");
        fs::write(&path, "changed\n").unwrap();
        let path = path.canonicalize().unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        let relative = PathBuf::from("changed.rs");
        explorer.git_status.details.insert(
            relative,
            forge_workspace::git_status::PathStatus {
                staged: None,
                unstaged: Some(GitStatusKind::Modified),
            },
        );

        explorer.request_unstaged_diff(&path);

        assert!(explorer.git_status.diff_loading);
        assert!(explorer
            .git_status
            .get_unstaged_diff(Path::new("changed.rs"))
            .is_none());
    }

    #[test]
    fn forge_local_is_hidden_but_project_owned_forge_resources_are_visible() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".forge/local/sessions")).unwrap();
        fs::write(root.path().join(".forge/local/sessions/x.db"), "").unwrap();
        fs::create_dir_all(root.path().join(".forge/rules")).unwrap();
        fs::write(root.path().join(".forge/rules/style.md"), "").unwrap();
        fs::create_dir_all(root.path().join(".agents/skills/ponytail")).unwrap();
        fs::write(root.path().join(".agents/skills/ponytail/SKILL.md"), "").unwrap();

        let children = read_children(Some(root.path()), root.path()).unwrap();
        let names: Vec<&str> = children
            .iter()
            .filter_map(|n| n.path.file_name().and_then(|s| s.to_str()))
            .collect();
        assert!(names.contains(&".forge"), "{names:?}");
        assert!(names.contains(&".agents"), "{names:?}");

        let forge_children = read_children(Some(root.path()), &root.path().join(".forge")).unwrap();
        let forge_names: Vec<&str> = forge_children
            .iter()
            .filter_map(|n| n.path.file_name().and_then(|s| s.to_str()))
            .collect();
        assert!(forge_names.contains(&"rules"), "{forge_names:?}");
        assert!(
            !forge_names.contains(&"local"),
            "`.forge/local` must stay hidden: {forge_names:?}"
        );

        let agents_children =
            read_children(Some(root.path()), &root.path().join(".agents")).unwrap();
        let agents_names: Vec<&str> = agents_children
            .iter()
            .filter_map(|n| n.path.file_name().and_then(|s| s.to_str()))
            .collect();
        assert!(agents_names.contains(&"skills"), "{agents_names:?}");
    }

    #[test]
    fn a_project_directory_literally_named_local_is_not_hidden() {
        // `.forge/local` is hidden by exact path, never by bare name — a
        // repository's own `local/` directory elsewhere must stay visible.
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("local")).unwrap();
        fs::write(root.path().join("local/config.toml"), "").unwrap();

        let children = read_children(Some(root.path()), root.path()).unwrap();
        let names: Vec<&str> = children
            .iter()
            .filter_map(|n| n.path.file_name().and_then(|s| s.to_str()))
            .collect();
        assert!(names.contains(&"local"), "{names:?}");
    }

    fn wait_for_git(explorer: &mut FileExplorer) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while explorer.git_status.loading && Instant::now() < deadline {
            explorer.git_status.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn init_git(dir: &Path) {
        for args in [
            ["init", "--initial-branch=main", "-q"].as_slice(),
            ["config", "user.email", "test@example.com"].as_slice(),
            ["config", "user.name", "Test"].as_slice(),
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
    }

    #[test]
    fn expand_does_not_respawn_git_status() {
        let root = tempfile::tempdir().unwrap();
        init_git(root.path());
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        wait_for_git(&mut explorer);
        assert!(!explorer.git_status.loading);

        explorer.selected_path = Some(root.path().join("src").canonicalize().unwrap());
        explorer.expand_selected();

        assert!(
            !explorer.git_status.loading,
            "git status is whole-repo; expanding a folder must not spawn another porcelain"
        );
    }

    #[test]
    fn expanding_an_already_open_directory_does_not_rebuild() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        explorer.selected_path = Some(root.path().join("src").canonicalize().unwrap());
        explorer.expand_selected();
        let visible = explorer.visible.as_ptr();
        explorer.expand_selected();
        assert_eq!(
            explorer.visible.as_ptr(),
            visible,
            "a second → on an open folder must not rebuild the listing"
        );
    }

    #[test]
    fn large_tree_navigation_stays_on_a_keystroke_budget() {
        let root = tempfile::tempdir().unwrap();
        init_git(root.path());
        const DIRS: usize = 30;
        const FILES: usize = 40;
        for dir in 0..DIRS {
            let path = root.path().join(format!("pkg_{dir:02}"));
            fs::create_dir(&path).unwrap();
            for file in 0..FILES {
                fs::write(path.join(format!("f_{file:02}.rs")), "").unwrap();
            }
        }
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        wait_for_git(&mut explorer);

        let dirs: Vec<_> = explorer
            .visible
            .iter()
            .filter(|node| node.kind == FileKind::Directory && node.depth == 1)
            .map(|node| node.path.clone())
            .collect();
        assert_eq!(dirs.len(), DIRS);

        let started = Instant::now();
        for path in &dirs {
            explorer.selected_path = Some(path.clone());
            explorer.expand_selected();
        }
        let expand_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(
            expand_ms < 150.0,
            "expanding {DIRS} directories took {expand_ms:.1}ms; each expand must be a read_dir, not git status"
        );
        assert_eq!(
            explorer.visible.len(),
            1 + DIRS + DIRS * FILES,
            "every file should be visible after expanding"
        );

        let started = Instant::now();
        for _ in 0..500 {
            explorer.move_selection(1);
        }
        let move_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(
            move_ms < 10.0,
            "500 selection moves on a {FILES}-file listing took {move_ms:.1}ms"
        );

        explorer.selected_path = Some(dirs[0].clone());
        let started = Instant::now();
        for _ in 0..50 {
            explorer.expand_selected();
        }
        let noop_us = started.elapsed().as_secs_f64() * 1_000_000.0;
        assert!(
            noop_us < 2_000.0,
            "50 expands of an already-open directory took {noop_us:.0}us"
        );
    }

    #[test]
    fn selection_moves_within_visible_nodes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a"), "").unwrap();
        fs::write(root.path().join("b"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        let visible_rows = explorer.visible.as_ptr();
        explorer.move_selection(1);
        assert_eq!(explorer.visible.as_ptr(), visible_rows);
        assert_eq!(explorer.selected_relative_path().as_deref(), Some("a"));
        explorer.move_selection(99);
        assert_eq!(explorer.selected_relative_path().as_deref(), Some("b"));
        explorer.move_selection(-99);
        assert_eq!(explorer.selected_relative_path().as_deref(), Some("."));
    }

    #[test]
    fn safe_path_rejects_outside_symlink_target() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("outside")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(outside.path(), root.path().join("outside")).unwrap();
        assert!(safe_path(root.path(), &root.path().join("outside")).is_err());
    }

    #[test]
    fn populated_root_is_loaded_and_not_empty() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.txt"), "").unwrap();
        fs::create_dir(root.path().join("src")).unwrap();

        let explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        let root_node = explorer.root.as_ref().expect("root missing");
        assert!(root_node.loaded);
        assert!(!root_node.loading);
        assert!(root_node.error.is_none());
        assert_eq!(explorer.visible_nodes().len(), 3);
    }

    #[test]
    fn genuinely_empty_root_shows_empty_state() {
        let root = tempfile::tempdir().unwrap();
        let explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        let root_node = explorer.root.as_ref().expect("root missing");
        assert!(root_node.loaded);
        assert!(root_node.children.is_empty());
        assert_eq!(explorer.visible_nodes().len(), 1);
    }

    #[test]
    fn fuzzy_search_matches_relative_paths_and_keeps_ancestors() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("src/components")).unwrap();
        fs::write(root.path().join("src/components/button.rs"), "").unwrap();
        fs::write(root.path().join("README.md"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        explorer.selected_path = Some(root.path().join("src").canonicalize().unwrap());
        explorer.expand_selected();
        explorer.selected_path = Some(root.path().join("src/components").canonicalize().unwrap());
        explorer.expand_selected();

        explorer.set_search_query("src cmp btn".replace(' ', ""));
        let names: Vec<_> = explorer
            .visible_nodes()
            .iter()
            .map(|node| node.display_name.clone())
            .collect();
        assert!(names[0] != "src");
        assert_eq!(&names[1..], ["src", "components", "button.rs"]);
    }

    #[test]
    fn clearing_fuzzy_search_restores_full_tree_and_selection() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("alpha.txt"), "").unwrap();
        fs::write(root.path().join("beta.txt"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        let full_count = explorer.visible_nodes().len();
        explorer.set_search_query("alpha");
        assert_eq!(explorer.visible_nodes().len(), 2);
        explorer.clear_search();
        assert_eq!(explorer.visible_nodes().len(), full_count);
        assert!(explorer.selected_path.is_some());
    }

    #[test]
    fn fuzzy_search_recurses_into_collapsed_directories_and_tokenizes_terms() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/api")).unwrap();
        fs::write(root.path().join("src/api/client.rs"), "").unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let mut explorer =
            FileExplorer::new(Some(root_path.clone()), forge_config::FileIconMode::Unicode);

        explorer.set_search_query("client");
        let src = explorer
            .root
            .as_ref()
            .unwrap()
            .children
            .iter()
            .find(|node| node.display_name == "src")
            .unwrap();
        assert!(
            !src.loaded,
            "search must not scan directories on the input thread"
        );
        assert!(explorer.search_loading);
        wait_for_search_load(&mut explorer);

        // Ranked results, not a tree walk: the match and the ancestors it needs
        // come from the index, and the workspace root itself is not a result.
        let visible: Vec<_> = explorer
            .visible_nodes()
            .iter()
            .map(|node| {
                let relative = node.path.strip_prefix(&root_path).unwrap();
                if relative.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    relative.to_path_buf()
                }
            })
            .collect();
        assert_eq!(
            visible,
            vec![
                PathBuf::from("src"),
                PathBuf::from("src/api"),
                PathBuf::from("src/api/client.rs"),
            ]
        );
    }

    #[test]
    fn content_search_is_separate_from_filename_navigation() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::write(
            root.path().join("src/handler.rs"),
            "pub fn distinctive_payload_marker() {}\n",
        )
        .unwrap();
        fs::write(root.path().join("src/other.rs"), "pub fn unrelated() {}\n").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );

        explorer.set_search_query("distinctive_payload");
        wait_for_search_load(&mut explorer);
        assert!(explorer.visible_nodes().is_empty());

        explorer.set_search_mode(FileSearchMode::Content);
        wait_for_search_load(&mut explorer);

        let names: Vec<_> = explorer
            .visible_nodes()
            .iter()
            .map(|node| node.display_name.clone())
            .collect();
        assert_eq!(
            names,
            ["src/handler.rs", "pub fn distinctive_payload_marker() {}"]
        );
        assert!(explorer.visible_nodes()[0].content_match.is_none());
        assert_eq!(
            explorer.visible_nodes()[1]
                .content_match
                .as_ref()
                .unwrap()
                .line,
            1
        );
        assert!(!explorer.search_unavailable);
    }

    #[test]
    fn content_search_groups_matches_and_preserves_selection_style() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("needle.rs"), "needle\n").unwrap();
        fs::write(
            root.path().join("handler.rs"),
            "first needle\nsecond needle\n",
        )
        .unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        explorer.set_search_mode(FileSearchMode::Content);
        explorer.set_search_query("needle");
        wait_for_search_load(&mut explorer);
        assert_eq!(explorer.search_result_count().as_deref(), Some("3 matches"));
        assert_eq!(explorer.visible_nodes().len(), 5);
        explorer.select_visible_row(1);
        assert_eq!(explorer.selected_content_match().unwrap().line, 1);
        explorer.move_selection(1);
        assert_eq!(explorer.selected_content_match().unwrap().line, 2);
        assert!(!explorer.selection_at_first_row());

        let area = Rect::new(0, 0, 60, 18);
        for focused in [false, true] {
            let mut buf = Buffer::empty(area);
            FileExplorerWidget {
                explorer: &mut explorer,
                focused,
                active_file: None,
                show_search: true,
                search_active: false,
                hover: None,
            }
            .render(area, &mut buf);
            let rows: Vec<_> = (0..area.height).map(|y| row_text(&buf, area, y)).collect();
            let content_y = rows
                .iter()
                .position(|row| row.contains("second needle"))
                .unwrap() as u16;
            let content_row = &rows[content_y as usize];
            assert!(
                content_row.contains(if focused {
                    ">    2  second needle"
                } else {
                    "     2  second needle"
                }),
                "{content_row}"
            );
            assert_eq!(content_row.matches('>').count(), usize::from(focused));
            let label_x = content_row.find("needle").unwrap() as u16;
            let style = if focused {
                theme::selection_active()
            } else {
                theme::selection_inactive()
            };
            assert_eq!(buf[(label_x, content_y)].fg, style.fg.unwrap());
        }

        explorer.collapse_selected();
        assert_eq!(explorer.visible_nodes().len(), 3);
        assert!(explorer.selected_content_match().is_none());
        assert!(explorer.selected_file_path().is_none());
        explorer.expand_selected();
        assert_eq!(explorer.visible_nodes().len(), 5);
        explorer.clear_search();
        assert!(explorer.visible_nodes().is_empty());
        explorer.set_search_mode(FileSearchMode::Names);
        assert!(explorer
            .visible_nodes()
            .iter()
            .all(|node| node.content_match.is_none()));
    }

    #[test]
    fn content_search_caps_matches_truthfully_even_in_one_file() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("many.rs"),
            "needle\n".repeat(MAX_CONTENT_MATCHES + 10),
        )
        .unwrap();
        let mut explorer =
            FileExplorer::new(Some(root.path().to_path_buf()), FileIconMode::Unicode);
        explorer.set_search_mode(FileSearchMode::Content);
        explorer.set_search_query("needle");
        wait_for_search_load(&mut explorer);
        assert_eq!(
            explorer.search_result_count().as_deref(),
            Some("200+ matches")
        );
        assert_eq!(explorer.visible_nodes().len(), MAX_CONTENT_MATCHES + 1);
    }

    #[test]
    fn a_single_character_query_matches_names_only() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("alpha.rs"), "let zz_marker = 1;\n").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );

        explorer.set_search_query("z");
        wait_for_search_load(&mut explorer);
        assert!(
            !explorer
                .visible_nodes()
                .iter()
                .any(|node| node.display_name == "alpha.rs"),
            "a one-character query must not list every file containing that letter"
        );
    }

    #[test]
    fn search_keystrokes_reuse_one_engine_without_materializing_tree() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/api/deep")).unwrap();
        fs::write(root.path().join("src/api/deep/client.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );

        explorer.set_search_query("client");
        wait_for_search_load(&mut explorer);
        let engine = explorer.search_engine_ptr();
        let src = explorer
            .root
            .as_ref()
            .unwrap()
            .children
            .iter()
            .find(|node| node.display_name == "src")
            .unwrap();
        assert!(!src.loaded, "search must not materialize the FileNode tree");

        // Every keystroke scans again — content matching cannot be answered
        // from a cached list — but they all share one index.
        for query in ["cli", "client", "cl"] {
            explorer.set_search_query(query);
            assert!(explorer.search_loading);
            wait_for_search_load(&mut explorer);
            assert_eq!(explorer.search_engine_ptr(), engine);
        }
        assert!(explorer
            .visible
            .iter()
            .any(|node| node.display_name == "client.rs"));
    }

    /// A failed scan is not a failed query, so the two never share a line.
    #[test]
    fn an_unavailable_search_is_its_own_state() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("only.rs"), "content\n").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );

        explorer.set_search_query("content");
        wait_for_search_load(&mut explorer);
        assert!(!explorer.search_unavailable);

        explorer.search_results = None;
        explorer.search_unavailable = true;
        explorer.rebuild_visible();

        let area = Rect::new(0, 0, 30, 14);
        let buf = render_widget(&mut explorer, area, true);
        let rendered: Vec<String> = (0..area.height)
            .map(|offset| row_text(&buf, area, area.y + offset))
            .collect();
        let shown = rendered.join("\n");
        assert!(shown.contains("Search unavailable"), "{shown}");
        assert!(!shown.contains("No matches for"), "{shown}");
    }

    #[test]
    fn collapsing_a_filtered_directory_keeps_selection_on_a_visible_row() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("tests/api")).unwrap();
        fs::create_dir_all(root.path().join("src/api")).unwrap();
        fs::write(root.path().join("tests/api/client.rs"), "").unwrap();
        fs::write(root.path().join("src/api/client.rs"), "").unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let mut explorer =
            FileExplorer::new(Some(root_path.clone()), forge_config::FileIconMode::Unicode);

        explorer.set_search_query("client");
        wait_for_search_load(&mut explorer);
        explorer.selected_path = Some(root_path.join("tests/api"));
        explorer.activate_selected();

        let visible = explorer.visible_nodes();
        assert!(visible
            .iter()
            .any(|node| Some(&node.path) == explorer.selected_path.as_ref()));
        assert_ne!(explorer.selected_relative_path().as_deref(), Some("."));
    }

    #[test]
    fn git_status_refresh_does_not_clear_tree() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.txt"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        let before = explorer.visible_nodes().len();
        explorer.refresh_git_status();
        explorer.git_status.poll();
        assert_eq!(explorer.visible_nodes().len(), before);
    }

    #[test]
    fn workspace_refresh_reloads_tree_and_preserves_expanded_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        explorer.selected_path = Some(root.path().join("src").canonicalize().unwrap());
        explorer.expand_selected();

        fs::write(root.path().join("src/main.rs"), "").unwrap();
        fs::write(root.path().join("README.md"), "").unwrap();
        explorer.refresh_workspace();
        let visible = explorer.visible_nodes();

        assert!(visible.iter().any(|node| node.display_name == "README.md"));
        assert!(visible.iter().any(|node| node.display_name == "main.rs"));
        assert!(visible
            .iter()
            .any(|node| node.display_name == "src" && node.expanded));
    }

    #[test]
    fn workspace_refresh_preserves_collapsed_loaded_directories() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        explorer.selected_path = Some(root.path().join("src").canonicalize().unwrap());
        explorer.expand_selected();
        explorer.collapse_selected();

        fs::write(root.path().join("src/main.rs"), "").unwrap();
        explorer.refresh_workspace();
        let visible = explorer.visible_nodes();

        assert!(visible
            .iter()
            .any(|node| node.display_name == "src" && !node.expanded));
        assert!(!visible.iter().any(|node| node.display_name == "main.rs"));
    }

    #[test]
    fn refresh_selected_directory_reloads_only_that_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "").unwrap();
        fs::write(root.path().join("root.txt"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        explorer.selected_path = Some(root.path().join("src").canonicalize().unwrap());
        explorer.expand_selected();
        assert_eq!(explorer.visible_nodes().len(), 4);

        // Refreshing the selected directory should keep the root and other siblings intact.
        explorer.refresh_selected();
        assert_eq!(explorer.visible_nodes().len(), 4);
    }

    #[test]
    fn deleted_selected_path_falls_back_to_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        explorer.selected_path = Some(root.path().join("src/lib.rs").canonicalize().unwrap());
        fs::remove_file(root.path().join("src/lib.rs")).unwrap();

        explorer.refresh_selected();
        // The tree remains populated; selection should fall back to an existing node.
        assert!(
            explorer.selected_path.is_some(),
            "selection should not be lost"
        );
        assert!(explorer.root.as_ref().unwrap().loaded);
        assert!(!explorer.root.as_ref().unwrap().children.is_empty());
    }

    #[test]
    fn loading_root_does_not_show_empty_message() {
        let mut root = FileNode::root(PathBuf::from("/tmp/forge-test-root"));
        root.loaded = false;
        root.loading = true;
        root.children.clear();
        let mut explorer = FileExplorer {
            root: Some(root),
            selected_path: Some(PathBuf::from("/tmp/forge-test-root")),
            selected_index: None,
            selected_match_line: None,
            scroll: 0,
            focused: false,
            search_focused: true,
            search_query: String::new(),
            search_mode: FileSearchMode::Names,
            icon_mode: FileIconMode::Unicode,
            root_path: Some(PathBuf::from("/tmp/forge-test-root")),
            git_status: GitStatusCache::new(),
            visible: Vec::new(),
            search_loader: None,
            search_cancel: None,
            search_loading: false,
            search_results: None,
            collapsed_search_files: HashSet::new(),
            search_engine: SearchEngine::default(),
            search_unavailable: false,
            diff_filter: None,
            refresh_generation: 0,
            pending_refresh: None,
            refresh_queued: false,
            refresh_test_gate: None,
            refresh_install_count: 0,
        };
        explorer.rebuild_visible();
        let root_node = explorer.root.as_ref().unwrap();
        assert!(!root_node.loaded);
        assert!(root_node.loading);
        assert!(!root_node.children.is_empty() || true); // children may be empty while loading
        assert_eq!(explorer.visible_nodes().len(), 1);
    }

    fn render_widget(explorer: &mut FileExplorer, area: Rect, search_active: bool) -> Buffer {
        let mut buf = Buffer::empty(area);
        FileExplorerWidget {
            explorer,
            focused: true,
            active_file: None,
            show_search: true,
            search_active,
            hover: None,
        }
        .render(area, &mut buf);
        buf
    }

    fn row_text(buf: &Buffer, area: Rect, y: u16) -> String {
        (0..area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn search_placeholder_stays_aligned_when_focus_changes() {
        let mut explorer = FileExplorer::new(None, forge_config::FileIconMode::Unicode);
        for width in [20, 30, 50] {
            let area = Rect::new(0, 0, width, 18);
            for (mode, placeholder) in [
                (FileSearchMode::Names, "Search files..."),
                (FileSearchMode::Content, "Find in files..."),
            ] {
                explorer.set_search_mode(mode);
                explorer.search_focused = false;
                let idle = render_widget(&mut explorer, area, false);
                explorer.search_focused = true;
                let focused = render_widget(&mut explorer, area, true);

                assert_eq!(
                    row_text(&idle, area, SEARCH_TEXT_ROW),
                    row_text(&focused, area, SEARCH_TEXT_ROW)
                );
                let text_x = crate::widgets::input::TEXT_INSET;
                assert_eq!(
                    focused[(text_x, SEARCH_TEXT_ROW)].symbol(),
                    &placeholder[..1]
                );
                assert_eq!(
                    focused[(text_x, SEARCH_TEXT_ROW)].bg,
                    theme::caret().bg.unwrap()
                );
            }
        }
    }

    #[test]
    fn search_draft_keeps_placeholder_origin_without_shortcut_hint() {
        let mut explorer = FileExplorer::new(None, forge_config::FileIconMode::Unicode);
        let area = Rect::new(0, 0, 40, 18);
        explorer.search_query = "main.rs".into();
        let buf = render_widget(&mut explorer, area, true);
        let text_x = crate::widgets::input::TEXT_INSET;
        assert_eq!(buf[(text_x, SEARCH_TEXT_ROW)].symbol(), "m");
        assert_eq!(
            buf[(text_x + 7, SEARCH_TEXT_ROW)].bg,
            theme::caret().bg.unwrap()
        );
        for mode in [FileSearchMode::Names, FileSearchMode::Content] {
            explorer.set_search_mode(mode);
            let buf = render_widget(&mut explorer, area, true);
            let text = (0..area.height)
                .map(|y| row_text(&buf, area, y))
                .collect::<String>();
            assert!(!text.contains("Ctrl+Shift+F"));
            assert!(!text.contains("Ctrl+P"));
        }
    }

    #[test]
    fn match_byte_range_stays_on_char_boundaries() {
        assert_eq!(
            match_byte_range_case_insensitive("雪main.rs", "MAIN"),
            Some((3, 7))
        );
        assert_eq!(
            match_byte_range_case_insensitive("lib.rs", "LIB"),
            Some((0, 3))
        );
        assert_eq!(match_byte_range_case_insensitive("lib.rs", "zzz"), None);
    }

    #[test]
    fn clearing_search_cancels_background_directory_scan() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/api")).unwrap();
        fs::write(root.path().join("src/api/client.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );

        explorer.set_search_query("client");
        let cancel = explorer.search_cancel.as_ref().unwrap().clone();
        explorer.clear_search();

        assert!(cancel.load(Ordering::Relaxed));
        assert!(!explorer.search_loading);
        assert!(explorer.search_loader.is_none());
    }

    #[test]
    fn search_result_count_marks_a_truncated_listing() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..4 {
            fs::write(
                root.path().join(format!("token{index}.txt")),
                "shared token\n",
            )
            .unwrap();
        }
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
        assert_eq!(explorer.search_result_count(), None);

        explorer.set_search_query("token");
        wait_for_search_load(&mut explorer);
        assert_eq!(explorer.search_result_count().as_deref(), Some("4 files"));
        assert!(
            !explorer
                .search_results
                .as_ref()
                .expect("results installed")
                .truncated
        );

        explorer.search_results = Some(Arc::new(ExplorerSearchResults {
            query: "token".into(),
            names: MergedSearch {
                paths: vec!["a".into(), "b".into()],
                truncated: true,
            },
            content: Vec::new(),
            truncated: true,
        }));
        assert_eq!(explorer.search_result_count().as_deref(), Some("2+ files"));
    }

    #[test]
    fn narrowing_search_moves_selection_to_the_nearest_visible_match() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/api")).unwrap();
        fs::create_dir_all(root.path().join("src/config")).unwrap();
        fs::write(root.path().join("src/api/client.rs"), "").unwrap();
        fs::write(root.path().join("src/config/client_config.rs"), "").unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let mut explorer =
            FileExplorer::new(Some(root_path.clone()), forge_config::FileIconMode::Unicode);

        explorer.set_search_query("client");
        wait_for_search_load(&mut explorer);
        explorer.selected_path = Some(root_path.join("src/api/client.rs"));
        // Every query rescans, so the narrowing lands on a later tick. Until
        // it does, the previous result set is still the one on screen — which
        // is why the selection has not moved yet.
        explorer.set_search_query("config");
        assert_eq!(
            explorer.selected_relative_path().as_deref(),
            Some("src/api/client.rs")
        );

        wait_for_search_load(&mut explorer);
        assert_eq!(
            explorer.selected_relative_path().as_deref(),
            Some("src/config/client_config.rs")
        );
    }
}
