//! `/diff` — change review as a mode of the Workspace pane.
//!
//! Deliberately not a full-screen overlay. Codex, Claude Code and OpenCode all
//! take the whole terminal to show a diff, which means losing sight of the
//! conversation that produced it. Forge already has three panes, so the patch
//! renders where a file would and the transcript keeps streaming beside it.
//!
//! State only. The file list comes from
//! [`forge_workspace::git_status::GitStatusCache`] and patch text from
//! [`forge_workspace::git_review`]; both are already async and revision-keyed,
//! so this module never blocks the frame on `git`.

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Widget};

use forge_workspace::git_review::FileDiff;
use forge_workspace::git_status::{ChangedFile, GitStatusKind, PathStatus};

use crate::theme;

/// Which set of changes `/diff` is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffSource {
    /// Everything that separates the working tree from `HEAD`, including
    /// staged hunks and untracked files.
    #[default]
    WorkingTree,
    /// Only what the most recent assistant turn wrote, taken from the
    /// transcript's own diff cards rather than from `git`.
    LastTurn,
}

impl DiffSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::WorkingTree => "working tree",
            Self::LastTurn => "last turn",
        }
    }

    /// The other source, for the `d` toggle.
    pub fn toggled(self) -> Self {
        match self {
            Self::WorkingTree => Self::LastTurn,
            Self::LastTurn => Self::WorkingTree,
        }
    }
}

/// One row of the changed-file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    /// Repository-root-relative.
    pub path: PathBuf,
    pub marker: &'static str,
    pub untracked: bool,
}

/// A patch ready to render: hunk headers interleaved with their body lines,
/// in the shape [`crate::conversation::render_numbered_diff`] expects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patch {
    pub path: PathBuf,
    pub lines: Vec<String>,
    /// Index into `lines` where each hunk's `@@` header sits, for `]c`/`[c`.
    pub hunk_starts: Vec<usize>,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
}

impl Patch {
    pub fn from_file_diff(diff: &FileDiff) -> Self {
        let mut lines = Vec::new();
        let mut hunk_starts = Vec::new();
        for hunk in &diff.hunks {
            hunk_starts.push(lines.len());
            lines.push(hunk.header.clone());
            lines.extend(hunk.lines.iter().cloned());
        }
        Self::finish(diff.path.clone(), lines, hunk_starts, diff.binary)
    }

    /// Build from transcript diff-card lines, which are already unified-diff
    /// text but carry no `FileDiff` wrapper.
    pub fn from_lines(path: PathBuf, raw: &[String]) -> Self {
        let mut lines = Vec::new();
        let mut hunk_starts = Vec::new();
        for line in raw {
            if line.starts_with("diff --git ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("index ")
            {
                continue;
            }
            if line.starts_with("@@") {
                hunk_starts.push(lines.len());
            }
            lines.push(line.clone());
        }
        // A card with no `@@` header is still one contiguous run of changes.
        if hunk_starts.is_empty() && !lines.is_empty() {
            hunk_starts.push(0);
        }
        Self::finish(path, lines, hunk_starts, false)
    }

    fn finish(path: PathBuf, lines: Vec<String>, hunk_starts: Vec<usize>, binary: bool) -> Self {
        let added = lines
            .iter()
            .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
            .count();
        let removed = lines
            .iter()
            .filter(|line| line.starts_with('-') && !line.starts_with("---"))
            .count();
        Self {
            path,
            lines,
            hunk_starts,
            added,
            removed,
            binary,
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// What the patch pane has to show right now.
///
/// Every variant renders something. A silent pane is the failure mode this
/// enum exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PatchState {
    /// A file is selected but `git` has not answered yet.
    #[default]
    Loading,
    Ready(Box<Patch>),
    Failed(String),
}

/// Why the file list is empty, when it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffStatus {
    Ready,
    /// Git answered, and there is genuinely nothing to show.
    NoChanges,
    NotARepo,
    Failed(String),
}

/// In-patch search. Mirrors the source viewer's `/`: a prompt while it is
/// open, highlights that survive closing it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatchSearch {
    pub open: bool,
    pub query: String,
    /// Indices into the patch's lines, in order.
    pub matches: Vec<usize>,
    pub current: usize,
}

impl PatchSearch {
    pub fn is_active(&self) -> bool {
        !self.query.is_empty() && !self.matches.is_empty()
    }

    /// `2 of 7` for the header, or `no matches` once something is typed.
    pub fn label(&self) -> Option<String> {
        if self.query.is_empty() {
            return None;
        }
        if self.matches.is_empty() {
            return Some("no matches".into());
        }
        Some(format!("{} of {}", self.current + 1, self.matches.len()))
    }
}

/// Whether the patch renders as one column or as old-beside-new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatchLayout {
    #[default]
    Unified,
    Split,
}

/// Below this many columns a split view gives each side too little room to
/// read, so the pane falls back to unified even when Split is selected.
///
/// 72 leaves 36 per side. `v` also hides the explorer, because the patch pane
/// alone never reaches this in the three-pane layout — the sidebar takes the
/// width a wider terminal adds.
pub const SPLIT_MIN_WIDTH: u16 = 72;

#[derive(Debug, Clone)]
pub struct DiffView {
    pub source: DiffSource,
    pub entries: Vec<DiffEntry>,
    pub selected: usize,
    pub scroll: usize,
    pub patch: PatchState,
    pub status: DiffStatus,
    /// `(status revision, path)` the current patch was loaded for, so a status
    /// refresh invalidates it without a flash of stale content.
    pub loaded_for: Option<(u64, PathBuf)>,
    /// Height of the patch viewport as of the last render, for paging.
    pub viewport_height: usize,
    /// Width of the patch viewport as of the last render, so `v` can say
    /// whether split will actually fit before the next frame proves it.
    pub viewport_width: u16,
    pub search: PatchSearch,
    pub layout: PatchLayout,
    /// Files the reviewer has ticked off. Session-scoped: a review is a
    /// reading of *this* set of changes, so it does not outlive them.
    pub reviewed: std::collections::HashSet<PathBuf>,
    rendered_unified: Option<(u16, PathBuf, Vec<Line<'static>>)>,
}

impl Default for DiffView {
    fn default() -> Self {
        Self {
            source: DiffSource::default(),
            entries: Vec::new(),
            selected: 0,
            scroll: 0,
            patch: PatchState::Loading,
            status: DiffStatus::Ready,
            loaded_for: None,
            viewport_height: 20,
            viewport_width: 0,
            search: PatchSearch::default(),
            layout: PatchLayout::default(),
            reviewed: std::collections::HashSet::new(),
            rendered_unified: None,
        }
    }
}

impl DiffView {
    pub fn new(source: DiffSource) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }

    /// Replace the file list, keeping the selection anchored to the same path
    /// when it survives. A file that changes mid-turn must not scroll the pane
    /// out from under whoever is reading it.
    pub fn set_entries(&mut self, entries: Vec<DiffEntry>) {
        let previous = self.selected_path().map(Path::to_path_buf);
        let same = entries == self.entries;
        self.entries = entries;
        if self.entries.is_empty() {
            self.selected = 0;
            if matches!(self.status, DiffStatus::Ready) {
                self.status = DiffStatus::NoChanges;
            }
            self.patch = PatchState::Loading;
            self.loaded_for = None;
            return;
        }
        if matches!(self.status, DiffStatus::NoChanges) {
            self.status = DiffStatus::Ready;
        }
        self.selected = previous
            .as_deref()
            .and_then(|path| self.entries.iter().position(|entry| entry.path == path))
            .unwrap_or(0)
            .min(self.entries.len() - 1);
        if !same && previous.as_deref() != self.selected_path() {
            self.scroll = 0;
        }
    }

    pub fn selected_entry(&self) -> Option<&DiffEntry> {
        self.entries.get(self.selected)
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.selected_entry().map(|entry| entry.path.as_path())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Point the pane at `index`, resetting scroll because it is a new file.
    pub fn select(&mut self, index: usize) {
        if self.entries.is_empty() {
            return;
        }
        let index = index.min(self.entries.len() - 1);
        if index != self.selected {
            self.selected = index;
            self.scroll = 0;
            self.patch = PatchState::Loading;
            self.loaded_for = None;
        }
    }

    pub fn select_path(&mut self, path: &Path) {
        if let Some(index) = self.entries.iter().position(|entry| entry.path == path) {
            self.select(index);
        }
    }

    /// `]f` / `[f`. Wraps at both ends so the list is a ring, which matches
    /// how the explorer's own selection behaves.
    pub fn select_next_file(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let next = (self.selected + 1) % self.entries.len();
        self.select(next);
    }

    pub fn select_prev_file(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let prev = (self.selected + self.entries.len() - 1) % self.entries.len();
        self.select(prev);
    }

    fn patch_len(&self) -> usize {
        match &self.patch {
            PatchState::Ready(patch) => patch.len(),
            _ => 0,
        }
    }

    fn max_scroll(&self) -> usize {
        self.patch_len().saturating_sub(1)
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let target = self.scroll as isize + delta;
        self.scroll = target.clamp(0, self.max_scroll() as isize) as usize;
    }

    pub fn scroll_page(&mut self, pages: isize) {
        let half = (self.viewport_height.max(2) / 2) as isize;
        self.scroll_by(pages * half);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = self.max_scroll();
    }

    /// `]c` — the next hunk header strictly below the current scroll position.
    pub fn next_hunk(&mut self) {
        let PatchState::Ready(patch) = &self.patch else {
            return;
        };
        if let Some(next) = patch
            .hunk_starts
            .iter()
            .copied()
            .find(|start| *start > self.scroll)
        {
            self.scroll = next.min(self.max_scroll());
        }
    }

    /// `[c` — the previous hunk header strictly above the current position.
    pub fn prev_hunk(&mut self) {
        let PatchState::Ready(patch) = &self.patch else {
            return;
        };
        if let Some(prev) = patch
            .hunk_starts
            .iter()
            .copied()
            .rev()
            .find(|start| *start < self.scroll)
        {
            self.scroll = prev;
        }
    }

    /// Which line of the underlying *new* file the cursor is sitting on, so
    /// `o` can open the source viewer at the right place.
    pub fn new_file_line_at_scroll(&self) -> Option<usize> {
        let PatchState::Ready(patch) = &self.patch else {
            return None;
        };
        // `next` is the number the *upcoming* content line will carry; a hunk
        // header names that number rather than describing a line of its own.
        let mut next: Option<usize> = None;
        let mut current: Option<usize> = None;
        for line in patch.lines.iter().take(self.scroll + 1) {
            if let Some(start) = parse_hunk_new_start(line) {
                next = Some(start);
                current = Some(start);
            } else if line.starts_with('-') {
                // Removed lines have no counterpart in the new file, so the
                // cursor keeps the last line that does.
            } else {
                current = next;
                next = next.map(|n| n + 1);
            }
        }
        current
    }

    pub fn set_patch(&mut self, revision: u64, path: PathBuf, patch: Patch) {
        // A file whose patch changed is no longer the file that was reviewed.
        let replaced = match (&self.patch, &self.loaded_for) {
            (PatchState::Ready(current), Some((_, loaded))) => {
                *loaded == path && current.lines != patch.lines
            }
            _ => false,
        };
        if replaced {
            self.forget_review(&path);
        }
        let anchor = self.scroll;
        self.patch = PatchState::Ready(Box::new(patch));
        self.rendered_unified = None;
        self.loaded_for = Some((revision, path));
        self.scroll = anchor.min(self.max_scroll());
        // Matches are line indices into a patch that just changed.
        self.recompute_matches();
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn set_patch_error(&mut self, revision: u64, path: PathBuf, error: String) {
        self.patch = PatchState::Failed(error);
        self.rendered_unified = None;
        self.loaded_for = Some((revision, path));
    }

    /// Whether the pane is showing a patch that still matches `revision` and
    /// the selected path.
    pub fn patch_is_current(&self, revision: u64) -> bool {
        match (&self.loaded_for, self.selected_path()) {
            (Some((loaded_revision, loaded_path)), Some(path)) => {
                *loaded_revision == revision && loaded_path == path
            }
            _ => false,
        }
    }

    fn unified_lines(&mut self, width: u16) -> Option<&[Line<'static>]> {
        let PatchState::Ready(patch) = &self.patch else {
            return None;
        };
        let path = patch.path.clone();
        let needs_render = !matches!(
            self.rendered_unified.as_ref(),
            Some((cached_width, cached_path, _)) if *cached_width == width && *cached_path == path
        );
        if needs_render {
            let rendered = crate::conversation::render_numbered_diff(
                &path.display().to_string(),
                &patch.lines,
                width as usize,
            );
            self.rendered_unified = Some((width, path, rendered));
        }
        self.rendered_unified
            .as_ref()
            .map(|(_, _, lines)| lines.as_slice())
    }

    /// Header text for a pane `width` columns wide.
    ///
    /// The path is elided rather than letting the whole line truncate: losing
    /// leading directories still leaves a readable filename, whereas letting
    /// the line run off the edge silently drops the counts and the position,
    /// which is the state you cannot get anywhere else.
    pub fn header_for_width(&self, width: usize) -> String {
        let full = self.header();
        if full.chars().count() <= width || width < 20 {
            return full;
        }
        let Some(entry) = self.selected_entry() else {
            return full;
        };
        let path = entry.path.display().to_string();
        let overflow = full.chars().count() - width;
        let budget = path.chars().count().saturating_sub(overflow).max(12);
        full.replacen(&path, &crate::path_display::elide_path(&path, budget), 1)
    }

    pub fn header(&self) -> String {
        let source = self.source.label();
        let Some(entry) = self.selected_entry() else {
            return format!("DIFF · {source}");
        };
        let mut position = format!("{} of {}", self.selected + 1, self.entries.len());
        let reviewed = self.reviewed_count();
        if reviewed > 0 {
            position.push_str(&format!(" · {reviewed} reviewed"));
        }
        if self.selected_is_reviewed() {
            position.insert_str(0, "✓ ");
        }
        let path = entry.path.display();
        let counts = match &self.patch {
            PatchState::Ready(patch) if patch.binary => " · binary".to_string(),
            // DESIGN-017: ASCII `+`/`-` — the old `−` (U+2212) broke the
            // no-Unicode-chrome rule and its glyph was missing in some fonts.
            PatchState::Ready(patch) => format!(" · +{} -{}", patch.added, patch.removed),
            _ => String::new(),
        };
        // The pane is ~50 columns wide in practice, so anything appended here
        // is invisible. Only the *unusual* source earns a place; the working
        // tree is the default and says nothing. Search and layout state live
        // on the bottom row instead.
        let source = match self.source {
            DiffSource::WorkingTree => String::new(),
            DiffSource::LastTurn => format!(" · {source}"),
        };
        format!("DIFF · {path}{counts} · {position}{source}")
    }

    // ---- review state -------------------------------------------------

    pub fn reviewed_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| self.reviewed.contains(&entry.path))
            .count()
    }

    pub fn selected_is_reviewed(&self) -> bool {
        self.selected_path()
            .is_some_and(|path| self.reviewed.contains(path))
    }

    /// `m`. Ticking the last file off is the end of the review, so say so by
    /// leaving the caller something to report rather than silently wrapping.
    pub fn toggle_reviewed(&mut self) -> bool {
        let Some(path) = self.selected_path().map(Path::to_path_buf) else {
            return false;
        };
        if !self.reviewed.remove(&path) {
            self.reviewed.insert(path);
            return true;
        }
        false
    }

    /// A file whose content changed is no longer the file that was reviewed.
    pub fn forget_review(&mut self, path: &Path) {
        self.reviewed.remove(path);
    }

    pub fn all_reviewed(&self) -> bool {
        !self.entries.is_empty() && self.reviewed_count() == self.entries.len()
    }

    // ---- layout -------------------------------------------------------

    /// `v`.
    pub fn toggle_layout(&mut self) {
        self.layout = match self.layout {
            PatchLayout::Unified => PatchLayout::Split,
            PatchLayout::Split => PatchLayout::Unified,
        };
    }

    /// What the pane will actually draw at `width`, which is not always what
    /// is selected — split needs room the three-pane layout may not have.
    pub fn effective_layout(&self, width: u16) -> PatchLayout {
        match self.layout {
            PatchLayout::Split if width >= SPLIT_MIN_WIDTH => PatchLayout::Split,
            _ => PatchLayout::Unified,
        }
    }

    // ---- search -------------------------------------------------------

    pub fn open_search(&mut self) {
        self.search.open = true;
    }

    /// Closing keeps the query and its highlights; only the prompt goes away.
    pub fn close_search(&mut self) {
        self.search.open = false;
    }

    pub fn clear_search(&mut self) {
        self.search = PatchSearch::default();
    }

    pub fn push_search_char(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        self.search.query.push(ch);
        self.recompute_matches_and_jump();
    }

    pub fn backspace_search(&mut self) {
        self.search.query.pop();
        self.recompute_matches_and_jump();
    }

    /// Recompute from scratch: the patch may have been replaced underneath a
    /// live query by a status refresh.
    pub fn recompute_matches(&mut self) {
        let query = self.search.query.to_lowercase();
        self.search.matches = match (&self.patch, query.is_empty()) {
            (PatchState::Ready(patch), false) => patch
                .lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(&query))
                .map(|(index, _)| index)
                .collect(),
            _ => Vec::new(),
        };
        self.search.current = 0;
    }

    /// Typing in the prompt recomputes *and* jumps to the first hit. Reloading
    /// a patch recomputes without moving the reader, which is why these are
    /// two calls rather than one.
    fn recompute_matches_and_jump(&mut self) {
        self.recompute_matches();
        if let Some(first) = self.search.matches.first().copied() {
            self.scroll = first.min(self.max_scroll());
        }
    }

    /// Enter while the prompt is open, and `Tab` once it is closed.
    pub fn next_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        self.search.current = (self.search.current + 1) % self.search.matches.len();
        self.scroll = self.search.matches[self.search.current].min(self.max_scroll());
    }

    pub fn prev_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        self.search.current =
            (self.search.current + self.search.matches.len() - 1) % self.search.matches.len();
        self.scroll = self.search.matches[self.search.current].min(self.max_scroll());
    }

    pub fn line_is_match(&self, index: usize) -> bool {
        self.search.is_active() && self.search.matches.contains(&index)
    }
}

fn parse_hunk_new_start(line: &str) -> Option<usize> {
    // `@@ -12,7 +14,9 @@ fn thing()` -> 14
    let rest = line.strip_prefix("@@ ")?;
    let plus = rest.split('+').nth(1)?;
    let digits: String = plus.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Build the changed-file list from a git status snapshot.
///
/// Ignored paths are dropped: the explorer currently lists `__pycache__` and
/// friends, and the diff view must not inherit that.
pub fn entries_from_changed_files(files: &[ChangedFile]) -> Vec<DiffEntry> {
    let mut entries: Vec<DiffEntry> = files
        .iter()
        .filter_map(|file| {
            let status = PathStatus {
                staged: file.staged,
                unstaged: file.unstaged,
            };
            let primary = status.primary()?;
            if primary == GitStatusKind::Ignored {
                return None;
            }
            Some(DiffEntry {
                path: file.path.clone(),
                marker: if status.is_untracked() {
                    // An untracked file is an addition as far as review goes;
                    // `?` is a status-porcelain detail, not a review concept.
                    GitStatusKind::Added.marker()
                } else {
                    primary.marker()
                },
                untracked: status.is_untracked(),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

pub struct DiffViewWidget<'a> {
    pub view: &'a mut DiffView,
    pub focused: bool,
}

impl Widget for DiffViewWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
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
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // DESIGN-017: the header names the selected file, so it renders as
        // the pane title — `>` marker plus focus colour when the pane owns
        // input, neutral indent otherwise. The 2-cell marker column comes
        // off the elision budget so the counts and position never clip.
        let header = self
            .view
            .header_for_width(inner.width.saturating_sub(2) as usize);
        let header_area = Rect { height: 1, ..inner };
        Paragraph::new(theme::pane_title(self.focused, &header)).render(header_area, buf);

        // The keymap is not obvious and `?` is not discoverable on its own, so
        // the pane carries its own hint row. `hint_spans` drops verbs before
        // pairs, so a narrow pane still shows every key.
        let hint_row = inner.height >= 3;
        if hint_row {
            let hints = Rect {
                y: inner.y.saturating_add(inner.height.saturating_sub(1)),
                height: 1,
                ..inner
            };
            // While `/` is open the bottom row is the prompt: the keys it
            // would otherwise advertise are not the keys that are live.
            if self.view.search.open {
                let mut spans = vec![
                    Span::styled(
                        "/".to_string(),
                        theme::metadata_style().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(self.view.search.query.clone(), theme::text()),
                    Span::styled("▏".to_string(), theme::text()),
                ];
                if let Some(label) = self.view.search.label() {
                    spans.push(Span::styled(format!("  {label}"), theme::metadata_style()));
                }
                spans.push(Span::styled(
                    "  Enter next · Esc close".to_string(),
                    theme::metadata_style(),
                ));
                Paragraph::new(Line::from(spans)).render(hints, buf);
            } else {
                // A right-aligned tag for the layout, so the header does not
                // have to carry state it has no room for.
                let tag = match self.view.effective_layout(inner.width) {
                    PatchLayout::Split => "split",
                    PatchLayout::Unified => "",
                };
                let budget = inner.width as usize - tag.len().min(inner.width as usize);
                Paragraph::new(Line::from(crate::hints::hint_spans(
                    crate::hints::DIFF,
                    budget,
                )))
                .render(hints, buf);
                if !tag.is_empty() {
                    Paragraph::new(Line::from(Span::styled(
                        tag.to_string(),
                        theme::metadata_style(),
                    )))
                    .alignment(Alignment::Right)
                    .render(hints, buf);
                }
            }
        }

        let body = Rect {
            y: inner.y.saturating_add(1),
            height: inner.height.saturating_sub(if hint_row { 2 } else { 1 }),
            ..inner
        };
        if body.height == 0 {
            return;
        }
        self.view.viewport_height = body.height as usize;
        self.view.viewport_width = body.width;

        match &self.view.status {
            DiffStatus::NotARepo => {
                render_message(
                    body,
                    buf,
                    "Not a git repository",
                    "Forge can only diff a workspace tracked by git.",
                );
                return;
            }
            DiffStatus::Failed(error) => {
                render_message(body, buf, "Could not compute the diff", error);
                return;
            }
            DiffStatus::NoChanges => {
                let hint = match self.view.source {
                    DiffSource::WorkingTree => {
                        "Nothing differs from HEAD.\nPress d to see the last turn's edits."
                    }
                    DiffSource::LastTurn => {
                        "The last turn did not edit any files.\nPress d to see the working tree."
                    }
                };
                let heading = match self.view.source {
                    DiffSource::WorkingTree => "No changes in the working tree",
                    DiffSource::LastTurn => "No changes in the last turn",
                };
                render_message(body, buf, heading, hint);
                return;
            }
            DiffStatus::Ready => {}
        }

        match &self.view.patch {
            PatchState::Loading => {
                render_message(body, buf, "Computing diff...", "");
            }
            PatchState::Failed(error) => {
                render_message(body, buf, "Could not compute the diff", error);
            }
            PatchState::Ready(patch) if patch.binary => {
                render_message(
                    body,
                    buf,
                    "Binary file",
                    "Forge cannot show this change as text.",
                );
            }
            PatchState::Ready(patch) if patch.is_empty() => {
                render_message(body, buf, "No textual change", "");
            }
            PatchState::Ready(patch) => match self.view.effective_layout(body.width) {
                PatchLayout::Unified => {
                    let scroll = self.view.scroll;
                    let search = self.view.search.clone();
                    let rendered = self.view.unified_lines(body.width).unwrap_or_default();
                    let visible: Vec<Line> = rendered
                        .iter()
                        .cloned()
                        .enumerate()
                        .skip(scroll)
                        .take(body.height as usize)
                        .map(|(index, line)| {
                            highlight_match(
                                line,
                                search.is_active() && search.matches.contains(&index),
                            )
                        })
                        .collect();
                    Paragraph::new(visible).render(body, buf);
                }
                PatchLayout::Split => render_split(self.view, patch, body, buf),
            },
        }
    }
}

/// Re-style a rendered patch row as a search hit. Applied after
/// `render_numbered_diff` so syntax highlighting still decides the text and
/// only the emphasis changes.
fn highlight_match(line: Line<'static>, is_match: bool) -> Line<'static> {
    if !is_match {
        return line;
    }
    let spans = line
        .spans
        .into_iter()
        .map(|span| {
            let style = span.style.patch(theme::search_match());
            Span::styled(span.content, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// Old on the left, new on the right. Context lines appear on both sides;
/// a removal leaves the right side blank and an addition the left, so the
/// two columns stay vertically aligned with each other.
///
/// Each side carries its own line number and `-/+/space` marker plus the same
/// syntax highlighting the unified view paints — bare colored text lost
/// position context the moment review switched to split mode.
fn render_split(view: &DiffView, patch: &Patch, area: Rect, buf: &mut Buffer) {
    let gutter = 1u16;
    let column = area.width.saturating_sub(gutter) / 2;
    if column < 4 {
        return;
    }
    let left_area = Rect {
        width: column,
        ..area
    };
    let right_area = Rect {
        x: area.x.saturating_add(column).saturating_add(gutter),
        width: column,
        ..area
    };

    let numbered = crate::conversation::number_diff_lines(&patch.lines);
    let number_width = numbered
        .iter()
        .flat_map(|line| [line.old, line.new])
        .flatten()
        .max()
        .map(|line| line.to_string().len())
        .unwrap_or(1);
    // One highlight pass over the new-file-order contents, indexed in row
    // order exactly like the unified view — context rows share one index so
    // both sides paint identical spans for identical text.
    let code = numbered
        .iter()
        .filter(|line| !line.header)
        .map(|line| line.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let highlighted = crate::conversation::lang_from_path(&patch.path.display().to_string())
        .map(|lang| forge_syntax::highlight_to_lines(lang, &code, &crate::theme::syntax_theme()));
    let mut code_index = 0;
    let mut rows: Vec<(Line<'static>, Line<'static>)> = Vec::with_capacity(numbered.len());
    for line in &numbered {
        // Search hits index raw patch lines; numbering skips preambles, so
        // the rendered position cannot stand in for the match index.
        let hit = view.line_is_match(line.raw);
        if line.header {
            let style = if hit {
                theme::diff_hunk().patch(theme::search_match())
            } else {
                theme::diff_hunk()
            };
            let header = Line::from(Span::styled(line.content.clone(), style));
            rows.push((header.clone(), header));
            continue;
        }
        let parts = highlighted.as_ref().and_then(|lines| lines.get(code_index));
        code_index += 1;
        let (left, right) = match line.marker {
            '-' => (
                Some(split_cell(
                    line.old,
                    number_width,
                    '-',
                    parts,
                    theme::diff_remove(),
                    hit,
                )),
                None,
            ),
            '+' => (
                None,
                Some(split_cell(
                    line.new,
                    number_width,
                    '+',
                    parts,
                    theme::diff_add(),
                    hit,
                )),
            ),
            _ => (
                Some(split_cell(
                    line.old,
                    number_width,
                    ' ',
                    parts,
                    theme::diff_context(),
                    hit,
                )),
                Some(split_cell(
                    line.new,
                    number_width,
                    ' ',
                    parts,
                    theme::diff_context(),
                    hit,
                )),
            ),
        };
        rows.push((
            left.unwrap_or_else(|| Line::from(Span::styled(String::new(), theme::panel()))),
            right.unwrap_or_else(|| Line::from(Span::styled(String::new(), theme::panel()))),
        ));
    }

    let take = area.height as usize;
    let skip = view.scroll;
    Paragraph::new(
        rows.iter()
            .map(|(left, _)| left.clone())
            .skip(skip)
            .take(take)
            .collect::<Vec<_>>(),
    )
    .render(left_area, buf);
    Paragraph::new(
        rows.iter()
            .map(|(_, right)| right.clone())
            .skip(skip)
            .take(take)
            .collect::<Vec<_>>(),
    )
    .render(right_area, buf);
}

/// One side of a split row: right-aligned line number, marker, content.
///
/// Numbers and markers carry the row's diff background so the gutter reads
/// as part of the row; syntax spans keep the same background via
/// `syntax_segment`, mirroring the unified view.
fn split_cell(
    num: Option<usize>,
    number_width: usize,
    marker: char,
    parts: Option<&Vec<forge_syntax::HighlightedSegment>>,
    line_style: ratatui::style::Style,
    hit: bool,
) -> Line<'static> {
    let style = if hit {
        line_style.patch(theme::search_match())
    } else {
        line_style
    };
    let num = num.map(|num| num.to_string()).unwrap_or_default();
    let mut spans = vec![Span::styled(
        format!("{num:>number_width$} {marker} "),
        style,
    )];
    if let Some(parts) = parts {
        for (text, rgb, bold, italic) in parts {
            let mut style =
                theme::syntax_segment(*rgb, Some(line_style.bg.unwrap_or(theme::panel_alt_bg())));
            if hit {
                style = style.patch(theme::search_match());
            }
            if *bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if *italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            spans.push(Span::styled(text.clone(), style));
        }
    }
    Line::from(spans)
}

fn render_message(area: Rect, buf: &mut Buffer, heading: &str, body: &str) {
    let lines: Vec<Line> = std::iter::once(Line::styled(heading, theme::heading()))
        .chain(body.lines().map(Line::raw))
        .collect();
    Paragraph::new(lines)
        .alignment(Alignment::Center)
        .render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_workspace::git_review::{DiffHunk, FileDiff};

    fn changed(
        path: &str,
        staged: Option<GitStatusKind>,
        unstaged: Option<GitStatusKind>,
    ) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            staged,
            unstaged,
        }
    }

    fn file_diff(path: &str, hunks: Vec<DiffHunk>) -> FileDiff {
        FileDiff {
            path: PathBuf::from(path),
            headers: Vec::new(),
            hunks,
            binary: false,
            untracked: false,
        }
    }

    fn hunk(header: &str, lines: &[&str]) -> DiffHunk {
        DiffHunk {
            header: header.into(),
            lines: lines.iter().map(|l| (*l).to_string()).collect(),
        }
    }

    #[test]
    fn untracked_files_are_listed_as_additions() {
        // Codex and OpenCode both show untracked files; Claude Code only shows
        // them when the same session created them. Ours never depends on that.
        let entries = entries_from_changed_files(&[
            changed("new.rs", None, Some(GitStatusKind::Untracked)),
            changed("old.rs", None, Some(GitStatusKind::Modified)),
        ]);
        assert_eq!(entries.len(), 2);
        let new = entries
            .iter()
            .find(|e| e.path == Path::new("new.rs"))
            .unwrap();
        assert_eq!(new.marker, "A");
        assert!(new.untracked);
    }

    #[test]
    fn ignored_paths_never_appear() {
        let entries = entries_from_changed_files(&[
            changed("__pycache__/x.pyc", None, Some(GitStatusKind::Ignored)),
            changed("real.rs", None, Some(GitStatusKind::Modified)),
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, Path::new("real.rs"));
    }

    #[test]
    fn empty_list_reports_no_changes_rather_than_staying_silent() {
        let mut view = DiffView::default();
        view.set_entries(Vec::new());
        assert_eq!(view.status, DiffStatus::NoChanges);
        assert!(view.is_empty());
    }

    #[test]
    fn file_navigation_wraps_at_both_ends() {
        let mut view = DiffView::default();
        view.set_entries(entries_from_changed_files(&[
            changed("a.rs", None, Some(GitStatusKind::Modified)),
            changed("b.rs", None, Some(GitStatusKind::Modified)),
        ]));
        assert_eq!(view.selected, 0);
        view.select_prev_file();
        assert_eq!(view.selected, 1, "[f wraps backwards from the first file");
        view.select_next_file();
        assert_eq!(view.selected, 0, "]f wraps forwards from the last file");
    }

    #[test]
    fn hunk_motions_land_on_hunk_headers() {
        let mut view = DiffView::default();
        view.set_entries(entries_from_changed_files(&[changed(
            "a.rs",
            None,
            Some(GitStatusKind::Modified),
        )]));
        let patch = Patch::from_file_diff(&file_diff(
            "a.rs",
            vec![
                hunk("@@ -1,2 +1,2 @@", &["-old", "+new"]),
                hunk("@@ -20,2 +20,2 @@", &["-far", "+away"]),
            ],
        ));
        assert_eq!(patch.hunk_starts, vec![0, 3]);
        view.set_patch(1, PathBuf::from("a.rs"), patch);

        view.next_hunk();
        assert_eq!(view.scroll, 3);
        view.next_hunk();
        assert_eq!(view.scroll, 3, "]c stops at the last hunk");
        view.prev_hunk();
        assert_eq!(view.scroll, 0);
    }

    #[test]
    fn counts_ignore_diff_file_headers() {
        let patch = Patch::from_lines(
            PathBuf::from("a.rs"),
            &[
                "diff --git a/a.rs b/a.rs".into(),
                "--- a/a.rs".into(),
                "+++ b/a.rs".into(),
                "@@ -1,1 +1,2 @@".into(),
                " keep".into(),
                "+added".into(),
                "-removed".into(),
            ],
        );
        assert_eq!(patch.added, 1, "+++ must not count as an addition");
        assert_eq!(patch.removed, 1, "--- must not count as a removal");
        assert_eq!(patch.hunk_starts, vec![0]);
    }

    #[test]
    fn a_file_changing_mid_turn_does_not_move_the_reader() {
        let mut view = DiffView::default();
        view.set_entries(entries_from_changed_files(&[changed(
            "b.rs",
            None,
            Some(GitStatusKind::Modified),
        )]));
        view.set_patch(
            1,
            PathBuf::from("b.rs"),
            Patch::from_file_diff(&file_diff(
                "b.rs",
                vec![hunk("@@ -1,4 +1,4 @@", &[" a", " b", " c", "+d"])],
            )),
        );
        view.scroll_by(3);
        let before = view.scroll;

        // A new file appears above `b.rs` alphabetically.
        view.set_entries(entries_from_changed_files(&[
            changed("a.rs", None, Some(GitStatusKind::Untracked)),
            changed("b.rs", None, Some(GitStatusKind::Modified)),
        ]));
        assert_eq!(view.selected_path(), Some(Path::new("b.rs")));
        assert_eq!(view.scroll, before, "scroll stays put when the file does");
    }

    #[test]
    fn source_toggle_round_trips() {
        assert_eq!(DiffSource::WorkingTree.toggled(), DiffSource::LastTurn);
        assert_eq!(DiffSource::LastTurn.toggled(), DiffSource::WorkingTree);
        assert_eq!(DiffSource::WorkingTree.label(), "working tree");
    }

    #[test]
    fn header_names_the_file_position_and_source() {
        let mut view = DiffView::default();
        view.set_entries(entries_from_changed_files(&[
            changed("a.rs", None, Some(GitStatusKind::Modified)),
            changed("b.rs", None, Some(GitStatusKind::Modified)),
        ]));
        view.select_next_file();
        view.set_patch(
            1,
            PathBuf::from("b.rs"),
            Patch::from_file_diff(&file_diff(
                "b.rs",
                vec![hunk("@@ -1,1 +1,1 @@", &["-old", "+new"])],
            )),
        );
        let header = view.header();
        assert!(header.contains("b.rs"), "{header}");
        assert!(header.contains("+1 -1"), "{header}");
        assert!(header.contains("2 of 2"), "{header}");
    }

    #[test]
    fn open_at_line_tracks_the_new_file_numbering() {
        let mut view = DiffView::default();
        view.set_entries(entries_from_changed_files(&[changed(
            "a.rs",
            None,
            Some(GitStatusKind::Modified),
        )]));
        view.set_patch(
            1,
            PathBuf::from("a.rs"),
            Patch::from_file_diff(&file_diff(
                "a.rs",
                vec![hunk("@@ -10,3 +14,3 @@", &[" ctx", "-old", "+new"])],
            )),
        );
        view.scroll = 1; // the context line, which is line 14 of the new file
        assert_eq!(view.new_file_line_at_scroll(), Some(14));
    }

    #[test]
    fn a_stale_patch_is_not_treated_as_current() {
        let mut view = DiffView::default();
        view.set_entries(entries_from_changed_files(&[changed(
            "a.rs",
            None,
            Some(GitStatusKind::Modified),
        )]));
        view.set_patch(7, PathBuf::from("a.rs"), Patch::default());
        assert!(view.patch_is_current(7));
        assert!(
            !view.patch_is_current(8),
            "a status refresh must invalidate the cached patch"
        );
    }
}

#[cfg(test)]
mod widget_tests {
    use super::*;
    use forge_workspace::git_review::DiffHunk;
    use ratatui::layout::Rect;

    fn view_with_patch() -> DiffView {
        let mut view = DiffView::default();
        view.set_entries(entries_from_changed_files(&[ChangedFile {
            path: PathBuf::from("a.rs"),
            staged: None,
            unstaged: Some(GitStatusKind::Modified),
        }]));
        view.set_patch(
            1,
            PathBuf::from("a.rs"),
            Patch::from_file_diff(&FileDiff {
                path: PathBuf::from("a.rs"),
                headers: Vec::new(),
                hunks: vec![DiffHunk {
                    header: "@@ -1,2 +1,2 @@".into(),
                    lines: vec!["-old".into(), "+new".into()],
                }],
                binary: false,
                untracked: false,
            }),
        );
        view
    }

    fn render(view: &mut DiffView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        DiffViewWidget {
            view,
            focused: true,
        }
        .render(area, &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_keymap_is_on_screen_not_only_behind_a_shortcut() {
        let mut view = view_with_patch();
        let out = render(&mut view, 60, 12);
        assert!(out.contains("] [ hunk"), "{out}");
        assert!(out.contains("Esc close"), "{out}");
    }

    #[test]
    fn the_hint_row_costs_the_patch_exactly_one_line() {
        let mut view = view_with_patch();
        // 12 rows: 2 borders, 1 header, 1 hint, 8 for the patch.
        render(&mut view, 60, 12);
        assert_eq!(view.viewport_height, 8);
    }

    #[test]
    fn a_pane_too_short_for_a_hint_row_keeps_the_patch_line_instead() {
        let mut view = view_with_patch();
        // 4 rows: 2 borders, 1 header, and the last row goes to the patch
        // rather than to a hint nobody has room to read.
        render(&mut view, 60, 4);
        assert_eq!(view.viewport_height, 1);
    }

    #[test]
    fn a_narrow_pane_drops_the_verbs_but_keeps_every_key() {
        let mut view = view_with_patch();
        let out = render(&mut view, 30, 12);
        assert!(!out.contains("] [ hunk"), "verbs go first:\n{out}");
        for key in ["] [", "n p", "m", "?", "Esc"] {
            assert!(out.contains(key), "lost {key:?} from:\n{out}");
        }
    }
}

#[cfg(test)]
mod v2_tests {
    use super::*;
    use forge_workspace::git_review::DiffHunk;

    fn patch_with(lines: &[&str]) -> Patch {
        Patch::from_file_diff(&FileDiff {
            path: PathBuf::from("a.rs"),
            headers: Vec::new(),
            hunks: vec![DiffHunk {
                header: "@@ -1,4 +1,4 @@".into(),
                lines: lines.iter().map(|l| (*l).to_string()).collect(),
            }],
            binary: false,
            untracked: false,
        })
    }

    fn view_with(lines: &[&str]) -> DiffView {
        let mut view = DiffView::default();
        view.set_entries(vec![DiffEntry {
            path: PathBuf::from("a.rs"),
            marker: "M",
            untracked: false,
        }]);
        view.set_patch(1, PathBuf::from("a.rs"), patch_with(lines));
        view
    }

    #[test]
    fn search_finds_lines_case_insensitively_and_cycles() {
        let mut view = view_with(&[" alpha", "+Beta", " gamma", "+beta again"]);
        view.open_search();
        for ch in "beta".chars() {
            view.push_search_char(ch);
        }
        assert_eq!(view.search.matches.len(), 2, "{:?}", view.search.matches);
        let first = view.scroll;
        view.next_match();
        assert_ne!(view.scroll, first);
        view.next_match();
        assert_eq!(view.scroll, first, "the hit list is a ring");
        view.prev_match();
        assert_ne!(view.scroll, first);
    }

    #[test]
    fn closing_search_keeps_the_highlights() {
        let mut view = view_with(&[" alpha", "+beta"]);
        view.open_search();
        for ch in "beta".chars() {
            view.push_search_char(ch);
        }
        view.close_search();
        assert!(!view.search.open, "the prompt is gone");
        assert!(view.search.is_active(), "the highlights are not");
        view.clear_search();
        assert!(!view.search.is_active());
    }

    #[test]
    fn a_query_with_no_hits_says_so_rather_than_looking_broken() {
        let mut view = view_with(&[" alpha"]);
        view.open_search();
        for ch in "zzz".chars() {
            view.push_search_char(ch);
        }
        assert_eq!(view.search.label().as_deref(), Some("no matches"));
        assert!(!view.search.is_active());
    }

    #[test]
    fn reloading_a_patch_recomputes_matches_without_moving_the_reader() {
        let mut view = view_with(&[" a", " b", " c", "+needle", " d"]);
        view.open_search();
        for ch in "needle".chars() {
            view.push_search_char(ch);
        }
        view.scroll = 1;
        // Same content, new revision: the reader must not be yanked to the hit.
        view.set_patch(
            2,
            PathBuf::from("a.rs"),
            patch_with(&[" a", " b", " c", "+needle", " d"]),
        );
        assert_eq!(view.scroll, 1, "scroll is where the reader left it");
        assert_eq!(view.search.matches.len(), 1, "matches are still correct");
    }

    #[test]
    fn marking_a_file_reviewed_counts_it() {
        let mut view = DiffView::default();
        view.set_entries(vec![
            DiffEntry {
                path: PathBuf::from("a.rs"),
                marker: "M",
                untracked: false,
            },
            DiffEntry {
                path: PathBuf::from("b.rs"),
                marker: "M",
                untracked: false,
            },
        ]);
        assert!(view.toggle_reviewed(), "first press marks");
        assert_eq!(view.reviewed_count(), 1);
        assert!(view.selected_is_reviewed());
        assert!(!view.all_reviewed());
        assert!(!view.toggle_reviewed(), "second press unmarks");
        assert_eq!(view.reviewed_count(), 0);
    }

    #[test]
    fn a_file_that_changes_loses_its_review_mark() {
        // A tick means "I read this". If the content moves under it, it is a
        // claim about something that no longer exists.
        let mut view = view_with(&[" a", "+b"]);
        assert!(view.toggle_reviewed());
        assert!(view.selected_is_reviewed());
        view.set_patch(2, PathBuf::from("a.rs"), patch_with(&[" a", "+b", "+c"]));
        assert!(
            !view.selected_is_reviewed(),
            "changed content invalidates the mark"
        );
    }

    #[test]
    fn split_falls_back_to_unified_when_the_pane_is_too_narrow() {
        let mut view = view_with(&[" a"]);
        view.toggle_layout();
        assert_eq!(view.layout, PatchLayout::Split);
        assert_eq!(
            view.effective_layout(SPLIT_MIN_WIDTH - 1),
            PatchLayout::Unified,
            "a cramped split is worse than unified"
        );
        assert_eq!(view.effective_layout(SPLIT_MIN_WIDTH), PatchLayout::Split);
    }

    #[test]
    fn split_rows_put_removals_left_and_additions_right() {
        // Numbering (shared with the unified view) decides sides now: a
        // removal carries only an old number, an addition only a new one.
        let numbered = crate::conversation::number_diff_lines(&[
            "@@ -1 +1 @@".to_string(),
            "-gone".to_string(),
            "+new".to_string(),
            " same".to_string(),
        ]);
        assert_eq!(numbered.len(), 4);
        assert!(numbered[0].header, "hunk header spans both columns");
        assert_eq!(
            (numbered[1].old, numbered[1].new, numbered[1].marker),
            (Some(1), None, '-')
        );
        assert_eq!(
            (numbered[2].old, numbered[2].new, numbered[2].marker),
            (None, Some(1), '+')
        );
        assert_eq!(
            (numbered[3].old, numbered[3].new, numbered[3].marker),
            (Some(2), Some(2), ' ')
        );
        // Raw indices survive numbering so search hits (indexed on patch
        // lines) still land on the right rendered row.
        let raws: Vec<usize> = numbered.iter().map(|line| line.raw).collect();
        assert_eq!(raws, vec![0, 1, 2, 3]);
    }

    #[test]
    fn split_cells_carry_number_and_marker_gutters() {
        let cell = split_cell(Some(12), 2, '-', None, theme::diff_remove(), false);
        let text: String = cell
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.starts_with("12 - "), "{text:?}");
        let blank = split_cell(None, 2, ' ', None, theme::diff_context(), false);
        let blank_text: String = blank
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(blank_text.starts_with("   "), "{blank_text:?}");
    }

    #[test]
    fn the_header_reports_review_state_and_leaves_the_rest_to_the_bottom_row() {
        let mut view = view_with(&[" alpha", "+beta"]);
        view.toggle_layout();
        view.toggle_reviewed();
        view.open_search();
        for ch in "beta".chars() {
            view.push_search_char(ch);
        }
        let header = view.header();
        // Layout and search live on the bottom row: at the ~50 columns this
        // pane really gets, anything appended past the counts is invisible.
        assert!(!header.contains("split"), "{header}");
        assert!(!header.contains("/beta"), "{header}");
        assert!(header.contains("✓"), "{header}");
        assert!(header.contains("1 reviewed"), "{header}");
        assert!(header.contains("a.rs"), "{header}");
    }
}

#[cfg(test)]
mod header_budget_tests {
    use super::*;

    #[test]
    fn the_default_source_earns_no_words_in_the_header() {
        // The pane is ~50 columns. "· working tree" is 14 of them spent
        // saying the thing that is true unless stated otherwise.
        let mut view = DiffView::default();
        view.set_entries(vec![DiffEntry {
            path: PathBuf::from("tracker/metrics.py"),
            marker: "M",
            untracked: false,
        }]);
        let working = view.header();
        assert!(!working.contains("working tree"), "{working}");

        view.source = DiffSource::LastTurn;
        let turn = view.header();
        assert!(turn.contains("last turn"), "{turn}");
    }

    #[test]
    fn a_realistic_header_fits_the_pane_it_gets() {
        let mut view = DiffView::default();
        view.set_entries(
            (1..=7)
                .map(|n| DiffEntry {
                    path: PathBuf::from(format!("crates/forge-tui/src/file_{n}.rs")),
                    marker: "M",
                    untracked: false,
                })
                .collect(),
        );
        view.set_patch(
            1,
            PathBuf::from("crates/forge-tui/src/file_1.rs"),
            Patch::from_lines(
                PathBuf::from("crates/forge-tui/src/file_1.rs"),
                &["@@ -1 +1 @@".into(), "-a".into(), "+b".into()],
            ),
        );
        let header = view.header_for_width(50);
        assert!(
            header.chars().count() <= 50,
            "{} chars: {header}",
            header.chars().count()
        );
        assert!(
            header.contains("file_1.rs"),
            "the filename survives: {header}"
        );
        assert!(header.contains("1 of 7"), "so does the position: {header}");
        assert!(header.contains("+1 -1"), "and the counts: {header}");
    }
}
