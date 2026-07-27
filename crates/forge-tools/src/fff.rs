use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
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

pub(crate) struct FffState {
    picker: fff_search::SharedPicker,
    inited: AtomicBool,
    first_call: AtomicBool,
}

impl FffState {
    pub(crate) fn new() -> Self {
        Self {
            picker: fff_search::SharedPicker::default(),
            inited: AtomicBool::new(false),
            first_call: AtomicBool::new(true),
        }
    }

    fn scan_status(&self) -> Option<String> {
        if !self.inited.load(Ordering::Acquire) {
            return None;
        }
        let guard = self.picker.read().ok()?;
        let picker = guard.as_ref()?;
        let progress = picker.get_scan_progress();
        let mut parts = vec![format!(
            "fff: {} files indexed",
            progress.scanned_files_count
        )];
        if progress.is_scanning {
            parts.push("scanning...".into());
        }
        if !progress.is_warmup_complete {
            parts.push("building index...".into());
        }
        if progress.is_watcher_ready {
            parts.push("watching for changes".into());
        }
        if !progress.is_scanning && progress.is_warmup_complete {
            parts.push("ready".into());
        }
        Some(parts.join(", "))
    }

    fn mark_first_call(&self) -> bool {
        self.first_call.swap(false, Ordering::Release)
    }

    async fn ensure_init(&self, base_path: &std::path::Path) -> Result<(), ToolError> {
        if self.inited.load(Ordering::Acquire) {
            return Ok(());
        }
        let p = self.picker.clone();
        let bp = base_path.to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || {
            fff_search::file_picker::FilePicker::new_with_shared_state(
                p,
                fff_search::SharedFrecency::default(),
                fff_search::file_picker::FilePickerOptions {
                    base_path: bp,
                    mode: fff_search::file_picker::FFFMode::Ai,
                    watch: true,
                    warmup_mmap_cache: true,
                    ..Default::default()
                },
            )
        })
        .await
        .map_err(|e| ToolError::Execution(format!("fff spawn: {e}")))?
        .map_err(|e| ToolError::Execution(format!("fff init: {e}")))?;

        let p = self.picker.clone();
        let ok = tokio::task::spawn_blocking(move || p.wait_for_scan(Duration::from_secs(30)))
            .await
            .map_err(|e| ToolError::Execution(format!("fff wait: {e}")))?;
        if !ok {
            return Err(ToolError::Execution("fff scan timed out after 30s".into()));
        }
        self.inited.store(true, Ordering::Release);
        Ok(())
    }
}

pub struct FffFindTool {
    state: Arc<FffState>,
}

impl FffFindTool {
    pub(crate) fn new(state: Arc<FffState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for FffFindTool {
    fn name(&self) -> &str {
        "fffind"
    }
    fn description(&self) -> &str {
        "Find files in the workspace by path/name pattern. Fast, typo-resistant, frecency-ranked. Supports glob and constraints."
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
        let a: FffFindArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::Execution(format!("fff args: {e}")))?;
        self.state.ensure_init(&ctx.workspace_root).await?;

        let query = fff_search::QueryParser::default().parse(&a.query);
        let picker_guard = self
            .state
            .picker
            .read()
            .map_err(|e| ToolError::Execution(format!("fff lock: {e}")))?;
        let picker = picker_guard
            .as_ref()
            .ok_or_else(|| ToolError::Execution("fff not initialized".into()))?;

        let results = fff_search::file_picker::FilePicker::fuzzy_search(
            picker.get_files(),
            &query,
            None::<&fff_search::query_tracker::QueryTracker>,
            fff_search::file_picker::FuzzySearchOptions {
                pagination: fff_search::types::PaginationArgs {
                    offset: 0,
                    limit: a.max_results as usize,
                },
                current_file: None,
                ..Default::default()
            },
        );

        let content = if results.items.is_empty() {
            "no matches found".to_string()
        } else {
            results
                .items
                .iter()
                .map(|item| item.relative_path.clone())
                .collect::<Vec<_>>()
                .join("\n")
        };

        let status = if self.state.mark_first_call() {
            self.state.scan_status()
        } else {
            None
        };
        let content = match status {
            Some(s) => format!("{s}\n{content}"),
            None => content,
        };

        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

pub struct FffGrepTool {
    state: Arc<FffState>,
}

impl FffGrepTool {
    pub(crate) fn new(state: Arc<FffState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for FffGrepTool {
    fn name(&self) -> &str {
        "ffgrep"
    }
    fn description(&self) -> &str {
        "Search file contents in the workspace. Supports plain text, regex, and fuzzy modes."
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
        let a: FffGrepArgs = serde_json::from_value(args)
            .map_err(|e| ToolError::Execution(format!("fff args: {e}")))?;
        self.state.ensure_init(&ctx.workspace_root).await?;

        let grep_mode = match a.mode {
            None | Some(FffModeArg::Plain) => fff_search::grep::GrepMode::PlainText,
            Some(FffModeArg::Regex) => fff_search::grep::GrepMode::Regex,
            Some(FffModeArg::Fuzzy) => fff_search::grep::GrepMode::Fuzzy,
        };
        let grep_opts = fff_search::grep::GrepSearchOptions {
            max_file_size: 10 * 1024 * 1024,
            max_matches_per_file: 0,
            mode: grep_mode,
            file_offset: 0,
            page_limit: a.max_results as usize,
            smart_case: true,
            time_budget_ms: 5000,
            before_context: 0,
            after_context: 0,
            classify_definitions: false,
        };

        let picker_guard = self
            .state
            .picker
            .read()
            .map_err(|e| ToolError::Execution(format!("fff lock: {e}")))?;
        let picker = picker_guard
            .as_ref()
            .ok_or_else(|| ToolError::Execution("fff not initialized".into()))?;

        let query = fff_search::QueryParser::default().parse(&a.pattern);
        let results = picker.grep(&query, &grep_opts);

        let content = if results.matches.is_empty() {
            "no matches found".to_string()
        } else {
            results
                .matches
                .iter()
                .map(|m| {
                    let path = results
                        .files
                        .get(m.file_index)
                        .map(|f| f.relative_path.clone())
                        .unwrap_or_default();
                    format!("{}:{}:{}", path, m.line_number, m.line_content)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let status = if self.state.mark_first_call() {
            self.state.scan_status()
        } else {
            None
        };
        let content = match status {
            Some(s) => format!("{s}\n{content}"),
            None => content,
        };

        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

pub fn fff_tools() -> Vec<Arc<dyn Tool>> {
    let state = Arc::new(FffState::new());
    vec![
        Arc::new(FffFindTool::new(state.clone())),
        Arc::new(FffGrepTool::new(state)),
    ]
}
