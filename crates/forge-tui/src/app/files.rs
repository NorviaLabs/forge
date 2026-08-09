//! File explorer, viewer and filesystem operations for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. Opening the explorer and the source viewer,
//! the create/rename/delete confirmation dialogs, and the reconciliation that
//! keeps open views consistent after a path changes on disk. Methods are moved
//! verbatim.

use super::*;
use std::io::Write;

use super::util::{rebase_path, relative_display};
use crate::source_viewer::ViewerStatus;

impl TuiApp {
    pub(super) fn save_active_editor(&mut self) {
        self.save_active_editor_with_force(false);
    }

    pub(super) fn save_active_editor_with_force(&mut self, force: bool) {
        let Some(path) = self.source_viewer.path.clone() else {
            self.set_feedback(FeedbackSeverity::Warn, "No file open to save");
            return;
        };
        let Some(editor) = self.editor_session.as_ref() else {
            self.set_feedback(FeedbackSeverity::Warn, "The active file is read-only");
            return;
        };
        if !force {
            let baseline = editor.accepted_serialized_text();
            match self
                .source_viewer
                .disk_conflicts_with(&path, baseline.as_bytes())
            {
                Ok(true) => {
                    self.explorer_dialog.current = Some(ExplorerDialog::SaveConflict);
                    return;
                }
                Err(error) => {
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        format!("could not check disk before save: {error}"),
                    );
                    return;
                }
                Ok(false) => {}
            }
        }
        let serialized = editor.serialized_text();
        let permissions = fs::metadata(&path).ok().map(|meta| meta.permissions());
        let parent = path.parent().unwrap_or_else(|| Path::new("."));

        let result = (|| -> Result<(), String> {
            let mut temporary = tempfile::NamedTempFile::new_in(parent)
                .map_err(|error| format!("could not create temporary save file: {error}"))?;
            temporary
                .write_all(serialized.as_bytes())
                .map_err(|error| format!("could not write file: {error}"))?;
            temporary
                .as_file()
                .sync_all()
                .map_err(|error| format!("could not flush file: {error}"))?;
            if let Some(permissions) = permissions {
                fs::set_permissions(temporary.path(), permissions)
                    .map_err(|error| format!("could not preserve permissions: {error}"))?;
            }
            temporary
                .persist(&path)
                .map_err(|error| format!("could not replace file: {}", error.error))?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                if let Some(editor) = self.editor_session.as_mut() {
                    editor.accept_current_text();
                }
                self.source_viewer.refresh(self.session.workspace_root());
                self.workspace_files.explorer.refresh_git_status();
                self.set_feedback(FeedbackSeverity::Ok, format!("saved {}", path.display()));
            }
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Warn, error);
            }
        }
    }

    pub(super) fn reload_active_editor_from_disk(&mut self) {
        let Some(path) = self.source_viewer.path.clone() else {
            return;
        };
        let Ok(bytes) = fs::read(&path) else {
            self.set_feedback(FeedbackSeverity::Warn, "could not reload file from disk");
            return;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            self.set_feedback(FeedbackSeverity::Warn, "file is no longer valid UTF-8");
            return;
        };
        if let Some(editor) = self.editor_session.as_mut() {
            editor.replace_text(&text);
        }
        self.source_viewer.refresh(self.session.workspace_root());
        self.set_feedback(FeedbackSeverity::Info, "reloaded file from disk");
    }

    pub(super) fn reconcile_open_file_external_rename(&mut self) -> bool {
        let Some(open_path) = self.source_viewer.path.clone() else {
            return false;
        };
        if open_path.exists() {
            return false;
        }
        let Some(parent) = open_path.parent() else {
            return false;
        };
        let Ok(entries) = fs::read_dir(parent) else {
            return false;
        };
        let root = self.session.workspace_root().to_path_buf();
        let old_line = self.source_viewer.current_line;
        let old_top = self.source_viewer.top_line;
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate == open_path || !candidate.is_file() {
                continue;
            }
            if self
                .source_viewer
                .reconcile_external_rename_if_same_identity(&root, &candidate)
            {
                let workspace_path = candidate.canonicalize().unwrap_or(candidate);
                self.source_viewer.current_line =
                    old_line.min(self.source_viewer.lines.len().saturating_sub(1));
                self.source_viewer.top_line =
                    old_top.min(self.source_viewer.lines.len().saturating_sub(1));
                self.workspace_navigation
                    .replace_view(WorkspaceView::File(workspace_path));
                self.set_feedback(FeedbackSeverity::Info, "Open file was renamed externally");
                return true;
            }
        }
        false
    }

    pub(super) fn resolve_workspace_path(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<PathBuf, TuiError> {
        let input = path.as_ref();
        let joined = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.runtime.cwd.join(input)
        };
        let canonical = joined.canonicalize()?;
        let root = self.runtime.cwd.canonicalize()?;
        if !canonical.starts_with(&root) {
            return Err(TuiError::Other(
                "file explorer is limited to the workspace".into(),
            ));
        }
        Ok(canonical)
    }

    pub(super) fn open_file_explorer(&mut self, path: Option<&str>, error: Option<String>) {
        let dir = match path {
            Some(path) if !path.trim().is_empty() => self
                .resolve_workspace_path(path.trim())
                .unwrap_or_else(|_| self.runtime.cwd.clone()),
            _ => self.runtime.cwd.clone(),
        };
        let dir = if dir.is_dir() {
            dir
        } else {
            dir.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.runtime.cwd.clone())
        };
        let root = self
            .runtime
            .cwd
            .canonicalize()
            .unwrap_or_else(|_| self.runtime.cwd.clone());
        let current = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        let mut items = Vec::new();
        if current != root {
            if let Some(parent) = dir.parent() {
                items.push(FileExplorerItem {
                    name: "..".into(),
                    path: parent.display().to_string(),
                    is_dir: true,
                });
            }
        }
        match fs::read_dir(&dir) {
            Ok(entries) => {
                let mut children = entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        let path = entry.path();
                        let file_type = entry.file_type().ok()?;
                        let is_dir = file_type.is_dir();
                        if !is_dir && !file_type.is_file() {
                            return None;
                        }
                        Some(FileExplorerItem {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            path: path.display().to_string(),
                            is_dir,
                        })
                    })
                    .collect::<Vec<_>>();
                children.sort_by(|left, right| {
                    right.is_dir.cmp(&left.is_dir).then_with(|| {
                        left.name
                            .to_ascii_lowercase()
                            .cmp(&right.name.to_ascii_lowercase())
                    })
                });
                items.extend(children);
            }
            Err(err) => {
                self.overlay = Some(Overlay::file_explorer(
                    dir.display().to_string(),
                    items,
                    Some(format!("Could not read directory: {err}")),
                ));
                return;
            }
        }
        self.overlay = Some(Overlay::file_explorer(
            dir.display().to_string(),
            items,
            error,
        ));
        self.status_state.message = "File explorer (readonly)".into();
        self.notice_state.items.clear();
    }

    pub(super) fn open_file_viewer(&mut self, path: &str) {
        match self.resolve_workspace_path(path).and_then(|path| {
            if !path.is_file() {
                return Err(TuiError::Other("selected path is not a file".into()));
            }
            let contents = String::from_utf8_lossy(&fs::read(&path)?).into_owned();
            Ok((path, contents))
        }) {
            Ok((path, contents)) => {
                self.overlay = Some(Overlay::file_viewer(path.display().to_string(), contents));
                self.status_state.message = "Viewing file (readonly)".into();
            }
            Err(err) => self.open_file_explorer(None, Some(format!("Could not open file: {err}"))),
        }
    }

    pub(super) fn show_file_in_editor(&mut self, path: &Path) {
        let root = self.session.workspace_root().to_path_buf();
        self.source_viewer.open(&root, path);
        self.editor_session = self
            .source_viewer
            .document_text
            .as_deref()
            .filter(|_| self.source_viewer.status == ViewerStatus::Ok)
            .map(EditorSession::new);
        if let Some(editor) = self.editor_session.as_mut() {
            editor.set_syntax_language(self.source_viewer.language_label.as_deref());
            editor.set_syntax_theme(crate::theme::syntax_theme());
        }
        self.focus_block(FocusBlock::Workspace);
        self.status_state.message = if self.editor_session.is_some() {
            "Editing file · NORMAL mode".into()
        } else {
            "Viewing file (readonly)".into()
        };
        // Keep the file explorer in sync with the active file.
        self.workspace_files.explorer.selected_path = Some(path.to_path_buf());
    }

    pub(super) fn open_file_in_editor(&mut self, path: &Path) {
        let same_path = self.source_viewer.path.as_deref() == Some(path)
            || self
                .source_viewer
                .path
                .as_ref()
                .and_then(|current| current.canonicalize().ok())
                .zip(path.canonicalize().ok())
                .is_some_and(|(current, requested)| current == requested);
        if same_path
            && self
                .editor_session
                .as_ref()
                .is_some_and(|editor| editor.is_dirty())
        {
            self.focus_block(FocusBlock::Workspace);
            self.set_feedback(
                FeedbackSeverity::Info,
                "Unsaved editor changes kept; use :e to reload explicitly",
            );
            return;
        }
        if !same_path
            && self
                .editor_session
                .as_ref()
                .is_some_and(|editor| editor.is_dirty())
        {
            self.pending_editor_path = Some(path.to_path_buf());
            self.explorer_dialog.current = Some(ExplorerDialog::DirtySwitch {
                path: path.to_path_buf(),
            });
            return;
        }
        self.navigate_to_workspace_view(WorkspaceView::File(path.to_path_buf()));
        self.note_workspace_file_opened(path);
    }

    pub(super) fn complete_pending_editor_switch(&mut self, discard: bool) {
        let Some(path) = self.pending_editor_path.take() else {
            return;
        };
        if discard {
            if let Some(editor) = self.editor_session.as_mut() {
                editor.accept_current_text();
            }
        }
        self.navigate_to_workspace_view(WorkspaceView::File(path.clone()));
        self.note_workspace_file_opened(&path);
    }

    #[cfg(test)]
    pub(crate) fn open_file_view_for_test(&mut self, path: &Path) {
        self.open_file_in_editor(path);
    }

    #[cfg(test)]
    pub(crate) fn review_changes_for_test(&mut self) {
        self.navigate_to_workspace_view(WorkspaceView::Diff(DiffCommandContext::Current));
    }

    fn file_ops(&self) -> Result<WorkspaceFileOps, FileOperationError> {
        WorkspaceFileOps::new(self.session.workspace_root())
    }

    pub(super) fn open_explorer_name_dialog(&mut self, action: ExplorerNameAction) {
        let Some(parent) = self.workspace_files.explorer.selected_creation_parent() else {
            self.set_feedback(FeedbackSeverity::Warn, "No workspace folder selected");
            return;
        };
        let (source, input) = if action == ExplorerNameAction::Rename {
            let Some(node) = self.workspace_files.explorer.selected_node() else {
                self.set_feedback(FeedbackSeverity::Warn, "No file or folder selected");
                return;
            };
            if self.workspace_files.explorer.root_path() == Some(node.path.as_path()) {
                self.set_feedback(FeedbackSeverity::Warn, "Cannot rename the workspace root");
                return;
            }
            (Some(node.path.clone()), node.display_name.clone())
        } else {
            (None, String::new())
        };
        self.explorer_dialog.current = Some(ExplorerDialog::Name {
            action,
            parent,
            source,
            input,
            error: None,
        });
        self.focus_block(FocusBlock::Files);
    }

    pub(super) fn open_explorer_delete_dialog(&mut self) {
        let Some(node) = self.workspace_files.explorer.selected_node() else {
            self.set_feedback(FeedbackSeverity::Warn, "No file or folder selected");
            return;
        };
        if self.workspace_files.explorer.root_path() == Some(node.path.as_path()) {
            self.set_feedback(FeedbackSeverity::Warn, "Cannot delete the workspace root");
            return;
        }
        let ops = match self.file_ops() {
            Ok(ops) => ops,
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Error, error.actionable());
                return;
            }
        };
        let kind = match ops.entry_kind(&node.path) {
            Ok(kind) => kind,
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Error, error.actionable());
                return;
            }
        };
        let non_empty = match ops.is_non_empty_directory(&node.path) {
            Ok(non_empty) => non_empty,
            Err(error) => {
                self.set_feedback(FeedbackSeverity::Error, error.actionable());
                return;
            }
        };
        self.explorer_dialog.current = Some(ExplorerDialog::ConfirmDelete {
            source: node.path.clone(),
            name: node.display_name.clone(),
            kind,
            non_empty,
            permanent: false,
            error: None,
        });
        self.focus_block(FocusBlock::Files);
    }

    pub(super) fn prepare_explorer_name_operation(
        &self,
        action: ExplorerNameAction,
        parent: &Path,
        source: Option<&Path>,
        input: &str,
    ) -> Result<ExplorerDialog, FileOperationError> {
        let ops = self.file_ops()?;
        match action {
            ExplorerNameAction::CreateFile | ExplorerNameAction::CreateDirectory => {
                let path = ops.plan_create(parent, input)?;
                Ok(ExplorerDialog::ConfirmCreate {
                    action,
                    parent: parent.to_path_buf(),
                    name: input.trim().to_string(),
                    path,
                })
            }
            ExplorerNameAction::Rename => {
                let source = source.ok_or(FileOperationError::MissingSource)?;
                let path = ops.plan_rename(source, input)?;
                Ok(ExplorerDialog::ConfirmRename {
                    source: source.to_path_buf(),
                    path,
                    name: input.trim().to_string(),
                })
            }
        }
    }

    pub(super) fn apply_confirmed_create(
        &mut self,
        action: ExplorerNameAction,
        parent: &Path,
        name: &str,
    ) {
        let result = match self.file_ops() {
            Ok(ops) if action == ExplorerNameAction::CreateFile => ops.create_file(parent, name),
            Ok(ops) => ops.create_directory(parent, name),
            Err(error) => Err(error),
        };
        match result {
            Ok(result) => self.reconcile_file_operation(result),
            Err(error) => self.set_feedback(FeedbackSeverity::Error, error.actionable()),
        }
    }

    pub(super) fn apply_confirmed_rename(&mut self, source: &Path, name: &str) {
        let result = self
            .file_ops()
            .and_then(|ops| ops.rename_entry(source, name));
        match result {
            Ok(result) => self.reconcile_file_operation(result),
            Err(error) => self.set_feedback(FeedbackSeverity::Error, error.actionable()),
        }
    }

    pub(super) fn apply_confirmed_delete(&mut self, source: &Path, mode: DeleteMode) {
        let result = self
            .file_ops()
            .and_then(|ops| ops.delete_entry(source, mode));
        match result {
            Ok(result) => self.reconcile_file_operation(result),
            Err(FileOperationError::TrashUnavailable(reason)) if mode == DeleteMode::Trash => {
                if let Some(node) = self.workspace_files.explorer.selected_node() {
                    let kind = self
                        .file_ops()
                        .and_then(|ops| ops.entry_kind(&node.path))
                        .unwrap_or(EntryKind::Other);
                    let non_empty = self
                        .file_ops()
                        .and_then(|ops| ops.is_non_empty_directory(&node.path))
                        .unwrap_or(false);
                    self.explorer_dialog.current = Some(ExplorerDialog::ConfirmDelete {
                        source: node.path.clone(),
                        name: node.display_name.clone(),
                        kind,
                        non_empty,
                        permanent: false,
                        error: Some(FileOperationError::TrashUnavailable(reason).actionable()),
                    });
                } else {
                    self.set_feedback(FeedbackSeverity::Error, "Trash is unavailable");
                }
            }
            Err(error) => self.set_feedback(FeedbackSeverity::Error, error.actionable()),
        }
    }

    fn reconcile_file_operation(&mut self, result: crate::file_ops::FileOperationResult) {
        let root = self.session.workspace_root().to_path_buf();
        match result.kind {
            FileOperationKind::CreateFile | FileOperationKind::CreateDirectory => {
                self.workspace_files
                    .explorer
                    .refresh_parent_and_select(&result.parent, &result.path);
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!("Created {}", relative_display(&root, &result.path)),
                );
                if result.kind == FileOperationKind::CreateFile {
                    self.open_file_in_editor(&result.path);
                }
            }
            FileOperationKind::RenameEntry => {
                if let Some(new_path) = result.new_path.as_ref() {
                    self.reconcile_path_rename(&result.path, new_path);
                    self.workspace_files
                        .explorer
                        .refresh_parent_and_select(&result.parent, new_path);
                    self.set_feedback(
                        FeedbackSeverity::Ok,
                        format!("Renamed to {}", relative_display(&root, new_path)),
                    );
                }
            }
            FileOperationKind::DeleteEntry => {
                self.reconcile_path_delete(&result.path);
                self.workspace_files
                    .explorer
                    .refresh_after_delete(&result.parent, &result.path);
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!("Removed {}", relative_display(&root, &result.path)),
                );
            }
        }
        self.diff_view.selected = self.diff_view.selected.min(
            self.workspace_files
                .explorer
                .git_status
                .changed_files()
                .len()
                .saturating_sub(1),
        );
    }

    fn reconcile_path_rename(&mut self, old_path: &Path, new_path: &Path) {
        let root = self.session.workspace_root().to_path_buf();
        let mut renamed_open_file = false;
        if let Some(open_path) = self.source_viewer.path.clone() {
            if let Some(rebased) = rebase_path(&open_path, old_path, new_path) {
                renamed_open_file = true;
                self.source_viewer
                    .reconcile_renamed_path(&root, &open_path, &rebased);
            }
        }
        if let Some(att) = self.attachment.pending.as_mut() {
            let abs = root.join(&att.rel_path);
            if let Some(rebased) = rebase_path(&abs, old_path, new_path) {
                att.rel_path = relative_display(&root, &rebased);
            }
        }
        if renamed_open_file {
            self.focus_block(FocusBlock::Workspace);
        }
        self.workspace_files.explorer.refresh_git_status();
    }

    fn reconcile_path_delete(&mut self, deleted_path: &Path) {
        let root = self.session.workspace_root().to_path_buf();
        if let Some(open_path) = self.source_viewer.path.clone() {
            if open_path == deleted_path || open_path.starts_with(deleted_path) {
                self.source_viewer.reconcile_deleted_path(&open_path);
            }
        }
        if self.attachment.pending.as_ref().is_some_and(|att| {
            let abs = root.join(&att.rel_path);
            abs == deleted_path || abs.starts_with(deleted_path)
        }) {
            self.attachment.pending = None;
        }
        self.workspace_files.explorer.refresh_git_status();
    }

    /// Toggle attachment of the current source-viewer file to the next message.
    pub(super) fn toggle_file_attachment(&mut self) {
        if self.attachment.pending.is_some() {
            self.attachment.pending = None;
            self.set_feedback(FeedbackSeverity::Info, "Attachment removed");
            return;
        }

        let path = match &self.source_viewer.path {
            Some(p) if self.source_viewer.status.is_openable() => p.clone(),
            _ => {
                self.set_feedback(FeedbackSeverity::Warn, "No openable file to attach");
                return;
            }
        };

        let root = self.session.workspace_root();
        let rel_path = match path.strip_prefix(root) {
            Ok(rel) => rel.display().to_string(),
            Err(_) => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "Active file is outside the repository",
                );
                return;
            }
        };

        let cursor_line = self.source_viewer.current_line;
        self.attachment.pending = Some(crate::file_context::FileAttachment::new(
            rel_path,
            cursor_line,
        ));
        if let Some(ref att) = self.attachment.pending {
            self.set_feedback(
                FeedbackSeverity::Info,
                format!("File attached · {}", att.label()),
            );
        }
    }

    pub(super) fn refresh_active_source_viewer(&mut self) {
        let root = self.session.workspace_root().to_path_buf();
        let path = self.source_viewer.path.clone();
        let old_line = self.source_viewer.current_line;
        let old_top = self.source_viewer.top_line;

        if let Some(p) = &path {
            if p.exists() {
                self.source_viewer.refresh(&root);
                // Preserve sensible cursor.
                self.source_viewer.current_line =
                    old_line.min(self.source_viewer.lines.len().saturating_sub(1));
                self.source_viewer.top_line =
                    old_top.min(self.source_viewer.lines.len().saturating_sub(1));
            } else {
                self.source_viewer.refresh(&root);
            }
        }

        // Invalidate search matches (recomputed lazily).
        let search_query = self.source_viewer.search.query.clone();
        if !search_query.is_empty() {
            self.source_viewer.update_search_query(&search_query);
        }
    }
    pub(super) fn render_explorer_dialog(
        &self,
        dialog: &ExplorerDialog,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let r = centered_rect(64, 34, area);
        crate::theme::fill(r, buf, crate::theme::panel());
        let mut lines = Vec::new();
        let (title, border) = match dialog {
            ExplorerDialog::Name { action, .. } => (
                match action {
                    ExplorerNameAction::CreateFile => " New File ",
                    ExplorerNameAction::CreateDirectory => " New Folder ",
                    ExplorerNameAction::Rename => " Rename ",
                },
                theme::brand(),
            ),
            ExplorerDialog::ConfirmDelete { permanent, .. } if *permanent => {
                (" Permanent Delete ", theme::danger())
            }
            ExplorerDialog::ConfirmDelete { .. } => (" Delete ", theme::warn()),
            ExplorerDialog::ConfirmCreate { .. } => (" Confirm Create ", theme::warn()),
            ExplorerDialog::ConfirmRename { .. } => (" Confirm Rename ", theme::warn()),
            ExplorerDialog::DirtyExit => (" Unsaved Changes ", theme::warn()),
            ExplorerDialog::DirtySwitch { .. } => (" Unsaved Changes ", theme::warn()),
            ExplorerDialog::SaveConflict => (" File Changed on Disk ", theme::warn()),
        };
        match dialog {
            ExplorerDialog::Name {
                action,
                parent,
                input,
                error,
                ..
            } => {
                let label = match action {
                    ExplorerNameAction::CreateFile => "Enter one file name:",
                    ExplorerNameAction::CreateDirectory => "Enter one folder name:",
                    ExplorerNameAction::Rename => "Enter the new name:",
                };
                lines.push(Line::styled(label, theme::text()));
                lines.push(Line::styled(
                    format!(
                        "Parent: {}",
                        relative_display(self.session.workspace_root(), parent)
                    ),
                    theme::muted(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled(format!("> {input}"), theme::text()));
                if let Some(error) = error {
                    lines.push(Line::from(""));
                    lines.push(Line::styled(error.clone(), theme::danger()));
                }
                lines.push(Line::from(""));
                lines.push(Line::styled("Enter confirm · Esc cancel", theme::muted()));
            }
            ExplorerDialog::ConfirmCreate { action, path, .. } => {
                let what = if *action == ExplorerNameAction::CreateDirectory {
                    "folder"
                } else {
                    "file"
                };
                lines.push(Line::styled(
                    format!(
                        "Create {what} \"{}\"?",
                        relative_display(self.session.workspace_root(), path)
                    ),
                    theme::text(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled("Enter/y confirm · Esc cancel", theme::muted()));
            }
            ExplorerDialog::ConfirmRename { source, path, .. } => {
                lines.push(Line::styled(
                    format!(
                        "Rename \"{}\"?",
                        relative_display(self.session.workspace_root(), source)
                    ),
                    theme::text(),
                ));
                lines.push(Line::styled(
                    format!(
                        "To \"{}\"",
                        relative_display(self.session.workspace_root(), path)
                    ),
                    theme::text(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled("Enter/y confirm · Esc cancel", theme::muted()));
            }
            ExplorerDialog::ConfirmDelete {
                name,
                kind,
                non_empty,
                permanent,
                error,
                ..
            } => {
                if let Some(error) = error {
                    lines.push(Line::styled(error.clone(), theme::danger()));
                    lines.push(Line::from(""));
                    lines.push(Line::styled(
                        "Press p to choose explicit permanent delete · Esc cancel",
                        theme::muted(),
                    ));
                } else if *permanent {
                    lines.push(Line::styled(
                        format!("Permanently delete \"{name}\"?"),
                        theme::danger(),
                    ));
                    lines.push(Line::styled(
                        "This cannot be undone by Forge.",
                        theme::danger(),
                    ));
                    lines.push(Line::from(""));
                    lines.push(Line::styled(
                        "Press D to permanently delete · Esc cancel",
                        theme::muted(),
                    ));
                } else {
                    let copy = match (kind, non_empty) {
                        (EntryKind::Directory, true) => {
                            format!("Move folder \"{name}\" and its contents to Trash?")
                        }
                        (EntryKind::Directory, false) => {
                            format!("Move folder \"{name}\" to Trash?")
                        }
                        _ => format!("Move \"{name}\" to Trash?"),
                    };
                    lines.push(Line::styled(copy, theme::text()));
                    lines.push(Line::from(""));
                    if *non_empty {
                        lines.push(Line::styled(
                            "Press D to confirm · Esc cancel",
                            theme::muted(),
                        ));
                    } else {
                        lines.push(Line::styled("Enter/y confirm · Esc cancel", theme::muted()));
                    }
                }
            }
            ExplorerDialog::DirtyExit => {
                lines.push(Line::styled(
                    "The current file has unsaved changes.",
                    theme::text(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    "Save and leave?  Enter/s save · d discard · Esc cancel",
                    theme::muted(),
                ));
            }
            ExplorerDialog::DirtySwitch { path } => {
                lines.push(Line::styled(
                    "The current file has unsaved changes.",
                    theme::text(),
                ));
                lines.push(Line::styled(
                    format!(
                        "Open {} after resolving them?",
                        relative_display(self.session.workspace_root(), path)
                    ),
                    theme::muted(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    "Save and switch?  Enter/s save · d discard · Esc cancel",
                    theme::muted(),
                ));
            }
            ExplorerDialog::SaveConflict => {
                lines.push(Line::styled(
                    "The file changed outside Forge while you were editing.",
                    theme::text(),
                ));
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    "Reload disk?  r reload · f force save · Esc cancel",
                    theme::muted(),
                ));
            }
        }
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border)
                    .style(theme::panel())
                    .title(Span::styled(title, border)),
            )
            .render(r, buf);
    }
}
