use crate::sensor::{SensorReport, SensorStatus};
use forge_types::{Message, MessageRole};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairTask {
    pub source: String,
    pub sensor: String,
    pub summary: String,
    pub details_uri: Option<String>,
    pub suggested_steps: Vec<String>,
}

impl RepairTask {
    pub fn from_sensor(r: &SensorReport) -> Self {
        Self {
            source: "sensor".into(),
            sensor: r.sensor.clone(),
            summary: r.summary.clone(),
            details_uri: r.artifact_uri.clone(),
            suggested_steps: if r.status == SensorStatus::Pass {
                vec![]
            } else {
                vec![
                    format!("Address failure from `{}`", r.sensor),
                    "Re-run the sensor command".into(),
                ]
            },
        }
    }

    pub fn to_user_message(&self) -> Message {
        let steps = self
            .suggested_steps
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        Message {
            role: MessageRole::User,
            content: format!(
                "[REPAIR TASK from {}]\nsensor: {}\nsummary: {}\ndetails: {}\nsuggested steps:\n{}",
                self.source,
                self.sensor,
                self.summary,
                self.details_uri.as_deref().unwrap_or("n/a"),
                steps
            ),
            tool_call_id: None,
            name: None,
            thinking: None,
        },
        thinking_duration_secs: None,
}
}

pub fn inject_repair_messages(messages: &mut Vec<Message>, repairs: &[RepairTask]) {
    for r in repairs {
        messages.push(r.to_user_message());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_labels_repair() {
        let mut msgs = vec![];
        inject_repair_messages(
            &mut msgs,
            &[RepairTask {
                source: "evaluator".into(),
                sensor: "cargo test".into(),
                summary: "failed".into(),
                details_uri: Some("file://x".into()),
                suggested_steps: vec!["fix it".into()],
            }],
        );
        assert!(msgs[0].content.contains("[REPAIR TASK"));
        assert!(msgs[0].content.contains("evaluator"));
    }
}
