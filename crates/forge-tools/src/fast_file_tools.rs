use async_trait::async_trait;
use forge_search::{GrepQueryMode, SearchError, WorkspaceIndex, WorkspaceIndexOptions};
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use crate::registry::ToolContext;
use crate::{Tool, ToolError};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FffFindArgs {
    /// Path/filename pattern (fuzzy, typo-resistant). Examples: `main.rs`, `src/**/*.rs`, `button`
    pub query: String,
    #[serde(default = "fff_find_max")]
    pub max_results: u32,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GlobArgs {
    /// File pattern, for example `src/**/*.rs`.
    pub pattern: String,
    /// Optional workspace-relative directory to search.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "fff_find_max")]
    pub max_results: u32,
}

fn fff_find_max() -> u32 {
    50
}

/// Hard cap so a model cannot request millions of hits and pin the agent loop.
const MAX_FFF_RESULTS: u32 = 200;
const MAX_FFF_QUERY_CHARS: usize = 512;

fn clamp_fff_results(requested: u32) -> usize {
    requested.min(MAX_FFF_RESULTS) as usize
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FffGrepArgs {
    /// The regex pattern to search for in file contents.
    pub pattern: String,
    /// The directory or file to search. Defaults to the workspace root.
    #[serde(default)]
    pub path: Option<String>,
    /// File pattern to include (e.g. `*.js`, `*.{ts,tsx}`).
    #[serde(default)]
    pub include: Option<String>,
    /// Search mode: regex (default, like ripgrep), plain, or fuzzy.
    #[serde(default)]
    pub mode: Option<FffModeArg>,
    #[serde(default = "fff_grep_max")]
    pub max_results: u32,
}

fn fff_grep_max() -> u32 {
    50
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[non_exhaustive]
pub enum FffModeArg {
    #[serde(rename = "plain")]
    Plain,
    #[serde(rename = "regex")]
    Regex,
    #[serde(rename = "fuzzy")]
    Fuzzy,
}

impl From<&FffModeArg> for GrepQueryMode {
    fn from(mode: &FffModeArg) -> Self {
        match mode {
            FffModeArg::Plain => GrepQueryMode::Plain,
            FffModeArg::Regex => GrepQueryMode::Regex,
            FffModeArg::Fuzzy => GrepQueryMode::Fuzzy,
        }
    }
}

type WorkspaceIndexSlot = Arc<OnceLock<Result<Arc<WorkspaceIndex>, String>>>;
type WorkspaceIndexSlots = Mutex<HashMap<PathBuf, WorkspaceIndexSlot>>;

/// Per-workspace FFF index cache shared by glob/grep/edit.
///
/// Opening starts the scan but does not wait for it. [`Self::index_for`]
/// waits so a tool call still sees a complete index; [`Self::warm`] starts
/// that scan on a background thread so session startup can overlap it with
/// journal I/O and the first prompt.
pub(crate) struct FastFileState {
    slots: WorkspaceIndexSlots,
}

impl FastFileState {
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    fn slot_for(&self, root: &Path) -> Result<(PathBuf, WorkspaceIndexSlot, bool), ToolError> {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut slots = self
            .slots
            .lock()
            .map_err(|e| ToolError::Execution(format!("fff cache lock: {e}")))?;
        let mut inserted = false;
        let slot = slots
            .entry(canonical.clone())
            .or_insert_with(|| {
                inserted = true;
                Arc::new(OnceLock::new())
            })
            .clone();
        Ok((canonical, slot, inserted))
    }

    fn open_index(root: &Path) -> Result<Arc<WorkspaceIndex>, String> {
        // Do not wait here: the scan runs in FilePicker's own thread. Callers
        // that need a complete index call `wait_for_scan` themselves.
        WorkspaceIndex::open_with_options(
            root,
            WorkspaceIndexOptions {
                watch: true,
                wait_for_scan: false,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())
    }

    /// Start the workspace scan if it is not already running. Returns
    /// immediately; the first tool call still waits via [`Self::index_for`].
    pub(crate) fn warm(&self, root: &Path) {
        let Ok((canonical, slot, inserted)) = self.slot_for(root) else {
            return;
        };
        if !inserted {
            return;
        }
        let _ = thread::Builder::new()
            .name("forge-workspace-index".into())
            .spawn(move || {
                let _ = slot.get_or_init(|| Self::open_index(&canonical));
            });
    }

    pub(crate) fn index_for(&self, root: &Path) -> Result<Arc<WorkspaceIndex>, ToolError> {
        let (canonical, slot, _) = self.slot_for(root)?;
        // `get_or_init` serialises concurrent first-open: a session-start warm
        // and the first glob/grep share one FilePicker instead of leaking a
        // second watcher.
        match slot.get_or_init(|| Self::open_index(&canonical)) {
            Ok(index) => {
                index.wait_for_scan().map_err(search_err)?;
                Ok(index.clone())
            }
            Err(error) => Err(ToolError::Execution(error.clone())),
        }
    }
}

fn search_err(error: SearchError) -> ToolError {
    ToolError::Execution(error.to_string())
}

pub struct FffFindTool {
    state: Arc<FastFileState>,
    name: &'static str,
}

impl FffFindTool {
    pub(crate) fn new(state: Arc<FastFileState>, name: &'static str) -> Self {
        Self { state, name }
    }
}

#[async_trait]
impl Tool for FffFindTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "Find files by glob/path pattern in the workspace using the indexed search engine. Use this instead of bash with `find`, `fd`, or `ls`."
    }
    fn input_schema(&self) -> Value {
        if self.name == "glob" {
            crate::builtins::schema_for::<GlobArgs>()
        } else {
            crate::builtins::schema_for::<FffFindArgs>()
        }
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn idempotent(&self) -> bool {
        true
    }
    fn parallel_safe(&self) -> bool {
        true
    }

    fn warm_workspace(&self, root: &Path) {
        self.state.warm(root);
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let (query, path, max_results) = if self.name == "glob" {
            let args: GlobArgs = serde_json::from_value(args)
                .map_err(|e| ToolError::Execution(format!("glob args: {e}")))?;
            (args.pattern, args.path, args.max_results)
        } else {
            let args: FffFindArgs = serde_json::from_value(args)
                .map_err(|e| ToolError::Execution(format!("fff args: {e}")))?;
            (args.query, None, args.max_results)
        };
        if let Some(output) = reject_blank_or_oversized(self.name, "pattern", &query) {
            return Ok(output);
        }
        let state = self.state.clone();
        let root = ctx.workspace_root.clone();
        let max_results = clamp_fff_results(max_results);
        let query = if let Some(path) = path {
            format!("{path}/{query}")
        } else {
            query
        };
        let response = tokio::task::spawn_blocking(move || {
            state
                .index_for(&root)?
                .find_files(&query, max_results, None)
                .map_err(search_err)
        })
        .await
        .map_err(|e| ToolError::Execution(format!("fff worker: {e}")))??;

        Ok(ToolOutput {
            outcome: Default::default(),
            content: if response.hits.is_empty() {
                "No files found".into()
            } else {
                let mut output = response
                    .hits
                    .iter()
                    .map(|hit| hit.path.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if response.total_matched > response.hits.len() {
                    output.push_str("\n\n(Results truncated; use a narrower pattern.)");
                }
                output
            },
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

pub struct FffGrepTool {
    state: Arc<FastFileState>,
    name: &'static str,
}

impl FffGrepTool {
    pub(crate) fn new(state: Arc<FastFileState>, name: &'static str) -> Self {
        Self { state, name }
    }
}

#[async_trait]
impl Tool for FffGrepTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "Fast content search. Searches file contents using regular expressions \
         (ripgrep syntax). Filter files with the include parameter \
         (e.g. `*.js`, `*.{ts,tsx}`). Prefer this over bash with `rg` or `grep`. \
         Do not use the unix `grep` command."
    }
    fn input_schema(&self) -> Value {
        crate::builtins::schema_for::<FffGrepArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn idempotent(&self) -> bool {
        true
    }
    fn parallel_safe(&self) -> bool {
        true
    }

    fn warm_workspace(&self, root: &Path) {
        self.state.warm(root);
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: FffGrepArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::Execution(format!("fff args: {e}")))?;
        if let Some(output) = reject_blank_or_oversized(self.name, "pattern", &args.pattern) {
            return Ok(output);
        }
        let mode = args
            .mode
            .as_ref()
            .map(GrepQueryMode::from)
            .unwrap_or(GrepQueryMode::Regex);
        let state = self.state.clone();
        let root = ctx.workspace_root.clone();
        let pattern = args.pattern.clone();
        let path = args.path.clone();
        let include = args.include.clone();
        let max_results = clamp_fff_results(args.max_results);
        let response = tokio::task::spawn_blocking(move || {
            state
                .index_for(&root)?
                .grep_scoped(
                    &pattern,
                    path.as_deref(),
                    include.as_deref(),
                    mode,
                    max_results,
                )
                .map_err(search_err)
        })
        .await
        .map_err(|e| ToolError::Execution(format!("grep worker: {e}")))??;

        let hits = response.hits;
        Ok(ToolOutput {
            outcome: Default::default(),
            content: if hits.is_empty() {
                "No matches found".into()
            } else {
                let mut output = String::new();
                let mut current_path = "";
                for hit in &hits {
                    if current_path != hit.path {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(&format!("{}:\n", hit.path));
                        current_path = &hit.path;
                    }
                    output.push_str(&format!("  Line {}: {}\n", hit.line, hit.text));
                }
                if response.total_matched >= max_results {
                    output.push_str("\n(Results truncated; use a narrower pattern.)");
                }
                output.trim_end().to_string()
            },
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

fn reject_blank_or_oversized(tool: &str, field: &str, value: &str) -> Option<ToolOutput> {
    if value.trim().is_empty() {
        return Some(ToolOutput {
            outcome: Default::default(),
            content: format!("{tool}: {field} must be non-empty"),
            is_error: true,
            exit_code: None,
            attachments: Vec::new(),
        });
    }
    if value.chars().count() > MAX_FFF_QUERY_CHARS {
        return Some(ToolOutput {
            outcome: Default::default(),
            content: format!("{tool}: {field} exceeds max length ({MAX_FFF_QUERY_CHARS} chars)"),
            is_error: true,
            exit_code: None,
            attachments: Vec::new(),
        });
    }
    None
}

pub fn fff_tools() -> Vec<Arc<dyn Tool>> {
    fff_tools_with(Arc::new(FastFileState::new()))
}

pub(crate) fn fff_tools_with(state: Arc<FastFileState>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(FffFindTool::new(state.clone(), "glob")),
        Arc::new(FffGrepTool::new(state.clone(), "grep")),
        Arc::new(crate::EditTool::new(state)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_search::FindResponse;
    use tempfile::tempdir;

    #[test]
    fn find_uses_shared_index() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();

        let state = FastFileState::new();
        let index = state.index_for(dir.path()).unwrap();
        let response = index.find_files("main.rs", 10, None).unwrap();
        assert_eq!(
            response.hits.first().map(|hit| hit.path.as_str()),
            Some("src/main.rs")
        );
    }

    #[test]
    fn grep_filters_results() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "hello world\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "goodbye\n").unwrap();

        let state = FastFileState::new();
        let index = state.index_for(dir.path()).unwrap();
        let response = index
            .grep("hello", Some("src/main.rs"), GrepQueryMode::Plain, 10)
            .unwrap();
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].path, "src/main.rs");
        assert_eq!(response.hits[0].line, 1);
        assert_eq!(response.hits[0].text, "hello world");
    }

    #[test]
    fn grep_filter_skips_non_matching_paths_when_others_match() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "shared line\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "shared line\n").unwrap();

        let state = FastFileState::new();
        let index = state.index_for(dir.path()).unwrap();
        let response = index
            .grep("shared", Some("src/main.rs"), GrepQueryMode::Plain, 10)
            .unwrap();
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].path, "src/main.rs");
    }

    #[test]
    fn find_returns_empty_without_scanning_when_max_results_is_zero() {
        let dir = tempdir().unwrap();
        let state = FastFileState::new();
        let index = state.index_for(dir.path()).unwrap();
        let response = index.find_files("anything", 0, None).unwrap();
        assert_eq!(
            response,
            FindResponse {
                hits: Vec::new(),
                total_matched: 0,
                total_files: 0,
            }
        );
    }

    #[test]
    fn grep_returns_empty_when_max_results_is_zero() {
        let dir = tempdir().unwrap();
        let state = FastFileState::new();
        let index = state.index_for(dir.path()).unwrap();
        let response = index
            .grep("anything", None, GrepQueryMode::Plain, 0)
            .unwrap();
        assert!(response.hits.is_empty());
    }

    #[test]
    fn grep_returns_empty_for_blank_pattern() {
        let dir = tempdir().unwrap();
        let state = FastFileState::new();
        let index = state.index_for(dir.path()).unwrap();
        let response = index.grep("   ", None, GrepQueryMode::Plain, 10).unwrap();
        assert!(response.hits.is_empty());
    }

    #[test]
    fn clamp_fff_results_caps_unbounded_requests() {
        assert_eq!(clamp_fff_results(0), 0);
        assert_eq!(clamp_fff_results(50), 50);
        assert_eq!(clamp_fff_results(MAX_FFF_RESULTS), MAX_FFF_RESULTS as usize);
        assert_eq!(
            clamp_fff_results(u32::MAX),
            MAX_FFF_RESULTS as usize,
            "a model-supplied u32::MAX must not become a multi-gigabyte scan"
        );
    }

    #[test]
    fn index_for_reuses_the_same_handle() {
        let dir = tempdir().unwrap();
        let state = FastFileState::new();
        let first = state.index_for(dir.path()).unwrap();
        let second = state.index_for(dir.path()).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn warm_starts_the_index_the_first_search_reuses() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let state = FastFileState::new();
        state.warm(dir.path());
        let first = state.index_for(dir.path()).unwrap();
        let second = state.index_for(dir.path()).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let response = first.find_files("main.rs", 10, None).unwrap();
        assert_eq!(
            response.hits.first().map(|hit| hit.path.as_str()),
            Some("main.rs")
        );
    }

    #[tokio::test]
    async fn warming_shared_tools_then_glob_finds_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn lib() {}\n").unwrap();
        let tools = fff_tools();
        for tool in &tools {
            tool.warm_workspace(dir.path());
        }
        for tool in &tools {
            tool.warm_workspace(dir.path());
        }
        let glob = tools.iter().find(|tool| tool.name() == "glob").unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let out = glob
            .call(&ctx, serde_json::json!({"pattern": "lib.rs"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("lib.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn glob_call_returns_json_hits() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let tool = FffFindTool::new(Arc::new(FastFileState::new()), "glob");
        let out = tool
            .call(&ctx, serde_json::json!({"pattern": "main.rs"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("src/main.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn glob_rejects_blank_query() {
        let dir = tempdir().unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let tool = FffFindTool::new(Arc::new(FastFileState::new()), "glob");
        let out = tool
            .call(&ctx, serde_json::json!({"pattern": "   "}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("non-empty"), "{}", out.content);
    }

    #[tokio::test]
    async fn glob_rejects_oversized_query() {
        let dir = tempdir().unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let tool = FffFindTool::new(Arc::new(FastFileState::new()), "glob");
        let query = "a".repeat(MAX_FFF_QUERY_CHARS + 1);
        let out = tool
            .call(&ctx, serde_json::json!({"pattern": query}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("max length"), "{}", out.content);
    }

    #[tokio::test]
    async fn grep_call_returns_json_hits() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "hello world\n").unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let tool = FffGrepTool::new(Arc::new(FastFileState::new()), "grep");
        let out = tool
            .call(
                &ctx,
                serde_json::json!({"pattern": "hello", "path": "src/main.rs"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("src/main.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn grep_path_scope_survives_earlier_out_of_scope_hits() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("crates/core")).unwrap();
        for i in 0..60 {
            std::fs::write(
                dir.path().join(format!("README-{i}.md")),
                "forge is the project name\n",
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("crates/core/lib.rs"), "//! forge-core\n").unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let tool = FffGrepTool::new(Arc::new(FastFileState::new()), "grep");
        let out = tool
            .call(
                &ctx,
                serde_json::json!({"pattern": "forge", "path": "crates"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("crates/core/lib.rs"),
            "expected crate hit, got {}",
            out.content
        );
        assert!(
            !out.content.contains("README-"),
            "root hits must not leak into a crates-scoped search: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn grep_rejects_blank_pattern() {
        let dir = tempdir().unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let tool = FffGrepTool::new(Arc::new(FastFileState::new()), "grep");
        let out = tool
            .call(&ctx, serde_json::json!({"pattern": "  "}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("non-empty"), "{}", out.content);
    }

    #[tokio::test]
    async fn grep_clamps_max_results() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hit\n").unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let tool = FffGrepTool::new(Arc::new(FastFileState::new()), "grep");
        let out = tool
            .call(
                &ctx,
                serde_json::json!({"pattern": "hit", "max_results": u32::MAX}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let match_count = out
            .content
            .lines()
            .filter(|line| line.trim_start().starts_with("Line "))
            .count();
        assert!(match_count <= MAX_FFF_RESULTS as usize);
    }

    #[tokio::test]
    async fn grep_invalid_regex_does_not_panic() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let ctx = crate::registry::ToolContext::new(dir.path().to_path_buf());
        let tool = FffGrepTool::new(Arc::new(FastFileState::new()), "grep");
        let result = tool
            .call(&ctx, serde_json::json!({"pattern": "[", "mode": "regex"}))
            .await;
        match result {
            Ok(out) => {
                // Either no matches or a structured error — never a panic.
                let _ = out.content;
            }
            Err(error) => {
                assert!(
                    error.to_string().contains("fff") || error.to_string().contains("regex"),
                    "{error}"
                );
            }
        }
    }
}
