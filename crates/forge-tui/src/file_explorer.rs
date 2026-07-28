use std::fs;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::git_status::{GitStatusCache, GitStatusKind};
use crate::theme;

const HIDDEN_DIRS: &[&str] = &[".git", "target", ".forge"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    File,
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
    pub scroll: usize,
    pub focused: bool,
    root_path: Option<PathBuf>,
    pub git_status: GitStatusCache,
}

impl FileExplorer {
    pub fn new(root_path: Option<PathBuf>) -> Self {
        let root_path = root_path.map(|p| p.canonicalize().unwrap_or(p));
        let mut explorer = Self {
            root: root_path.clone().map(FileNode::root),
            selected_path: root_path.clone(),
            scroll: 0,
            focused: false,
            root_path: root_path.clone(),
            git_status: GitStatusCache::new(),
        };
        explorer.load_root();
        if let Some(root) = root_path {
            explorer.git_status.start_refresh(root);
        }
        explorer
    }

    pub fn load_root(&mut self) {
        if let Some(root) = self.root.as_mut() {
            load_children(self.root_path.as_deref(), root);
        }
    }

    pub fn refresh_selected(&mut self) {
        let selected = self.selected_path.clone();
        let root_path = self.root_path.clone();
        if let Some(path) = selected {
            if let Some(node) = self.find_mut(&path) {
                if node.kind == FileKind::Directory {
                    node.loaded = false;
                    load_children(root_path.as_deref(), node);
                    node.expanded = true;
                    if let Some(root) = root_path {
                        self.git_status.start_refresh(root);
                    }
                    return;
                }
            }
        }
        self.load_root();
        if let Some(root) = root_path {
            self.git_status.start_refresh(root);
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focused = !self.focused;
    }

    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            flatten(root, 0, &mut out);
        }
        out
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

    pub fn git_status_for(&self, path: &Path) -> Option<GitStatusKind> {
        let root = self.root_path.as_ref()?;
        let rel = path.strip_prefix(root).ok()?;
        self.git_status.get(rel)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let visible = self.visible_nodes();
        if visible.is_empty() {
            return;
        }
        let current = self
            .selected_path
            .as_ref()
            .and_then(|path| visible.iter().position(|node| &node.path == path))
            .unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(visible.len() - 1);
        self.selected_path = Some(visible[next].path.clone());
    }

    pub fn expand_selected(&mut self) {
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        let root_path = self.root_path.clone();
        if let Some(node) = self.find_mut(&path) {
            if node.kind == FileKind::Directory {
                if !node.loaded {
                    load_children(root_path.as_deref(), node);
                }
                node.expanded = true;
                if let Some(root) = root_path {
                    self.git_status.start_refresh(root);
                }
            }
        }
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
        if let Some(node) = self.find_mut(&path) {
            if node.kind == FileKind::Directory && node.expanded {
                node.expanded = false;
                return;
            }
        }
        if let Some(parent) = path.parent() {
            if self.contains(parent) {
                self.selected_path = Some(parent.to_path_buf());
            }
        }
    }

    /// Returns the selected path if it points to a regular file.
    pub fn selected_file_path(&self) -> Option<PathBuf> {
        let path = self.selected_path.as_ref()?;
        self.find(path)
            .filter(|node| node.kind == FileKind::File)
            .map(|_| path.clone())
    }

    pub fn ensure_selection_visible(&mut self, height: usize) {
        let visible = self.visible_nodes();
        let Some(selected) = self.selected_path.as_ref() else {
            return;
        };
        let Some(index) = visible.iter().position(|node| &node.path == selected) else {
            self.selected_path = visible.first().map(|node| node.path.clone());
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

fn flatten(node: &FileNode, depth: usize, out: &mut Vec<VisibleNode>) {
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
    if node.kind == FileKind::Directory && node.expanded {
        for child in &node.children {
            flatten(child, depth + 1, out);
        }
    }
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
    let entries = fs::read_dir(&dir).map_err(|error| error.to_string())?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if should_hide(&path) {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            if safe_path(root, &path).is_err() {
                continue;
            }
        }
        let kind = if file_type.is_dir() {
            FileKind::Directory
        } else {
            FileKind::File
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

fn should_hide(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| HIDDEN_DIRS.contains(&name) || name.starts_with('.'))
}

fn sort_nodes(nodes: &mut [FileNode]) {
    nodes.sort_by(|a, b| {
        (a.kind != FileKind::Directory, a.display_name.to_lowercase())
            .cmp(&(b.kind != FileKind::Directory, b.display_name.to_lowercase()))
    });
}

pub struct FileExplorerWidget<'a> {
    pub explorer: &'a mut FileExplorer,
}

impl Widget for FileExplorerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.explorer.git_status.poll();
        let title = if self.explorer.focused {
            "FILES *"
        } else {
            "FILES"
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(if self.explorer.focused {
                theme::brand()
            } else {
                theme::border()
            });
        let inner = block.inner(area);
        block.render(area, buf);
        let height = inner.height.saturating_sub(1) as usize;
        self.explorer.ensure_selection_visible(height);
        let visible = self.explorer.visible_nodes();
        let mut lines = Vec::new();
        if self.explorer.root.is_none() {
            lines.push(Line::from("No repository detected"));
        } else if visible.len() == 1 && visible[0].error.is_none() {
            lines.push(Line::from("This directory is empty"));
        } else {
            for node in visible.iter().skip(self.explorer.scroll).take(height) {
                let selected = self.explorer.selected_path.as_ref() == Some(&node.path);
                let marker = match node.kind {
                    FileKind::Directory if node.loading => "…",
                    FileKind::Directory if node.expanded => "▾",
                    FileKind::Directory => "▸",
                    FileKind::File => " ",
                };
                let prefix = "  ".repeat(node.depth);
                let status_marker = if node.kind == FileKind::File {
                    self.explorer
                        .git_status_for(&node.path)
                        .map(|s| format!("{} ", s.marker()))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let mut line = Line::from(vec![Span::raw(format!(
                    "{prefix}{marker} {status_marker}{}",
                    node.display_name
                ))]);
                if selected {
                    line.style = theme::brand();
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
        Paragraph::new(lines).render(
            Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(1),
            ),
            buf,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut explorer = FileExplorer::new(Some(root.path().to_path_buf()));
        assert_eq!(explorer.visible_nodes().len(), 2);
        explorer.selected_path = Some(root.path().join("src").canonicalize().unwrap());
        explorer.expand_selected();
        assert_eq!(explorer.visible_nodes().len(), 3);
        explorer.collapse_selected();
        assert_eq!(explorer.visible_nodes().len(), 2);
    }

    #[test]
    fn selection_moves_within_visible_nodes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a"), "").unwrap();
        fs::write(root.path().join("b"), "").unwrap();
        let mut explorer = FileExplorer::new(Some(root.path().to_path_buf()));
        explorer.move_selection(1);
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
}
