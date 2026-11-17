//! Execution targets: where a session's shell commands run and its files live.
//!
//! Every workspace tool goes through a [`Target`]: `bash`, background jobs and
//! `exec_command` ask it how to spawn a shell ([`Target::shell`]); `read_file`,
//! `write_file`, `edit`, `apply_patch`, `ls` and `view_image` use its file
//! methods; `glob`, `grep` and `git` use [`Target::find`], [`Target::grep`]
//! and [`Target::run`] when it is remote.
//!
//! * [`LocalTarget`] is this machine, under Forge's sandbox: today's code,
//!   behind the interface.
//! * [`SshTarget`] is a host from the user's SSH config and a folder on it.
//!   An SSH command is itself a local process (the `ssh` client), so its
//!   spawn is a [`ShellSpawnSpec`] like any other and the existing output,
//!   exit-code, yield, PTY and stdin handling applies unchanged; the
//!   [`RemoteRun`] beside it handles what only a remote run has: telling
//!   a lost connection from an exit status of 255, and killing the remote
//!   process on cancel.
//!
//! Virtual machines and microVMs are further targets behind the same trait.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;

use crate::registry::ToolContext;
use crate::shell_spawn::{ShellSpawnRequest, ShellSpawnSpec};
use crate::ToolError;

pub(crate) mod helper;
pub mod quote;
pub mod ssh;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use ssh::{private_control_dir, SshOptions, SshTarget};

/// Whether a path is resolved to be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// An absolute, resolved path on the target, and what was there when it
/// was resolved, when the target learned that on the way (a remote target
/// does: it saves the tool a round trip to ask again).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPath {
    path: PathBuf,
    /// `Some(None)`: nothing was there.
    seen: Option<Option<FileMeta>>,
}

impl TargetPath {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            seen: None,
        }
    }

    /// This path, and what resolving it found there.
    pub fn seen(mut self, meta: Option<FileMeta>) -> Self {
        self.seen = Some(meta);
        self
    }

    /// What resolving found there, if the target said.
    pub fn seen_meta(&self) -> Option<Option<FileMeta>> {
        self.seen
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    pub is_dir: bool,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// A command run without a shell (`git`), in the target's workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

/// One match from [`Target::grep`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepHit {
    /// Relative to the workspace.
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// How to spawn one shell command on a target, and what a remote run needs
/// when it ends.
pub struct TargetShell {
    pub spec: ShellSpawnSpec,
    /// `None` on this machine.
    pub remote: Option<RemoteRun>,
}

/// The remote half of one command: its exit status and its process group
/// live on the host, and the local `ssh` only relays them.
///
/// Dropped before [`Self::exit_status`] was asked (the call was cancelled,
/// or its future dropped), it kills the command's process group on the host:
/// killing the local `ssh` alone would leave the command running there.
pub struct RemoteRun {
    pub(crate) inner: ssh::RunHandle,
    armed: std::sync::atomic::AtomicBool,
}

impl RemoteRun {
    pub(crate) fn new(inner: ssh::RunHandle) -> Self {
        Self {
            inner,
            armed: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// The command's exit status, given the local `ssh` client's. `ssh`
    /// exits 255 both when the command did and when the connection failed;
    /// the status the wrapper recorded on the host tells them apart. A
    /// failed connection is [`ToolError::Execution`] naming the host.
    pub async fn exit_status(&self, local: Option<i32>) -> Result<Option<i32>, ToolError> {
        self.armed.store(false, std::sync::atomic::Ordering::SeqCst);
        self.inner.exit_status(local).await
    }

    /// Kill the command's process group on the host (TERM, then KILL).
    /// Best effort, and returns at once; called on cancel and when a
    /// session closes.
    pub fn cancel(&self) {
        self.armed.store(false, std::sync::atomic::Ordering::SeqCst);
        self.inner.cancel();
    }
}

impl Drop for RemoteRun {
    fn drop(&mut self) {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            self.inner.cancel();
        }
    }
}

#[async_trait]
pub trait Target: Send + Sync + std::fmt::Debug {
    /// `None` for this machine; the SSH host alias for a remote target.
    fn remote_host(&self) -> Option<&str>;

    /// `this Mac` or `devbox:/srv/app`.
    fn label(&self) -> String;

    /// Release what the target holds for its session (a connection, a
    /// temp directory on the host). Best effort; the next use reconnects.
    async fn close(&self) {}

    /// How to spawn one shell command (bash, background jobs, exec_command).
    async fn shell(&self, request: ShellSpawnRequest<'_>) -> Result<TargetShell, ToolError>;

    /// Resolve a tool-supplied path, confined to the workspace (and the
    /// session's temp directory). Writes under `.git` and `.forge` are
    /// refused.
    async fn resolve(
        &self,
        ctx: &ToolContext,
        path: &str,
        access: Access,
    ) -> Result<TargetPath, ToolError>;

    /// `None` when nothing is there.
    async fn metadata(&self, path: &TargetPath) -> Result<Option<FileMeta>, ToolError>;

    /// The first `max` bytes of a file. A missing file is
    /// [`ToolError::Io`] with [`std::io::ErrorKind::NotFound`].
    async fn read(&self, path: &TargetPath, max: u64) -> Result<Vec<u8>, ToolError>;

    /// Lines `offset..offset+limit` (1-based), without their line endings,
    /// joined with `\n`.
    async fn read_lines(
        &self,
        path: &TargetPath,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<String, ToolError>;

    /// [`Self::read`] of the first `head` bytes and [`Self::read_lines`]
    /// together (the lines may fail on their own, e.g. for a binary file):
    /// what `read_file` needs, in one round trip on a remote target.
    async fn read_head_and_lines(
        &self,
        path: &TargetPath,
        head: u64,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<(Vec<u8>, Result<String, ToolError>), ToolError> {
        let header = self.read(path, head).await?;
        let lines = self.read_lines(path, offset, limit).await;
        Ok((header, lines))
    }

    /// Write a file, creating its parent directories.
    async fn write(&self, path: &TargetPath, bytes: &[u8]) -> Result<(), ToolError>;

    /// [`Self::write`], returning what the file held before (`None` when
    /// there was no file): `write_file`'s diff, in one round trip on a
    /// remote target.
    async fn replace(&self, path: &TargetPath, bytes: &[u8]) -> Result<Option<Vec<u8>>, ToolError> {
        let old = self.read(path, u64::MAX).await.ok();
        self.write(path, bytes).await?;
        Ok(old)
    }

    /// Delete a file.
    async fn remove(&self, path: &TargetPath) -> Result<(), ToolError>;

    /// A directory's entries, hidden ones included, sorted by name. A
    /// symbolic link is not a directory, whatever it points at.
    async fn list(&self, path: &TargetPath) -> Result<Vec<DirEntry>, ToolError>;

    /// Run `argv` (no shell) in the workspace. Remote targets only: on this
    /// machine `git` keeps its sandboxed path.
    async fn run(&self, argv: &[String], timeout: Duration) -> Result<RunOutput, ToolError> {
        let _ = (argv, timeout);
        Err(ToolError::Execution(
            "this target runs commands through the shell spawn path".into(),
        ))
    }

    /// Files matching a glob, relative to the workspace, and whether there
    /// were more than `max`. Remote targets only: on this machine the
    /// indexed search serves `glob`.
    async fn find(&self, pattern: &str, max: usize) -> Result<(Vec<String>, bool), ToolError> {
        let _ = (pattern, max);
        Err(ToolError::Execution(
            "this target searches with its index".into(),
        ))
    }

    /// Lines matching `pattern` (a regex, or a literal when `literal`) under
    /// `path`, and whether there were more than `max`. Remote targets only,
    /// as [`Self::find`].
    async fn grep(
        &self,
        pattern: &str,
        literal: bool,
        path: Option<&TargetPath>,
        include: Option<&str>,
        max: usize,
    ) -> Result<(Vec<GrepHit>, bool), ToolError> {
        let _ = (pattern, literal, path, include, max);
        Err(ToolError::Execution(
            "this target searches with its index".into(),
        ))
    }
}

/// This machine: Forge's sandbox, egress gateway and brokers, and the local
/// file system. Today's behaviour.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalTarget;

#[async_trait]
impl Target for LocalTarget {
    fn remote_host(&self) -> Option<&str> {
        None
    }

    fn label(&self) -> String {
        "this Mac".into()
    }

    async fn shell(&self, request: ShellSpawnRequest<'_>) -> Result<TargetShell, ToolError> {
        Ok(TargetShell {
            spec: ShellSpawnSpec::build(request).await?,
            remote: None,
        })
    }

    async fn resolve(
        &self,
        ctx: &ToolContext,
        path: &str,
        access: Access,
    ) -> Result<TargetPath, ToolError> {
        match access {
            Access::Read => ctx.resolve_path(path),
            Access::Write => ctx.resolve_write_path(path),
        }
        .map(TargetPath::new)
    }

    async fn metadata(&self, path: &TargetPath) -> Result<Option<FileMeta>, ToolError> {
        match tokio::fs::metadata(path.as_path()).await {
            Ok(meta) => Ok(Some(FileMeta {
                is_dir: meta.is_dir(),
                len: meta.len(),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ToolError::Io(error)),
        }
    }

    async fn read(&self, path: &TargetPath, max: u64) -> Result<Vec<u8>, ToolError> {
        use tokio::io::AsyncReadExt;
        let file = tokio::fs::File::open(path.as_path()).await?;
        let mut bytes = Vec::new();
        file.take(max).read_to_end(&mut bytes).await?;
        Ok(bytes)
    }

    async fn read_lines(
        &self,
        path: &TargetPath,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<String, ToolError> {
        use tokio::io::AsyncBufReadExt;
        let file = tokio::fs::File::open(path.as_path()).await?;
        let mut lines = tokio::io::BufReader::new(file).lines();
        for _ in 0..offset.saturating_sub(1) {
            if lines.next_line().await?.is_none() {
                break;
            }
        }
        let mut content = String::new();
        let mut count = 0;
        while limit.is_none_or(|limit| count < limit) {
            let Some(line) = lines.next_line().await? else {
                break;
            };
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&line);
            count += 1;
        }
        Ok(content)
    }

    async fn write(&self, path: &TargetPath, bytes: &[u8]) -> Result<(), ToolError> {
        if let Some(parent) = path.as_path().parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path.as_path(), bytes).await?;
        Ok(())
    }

    async fn remove(&self, path: &TargetPath) -> Result<(), ToolError> {
        tokio::fs::remove_file(path.as_path()).await?;
        Ok(())
    }

    async fn list(&self, path: &TargetPath) -> Result<Vec<DirEntry>, ToolError> {
        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(path.as_path()).await?;
        while let Some(entry) = dir.next_entry().await? {
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: entry.file_type().await?.is_dir(),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
}
