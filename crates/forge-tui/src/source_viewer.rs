//! Read-only source viewer for the workspace Editor tab.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::file_explorer::safe_path;
use crate::theme;

/// Files at most this large are loaded fully.
const MAX_FULL_BYTES: u64 = 1_048_576; // 1 MiB
/// Files larger than `MAX_FULL_BYTES` are previewed up to this many bytes.
const MAX_PREVIEW_BYTES: u64 = 65_536; // 64 KiB
/// Files larger than this are refused outright.
const MAX_REFUSE_BYTES: u64 = 10_485_760; // 10 MiB
/// How many bytes to inspect for binary detection.
const BINARY_PROBE_BYTES: usize = 8_192;
/// Tab stops are every N columns.
const TAB_WIDTH: usize = 4;
/// Files larger than this are displayed as plain text without syntax highlighting.
const HIGHLIGHT_DISABLE_BYTES: u64 = 262_144; // 256 KiB

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerStatus {
    Empty,
    Loading,
    Ok,
    Binary,
    LargeFile { preview: bool },
    TooLarge,
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

#[derive(Debug, Clone)]
pub struct SourceViewer {
    /// Canonical absolute path of the open file, if any.
    pub path: Option<PathBuf>,
    /// Path relative to the workspace root.
    pub rel_path: String,
    /// Decoded text lines (possibly a limited preview).
    pub lines: Vec<String>,
    /// First visible line index (0-based).
    pub top_line: usize,
    /// Current/cursor line index (0-based).
    pub current_line: usize,
    /// Horizontal scroll offset in display columns.
    pub h_scroll: usize,
    pub status: ViewerStatus,
    /// Raw size on disk when the file was loaded.
    pub size_bytes: u64,
    /// Whether the current view is a limited preview.
    pub preview: bool,
    /// Last modified time observed for change detection.
    modified: Option<SystemTime>,
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

impl Default for SourceViewer {
    fn default() -> Self {
        Self {
            path: None,
            rel_path: String::new(),
            lines: Vec::new(),
            top_line: 0,
            current_line: 0,
            h_scroll: 0,
            status: ViewerStatus::Empty,
            size_bytes: 0,
            preview: false,
            modified: None,
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

    /// Open a workspace file. `root` is the workspace root; `path` may be
    /// absolute or relative and is validated to stay inside `root`.
    pub fn open(&mut self, root: &Path, path: &Path) {
        self.path = None;
        self.rel_path = String::new();
        self.lines.clear();
        self.top_line = 0;
        self.current_line = 0;
        self.h_scroll = 0;
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

        if self.size_bytes > MAX_REFUSE_BYTES {
            self.status = ViewerStatus::TooLarge;
            return;
        }

        let limit = if self.size_bytes > MAX_FULL_BYTES {
            MAX_PREVIEW_BYTES
        } else {
            self.size_bytes
        };
        self.preview = limit < self.size_bytes;

        let bytes = match read_limited(&resolved, limit as usize) {
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

        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.lines = split_lines(&text);
        self.status = if self.preview {
            ViewerStatus::LargeFile { preview: true }
        } else {
            ViewerStatus::Ok
        };
        self.rebuild_highlight(&text);
        self.clamp_viewport();
    }

    /// Detect the language and build a cached highlighted representation of the
    /// current plain-text lines.
    fn rebuild_highlight(&mut self, expanded_text: &str) {
        self.highlighted_lines.clear();
        let (label, lang) = detect_highlight_language(&self.rel_path, expanded_text);
        self.language_label = label;

        if self.size_bytes > HIGHLIGHT_DISABLE_BYTES {
            self.highlight_disabled = true;
            return;
        }

        let Some(lang) = lang else {
            return;
        };

        let theme = forge_syntax::HighlightTheme::default();
        let lines = match std::panic::catch_unwind(|| {
            forge_syntax::highlight_to_lines(&lang, expanded_text, &theme)
        }) {
            Ok(lines) => lines,
            Err(_) => {
                // Contain any parser panic; plain text remains usable.
                return;
            }
        };

        self.highlighted_lines = lines
            .into_iter()
            .map(|segments| {
                segments
                    .into_iter()
                    .map(|(text, (r, g, b), bold, italic)| {
                        let mut style = Style::default().fg(Color::Rgb(r, g, b));
                        if bold {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if italic {
                            style = style.add_modifier(Modifier::ITALIC);
                        }
                        Span::styled(text, style)
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
            self.lines.clear();
            self.size_bytes = 0;
            self.modified = None;
            self.preview = false;
            self.notice = Some("File no longer exists".into());
            self.search.open = false;
            self.jump.open = false;
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
        if matches!(
            self.status,
            ViewerStatus::Ok | ViewerStatus::LargeFile { .. }
        ) {
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
        matches!(
            self.status,
            ViewerStatus::Ok | ViewerStatus::LargeFile { .. }
        ) && !self.lines.is_empty()
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

fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut reader = file.take(limit as u64);
    let mut buf = Vec::with_capacity(limit.min(65_536));
    reader.read_to_end(&mut buf)?;
    Ok(buf)
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
    let mut matches = Vec::new();
    for (line_idx, line) in lines.iter().enumerate() {
        let line_lower: Vec<char> = line.to_lowercase().chars().collect();
        if line_lower.len() < query_lower.len() {
            continue;
        }
        let max_start = line_lower.len() - query_lower.len();
        for start in 0..=max_start {
            if line_lower[start..start + query_lower.len()] == query_lower[..] {
                matches.push(Match {
                    line: line_idx,
                    start,
                    end: start + query_lower.len(),
                });
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
        let start_in_span = if skipped < skip { skip - skipped } else { 0 };
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
                    Style::default()
                        .bg(theme::WARN)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().bg(theme::WARN)
                };
                *style = style.patch(patch);
            }
        }
    }

    let mut out = Vec::new();
    let mut current_text = String::new();
    let mut current_style: Option<Style> = None;
    for (ch, style) in chars {
        if current_style.map_or(true, |s| s != style) {
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
}

impl Widget for SourceViewerWidget<'_> {
    fn render(mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(self.title())
            .borders(Borders::ALL)
            .border_style(theme::border());
        let inner = block.inner(area);
        block.render(area, buf);

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
            ViewerStatus::LargeFile { .. } => {
                let size = format_size(self.viewer.size_bytes);
                self.render_message(
                    inner,
                    buf,
                    "Large file",
                    &format!("This file is {size}.\nShowing a limited preview."),
                );
            }
            ViewerStatus::TooLarge => {
                let size = format_size(self.viewer.size_bytes);
                self.render_message(
                    inner,
                    buf,
                    "Large file",
                    &format!("This file is {size}.\nForge cannot preview files this large."),
                );
            }
            ViewerStatus::NotFound => {
                self.render_message(inner, buf, "File no longer exists", &self.viewer.rel_path);
            }
            ViewerStatus::Error(ref err) => {
                self.render_message(inner, buf, "Unable to open file", err);
            }
            ViewerStatus::Ok => self.render_content(inner, buf),
        }
    }
}

impl SourceViewerWidget<'_> {
    fn title(&self) -> String {
        let mut title = " Editor ".to_string();
        if let Some(notice) = &self.viewer.notice {
            title.push_str(&format!("· {notice} "));
        }
        title
    }

    fn render_message(&self, area: Rect, buf: &mut Buffer, heading: &str, body: &str) {
        let lines: Vec<Line> = std::iter::once(Line::styled(heading, theme::brand()))
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

        let mut header = format!(
            "{} · line {} of {}",
            self.viewer.rel_path,
            self.viewer.current_line + 1,
            total
        );
        if let Some(label) = &self.viewer.language_label {
            header.push_str(&format!(" · {label}"));
        }
        if self.viewer.highlight_disabled {
            header.push_str(" · plain text");
        }
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
                    let plain = vec![Span::styled(line.to_string(), theme::text())];
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
                    vec![Span::styled(text, theme::text())]
                }
            };

            let displayed_chars: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            let mut spans = spans;
            if selected && displayed_chars < content_width {
                let pad = " ".repeat(content_width - displayed_chars);
                spans.push(Span::styled(pad, theme::selected_row()));
            }

            let number = format!("{:>number_width$}", index + 1);
            let gutter_text = format!("{number} │ ");

            let gutter_style = if selected {
                theme::brand().add_modifier(Modifier::BOLD)
            } else {
                theme::muted()
            };

            let mut line_spans = vec![Span::styled(gutter_text, gutter_style)];
            if selected {
                line_spans.extend(spans.into_iter().map(|span| {
                    let style = span.style.patch(theme::selected_row());
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
            let text = format!("Search: {}{status}", self.viewer.search.query);
            Paragraph::new(Line::styled(text, theme::text())).render(area, buf);
            // Cursor position: after "Search: " + query.
            let cursor_x = area.x + 8 + self.viewer.search.query.chars().count() as u16;
            if cursor_x < area.x + area.width {
                buf[(cursor_x, area.y)].set_style(theme::caret());
            }
        } else if self.viewer.jump.open {
            let text = format!("Go to line: {}", self.viewer.jump.input);
            Paragraph::new(Line::styled(text, theme::text())).render(area, buf);
            let cursor_x = area.x + 12 + self.viewer.jump.input.chars().count() as u16;
            if cursor_x < area.x + area.width {
                buf[(cursor_x, area.y)].set_style(theme::caret());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
    fn open_shows_preview_for_large_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("big.txt");
        fs::write(&path, "x\n".repeat(2_000_000)).unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert!(matches!(viewer.status, ViewerStatus::LargeFile { .. }));
        assert!(!viewer.lines.is_empty());
        assert!(viewer.preview);
    }

    #[test]
    fn open_shows_not_found_after_deletion() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("gone.txt");
        fs::write(&path, "hi").unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);
        fs::remove_file(&path).unwrap();
        viewer.refresh(root.path());

        assert_eq!(viewer.status, ViewerStatus::NotFound);
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
    fn large_file_refused_over_threshold() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("huge.txt");
        fs::write(&path, "x".repeat(11 * 1024 * 1024)).unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert_eq!(viewer.status, ViewerStatus::TooLarge);
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
    fn large_file_disables_highlighting() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("big.rs");
        fs::write(&path, "let x = 1;\n".repeat(30_000)).unwrap();

        let mut viewer = SourceViewer::new();
        viewer.open(root.path(), &path);

        assert!(viewer.highlight_disabled);
        assert!(viewer.highlighted_lines.is_empty());
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
