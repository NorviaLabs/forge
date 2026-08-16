//! The structured state checkpoint: schema, parsing, validation, and the
//! canonical rendering that goes back into model-visible context.
//!
//! A checkpoint is a snapshot of *current task state*, not a narrative
//! summary of the conversation. It is deliberately a small tagged format
//! rather than JSON: models produce it reliably without escaping, and a
//! partial/truncated response fails the root-tag check instead of silently
//! parsing into a half-empty object.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CHECKPOINT_VERSION: u32 = 1;
pub const CHECKPOINT_ROOT: &str = "forge_checkpoint";

/// Every section the schema defines, in canonical render order.
pub const CHECKPOINT_SECTIONS: &[&str] = &[
    "objective",
    "user_constraints",
    "decisions",
    "completed",
    "current_work",
    "files",
    "symbols",
    "commands_and_results",
    "failures",
    "pending",
    "next_action",
];

/// Sections a checkpoint must carry to be installable. Everything else is
/// legitimately empty for some tasks (a read-only session has no `failures`),
/// but a checkpoint with no objective and no next action cannot let another
/// agent continue, which is the whole contract.
pub const REQUIRED_SECTIONS: &[&str] = &["objective", "next_action"];

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckpointError {
    #[error("checkpoint response was empty")]
    Empty,
    #[error("no <{CHECKPOINT_ROOT}> element found in the model response")]
    MissingRoot,
    #[error("unterminated <{CHECKPOINT_ROOT}> element")]
    UnterminatedRoot,
    #[error("checkpoint has no populated sections")]
    NoSections,
    #[error("checkpoint is missing required section <{0}>")]
    MissingSection(String),
    #[error("unsupported checkpoint version {0}")]
    UnsupportedVersion(u32),
}

/// A parsed, validated state checkpoint.
///
/// Sections are stored by name so the schema can grow without a struct
/// migration; `CHECKPOINT_SECTIONS` defines render order and
/// `REQUIRED_SECTIONS` defines validity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub version: u32,
    /// Section name → trimmed body. Only populated sections are kept.
    pub sections: BTreeMap<String, String>,
}

impl Checkpoint {
    /// Extract and validate a checkpoint from a raw model response.
    ///
    /// The model may wrap the element in prose or a code fence; only the
    /// root element is read, and any text around it is discarded.
    pub fn parse(response: &str) -> Result<Self, CheckpointError> {
        if response.trim().is_empty() {
            return Err(CheckpointError::Empty);
        }
        let open_start = find_root_open(response).ok_or(CheckpointError::MissingRoot)?;
        let open_end = response[open_start..]
            .find('>')
            .map(|offset| open_start + offset + 1)
            .ok_or(CheckpointError::UnterminatedRoot)?;
        let close_tag = format!("</{CHECKPOINT_ROOT}>");
        let close_start = response[open_end..]
            .find(&close_tag)
            .map(|offset| open_end + offset)
            .ok_or(CheckpointError::UnterminatedRoot)?;

        let version = parse_version(&response[open_start..open_end]).unwrap_or(CHECKPOINT_VERSION);
        if version != CHECKPOINT_VERSION {
            return Err(CheckpointError::UnsupportedVersion(version));
        }

        let body = &response[open_end..close_start];
        let mut sections = BTreeMap::new();
        for name in CHECKPOINT_SECTIONS {
            if let Some(text) = extract_section(body, name) {
                let text = text.trim();
                if !text.is_empty() && !is_placeholder(text) {
                    sections.insert((*name).to_string(), text.to_string());
                }
            }
        }
        let checkpoint = Self { version, sections };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn validate(&self) -> Result<(), CheckpointError> {
        if self.version != CHECKPOINT_VERSION {
            return Err(CheckpointError::UnsupportedVersion(self.version));
        }
        if self.sections.is_empty() {
            return Err(CheckpointError::NoSections);
        }
        for required in REQUIRED_SECTIONS {
            if !self.sections.contains_key(*required) {
                return Err(CheckpointError::MissingSection((*required).to_string()));
            }
        }
        Ok(())
    }

    pub fn section(&self, name: &str) -> Option<&str> {
        self.sections.get(name).map(String::as_str)
    }

    /// True when the checkpoint records at least one explicit user
    /// constraint. Used to verify that protected facts survived compaction.
    pub fn has_user_constraints(&self) -> bool {
        self.section("user_constraints").is_some()
    }

    /// Canonical serialization — deterministic section order, only populated
    /// sections. This is what is installed into model-visible context, and
    /// what a later compaction reads back as existing structured state.
    pub fn render(&self) -> String {
        let mut out = format!("<{CHECKPOINT_ROOT} version=\"{}\">\n", self.version);
        for name in CHECKPOINT_SECTIONS {
            let Some(body) = self.sections.get(*name) else {
                continue;
            };
            out.push_str(&format!("\n<{name}>\n{body}\n</{name}>\n"));
        }
        out.push_str(&format!("\n</{CHECKPOINT_ROOT}>"));
        out
    }
}

/// Locate `<forge_checkpoint` allowing for attributes, ignoring the closing tag.
fn find_root_open(text: &str) -> Option<usize> {
    let needle = format!("<{CHECKPOINT_ROOT}");
    let mut from = 0usize;
    while let Some(offset) = text[from..].find(&needle) {
        let start = from + offset;
        let after = text[start + needle.len()..].chars().next();
        if matches!(after, Some(c) if c == '>' || c.is_whitespace()) {
            return Some(start);
        }
        from = start + needle.len();
    }
    None
}

fn parse_version(open_tag: &str) -> Option<u32> {
    let key = "version=";
    let start = open_tag.find(key)? + key.len();
    let rest = open_tag[start..].trim_start_matches(['"', '\'']);
    let end = rest.find(['"', '\'', ' ', '>']).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_section<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(&body[start..end])
}

/// Models sometimes echo the schema's own guidance text back for a section
/// they have nothing to say about. Treating that as content would make an
/// empty checkpoint look populated.
fn is_placeholder(text: &str) -> bool {
    matches!(
        text.trim()
            .trim_end_matches('.')
            .to_ascii_lowercase()
            .as_str(),
        "none" | "n/a" | "na" | "nothing" | "empty" | "-" | "(none)" | "unknown"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_checkpoint() -> String {
        r#"Here is the state:

<forge_checkpoint version="1">
<objective>
Add context compaction to Forge.
</objective>
<user_constraints>
Do not change the public API.
</user_constraints>
<decisions>
Checkpoint installs as a system message.
</decisions>
<completed>
Audited the caching architecture.
</completed>
<current_work>
Writing the tail selector.
</current_work>
<files>
crates/forge-context/src/compaction/mod.rs
</files>
<symbols>
CompactionPolicy, TailSelector
</symbols>
<commands_and_results>
cargo test --workspace — green
</commands_and_results>
<failures>
None
</failures>
<pending>
Wire the slash command.
</pending>
<next_action>
Add the transactional install path.
</next_action>
</forge_checkpoint>

That's everything."#
            .to_string()
    }

    #[test]
    fn parses_a_full_checkpoint_out_of_surrounding_prose() {
        let checkpoint = Checkpoint::parse(&full_checkpoint()).unwrap();
        assert_eq!(checkpoint.version, 1);
        assert_eq!(
            checkpoint.section("objective"),
            Some("Add context compaction to Forge.")
        );
        assert_eq!(
            checkpoint.section("user_constraints"),
            Some("Do not change the public API.")
        );
        assert!(checkpoint.has_user_constraints());
        // `<failures>None</failures>` is a placeholder, not content.
        assert_eq!(checkpoint.section("failures"), None);
    }

    #[test]
    fn render_round_trips_through_parse() {
        let checkpoint = Checkpoint::parse(&full_checkpoint()).unwrap();
        let reparsed = Checkpoint::parse(&checkpoint.render()).unwrap();
        assert_eq!(checkpoint, reparsed);
        // Deterministic: rendering twice is byte-identical.
        assert_eq!(checkpoint.render(), reparsed.render());
    }

    #[test]
    fn rejects_empty_missing_and_truncated_checkpoints() {
        assert_eq!(Checkpoint::parse("   "), Err(CheckpointError::Empty));
        assert_eq!(
            Checkpoint::parse("I could not summarize that."),
            Err(CheckpointError::MissingRoot)
        );
        assert_eq!(
            Checkpoint::parse("<forge_checkpoint version=\"1\">\n<objective>x</objective>"),
            Err(CheckpointError::UnterminatedRoot)
        );
    }

    #[test]
    fn rejects_a_checkpoint_missing_a_required_section() {
        let text =
            "<forge_checkpoint version=\"1\">\n<completed>stuff</completed>\n</forge_checkpoint>";
        assert_eq!(
            Checkpoint::parse(text),
            Err(CheckpointError::MissingSection("objective".into()))
        );
    }

    #[test]
    fn rejects_a_structurally_valid_but_contentless_checkpoint() {
        let text =
            "<forge_checkpoint version=\"1\">\n<objective>none</objective>\n<next_action>N/A</next_action>\n</forge_checkpoint>";
        assert_eq!(Checkpoint::parse(text), Err(CheckpointError::NoSections));
    }

    #[test]
    fn rejects_an_unknown_schema_version() {
        let text = "<forge_checkpoint version=\"7\">\n<objective>x</objective>\n<next_action>y</next_action>\n</forge_checkpoint>";
        assert_eq!(
            Checkpoint::parse(text),
            Err(CheckpointError::UnsupportedVersion(7))
        );
    }
}
