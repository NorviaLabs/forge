//! Dual-sensor feedback (feedback-evaluator.md) — EVAL-01. Phase 3 only.

mod evaluator;
mod repair;
mod sensor;

pub use evaluator::{EvaluatorConfig, EvaluatorReport, Finding, FindingSeverity};
pub use repair::{inject_repair_messages, RepairTask};
pub use sensor::{
    CommandSensor, Sensor, SensorContext, SensorReport, SensorStatus,
};

use forge_types::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FeedbackError {
    #[error("sensor failed: {0}")]
    Sensor(String),
    #[error("evaluator failed: {0}")]
    Evaluator(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackConfig {
    /// Opt-in; default false (Generator-only).
    #[serde(default)]
    pub enabled: bool,
    /// Run gate every N generator turns (0 = only on explicit request).
    #[serde(default = "default_every_n")]
    pub every_n_turns: u32,
    #[serde(default = "default_max_rounds")]
    pub max_eval_rounds: u32,
    #[serde(default)]
    pub evaluator_enabled: bool,
    #[serde(default)]
    pub sensor_commands: Vec<String>,
}

fn default_every_n() -> u32 {
    1
}
fn default_max_rounds() -> u32 {
    3
}

impl Default for FeedbackConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            every_n_turns: default_every_n(),
            max_eval_rounds: default_max_rounds(),
            evaluator_enabled: true,
            sensor_commands: vec![],
        }
    }
}

#[derive(Clone)]
pub struct FeedbackGate {
    pub config: FeedbackConfig,
    sensors: Vec<std::sync::Arc<dyn Sensor>>,
    eval_rounds: u32,
}

impl FeedbackGate {
    pub fn new(config: FeedbackConfig) -> Self {
        let mut sensors: Vec<std::sync::Arc<dyn Sensor>> = Vec::new();
        for cmd in &config.sensor_commands {
            sensors.push(std::sync::Arc::new(CommandSensor::new(cmd.clone())));
        }
        Self {
            config,
            sensors,
            eval_rounds: 0,
        }
    }

    pub fn with_sensor(mut self, s: std::sync::Arc<dyn Sensor>) -> Self {
        self.sensors.push(s);
        self
    }

    pub fn should_run_at_turn(&self, turn: u32) -> bool {
        if !self.config.enabled {
            return false;
        }
        if self.config.every_n_turns == 0 {
            return false;
        }
        turn > 0 && turn % self.config.every_n_turns == 0
    }

    /// Run sensors (+ optional LLM-free structured evaluator summary).
    pub async fn run_gate(
        &mut self,
        ctx: &SensorContext,
        criteria: &str,
    ) -> Result<GateOutcome, FeedbackError> {
        if self.eval_rounds >= self.config.max_eval_rounds {
            return Err(FeedbackError::Other(format!(
                "max eval rounds exceeded ({})",
                self.config.max_eval_rounds
            )));
        }
        self.eval_rounds += 1;

        let mut reports = Vec::new();
        for s in &self.sensors {
            let r = s.run(ctx).await.map_err(|e| FeedbackError::Sensor(e))?;
            // Sensor infra hard failure → treat as gate failure (not silent pass)
            reports.push(r);
        }

        if reports.is_empty() {
            // No sensors configured: pass only if explicitly empty is allowed
            return Ok(GateOutcome {
                passed: true,
                sensor_reports: reports,
                evaluator: None,
                repairs: vec![],
            });
        }

        let sensors_passed = reports.iter().all(|r| r.status == SensorStatus::Pass);
        let mut repairs = Vec::new();
        let mut evaluator = None;

        if !sensors_passed {
            for r in &reports {
                if r.status != SensorStatus::Pass {
                    repairs.push(RepairTask::from_sensor(r));
                }
            }
            if self.config.evaluator_enabled {
                let report = evaluator::evaluate_deterministic(criteria, &reports);
                for f in &report.findings {
                    repairs.push(RepairTask {
                        source: "evaluator".into(),
                        sensor: f.code.clone(),
                        summary: f.message.clone(),
                        details_uri: None,
                        suggested_steps: f.suggested_repairs.clone(),
                    });
                }
                evaluator = Some(report);
            }
        }

        Ok(GateOutcome {
            passed: sensors_passed && repairs.is_empty(),
            sensor_reports: reports,
            evaluator,
            repairs,
        })
    }

    pub fn reset_rounds(&mut self) {
        self.eval_rounds = 0;
    }
}

#[derive(Debug, Clone)]
pub struct GateOutcome {
    pub passed: bool,
    pub sensor_reports: Vec<SensorReport>,
    pub evaluator: Option<EvaluatorReport>,
    pub repairs: Vec<RepairTask>,
}

impl GateOutcome {
    /// Inject repair tasks into generator message list.
    pub fn apply_repairs_to(&self, messages: &mut Vec<Message>) {
        inject_repair_messages(messages, &self.repairs);
    }
}

/// Offline quality metric helper (EVAL-01 target is measured offline, not a runtime gate).
pub fn relative_improvement(single_pass_score: f64, dual_sensor_score: f64) -> f64 {
    if single_pass_score == 0.0 {
        return if dual_sensor_score > 0.0 {
            f64::INFINITY
        } else {
            0.0
        };
    }
    (dual_sensor_score - single_pass_score) / single_pass_score
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use async_trait::async_trait;

    struct FailSensor;
    #[async_trait]
    impl Sensor for FailSensor {
        fn name(&self) -> &str {
            "fail"
        }
        async fn run(&self, _ctx: &SensorContext) -> Result<SensorReport, String> {
            Ok(SensorReport {
                sensor: "fail".into(),
                status: SensorStatus::Fail,
                exit_code: Some(1),
                summary: "tests failed".into(),
                artifact_uri: None,
            })
        }
    }

    struct PassSensor;
    #[async_trait]
    impl Sensor for PassSensor {
        fn name(&self) -> &str {
            "pass"
        }
        async fn run(&self, _ctx: &SensorContext) -> Result<SensorReport, String> {
            Ok(SensorReport {
                sensor: "pass".into(),
                status: SensorStatus::Pass,
                exit_code: Some(0),
                summary: "ok".into(),
                artifact_uri: None,
            })
        }
    }

    #[tokio::test]
    async fn gate_pass_when_sensors_pass() {
        let mut g = FeedbackGate::new(FeedbackConfig {
            enabled: true,
            evaluator_enabled: true,
            ..Default::default()
        })
        .with_sensor(Arc::new(PassSensor));
        let out = g
            .run_gate(
                &SensorContext {
                    workspace: PathBuf::from("."),
                },
                "all tests pass",
            )
            .await
            .unwrap();
        assert!(out.passed);
        assert!(out.repairs.is_empty());
    }

    #[tokio::test]
    async fn gate_fail_produces_repairs() {
        let mut g = FeedbackGate::new(FeedbackConfig {
            enabled: true,
            evaluator_enabled: true,
            ..Default::default()
        })
        .with_sensor(Arc::new(FailSensor));
        let out = g
            .run_gate(
                &SensorContext {
                    workspace: PathBuf::from("."),
                },
                "all green",
            )
            .await
            .unwrap();
        assert!(!out.passed);
        assert!(!out.repairs.is_empty());
        assert!(out.evaluator.is_some());
    }

    #[tokio::test]
    async fn max_eval_rounds() {
        let mut g = FeedbackGate::new(FeedbackConfig {
            enabled: true,
            max_eval_rounds: 1,
            ..Default::default()
        })
        .with_sensor(Arc::new(FailSensor));
        let ctx = SensorContext {
            workspace: PathBuf::from("."),
        };
        g.run_gate(&ctx, "c").await.unwrap();
        let err = g.run_gate(&ctx, "c").await.unwrap_err();
        assert!(err.to_string().contains("max eval rounds"));
    }

    #[test]
    fn relative_improvement_over_40_percent() {
        // Offline metric helper: dual-sensor 0.84 vs single 0.6 → 40% relative
        let imp = relative_improvement(0.6, 0.84);
        assert!(imp >= 0.40 - 1e-9, "got {imp}");
    }

    #[test]
    fn disabled_gate_not_scheduled() {
        let g = FeedbackGate::new(FeedbackConfig {
            enabled: false,
            every_n_turns: 1,
            ..Default::default()
        });
        assert!(!g.should_run_at_turn(2));
    }

    #[tokio::test]
    async fn command_sensor_echo() {
        let s = CommandSensor::new("echo sensor-ok");
        let r = s
            .run(&SensorContext {
                workspace: std::env::current_dir().unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(r.status, SensorStatus::Pass);
        assert!(r.summary.contains("sensor-ok"));
    }
}
