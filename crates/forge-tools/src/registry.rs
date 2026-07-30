use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use forge_types::{ToolDescriptor, ToolOutput};
use serde_json::Value;

use crate::validation::{validate_args, ValidationBudget};
use crate::{Tool, ToolError};

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace_root: PathBuf,
    pub principal: String,
}

impl ToolContext {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            principal: "local-dev".into(),
        }
    }

    /// Resolve a tool-supplied path for reading, confined to the workspace.
    pub fn resolve_path(&self, rel: &str) -> Result<PathBuf, ToolError> {
        self.resolve(rel, PathAccess::Read)
    }

    /// Resolve a tool-supplied path for writing. Same confinement as
    /// [`Self::resolve_path`], and additionally refuses anything under `.git`,
    /// because Git takes executable behaviour from its own config and hook
    /// files — writing them is equivalent to running a command.
    pub fn resolve_write_path(&self, rel: &str) -> Result<PathBuf, ToolError> {
        self.resolve(rel, PathAccess::Write)
    }

    fn resolve(&self, rel: &str, access: PathAccess) -> Result<PathBuf, ToolError> {
        if rel.trim().is_empty() {
            return Err(ToolError::Execution("path must not be empty".into()));
        }
        let requested = Path::new(rel);
        let full = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.workspace_root.join(requested)
        };

        // Reject `..` structurally rather than comparing strings. A prefix
        // comparison passes for `<root>/../../etc/x`, which the kernel then
        // resolves outside the workspace at open time.
        if full.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(escapes_workspace(rel));
        }

        // Judge the `.git` denial on the portion below the workspace root, so a
        // workspace that itself lives inside a `.git` directory is not
        // misjudged. Falls back to the whole path when it is not below the
        // root, which fails closed.
        if access == PathAccess::Write {
            let below_root = full.strip_prefix(&self.workspace_root).unwrap_or(&full);
            if below_root
                .components()
                .any(|c| matches!(c, Component::Normal(name) if name == ".git"))
            {
                return Err(ToolError::Execution(format!(
                    "refusing to write `{rel}`: paths under `.git` are not writable by tools"
                )));
            }
        }

        let root = self
            .workspace_root
            .canonicalize()
            .map_err(|error| ToolError::Execution(format!("cannot resolve workspace: {error}")))?;

        // Existing target: resolve it completely. This follows symlinks, so a
        // link pointing outside the workspace is caught by the containment
        // check rather than honoured.
        if let Ok(canonical) = full.canonicalize() {
            if !canonical.starts_with(&root) {
                return Err(escapes_workspace(rel));
            }
            return Ok(canonical);
        }

        // Target does not exist yet, which is normal for a write. Verify the
        // nearest ancestor that does exist instead.
        //
        // The walk tests `symlink_metadata`, not `exists()`. `exists()` follows
        // links and so reports false for a *dangling* one, which stepped the
        // walk past the link and left its target unchecked — a link committed in
        // a repository could then redirect a write anywhere on disk.
        let mut ancestor = full.as_path();
        while ancestor.symlink_metadata().is_err() {
            ancestor = ancestor.parent().ok_or_else(|| escapes_workspace(rel))?;
        }

        // `canonicalize` resolves links, so an ancestor pointing outside the
        // workspace is caught by the containment check below and one pointing
        // inside still works. A dangling link cannot be resolved at all, so it
        // cannot be shown to be contained, and is refused rather than trusted.
        let canonical = ancestor.canonicalize().map_err(|_| {
            ToolError::Execution(format!("path `{rel}` resolves through a broken symlink"))
        })?;
        if !canonical.starts_with(&root) {
            return Err(escapes_workspace(rel));
        }
        Ok(full)
    }
}

/// Whether a resolved path is about to be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathAccess {
    Read,
    Write,
}

fn escapes_workspace(rel: &str) -> ToolError {
    ToolError::Execution(format!("path `{rel}` escapes workspace"))
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn list_descriptors(&self) -> Vec<ToolDescriptor> {
        let mut v: Vec<_> = self.tools.values().map(|t| t.descriptor()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<_> = self.tools.keys().cloned().collect();
        n.sort();
        n
    }

    /// Validate then execute. Never calls handler on validation failure.
    pub async fn call(
        &self,
        ctx: &ToolContext,
        name: &str,
        args: Value,
        budget: &mut ValidationBudget,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;

        if let Err(ve) = validate_args(name, &tool.input_schema(), &args) {
            let signature =
                crate::validation::validation_error_signature(name, &ve.path, &ve.message);
            budget
                .record_failure_with_signature(name, Some(&signature))
                .map_err(ToolError::Execution)?;
            return Err(ToolError::Validation(ve));
        }

        tool.call(ctx, args).await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::ReadFileTool;
    use crate::ValidationBudget;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn unknown_tool() {
        let reg = ToolRegistry::new();
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let mut b = ValidationBudget::with_default_max();
        let err = reg.call(&ctx, "nope", json!({}), &mut b).await.unwrap_err();
        assert!(matches!(err, ToolError::Unknown(_)));
    }

    #[tokio::test]
    async fn validation_blocks_side_effects() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("x.txt");
        std::fs::write(&path, "hello").unwrap();

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let mut b = ValidationBudget::with_default_max();
        let err = reg
            .call(&ctx, "read_file", json!({"path": 1}), &mut b)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn read_file_ok() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let mut b = ValidationBudget::with_default_max();
        let out = reg
            .call(&ctx, "read_file", json!({"path": "a.txt"}), &mut b)
            .await
            .unwrap();
        assert_eq!(out.content, "hi");
        assert!(!out.is_error);
    }

    #[test]
    fn tool_context_resolves_relative_absolute_and_missing_paths() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hi").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        assert_eq!(
            ctx.resolve_path("a.txt").unwrap(),
            file.canonicalize().unwrap()
        );
        assert_eq!(
            ctx.resolve_path(file.to_str().unwrap())
                .unwrap()
                .canonicalize()
                .unwrap(),
            file.canonicalize().unwrap()
        );

        let missing = ctx.resolve_path("new.txt").unwrap();
        assert_eq!(missing, dir.path().join("new.txt"));
    }

    #[test]
    fn tool_context_rejects_canonical_escape() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "no").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        let err = ctx
            .resolve_path(outside_file.to_str().unwrap())
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Execution(message) if message.contains("escapes workspace"))
        );
    }

    /// Absolute, outside the workspace, and not yet existing. The previous
    /// containment check only ran when the path contained `..`, so this was
    /// returned unchecked and the file was created on write.
    #[test]
    fn resolve_path_rejects_absent_target_outside_workspace() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let target = outside.path().join("created.txt");

        let err = ctx.resolve_path(target.to_str().unwrap()).unwrap_err();
        assert!(
            matches!(err, ToolError::Execution(message) if message.contains("escapes workspace"))
        );
        assert!(!target.exists());
    }

    /// `..` used to be compared as a string prefix, which passes for
    /// `<root>/../../x` because that literally starts with `<root>`.
    #[test]
    fn resolve_path_rejects_parent_traversal_to_absent_target() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let ctx = ToolContext::new(workspace);

        for rel in [
            "../escaped.txt",
            "../../../../escaped.txt",
            "nested/../../escaped.txt",
        ] {
            let err = ctx.resolve_path(rel).unwrap_err();
            assert!(
                matches!(err, ToolError::Execution(message) if message.contains("escapes workspace")),
                "expected `{rel}` to be rejected"
            );
        }
    }

    #[test]
    fn resolve_path_allows_absent_nested_target_inside_workspace() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        assert_eq!(
            ctx.resolve_path("nested/deeper/new.txt").unwrap(),
            dir.path().join("nested/deeper/new.txt")
        );
    }

    #[test]
    fn resolve_path_rejects_empty_input() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        assert!(ctx.resolve_path("").is_err());
        assert!(ctx.resolve_path("   ").is_err());
    }

    /// The argument here is neither absolute nor contains `..`, so nothing about
    /// it looks suspect — the escape comes from a link that repository content
    /// can legitimately ship. `exists()` follows links and reports false for a
    /// dangling one, which stepped the ancestor walk past it.
    #[cfg(unix)]
    #[test]
    fn resolve_path_rejects_dangling_symlink_target() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let escape_target = outside.join("absent.txt");
        std::os::unix::fs::symlink(&escape_target, workspace.join("innocent")).unwrap();
        let ctx = ToolContext::new(workspace);

        let err = ctx.resolve_path("innocent").unwrap_err();
        assert!(matches!(err, ToolError::Execution(message) if message.contains("symlink")));
        assert!(!escape_target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_path_rejects_symlink_to_existing_file_outside_workspace() {
        let dir = tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "no").unwrap();
        std::os::unix::fs::symlink(&secret, workspace.join("link.txt")).unwrap();
        let ctx = ToolContext::new(workspace);

        let err = ctx.resolve_path("link.txt").unwrap_err();
        assert!(
            matches!(err, ToolError::Execution(message) if message.contains("escapes workspace"))
        );
    }

    /// Confinement is about where a path lands, not about symlinks being
    /// inherently suspect: a link to a file inside the workspace still resolves.
    #[cfg(unix)]
    #[test]
    fn resolve_path_still_follows_symlink_inside_workspace() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let real = dir.path().join("real.txt");
        std::fs::write(&real, "hi").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link.txt")).unwrap();

        assert_eq!(
            ctx.resolve_path("link.txt").unwrap(),
            real.canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_write_path_refuses_git_directory() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]\n").unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        for rel in [
            ".git/config",
            ".git/hooks/pre-commit",
            ".git/info/attributes",
            "nested/.git/config",
        ] {
            let err = ctx.resolve_write_path(rel).unwrap_err();
            assert!(
                matches!(err, ToolError::Execution(message) if message.contains(".git")),
                "expected `{rel}` to be refused for writing"
            );
        }

        // Reads are deliberately unaffected: the risk is writing configuration
        // that Git later executes, not inspecting it.
        assert!(ctx.resolve_path(".git/config").is_ok());
    }

    #[test]
    fn resolve_write_path_allows_ordinary_paths() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());

        assert_eq!(
            ctx.resolve_write_path("src/main.rs").unwrap(),
            dir.path().join("src/main.rs")
        );
        // Matching is per path component, so these are not `.git`.
        for rel in [".gitignore", ".gitattributes", "docs/.gitkeep"] {
            assert!(
                ctx.resolve_write_path(rel).is_ok(),
                "expected `{rel}` to remain writable"
            );
        }
    }

    #[test]
    fn registry_descriptors_and_names_are_sorted() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        assert_eq!(reg.names(), vec!["read_file"]);
        let descriptors = reg.list_descriptors();
        assert_eq!(descriptors[0].name, "read_file");
    }
}
