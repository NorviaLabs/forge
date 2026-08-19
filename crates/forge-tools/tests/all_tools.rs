//! Contract suite for every built-in tool.
//!
//! Unit tests next to each impl cover parsers and edge cases. This file is the
//! inventory + registry-path gate: if a tool is added, renamed, or stops
//! executing through `ToolRegistry::call`, these tests fail first.

use std::collections::BTreeSet;
use std::process::Command;
use std::sync::Arc;

use forge_config::WebSearchConfig;
use forge_tools::{
    default_builtins, web_search_tool, web_search_tool_for_tests, ToolContext, ToolError,
    ToolRegistry, ValidationBudget,
};
use forge_types::SideEffectClass;
use serde_json::{json, Value};
use tempfile::TempDir;

/// Every name `default_builtins()` must expose. Sorted so a missing or extra
/// tool is an obvious assert failure, not an order flake.
const DEFAULT_TOOL_NAMES: &[&str] = &[
    "apply_patch",
    "ask_user_question",
    "background_run",
    "bash",
    "edit",
    "exec_command",
    "git",
    "glob",
    "grep",
    "load_skill",
    "ls",
    "read_file",
    "update_plan",
    "view_image",
    "web_fetch",
    "write_file",
    "write_stdin",
];

fn register_all() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in default_builtins() {
        registry.register(tool);
    }
    registry.register(web_search_tool_for_tests());
    registry
}

struct Workspace {
    _dir: TempDir,
    ctx: ToolContext,
    registry: ToolRegistry,
}

impl Workspace {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello forge\n").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "fn forge() {}\n").unwrap();
        std::fs::write(dir.path().join("shot.png"), forge_types::sample_png_bytes()).unwrap();

        let skill = dir.path().join(".agents/skills/reviewer");
        std::fs::create_dir_all(skill.join("references")).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: reviewer\ndescription: Reviews PRs.\n---\n\nFull instructions.\n",
        )
        .unwrap();
        std::fs::write(skill.join("references/style.md"), "Use two spaces.\n").unwrap();

        assert!(Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        for (key, value) in [("user.email", "forge@test"), ("user.name", "Forge Test")] {
            assert!(Command::new("git")
                .args(["config", key, value])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success());
        }

        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.image_input = true;
        ctx.active_model = "test/vision".into();

        Self {
            _dir: dir,
            ctx,
            registry: register_all(),
        }
    }

    async fn call(&self, name: &str, args: Value) -> Result<forge_types::ToolOutput, ToolError> {
        let mut budget = ValidationBudget::with_default_max();
        self.registry.call(&self.ctx, name, args, &mut budget).await
    }
}

#[test]
fn default_builtins_match_the_known_inventory() {
    let names: BTreeSet<String> = default_builtins()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    let expected: BTreeSet<String> = DEFAULT_TOOL_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(
        names, expected,
        "update DEFAULT_TOOL_NAMES when adding or removing a built-in"
    );
}

#[test]
fn every_default_tool_has_a_usable_descriptor() {
    for tool in default_builtins() {
        assert!(!tool.name().is_empty(), "tool name must not be empty");
        assert!(
            !tool.description().trim().is_empty(),
            "{} is missing a description",
            tool.name()
        );
        let schema = tool.input_schema();
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{} schema must be an object, got {schema}",
            tool.name()
        );
        let _ = tool.side_effect_class();
        let _ = tool.idempotent();
    }
}

#[test]
fn web_search_is_optional_and_network_class() {
    let names: Vec<_> = default_builtins()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert!(
        !names.iter().any(|name| name == "web_search"),
        "default_builtins must not include web_search"
    );
    assert!(
        web_search_tool(&WebSearchConfig::default()).is_none(),
        "default mock config must not register web_search for users"
    );
    let search = web_search_tool_for_tests();
    assert_eq!(search.name(), "web_search");
    assert_eq!(search.side_effect_class(), SideEffectClass::Network);
}

#[tokio::test]
async fn every_tool_rejects_missing_required_args() {
    let workspace = Workspace::new();
    let required: &[(&str, Value)] = &[
        ("read_file", json!({})),
        ("write_file", json!({})),
        ("edit", json!({})),
        ("view_image", json!({})),
        ("apply_patch", json!({})),
        ("bash", json!({})),
        ("background_run", json!({})),
        ("git", json!({})),
        ("exec_command", json!({})),
        ("write_stdin", json!({})),
        ("update_plan", json!({})),
        ("ask_user_question", json!({})),
        ("load_skill", json!({})),
        ("glob", json!({})),
        ("grep", json!({})),
        ("web_search", json!({})),
        ("web_fetch", json!({})),
    ];
    for (name, args) in required {
        let error = workspace.call(name, args.clone()).await.expect_err(name);
        assert!(
            matches!(error, ToolError::Validation(_)),
            "{name} must fail schema validation on {args}, got {error}"
        );
    }
}

#[tokio::test]
async fn unknown_tool_is_rejected() {
    let workspace = Workspace::new();
    let error = workspace.call("not_a_tool", json!({})).await.unwrap_err();
    assert!(matches!(error, ToolError::Unknown(name) if name == "not_a_tool"));
}

#[tokio::test]
async fn read_file_ls_glob_and_grep() {
    let workspace = Workspace::new();

    let read = workspace
        .call("read_file", json!({"path": "hello.txt"}))
        .await
        .unwrap();
    assert!(!read.is_error, "{}", read.content);
    assert_eq!(read.content, "hello forge");

    let sliced = workspace
        .call(
            "read_file",
            json!({"path": "src/lib.rs", "offset": 1, "limit": 1}),
        )
        .await
        .unwrap();
    assert_eq!(sliced.content, "fn forge() {}");

    let listed = workspace.call("ls", json!({"path": "src"})).await.unwrap();
    assert!(!listed.is_error, "{}", listed.content);
    assert!(listed.content.contains("lib.rs"), "{}", listed.content);

    let found = workspace
        .call("glob", json!({"pattern": "lib.rs"}))
        .await
        .unwrap();
    assert!(found.content.contains("src/lib.rs"), "{}", found.content);

    let matches = workspace
        .call("grep", json!({"pattern": "forge", "path": "src"}))
        .await
        .unwrap();
    assert!(
        matches.content.contains("src/lib.rs"),
        "{}",
        matches.content
    );

    let via_alias = workspace
        .call("rg", json!({"pattern": "hello"}))
        .await
        .unwrap();
    assert!(
        via_alias.content.contains("hello.txt"),
        "{}",
        via_alias.content
    );
}

#[tokio::test]
async fn write_file_and_apply_patch() {
    let workspace = Workspace::new();

    let written = workspace
        .call(
            "write_file",
            json!({"path": "nested/out.txt", "content": "created\n"}),
        )
        .await
        .unwrap();
    assert!(!written.is_error, "{}", written.content);
    assert_eq!(
        std::fs::read_to_string(workspace.ctx.workspace_root.join("nested/out.txt")).unwrap(),
        "created\n"
    );

    let edited = workspace
        .call(
            "edit",
            json!({
                "path": "hello.txt",
                "old_string": "hello forge",
                "new_string": "hello edited"
            }),
        )
        .await
        .unwrap();
    assert!(!edited.is_error, "{}", edited.content);
    assert_eq!(
        std::fs::read_to_string(workspace.ctx.workspace_root.join("hello.txt")).unwrap(),
        "hello edited\n"
    );

    let via_alias = workspace
        .call(
            "search_replace",
            json!({
                "path": "hello.txt",
                "old_string": "hello edited",
                "new_string": "hello forge"
            }),
        )
        .await
        .unwrap();
    assert!(!via_alias.is_error, "{}", via_alias.content);

    let patched = workspace
        .call(
            "apply_patch",
            json!({
                "patch": "*** Begin Patch\n*** Update File: hello.txt\n@@\n-hello forge\n+hello patched\n*** End Patch"
            }),
        )
        .await
        .unwrap();
    assert!(!patched.is_error, "{}", patched.content);
    assert_eq!(
        std::fs::read_to_string(workspace.ctx.workspace_root.join("hello.txt")).unwrap(),
        "hello patched\n"
    );
}

#[tokio::test]
async fn view_image_attaches_a_workspace_png() {
    let workspace = Workspace::new();
    let out = workspace
        .call("view_image", json!({"path": "shot.png"}))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.attachments.len(), 1);
    assert_eq!(out.attachments[0].mime, "image/png");
}

#[tokio::test]
async fn bash_git_and_background_run() {
    let workspace = Workspace::new();

    let echoed = workspace
        .call("bash", json!({"command": "printf hi"}))
        .await
        .unwrap();
    assert!(!echoed.is_error, "{}", echoed.content);
    assert!(echoed.content.contains("hi"), "{}", echoed.content);

    let status = workspace
        .call("git", json!({"subcommand": "status", "args": ["--short"]}))
        .await
        .unwrap();
    assert!(!status.is_error, "{}", status.content);

    let error = workspace
        .call("background_run", json!({"command": "sleep 1"}))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("intercepted"),
        "background_run must refuse direct registry execution, got {error}"
    );
}

#[tokio::test]
async fn exec_command_and_write_stdin_share_a_session() {
    let workspace = Workspace::new();
    let first = workspace
        .call(
            "exec_command",
            json!({"cmd": "printf ready; sleep 1", "yield_time_ms": 20}),
        )
        .await
        .unwrap();
    let mut body: Value = serde_json::from_str(&first.content).unwrap();
    let id = body["session_id"].as_u64().expect("session id");
    for _ in 0..50 {
        if body["output"]
            .as_str()
            .unwrap_or_default()
            .contains("ready")
        {
            break;
        }
        assert_eq!(body["running"], true, "{body}");
        let polled = workspace
            .call(
                "write_stdin",
                json!({"session_id": id, "yield_time_ms": 50}),
            )
            .await
            .unwrap();
        body = serde_json::from_str(&polled.content).unwrap();
    }
    assert!(body["output"].as_str().unwrap().contains("ready"), "{body}");
}

#[tokio::test]
async fn update_plan_load_skill_and_web_search() {
    let workspace = Workspace::new();

    let plan = workspace
        .call(
            "update_plan",
            json!({
                "explanation": "start",
                "plan": [
                    {"step": "read code", "status": "completed"},
                    {"step": "implement", "status": "in_progress"}
                ]
            }),
        )
        .await
        .unwrap();
    assert!(!plan.is_error, "{}", plan.content);
    assert_eq!(plan.content, "Plan updated");

    let skill = workspace
        .call("load_skill", json!({"name": "reviewer"}))
        .await
        .unwrap();
    assert!(
        skill.content.contains("Full instructions."),
        "{}",
        skill.content
    );

    let reference = workspace
        .call(
            "load_skill",
            json!({"name": "reviewer", "path": "references/style.md"}),
        )
        .await
        .unwrap();
    assert_eq!(reference.content, "Use two spaces.\n");

    let search = workspace
        .call("web_search", json!({"query": "serde json schema"}))
        .await
        .unwrap();
    assert!(!search.is_error, "{}", search.content);
    assert!(search.content.contains("serde"), "{}", search.content);
}

#[tokio::test]
async fn web_fetch_runs_through_the_registry_and_blocks_private_targets() {
    let workspace = Workspace::new();

    let blocked = workspace
        .call("web_fetch", json!({"url": "http://127.0.0.1:1/"}))
        .await
        .unwrap();
    assert!(blocked.is_error);
    assert!(
        blocked.content.contains("non-public"),
        "{}",
        blocked.content
    );

    let bad_scheme = workspace
        .call("web_fetch", json!({"url": "file:///etc/passwd"}))
        .await
        .unwrap();
    assert!(bad_scheme.is_error);
    assert!(
        bad_scheme.content.contains("scheme"),
        "{}",
        bad_scheme.content
    );
}

#[tokio::test]
async fn workspace_confinement_holds_for_file_tools() {
    let workspace = Workspace::new();
    let error = workspace
        .call("read_file", json!({"path": "../secret.txt"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("escapes workspace"), "{error}");

    let error = workspace
        .call(
            "write_file",
            json!({"path": "../escape.txt", "content": "nope"}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("escapes workspace"), "{error}");

    let error = workspace
        .call(
            "apply_patch",
            json!({"patch": "*** Begin Patch\n*** Add File: ../escape.txt\n+nope\n*** End Patch"}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("escapes workspace"), "{error}");

    let error = workspace
        .call(
            "edit",
            json!({
                "path": "../escape.rs",
                "old_string": "a",
                "new_string": "b"
            }),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("escapes workspace"), "{error}");
}

#[test]
fn registry_names_include_every_default_tool() {
    let registry = register_all();
    let names: BTreeSet<String> = registry.names().into_iter().collect();
    for name in DEFAULT_TOOL_NAMES {
        assert!(names.contains(*name), "registry missing {name}: {names:?}");
    }
    assert!(names.contains("web_search"));
}

#[test]
fn every_default_tool_is_reachable_from_the_registry() {
    let registry = register_all();
    for name in DEFAULT_TOOL_NAMES {
        let tool: Option<Arc<dyn forge_tools::Tool>> = registry.get(name);
        assert!(tool.is_some(), "registry.get({name}) returned None");
        assert_eq!(tool.unwrap().name(), *name);
    }
    assert_eq!(
        registry.get("rg").expect("rg alias").name(),
        "grep",
        "rg must resolve to the grep implementation"
    );
    assert_eq!(
        registry.get("search_replace").expect("edit alias").name(),
        "edit",
        "search_replace must resolve to the edit implementation"
    );
}
