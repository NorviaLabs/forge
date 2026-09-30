use crate::quick_open::rerank_quick_open_hits;
use crate::types::{
    FileSearchHit, FindResponse, GrepQueryMode, GrepResponse, GrepSearchHit, MergedSearch,
};
use fff_query_parser::{Constraint, GrepConfig, QueryParser as GrepQueryParser};
use fff_search::{
    file_picker::{FFFMode, FilePicker, FilePickerOptions, FuzzySearchOptions},
    grep::{GrepMode, GrepSearchOptions},
    PaginationArgs, QueryParser, SharedFilePicker, SharedFrecency,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CONTEXT_LINES: usize = 1;
/// Cap in-memory grep hit text so a 30-hit search cannot materialize whole files.
const MAX_GREP_LINE_CHARS: usize = 240;
const MAX_GREP_CONTEXT_CHARS: usize = 320;
/// Ceiling on files listed for one search-as-you-type query.
pub const DEFAULT_SEARCH_MAX_FILES: usize = 200;
/// Content matching waits for this many characters. One character is a
/// whole-workspace scan whose only possible output is noise.
pub const MIN_CONTENT_QUERY_CHARS: usize = 2;
/// Content hits requested per listed file. Grep returns one hit per matching
/// *line*, so a query that lands in popular files needs several pages of hits
/// before the distinct-file count reaches [`DEFAULT_SEARCH_MAX_FILES`].
const CONTENT_PAGE_MULTIPLIER: usize = 8;

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
    /// When true (the default), [`WorkspaceIndex::open_with_options`] blocks
    /// until the first scan finishes. Session startup sets this to false so
    /// the scan overlaps journal open / first-prompt wait; [`Self::find_files`]
    /// and [`Self::grep`] still wait before returning hits.
    pub wait_for_scan: bool,
}

impl Default for WorkspaceIndexOptions {
    fn default() -> Self {
        Self {
            watch: true,
            scan_timeout: DEFAULT_SCAN_TIMEOUT,
            wait_for_scan: true,
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
        if options.wait_for_scan {
            index.wait_for_scan()?;
        }
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

    /// Search the workspace by file name *and* file content.
    ///
    /// Name hits come first, ranked as the Files panel already ranks them;
    /// files that matched only on content follow. A file that matches both
    /// ways is listed once, in the name tier, so content search can never
    /// displace a filename the user expected to see.
    pub fn search_files(&self, query: &str, max_files: usize) -> Result<MergedSearch, SearchError> {
        let query = query.trim();
        if max_files == 0 || query.is_empty() {
            return Ok(MergedSearch::default());
        }
        let name = self.find_files_quick_open(query, max_files, None)?;
        let remaining = max_files.saturating_sub(name.hits.len());
        let content = if remaining > 0 && query.chars().count() >= MIN_CONTENT_QUERY_CHARS {
            let page_limit = max_files
                .saturating_mul(CONTENT_PAGE_MULTIPLIER)
                .max(remaining);
            self.grep_scoped(query, None, None, GrepQueryMode::Literal, page_limit)?
        } else {
            GrepResponse {
                hits: Vec::new(),
                total_matched: 0,
            }
        };
        Ok(merge_search_results(name, content, max_files))
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

/// Merge name hits and content hits into one ordered, deduplicated file list.
///
/// Name hits keep their ranking and always precede content-only hits. Pure, so
/// the rule is testable without a workspace, an index, or a UI.
pub fn merge_search_results(
    name: FindResponse,
    content: GrepResponse,
    max_files: usize,
) -> MergedSearch {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut paths: Vec<String> = Vec::new();
    for hit in name
        .hits
        .iter()
        .map(|hit| hit.path.as_str())
        .chain(content.hits.iter().map(|hit| hit.path.as_str()))
    {
        if paths.len() >= max_files {
            break;
        }
        if seen.insert(hit) {
            paths.push(hit.to_string());
        }
    }
    MergedSearch {
        // Either tier stopped early — a full name tier, or a content page that
        // came back full — so the listing is a prefix, not the result set.
        truncated: max_files > 0
            && (paths.len() >= max_files
                || content.total_matched >= max_files.saturating_mul(CONTENT_PAGE_MULTIPLIER)),
        paths,
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
        GrepQueryMode::Literal => GrepMode::PlainText,
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
    fn open_without_waiting_still_serves_find_after_scan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                wait_for_scan: false,
                ..Default::default()
            },
        )
        .unwrap();
        let response = index.find_files("main.rs", 10, None).unwrap();
        assert_eq!(
            response.hits.first().map(|hit| hit.path.as_str()),
            Some("src/main.rs")
        );
    }

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

    #[test]
    fn query_helpers_cover_scope_modes_and_bounded_formatting() {
        let path_query = scoped_grep_query("needle", Some("./src/"), Some("*.rs"));
        assert!(path_query
            .constraints
            .iter()
            .any(|constraint| matches!(constraint, Constraint::PathSegment("src"))));
        assert!(path_query
            .constraints
            .iter()
            .any(|constraint| matches!(constraint, Constraint::Extension("rs"))));

        let file_query = scoped_grep_query("needle", Some("README.md"), Some("*.{md,txt}"));
        assert!(file_query
            .constraints
            .iter()
            .any(|constraint| matches!(constraint, Constraint::FilePath("README.md"))));
        assert!(file_query
            .constraints
            .iter()
            .any(|constraint| matches!(constraint, Constraint::Glob("*.{md,txt}"))));

        assert_eq!(
            grep_mode("plain", GrepQueryMode::Plain),
            GrepMode::PlainText
        );
        assert_eq!(grep_mode("plain", GrepQueryMode::Regex), GrepMode::Regex);
        assert_eq!(grep_mode("plain", GrepQueryMode::Fuzzy), GrepMode::Fuzzy);
        assert_eq!(parse_regex_literal("/abc/"), Some("abc"));
        assert_eq!(parse_regex_literal("abc"), None);
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        assert_eq!(format_grep_context(&[], &[]), None);
        assert_eq!(
            format_grep_context(&["before".into()], &["after".into()]),
            Some("before\nafter".into())
        );
    }

    #[test]
    fn zero_result_and_file_change_paths_are_noops() {
        let dir = tempfile::tempdir().unwrap();
        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            index.find_files("anything", 0, None).unwrap().total_files,
            0
        );
        assert_eq!(
            index
                .grep("anything", None, GrepQueryMode::Plain, 0)
                .unwrap()
                .total_matched,
            0
        );
        assert_eq!(
            index
                .find_files_quick_open(" ", 0, None)
                .unwrap()
                .hits
                .len(),
            0
        );
        let path = dir.path().join("new.txt");
        std::fs::write(&path, "new").unwrap();
        index.note_file_changed(&path).unwrap();
        index.note_file_opened(&path).unwrap();
    }

    fn name_response(paths: &[&str]) -> FindResponse {
        FindResponse {
            hits: paths
                .iter()
                .map(|path| FileSearchHit {
                    path: (*path).to_string(),
                    score: 1,
                    relevance: 1.0,
                    match_ranges: Vec::new(),
                })
                .collect(),
            total_matched: paths.len(),
            total_files: paths.len(),
        }
    }

    fn content_response(paths: &[&str]) -> GrepResponse {
        GrepResponse {
            hits: paths
                .iter()
                .map(|path| GrepSearchHit {
                    path: (*path).to_string(),
                    line: 1,
                    column: 1,
                    text: String::new(),
                    context: None,
                    relevance: None,
                    is_definition: false,
                })
                .collect(),
            total_matched: paths.len(),
        }
    }

    #[test]
    fn merge_puts_name_hits_first_and_dedupes_across_tiers() {
        let merged = merge_search_results(
            name_response(&["src/lib.rs", "src/main.rs"]),
            content_response(&["src/main.rs", "docs/guide.md"]),
            10,
        );
        assert_eq!(
            merged.paths,
            vec!["src/lib.rs", "src/main.rs", "docs/guide.md"]
        );
        assert!(!merged.truncated);
    }

    #[test]
    fn merge_reports_truncation_when_a_tier_fills_the_cap() {
        let filled =
            merge_search_results(name_response(&["a.rs", "b.rs"]), content_response(&[]), 2);
        assert_eq!(filled.paths, vec!["a.rs", "b.rs"]);
        assert!(filled.truncated);

        // A full content page means the scan stopped early even though the
        // listing is nowhere near the cap.
        let full_page = merge_search_results(
            name_response(&["a.rs"]),
            GrepResponse {
                hits: Vec::new(),
                total_matched: DEFAULT_SEARCH_MAX_FILES * 8,
            },
            DEFAULT_SEARCH_MAX_FILES,
        );
        assert_eq!(full_page.paths, vec!["a.rs"]);
        assert!(full_page.truncated);
    }

    #[test]
    fn merge_with_no_capacity_lists_nothing_and_claims_no_truncation() {
        let merged = merge_search_results(name_response(&["a.rs"]), content_response(&[]), 0);
        assert!(merged.paths.is_empty());
        assert!(!merged.truncated);
    }

    #[test]
    fn search_files_finds_content_in_a_file_whose_name_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn distinctive_symbol_name() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/other.rs"), "pub fn unrelated() {}\n").unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        let merged = index.search_files("distinctive_symbol", 10).unwrap();
        assert_eq!(merged.paths, vec!["src/lib.rs"]);
        assert!(!merged.truncated);
    }

    #[test]
    fn search_files_ranks_a_name_match_above_a_content_only_hit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("needle.rs"), "nothing here\n").unwrap();
        std::fs::write(dir.path().join("aardvark.rs"), "let needle = 1;\n").unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        let merged = index.search_files("needle", 10).unwrap();
        assert_eq!(
            merged.paths,
            vec!["needle.rs", "aardvark.rs"],
            "the filename match leads and the content-only hit follows"
        );
    }

    #[test]
    fn search_files_waits_for_two_characters_before_scanning_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain.rs"), "let zz = 1;\n").unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        // `z` would sweep the whole workspace for a single character, so the
        // content tier stays off and only names are considered.
        let single = index.search_files("z", 10).unwrap();
        assert!(single.paths.iter().all(|path| path.contains('z')));

        let two = index.search_files("zz", 10).unwrap();
        assert!(two.paths.contains(&"plain.rs".to_string()));
    }

    #[test]
    fn search_files_stops_at_the_cap_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..12 {
            std::fs::write(dir.path().join(format!("f{index}.txt")), "shared token\n").unwrap();
        }

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        let merged = index.search_files("token", 3).unwrap();
        assert_eq!(merged.paths.len(), 3);
        assert!(merged.truncated);
    }

    #[test]
    fn search_files_treats_a_slash_wrapped_query_as_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "wrapped\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "/wrapped/ inner\n").unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        // `Plain` reads `/…/` as a regex literal and would list both files. A
        // search field's query is never a regex, so only the file that really
        // contains the slashes comes back.
        let merged = index.search_files("/wrapped/", 10).unwrap();
        assert_eq!(merged.paths, vec!["b.txt"]);
    }

    /// A short query against a deeply nested path scored below zero from the
    /// leading and suffix penalties alone. That used to read as "no match", so
    /// `cl` listed nothing at all even though the index held the file.
    #[test]
    fn short_queries_still_match_deeply_nested_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/api/deep")).unwrap();
        std::fs::write(dir.path().join("src/api/deep/client.rs"), "").unwrap();

        let index = WorkspaceIndex::open_with_options(
            dir.path(),
            WorkspaceIndexOptions {
                watch: false,
                ..Default::default()
            },
        )
        .unwrap();
        for query in ["c", "cl", "cli", "client"] {
            assert_eq!(
                index.search_files(query, 200).unwrap().paths,
                vec!["src/api/deep/client.rs"],
                "{query:?} must still list the file whose name contains it"
            );
        }
    }
}
