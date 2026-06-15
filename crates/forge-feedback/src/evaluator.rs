//! Clean-context evaluator (no Generator history). Deterministic summary from sensors.

use crate::sensor::{SensorReport, SensorStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorConfig {
    pub criteria: String,
    /// Evaluator must not edit files by default (design recommendation).
    #[serde(default)]
    pub allow_writes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
    pub severity: FindingSeverity,
    pub message: String,
    pub suggested_repairs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorReport {
    pub criteria: String,
    pub findings: Vec<Finding>,
    pub clean_context: bool,
}

/// Deterministic evaluator: clean context = only criteria + sensor reports (no chat history).
pub fn evaluate_deterministic(criteria: &str, reports: &[SensorReport]) -> EvaluatorReport {
    let mut findings = Vec::new();
    for r in reports {
        if r.status != SensorStatus::Pass {
            findings.push(Finding {
                code: r.sensor.clone(),
                severity: if r.status == SensorStatus::Error {
                    FindingSeverity::Error
                } else {
                    FindingSeverity::Error
                },
                message: r.summary.clone(),
                suggested_repairs: vec![
                    format!("Investigate sensor `{}`", r.sensor),
                    "Fix failures reported above".into(),
                    "Re-run sensors until green".into(),
                ],
            });
        }
    }
    if findings.is_empty() && !criteria.is_empty() {
        findings.push(Finding {
            code: "criteria".into(),
            severity: FindingSeverity::Info,
            message: format!("Criteria satisfied: {criteria}"),
            suggested_repairs: vec![],
        });
    }
    EvaluatorReport {
        criteria: criteria.into(),
        findings,
        clean_context: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_context_flag_set() {
        let r = evaluate_deterministic(
            "tests pass",
            &[SensorReport {
                sensor: "t".into(),
                status: SensorStatus::Fail,
                exit_code: Some(1),
                summary: "boom".into(),
                artifact_uri: None,
            }],
        );
        assert!(r.clean_context);
        assert_eq!(r.findings.len(), 1);
        assert!(!r.findings[0].suggested_repairs.is_empty());
    }
}
