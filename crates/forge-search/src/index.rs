use crate::types::{FileSearchHit, FindResponse, GrepQueryMode, GrepResponse, GrepSearchHit};
use fff_search::{
    file_picker::{FFFMode, FilePicker, FilePickerOptions, FuzzySearchOptions},
    grep::{parse_grep_query, GrepMode, GrepSearchOptions},
    PaginationArgs, QueryParser, SharedFilePicker, SharedFrecency,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CONTEXT_LINES: usize = 1;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("search init failed: {0}")]
    Init(String),
    #[error("workspace scan timed out")]
    ScanTimeout,
    #[error("search lock error: {0}")]
    Lock(String),
}

/// Options for opening a long-lived workspace index.
#[derive(Debug, Clone)]
pub struct WorkspaceIndexOptions {
    pub watch: bool,
    pub scan_timeout: Duration,
}

impl Default for WorkspaceIndexOptions {
    fn default() -> Self {
        Self {
            watch: true,
            scan_timeout: DEFAULT_SCAN_TIMEOUT,
        }
    }
}

/// Shared, incrementally updated workspace file index backed by `fff-search`.
#[derive(Debug)]
pub struct WorkspaceIndex {
    root: PathBuf,
    shared_picker: SharedFilePicker,
    shared_frecency: SharedFrecency,
    scan_timeout: Duration,
}

impl WorkspaceIndex {
    pub fn open(root: impl AsRef<Path>) -> Result<Arc<Self>, SearchError> {
        Self::open_with_options(root, WorkspaceIndexOptions::default())
    }

    pub fn open_with_options(
        root: impl AsRef<Path>,
        options: WorkspaceIndexOptions,
    ) -> Result<Arc<Self>, SearchError> {
        let root = root.as_ref().to_path_buf();
        let shared_picker = SharedFilePicker::default();
        let shared_frecency = SharedFrecency::default();
        FilePicker::new_with_shared_state(
            shared_picker.clone(),
            shared_frecency.clone(),
            FilePickerOptions {
                base_path: root.display().to_string(),
                mode: FFFMode::Ai,
                watch: options.watch,
                ..Default::default()
            },
        )
        .map_err(|e| SearchError::Init(e.to_string()))?;

        let index = Arc::new(Self {
            root,
            shared_picker,
            shared_frecency,
            scan_timeout: options.scan_timeout,
        });
        index.wait_for_scan()?;
        Ok(index)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub fn wait_for_scan(&self) -> Result<(), SearchError> {
        if self.shared_picker.wait_for_scan(self.scan_timeout) {
            Ok(())
        } else {
            Err(SearchError::ScanTimeout)
        }
    }

    /// Fuzzy filename search ranked by score, frecency, and optional project context.
    pub fn find_files(
        &self,
        query: &str,
        max_results: usize,
        current_file: Option<&Path>,
    ) -> Result<FindResponse, SearchError> {
        if max_results == 0 {
            return Ok(FindResponse {
                hits: Vec::new(),
                total_matched: 0,
                total_files: 0,
            });
        }

        self.wait_for_scan()?;
        let picker_guard = self
            .shared_picker
            .read()
            .map_err(|e| SearchError::Lock(e.to_string()))?;
        let picker = picker_guard
            .as_ref()
            .ok_or_else(|| SearchError::Init("workspace picker missing".into()))?;

        let parser = QueryParser::default();
        let parsed = parser.parse(query.trim());
        let current_file = current_file.and_then(|path| path.to_str());
        let results = picker.fuzzy_search(
            &parsed,
            None,
            FuzzySearchOptions {
                max_threads: 0,
                current_file,
                project_path: Some(self.root.as_path()),
                pagination: PaginationArgs {
                    offset: 0,
                    limit: max_results,
                },
                ..Default::default()
            },
        );

        let top_score = results
            .scores
            .first()
            .map(|score| score.total.max(1))
            .unwrap_or(1);
        let total_matched = results.total_matched;
        let total_files = results.total_files;
        let hits = results
            .items
            .into_iter()
            .zip(results.scores)
            .zip(results.match_byte_offsets)
            .take(max_results)
            .map(|((item, score), match_ranges)| {
                let path = item.relative_path(picker).to_string();
                let relevance = (score.total as f32 / top_score as f32).clamp(0.0, 1.0);
                FileSearchHit {
                    path,
                    score: score.total,
                    relevance,
                    match_ranges: match_ranges.into_iter().collect(),
                }
            })
            .collect();

        Ok(FindResponse {
            hits,
            total_matched,
            total_files,
        })
    }

    /// Full-text search across indexed files, respecting git-aware ignore rules.
    pub fn grep(
        &self,
        pattern: &str,
        path_filter: Option<&str>,
        mode: GrepQueryMode,
        max_results: usize,
    ) -> Result<GrepResponse, SearchError> {
        if max_results == 0 || pattern.trim().is_empty() {
            return Ok(GrepResponse {
                hits: Vec::new(),
                total_matched: 0,
            });
        }

        self.wait_for_scan()?;
        let picker_guard = self
            .shared_picker
            .read()
            .map_err(|e| SearchError::Lock(e.to_string()))?;
        let picker = picker_guard
            .as_ref()
            .ok_or_else(|| SearchError::Init("workspace picker missing".into()))?;

        let parsed = parse_grep_query(pattern);
        let result = picker.grep(
            &parsed,
            &GrepSearchOptions {
                mode: grep_mode(pattern, mode),
                page_limit: max_results,
                before_context: DEFAULT_CONTEXT_LINES,
                after_context: DEFAULT_CONTEXT_LINES,
                ..Default::default()
            },
        );

        let max_fuzzy = result
            .matches
            .iter()
            .filter_map(|entry| entry.fuzzy_score)
            .max()
            .unwrap_or(1)
            .max(1);
        let filter = path_filter.map(|value| value.to_ascii_lowercase());
        let total_matched = result.matches.len();
        let mut hits = Vec::with_capacity(result.matches.len().min(max_results));
        for entry in result.matches.into_iter().take(max_results) {
            let rel = result.files[entry.file_index].relative_path(picker);
            if let Some(filter) = &filter {
                if !rel.to_ascii_lowercase().contains(filter) {
                    continue;
                }
            }
            let relevance = entry
                .fuzzy_score
                .map(|score| (score as f32 / max_fuzzy as f32).clamp(0.0, 1.0));
            hits.push(GrepSearchHit {
                path: rel.to_string(),
                line: entry.line_number,
                column: entry.col.saturating_add(1) as u32,
                text: entry.line_content.trim().to_string(),
                context: format_grep_context(&entry.context_before, &entry.context_after),
                relevance,
                is_definition: entry.is_definition,
            });
        }

        Ok(GrepResponse {
            hits,
            total_matched,
        })
    }

    /// Record that a file was opened so future ranking can boost recency.
    pub fn note_file_opened(&self, path: impl AsRef<Path>) -> Result<(), SearchError> {
        let path = path.as_ref();
        let frecency = self
            .shared_frecency
            .read()
            .map_err(|e| SearchError::Lock(e.to_string()))?;
        if let Some(tracker) = frecency.as_ref() {
            let _ = tracker.track_access(path);
        }
        drop(frecency);

        let mut picker_guard = self
            .shared_picker
            .write()
            .map_err(|e| SearchError::Lock(e.to_string()))?;
        if let Some(picker) = picker_guard.as_mut() {
            if let Ok(frecency) = self.shared_frecency.read() {
                if let Some(tracker) = frecency.as_ref() {
                    let _ = picker.update_single_file_frecency(path, tracker);
                }
            }
        }
        Ok(())
    }
}

fn grep_mode(pattern: &str, mode: GrepQueryMode) -> GrepMode {
    match mode {
        GrepQueryMode::Plain => {
            if parse_regex_literal(pattern).is_some() {
                GrepMode::Regex
            } else {
                GrepMode::PlainText
            }
        }
        GrepQueryMode::Regex => GrepMode::Regex,
        GrepQueryMode::Fuzzy => GrepMode::Fuzzy,
    }
}

fn parse_regex_literal(pattern: &str) -> Option<&str> {
    if pattern.len() >= 2 && pattern.starts_with('/') && pattern.ends_with('/') {
        Some(&pattern[1..pattern.len() - 1])
    } else {
        None
    }
}

fn format_grep_context(before: &[String], after: &[String]) -> Option<String> {
    if before.is_empty() && after.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    lines.extend(before.iter().cloned());
    lines.extend(after.iter().cloned());
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GrepQueryMode;

    #[test]
    fn find_files_returns_structured_hits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        let response = index.find_files("main.rs", 10, None).unwrap();
        assert!(!response.hits.is_empty());
        assert_eq!(response.hits[0].path, "src/main.rs");
        assert!(response.hits[0].relevance > 0.0);
    }

    #[test]
    fn grep_returns_context_and_columns() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "alpha\nhello world\nomega\n",
        )
        .unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        let response = index
            .grep("hello", Some("main"), GrepQueryMode::Plain, 10)
            .unwrap();
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].line, 2);
        assert_eq!(response.hits[0].text, "hello world");
        assert!(response.hits[0].context.is_some());
    }

    #[test]
    fn plain_mode_auto_detects_regex_literal() {
        assert_eq!(grep_mode("/foo.*/", GrepQueryMode::Plain), GrepMode::Regex);
    }
}
