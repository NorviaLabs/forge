//! Source viewer and Forge-owned chrome for the workspace Editor tab.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Widget};

use crate::editor_session::EditorSession;
use crate::file_explorer::safe_path;
use crate::theme;

/// How many bytes to inspect for binary detection.
const BINARY_PROBE_BYTES: usize = 8_192;
/// Tab stops are every N columns.
const TAB_WIDTH: usize = 4;

/// Legacy mode tag used by the non-editable fallback viewer. Editable text
/// files derive their mode from [`EditorSession`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerMode {
    #[default]
    Normal,
    Insert,
}

impl ViewerMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerStatus {
    Empty,
    Loading,
    Ok,
    Binary,
    InvalidUtf8,
    NotFound,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub open: bool,
    pub query: String,
    pub matches: Vec<Match>,
    pub active_index: usize,
    pub pre_line: usize,
    pub pre_top: usize,
    pub pre_h_scroll: usize,
}

#[derive(Debug, Clone, Default)]
pub struct JumpState {
    pub open: bool,
    pub input: String,
    pub pre_line: usize,
    pub pre_top: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    fn from_metadata(meta: &fs::Metadata) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self {
                len: meta.len(),
                modified: meta.modified().ok(),
                dev: meta.dev(),
                ino: meta.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {
                len: meta.len(),
                modified: meta.modified().ok(),
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceViewer {
    /// Canonical absolute path of the open file, if any.
    pub path: Option<PathBuf>,
    /// Path relative to the workspace root.
    pub rel_path: String,
    /// Decoded text lines (possibly a limited preview).
    pub lines: Vec<String>,
    /// Exact validated UTF-8 document text for the editable session.
    pub(crate) document_text: Option<String>,
    /// First visible line index (0-based).
    pub top_line: usize,
    /// Current/cursor line index (0-based).
    pub current_line: usize,
    /// Horizontal scroll offset in display columns.
    pub h_scroll: usize,
    /// Whether the editor panel currently has focus. Affects current-line emphasis.
    pub focused: bool,
    /// Fallback-viewer mode tag. Editable text files use [`EditorSession`].
    pub mode: ViewerMode,
    pub status: ViewerStatus,
    /// Raw size on disk when the file was loaded.
    pub size_bytes: u64,
    /// Whether the current view is a limited preview.
    pub preview: bool,
    /// Last modified time observed for change detection.
    modified: Option<SystemTime>,
    /// Best-effort file identity from the last successful load. Unix uses
    /// device/inode; other platforms fall back to size/mtime.
    identity: Option<FileIdentity>,
    /// Transient notice shown after a refresh.
    pub notice: Option<String>,
    /// Detected language label, shown in the header when known.
    pub language_label: Option<String>,
    /// Whether highlighting was disabled due to file size.
    pub highlight_disabled: bool,
    /// Cached highlighted lines. Empty when highlighting is unavailable.
    highlighted_lines: Vec<Vec<Span<'static>>>,
    /// In-file search state.
    pub search: SearchState,
    /// Jump-to-line state.
    pub jump: JumpState,
    /// Last measured content width for horizontal match visibility.
    last_content_width: usize,
}

impl ViewerStatus {
    /// Whether the viewer has a text file that can be sent to an external editor.
    pub fn is_openable(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

impl Default for SourceViewer {
    fn default() -> Self {
        Self {
            path: None,
            rel_path: String::new(),
            lines: Vec::new(),
            document_text: None,
            top_line: 0,
            current_line: 0,
            h_scroll: 0,
            focused: true,
            mode: ViewerMode::Normal,
            status: ViewerStatus::Empty,
            size_bytes: 0,
            preview: false,
            modified: None,
            identity: None,
            notice: None,
            language_label: None,
            highlight_disabled: false,
            highlighted_lines: Vec::new(),
            search: SearchState::default(),
            jump: JumpState::default(),
            last_content_width: 0,
        }
    }
}

impl SourceViewer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check whether the on-disk file differs from the last observed version.
    /// Size and mtime are the cheap fast path; content is read only when either
    /// metadata value changed, so a metadata-only touch is not a conflict.
    pub(crate) fn disk_conflicts_with(
        &self,
        path: &Path,
        expected_disk_text: &[u8],
    ) -> Result<bool, String> {
        let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
        let metadata_same =
            metadata.len() == self.size_bytes && metadata.modified().ok() == self.modified;
        if metadata_same {
            return Ok(false);
        }
        let bytes = fs::read(path).map_err(|error| error.to_string())?;
        Ok(bytes != expected_disk_text)
    }

    /// Open a workspace file. `root` is the workspace root; `path` may be
    /// absolute or relative and is validated to stay inside `root`.
    pub fn open(&mut self, root: &Path, path: &Path) {
        self.path = None;
        self.rel_path = String::new();
        self.lines.clear();
        self.document_text = None;
        self.top_line = 0;
        self.current_line = 0;
        self.h_scroll = 0;
        self.mode = ViewerMode::Normal;
        self.status = ViewerStatus::Loading;
        self.size_bytes = 0;
        self.preview = false;
        self.modified = None;
        self.notice = None;
        self.language_label = None;
        self.highlight_disabled = false;
        self.highlighted_lines.clear();
        self.search = SearchState::default();
        self.jump = JumpState::default();
        self.last_content_width = 0;

        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let resolved = match safe_path(&root, path) {
            Ok(p) => p,
            Err(err) => {
                self.status = ViewerStatus::Error(err);
                return;
            }
        };

        let rel = pathdiff::diff_paths(&resolved, &root)
            .unwrap_or_else(|| resolved.clone())
            .display()
            .to_string();
        self.rel_path = rel;
        self.path = Some(resolved.clone());

        match fs::metadata(&resolved) {
            Ok(meta) => {
                self.size_bytes = meta.len();
                self.modified = meta.modified().ok();
                self.identity = FileIdentity::from_metadata(&meta);
            }
            Err(err) => {
                self.status = ViewerStatus::Error(format!("{err}"));
                self.path = None;
                return;
            }
        }

        if !resolved.is_file() {
            self.status = ViewerStatus::Error("selected path is not a file".into());
            return;
        }

        let bytes = match fs::read(&resolved) {
            Ok(b) => b,
            Err(err) => {
                self.status = ViewerStatus::Error(format!("{err}"));
                return;
            }
        };

        if is_binary(&bytes) {
            self.status = ViewerStatus::Binary;
            self.lines.clear();
            return;
        }

        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                self.status = ViewerStatus::InvalidUtf8;
                self.lines.clear();
                return;
            }
        };
        self.document_text = Some(text.clone());
        self.lines = split_lines(&text);
        self.status = ViewerStatus::Ok;
        self.rebuild_highlight(&text);
        self.clamp_viewport();
    }

    /// Detect the language and build a cached highlighted representation of the
    /// current plain-text lines.
    fn rebuild_highlight(&mut self, expanded_text: &str) {
        self.highlighted_lines.clear();
        let (label, lang) = detect_highlight_language(&self.rel_path, expanded_text);
        self.language_label = label;

        let Some(lang) = lang else {
            return;
        };

        let theme = theme::syntax_theme();
        let lines = match std::panic::catch_unwind(|| {
            forge_syntax::highlight_to_lines(&lang, expanded_text, &theme)
        }) {
            Ok(lines) => lines,
            Err(_) => {
                // Contain any parser panic; plain text remains usable.
                return;
            }
        };

        // Borrowed rather than consumed: the highlight is shared with the cache.
        // The text is cloned here because a `Span` owns its content, which is a
        // copy this path always had to make.
        self.highlighted_lines = lines
            .iter()
            .map(|segments| {
                segments
                    .iter()
                    .map(|(text, (r, g, b), bold, italic)| {
                        let mut style = theme::syntax_segment((*r, *g, *b), theme::panel().bg);
                        if *bold {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if *italic {
                            style = style.add_modifier(Modifier::ITALIC);
                        }
                        Span::styled(text.clone(), style)
                    })
                    .collect()
            })
            .collect();
    }

    /// Reload the current file if one is open. Preserves the viewport when
    /// possible and shows a brief notice when content changed.
    pub fn refresh(&mut self, root: &Path) {
        let Some(path) = self.path.clone() else {
            return;
        };

        if !path.exists() {
            self.status = ViewerStatus::NotFound;
            self.size_bytes = 0;
            self.modified = None;
            self.preview = false;
            self.language_label = None;
            self.highlight_disabled = false;
            self.highlighted_lines.clear();
            self.notice = Some("File no longer exists".into());
            self.search.open = false;
            self.search.matches.clear();
            self.jump.open = false;
            self.clamp_viewport();
            return;
        }

        let old_top = self.top_line;
        let old_current = self.current_line;
        let old_h = self.h_scroll;
        let old_rel = self.rel_path.clone();
        let old_modified = self.modified;
        let old_size = self.size_bytes;
        let search_open = self.search.open;
        let search_query = self.search.query.clone();
        let jump_open = self.jump.open;
        let jump_input = self.jump.input.clone();

        self.open(root, &path);

        // Restore viewport if the file is still readable.
        if matches!(self.status, ViewerStatus::Ok) {
            self.top_line = old_top.min(self.lines.len().saturating_sub(1));
            self.current_line = old_current.min(self.lines.len().saturating_sub(1));
            self.h_scroll = old_h;
            self.rel_path = old_rel;
            let changed = self.modified != old_modified || self.size_bytes != old_size;
            if changed {
                self.notice = Some("Reloaded".into());
            }
            if search_open {
                self.start_search();
                self.update_search_query(&search_query);
            }
            if jump_open {
                self.start_jump();
                self.jump.input = jump_input;
            }
        }
    }

    pub fn reconcile_renamed_path(&mut self, root: &Path, old_path: &Path, new_path: &Path) {
        if self.path.as_deref() != Some(old_path) {
            return;
        }
        self.path = Some(new_path.to_path_buf());
        self.rel_path = pathdiff::diff_paths(new_path, root)
            .unwrap_or_else(|| new_path.to_path_buf())
            .display()
            .to_string();
        self.refresh(root);
    }

    pub fn reconcile_deleted_path(&mut self, deleted_path: &Path) {
        if self.path.as_deref() != Some(deleted_path) {
            return;
        }
        self.status = ViewerStatus::NotFound;
        self.size_bytes = 0;
        self.modified = None;
        self.preview = false;
        self.language_label = None;
        self.highlight_disabled = false;
        self.highlighted_lines.clear();
        self.notice = Some("File moved to Trash or deleted".into());
        self.search.open = false;
        self.search.matches.clear();
        self.jump.open = false;
    }

    pub fn reconcile_external_rename_if_same_identity(
        &mut self,
        root: &Path,
        new_path: &Path,
    ) -> bool {
        let Some(old_path) = self.path.clone() else {
            return false;
        };
        if old_path == new_path || old_path.exists() {
            return false;
        }
        let Some(old_identity) = self.identity.clone() else {
            return false;
        };
        let Ok(meta) = fs::metadata(new_path) else {
            return false;
        };
        if FileIdentity::from_metadata(&meta).as_ref() != Some(&old_identity) {
            return false;
        }
        let old_top = self.top_line;
        let old_current = self.current_line;
        let old_h = self.h_scroll;
        self.path = Some(new_path.to_path_buf());
        self.rel_path = pathdiff::diff_paths(new_path, root)
            .unwrap_or_else(|| new_path.to_path_buf())
            .display()
            .to_string();
        self.refresh(root);
        self.top_line = old_top.min(self.lines.len().saturating_sub(1));
        self.current_line = old_current.min(self.lines.len().saturating_sub(1));
        self.h_scroll = old_h;
        self.notice = Some("File renamed externally".into());
        true
    }

    /// Switch the cosmetic mode tag to INSERT. Doesn't change key behavior.
    pub fn enter_insert_mode(&mut self) {
        self.mode = ViewerMode::Insert;
    }

    /// Switch the cosmetic mode tag back to NORMAL. Doesn't change key behavior.
    pub fn enter_normal_mode(&mut self) {
        self.mode = ViewerMode::Normal;
    }

    pub fn move_cursor_vertical(&mut self, delta: isize, page_height: usize) {
        if delta.abs() >= page_height as isize {
            // Page-style movement already sized to the viewport.
            let next = if delta < 0 {
                self.current_line.saturating_sub(page_height)
            } else {
                (self.current_line + page_height).min(self.lines.len().saturating_sub(1))
            };
            self.current_line = next;
        } else {
            self.current_line = if delta < 0 {
                self.current_line.saturating_sub(delta.unsigned_abs())
            } else {
                (self.current_line + delta as usize).min(self.lines.len().saturating_sub(1))
            };
        }
        self.ensure_current_visible(page_height);
    }

    pub fn move_cursor_horizontal(&mut self, delta: isize) {
        if delta < 0 {
            self.h_scroll = self.h_scroll.saturating_sub(delta.unsigned_abs());
        } else {
            // Arbitrary upper bound; real files rarely exceed a few thousand columns.
            self.h_scroll = (self.h_scroll + delta as usize).min(16_384);
        }
    }

    pub fn move_to_start_of_line(&mut self) {
        self.h_scroll = 0;
    }

    pub fn move_to_end_of_line(&mut self) {
        let width = self
            .lines
            .get(self.current_line)
            .map(|line| display_width(line))
            .unwrap_or(0);
        self.h_scroll = width;
    }

    pub fn move_to_first_line(&mut self) {
        self.current_line = 0;
        self.top_line = 0;
    }

    pub fn move_to_last_line(&mut self) {
        self.current_line = self.lines.len().saturating_sub(1);
        self.top_line = self.lines.len().saturating_sub(1);
    }

    fn ensure_current_visible(&mut self, page_height: usize) {
        if self.current_line < self.top_line {
            self.top_line = self.current_line;
        } else if self.current_line >= self.top_line + page_height {
            self.top_line = self.current_line + 1 - page_height;
        }
    }

    fn clamp_viewport(&mut self) {
        if self.lines.is_empty() {
            self.top_line = 0;
            self.current_line = 0;
            return;
        }
        self.current_line = self.current_line.min(self.lines.len() - 1);
        self.top_line = self.top_line.min(self.lines.len() - 1);
    }

    /// Clear any transient notice. Called once per frame by the renderer.
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    pub fn start_search(&mut self) {
        if !self.can_search() {
            return;
        }
        self.close_jump();
        let pre_line = self.current_line;
        let pre_top = self.top_line;
        let pre_h_scroll = self.h_scroll;
        self.search = SearchState {
            open: true,
            query: String::new(),
            matches: Vec::new(),
            active_index: 0,
            pre_line,
            pre_top,
            pre_h_scroll,
        };
    }

    pub fn close_search(&mut self) {
        if !self.search.open {
            return;
        }
        self.current_line = self.search.pre_line.min(self.lines.len().saturating_sub(1));
        self.top_line = self.search.pre_top.min(self.lines.len().saturating_sub(1));
        self.h_scroll = self.search.pre_h_scroll;
        self.search.open = false;
        self.search.query.clear();
        self.search.matches.clear();
    }

    #[cfg(test)]
    pub fn accept_search(&mut self) {
        self.search.pre_line = self.current_line;
        self.search.pre_top = self.top_line;
        self.search.pre_h_scroll = self.h_scroll;
        self.search.open = false;
    }

    pub fn start_jump(&mut self) {
        if !self.can_search() {
            return;
        }
        self.close_search();
        let pre_line = self.current_line;
        let pre_top = self.top_line;
        self.jump = JumpState {
            open: true,
            input: String::new(),
            pre_line,
            pre_top,
        };
    }

    pub fn close_jump(&mut self) {
        if !self.jump.open {
            return;
        }
        self.current_line = self.jump.pre_line.min(self.lines.len().saturating_sub(1));
        self.top_line = self.jump.pre_top.min(self.lines.len().saturating_sub(1));
        self.jump.open = false;
        self.jump.input.clear();
    }

    pub fn accept_jump(&mut self) {
        self.jump.open = false;
        self.jump.input.clear();
    }

    fn can_search(&self) -> bool {
        matches!(self.status, ViewerStatus::Ok) && !self.lines.is_empty()
    }

    pub fn update_search_query(&mut self, query: &str) {
        self.search.query = query.into();
        self.search.matches = find_matches(&self.lines, query);
        self.search.active_index = 0;
        if !self.search.matches.is_empty() {
            self.goto_match(0);
        }
    }

    pub fn next_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let next = (self.search.active_index + 1) % self.search.matches.len();
        self.goto_match(next);
    }

    pub fn prev_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let n = self.search.matches.len();
        let prev = (self.search.active_index + n - 1) % n;
        self.goto_match(prev);
    }

    fn goto_match(&mut self, index: usize) {
        let m = match self.search.matches.get(index) {
            Some(m) => *m,
            None => return,
        };
        self.search.active_index = index;
        self.current_line = m.line.min(self.lines.len().saturating_sub(1));
        self.ensure_current_visible(self.last_content_width.max(1));
        let width = self.last_content_width.max(1);
        if m.start < self.h_scroll {
            self.h_scroll = m.start;
        } else if m.end > self.h_scroll + width {
            self.h_scroll = m.end.saturating_sub(width);
        }
    }

    pub fn commit_jump(&mut self) {
        let line: usize = match self.jump.input.trim().parse() {
            Ok(0) => 1,
            Ok(n) => n,
            Err(_) => {
                self.close_jump();
                return;
            }
        };
        let max_line = self.lines.len().max(1);
        let target = line.min(max_line).saturating_sub(1);
        self.current_line = target;
        self.top_line = target;
        self.h_scroll = 0;
        self.accept_jump();
    }

    pub fn append_search_char(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        let mut query = self.search.query.clone();
        query.push(ch);
        self.update_search_query(&query);
    }

    pub fn backspace_search(&mut self) {
        let mut query = self.search.query.clone();
        query.pop();
        self.update_search_query(&query);
    }

    pub fn append_jump_char(&mut self, ch: char) {
        if ch.is_ascii_digit() {
            let mut input = self.jump.input.clone();
            input.push(ch);
            self.jump.input = input;
        }
    }

    pub fn backspace_jump(&mut self) {
        self.jump.input.pop();
    }
}

fn split_lines(text: &str) -> Vec<String> {
    text.lines().map(expand_tabs).collect()
}

fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col: usize = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = TAB_WIDTH - (col % TAB_WIDTH);
            out.push_str(&" ".repeat(spaces));
            col += spaces;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

fn display_width(s: &str) -> usize {
    // ponytail: char-count approximation; ratatui renders wide chars correctly,
    // but scrolling is per-character rather than per-column. Upgrade to
    // unicode-width only if mixed-width scripts need exact horizontal paging.
    s.chars().count()
}

fn find_matches(lines: &[String], query: &str) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let query_lower: Vec<char> = query.to_lowercase().chars().collect();
    if query_lower.is_empty() {
        return Vec::new();
    }
    let mut prefix = vec![0; query_lower.len()];
    for i in 1..query_lower.len() {
        let mut length = prefix[i - 1];
        while length > 0 && query_lower[i] != query_lower[length] {
            length = prefix[length - 1];
        }
        if query_lower[i] == query_lower[length] {
            length += 1;
        }
        prefix[i] = length;
    }
    let mut matches = Vec::new();
    for (line_idx, line) in lines.iter().enumerate() {
        let mut matched = 0;
        for (position, ch) in line.to_lowercase().chars().enumerate() {
            while matched > 0 && ch != query_lower[matched] {
                matched = prefix[matched - 1];
            }
            if ch == query_lower[matched] {
                matched += 1;
            }
            if matched == query_lower.len() {
                let start = position + 1 - matched;
                matches.push(Match {
                    line: line_idx,
                    start,
                    end: start + query_lower.len(),
                });
                matched = prefix[matched - 1];
            }
        }
    }
    matches
}

fn clip_line(line: &str, skip: usize, width: usize) -> String {
    line.chars().skip(skip).take(width).collect::<String>()
}

/// Clip a list of styled spans horizontally. Characters are skipped/kept by
/// char count, which is the same unit used by `clip_line` and by the viewer's
/// horizontal scroll.
fn clip_spans(spans: &[Span<'static>], skip: usize, width: usize) -> Vec<Span<'static>> {
    let mut skipped = 0;
    let mut out = Vec::new();
    let mut taken = 0;
    for span in spans {
        let chars: Vec<char> = span.content.chars().collect();
        if skipped + chars.len() <= skip {
            skipped += chars.len();
            continue;
        }
        let start_in_span = skip.saturating_sub(skipped);
        let available = chars.len() - start_in_span;
        let take = available.min(width - taken);
        if take > 0 {
            let text: String = chars[start_in_span..start_in_span + take].iter().collect();
            out.push(Span::styled(text, span.style));
            taken += take;
        }
        skipped += chars.len();
        if taken >= width {
            break;
        }
    }
    out
}

/// Apply search-match styles to visible spans for a given line.
fn apply_search_styles(
    spans: &[Span<'static>],
    line_idx: usize,
    h_scroll: usize,
    content_width: usize,
    matches: &[Match],
    active_index: usize,
) -> Vec<Span<'static>> {
    let mut chars: Vec<(char, Style)> = Vec::new();
    let mut pos = 0;
    for span in spans {
        for ch in span.content.chars() {
            if pos >= h_scroll && chars.len() < content_width {
                chars.push((ch, span.style));
            }
            pos += 1;
            if chars.len() >= content_width {
                break;
            }
        }
        if chars.len() >= content_width {
            break;
        }
    }

    for (i, m) in matches.iter().enumerate() {
        if m.line != line_idx {
            continue;
        }
        let start = m.start.saturating_sub(h_scroll);
        let end = m.end.saturating_sub(h_scroll).min(content_width);
        for j in start..end {
            if let Some((_, style)) = chars.get_mut(j) {
                let patch = if i == active_index {
                    theme::search_match().add_modifier(Modifier::BOLD)
                } else {
                    theme::search_match()
                };
                *style = style.patch(patch);
            }
        }
    }

    let mut out = Vec::new();
    let mut current_text = String::new();
    let mut current_style: Option<Style> = None;
    for (ch, style) in chars {
        if current_style != Some(style) {
            if let Some(s) = current_style {
                out.push(Span::styled(current_text, s));
            }
            current_text = String::new();
            current_style = Some(style);
        }
        current_text.push(ch);
    }
    if let Some(s) = current_style {
        out.push(Span::styled(current_text, s));
    }
    out
}

fn is_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let probe = &bytes[..bytes.len().min(BINARY_PROBE_BYTES)];
    if probe.contains(&0) {
        return true;
    }
    let mut control = 0usize;
    for &b in probe {
        if b != 0 && b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t') {
            control += 1;
        }
    }
    // More than 10% unusual control bytes is treated as binary.
    control * 10 > probe.len()
}

fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{size} bytes")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    }
}

/// Detect language for display and highlighting. Returns `(label, highlight_lang)`.
/// `highlight_lang` is `Some` only when Forge has a syntax grammar for it.
fn detect_highlight_language(rel_path: &str, content: &str) -> (Option<String>, Option<String>) {
    let filename = Path::new(rel_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    // Exact filenames.
    match filename.as_str() {
        "cargo.toml" | "cargo.lock" | "gopkg.mod" | "pyproject.toml" | "poetry.lock" => {
            return (Some("toml".into()), None);
        }
        "dockerfile" | "makefile" | "docker-compose.yml" | "docker-compose.yaml" => {
            return (Some(filename.clone()), None);
        }
        ".gitignore" | ".gitattributes" | ".dockerignore" => {
            return (Some("gitignore".into()), None);
        }
        "readme.md" | "readme.markdown" => {
            return (Some("markdown".into()), None);
        }
        _ => {}
    }

    // Extension-based labels (some unsupported by the grammar).
    let ext = Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "toml" => return (Some("toml".into()), None),
        "md" | "markdown" => return (Some("markdown".into()), None),
        "yaml" | "yml" => return (Some("yaml".into()), None),
        "dockerfile" => return (Some("dockerfile".into()), None),
        _ => {}
    }

    // Grammar-supported detection.
    let lang = forge_syntax::detect_from_path(rel_path);
    if lang != forge_syntax::SyntaxLanguage::Unknown {
        let label = lang.to_string();
        return (Some(label.clone()), Some(label));
    }

    if let Ok(lang) = forge_syntax::detect_language(content) {
        if lang != forge_syntax::SyntaxLanguage::Unknown {
            let label = lang.to_string();
            return (Some(label.clone()), Some(label));
        }
    }

    (None, None)
}

pub struct SourceViewerWidget<'a> {
    pub viewer: &'a mut SourceViewer,
    pub focused: bool,
    /// Optional editable surface. The surrounding Forge chrome remains owned
    /// by this widget; the edtui session only paints the content area.
    pub editor: Option<&'a mut EditorSession>,
    /// Active Vim command without the leading `:`.
    pub editor_command: Option<&'a str>,
    /// Last editor result, shown until the next keypress.
    pub editor_message: Option<&'a str>,
}

impl Widget for SourceViewerWidget<'_> {
    fn render(mut self, area: Rect, buf: &mut Buffer) {
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
        theme::fill(inner, buf, theme::panel());

        match self.viewer.status.clone() {
            ViewerStatus::Empty => self.render_message(
                inner,
                buf,
                "No file open",
                "Select a file from the Files panel and press Enter.",
            ),
            ViewerStatus::Loading => {
                self.render_message(inner, buf, "Loading", "Reading file…");
            }
            ViewerStatus::Binary => {
                let path = &self.viewer.rel_path;
                let size = if self.viewer.size_bytes > 0 {
                    format!("\n{}", format_size(self.viewer.size_bytes))
                } else {
                    String::new()
                };
                self.render_message(
                    inner,
                    buf,
                    "Binary file",
                    &format!("Forge cannot display this file as text.\n{path}{size}"),
                );
            }
            ViewerStatus::InvalidUtf8 => self.render_message(
                inner,
                buf,
                "Invalid UTF-8",
                "This file is read-only in Forge because it is not valid UTF-8.",
            ),
            ViewerStatus::NotFound => {
                self.render_message(
                    inner,
                    buf,
                    "File no longer exists",
                    &format!("{}\nBack · Locate in Files", self.viewer.rel_path),
                );
            }
            ViewerStatus::Error(ref err) => {
                self.render_message(inner, buf, "Unable to open file", err);
            }
            ViewerStatus::Ok => self.render_content(inner, buf),
        }
    }
}

impl SourceViewerWidget<'_> {
    fn render_message(&self, area: Rect, buf: &mut Buffer, heading: &str, body: &str) {
        let lines: Vec<Line> = std::iter::once(Line::styled(heading, theme::heading()))
            .chain(body.lines().map(Line::raw))
            .collect();
        Paragraph::new(lines)
            .alignment(ratatui::layout::Alignment::Center)
            .render(area, buf);
    }

    fn render_content(&mut self, area: Rect, buf: &mut Buffer) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        if self.editor.is_some() {
            self.render_editor_content(area, buf);
            return;
        }

        let input_open = self.viewer.search.open || self.viewer.jump.open;
        let constraints = [
            Constraint::Length(1),                              // header
            Constraint::Min(0),                                 // content
            Constraint::Length(if input_open { 1 } else { 0 }), // input
        ];
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let total = self.viewer.lines.len().max(1);
        let number_width = total.to_string().len().max(3);
        let gutter = (number_width + 3) as u16; // number + " │ "

        let header = compose_source_header(
            &SourceHeader {
                mode: self.viewer.mode.label(),
                rel_path: &self.viewer.rel_path,
                line: self.viewer.current_line + 1,
                total,
                language: self.viewer.language_label.as_deref(),
                note: self.viewer.highlight_disabled.then_some("plain text"),
                modified: false,
            },
            rows[0].width as usize,
        );
        Paragraph::new(Line::styled(header, theme::muted())).render(rows[0], buf);

        let body = rows[1];
        let visible_height = body.height as usize;
        let content_width = body.width.saturating_sub(gutter) as usize;
        self.viewer.last_content_width = content_width;
        self.viewer.clear_notice();

        let start = self.viewer.top_line.min(total.saturating_sub(1));
        let end = (start + visible_height).min(total);

        for (row, index) in (start..end).enumerate() {
            if row >= visible_height {
                break;
            }
            let y = body.y + row as u16;
            let selected = index == self.viewer.current_line;

            let spans = if let Some(hl) = self
                .viewer
                .highlighted_lines
                .get(index)
                .filter(|hl| !hl.is_empty())
            {
                if self.viewer.search.open && !self.viewer.search.matches.is_empty() {
                    apply_search_styles(
                        hl,
                        index,
                        self.viewer.h_scroll,
                        content_width,
                        &self.viewer.search.matches,
                        self.viewer.search.active_index,
                    )
                } else {
                    clip_spans(hl, self.viewer.h_scroll, content_width)
                }
            } else {
                let line = self
                    .viewer
                    .lines
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or("");
                if self.viewer.search.open && !self.viewer.search.matches.is_empty() {
                    let plain = vec![Span::styled(line.to_string(), theme::code_block())];
                    apply_search_styles(
                        &plain,
                        index,
                        self.viewer.h_scroll,
                        content_width,
                        &self.viewer.search.matches,
                        self.viewer.search.active_index,
                    )
                } else {
                    let text = clip_line(line, self.viewer.h_scroll, content_width);
                    vec![Span::styled(text, theme::code_block())]
                }
            };

            let number = format!("{:>number_width$}", index + 1);
            let gutter_text = format!("{number} │ ");

            let gutter_style = if selected {
                if self.viewer.focused {
                    theme::brand().add_modifier(Modifier::BOLD)
                } else {
                    theme::text()
                }
            } else {
                theme::muted()
            };

            let mut line_spans = vec![Span::styled(gutter_text, gutter_style)];
            if selected && self.viewer.focused {
                // Subtle dark tint behind the current line so syntax colours stay readable.
                let base = theme::accent_soft_bg();
                line_spans.extend(spans.into_iter().map(|span| {
                    let style = span.style.bg(base);
                    Span::styled(span.content.into_owned(), style)
                }));
            } else {
                line_spans.extend(spans);
            }
            Line::from(line_spans).render(Rect::new(body.x, y, body.width, 1), buf);
        }

        // Vertical position indicator.
        if total > visible_height && visible_height > 0 {
            let thumb = visible_height
                .saturating_mul(visible_height)
                .div_ceil(total);
            let thumb = thumb.max(1);
            let track = total.saturating_sub(visible_height);
            let pos = if track == 0 {
                0
            } else {
                start.saturating_mul(visible_height - thumb).div_ceil(track)
            };
            let bar_y = body.y + pos.min(visible_height.saturating_sub(1)) as u16;
            if bar_y < body.y + body.height {
                let x = body.x + body.width - 1;
                buf[(x, bar_y)].set_symbol("█").set_style(theme::dim());
            }
        }

        if input_open {
            self.render_input(rows[2], buf);
        }
    }

    fn render_editor_content(&mut self, area: Rect, buf: &mut Buffer) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        let (mode, current_line, total, modified) = self
            .editor
            .as_deref()
            .map(|editor| {
                (
                    editor.mode().name().to_uppercase(),
                    editor.cursor_row(),
                    editor.line_count(),
                    editor.is_dirty(),
                )
            })
            .map(|(mode, line, total, modified)| (mode, line, total.max(1), modified))
            .unwrap_or_else(|| {
                (
                    self.viewer.mode.label().to_string(),
                    self.viewer.current_line,
                    self.viewer.lines.len().max(1),
                    false,
                )
            });
        let header = compose_source_header(
            &SourceHeader {
                mode: &mode,
                rel_path: &self.viewer.rel_path,
                line: current_line + 1,
                total,
                language: self.viewer.language_label.as_deref(),
                note: None,
                modified,
            },
            rows[0].width as usize,
        );
        Paragraph::new(Line::styled(header, theme::muted())).render(rows[0], buf);

        if let Some(editor) = self.editor.as_deref_mut() {
            editor.render(rows[1], buf);
        }

        let status_style = if self.focused {
            theme::text()
        } else {
            theme::muted()
        };
        let status = if let Some(command) = self.editor_command {
            format!(":{command}")
        } else if let Some(message) = self.editor_message {
            message.to_string()
        } else if mode == "SEARCH" {
            format!(
                "SEARCH /{}",
                self.editor
                    .as_deref()
                    .map(EditorSession::search_pattern)
                    .unwrap_or_default()
            )
        } else if mode == "INSERT" {
            "-- INSERT --".to_string()
        } else if mode == "NORMAL" {
            "NORMAL".to_string()
        } else {
            mode
        };
        Paragraph::new(Line::styled(status, status_style)).render(rows[2], buf);
        if self.editor_command.is_some() {
            let command = self.editor_command.unwrap_or_default();
            let cursor_x = rows[2].x + 1 + command.chars().count() as u16;
            if cursor_x < rows[2].x + rows[2].width {
                theme::paint_caret(buf, cursor_x, rows[2].y);
            }
        }
    }

    fn render_input(&self, area: Rect, buf: &mut Buffer) {
        if self.viewer.search.open {
            let count = self.viewer.search.matches.len();
            let status = if count == 0 {
                if self.viewer.search.query.is_empty() {
                    String::new()
                } else {
                    " · No matches".to_string()
                }
            } else {
                format!(" · {} of {}", self.viewer.search.active_index + 1, count)
            };
            let text = format!(
                "Search: {}{}{status}",
                self.viewer.search.query,
                theme::CURSOR_GLYPH
            );
            Paragraph::new(Line::styled(text, theme::text())).render(area, buf);
            let cursor_x = area.x + 8 + self.viewer.search.query.chars().count() as u16;
            if cursor_x < area.x + area.width {
                theme::paint_caret(buf, cursor_x, area.y);
            }
        } else if self.viewer.jump.open {
            let text = format!(
                "Go to line: {}{}",
                self.viewer.jump.input,
                theme::CURSOR_GLYPH
            );
            Paragraph::new(Line::styled(text, theme::text())).render(area, buf);
            let cursor_x = area.x + 12 + self.viewer.jump.input.chars().count() as u16;
            if cursor_x < area.x + area.width {
                theme::paint_caret(buf, cursor_x, area.y);
            }
        }
    }
}

/// Compose the source header so it still says what matters in a narrow pane.
///
/// The header used to be built by appending, which put the unsaved-changes
/// state last and therefore made it the first thing a `Paragraph` truncated.
/// At 120 columns the editor pane is roughly a third of the width, so a real
/// path plus a line counter already overflows — and the one piece of state a
/// reader cannot recover by looking at the pane was the piece that vanished.
///
/// Parts are sacrificed in increasing order of importance: the plain-text
/// note, the language, the mode tag, the directory portion of the path, then
/// the line counter. The `●` marker is never dropped.
struct SourceHeader<'a> {
    mode: &'a str,
    rel_path: &'a str,
    line: usize,
    total: usize,
    language: Option<&'a str>,
    /// Low-priority aside such as "plain text"; first to be dropped.
    note: Option<&'a str>,
    modified: bool,
}

fn compose_source_header(header: &SourceHeader<'_>, width: usize) -> String {
    let SourceHeader {
        mode,
        rel_path,
        line,
        total,
        language,
        note,
        modified,
    } = *header;
    let marker = if modified { "\u{25cf} " } else { "" };
    let basename = std::path::Path::new(rel_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(rel_path);
    let counter = format!("line {} of {}", line, total);
    let word = if modified { "modified" } else { "" };
    let full = format!("{marker}{rel_path}");
    let short = format!("{marker}{basename}");

    let join = |parts: &[&str]| {
        parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
    };

    let candidates = [
        join(&[
            mode,
            &full,
            &counter,
            language.unwrap_or(""),
            note.unwrap_or(""),
            word,
        ]),
        join(&[mode, &full, &counter, language.unwrap_or(""), word]),
        join(&[mode, &full, &counter, word]),
        join(&[&full, &counter, word]),
        join(&[&short, &counter, word]),
        join(&[&short, word]),
        short.clone(),
    ];

    for candidate in &candidates {
        if candidate.chars().count() <= width {
            return candidate.clone();
        }
    }

    // Narrower than the filename itself: keep the marker, shorten the name.
    let keep = width.saturating_sub(marker.chars().count());
    let name: String = if keep == 0 {
        String::new()
    } else if basename.chars().count() <= keep {
        basename.to_string()
    } else {
        basename
            .chars()
            .take(keep.saturating_sub(1))
            .chain(std::iter::once('\u{2026}'))
            .collect()
    };
    format!("{marker}{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn render_viewer(viewer: &mut SourceViewer) -> String {
        let area = Rect::new(0, 0, 80, 16);
        let mut buf = Buffer::empty(area);
        SourceViewerWidget {
            viewer,
            focused: true,
            editor: None,
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn expand_tabs_replaces_tabs_with_spaces() {
        assert_eq!(expand_tabs("a\tb"), "a   b");
        assert_eq!(expand_tabs("\t\t"), "        ");
        assert_eq!(expand_tabs("1234\tx"), "1234    x");
    }

    #[test]
    fn split_lines_preserves_empty_lines() {
        let lines = split_lines("a\n\nb");
        assert_eq!(lines, vec!["a", "", "b"]);
    }

    #[test]
    fn viewer_status_openable_matches_text_file_states() {
        assert!(!ViewerStatus::Empty.is_openable());
        assert!(!ViewerStatus::Loading.is_openable());
        assert!(ViewerStatus::Ok.is_openable());
        assert!(!ViewerStatus::Binary.is_openable());
        assert!(!ViewerStatus::InvalidUtf8.is_openable());
        assert!(!ViewerStatus::NotFound.is_openable());
        assert!(!ViewerStatus::Error("x".into()).is_openable());
    }

    #[test]
    fn is_binary_detects_null_bytes() {
        assert!(is_binary(b"hello\0world"));
        assert!(!is_binary(b"hello world\n"));
    }

    #[test]
    fn is_binary_detects_control_heavy_data() {
        let mut data = vec![0x1f; 100];
        assert!(is_binary(&data));
        data.fill(b'\n');
        assert!(!is_binary(&data));
    }

    #[test]
    fn clip_line_respects_width_and_skip() {
        let line = "abcdef";
        assert_eq!(clip_line(line, 0, 3), "abc");
        assert_eq!(clip_line(line, 2, 3), "cde");
        assert_eq!(clip_line(line, 0, 100), "abcdef");
        assert_eq!(clip_line(line, 20, 3), "");
    }

    #[test]
    fn format_size_and_language_detection_cover_labels() {
        assert_eq!(format_size(12), "12 bytes");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MB");

        assert_eq!(
            detect_highlight_language("Cargo.toml", "").0.as_deref(),
            Some("toml")
        );
        assert_eq!(
            detect_highlight_language("Dockerfile", "").0.as_deref(),
            Some("dockerfile")
        );
        assert_eq!(
            detect_highlight_language(".gitignore", "").0.as_deref(),
            Some("gitignore")
        );
        assert_eq!(
            detect_highlight_language("README.markdown", "")
                .0
                .as_deref(),
            Some("markdown")
        );
        assert_eq!(
            detect_highlight_language("config.yaml", "").0.as_deref(),
            Some("yaml")
        );
        assert_eq!(
            detect_highlight_language("script", "#!/usr/bin/env python\nprint('x')")
                .0
                .as_deref(),
            Some("python")
        );
    }

    #[test]
    fn clip_line_is_unicode_safe() {
        let line = "αβγδε";
        assert_eq!(clip_line(line, 0, 3), "αβγ");
        assert_eq!(clip_line(line, 2, 2), "γδ");
    }

    #[test]
    fn open_rejects_path_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), outside.path().join("x.txt").as_path());
        assert!(!matches!(viewer.status, ViewerStatus::Ok));
    }

    #[test]
    fn open_loads_text_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("src/agent.rs");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "use std::sync::Arc;\n\npub struct Agent;\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert_eq!(viewer.status, ViewerStatus::Ok);
        assert_eq!(viewer.lines.len(), 3);
        assert_eq!(viewer.rel_path, "src/agent.rs");
        assert_eq!(viewer.current_line, 0);
    }

    #[test]
    fn open_shows_binary_for_binary_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bin");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
            .unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert_eq!(viewer.status, ViewerStatus::Binary);
        assert!(viewer.lines.is_empty());
    }

    #[test]
    fn open_loads_large_file_without_previewing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("big.txt");
        fs::write(&path, "x\n".repeat(2_000_000)).unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert_eq!(viewer.status, ViewerStatus::Ok);
        assert_eq!(viewer.lines.len(), 2_000_000);
        assert!(!viewer.preview);
    }

    #[test]
    fn open_shows_not_found_after_deletion() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("gone.txt");
        fs::write(&path, "hi").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        let opened = viewer.path.clone();
        assert_eq!(viewer.lines, vec!["hi"]);
        fs::remove_file(&path).unwrap();
        viewer.refresh(root.path());

        assert_eq!(viewer.status, ViewerStatus::NotFound);
        assert_eq!(viewer.path, opened);
        assert_eq!(viewer.lines, vec!["hi"]);
        assert!(viewer.notice.is_some());
    }

    #[test]
    fn viewport_clamps_after_refresh() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "1\n2\n3\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.current_line = 5;
        viewer.clamp_viewport();
        assert_eq!(viewer.current_line, 2);
    }

    #[test]
    fn move_cursor_horizontal_is_bounded() {
        let mut viewer = SourceViewer::new();
        viewer.lines = vec!["short".into()];
        viewer.move_cursor_horizontal(100_000);
        assert_eq!(viewer.h_scroll, 16_384);
        viewer.move_cursor_horizontal(-1_000_000);
        assert_eq!(viewer.h_scroll, 0);
    }

    #[test]
    fn cursor_line_movement_and_search_input_editing_cover_edges() {
        let mut viewer = SourceViewer::new();
        viewer.lines = vec!["abc".into(), "def".into(), "ghi".into()];
        viewer.status = ViewerStatus::Ok;
        viewer.current_line = 1;
        viewer.top_line = 1;
        viewer.h_scroll = 2;

        viewer.move_cursor_vertical(-10, 2);
        assert_eq!(viewer.current_line, 0);
        assert_eq!(viewer.top_line, 0);
        viewer.move_cursor_vertical(10, 2);
        assert_eq!(viewer.current_line, 2);
        viewer.move_to_start_of_line();
        assert_eq!(viewer.h_scroll, 0);
        viewer.move_to_end_of_line();
        assert_eq!(viewer.h_scroll, 3);
        viewer.move_to_first_line();
        assert_eq!((viewer.current_line, viewer.top_line), (0, 0));
        viewer.move_to_last_line();
        assert_eq!((viewer.current_line, viewer.top_line), (2, 2));

        viewer.start_search();
        viewer.append_search_char('d');
        viewer.append_search_char('\n');
        assert_eq!(viewer.search.query, "d");
        viewer.backspace_search();
        assert!(viewer.search.query.is_empty());
        viewer.close_search();

        viewer.start_jump();
        viewer.append_jump_char('2');
        viewer.append_jump_char('x');
        assert_eq!(viewer.jump.input, "2");
        viewer.backspace_jump();
        assert!(viewer.jump.input.is_empty());
        viewer.close_jump();
    }

    #[test]
    fn large_file_is_openable_without_an_arbitrary_refusal_threshold() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("huge.txt");
        fs::write(&path, "x".repeat(11 * 1024 * 1024)).unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert_eq!(viewer.status, ViewerStatus::Ok);
    }

    #[test]
    fn refresh_preserves_viewport_and_notices_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "1\n2\n3\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.current_line = 2;
        viewer.top_line = 1;
        viewer.h_scroll = 1;
        viewer.notice = None;

        viewer.refresh(root.path());
        assert_eq!(viewer.status, ViewerStatus::Ok);
        assert_eq!(viewer.current_line, 2);
        assert_eq!(viewer.top_line, 1);
        assert_eq!(viewer.h_scroll, 1);
        assert!(viewer.notice.is_none(), "unchanged file should not notify");

        fs::write(&path, "1\n2\n3\n4\n").unwrap();
        viewer.refresh(root.path());
        assert_eq!(viewer.current_line, 2);
        assert_eq!(viewer.top_line, 1);
        assert!(viewer.notice.is_some(), "changed file should notify");
    }

    #[test]
    fn rust_file_gets_syntax_highlighting() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.rs");
        fs::write(&path, "fn main() { let x = 42; }\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert_eq!(viewer.status, ViewerStatus::Ok);
        assert_eq!(viewer.language_label.as_deref(), Some("rust"));
        assert!(
            !viewer.highlighted_lines.is_empty(),
            "rust file should be highlighted"
        );
    }

    #[test]
    fn unknown_extension_uses_plain_text() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.xyz");
        fs::write(&path, "key = value\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert!(viewer.highlighted_lines.is_empty());
        assert!(viewer.language_label.is_none());
    }

    #[test]
    fn large_file_keeps_syntax_highlighting_enabled() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("big.rs");
        fs::write(&path, "let x = 1;\n".repeat(30_000)).unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert!(!viewer.highlight_disabled);
        assert!(!viewer.highlighted_lines.is_empty());
    }

    #[test]
    fn invalid_utf8_is_explicitly_read_only() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bytes.bin");
        fs::write(&path, [b'a', 0x80]).unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert_eq!(viewer.status, ViewerStatus::InvalidUtf8);
        assert!(viewer.lines.is_empty());
        assert!(!viewer.status.is_openable());
    }

    #[test]
    fn clip_spans_preserves_styles() {
        let spans = vec![
            Span::styled("hello ", theme::text()),
            Span::styled("world", theme::brand()),
        ];
        let clipped = clip_spans(&spans, 3, 5);
        assert_eq!(clipped.len(), 2);
        assert_eq!(clipped[0].content, "lo ");
        assert_eq!(clipped[1].content, "wo");
    }

    #[test]
    fn find_matches_is_case_insensitive_and_literal() {
        let lines = vec!["Hello World".into(), "worldly".into(), "HELLO".into()];
        let matches = find_matches(&lines, "world");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[0].start, 6);
        assert_eq!(matches[0].end, 11);
        assert_eq!(matches[1].line, 1);
    }

    #[test]
    fn find_matches_empty_query() {
        let lines = vec!["hello".into()];
        assert!(find_matches(&lines, "").is_empty());
    }

    #[test]
    fn find_matches_empty_line_with_non_empty_query() {
        // Regression: this used to panic with "range end index 1 out of range
        // for slice of length 0" at the empty line.
        let lines = vec!["hello".into(), "".into(), "world".into()];
        let matches = find_matches(&lines, "o");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[1].line, 2);
    }

    #[test]
    fn find_matches_zero_matches() {
        let lines = vec!["hello".into(), "world".into()];
        assert!(find_matches(&lines, "xyz").is_empty());
    }

    #[test]
    fn find_matches_at_start_and_end() {
        let lines = vec!["abc".into(), "cab".into()];
        let matches = find_matches(&lines, "a");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[0].start, 0);
        assert_eq!(matches[1].line, 1);
        assert_eq!(matches[1].start, 1);
    }

    #[test]
    fn find_matches_keeps_overlapping_matches() {
        let matches = find_matches(&["aaaa".into()], "aa");
        assert_eq!(
            matches.iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
            vec![(0, 2), (1, 3), (2, 4)]
        );
    }

    #[test]
    fn find_matches_is_unicode_safe() {
        let lines = vec!["αβγδ".into(), "ΓΔ".into()];
        let matches = find_matches(&lines, "βγ");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[0].start, 1);
        assert_eq!(matches[0].end, 3);

        // Case-insensitive match across scripts.
        let matches = find_matches(&lines, "γδ");
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn search_empty_file_is_safe() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.start_search();
        viewer.update_search_query("foo");
        assert!(viewer.search.matches.is_empty());
        viewer.next_match();
        viewer.prev_match();
    }

    #[test]
    fn search_zero_matches_is_safe() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "hello world\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.start_search();
        viewer.update_search_query("xyz");
        assert!(viewer.search.matches.is_empty());
        assert_eq!(viewer.search.active_index, 0);
        viewer.next_match();
        viewer.prev_match();
    }

    #[test]
    fn search_file_emptied_while_open() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "hello\nworld\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.start_search();
        viewer.update_search_query("hello");
        assert_eq!(viewer.search.matches.len(), 1);

        fs::write(&path, "").unwrap();
        viewer.refresh(root.path());
        assert!(viewer.search.matches.is_empty());
        assert!(viewer.search.active_index < viewer.search.matches.len().max(1));
    }

    #[test]
    fn search_file_shortened_while_open() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "hello\nworld\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.start_search();
        viewer.update_search_query("world");
        assert_eq!(viewer.search.matches.len(), 1);
        assert_eq!(viewer.search.matches[0].line, 1);

        fs::write(&path, "hello\n").unwrap();
        viewer.refresh(root.path());
        assert!(viewer.search.matches.is_empty());
        assert!(viewer.current_line < viewer.lines.len().max(1));
    }

    #[test]
    fn search_navigation_wraps() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "foo\nfoo\nfoo\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.start_search();
        viewer.update_search_query("foo");

        assert_eq!(viewer.search.matches.len(), 3);
        assert_eq!(viewer.search.active_index, 0);
        viewer.prev_match();
        assert_eq!(viewer.search.active_index, 2);
        viewer.next_match();
        assert_eq!(viewer.search.active_index, 0);
    }

    #[test]
    fn jump_to_line_moves_cursor() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "1\n2\n3\n4\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.start_jump();
        viewer.jump.input = "3".into();
        viewer.commit_jump();

        assert!(!viewer.jump.open);
        assert_eq!(viewer.current_line, 2);
    }

    #[test]
    fn jump_out_of_range_clamps() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "1\n2\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.start_jump();
        viewer.jump.input = "99".into();
        viewer.commit_jump();

        assert_eq!(viewer.current_line, 1);
    }

    #[test]
    fn jump_invalid_input_cancels() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("x.txt");
        fs::write(&path, "1\n2\n3\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        viewer.start_jump();
        viewer.jump.input = "abc".into();
        viewer.commit_jump();

        assert!(!viewer.jump.open);
        assert_eq!(viewer.current_line, 0);
    }

    #[test]
    fn transient_inputs_restore_previous_viewport_or_accept_current() {
        let mut viewer = SourceViewer::new();
        viewer.lines = vec!["one".into(), "two".into(), "three".into()];
        viewer.status = ViewerStatus::Ok;
        viewer.current_line = 1;
        viewer.top_line = 1;
        viewer.h_scroll = 4;

        viewer.start_search();
        viewer.update_search_query("three");
        assert_eq!(viewer.current_line, 2);
        viewer.close_search();
        assert_eq!(
            (viewer.current_line, viewer.top_line, viewer.h_scroll),
            (1, 1, 4)
        );

        viewer.start_search();
        viewer.update_search_query("three");
        viewer.accept_search();
        assert_eq!(viewer.current_line, 2);
        assert!(!viewer.search.open);

        viewer.start_jump();
        viewer.jump.input = "1".into();
        viewer.commit_jump();
        assert_eq!(viewer.current_line, 0);
    }

    #[test]
    fn source_viewer_widget_renders_status_messages_and_transient_inputs() {
        let mut empty = SourceViewer::new();
        assert!(render_viewer(&mut empty).contains("No file open"));

        let mut loading = SourceViewer::new();
        loading.status = ViewerStatus::Loading;
        assert!(render_viewer(&mut loading).contains("Loading"));

        let mut binary = SourceViewer::new();
        binary.status = ViewerStatus::Binary;
        binary.rel_path = "bin.dat".into();
        binary.size_bytes = 2048;
        let binary_text = render_viewer(&mut binary);
        assert!(binary_text.contains("Binary file"));
        assert!(binary_text.contains("2.0 KB"));

        let mut missing = SourceViewer::new();
        missing.status = ViewerStatus::NotFound;
        missing.rel_path = "gone.rs".into();
        assert!(render_viewer(&mut missing).contains("File no longer exists"));

        let mut error = SourceViewer::new();
        error.status = ViewerStatus::Error("permission denied".into());
        assert!(render_viewer(&mut error).contains("Unable to open file"));

        let mut ok = SourceViewer::new();
        ok.status = ViewerStatus::Ok;
        ok.rel_path = "src/lib.rs".into();
        ok.lines = vec!["fn main() {}".into()];
        ok.language_label = Some("rust".into());
        ok.notice = Some("Reloaded".into());
        ok.start_search();
        ok.update_search_query("missing");
        let text = render_viewer(&mut ok);
        assert!(text.contains("src/lib.rs"));
        assert!(text.contains("line 1 of 1"));
        assert!(text.contains("rust"));
        assert!(text.contains("Search: missing"));
        assert!(text.contains("No matches"));

        ok.close_search();
        ok.start_jump();
        ok.jump.input = "12".into();
        assert!(render_viewer(&mut ok).contains("Go to line: 12"));
    }

    #[test]
    fn source_viewer_widget_keeps_forge_chrome_around_editor_surface() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes.txt");
        fs::write(&path, "old text\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        let mut editor = EditorSession::new("new text");
        let area = Rect::new(0, 0, 40, 8);
        let mut buf = Buffer::empty(area);

        SourceViewerWidget {
            viewer: &mut viewer,
            focused: true,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);

        let mut rendered = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(rendered.contains("notes.txt"));
        assert!(rendered.contains("new text"));
        assert!(!rendered.contains("old text"));
    }

    #[test]
    fn editor_header_identifies_unsaved_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes.txt");
        fs::write(&path, "old text\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        let mut editor = EditorSession::new("old text\n");
        editor.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            crossterm::event::KeyModifiers::NONE,
        ));
        editor.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        ));

        let area = Rect::new(0, 0, 100, 8);
        let mut buf = Buffer::empty(area);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: true,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);

        let mut rendered = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(rendered.contains("modified"));
    }

    /// The pure composer is only half of it: the widget has to pass the pane
    /// width through. Rendered at 40 columns, which is about what a 120-column
    /// terminal leaves the editor pane in the three-pane layout.
    #[test]
    fn a_narrow_editor_pane_still_shows_unsaved_state() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("some_longish_name.txt");
        fs::write(&path, "old text\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        let mut editor = EditorSession::new("old text\n");
        for key in [
            crossterm::event::KeyCode::Char('i'),
            crossterm::event::KeyCode::Char('x'),
        ] {
            editor.handle_key(crossterm::event::KeyEvent::new(
                key,
                crossterm::event::KeyModifiers::NONE,
            ));
        }

        let area = Rect::new(0, 0, 40, 8);
        let mut buf = Buffer::empty(area);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: true,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut buf);

        let mut rendered = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
            rendered.push('\n');
        }
        assert!(
            rendered.contains('\u{25cf}'),
            "unsaved state must be visible in a narrow pane:\n{rendered}"
        );
    }

    /// The width the usability run actually hit: a 120-column terminal gives
    /// the editor pane roughly a third, and the old header lost the
    /// unsaved-changes state there while looking perfectly fine at 100.
    #[test]
    fn unsaved_state_survives_a_narrow_pane() {
        let path = "crates/forge-tui/src/source_viewer.rs";

        for width in [80, 60, 40, 30, 20, 12, 6] {
            let header = compose_source_header(
                &SourceHeader {
                    mode: "NORMAL",
                    rel_path: path,
                    line: 42,
                    total: 2100,
                    language: Some("Rust"),
                    note: None,
                    modified: true,
                },
                width,
            );

            assert!(
                header.chars().count() <= width,
                "header overflows at width {width}: {header:?}"
            );
            assert!(
                header.contains('\u{25cf}'),
                "the unsaved marker must survive width {width}: {header:?}"
            );
        }
    }

    /// Detail is given up in a deliberate order, not arbitrarily.
    #[test]
    fn header_sheds_the_least_important_detail_first() {
        let path = "src/app/workspace.rs";
        let at = |width| {
            compose_source_header(
                &SourceHeader {
                    mode: "NORMAL",
                    rel_path: path,
                    line: 7,
                    total: 900,
                    language: Some("Rust"),
                    note: Some("plain text"),
                    modified: true,
                },
                width,
            )
        };

        let full = at(100);
        assert!(full.contains("plain text") && full.contains("Rust") && full.contains("NORMAL"));
        assert!(full.contains(path), "a wide pane keeps the whole path");

        // Enough for the path and counter, not for the decorations.
        let medium = at(46);
        assert!(
            !medium.contains("plain text"),
            "note goes first: {medium:?}"
        );
        assert!(
            medium.contains("line 7 of 900"),
            "the counter outranks it: {medium:?}"
        );

        // Too narrow for the directories, wide enough for the name.
        let narrow = at(24);
        assert!(
            !narrow.contains("src/app"),
            "path shortens to a basename: {narrow:?}"
        );
        assert!(
            narrow.contains("workspace.rs"),
            "the name is kept: {narrow:?}"
        );

        // Nothing but the name fits.
        let tiny = at(10);
        assert!(
            !tiny.contains("line"),
            "the counter goes before the name: {tiny:?}"
        );
        assert!(
            tiny.starts_with('\u{25cf}'),
            "the marker still leads: {tiny:?}"
        );
    }

    /// An unmodified file must not grow a marker, or the indicator means
    /// nothing.
    #[test]
    fn the_marker_appears_only_when_modified() {
        let clean = compose_source_header(
            &SourceHeader {
                mode: "NORMAL",
                rel_path: "a/b.rs",
                line: 1,
                total: 10,
                language: None,
                note: None,
                modified: false,
            },
            60,
        );
        assert!(!clean.contains('\u{25cf}'), "{clean:?}");
        assert!(!clean.contains("modified"), "{clean:?}");
    }

    #[test]
    fn editor_status_row_shows_mode_command_and_result_states() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes.txt");
        fs::write(&path, "old text\n").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        let mut editor = EditorSession::new("old text\n");
        let area = Rect::new(0, 0, 60, 8);

        let mut normal = Buffer::empty(area);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: true,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut normal);
        assert!(buffer_text(&normal, area).contains("NORMAL"));

        editor.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let mut insert = Buffer::empty(area);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: true,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: None,
        }
        .render(area, &mut insert);
        assert!(buffer_text(&insert, area).contains("-- INSERT --"));

        let mut command = Buffer::empty(area);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: true,
            editor: Some(&mut editor),
            editor_command: Some("w"),
            editor_message: None,
        }
        .render(area, &mut command);
        assert!(buffer_text(&command, area).contains(":w"));
        let cursor_style = command[(4, area.bottom() - 2)].style();
        let caret_style = theme::caret();
        assert_eq!(cursor_style.fg, caret_style.fg);
        assert_eq!(cursor_style.bg, caret_style.bg);
        assert!(cursor_style.add_modifier.contains(caret_style.add_modifier));

        let mut result = Buffer::empty(area);
        SourceViewerWidget {
            viewer: &mut viewer,
            focused: false,
            editor: Some(&mut editor),
            editor_command: None,
            editor_message: Some("written notes.txt"),
        }
        .render(area, &mut result);
        assert!(buffer_text(&result, area).contains("written notes.txt"));
        assert_eq!(result[(2, area.bottom() - 2)].style().fg, theme::muted().fg);
    }

    fn buffer_text(buffer: &Buffer, area: Rect) -> String {
        let mut text = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn header_shows_cosmetic_mode_tag() {
        let mut viewer = SourceViewer::new();
        viewer.status = ViewerStatus::Ok;
        viewer.rel_path = "src/lib.rs".into();
        viewer.lines = vec!["fn main() {}".into()];
        assert!(render_viewer(&mut viewer).contains("NORMAL"));

        viewer.enter_insert_mode();
        assert!(render_viewer(&mut viewer).contains("INSERT"));

        viewer.enter_normal_mode();
        assert!(render_viewer(&mut viewer).contains("NORMAL"));
    }

    #[test]
    fn search_resets_when_new_file_opens() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        fs::write(&a, "foo").unwrap();
        fs::write(&b, "bar").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &a);
        viewer.start_search();
        viewer.update_search_query("foo");
        assert!(!viewer.search.query.is_empty());

        viewer.open(root.path(), &b);
        assert!(!viewer.search.open);
        assert!(viewer.search.query.is_empty());
    }
}
