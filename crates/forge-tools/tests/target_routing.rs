//! Every workspace tool goes through the session's target, with no SSH
//! needed: a target that refuses everything must be what each tool reports,
//! so none of them reached the local disk or spawned locally on its own.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use forge_tools::shell_spawn::ShellSpawnRequest;
use forge_tools::target::{
    Access, DirEntry, FileMeta, GrepHit, RunOutput, Target, TargetPath, TargetShell,
};
use forge_tools::{default_builtins, ToolContext, ToolError};
use serde_json::json;

const MARK: &str = "refusing-target-was-asked";

/// A remote target that answers every request with [`MARK`].
#[derive(Debug)]
struct Refusing;

fn refuse<T>() -> Result<T, ToolError> {
    Err(ToolError::Execution(MARK.into()))
}

#[async_trait]
impl Target for Refusing {
    fn remote_host(&self) -> Option<&str> {
        Some("refusing")
    }
    fn label(&self) -> String {
        "refusing:/w".into()
    }
    async fn shell(&self, _: ShellSpawnRequest<'_>) -> Result<TargetShell, ToolError> {
        refuse()
    }
    async fn resolve(&self, _: &ToolContext, _: &str, _: Access) -> Result<TargetPath, ToolError> {
        refuse()
    }
    async fn metadata(&self, _: &TargetPath) -> Result<Option<FileMeta>, ToolError> {
        refuse()
    }
    async fn read(&self, _: &TargetPath, _: u64) -> Result<Vec<u8>, ToolError> {
        refuse()
    }
    async fn read_lines(
        &self,
        _: &TargetPath,
        _: u64,
        _: Option<u64>,
    ) -> Result<String, ToolError> {
        refuse()
    }
    async fn write(&self, _: &TargetPath, _: &[u8]) -> Result<(), ToolError> {
        refuse()
    }
    async fn remove(&self, _: &TargetPath) -> Result<(), ToolError> {
        refuse()
    }
    async fn list(&self, _: &TargetPath) -> Result<Vec<DirEntry>, ToolError> {
        refuse()
    }
    async fn run(&self, _: &[String], _: Duration) -> Result<RunOutput, ToolError> {
        refuse()
    }
    async fn find(&self, _: &str, _: usize) -> Result<(Vec<String>, bool), ToolError> {
        refuse()
    }
    async fn grep(
        &self,
        _: &str,
        _: bool,
        _: Option<&TargetPath>,
        _: Option<&str>,
        _: usize,
    ) -> Result<(Vec<GrepHit>, bool), ToolError> {
        refuse()
    }
}

#[tokio::test]
async fn every_workspace_tool_asks_the_target() {
    let local = tempfile::tempdir().unwrap();
    std::fs::write(local.path().join("here.txt"), "local file").unwrap();
    let mut ctx = ToolContext::new(local.path().to_path_buf()).with_target(Arc::new(Refusing));
    ctx.image_input = true;
    let calls = [
        ("bash", json!({"command": "echo local"})),
        ("exec_command", json!({"cmd": "echo local"})),
        ("read_file", json!({"path": "here.txt"})),
        ("write_file", json!({"path": "new.txt", "content": "x"})),
        (
            "edit",
            json!({"path": "here.txt", "old_string": "local", "new_string": "x"}),
        ),
        (
            "apply_patch",
            json!({"patch": "*** Begin Patch\n*** Add File: p.txt\n+x\n*** End Patch"}),
        ),
        ("ls", json!({})),
        ("view_image", json!({"path": "here.txt"})),
        ("glob", json!({"pattern": "*.txt"})),
        ("grep", json!({"pattern": "local"})),
        ("git", json!({"subcommand": "status", "args": []})),
    ];
    for (name, args) in calls {
        let tool = default_builtins()
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap();
        let reported = match tool.call(&ctx, args).await {
            Err(error) => error.to_string(),
            Ok(output) => output.content,
        };
        assert!(
            reported.contains(MARK),
            "{name} did not go through the target: {reported}"
        );
    }
    assert!(!local.path().join("new.txt").exists());
    assert!(!local.path().join("p.txt").exists());
    assert_eq!(
        std::fs::read_to_string(local.path().join("here.txt")).unwrap(),
        "local file",
        "the local file was never edited"
    );
    let capture = Arc::new(forge_tools::ShellOutputCapture::default());
    let job = forge_tools::run_shell_job(&ctx, "echo local", capture).await;
    assert!(
        job.unwrap_err().to_string().contains(MARK),
        "background jobs too"
    );
}
