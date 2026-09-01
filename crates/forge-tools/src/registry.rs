use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use forge_types::{SideEffectClass, ToolCall, ToolDescriptor, ToolOutput, ToolValidationError};
use jsonschema::Validator;
use serde_json::Value;

use crate::validation::{validate_args_with, ValidationBudget};
use crate::{Tool, ToolError};

#[derive(Debug)]
pub struct SessionTempDir {
    dir: tempfile::TempDir,
}

impl SessionTempDir {
    pub fn create(session_id: impl std::fmt::Display) -> Result<Arc<Self>, ToolError> {
        let dir = tempfile::Builder::new()
            .prefix(&format!("forge-{}-", session_id))
            .tempdir_in(std::env::temp_dir())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Arc::new(Self { dir }))
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Resolve a model-supplied tool name to the registered built-in.
///
/// OpenCode advertises only `grep` and implements it with ripgrep. Models
/// still emit a tool named `rg`; that inbound name is accepted as `grep`
/// and is not a second advertised tool.
pub fn canonical_tool_name(name: &str) -> &str {
    match name {
        "rg" => "grep",
        "search_replace" | "edit_file" => "edit",
        other => other,
    }
}

/// Rewrite a call so synonym names execute as their canonical tool.
pub fn canonicalize_tool_call(mut call: ToolCall) -> ToolCall {
    let canonical = canonical_tool_name(&call.name);
    if canonical != call.name {
        call.name = canonical.to_string();
    }
    call
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace_root: PathBuf,
    pub principal: String,
    /// Fail-closed: `view_image` is allowed only when the active model
    /// advertises image input and the transport can serialize attachments.
    pub image_input: bool,
    /// Display id used in the capability-gate error (`provider/model`).
    pub active_model: String,
    /// Where confined commands may reach the network, if anywhere.
    ///
    /// `None` means the network is off, which is the default. A session that
    /// starts an egress proxy sets this, and every shell spawn picks it up —
    /// so there is one place that decides, rather than each tool deciding for
    /// itself.
    ///
    /// Behind an `Arc` because `ToolContext` is carried inside enum variants
    /// upstream, and growing it by an inline `PathBuf` pushes those over
    /// clippy's `large_enum_variant` threshold. One session shares one grant,
    /// so sharing is also the honest shape.
    pub egress: Option<std::sync::Arc<crate::sandbox::EgressGrant>>,
    /// Private writable scratch space outside the repository.
    pub session_tmp: Option<Arc<SessionTempDir>>,
    /// True only for an exact sandbox-denied invocation replayed after HITL.
    pub unconfined_shell: bool,
}

impl ToolContext {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            principal: "local-dev".into(),
            image_input: false,
            active_model: String::new(),
            // Default: no network. A session that starts a proxy overrides it.
            egress: None,
            session_tmp: None,
            unconfined_shell: false,
        }
    }

    pub fn with_session_tmp(mut self, session_tmp: Arc<SessionTempDir>) -> Self {
        self.session_tmp = Some(session_tmp);
        self
    }

    pub fn with_unconfined_shell(mut self) -> Self {
        self.unconfined_shell = true;
        self
    }

    /// Resolve a tool-supplied path for reading, confined to the workspace or
    /// this session's private temp directory.
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

    fn allowed_roots(&self) -> Result<Vec<PathBuf>, ToolError> {
        let workspace = self
            .workspace_root
            .canonicalize()
            .map_err(|error| ToolError::Execution(format!("cannot resolve workspace: {error}")))?;
        let mut roots = vec![workspace];
        if let Some(tmp) = &self.session_tmp {
            let tmp = tmp.path().canonicalize().map_err(|error| {
                ToolError::Execution(format!("cannot resolve session temp: {error}"))
            })?;
            roots.push(tmp);
        }
        Ok(roots)
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
            return Err(self.escapes_allowed(rel));
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

        let roots = self.allowed_roots()?;

        // Existing target: resolve it completely. This follows symlinks, so a
        // link pointing outside the allowed roots is caught by the containment
        // check rather than honoured.
        if let Ok(canonical) = full.canonicalize() {
            if !contained_in(&canonical, &roots) {
                return Err(self.escapes_allowed(rel));
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
            ancestor = ancestor.parent().ok_or_else(|| self.escapes_allowed(rel))?;
        }

        // `canonicalize` resolves links, so an ancestor pointing outside the
        // allowed roots is caught by the containment check below and one
        // pointing inside still works. A dangling link cannot be resolved at
        // all, so it cannot be shown to be contained, and is refused rather
        // than trusted.
        let canonical = ancestor.canonicalize().map_err(|_| {
            ToolError::Execution(format!("path `{rel}` resolves through a broken symlink"))
        })?;
        if !contained_in(&canonical, &roots) {
            return Err(self.escapes_allowed(rel));
        }
        Ok(full)
    }

    fn escapes_allowed(&self, rel: &str) -> ToolError {
        if self.session_tmp.is_some() {
            ToolError::Execution(format!(
                "path `{rel}` is outside the workspace and session temp"
            ))
        } else {
            escapes_workspace(rel)
        }
    }
}

fn contained_in(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
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
    schemas: HashMap<String, Value>,
    validators: Mutex<HashMap<String, Arc<Validator>>>,
    /// Rebuilt on `register`. Callers that only need to send the list to the
    /// model clone this handle instead of reconstructing every JSON schema.
    descriptors: Mutex<Option<Arc<Vec<ToolDescriptor>>>>,
}

pub struct ValidatedToolCall {
    name: String,
    args: Value,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            schemas: HashMap::new(),
            validators: Mutex::new(HashMap::new()),
            descriptors: Mutex::new(None),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        self.schemas.insert(name.clone(), tool.input_schema());
        self.validators.get_mut().unwrap().remove(&name);
        self.tools.insert(name, tool);
        *self.descriptors.get_mut().unwrap() = None;
    }

    /// Register built-ins that are not already present, then start the
    /// workspace file index in the background so the first glob/grep/find
    /// overlaps journal I/O instead of stalling the first tool call.
    pub fn install_default_builtins(
        &mut self,
        web_search: &forge_config::WebSearchConfig,
        workspace: &Path,
    ) {
        for tool in crate::default_builtins_with_web_search(web_search) {
            if self.get(tool.name()).is_none() {
                self.register(tool);
            }
        }
        self.warm_workspace_index(workspace);
    }

    /// Kick off each tool's workspace-scoped background work. Safe to call
    /// more than once: search tools share one FFF scan per root.
    pub fn warm_workspace_index(&self, workspace: &Path) {
        for tool in self.tools.values() {
            tool.warm_workspace(workspace);
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .get(name)
            .or_else(|| {
                let canonical = canonical_tool_name(name);
                (canonical != name)
                    .then(|| self.tools.get(canonical))
                    .flatten()
            })
            .cloned()
    }

    pub fn list_descriptors(&self) -> Arc<Vec<ToolDescriptor>> {
        let mut cache = self
            .descriptors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(descriptors) = cache.as_ref() {
            return Arc::clone(descriptors);
        }
        let mut v: Vec<_> = self
            .tools
            .values()
            .map(|tool| ToolDescriptor {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: self.schemas[tool.name()].clone(),
                side_effect_class: tool.side_effect_class(),
                idempotent: tool.idempotent(),
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        let descriptors = Arc::new(v);
        *cache = Some(Arc::clone(&descriptors));
        descriptors
    }

    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<_> = self.tools.keys().cloned().collect();
        n.sort();
        n
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Each tool's name and side-effect class, borrowed.
    ///
    /// This is what an ACL check needs. [`list_descriptors`](Self::list_descriptors)
    /// answers the same question but clones every name, description and input
    /// schema to do it — far too much for a caller that only wants to count
    /// what a principal may see, which the status bar does on every frame.
    pub fn name_classes(&self) -> impl Iterator<Item = (&str, SideEffectClass)> + '_ {
        self.tools
            .values()
            .map(|tool| (tool.name(), tool.side_effect_class()))
    }

    /// Validate then execute. Never calls handler on validation failure.
    pub async fn call(
        &self,
        ctx: &ToolContext,
        name: &str,
        args: Value,
        budget: &mut ValidationBudget,
    ) -> Result<ToolOutput, ToolError> {
        let name = canonical_tool_name(name);
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;

        if let Err(ve) = self.validate_call(name, &args) {
            let signature =
                crate::validation::validation_error_signature(name, &ve.path, &ve.message);
            budget
                .record_failure_with_signature(name, Some(&signature))
                .map_err(ToolError::Execution)?;
            return Err(ToolError::Validation(ve));
        }

        tool.call(ctx, args).await
    }

    /// Execute a call whose canonical name and arguments were validated by the
    /// caller immediately before scheduling it. Only use for immutable,
    /// read-only calls retained from the same response.
    pub fn prepare_call(&self, name: &str, args: Value) -> Result<ValidatedToolCall, ToolError> {
        let name = canonical_tool_name(name);
        self.get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        self.validate_call(name, &args)
            .map_err(ToolError::Validation)?;
        Ok(ValidatedToolCall {
            name: name.to_string(),
            args,
        })
    }

    pub async fn call_prepared(
        &self,
        ctx: &ToolContext,
        prepared: ValidatedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .get(&prepared.name)
            .ok_or_else(|| ToolError::Unknown(prepared.name.clone()))?;
        tool.call(ctx, prepared.args).await
    }

    /// Validate without executing or consuming retry budget.
    pub fn validate_call(&self, name: &str, args: &Value) -> Result<(), ToolValidationError> {
        let name = canonical_tool_name(name);
        let validator = self
            .validators(name)
            .map_err(|compile_error| ToolValidationError {
                tool: name.to_string(),
                path: "$".into(),
                message: format!("invalid tool schema: {compile_error}"),
                schema_hint: None,
            })?;
        validate_args_with(name, &self.schemas[name], &validator, args)
    }

    fn validators(&self, name: &str) -> Result<Arc<Validator>, String> {
        let schema = self
            .schemas
            .get(name)
            .ok_or_else(|| format!("no schema for `{name}`"))?;
        let mut cache = self.validators.lock().unwrap();
        if let Some(v) = cache.get(name) {
            return Ok(v.clone());
        }
        let validator = Arc::new(crate::validation::compile_validator(schema)?);
        cache.insert(name.to_string(), validator.clone());
        Ok(validator)
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
    use crate::builtins::{ReadFileTool, WriteFileTool};
    use crate::fast_file_tools::{FastFileState, FffGrepTool};
    use crate::ValidationBudget;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn session_temp_is_external_private_and_removed_on_drop() {
        let workspace = tempdir().unwrap();
        let session_tmp = SessionTempDir::create("test-session").unwrap();
        let path = session_tmp.path().to_path_buf();

        assert!(!path.starts_with(workspace.path()));
        assert!(path.starts_with(std::env::temp_dir()));
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("forge-test-session-"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        drop(session_tmp);
        assert!(!path.exists());
    }

    #[test]
    fn resolve_path_allows_session_temp_and_rejects_the_rest_of_tmp() {
        let workspace = tempdir().unwrap();
        let session_tmp = SessionTempDir::create("read-temp").unwrap();
        let ctx =
            ToolContext::new(workspace.path().to_path_buf()).with_session_tmp(session_tmp.clone());

        let existing = session_tmp.path().join("note.txt");
        std::fs::write(&existing, "scratch").unwrap();
        assert_eq!(
            ctx.resolve_path(existing.to_str().unwrap()).unwrap(),
            existing.canonicalize().unwrap()
        );

        let missing = session_tmp.path().join("nested/new.txt");
        assert_eq!(
            ctx.resolve_write_path(missing.to_str().unwrap()).unwrap(),
            missing
        );

        let stray = std::env::temp_dir().join("outside-workspace.json");
        let err = ctx.resolve_path(stray.to_str().unwrap()).unwrap_err();
        match &err {
            ToolError::Execution(message) => {
                assert!(
                    message.contains("outside the workspace and session temp"),
                    "{err}"
                );
            }
            other => panic!("expected execution error, got {other}"),
        }
        let tmp_root = std::env::temp_dir();
        let err = ctx.resolve_path(tmp_root.to_str().unwrap()).unwrap_err();
        match &err {
            ToolError::Execution(message) => {
                assert!(
                    message.contains("outside the workspace and session temp"),
                    "{err}"
                );
            }
            other => panic!("expected execution error, got {other}"),
        }
    }

    #[tokio::test]
    async fn read_file_reads_session_temp() {
        let workspace = tempdir().unwrap();
        let session_tmp = SessionTempDir::create("read-file-temp").unwrap();
        let path = session_tmp.path().join("probe.txt");
        std::fs::write(&path, "from tmp\n").unwrap();

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        let ctx = ToolContext::new(workspace.path().to_path_buf()).with_session_tmp(session_tmp);
        let mut b = ValidationBudget::with_default_max();
        let out = reg
            .call(
                &ctx,
                "read_file",
                json!({"path": path.to_str().unwrap()}),
                &mut b,
            )
            .await
            .unwrap();
        assert!(out.content.contains("from tmp"), "{out:?}");
    }

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
    fn compiled_validator_is_cached_per_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        assert!(reg.validators("read_file").is_ok());
        let first = reg.validators("read_file").unwrap();
        let second = reg.validators("read_file").unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "validator must be compiled once"
        );
        assert!(reg.validators("nope").is_err());
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
    fn list_descriptors_serialize_identically_across_rebuilds() {
        let mut first = ToolRegistry::new();
        first.register(Arc::new(ReadFileTool));
        first.register(Arc::new(WriteFileTool));
        let mut second = ToolRegistry::new();
        second.register(Arc::new(WriteFileTool));
        second.register(Arc::new(ReadFileTool));
        let a = serde_json::to_string(first.list_descriptors().as_ref()).unwrap();
        let b = serde_json::to_string(second.list_descriptors().as_ref()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn list_descriptors_reuses_the_cached_allocation() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        let first = reg.list_descriptors();
        let second = reg.list_descriptors();
        assert!(
            Arc::ptr_eq(&first, &second),
            "descriptors must be rebuilt only when the registry changes"
        );
        reg.register(Arc::new(ReadFileTool));
        let third = reg.list_descriptors();
        assert!(
            !Arc::ptr_eq(&first, &third),
            "register must drop the descriptor cache"
        );
    }

    #[test]
    fn registry_descriptors_and_names_are_sorted() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool));
        assert_eq!(reg.names(), vec!["read_file"]);
        let descriptors = reg.list_descriptors();
        assert_eq!(descriptors[0].name, "read_file");
    }

    #[test]
    fn rg_resolves_to_grep_even_when_only_grep_is_registered() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(FffGrepTool::new(
            Arc::new(FastFileState::new()),
            "grep",
        )));
        let tool = reg.get("rg").expect("rg should resolve to grep");
        assert_eq!(tool.name(), "grep");
        assert_eq!(canonical_tool_name("rg"), "grep");
        let call = canonicalize_tool_call(ToolCall {
            id: "1".into(),
            name: "rg".into(),
            arguments: json!({"pattern": "hello"}),
        });
        assert_eq!(call.name, "grep");
    }

    #[test]
    fn search_replace_resolves_to_edit() {
        assert_eq!(canonical_tool_name("search_replace"), "edit");
        assert_eq!(canonical_tool_name("edit_file"), "edit");
        let call = canonicalize_tool_call(ToolCall {
            id: "1".into(),
            name: "search_replace".into(),
            arguments: json!({"path": "a.rs", "old_string": "a", "new_string": "b"}),
        });
        assert_eq!(call.name, "edit");
    }

    #[tokio::test]
    async fn install_default_builtins_warms_search_and_serves_glob() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("hit.rs"), "hello from install\n").unwrap();
        let mut reg = ToolRegistry::new();
        reg.install_default_builtins(&forge_config::WebSearchConfig::default(), dir.path());
        assert!(reg.get("glob").is_some());
        assert!(reg.get("grep").is_some());
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let mut budget = ValidationBudget::with_default_max();
        let out = reg
            .call(&ctx, "glob", json!({"pattern": "hit.rs"}), &mut budget)
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("hit.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn calling_rg_executes_grep() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("hit.rs"), "hello from rg\n").unwrap();
        let mut reg = ToolRegistry::new();
        for tool in crate::default_builtins() {
            reg.register(tool);
        }
        assert!(
            !reg.list_descriptors().iter().any(|d| d.name == "rg"),
            "OpenCode-style: rg is inbound-only, not advertised"
        );
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let mut budget = ValidationBudget::with_default_max();
        let out = reg
            .call(&ctx, "rg", json!({"pattern": "hello"}), &mut budget)
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("hello"), "{}", out.content);
    }

    #[test]
    fn default_registry_behaves_like_new() {
        // `Default` is a thin wrapper over `new`; verify it produces the same
        // empty, usable registry rather than assuming the delegation.
        let reg = ToolRegistry::default();
        assert!(reg.names().is_empty());
        assert!(reg.list_descriptors().is_empty());
        assert!(reg.get("read_file").is_none());
    }
}
