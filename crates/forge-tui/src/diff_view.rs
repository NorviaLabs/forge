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
        self.patch = PatchState::Ready(Box::new(patch));
        self.loaded_for = Some((revision, path));
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn set_patch_error(&mut self, revision: u64, path: PathBuf, error: String) {
        self.patch = PatchState::Failed(error);
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

    pub fn header(&self) -> String {
        let source = self.source.label();
        let Some(entry) = self.selected_entry() else {
            return format!("DIFF · {source}");
        };
        let position = format!("{} of {}", self.selected + 1, self.entries.len());
        let path = entry.path.display();
        match &self.patch {
            PatchState::Ready(patch) if patch.binary => {
                format!("DIFF · {path} · binary · {position} · {source}")
            }
            PatchState::Ready(patch) => format!(
                "DIFF · {path} · +{} −{} · {position} · {source}",
                patch.added, patch.removed
            ),
            _ => format!("DIFF · {path} · {position} · {source}"),
        }
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

        let header = self.view.header();
        let header_area = Rect { height: 1, ..inner };
        Paragraph::new(Line::from(Span::styled(
            header,
            theme::heading().add_modifier(Modifier::BOLD),
        )))
        .render(header_area, buf);

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
            Paragraph::new(Line::from(crate::hints::hint_spans(
                crate::hints::DIFF,
                inner.width as usize,
            )))
            .render(hints, buf);
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
                render_message(body, buf, "Computing diff…", "");
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
            PatchState::Ready(patch) => {
                let path = patch.path.display().to_string();
                let rendered = crate::conversation::render_numbered_diff(
                    &path,
                    &patch.lines,
                    body.width as usize,
                );
                let visible: Vec<Line> = rendered
                    .into_iter()
                    .skip(self.view.scroll)
                    .take(body.height as usize)
                    .collect();
                Paragraph::new(visible).render(body, buf);
            }
        }
    }
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
        assert!(header.contains("+1 −1"), "{header}");
        assert!(header.contains("2 of 2"), "{header}");
        assert!(header.contains("working tree"), "{header}");
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
        for key in ["] [", "n p", "?", "Esc"] {
            assert!(out.contains(key), "lost {key:?} from:\n{out}");
        }
    }
}
