//! Activation and execution stages of the Agent Skills open standard
//! (agentskills.io) — the discovery stage (name + description in the system
//! prompt) lives in `forge-core`'s `assemble_system_prompt`, backed by
//! `forge_context::discover_skills`.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use forge_types::SideEffectClass;
use forge_types::ToolOutput;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::builtins::schema_for;
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LoadSkillArgs {
    /// Skill name, exactly as listed under "# Skills" in the system prompt.
    pub name: String,
    /// Path to a bundled file inside the skill's own directory (e.g.
    /// "references/style.md", "scripts/setup.sh"), relative to that
    /// directory. Omit to load the skill's own SKILL.md instructions.
    #[serde(default)]
    pub path: Option<String>,
}

/// Loads a skill's full `SKILL.md` instructions (Activation stage), or a
/// bundled `references/`, `scripts/`, `assets/` file within it (Execution
/// stage). Only the skill's `name`/`description` sit in every system prompt;
/// everything else is fetched on demand through this tool.
pub struct LoadSkillTool;

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }

    fn description(&self) -> &str {
        "Load the full instructions for a skill listed under '# Skills' (by name), or read an \
auxiliary file bundled with it under references/, scripts/, or assets/. Call this once a \
skill's description matches the current task, before following its instructions."
    }

    fn input_schema(&self) -> Value {
        schema_for::<LoadSkillArgs>()
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

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: LoadSkillArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::Execution(format!("internal deserialize after validation: {e}"))
        })?;

        let manifests = forge_context::discover_skills(&ctx.workspace_root);
        let manifest = manifests
            .into_iter()
            .find(|m| m.name == a.name)
            .ok_or_else(|| ToolError::Execution(format!("no skill named `{}`", a.name)))?;

        let content = match a.path {
            None => format!(
                "Skill directory: {}\n\n{}",
                manifest.dir.display(),
                manifest.body
            ),
            Some(rel) => {
                let full = resolve_within_dir(&manifest.dir, &rel)?;
                tokio::fs::read_to_string(&full).await.map_err(|e| {
                    ToolError::Execution(format!("reading `{rel}` for skill `{}`: {e}", a.name))
                })?
            }
        };

        Ok(ToolOutput {
            outcome: Default::default(),
            content,
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

/// Confines `rel` to `dir` — the same treatment `ToolContext::resolve_path`
/// gives workspace paths, but scoped to one skill's own directory rather than
/// the workspace root, since a global skill (`~/.agents/skills/...`)
/// legitimately sits outside the workspace.
fn resolve_within_dir(dir: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    if rel.trim().is_empty() {
        return Err(ToolError::Execution("path must not be empty".into()));
    }
    let requested = Path::new(rel);
    if requested.is_absolute() {
        return Err(ToolError::Execution(format!(
            "path `{rel}` must be relative to the skill directory"
        )));
    }
    if requested
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(ToolError::Execution(format!(
            "path `{rel}` escapes the skill directory"
        )));
    }

    let root = dir.canonicalize().map_err(|e| {
        ToolError::Execution(format!(
            "cannot resolve skill directory {}: {e}",
            dir.display()
        ))
    })?;
    let full = root.join(requested);
    let canonical = full
        .canonicalize()
        .map_err(|e| ToolError::Execution(format!("path `{rel}` not found: {e}")))?;
    if !canonical.starts_with(&root) {
        return Err(ToolError::Execution(format!(
            "path `{rel}` escapes the skill directory"
        )));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn write_skill(root: &Path, name: &str, frontmatter_body: &str) {
        let dir = root.join(".agents/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), frontmatter_body).unwrap();
    }

    #[tokio::test]
    async fn loads_full_skill_body_by_name() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "reviewer",
            "---\nname: reviewer\ndescription: Reviews PRs.\n---\n\nFull instructions.",
        );
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = LoadSkillTool
            .call(&ctx, json!({"name": "reviewer"}))
            .await
            .unwrap();
        assert!(out.content.contains("Full instructions."));
        assert!(out.content.contains("Skill directory:"));
    }

    #[tokio::test]
    async fn unknown_skill_name_errors() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let err = LoadSkillTool
            .call(&ctx, json!({"name": "nope"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no skill named"));
    }

    #[tokio::test]
    async fn reads_bundled_reference_file() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "reviewer",
            "---\nname: reviewer\ndescription: Reviews PRs.\n---\n\nSee references/style.md.",
        );
        let refs = dir.path().join(".agents/skills/reviewer/references");
        std::fs::create_dir_all(&refs).unwrap();
        std::fs::write(refs.join("style.md"), "Use two spaces.").unwrap();

        let ctx = ToolContext::new(dir.path().to_path_buf());
        let out = LoadSkillTool
            .call(
                &ctx,
                json!({"name": "reviewer", "path": "references/style.md"}),
            )
            .await
            .unwrap();
        assert_eq!(out.content, "Use two spaces.");
    }

    #[tokio::test]
    async fn refuses_path_escaping_skill_directory() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "reviewer",
            "---\nname: reviewer\ndescription: Reviews PRs.\n---\n\nbody",
        );
        std::fs::write(dir.path().join("secret.txt"), "top secret").unwrap();

        let ctx = ToolContext::new(dir.path().to_path_buf());
        let err = LoadSkillTool
            .call(
                &ctx,
                json!({"name": "reviewer", "path": "../../secret.txt"}),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("escapes the skill directory"));
    }
}
