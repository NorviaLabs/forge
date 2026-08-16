use crate::quick_open::rerank_quick_open_hits;
use crate::types::{FileSearchHit, FindResponse, GrepQueryMode, GrepResponse, GrepSearchHit};
use fff_query_parser::{Constraint, GrepConfig, QueryParser as GrepQueryParser};
use fff_search::{
    file_picker::{FFFMode, FilePicker, FilePickerOptions, FuzzySearchOptions},
    grep::{GrepMode, GrepSearchOptions},
    PaginationArgs, QueryParser, SharedFilePicker, SharedFrecency,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CONTEXT_LINES: usize = 1;
/// Cap in-memory grep hit text so a 30-hit search cannot materialize whole files.
const MAX_GREP_LINE_CHARS: usize = 240;
const MAX_GREP_CONTEXT_CHARS: usize = 320;

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

    /// Quick Open search with VS Code–style word-boundary scoring and path awareness.
    ///
    /// Empty queries still return frecency-ranked files. Non-empty queries pull a
    /// broader fuzzy candidate set from fff, then re-rank with [`crate::quick_open`].
    pub fn find_files_quick_open(
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

        if query.trim().is_empty() {
            return self.find_files(query, max_results, current_file);
        }

        let candidate_limit = max_results.saturating_mul(8).clamp(100, 400);
        let response = self.find_files(query, candidate_limit, current_file)?;
        let total_files = response.total_files;
        let hits = rerank_quick_open_hits(response.hits, query);
        let total_matched = hits.len();
        let hits = hits.into_iter().take(max_results).collect();
        Ok(FindResponse {
            hits,
            total_matched,
            total_files,
        })
    }

    /// Full-text search across indexed files, respecting git-aware ignore rules.
    ///
    /// `path` and `include` are applied as FFF constraints *before* pagination.
    /// Post-filtering the first page used to drop every hit when the first
    /// `max_results` matches lived outside the requested directory (e.g.
    /// searching `forge` under `crates/` in this repo).
    pub fn grep(
        &self,
        pattern: &str,
        path_filter: Option<&str>,
        mode: GrepQueryMode,
        max_results: usize,
    ) -> Result<GrepResponse, SearchError> {
        self.grep_scoped(pattern, path_filter, None, mode, max_results)
    }

    pub fn grep_scoped(
        &self,
        pattern: &str,
        path_filter: Option<&str>,
        include: Option<&str>,
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

        let parsed = scoped_grep_query(pattern, path_filter, include);
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
        let total_matched = result.matches.len();
        let mut hits = Vec::with_capacity(result.matches.len().min(max_results));
        for entry in result.matches.into_iter().take(max_results) {
            let rel = result.files[entry.file_index].relative_path(picker);
            let relevance = entry
                .fuzzy_score
                .map(|score| (score as f32 / max_fuzzy as f32).clamp(0.0, 1.0));
            hits.push(GrepSearchHit {
                path: rel.to_string(),
                line: entry.line_number,
                column: entry.col.saturating_add(1) as u32,
                text: truncate_chars(entry.line_content.trim(), MAX_GREP_LINE_CHARS),
                context: format_grep_context(&entry.context_before, &entry.context_after)
                    .map(|ctx| truncate_chars(&ctx, MAX_GREP_CONTEXT_CHARS)),
                relevance,
                is_definition: entry.is_definition,
            });
        }

        Ok(GrepResponse {
            hits,
            total_matched,
        })
    }

    /// Re-read a file the agent just wrote so later grep/glob see the new bytes
    /// without waiting on the filesystem watcher.
    pub fn note_file_changed(&self, path: impl AsRef<Path>) -> Result<(), SearchError> {
        let path = path.as_ref();
        let mut picker = self
            .shared_picker
            .write()
            .map_err(|e| SearchError::Lock(e.to_string()))?;
        if let Some(picker) = picker.as_mut() {
            let _ = picker.handle_create_or_modify(path);
        }
        Ok(())
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

fn scoped_grep_query<'a>(
    pattern: &'a str,
    path_filter: Option<&'a str>,
    include: Option<&'a str>,
) -> fff_query_parser::FFFQuery<'a> {
    let mut query = GrepQueryParser::new(GrepConfig).parse(pattern);
    if let Some(path) = path_filter
        .map(str::trim)
        .map(|path| path.trim_start_matches("./").trim_end_matches('/'))
        .filter(|path| !path.is_empty())
    {
        if Constraint::is_filename_constraint_token(path) {
            query.constraints.push(Constraint::FilePath(path));
        } else {
            query.constraints.push(Constraint::PathSegment(path));
        }
    }
    if let Some(include) = include.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some(ext) = include.strip_prefix("*.") {
            if !ext.is_empty()
                && !ext
                    .bytes()
                    .any(|b| matches!(b, b'*' | b'?' | b'{' | b'[' | b'/'))
            {
                query.constraints.push(Constraint::Extension(ext));
            } else {
                query.constraints.push(Constraint::Glob(include));
            }
        } else {
            query.constraints.push(Constraint::Glob(include));
        }
    }
    query
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

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
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
            .grep("hello", Some("src/main.rs"), GrepQueryMode::Plain, 10)
            .unwrap();
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].line, 2);
        assert_eq!(response.hits[0].text, "hello world");
        assert!(response.hits[0].context.is_some());
    }

    #[test]
    fn grep_path_scope_applies_before_pagination() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/core")).unwrap();
        for i in 0..60 {
            std::fs::write(
                dir.path().join(format!("root-{i}.md")),
                "this repository is called forge\n",
            )
            .unwrap();
        }
        std::fs::write(
            dir.path().join("crates/core/lib.rs"),
            "pub const NAME: &str = \"forge\";\n",
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
            .grep("forge", Some("crates"), GrepQueryMode::Plain, 50)
            .unwrap();
        assert!(
            response
                .hits
                .iter()
                .any(|hit| hit.path == "crates/core/lib.rs"),
            "path-scoped grep must not be emptied by earlier out-of-scope hits, got {:?}",
            response
                .hits
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            response
                .hits
                .iter()
                .all(|hit| hit.path.starts_with("crates/")),
            "hits must stay under the requested path"
        );
    }

    #[test]
    fn grep_include_glob_is_applied_before_pagination() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..60 {
            std::fs::write(
                dir.path().join(format!("note-{i}.md")),
                "forge lives here\n",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("lib.rs"), "fn forge() {}\n").unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        let response = index
            .grep_scoped("forge", None, Some("*.rs"), GrepQueryMode::Plain, 50)
            .unwrap();
        assert_eq!(response.hits.len(), 1, "{:?}", response.hits);
        assert_eq!(response.hits[0].path, "lib.rs");
    }

    #[test]
    fn grep_truncates_oversized_hit_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("wide.txt"),
            format!("prefix {} suffix\n", "x".repeat(800)),
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
            .grep("prefix", None, GrepQueryMode::Plain, 10)
            .unwrap();
        assert_eq!(response.hits.len(), 1);
        assert!(response.hits[0].text.chars().count() <= MAX_GREP_LINE_CHARS);
        assert!(response.hits[0].text.ends_with('…'));
    }

    #[test]
    fn plain_mode_auto_detects_regex_literal() {
        assert_eq!(grep_mode("/foo.*/", GrepQueryMode::Plain), GrepMode::Regex);
    }
}
