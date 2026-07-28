use async_trait::async_trait;
use fff_search::{
    file_picker::{FFFMode, FilePicker, FilePickerOptions, FuzzySearchOptions},
    grep::{parse_grep_query, GrepMode, GrepSearchOptions},
    PaginationArgs, QueryParser, SharedFilePicker, SharedFrecency,
};
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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
pub enum FffModeArg {
    #[serde(rename = "plain")]
    Plain,
    #[serde(rename = "regex")]
    Regex,
    #[serde(rename = "fuzzy")]
    Fuzzy,
}

pub(crate) struct FastFileState;

impl FastFileState {
    pub(crate) fn new() -> Self {
        Self
    }
}

pub struct FffFindTool {
    _state: Arc<FastFileState>,
}

impl FffFindTool {
    pub(crate) fn new(state: Arc<FastFileState>) -> Self {
        Self { _state: state }
    }
}

#[async_trait]
impl Tool for FffFindTool {
    fn name(&self) -> &str {
        "fffind"
    }
    fn description(&self) -> &str {
        "Find files in the workspace by path/name pattern."
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
        let matches = find_with_fff(&ctx.workspace_root, &args.query, args.max_results as usize)?;

        Ok(ToolOutput {
            content: if matches.is_empty() {
                "no matches found".into()
            } else {
                matches.join("\n")
            },
            is_error: false,
        })
    }
}

pub struct FffGrepTool {
    _state: Arc<FastFileState>,
}

impl FffGrepTool {
    pub(crate) fn new(state: Arc<FastFileState>) -> Self {
        Self { _state: state }
    }
}

#[async_trait]
impl Tool for FffGrepTool {
    fn name(&self) -> &str {
        "ffgrep"
    }
    fn description(&self) -> &str {
        "Search file contents in the workspace. Supports plain text, regex, and fuzzy matching."
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
        let rows = grep_workspace(
            &ctx.workspace_root,
            args.path.as_deref(),
            &args.pattern,
            args.mode.as_ref(),
            args.max_results as usize,
        )?;

        Ok(ToolOutput {
            content: if rows.is_empty() {
                "no matches found".into()
            } else {
                rows.join("\n")
            },
            is_error: false,
        })
    }
}

fn find_with_fff(root: &Path, query: &str, max_results: usize) -> Result<Vec<String>, ToolError> {
    if max_results == 0 {
        return Ok(Vec::new());
    }

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();
    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency,
        FilePickerOptions {
            base_path: root.display().to_string(),
            mode: FFFMode::Ai,
            watch: false,
            ..Default::default()
        },
    )
    .map_err(|e| ToolError::Execution(format!("fff init: {e}")))?;

    if !shared_picker.wait_for_scan(Duration::from_secs(10)) {
        return Err(ToolError::Execution("fff scan timed out".into()));
    }

    let picker_guard = shared_picker
        .read()
        .map_err(|e| ToolError::Execution(format!("fff lock: {e}")))?;
    let picker = picker_guard
        .as_ref()
        .ok_or_else(|| ToolError::Execution("fff picker missing".into()))?;

    let parser = QueryParser::default();
    let parsed = parser.parse(query.trim());
    let results = picker.fuzzy_search(
        &parsed,
        None,
        FuzzySearchOptions {
            max_threads: 0,
            current_file: None,
            project_path: Some(root),
            pagination: PaginationArgs {
                offset: 0,
                limit: max_results,
            },
            ..Default::default()
        },
    );

    Ok(results
        .items
        .into_iter()
        .map(|item| item.relative_path(picker))
        .take(max_results)
        .collect())
}

fn grep_workspace(
    root: &Path,
    path_filter: Option<&str>,
    pattern: &str,
    mode: Option<&FffModeArg>,
    max_results: usize,
) -> Result<Vec<String>, ToolError> {
    if max_results == 0 || pattern.trim().is_empty() {
        return Ok(Vec::new());
    }
    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();
    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency,
        FilePickerOptions {
            base_path: root.display().to_string(),
            mode: FFFMode::Ai,
            watch: false,
            ..Default::default()
        },
    )
    .map_err(|e| ToolError::Execution(format!("fff init: {e}")))?;

    if !shared_picker.wait_for_scan(Duration::from_secs(10)) {
        return Err(ToolError::Execution("fff scan timed out".into()));
    }

    let picker_guard = shared_picker
        .read()
        .map_err(|e| ToolError::Execution(format!("fff lock: {e}")))?;
    let picker = picker_guard
        .as_ref()
        .ok_or_else(|| ToolError::Execution("fff picker missing".into()))?;

    let parsed = parse_grep_query(pattern);
    let result = picker.grep(
        &parsed,
        &GrepSearchOptions {
            mode: grep_mode(pattern, mode),
            page_limit: max_results,
            ..Default::default()
        },
    );

    let filter = path_filter.map(|value| value.to_ascii_lowercase());
    let mut rows = Vec::with_capacity(result.matches.len().min(max_results));
    for entry in result.matches.into_iter().take(max_results) {
        let rel = result.files[entry.file_index].relative_path(picker);
        if let Some(filter) = &filter {
            if !rel.to_ascii_lowercase().contains(filter) {
                continue;
            }
        }
        rows.push(format!(
            "{}:{}:{}",
            rel,
            entry.line_number,
            entry.line_content.trim()
        ));
    }
    Ok(rows)
}

fn grep_mode(pattern: &str, mode: Option<&FffModeArg>) -> GrepMode {
    match mode.unwrap_or(&FffModeArg::Plain) {
        FffModeArg::Plain => {
            if parse_regex_literal(pattern).is_some() {
                GrepMode::Regex
            } else {
                GrepMode::PlainText
            }
        }
        FffModeArg::Regex => GrepMode::Regex,
        FffModeArg::Fuzzy => GrepMode::Fuzzy,
    }
}

fn parse_regex_literal(pattern: &str) -> Option<&str> {
    if pattern.len() >= 2 && pattern.starts_with('/') && pattern.ends_with('/') {
        Some(&pattern[1..pattern.len() - 1])
    } else {
        None
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
    use tempfile::tempdir;

    #[test]
    fn plain_mode_auto_detects_regex_literal() {
        assert_eq!(grep_mode("/foo.*/", Some(&FffModeArg::Plain)), GrepMode::Regex);
    }

    #[test]
    fn find_uses_fff_index() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();

        let matches = find_with_fff(dir.path(), "main.rs", 10).unwrap();
        assert_eq!(matches.first().map(String::as_str), Some("src/main.rs"));
    }

    #[test]
    fn grep_filters_results() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "hello world\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "goodbye\n").unwrap();

        let rows = grep_workspace(
            dir.path(),
            Some("main"),
            "hello",
            Some(&FffModeArg::Plain),
            10,
        )
        .unwrap();
        assert_eq!(rows, vec!["src/main.rs:1:hello world"]);
    }
}
