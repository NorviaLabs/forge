//! Workspace search index and Quick Open.

use forge_search::{SearchError, WorkspaceIndex};
use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::overlays::{Overlay, QuickOpenItem};

impl TuiApp {
    pub(super) fn open_quick_open(&mut self) {
        self.overlay = Some(Overlay::quick_open());
        self.refresh_quick_open_results();
        self.set_feedback(
            FeedbackSeverity::Info,
            "Quick Open · type to filter · ↑↓ navigate · Enter open · Esc close",
        );
    }

    pub(super) fn refresh_quick_open_results(&mut self) {
        let current_file = self.current_workspace_file().map(Path::to_path_buf);
        let index = match self.workspace_search_index() {
            Ok(index) => index,
            Err(err) => {
                if let Some(Overlay::QuickOpen {
                    error,
                    hits,
                    selected,
                    ..
                }) = self.overlay.as_mut()
                {
                    *error = Some(err.to_string());
                    hits.clear();
                    *selected = 0;
                }
                return;
            }
        };

        let Some(Overlay::QuickOpen {
            query,
            selected,
            hits,
            error,
        }) = self.overlay.as_mut()
        else {
            return;
        };

        *error = None;
        match index.find_files_quick_open(query, 50, current_file.as_deref()) {
            Ok(response) => {
                *hits = response
                    .hits
                    .into_iter()
                    .map(|hit| QuickOpenItem {
                        path: hit.path,
                        score: hit.score,
                        match_ranges: hit.match_ranges,
                    })
                    .collect();
                if hits.is_empty() {
                    *selected = 0;
                } else if *selected >= hits.len() {
                    *selected = hits.len() - 1;
                }
            }
            Err(err) => {
                *error = Some(err.to_string());
                hits.clear();
                *selected = 0;
            }
        }
    }

    fn workspace_search_index(&mut self) -> Result<Arc<WorkspaceIndex>, SearchError> {
        if let Some(index) = &self.workspace_search.index {
            return Ok(index.clone());
        }
        if let Some(message) = &self.workspace_search.error {
            return Err(SearchError::Init(message.clone()));
        }

        match WorkspaceIndex::open(self.session.workspace_root()) {
            Ok(index) => {
                self.workspace_search.index = Some(index.clone());
                Ok(index)
            }
            Err(err) => {
                self.workspace_search.error = Some(err.to_string());
                Err(err)
            }
        }
    }

    fn current_workspace_file(&self) -> Option<&Path> {
        match &self.workspace_navigation.current {
            WorkspaceView::File(path) => Some(path.as_path()),
            _ => None,
        }
    }

    pub(super) fn note_workspace_file_opened(&mut self, path: &Path) {
        if let Ok(index) = self.workspace_search_index() {
            let _ = index.note_file_opened(path);
        }
    }
}
