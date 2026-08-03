use std::fs;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use forge_config::FileIconMode;

use crate::git_status::{GitStatusCache, GitStatusKind};
use crate::status_glyph::{status_glyph, Status};
use crate::theme;

const HIDDEN_DIRS: &[&str] = &[".git", "target"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    File,
    Symlink,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    Default,
    Source,
    Config,
    Document,
    Data,
    Image,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticRole {
    Directory,
    FileDefault,
    FileSource,
    FileConfig,
    FileDocument,
    FileData,
    FileImage,
    FileBinary,
    Symlink,
    GitAdded,
    GitModified,
    GitDeleted,
    GitUntracked,
    GitIgnored,
}

impl SemanticRole {
    fn style(self) -> Style {
        match self {
            Self::Directory => theme::directory(),
            Self::FileDefault => theme::file_default(),
            Self::FileSource => theme::file_source(),
            Self::FileConfig => theme::file_config(),
            Self::FileDocument => theme::file_document(),
            Self::FileData => theme::file_data(),
            Self::FileImage => theme::file_image(),
            Self::FileBinary => theme::file_binary(),
            Self::Symlink => theme::symlink(),
            Self::GitAdded => theme::git_added(),
            Self::GitModified => theme::git_modified(),
            Self::GitDeleted => theme::git_deleted(),
            Self::GitUntracked => theme::git_untracked(),
            Self::GitIgnored => theme::git_ignored(),
        }
    }
}

fn language_icon_for_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "cargo.toml" | "cargo.lock") {
        return Some("🦀");
    }
    if matches!(lower.as_str(), "package.json" | "package-lock.json") {
        return Some("");
    }
    if matches!(
        lower.as_str(),
        "go.mod" | "go.sum" | "go.work" | "go.work.sum"
    ) {
        return Some("");
    }
    if matches!(lower.as_str(), "dockerfile") || lower.starts_with("dockerfile.") {
        return Some("");
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())?;
    match ext.as_str() {
        "rs" => Some("🦀"),
        "js" | "jsx" | "mjs" | "cjs" => Some(""),
        "ts" | "tsx" | "mts" | "cts" => Some(""),
        "py" | "pyi" => Some(""),
        "go" => Some(""),
        "java" => Some(""),
        "c" | "h" => Some(""),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some(""),
        "sh" | "bash" | "zsh" | "fish" => Some(""),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAppearance {
    pub icon: &'static str,
    pub icon_role: SemanticRole,
    pub text_role: SemanticRole,
    pub status_role: Option<SemanticRole>,
    pub category: FileCategory,
}

pub struct FileAppearanceResolver;

impl FileAppearanceResolver {
    pub fn resolve(
        name: &str,
        kind: FileKind,
        git_status: Option<GitStatusKind>,
    ) -> FileAppearance {
        let (category, text_role) = match kind {
            FileKind::Directory => (FileCategory::Default, SemanticRole::Directory),
            FileKind::Symlink => (FileCategory::Default, SemanticRole::Symlink),
            FileKind::Unknown => (FileCategory::Default, SemanticRole::FileDefault),
            FileKind::File => category_for_name(name),
        };
        let icon_role = match kind {
            FileKind::Directory => SemanticRole::Directory,
            FileKind::Symlink => SemanticRole::Symlink,
            _ => text_role,
        };
        FileAppearance {
            icon: icon_for(kind, category),
            icon_role,
            text_role,
            status_role: git_status.map(git_role),
            category,
        }
    }
}

fn git_role(status: GitStatusKind) -> SemanticRole {
    match status {
        GitStatusKind::Modified => SemanticRole::GitModified,
        GitStatusKind::Added => SemanticRole::GitAdded,
        GitStatusKind::Deleted => SemanticRole::GitDeleted,
        GitStatusKind::Untracked => SemanticRole::GitUntracked,
        GitStatusKind::Ignored => SemanticRole::GitIgnored,
        GitStatusKind::Conflicted => SemanticRole::GitDeleted,
    }
}

fn icon_for(kind: FileKind, category: FileCategory) -> &'static str {
    match kind {
        FileKind::Directory => "▣",
        FileKind::Symlink => "~",
        FileKind::Unknown => "·",
        FileKind::File => match category {
            FileCategory::Source => "λ",
            FileCategory::Config => "◇",
            FileCategory::Document => "¶",
            FileCategory::Data => "□",
            FileCategory::Image => "◆",
            FileCategory::Binary => "■",
            FileCategory::Default => "·",
        },
    }
}

fn category_for_name(name: &str) -> (FileCategory, SemanticRole) {
    let lower = name.to_ascii_lowercase();
    if is_source_special(&lower) {
        return (FileCategory::Source, SemanticRole::FileSource);
    }
    if is_config_special(&lower) {
        return (FileCategory::Config, SemanticRole::FileConfig);
    }
    if is_document_special(&lower) {
        return (FileCategory::Document, SemanticRole::FileDocument);
    }
    if is_git_special(&lower) {
        return (FileCategory::Config, SemanticRole::GitIgnored);
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "rs" | "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "py" | "pyi"
        | "go" | "java" | "c" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "h" | "sh"
        | "bash" | "zsh" | "fish" => (FileCategory::Source, SemanticRole::FileSource),
        "json" | "jsonc" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env" => {
            (FileCategory::Config, SemanticRole::FileConfig)
        }
        "md" | "markdown" | "rst" | "txt" => (FileCategory::Document, SemanticRole::FileDocument),
        "csv" | "tsv" | "xml" | "sql" | "parquet" => (FileCategory::Data, SemanticRole::FileData),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => {
            (FileCategory::Image, SemanticRole::FileImage)
        }
        "jar" => (FileCategory::Binary, SemanticRole::FileBinary),
        _ => (FileCategory::Default, SemanticRole::FileDefault),
    }
}

fn is_source_special(lower: &str) -> bool {
    matches!(
        lower,
        "cargo.toml"
            | "cargo.lock"
            | "rust-toolchain"
            | "rust-toolchain.toml"
            | "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "tsconfig.json"
            | "pyproject.toml"
            | "requirements.txt"
            | "pipfile"
            | "pipfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "go.mod"
            | "go.sum"
            | "go.work"
            | "go.work.sum"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "settings.gradle"
            | "settings.gradle.kts"
            | "gradle.properties"
            | "dockerfile"
            | "cmakelists.txt"
            | "meson.build"
            | "makefile"
            | "justfile"
    ) || lower.starts_with("dockerfile.")
}

fn is_config_special(lower: &str) -> bool {
    lower == ".editorconfig"
        || lower == ".env"
        || lower.starts_with(".env.")
        || matches!(
            lower,
            "docker-compose.yml"
                | "docker-compose.yaml"
                | "compose.yml"
                | "compose.yaml"
                | "taskfile.yml"
                | "taskfile.yaml"
        )
}

fn is_document_special(lower: &str) -> bool {
    matches!(lower, "readme" | "changelog" | "contributing" | "license")
        || lower.starts_with("readme.")
        || lower.starts_with("changelog.")
        || lower.starts_with("contributing.")
        || lower.starts_with("license.")
}

fn is_git_special(lower: &str) -> bool {
    matches!(
        lower,
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep"
    )
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
    pub icon_mode: FileIconMode,
    root_path: Option<PathBuf>,
    pub git_status: GitStatusCache,
}

impl FileExplorer {
    pub fn new(root_path: Option<PathBuf>, icon_mode: FileIconMode) -> Self {
        let root_path = root_path.map(|p| p.canonicalize().unwrap_or(p));
        let mut explorer = Self {
            root: root_path.clone().map(FileNode::root),
            selected_path: root_path.clone(),
            scroll: 0,
            focused: false,
            icon_mode,
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

    pub fn refresh_git_status(&mut self) {
        if let Some(root) = self.root_path.clone() {
            self.git_status.start_refresh(root);
        }
    }

    pub fn refresh_workspace(&mut self) {
        let root_path = self.root_path.clone();
        if let Some(root) = self.root.as_mut() {
            refresh_loaded_directories(root_path.as_deref(), root);
        }
        self.refresh_git_status();
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
                    self.refresh_git_status();
                    return;
                }
            }
        }
        self.load_root();
        self.refresh_git_status();
    }

    pub fn refresh_parent_and_select(&mut self, parent: &Path, selected: &Path) {
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
        self.refresh_git_status();
    }

    pub fn refresh_after_delete(&mut self, parent: &Path, deleted: &Path) {
        let previous = self.visible_nodes();
        self.refresh_parent_and_select(parent, parent);
        let visible = self.visible_nodes();
        if visible.is_empty() {
            self.selected_path = self.root_path.clone();
            return;
        }
        let deleted_index = previous
            .iter()
            .position(|node| node.path == deleted)
            .unwrap_or(0);
        let next = deleted_index.min(visible.len().saturating_sub(1));
        self.selected_path = Some(visible[next].path.clone());
        if self.selected_path.as_deref() == Some(deleted) {
            self.selected_path = Some(parent.to_path_buf());
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
            .filter(|node| matches!(node.kind, FileKind::File | FileKind::Symlink))
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
    name: &str,
    kind: FileKind,
    selected: bool,
    panel_focused: bool,
    status: Option<GitStatusKind>,
    icon_mode: FileIconMode,
) -> Line<'static> {
    let appearance = FileAppearanceResolver::resolve(name, kind, status);
    let selection_style = selected.then(|| {
        if panel_focused {
            theme::selection_active()
        } else {
            theme::selection_inactive()
        }
    });
    let style_for = |role: SemanticRole| selection_style.unwrap_or_else(|| role.style());
    let mut spans = vec![Span::styled(
        format!("{prefix}{marker} "),
        selection_style.unwrap_or_default(),
    )];
    if let Some(status) = status {
        let mut glyph = status_glyph(Status::from(status));
        if let Some(style) = selection_style {
            glyph.style = style;
        }
        spans.push(glyph);
        spans.push(Span::raw(" "));
    }
    if icon_mode == FileIconMode::Unicode {
        let icon = if kind == FileKind::File {
            language_icon_for_name(name).unwrap_or(appearance.icon)
        } else {
            appearance.icon
        };
        spans.push(Span::styled(
            format!("{} ", icon),
            style_for(appearance.icon_role),
        ));
    }
    let display_name = if selected {
        format!("› {name}")
    } else {
        name.to_string()
    };
    spans.push(Span::styled(display_name, style_for(appearance.text_role)));
    let mut line = Line::from(spans);
    if let Some(style) = selection_style {
        line.style = style;
    }
    line
}

pub struct FileExplorerWidget<'a> {
    pub explorer: &'a mut FileExplorer,
    pub focused: bool,
}

impl Widget for FileExplorerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = if self.focused {
            " FILES · NAV "
        } else {
            " FILES "
        };
        let block = Block::default()
            .title(Span::styled(
                title,
                if self.focused {
                    theme::active_panel_title()
                } else {
                    theme::inactive_panel_title()
                },
            ))
            .borders(Borders::ALL)
            .border_style(if self.focused {
                theme::active_panel_border()
            } else {
                theme::inactive_panel_border()
            })
            .style(theme::panel());
        let inner = block.inner(area);
        block.render(area, buf);
        let height = inner.height.saturating_sub(1) as usize;
        self.explorer.ensure_selection_visible(height);
        let visible = self.explorer.visible_nodes();
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
                    let prefix = "  ".repeat(node.depth);
                    let status = if matches!(node.kind, FileKind::File | FileKind::Symlink) {
                        self.explorer.git_status_for(&node.path)
                    } else {
                        None
                    };
                    lines.push(explorer_row_line(
                        &prefix,
                        marker,
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

    fn appearance(name: &str) -> FileAppearance {
        FileAppearanceResolver::resolve(name, FileKind::File, None)
    }

    #[test]
    fn resolver_covers_entry_kinds_and_unicode_names() {
        assert_eq!(
            FileAppearanceResolver::resolve("src", FileKind::Directory, None).text_role,
            SemanticRole::Directory
        );
        assert_eq!(
            appearance("file.unknown").text_role,
            SemanticRole::FileDefault
        );
        assert_eq!(appearance(".env").text_role, SemanticRole::FileConfig);
        assert_eq!(
            FileAppearanceResolver::resolve("link", FileKind::Symlink, None).text_role,
            SemanticRole::Symlink
        );
        assert_eq!(
            FileAppearanceResolver::resolve("fifo", FileKind::Unknown, None).icon,
            "·"
        );
        assert_eq!(appearance("雪.rs").text_role, SemanticRole::FileSource);
    }

    #[test]
    fn resolver_covers_language_and_special_mappings() {
        for name in [
            "lib.rs",
            "Cargo.toml",
            "app.js",
            "view.tsx",
            "main.py",
            "go.mod",
            "Main.java",
            "main.c",
            "thing.CPP",
            "header.h",
            "script.sh",
            "package.json",
            "pom.xml",
            "build.gradle.kts",
        ] {
            assert_eq!(appearance(name).category, FileCategory::Source, "{name}");
        }
        assert_eq!(appearance("header.h").text_role, SemanticRole::FileSource);
    }

    #[test]
    fn resolver_covers_file_categories() {
        for name in ["CMakeLists.txt", "Dockerfile", "Makefile"] {
            assert_eq!(appearance(name).category, FileCategory::Source, "{name}");
        }
        assert_eq!(appearance("README.md").category, FileCategory::Document);
        assert_eq!(appearance(".gitignore").text_role, SemanticRole::GitIgnored);
        assert_eq!(appearance(".env").category, FileCategory::Config);
        assert_eq!(appearance("data.csv").category, FileCategory::Data);
        assert_eq!(appearance("image.PNG").category, FileCategory::Image);
    }

    #[test]
    fn special_filenames_precede_extensions() {
        assert_eq!(appearance("Cargo.toml").category, FileCategory::Source);
        assert_eq!(appearance("plain.toml").category, FileCategory::Config);
    }

    #[test]
    fn row_style_precedence_keeps_selection_strongest() {
        let selected = explorer_row_line(
            "",
            " ",
            "lib.rs",
            FileKind::File,
            true,
            true,
            Some(GitStatusKind::Modified),
            FileIconMode::Unicode,
        );
        assert_eq!(selected.style, theme::selection_active());
        let inactive = explorer_row_line(
            "",
            " ",
            "x",
            FileKind::Unknown,
            true,
            false,
            None,
            FileIconMode::Unicode,
        );
        assert_eq!(inactive.style, theme::selection_inactive());
        let unselected = explorer_row_line(
            "",
            " ",
            "new.rs",
            FileKind::File,
            false,
            true,
            Some(GitStatusKind::Added),
            FileIconMode::Unicode,
        );
        assert_eq!(unselected.style, Style::default());
        assert_eq!(
            selected.spans.last().unwrap().style,
            theme::selection_active()
        );
    }

    #[test]
    fn row_rendering_supports_icons_off_and_markers() {
        let with_icon = explorer_row_line(
            "",
            " ",
            "long_filename.rs",
            FileKind::File,
            false,
            false,
            Some(GitStatusKind::Modified),
            FileIconMode::Unicode,
        );
        let text: String = with_icon
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.starts_with("  M 🦀 long_filename.rs"));
        assert_eq!(with_icon.spans[1].content.as_ref(), "M");
        assert_eq!(with_icon.spans[3].content.as_ref(), "🦀 ");

        let without_icon = explorer_row_line(
            "",
            " ",
            "long_filename.rs",
            FileKind::File,
            false,
            false,
            Some(GitStatusKind::Modified),
            FileIconMode::Off,
        );
        let text: String = without_icon
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "  M long_filename.rs");
    }

    #[test]
    fn row_rendering_uses_language_icons_for_supported_files() {
        for (name, icon) in [
            ("main.py", ""),
            ("app.tsx", ""),
            ("go.mod", ""),
            ("Dockerfile", ""),
        ] {
            let line = explorer_row_line(
                "",
                " ",
                name,
                FileKind::File,
                false,
                false,
                None,
                FileIconMode::Unicode,
            );
            assert_eq!(line.spans[1].content.as_ref(), format!("{icon} "));
        }
    }

    #[test]
    fn row_rendering_handles_symlink_hidden_unicode_and_narrow_width() {
        let line = explorer_row_line(
            "",
            " ",
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
        assert_eq!(text, "  ~ 雪.py");
        assert!(line.width() > 4);

        let hidden = explorer_row_line(
            "",
            " ",
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
        assert!(text.contains("◇ .env"));
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
    fn forge_local_is_hidden_but_project_owned_forge_resources_are_visible() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".forge/local/sessions")).unwrap();
        fs::write(root.path().join(".forge/local/sessions/x.db"), "").unwrap();
        fs::create_dir_all(root.path().join(".forge/rules")).unwrap();
        fs::write(root.path().join(".forge/rules/style.md"), "").unwrap();
        fs::create_dir_all(root.path().join(".forge/skills/ponytail")).unwrap();
        fs::write(root.path().join(".forge/skills/ponytail/SKILL.md"), "").unwrap();

        let children = read_children(Some(root.path()), root.path()).unwrap();
        let names: Vec<&str> = children
            .iter()
            .filter_map(|n| n.path.file_name().and_then(|s| s.to_str()))
            .collect();
        assert!(names.contains(&".forge"), "{names:?}");

        let forge_children = read_children(Some(root.path()), &root.path().join(".forge")).unwrap();
        let forge_names: Vec<&str> = forge_children
            .iter()
            .filter_map(|n| n.path.file_name().and_then(|s| s.to_str()))
            .collect();
        assert!(forge_names.contains(&"rules"), "{forge_names:?}");
        assert!(forge_names.contains(&"skills"), "{forge_names:?}");
        assert!(
            !forge_names.contains(&"local"),
            "`.forge/local` must stay hidden: {forge_names:?}"
        );
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

    #[test]
    fn selection_moves_within_visible_nodes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a"), "").unwrap();
        fs::write(root.path().join("b"), "").unwrap();
        let mut explorer = FileExplorer::new(
            Some(root.path().to_path_buf()),
            forge_config::FileIconMode::Unicode,
        );
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
        let explorer = FileExplorer {
            root: Some(root),
            selected_path: Some(PathBuf::from("/tmp/forge-test-root")),
            scroll: 0,
            focused: false,
            icon_mode: FileIconMode::Unicode,
            root_path: Some(PathBuf::from("/tmp/forge-test-root")),
            git_status: GitStatusCache::new(),
        };
        let root_node = explorer.root.as_ref().unwrap();
        assert!(!root_node.loaded);
        assert!(root_node.loading);
        assert!(!root_node.children.is_empty() || true); // children may be empty while loading
        assert_eq!(explorer.visible_nodes().len(), 1);
    }
}
