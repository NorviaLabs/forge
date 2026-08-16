use async_trait::async_trait;
use forge_search::{GrepQueryMode, SearchError, WorkspaceIndex, WorkspaceIndexOptions};
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::registry::ToolContext;
use crate::{Tool, ToolError};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FffFindArgs {
    /// Path/filename pattern (fuzzy, typo-resistant). Examples: `main.rs`, `src/**/*.rs`, `button`
    pub query: String,
    #[serde(default = "fff_find_max")]
    pub max_results: u32,
}

fn fff_find_max() -> u32 {
    50
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FffGrepArgs {
    /// Search pattern (plain text, regex, or fuzzy)
    pub pattern: String,
    /// Optional file path filter
    #[serde(default)]
    pub path: Option<String>,
    /// Search mode: plain (default), regex, fuzzy
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

pub(crate) struct FastFileState {
    indices: Mutex<HashMap<PathBuf, Arc<WorkspaceIndex>>>,
}

impl FastFileState {
    pub(crate) fn new() -> Self {
        Self {
            indices: Mutex::new(HashMap::new()),
        }
    }

    fn index_for(&self, root: &Path) -> Result<Arc<WorkspaceIndex>, ToolError> {
        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if let Some(index) = self
            .indices
            .lock()
            .map_err(|e| ToolError::Execution(format!("fff cache lock: {e}")))?
            .get(&canonical)
        {
            return Ok(index.clone());
        }
        // Build outside the cache lock: the first scan of a large workspace can
        // take seconds, and holding the mutex across it serialized every
        // concurrent find/grep behind the cold-cache scan. `watch: true` (the
        // default the TUI already uses) keeps the tool index live so files the
        // agent itself writes become searchable, instead of freezing the first
        // scan for the whole session.
        let index = WorkspaceIndex::open_with_options(
            &canonical,
            WorkspaceIndexOptions {
                watch: true,
                ..Default::default()
            },
        )
        .map_err(search_err)?;
        let mut guard = self
            .indices
            .lock()
            .map_err(|e| ToolError::Execution(format!("fff cache lock: {e}")))?;
        guard.insert(canonical, index.clone());
        Ok(index)
    }
}

fn search_err(error: SearchError) -> ToolError {
    ToolError::Execution(error.to_string())
}

pub struct FffFindTool {
    state: Arc<FastFileState>,
}

impl FffFindTool {
    pub(crate) fn new(state: Arc<FastFileState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for FffFindTool {
    fn name(&self) -> &str {
        "fffind"
    }
    fn description(&self) -> &str {
        "Find files in the workspace by path/name pattern. Prefer this over `find`, `fd`, or `ls` via bash."
    }
    fn input_schema(&self) -> Value {
        crate::builtins::schema_for::<FffFindArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn idempotent(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: FffFindArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::Execution(format!("fff args: {e}")))?;
        let state = self.state.clone();
        let root = ctx.workspace_root.clone();
        let query = args.query.clone();
        let max_results = args.max_results as usize;
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
                serde_json::json!({
                    "hits": [],
                    "total_matched": response.total_matched,
                    "total_files": response.total_files,
                    "message": "no matches found",
                })
                .to_string()
            } else {
                serde_json::to_string(&response)
                    .map_err(|e| ToolError::Execution(format!("fff encode: {e}")))?
            },
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

pub struct FffGrepTool {
    state: Arc<FastFileState>,
}

impl FffGrepTool {
    pub(crate) fn new(state: Arc<FastFileState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for FffGrepTool {
    fn name(&self) -> &str {
        "ffgrep"
    }
    fn description(&self) -> &str {
        "Search file contents in the workspace. Supports plain text, regex, and fuzzy matching. Prefer this over `rg` or `grep` via bash."
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

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: FffGrepArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::Execution(format!("fff args: {e}")))?;
        let mode = args
            .mode
            .as_ref()
            .map(GrepQueryMode::from)
            .unwrap_or(GrepQueryMode::Plain);
        let state = self.state.clone();
        let root = ctx.workspace_root.clone();
        let pattern = args.pattern.clone();
        let path = args.path.clone();
        let max_results = args.max_results as usize;
        let response = tokio::task::spawn_blocking(move || {
            state
                .index_for(&root)?
                .grep(&pattern, path.as_deref(), mode, max_results)
                .map_err(search_err)
        })
        .await
        .map_err(|e| ToolError::Execution(format!("ffgrep worker: {e}")))??;

        Ok(ToolOutput {
            outcome: Default::default(),
            content: if response.hits.is_empty() {
                serde_json::json!({
                    "hits": [],
                    "total_matched": response.total_matched,
                    "message": "no matches found",
                })
                .to_string()
            } else {
                serde_json::to_string(&response)
                    .map_err(|e| ToolError::Execution(format!("fff encode: {e}")))?
            },
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

pub fn fff_tools() -> Vec<Arc<dyn Tool>> {
    let state = Arc::new(FastFileState::new());
    vec![
        Arc::new(FffFindTool::new(state.clone())),
        Arc::new(FffGrepTool::new(state)),
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
            .grep("hello", Some("main"), GrepQueryMode::Plain, 10)
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
            .grep("shared", Some("main"), GrepQueryMode::Plain, 10)
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
}
