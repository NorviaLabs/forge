//! Structural refactoring tool using tree-sitter.
//!
//! Provides query-based code analysis and transformation via tree-sitter queries.

use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::registry::ToolContext;
use crate::{Tool, ToolError};

pub struct RefactorTool;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RefactorArgs {
    /// File path (relative to workspace).
    pub path: String,
    /// Tree-sitter query pattern (e.g., "(function_item) @fn").
    pub query: String,
    /// Operation type: extract | rename | delete | replace | wrap
    #[serde(default)]
    pub operation: Option<String>,
    /// For rename: new name
    #[serde(default)]
    pub new_name: Option<String>,
    /// For wrap: text before
    #[serde(default)]
    pub before: Option<String>,
    /// For wrap: text after
    #[serde(default)]
    pub after: Option<String>,
    /// For replace: replacement template
    #[serde(default)]
    pub replacement: Option<String>,
    /// Language override (auto-detected from file extension if not provided)
    #[serde(default)]
    pub language: Option<String>,
}

#[async_trait]
impl Tool for RefactorTool {
    fn name(&self) -> &str {
        "refactor"
    }

    fn description(&self) -> &str {
        "Structural refactoring via tree-sitter queries"
    }

    fn input_schema(&self) -> Value {
        schema_for::<RefactorArgs>()
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Write
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: RefactorArgs =
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?;

        let path = ctx.resolve_path(&a.path)?;
        let code = tokio::fs::read_to_string(&path).await?;

        // Detect language from file extension or override
        let lang = a
            .language
            .unwrap_or_else(|| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string()
            });

        // For now, just do query extraction
        // Full refactoring would modify the file
        match a.operation.as_deref() {
            Some("query") | None => {
                use forge_syntax::query_code;
                let captures = query_code(&lang, &code, &a.query)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;

                let result: Value = json!({
                    "path": a.path,
                    "language": lang,
                    "query": a.query,
                    "matches": captures.len(),
                    "captures": captures,
                });

                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&result).unwrap_or_default(),
                    is_error: false,
                })
            }
            Some("rename") => {
                if let Some(new_name) = a.new_name {
                    let op = forge_syntax::RefactorOp {
                        op_type: forge_syntax::RefactorType::Rename {
                            old_name: "".to_string(),
                            new_name: new_name.clone(),
                        },
                        query: a.query,
                        replacement: Some(new_name),
                    };

                    let result = forge_syntax::refactor(&lang, &code, &op)
                        .map_err(|e| ToolError::Execution(e.to_string()))?;

                    // Write changes back
                    tokio::fs::write(&path, &result.code).await?;

                    Ok(ToolOutput {
                        content: format!(
                            "Renamed {} occurrences in {}:\n{}",
                            result.changes,
                            a.path,
                            serde_json::to_string_pretty(&result.captures).unwrap_or_default()
                        ),
                        is_error: false,
                    })
                } else {
                    Err(ToolError::Execution("new_name required for rename".into()))
                }
            }
            Some(op) => Err(ToolError::Execution(format!(
                "unknown operation: {op} (supported: query, rename)"
            ))),
        }
    }
}

fn schema_for<T: JsonSchema>() -> Value {
    let s = schemars::schema_for!(T);
    serde_json::to_value(s).unwrap_or_else(|_| json!({"type": "object"}))
}
