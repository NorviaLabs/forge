use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

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
    #[error("{0}")]
    Other(String),
}

/// Light isolation: run in workspace cwd, scrub some env, no container.
pub fn light_sandbox_exec(req: &ExecRequest) -> Result<ExecResult, SandboxError> {
    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(&req.command)
        .current_dir(&req.cwd)
        .env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        )
        .env(
            "HOME",
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()),
        )
        .env("LANG", "C");
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    let out = cmd.output()?;
    Ok(ExecResult {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        is_error: !out.status.success(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_echo() {
        let r = light_sandbox_exec(&ExecRequest {
            command: "echo sandbox-ok".into(),
            cwd: std::env::current_dir().unwrap(),
            env: vec![],
        })
        .unwrap();
        assert!(r.stdout.contains("sandbox-ok"));
    }
}
