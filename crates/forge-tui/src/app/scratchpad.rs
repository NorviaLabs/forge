//! Session-scoped scratchpad: running notes that belong to the operator.
//!
//! The scratchpad is deliberately *not* agent memory. Nothing here reaches the
//! model: it costs zero tokens, carries no prompt-injection surface, and is
//! never summarized. It is a plain text document, one per session, that the
//! operator opens and edits inside the TUI.
//!
//! # Where the bytes live
//!
//! The path is derived from the *resolved journal directory*
//! (`TuiApp::selected_journal_dir`), never from a hand-built `.forge/` string.
//! That resolver has three outcomes — `.forge/local/sessions` inside a Git
//! repository, the platform application-data directory outside one, or an
//! explicit `journal.path` override — so a scratchpad must follow it to stay
//! correct on `forge resume` in all three. The file sits beside the session's
//! own `<session-id>.db`.
//!
//! # Why a fork starts empty
//!
//! A fork is a new session with a new id, and the file is keyed by that id. The
//! notes describing the parent branch's work are simply not addressed by the
//! child. Carrying them forward would need a copy step and a merge story; v1
//! declines both.
//!
//! # Durability
//!
//! Saves reuse the same atomic write the source viewer uses
//! (`app/files.rs::save_active_editor_with_force`): temp file in the same
//! directory, `write_all`, `sync_all`, then `persist`. Autosave fires on close
//! and on focus loss, so the dirty window is only "since the last `:w`" — and
//! the footer chip says so in words, because colour never travels alone
//! (`FORGE-DESIGN.md` §5.4).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crossterm::event;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::editor_session::EditorSession;
use crate::theme;

/// A loaded scratchpad document plus the path it was loaded from.
pub(crate) struct Scratchpad {
    path: PathBuf,
    editor: EditorSession,
}

impl Scratchpad {
    /// Load the scratchpad at `path`. A missing file is an empty document, not
    /// an error: the first note of a session starts by not existing. Only a
    /// read that fails for another reason is reported, so a permissions problem
    /// never silently looks like a blank page.
    pub(super) fn open(path: PathBuf) -> Result<Self, String> {
        let text = match fs::read(&path) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|_| {
                format!(
                    "{} is not valid UTF-8; move it aside to start a new scratchpad",
                    path.display()
                )
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("could not read {}: {error}", path.display())),
        };
        Ok(Self {
            path,
            editor: EditorSession::new(&text),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn editor(&self) -> &EditorSession {
        &self.editor
    }

    pub(super) fn editor_mut(&mut self) -> &mut EditorSession {
        &mut self.editor
    }

    /// Lines shown in the footer chip. Reported as a *word* beside the count
    /// (`9 lines`), never as a meter.
    pub(super) fn line_count(&self) -> usize {
        self.editor.line_count()
    }

    /// True when the buffer differs from what was last written to disk. This is
    /// exactly the crash window between an edit and the next save.
    pub(super) fn is_dirty(&self) -> bool {
        self.editor.is_dirty()
    }

    /// Atomically write the buffer and mark it accepted, clearing the dirty
    /// state. Preserves an existing file's permissions, as the source viewer
    /// does, so a notes file is not silently reset to a wider mode.
    pub(super) fn save(&mut self) -> Result<(), String> {
        let serialized = self.editor.serialized_text();
        let path = self.path.clone();
        let permissions = fs::metadata(&path).ok().map(|meta| meta.permissions());
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;

        fs::create_dir_all(&parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;

        let mut temporary = tempfile::NamedTempFile::new_in(&parent)
            .map_err(|error| format!("could not create temporary file: {error}"))?;
        temporary
            .write_all(serialized.as_bytes())
            .map_err(|error| format!("could not write scratchpad: {error}"))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| format!("could not flush scratchpad: {error}"))?;
        if let Some(permissions) = permissions {
            fs::set_permissions(temporary.path(), permissions)
                .map_err(|error| format!("could not preserve permissions: {error}"))?;
        }
        temporary
            .persist(&path)
            .map_err(|error| format!("could not save scratchpad: {}", error.error))?;

        self.editor.accept_current_text();
        Ok(())
    }

    /// Save only when there is something to save. Returns whether a write
    /// happened, so callers can report "nothing to save" without touching the
    /// filesystem for a clean document.
    pub(super) fn save_if_dirty(&mut self) -> Result<bool, String> {
        if !self.is_dirty() {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }
}

/// Path of the scratchpad for `session_id`, resolved inside `journal_dir`.
///
/// Naming it after the session's own journal file keeps the two together and
/// makes the pairing obvious in a directory listing.
pub(crate) fn scratchpad_path(journal_dir: &Path, session_id: uuid::Uuid) -> PathBuf {
    journal_dir.join(format!("{session_id}.notes"))
}

impl super::TuiApp {
    /// Path of this session's scratchpad, resolved against the journal
    /// directory rather than a hand-built `.forge/` string so it stays correct
    /// inside a Git repo, outside one, and under a `journal.path` override.
    pub(super) fn scratchpad_path(&self) -> PathBuf {
        scratchpad_path(&self.selected_journal_dir(), self.selected_session_id)
    }

    /// Open the scratchpad, loading whatever is already on disk. Opening an
    /// already-open scratchpad is a no-op rather than a reload, so a stray
    /// `/notes` never discards unsaved edits.
    pub(super) fn open_scratchpad(&mut self) {
        if self.scratchpad.is_some() {
            return;
        }
        let path = self.scratchpad_path();
        match Scratchpad::open(path.clone()) {
            Ok(scratchpad) => {
                let lines = scratchpad.line_count();
                self.scratchpad = Some(scratchpad);
                self.set_feedback(
                    super::FeedbackSeverity::Info,
                    format!("scratchpad {} ({lines} lines)", path.display()),
                );
            }
            Err(error) => self.set_feedback(super::FeedbackSeverity::Warn, error),
        }
    }

    /// Save the scratchpad if it has unsaved edits. Used by `:w` and by the
    /// autosave that fires on close.
    pub(super) fn save_scratchpad(&mut self) {
        let Some(scratchpad) = self.scratchpad.as_mut() else {
            return;
        };
        match scratchpad.save() {
            Ok(()) => self.set_feedback(super::FeedbackSeverity::Ok, "saved scratchpad"),
            Err(error) => self.set_feedback(super::FeedbackSeverity::Warn, error),
        }
    }

    /// Close the scratchpad, autosaving first. Autosave on close is what makes
    /// the remaining dirty window only "since the last `:w`"; a failure to write
    /// keeps the document open so the notes are not dropped on the floor.
    pub(super) fn close_scratchpad(&mut self) {
        let Some(mut scratchpad) = self.scratchpad.take() else {
            return;
        };
        match scratchpad.save_if_dirty() {
            Ok(true) => {
                self.set_feedback(super::FeedbackSeverity::Ok, "saved scratchpad");
            }
            Ok(false) => {}
            Err(error) => {
                // Put it back: losing the buffer because the disk refused the
                // write is exactly the failure this feature must not have.
                self.scratchpad = Some(scratchpad);
                self.set_feedback(super::FeedbackSeverity::Warn, error);
            }
        }
    }

    /// `(lines, dirty)` for the footer chip. The count is cached after the first
    /// read: the footer repaints on every event-loop tick (the throbber alone
    /// guarantees that), so a per-frame disk read here would be a real cost.
    /// While the scratchpad is open the live buffer is authoritative and the
    /// cache is refreshed from it.
    pub(super) fn scratchpad_status(&mut self) -> Option<(usize, bool)> {
        if let Some(scratchpad) = self.scratchpad.as_ref() {
            let status = (scratchpad.line_count(), scratchpad.is_dirty());
            self.scratchpad_summary = Some(status);
            return Some(status);
        }
        if let Some(status) = self.scratchpad_summary {
            return Some(status);
        }
        let path = self.scratchpad_path();
        // A session with no notes file yet reads as an empty, clean document,
        // which is what the chip shows before the first note is written.
        let status = match fs::read_to_string(&path) {
            Ok(text) => (text.lines().count(), false),
            Err(_) => (0, false),
        };
        self.scratchpad_summary = Some(status);
        Some(status)
    }

    /// Own the keyboard while the scratchpad is open. Every key except the
    /// reserved `:` goes to the shared `edtui` buffer, so Vim mode, undo, and
    /// search behave exactly as they do in the source editor.
    ///
    /// `Esc` is deliberately *not* a close. In Vim it leaves Insert mode, and
    /// stealing that would make the editor feel broken; closing is `:q` (which
    /// autosaves), `:q!`, `/notes`, or the footer chip.
    ///
    /// `:` is only a command when the buffer is in Normal mode — in Insert mode
    /// it is an ordinary character and must reach the editor.
    pub(super) fn handle_scratchpad_key(&mut self, key: event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(scratchpad) = self.scratchpad.as_mut() else {
            return;
        };

        let normal_mode = scratchpad.editor().mode() == edtui::EditorMode::Normal;
        if normal_mode && key.code == KeyCode::Char(':') && key.modifiers.is_empty() {
            self.editor_command = Some(String::new());
            self.status_state.message = ":".into();
            return;
        }

        scratchpad.editor_mut().handle_key(key);
    }

    /// `:w` / `:q` for the scratchpad, routed through the same command line the
    /// source editor uses so both surfaces speak one Vim grammar.
    pub(super) fn run_scratchpad_command(&mut self, command: &str) {
        match command {
            "w" | "write" => self.save_scratchpad(),
            "q" | "quit" => self.close_scratchpad(),
            // Discard is explicit about what it throws away; the notes are the
            // operator's, so a bare `:q` never does this.
            "q!" | "quit!" => {
                if let Some(mut scratchpad) = self.scratchpad.take() {
                    scratchpad.editor_mut().accept_current_text();
                }
            }
            other => {
                self.editor_message = Some(format!("not a scratchpad command: {other}"));
            }
        }
    }

    /// Paint the scratchpad over the conversation. Focus is carried by the
    /// accent border plus the `>` title marker, never by a tint alone
    /// (`FORGE-DESIGN.md` §5.4); the unsaved signal is the word `unsaved` in
    /// the warning hue, never colour on its own.
    pub(super) fn render_scratchpad(&mut self, area: Rect, buf: &mut Buffer) {
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Widget};

        let Some(scratchpad) = self.scratchpad.as_mut() else {
            return;
        };
        if area.width < 8 || area.height < 6 {
            return;
        }

        let panel = crate::overlay_layout::centered_rect(78, 78, area);
        // The resolved path is shown so the operator can see this really does
        // follow the journal directory (and therefore survives a resume),
        // rather than being a hidden `.forge/` guess.
        let location = scratchpad
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "notes".to_string());
        let title = Line::from(vec![
            Span::styled("> ", theme::accent_style()),
            Span::styled(
                "Scratchpad",
                theme::accent_style().add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(
                format!("  {location}"),
                ratatui::style::Style::default().fg(theme::text_dim_color()),
            ),
        ]);
        let block = Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(theme::accent_style())
            .title(title);

        let inner = block.inner(panel);
        ratatui::widgets::Clear.render(panel, buf);
        theme::fill(panel, buf, theme::panel());
        block.render(panel, buf);

        // Leave the last row for the status line the editor itself reports.
        let editor_area = Rect {
            height: inner.height.saturating_sub(1),
            ..inner
        };
        scratchpad.editor_mut().render(editor_area, buf);

        let dirty = scratchpad.is_dirty();
        let lines = scratchpad.line_count();
        let mode = if scratchpad.editor().mode() == edtui::EditorMode::Insert {
            "-- INSERT --"
        } else {
            "-- NORMAL --"
        };
        let mut status = vec![
            Span::styled(
                mode,
                ratatui::style::Style::default().fg(theme::text_dim_color()),
            ),
            Span::raw(" "),
            Span::styled(
                format!("{lines} lines"),
                ratatui::style::Style::default().fg(theme::text_secondary_color()),
            ),
        ];
        if dirty {
            status.push(Span::raw("  "));
            // Glyph plus word: colour never travels alone.
            status.push(Span::styled(
                "● unsaved",
                ratatui::style::Style::default()
                    .fg(theme::warning_color())
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        status.push(Span::raw("  "));
        status.push(Span::styled(
            ":w save · ⎋ close · i insert",
            ratatui::style::Style::default().fg(theme::text_dim_color()),
        ));
        if let Some(row) = editor_area.height.checked_add(inner.y) {
            buf.set_line(inner.x, row, &Line::from(status), inner.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_id() -> uuid::Uuid {
        uuid::Uuid::from_u128(0x9f2c_0000_0000_4000_8000_0000_0000_0001)
    }

    /// Type `text` the way the overlay receives it: `i` enters Insert mode, then
    /// each character arrives as a key event. `replace_text` is deliberately not
    /// used to dirty a buffer — it marks its input accepted, so it cannot stand
    /// in for an operator edit.
    fn type_text(editor: &mut EditorSession, text: &str) {
        use crossterm::event::{KeyCode, KeyEvent};
        editor.handle_key(KeyEvent::from(KeyCode::Char('i')));
        for character in text.chars() {
            let code = match character {
                '\n' => KeyCode::Enter,
                other => KeyCode::Char(other),
            };
            editor.handle_key(KeyEvent::from(code));
        }
    }

    /// A session id that is not a valid filename on any supported platform is
    /// still a valid one here, because the id is uuid-formatted text.
    #[test]
    fn path_sits_beside_the_session_journal_and_carries_its_own_suffix() {
        let dir = Path::new("/tmp/forge-sessions");
        let path = scratchpad_path(dir, session_id());
        assert_eq!(path.parent(), Some(dir));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("9f2c0000-0000-4000-8000-000000000001.notes")
        );
    }

    /// The whole point of the scratchpad: a session that has never been written
    /// to opens as an empty, clean document instead of failing.
    #[test]
    fn a_missing_file_opens_as_an_empty_clean_document() {
        let path = std::env::temp_dir().join("forge-scratchpad-absent-test.notes");
        let _ = fs::remove_file(&path);
        let scratchpad = Scratchpad::open(path.clone()).expect("missing file is not an error");
        assert!(!scratchpad.is_dirty());
        // `Lines::from("")` is genuinely empty, so the footer chip reads
        // "0 lines" rather than claiming a blank row exists.
        assert_eq!(scratchpad.line_count(), 0);
        assert_eq!(scratchpad.editor().text(), "");
    }

    #[test]
    fn saving_persists_the_buffer_and_clears_the_dirty_state() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("s.notes");
        let mut scratchpad = Scratchpad::open(path.clone()).expect("open");

        type_text(scratchpad.editor_mut(), "first line\nsecond line");
        assert!(scratchpad.is_dirty());

        scratchpad.save().expect("save writes");

        assert!(!scratchpad.is_dirty(), "a save accepts the current text");
        let on_disk = fs::read_to_string(&path).expect("notes file exists after save");
        assert!(
            on_disk.starts_with("first line\nsecond line"),
            "buffer reached disk, got {on_disk:?}"
        );
    }

    /// Autosave on close must not touch the disk for a document nobody edited.
    #[test]
    fn saving_a_clean_document_is_a_no_op_that_creates_no_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("untouched.notes");
        let mut scratchpad = Scratchpad::open(path.clone()).expect("open");

        let wrote = scratchpad.save_if_dirty().expect("clean save cannot fail");

        assert!(!wrote);
        assert!(
            !path.exists(),
            "no file for a document that was never edited"
        );
    }

    #[test]
    fn reopening_a_saved_scratchpad_restores_the_text() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("resume.notes");
        let mut scratchpad = Scratchpad::open(path.clone()).expect("open");
        type_text(scratchpad.editor_mut(), "carried across resume\n");
        assert!(scratchpad.is_dirty());
        scratchpad.save().expect("save");

        // This is the `forge resume` path: a brand-new session object, same file.
        let reopened = Scratchpad::open(path).expect("reopen");
        assert_eq!(reopened.editor().text(), "carried across resume\n");
        assert!(!reopened.is_dirty(), "a freshly loaded document is clean");
    }

    /// Saving must never lose notes because the parent directory was removed
    /// between open and save.
    #[test]
    fn save_creates_a_missing_parent_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join("deep").join("s.notes");
        let mut scratchpad = Scratchpad::open(path.clone()).expect("open");
        type_text(scratchpad.editor_mut(), "still here\n");

        scratchpad.save().expect("save creates parents");

        assert_eq!(
            fs::read_to_string(&path).expect("nested notes file"),
            "still here\n"
        );
    }

    /// Non-UTF-8 content is a real operator problem, not an empty scratchpad —
    /// reporting it beats silently handing back a blank document.
    #[test]
    fn invalid_utf8_is_reported_rather_than_shown_as_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("binary.notes");
        fs::write(&path, [0xFF, 0xFE, 0x00]).expect("write bytes");

        let error = match Scratchpad::open(path) {
            Err(error) => error,
            Ok(_) => panic!("binary notes must not open as an empty document"),
        };

        assert!(
            error.contains("not valid UTF-8"),
            "error names the real problem, got {error:?}"
        );
    }
}
