//! Task kinds: what ran a turn.
//!
//! Every turn record names the kind of task that ran it, so models can be
//! compared per kind (`forge models report`):
//! - `main` — a top-level session;
//! - `subagent` — a built-in child, writer or read-only alike;
//! - `custom:<name>` — a child started as the named custom agent, in either
//!   mode.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What ran a turn. Stored and asked for by its text form (`main`,
/// `subagent`, `custom:<name>`); the derived order is the order reports list
/// kinds in: main, subagent, then custom agents by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TaskKind {
    Main,
    Subagent,
    Custom(String),
}

const CUSTOM_PREFIX: &str = "custom:";

/// Longest custom agent name. Mirrors `forge_config::agents`, which owns the
/// agent file format (this crate sits below it).
const AGENT_NAME_MAX_LEN: usize = 64;

/// Custom agent names as `forge_config::agents::validate_name` accepts them:
/// 1–64 lowercase letters, digits and single hyphens, starting and ending
/// with a letter or digit.
fn valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= AGENT_NAME_MAX_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// A task kind's text was not `main`, `subagent` or `custom:<agent name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskKindError(String);

impl fmt::Display for TaskKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a task kind: use `main`, `subagent` or `custom:<agent name>`",
            self.0
        )
    }
}

impl std::error::Error for TaskKindError {}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Main => f.write_str("main"),
            Self::Subagent => f.write_str("subagent"),
            Self::Custom(name) => write!(f, "{CUSTOM_PREFIX}{name}"),
        }
    }
}

impl FromStr for TaskKind {
    type Err = TaskKindError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "main" => Ok(Self::Main),
            "subagent" => Ok(Self::Subagent),
            _ => match text.strip_prefix(CUSTOM_PREFIX) {
                Some(name) if valid_agent_name(name) => Ok(Self::Custom(name.to_string())),
                _ => Err(TaskKindError(text.to_string())),
            },
        }
    }
}

impl TaskKind {
    /// The kind of a session's turns, from how it was started: a child
    /// started as the custom agent `agent` is `custom:<agent>`; any other
    /// child (or one whose agent name is not a valid one) `subagent`; a
    /// top-level session `main`. The one rule: for a session being built,
    /// and for one — or a turn record — read back from its journal.
    pub fn of_session(is_child: bool, agent: Option<&str>) -> Self {
        if !is_child {
            return Self::Main;
        }
        match agent {
            Some(name) if valid_agent_name(name) => Self::Custom(name.to_string()),
            _ => Self::Subagent,
        }
    }
}

impl Serialize for TaskKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TaskKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one rule for what a session's turns are recorded as.
    #[test]
    fn a_sessions_kind_is_how_it_was_started() {
        assert_eq!(TaskKind::of_session(false, None), TaskKind::Main);
        assert_eq!(TaskKind::of_session(true, None), TaskKind::Subagent);
        assert_eq!(
            TaskKind::of_session(true, Some("reviewer")),
            TaskKind::Custom("reviewer".into())
        );
        // A name no agent can have is not a kind: the child is a subagent.
        assert_eq!(
            TaskKind::of_session(true, Some("Not A Name")),
            TaskKind::Subagent
        );
        // Only a child runs as an agent.
        assert_eq!(
            TaskKind::of_session(false, Some("reviewer")),
            TaskKind::Main
        );
    }
}
