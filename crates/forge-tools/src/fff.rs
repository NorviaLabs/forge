use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

pub(crate) struct FffState;

impl FffState {
    pub(crate) fn new() -> Self {
        Self
    }
}

pub struct FffFindTool {
    _state: Arc<FffState>,
}

impl FffFindTool {
    pub(crate) fn new(state: Arc<FffState>) -> Self {
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
        let files = collect_files(&ctx.workspace_root)?;
        let query = args.query.to_lowercase();
        let mut matches = files
            .into_iter()
            .filter(|path| path.to_string_lossy().to_lowercase().contains(&query))
            .take(args.max_results as usize)
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        matches.sort();

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
    _state: Arc<FffState>,
}

impl FffGrepTool {
    pub(crate) fn new(state: Arc<FffState>) -> Self {
        Self { _state: state }
    }
}

#[async_trait]
impl Tool for FffGrepTool {
    fn name(&self) -> &str {
        "ffgrep"
    }
    fn description(&self) -> &str {
        "Search file contents in the workspace. Supports plain text matching."
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
        let query = args.pattern.to_lowercase();
        let path_filter = args.path.as_deref().map(str::to_lowercase);
        let mut rows = Vec::new();

        for path in collect_files(&ctx.workspace_root)? {
            if rows.len() >= args.max_results as usize {
                break;
            }
            let rel = path.to_string_lossy();
            if let Some(filter) = &path_filter {
                if !rel.to_lowercase().contains(filter) {
                    continue;
                }
            }
            let full_path = ctx.workspace_root.join(&path);
            let Ok(content) = std::fs::read_to_string(full_path) else {
                continue;
            };
            for (index, line) in content.lines().enumerate() {
                if line.to_lowercase().contains(&query) {
                    rows.push(format!("{}:{}:{}", rel, index + 1, line.trim()));
                    if rows.len() >= args.max_results as usize {
                        break;
                    }
                }
            }
        }

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

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, ToolError> {
    let mut files = Vec::new();
    collect_files_inner(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files_inner(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), ToolError> {
    for entry in std::fs::read_dir(dir).map_err(|e| ToolError::Execution(format!("fff read: {e}")))? {
        let entry = entry.map_err(|e| ToolError::Execution(format!("fff read: {e}")))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == "target" || name == "node_modules" || name == ".forge" {
            continue;
        }
        if path.is_dir() {
            collect_files_inner(root, &path, files)?;
        } else if path.is_file() {
            if let Ok(relative) = path.strip_prefix(root) {
                files.push(relative.to_path_buf());
            }
        }
    }
    Ok(())
}

pub fn fff_tools() -> Vec<Arc<dyn Tool>> {
    let state = Arc::new(FffState::new());
    vec![
        Arc::new(FffFindTool::new(state.clone())),
        Arc::new(FffGrepTool::new(state)),
    ]
}
