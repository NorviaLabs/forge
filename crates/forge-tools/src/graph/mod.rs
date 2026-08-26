//! `find_definition`/`find_references` — narrow, purpose-built tools over
//! `forge-graph`'s persisted symbol/edge store, matching how every other
//! forge tool is scoped (`grep`, `find`, `read_file`), not a graph-query
//! DSL. Conditionally registered, mirroring `web_search`'s pattern exactly
//! (`crate::web_search::web_search_tool` / `default_builtins_with_web_search`
//! in `builtins.rs`): no handle, no tools.
//!
//! v1 scope: `find_references` only surfaces call-site references (the
//! `calls` edge kind) — a reference to a *type* used as a field/parameter/
//! return type (as opposed to a `use` import or an `impl ... for`) isn't
//! tracked as an edge in v1, so `find_references` on a struct/enum/trait
//! name returns nothing today, not "no references exist." Documented, not
//! silently pretended comprehensive.

use std::sync::Arc;

use async_trait::async_trait;
use forge_graph::GraphHandle;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::ToolContext;
use crate::{Tool, ToolError};

fn schema_for<T: JsonSchema>() -> Value {
    let s = schemars::schema_for!(T);
    serde_json::to_value(s).unwrap_or_else(|_| serde_json::json!({"type": "object"}))
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindDefinitionArgs {
    /// Symbol name to look up (bare identifier, e.g. "AgentSession" or "process").
    pub symbol: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindReferencesArgs {
    /// Symbol name whose call sites to find (bare identifier).
    pub symbol: String,
}

/// Formats zero, one, or many `SymbolMatch`es the same way for both tools'
/// "no result" / "one confident answer" / "honest candidate set" cases.
fn format_definitions(symbol: &str, matches: &[forge_graph::SymbolMatch]) -> String {
    match matches {
        [] => format!("no definition found for \"{symbol}\" in the indexed graph"),
        [one] => format!(
            "{} — {}, defined at {}:{}",
            one.name, one.kind, one.file, one.line
        ),
        many => {
            let mut out = format!(
                "{} candidates for \"{symbol}\", cannot disambiguate without type information:\n",
                many.len()
            );
            for m in many {
                out.push_str(&format!("  - {}  {}:{}\n", m.name, m.file, m.line));
            }
            out.trim_end().to_string()
        }
    }
}

fn format_references(symbol: &str, matches: &[forge_graph::ReferenceMatch]) -> String {
    if matches.is_empty() {
        return format!(
            "no call sites found for \"{symbol}\" in the indexed graph \
             (note: find_references only tracks function/method calls, not type usages)"
        );
    }
    let mut out = format!("{} call site(s) for \"{symbol}\":\n", matches.len());
    for r in matches {
        let note = if r.ambiguous_at_callsite {
            " [ambiguous: this call site also matches other candidates]"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {}:{} -> {} ({}:{}){}\n",
            r.from_file, r.from_line, r.resolved.name, r.resolved.file, r.resolved.line, note
        ));
    }
    out.trim_end().to_string()
}

pub struct FindDefinitionTool {
    graph: Arc<GraphHandle>,
}

#[async_trait]
impl Tool for FindDefinitionTool {
    fn name(&self) -> &str {
        "find_definition"
    }
    fn description(&self) -> &str {
        "Find where a symbol (function, method, type) is defined in the repo. Syntactic only — \
         if the name is genuinely ambiguous (multiple unrelated definitions share it), every \
         real candidate is returned rather than a guess."
    }
    fn input_schema(&self) -> Value {
        schema_for::<FindDefinitionArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn idempotent(&self) -> bool {
        true
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: FindDefinitionArgs =
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?;
        let matches = self
            .graph
            .find_definition(&args.symbol)
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        Ok(ToolOutput::success(format_definitions(
            &args.symbol,
            &matches,
        )))
    }
}

pub struct FindReferencesTool {
    graph: Arc<GraphHandle>,
}

#[async_trait]
impl Tool for FindReferencesTool {
    fn name(&self) -> &str {
        "find_references"
    }
    fn description(&self) -> &str {
        "Find call sites that reference a function or method by name. Syntactic only — a call \
         site that could match more than one same-named definition is flagged ambiguous rather \
         than silently attributed to one."
    }
    fn input_schema(&self) -> Value {
        schema_for::<FindReferencesArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    fn idempotent(&self) -> bool {
        true
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: FindReferencesArgs =
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?;
        let matches = self
            .graph
            .find_references(&args.symbol)
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        Ok(ToolOutput::success(format_references(
            &args.symbol,
            &matches,
        )))
    }
}

/// `None` when the graph isn't enabled/available — no tools registered, so
/// the model never sees them (mirrors `web_search_tool`'s `Option` shape).
pub fn graph_tools(handle: Option<Arc<GraphHandle>>) -> Vec<Arc<dyn Tool>> {
    match handle {
        Some(h) => vec![
            Arc::new(FindDefinitionTool { graph: h.clone() }),
            Arc::new(FindReferencesTool { graph: h }),
        ],
        None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_graph::{GraphHandle, SymbolMatch};
    use tempfile::tempdir;

    async fn fixture_handle() -> Arc<GraphHandle> {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() {}\n\npub fn entry() {\n    helper();\n}\n\n\
             pub struct Foo;\nimpl Foo {\n    pub fn process(&self) {}\n}\n\
             pub struct Bar;\nimpl Bar {\n    pub fn process(&self) {}\n}\n",
        )
        .unwrap();
        Arc::new(GraphHandle::open_in_memory(dir.path()).await.unwrap())
    }

    #[test]
    fn formats_a_unique_definition() {
        let out = format_definitions(
            "AgentSession",
            &[SymbolMatch {
                name: "AgentSession".into(),
                kind: "type".into(),
                file: "crates/forge-core/src/lib.rs".into(),
                line: 84,
            }],
        );
        assert_eq!(
            out,
            "AgentSession — type, defined at crates/forge-core/src/lib.rs:84"
        );
    }

    #[test]
    fn formats_no_match() {
        let out = format_definitions("Bogus", &[]);
        assert_eq!(
            out,
            "no definition found for \"Bogus\" in the indexed graph"
        );
    }

    #[test]
    fn formats_ambiguous_candidates_honestly_not_as_a_guess() {
        let out = format_definitions(
            "process",
            &[
                SymbolMatch {
                    name: "process".into(),
                    kind: "method".into(),
                    file: "a.rs".into(),
                    line: 1,
                },
                SymbolMatch {
                    name: "process".into(),
                    kind: "method".into(),
                    file: "b.rs".into(),
                    line: 2,
                },
            ],
        );
        assert!(out.starts_with("2 candidates for \"process\", cannot disambiguate"));
        assert!(out.contains("a.rs:1"));
        assert!(out.contains("b.rs:2"));
    }

    #[tokio::test]
    async fn find_definition_tool_end_to_end_unique_match() {
        let tool = FindDefinitionTool {
            graph: fixture_handle().await,
        };
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let out = tool
            .call(&ctx, serde_json::json!({"symbol": "helper"}))
            .await
            .unwrap();
        assert!(out.content.contains("src/lib.rs:1"), "{}", out.content);
    }

    #[tokio::test]
    async fn find_definition_tool_end_to_end_ambiguous_match() {
        let tool = FindDefinitionTool {
            graph: fixture_handle().await,
        };
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let out = tool
            .call(&ctx, serde_json::json!({"symbol": "process"}))
            .await
            .unwrap();
        assert!(out.content.starts_with("2 candidates"), "{}", out.content);
    }

    #[tokio::test]
    async fn find_references_tool_end_to_end() {
        let tool = FindReferencesTool {
            graph: fixture_handle().await,
        };
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let out = tool
            .call(&ctx, serde_json::json!({"symbol": "helper"}))
            .await
            .unwrap();
        assert!(out.content.contains("1 call site"), "{}", out.content);
        assert!(out.content.contains("src/lib.rs:4"), "{}", out.content);
    }

    #[test]
    fn graph_tools_registers_nothing_when_disabled() {
        assert!(graph_tools(None).is_empty());
    }

    #[tokio::test]
    async fn graph_tools_registers_both_tools_when_enabled() {
        let tools = graph_tools(Some(fixture_handle().await));
        let names: Vec<_> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["find_definition", "find_references"]);
    }
}
