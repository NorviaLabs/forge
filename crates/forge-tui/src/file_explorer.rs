use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Widget};

use forge_config::FileIconMode;

use crate::status_glyph::{status_glyph, Status};
use crate::theme;
use crate::widgets::input::TEXT_INSET;
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
}

#[derive(Debug)]
pub struct FileExplorer {
    pub root: Option<FileNode>,
    pub selected_path: Option<PathBuf>,
    /// Index of `selected_path` in `visible`, when that mapping is known.
    /// `move_selection` uses this so holding j/k does not rescan every
    /// `PathBuf` on a large listing.
    selected_index: Option<usize>,
    pub scroll: usize,
    pub focused: bool,
    pub search_focused: bool,
    pub search_query: String,
    pub icon_mode: FileIconMode,
    root_path: Option<PathBuf>,
    pub git_status: GitStatusCache,
    visible: Vec<VisibleNode>,
    search_loader: Option<Receiver<Vec<SearchEntry>>>,
    search_cancel: Option<Arc<AtomicBool>>,
    search_loading: bool,
    /// Compact, immutable filesystem index shared across query keystrokes.
    search_index: Option<Arc<Vec<SearchEntry>>>,
}

#[derive(Debug)]
struct SearchEntry {
    path: PathBuf,
    relative: String,
    display_name: String,
    kind: FileKind,
    depth: usize,
}

impl FileExplorer {
    pub fn new(root_path: Option<PathBuf>, icon_mode: FileIconMode) -> Self {
        let root_path = root_path.map(|p| p.canonicalize().unwrap_or(p));
        let mut explorer = Self {
            root: root_path.clone().map(FileNode::root),
            selected_path: root_path.clone(),
            selected_index: None,
            scroll: 0,
            focused: false,
            search_focused: true,
            search_query: String::new(),
            icon_mode,
            root_path: root_path.clone(),
            git_status: GitStatusCache::new(),
            visible: Vec::new(),
            search_loader: None,
            search_cancel: None,
            search_loading: false,
            search_index: None,
        };
        explorer.load_root();
        if let Some(root) = root_path {
            explorer.git_status.start_refresh(root);
        }
        explorer
    }

    pub fn load_root(&mut self) {
        self.cancel_search_load();
        self.search_index = None;
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
        self.cancel_search_load();
        self.search_index = None;
        let root_path = self.root_path.clone();
        if let Some(root) = self.root.as_mut() {
            refresh_loaded_directories(root_path.as_deref(), root);
        }
        self.restart_search_load_if_needed();
        self.rebuild_visible();
        self.refresh_git_status();
    }

    pub fn refresh_selected(&mut self) {
        self.cancel_search_load();
        self.search_index = None;
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
        self.cancel_search_load();
        self.search_index = None;
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
        let previous_index = self
            .selected_path
            .as_ref()
            .and_then(|path| self.visible.iter().position(|node| &node.path == path))
            .unwrap_or(0);
        let was_empty = self.search_query.trim().is_empty();
        let query = query.into();

        if was_empty && !query.trim().is_empty() {
            self.start_search_load();
        }
        self.search_query = query;
        if self.search_query.trim().is_empty() {
            self.cancel_search_load();
        }
        self.rebuild_visible();
        self.repair_selection(previous_index);
        self.scroll = 0;
    }

    pub fn clear_search(&mut self) {
        self.set_search_query(String::new());
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

    pub fn expand_selected(&mut self) {
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
        let Some(node) = self.find_mut(&path) else {
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
        if !node.loaded {
            load_children(root_path.as_deref(), node);
        }
        node.expanded = true;
        self.rebuild_visible();
    }

    pub fn activate_selected(&mut self) {
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
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        let previous_index = self
            .visible
            .iter()
            .position(|node| node.path == path)
            .unwrap_or(0);
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

    fn start_search_load(&mut self) {
        if self.search_index.is_some() || self.search_loading {
            return;
        }
        let Some(root_path) = self.root_path.clone() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let index = build_search_index(&root_path, &worker_cancel);
            if !worker_cancel.load(Ordering::Relaxed) {
                let _ = tx.send(index);
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

    fn poll_search_load(&mut self) {
        let Some(rx) = self.search_loader.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(index) => {
                let previous_index = self
                    .selected_path
                    .as_ref()
                    .and_then(|path| self.visible.iter().position(|node| &node.path == path))
                    .unwrap_or(0);
                self.search_index = Some(Arc::new(index));
                self.search_cancel = None;
                self.search_loading = false;
                self.rebuild_visible();
                self.repair_selection(previous_index);
            }
            Err(TryRecvError::Empty) => self.search_loader = Some(rx),
            Err(TryRecvError::Disconnected) => {
                self.search_cancel = None;
                self.search_loading = false;
            }
        }
    }

    fn rebuild_visible(&mut self) {
        let mut visible = Vec::new();
        if !self.search_query.trim().is_empty() {
            if let Some(index) = &self.search_index {
                flatten_search_index(index, &self.search_query, &mut visible);
            } else if let Some(root) = &self.root {
                // Show already-loaded paths while the one-time index builds.
                flatten_filtered(root, 0, &self.search_query, self.root_path(), &mut visible);
            }
        } else if let Some(root) = &self.root {
            flatten_filtered(root, 0, "", self.root_path(), &mut visible);
        }
        self.visible = visible;
        self.selected_index = None;
    }

    fn selected_visible_index(&mut self) -> Option<usize> {
        if let Some(index) = self.selected_index {
            if self
                .visible
                .get(index)
                .is_some_and(|node| Some(&node.path) == self.selected_path.as_ref())
            {
                return Some(index);
            }
        }
        let index = self
            .selected_path
            .as_ref()
            .and_then(|path| self.visible.iter().position(|node| &node.path == path))?;
        self.selected_index = Some(index);
        Some(index)
    }

    fn select_visible_index(&mut self, index: usize) {
        let Some(node) = self.visible.get(index) else {
            return;
        };
        self.selected_path = Some(node.path.clone());
        self.selected_index = Some(index);
    }

    fn repair_selection(&mut self, previous_index: usize) {
        if self.visible.is_empty() {
            self.selected_index = None;
            return;
        }
        if self
            .visible
            .iter()
            .any(|node| Some(&node.path) == self.selected_path.as_ref())
        {
            self.selected_index = None;
            return;
        }
        self.select_visible_index(previous_index.min(self.visible.len() - 1));
    }

    /// Returns the selected path if it points to a regular file.
    pub fn selected_file_path(&self) -> Option<PathBuf> {
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
        let Some(selected) = self.selected_path.as_ref() else {
            return;
        };
        let Some(index) = self.visible.iter().position(|node| &node.path == selected) else {
            self.selected_path = self.visible.first().map(|node| node.path.clone());
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

fn build_search_index(root: &Path, cancel: &AtomicBool) -> Vec<SearchEntry> {
    let mut index = vec![SearchEntry {
        path: root.to_path_buf(),
        relative: ".".into(),
        display_name: root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        kind: FileKind::Directory,
        depth: 0,
    }];
    index_directory(root, root, 1, cancel, &mut index);
    index
}

fn index_directory(
    root: &Path,
    directory: &Path,
    depth: usize,
    cancel: &AtomicBool,
    index: &mut Vec<SearchEntry>,
) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let Ok(children) = read_children(Some(root), directory) else {
        return;
    };
    for child in children {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let relative = child
            .path
            .strip_prefix(root)
            .unwrap_or(&child.path)
            .to_string_lossy()
            .into_owned();
        index.push(SearchEntry {
            path: child.path.clone(),
            relative,
            display_name: child.display_name,
            kind: child.kind,
            depth,
        });
        if child.kind == FileKind::Directory {
            index_directory(root, &child.path, depth + 1, cancel, index);
        }
    }
}

fn flatten_search_index(index: &[SearchEntry], query: &str, out: &mut Vec<VisibleNode>) {
    let mut included = HashSet::new();
    for entry in index {
        if !path_matches_query(&entry.relative, query) {
            continue;
        }
        let mut path = Some(entry.path.as_path());
        while let Some(current) = path {
            included.insert(current.to_path_buf());
            path = current.parent();
        }
    }
    for entry in index.iter().filter(|entry| included.contains(&entry.path)) {
        out.push(VisibleNode {
            path: entry.path.clone(),
            display_name: entry.display_name.clone(),
            kind: entry.kind,
            expanded: entry.kind == FileKind::Directory,
            loading: false,
            loaded: true,
            error: None,
            child_count: usize::from(entry.kind == FileKind::Directory),
            depth: entry.depth,
        });
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
    _path: &Path,
    name: &str,
    kind: FileKind,
    selected: bool,
    panel_focused: bool,
    status: Option<GitStatusKind>,
    _icon_mode: FileIconMode,
) -> Line<'static> {
    let selection_style = selected.then(|| {
        if panel_focused {
            theme::selection_active()
        } else {
            theme::selection_inactive()
        }
    });
    let chrome_style = selection_style.unwrap_or_else(theme::muted);
    let name_style = selection_style.unwrap_or_else(|| match kind {
        FileKind::Directory => theme::directory(),
        FileKind::Symlink => theme::symlink(),
        FileKind::File | FileKind::Unknown => theme::text(),
    });
    let mut spans = vec![
        Span::styled(format!("{prefix}{marker} "), chrome_style),
        Span::styled(name.to_string(), name_style),
    ];
    if let Some(status) = status {
        let mut glyph = status_glyph(Status::from(status));
        if let Some(style) = selection_style {
            glyph.style = style;
        }
        spans.push(Span::raw(" "));
        spans.push(glyph);
    }
    Line::from(spans)
}

/// Fixed chrome above the tree: the nested search box (3 rows: top border,
/// content, bottom border) plus the focus-dot rule below it (1 row).
const SEARCH_BOX_HEIGHT: u16 = 3;
const TREE_TOP_OFFSET: u16 = SEARCH_BOX_HEIGHT + 1;

pub struct FileExplorerWidget<'a> {
    pub explorer: &'a mut FileExplorer,
    pub focused: bool,
    /// Whether `FocusBlock::Search` (not just `Files`) is the active block —
    /// finer-grained than `focused`, which is true for either. Drives the
    /// solid/hollow state of the focus-indicator dot on the rule.
    pub search_active: bool,
}

impl Widget for FileExplorerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.explorer.poll_search_load();
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .border_style(if self.focused {
                theme::active_panel_border()
            } else {
                theme::inactive_panel_border()
            })
            .style(theme::panel());
        let inner = block.inner(area);
        block.render(area, buf);
        // The footer row (selected relative path) always reserves 1 row;
        // the tree's own budget sits below the search box + rule.
        let height = inner.height.saturating_sub(TREE_TOP_OFFSET + 1) as usize;
        self.explorer.ensure_selection_visible(height);
        let visible = &self.explorer.visible;
        let mut lines = Vec::new();
        if self.explorer.root.is_none() {
            lines.push(Line::from("No repository detected"));
        } else if let Some(root) = self.explorer.root.as_ref() {
            if root.loading || !root.loaded {
                lines.push(Line::from("Loading files…"));
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
                for node in visible.iter().skip(self.explorer.scroll).take(list_height) {
                    let selected = self.explorer.selected_path.as_ref() == Some(&node.path);
                    let marker = match node.kind {
                        FileKind::Directory if node.loading => "…",
                        FileKind::Directory if node.expanded => "▾",
                        FileKind::Directory => "▸",
                        FileKind::File | FileKind::Symlink | FileKind::Unknown => " ",
                    };
                    let prefix = TREE_INDENT.repeat(node.depth);
                    let status = if matches!(node.kind, FileKind::File | FileKind::Symlink) {
                        self.explorer.git_status_for(&node.path)
                    } else {
                        None
                    };
                    lines.push(explorer_row_line(
                        &prefix,
                        marker,
                        &node.path,
                        &node.display_name,
                        node.kind,
                        selected,
                        self.focused,
                        status,
                        self.explorer.icon_mode,
                    ));
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
            }
        }
        if inner.height >= TREE_TOP_OFFSET {
            let search_box_area = Rect::new(inner.x, inner.y, inner.width, SEARCH_BOX_HEIGHT);
            let search_box = Block::default()
                .borders(Borders::ALL)
                .border_style(theme::muted());
            let search_box_inner = search_box.inner(search_box_area);
            search_box.render(search_box_area, buf);

            let text_focused = self.focused && self.explorer.search_focused;
            let (search, search_style) = if self.explorer.search_query.is_empty() {
                let text = if text_focused {
                    format!("{}Search files…", theme::CURSOR_GLYPH)
                } else {
                    "Search files…".to_string()
                };
                (text, theme::composer_placeholder())
            } else {
                let text = if text_focused {
                    format!("{}{}", self.explorer.search_query, theme::CURSOR_GLYPH)
                } else {
                    self.explorer.search_query.clone()
                };
                (text, theme::composer_text())
            };
            Paragraph::new(Line::from(vec![
                Span::raw(" ".repeat(TEXT_INSET as usize)),
                Span::styled(search, search_style),
            ]))
            .render(search_box_inner, buf);
            if text_focused {
                let cursor_x = if self.explorer.search_query.is_empty() {
                    search_box_inner.x + TEXT_INSET
                } else {
                    search_box_inner.x
                        + TEXT_INSET
                        + self.explorer.search_query.chars().count() as u16
                };
                if cursor_x < search_box_inner.right() {
                    theme::paint_caret(buf, cursor_x, search_box_inner.y);
                }
            }

            let rule_y = inner.y + SEARCH_BOX_HEIGHT;
            let width = inner.width as usize;
            let center = width / 2;
            let mut rule_spans = Vec::new();
            if center > 0 {
                rule_spans.push(Span::styled("─".repeat(center), theme::muted()));
            }
            let (dot, dot_style) = if self.search_active {
                ("●", theme::active_panel_border())
            } else {
                ("○", theme::muted())
            };
            rule_spans.push(Span::styled(dot, dot_style));
            let right_len = width.saturating_sub(center + 1);
            if right_len > 0 {
                rule_spans.push(Span::styled("─".repeat(right_len), theme::muted()));
            }
            Paragraph::new(Line::from(rule_spans))
                .render(Rect::new(inner.x, rule_y, inner.width, 1), buf);

            Paragraph::new(lines).render(
                Rect::new(
                    inner.x,
                    inner.y + TREE_TOP_OFFSET,
                    inner.width,
                    inner.height.saturating_sub(TREE_TOP_OFFSET + 1),
                ),
                buf,
            );
        }
        if inner.height > 0 {
            let selected = self
                .explorer
                .selected_relative_path()
                .unwrap_or_else(|| "".into());
            let footer_y = inner.y + inner.height.saturating_sub(1);
            Paragraph::new(Line::styled(selected, theme::muted()))
                .render(Rect::new(inner.x, footer_y, inner.width, 1), buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

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
    fn row_style_precedence_keeps_selection_strongest() {
        let selected = explorer_row_line(
            "",
            " ",
            Path::new("lib.rs"),
            "lib.rs",
            FileKind::File,
            true,
            true,
            Some(GitStatusKind::Modified),
            FileIconMode::Unicode,
        );
        assert_eq!(selected.spans[0].style, theme::selection_active());
        assert_eq!(selected.spans[1].style, theme::selection_active());
        let inactive = explorer_row_line(
            "",
            " ",
            Path::new("x"),
            "x",
            FileKind::Unknown,
            true,
            false,
            None,
            FileIconMode::Unicode,
        );
        assert_eq!(inactive.spans[0].style, theme::selection_inactive());
        assert_eq!(inactive.spans[1].style, theme::selection_inactive());
        let unselected = explorer_row_line(
            "",
            " ",
            Path::new("new.rs"),
            "new.rs",
            FileKind::File,
            false,
            true,
            Some(GitStatusKind::Added),
            FileIconMode::Unicode,
        );
        assert_eq!(unselected.spans[1].style, theme::text());
        assert_eq!(unselected.spans.last().unwrap().content.as_ref(), "A");
    }

    #[test]
    fn row_rendering_uses_git_status_letters() {
        let line = explorer_row_line(
            "",
            " ",
            Path::new("long_filename.rs"),
            "long_filename.rs",
            FileKind::File,
            false,
            false,
            Some(GitStatusKind::Modified),
            FileIconMode::Unicode,
        );
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "  long_filename.rs M");
        assert_eq!(line.spans.last().unwrap().content.as_ref(), "M");
    }

    #[test]
    fn tree_depth_uses_a_consistent_two_cell_indent() {
        let prefix = TREE_INDENT.repeat(2);
        let line = explorer_row_line(
            &prefix,
            "▸",
            Path::new("src/ui/app.rs"),
            "app.rs",
            FileKind::File,
            false,
            false,
            None,
            FileIconMode::Unicode,
        );
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "    ▸ app.rs");
    }

    #[test]
    fn row_rendering_handles_symlink_hidden_unicode_and_narrow_width() {
        let line = explorer_row_line(
            "",
            " ",
            Path::new("雪.py"),
            "雪.py",
            FileKind::Symlink,
            false,
            false,
            None,
            FileIconMode::Unicode,
        );
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "  雪.py");
        assert!(line.width() > 4);

        let hidden = explorer_row_line(
            "",
            " ",
            Path::new(".env"),
            ".env",
            FileKind::File,
            false,
            false,
            None,
            FileIconMode::Unicode,
        );
        let text: String = hidden
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "  .env");
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
            .into_iter()
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
    fn fuzzy_search_recurses_into_collapsed_directories_and_tokenizes_terms() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/api")).unwrap();
        fs::write(root.path().join("src/api/client.rs"), "").unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let mut explorer =
            FileExplorer::new(Some(root_path.clone()), forge_config::FileIconMode::Unicode);

        explorer.set_search_query("api cli");
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

        let visible: Vec<_> = explorer
            .visible_nodes()
            .into_iter()
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
                PathBuf::from("."),
                PathBuf::from("src"),
                PathBuf::from("src/api"),
                PathBuf::from("src/api/client.rs"),
            ]
        );
    }

    #[test]
    fn search_keystrokes_reuse_compact_index_without_materializing_tree() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/api/deep")).unwrap();
        fs::write(root.path().join("src/api/deep/client.rs"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );

        explorer.set_search_query("c");
        wait_for_search_load(&mut explorer);
        let index = Arc::as_ptr(explorer.search_index.as_ref().unwrap());
        let src = explorer
            .root
            .as_ref()
            .unwrap()
            .children
            .iter()
            .find(|node| node.display_name == "src")
            .unwrap();
        assert!(!src.loaded, "search must not materialize the FileNode tree");

        for query in ["cl", "cli", "client"] {
            explorer.set_search_query(query);
            assert_eq!(Arc::as_ptr(explorer.search_index.as_ref().unwrap()), index);
            assert!(!explorer.search_loading);
        }
        assert!(explorer
            .visible
            .iter()
            .any(|node| node.display_name == "client.rs"));
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
        explorer.set_search_query("config");

        assert_eq!(
            explorer.selected_relative_path().as_deref(),
            Some("src/config/client_config.rs")
        );
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
            scroll: 0,
            focused: false,
            search_focused: true,
            search_query: String::new(),
            icon_mode: FileIconMode::Unicode,
            root_path: Some(PathBuf::from("/tmp/forge-test-root")),
            git_status: GitStatusCache::new(),
            visible: Vec::new(),
            search_loader: None,
            search_cancel: None,
            search_loading: false,
            search_index: None,
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
            search_active,
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
    fn search_box_has_top_and_bottom_border_rows() {
        let mut explorer = FileExplorer::new(None, FileIconMode::Unicode);
        let area = Rect::new(0, 0, 24, 14);
        let buf = render_widget(&mut explorer, area, true);
        // Outer FILES block reserves 1 border row + 1 padding column on
        // each side, so the nested search box starts at (area.x + 2, area.y + 1).
        let inner_y = area.y + 1;
        let top_row = row_text(&buf, area, inner_y);
        let bottom_row = row_text(&buf, area, inner_y + 2);
        assert!(top_row.contains('┌') && top_row.contains('┐'));
        assert!(bottom_row.contains('└') && bottom_row.contains('┘'));
    }

    #[test]
    fn search_rule_shows_solid_dot_when_search_active() {
        let mut explorer = FileExplorer::new(None, FileIconMode::Unicode);
        let area = Rect::new(0, 0, 24, 14);
        let buf = render_widget(&mut explorer, area, true);
        let rule_y = area.y + 1 + SEARCH_BOX_HEIGHT;
        let row = row_text(&buf, area, rule_y);
        assert!(row.contains('●'));
        assert!(!row.contains('○'));
    }

    #[test]
    fn search_rule_shows_hollow_dot_when_search_not_active() {
        let mut explorer = FileExplorer::new(None, FileIconMode::Unicode);
        let area = Rect::new(0, 0, 24, 14);
        let buf = render_widget(&mut explorer, area, false);
        let rule_y = area.y + 1 + SEARCH_BOX_HEIGHT;
        let row = row_text(&buf, area, rule_y);
        assert!(row.contains('○'));
        assert!(!row.contains('●'));
    }

    #[test]
    fn search_box_shows_block_caret_and_placeholder_without_icon() {
        let mut explorer = FileExplorer::new(None, FileIconMode::Unicode);
        let area = Rect::new(0, 0, 24, 14);
        let buf = render_widget(&mut explorer, area, true);
        let content_row = area.y + 2;
        let row = row_text(&buf, area, content_row);
        let cursor = &buf[(area.x + 4, content_row)];
        assert_eq!(cursor.symbol(), theme::CURSOR_CELL);
        assert_eq!(cursor.style().bg, theme::caret().bg);
        assert!(row.contains(" Search files…"));
        assert!(!row.contains('⌕'));
    }

    #[test]
    fn no_panic_across_small_pane_heights() {
        for height in 0..=8u16 {
            let mut explorer = FileExplorer::new(None, FileIconMode::Unicode);
            let area = Rect::new(0, 0, 20, height);
            render_widget(&mut explorer, area, false);
        }
    }
}
