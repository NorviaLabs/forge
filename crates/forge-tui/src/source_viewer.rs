//! Read-only plain-text source viewer for the workspace Editor tab.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
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
        self.clamp_viewport();
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
            return;
        }

        let old_top = self.top_line;
        let old_current = self.current_line;
        let old_h = self.h_scroll;
        let old_rel = self.rel_path.clone();
        let old_modified = self.modified;
        let old_size = self.size_bytes;

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

fn clip_line(line: &str, skip: usize, width: usize) -> String {
    line.chars().skip(skip).take(width).collect::<String>()
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

pub struct SourceViewerWidget<'a> {
    pub viewer: &'a SourceViewer,
}

impl Widget for SourceViewerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(self.title())
            .borders(Borders::ALL)
            .border_style(theme::border());
        let inner = block.inner(area);
        block.render(area, buf);

        match self.viewer.status {
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

    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        // Reserve one row for the path/status header.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);

        let total = self.viewer.lines.len().max(1);
        let number_width = total.to_string().len().max(3);
        let gutter = (number_width + 3) as u16; // number + " │ "

        let header = format!(
            "{} · line {} of {}",
            self.viewer.rel_path,
            self.viewer.current_line + 1,
            total
        );
        Paragraph::new(Line::styled(header, theme::muted())).render(rows[0], buf);

        let body = rows[1];
        let visible_height = body.height as usize;
        let content_width = body.width.saturating_sub(gutter) as usize;

        let start = self.viewer.top_line.min(total.saturating_sub(1));
        let end = (start + visible_height).min(total);

        for (row, index) in (start..end).enumerate() {
            if row >= visible_height {
                break;
            }
            let y = body.y + row as u16;
            let selected = index == self.viewer.current_line;
            let line = self
                .viewer
                .lines
                .get(index)
                .map(String::as_str)
                .unwrap_or("");
            let mut visible = clip_line(line, self.viewer.h_scroll, content_width);
            // Fill the content area so the selected-row background spans the full width.
            while visible.chars().count() < content_width {
                visible.push(' ');
            }

            let number = format!("{:>number_width$}", index + 1);
            let gutter_text = format!("{number} │ ");

            let line_style = if selected {
                theme::selected_row()
            } else {
                theme::text()
            };
            let gutter_style = if selected {
                theme::brand().add_modifier(Modifier::BOLD)
            } else {
                theme::muted()
            };

            let spans = vec![
                Span::styled(gutter_text, gutter_style),
                Span::styled(visible, line_style),
            ];
            Line::from(spans).render(Rect::new(body.x, y, body.width, 1), buf);
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
}
