//! Consent a tool asks for in the middle of a call.
//!
//! Some tools act outside the sandbox on the user's behalf: driving the
//! user's screen, or submitting a form in a browser. Only the tool knows
//! what exactly it is about to do (which display, which site, which
//! fields), so the tool raises the request and the session turns it into an
//! approval. See `forge_tools::SessionConsents` for where grants live.

use serde::{Deserialize, Serialize};

/// What a consent covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentKind {
    /// See the screen and drive the real mouse and keyboard.
    ScreenControl,
    /// Submit a form in the agent's browser.
    FormSubmit,
}

impl ConsentKind {
    /// Stable lower-case name, for journals and audit rows.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScreenControl => "screen_control",
            Self::FormSubmit => "form_submit",
        }
    }

    /// Whether a single-use grant may be offered. Screen control is granted
    /// for a session or not at all.
    pub fn allows_once(self) -> bool {
        matches!(self, Self::FormSubmit)
    }
}

/// One labelled line of what a consent means ("Sees: your primary display").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentFact {
    pub label: String,
    pub value: String,
}

/// A tool's request for consent, shown to the user as an approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRequest {
    pub kind: ConsentKind,
    /// What the grant is scoped to: `screen` for screen control, the site
    /// (`host[:port]`) for a form submit.
    pub scope: String,
    /// What is about to happen, shown in the card's well.
    pub summary: String,
    /// Labelled facts shown under the summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<ConsentFact>,
    /// Why the agent asked, in the agent's words when it gave any.
    #[serde(default)]
    pub reason: String,
}

impl ConsentRequest {
    pub fn new(kind: ConsentKind, scope: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            kind,
            scope: scope.into(),
            summary: summary.into(),
            facts: Vec::new(),
            reason: String::new(),
        }
    }

    pub fn with_fact(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.facts.push(ConsentFact {
            label: label.into(),
            value: value.into(),
        });
        self
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_have_stable_names() {
        assert_eq!(ConsentKind::ScreenControl.as_str(), "screen_control");
        assert_eq!(ConsentKind::FormSubmit.as_str(), "form_submit");
        assert_eq!(
            serde_json::to_value(ConsentKind::ScreenControl).unwrap(),
            "screen_control"
        );
    }

    #[test]
    fn only_a_form_submit_can_be_granted_once() {
        assert!(ConsentKind::FormSubmit.allows_once());
        assert!(!ConsentKind::ScreenControl.allows_once());
    }

    #[test]
    fn a_request_round_trips_and_facts_are_optional() {
        let request = ConsentRequest::new(ConsentKind::FormSubmit, "localhost:5173", "Submit")
            .with_fact("Sends", "email")
            .with_reason("sign up");
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<ConsentRequest>(json).unwrap(),
            request
        );
        let bare: ConsentRequest = serde_json::from_value(serde_json::json!({
            "kind": "screen_control", "scope": "screen", "summary": "s"
        }))
        .unwrap();
        assert!(bare.facts.is_empty());
        assert!(bare.reason.is_empty());
    }
}
