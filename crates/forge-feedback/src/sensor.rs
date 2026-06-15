use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorStatus {
    Pass,
    Fail,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorReport {
    pub sensor: String,
    pub status: SensorStatus,
    pub exit_code: Option<i32>,
    pub summary: String,
    pub artifact_uri: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SensorContext {
    pub workspace: PathBuf,
}

#[async_trait]
pub trait Sensor: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, ctx: &SensorContext) -> Result<SensorReport, String>;
}

/// Deterministic command sensor (`cargo test`, `npm test`, …).
pub struct CommandSensor {
    command: String,
}

impl CommandSensor {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
        }
    }
}

#[async_trait]
impl Sensor for CommandSensor {
    fn name(&self) -> &str {
        &self.command
    }

    async fn run(&self, ctx: &SensorContext) -> Result<SensorReport, String> {
        let out = Command::new("bash")
            .arg("-lc")
            .arg(&self.command)
            .current_dir(&ctx.workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("sensor spawn failed: {e}"))?;

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut summary = stdout.trim().to_string();
        if !stderr.trim().is_empty() {
            if !summary.is_empty() {
                summary.push('\n');
            }
            summary.push_str(stderr.trim());
        }
        if summary.len() > 2000 {
            summary.truncate(2000);
            summary.push_str("…");
        }
        let code = out.status.code();
        let status = if out.status.success() {
            SensorStatus::Pass
        } else if code.is_none() {
            SensorStatus::Error
        } else {
            SensorStatus::Fail
        };
        // Infra failure (spawn already mapped); non-zero = Fail not silent pass
        Ok(SensorReport {
            sensor: self.command.clone(),
            status,
            exit_code: code,
            summary,
            artifact_uri: None,
        })
    }
}
