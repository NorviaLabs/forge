use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxProfile {
    Light,
    Container,
    Ebpf,
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub command: String,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub is_error: bool,
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("profile `{0}` not available on this host")]
    Unavailable(String),
    #[error("{0}")]
    Other(String),
}

pub trait Sandbox: Send + Sync {
    fn exec(&self, req: ExecRequest) -> Result<ExecResult, SandboxError>;
}

/// Light isolation: run in workspace cwd, scrub some env, no container (SEC-03 Phase 2 light).
pub struct LightSandbox;

impl Sandbox for LightSandbox {
    fn exec(&self, req: ExecRequest) -> Result<ExecResult, SandboxError> {
        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(&req.command)
            .current_dir(&req.cwd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()))
            .env("HOME", std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
            .env("LANG", "C");
        for (k, v) in &req.env {
            // Never inject keys that look like secrets into child without explicit list —
            // callers pass only materialised short-lived secrets.
            cmd.env(k, v);
        }
        let out = cmd.output()?;
        Ok(ExecResult {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            is_error: !out.status.success(),
        })
    }
}

/// Container profile: Phase 2 marks intent; without docker, returns Unavailable.
pub struct ContainerSandbox;

impl Sandbox for ContainerSandbox {
    fn exec(&self, _req: ExecRequest) -> Result<ExecResult, SandboxError> {
        // Detect docker; if missing, fail closed with clear error (not silent primary).
        let docker = Command::new("docker").arg("version").output();
        match docker {
            Ok(o) if o.status.success() => Err(SandboxError::Other(
                "container profile configured but full image policy not wired in this build; use light"
                    .into(),
            )),
            _ => Err(SandboxError::Unavailable("container".into())),
        }
    }
}

pub fn sandbox_for(profile: SandboxProfile) -> Box<dyn Sandbox> {
    match profile {
        SandboxProfile::Light | SandboxProfile::Ebpf => Box::new(LightSandbox),
        SandboxProfile::Container => Box::new(ContainerSandbox),
    }
}

#[allow(dead_code)]
fn _ensure_sandbox_for_linked() {
    let _ = sandbox_for(SandboxProfile::Light);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_echo() {
        let r = LightSandbox
            .exec(ExecRequest {
                command: "echo sandbox-ok".into(),
                cwd: std::env::current_dir().unwrap(),
                env: vec![],
            })
            .unwrap();
        assert!(r.stdout.contains("sandbox-ok"));
    }
}
