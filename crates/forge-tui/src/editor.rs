//! External editor resolution and argument parsing.

use std::env;
use std::ffi::OsString;

/// Resolve the external editor command from the environment.
///
/// Returns `(command, args)` where `command` is the executable path
/// and `args` are the fixed arguments (flags) from the variable value.
///
/// Resolution order:
/// 1. `$VISUAL` when set and non-empty
/// 2. `$EDITOR` otherwise
/// 3. `None` when neither is set
pub fn resolve_editor() -> Option<(String, Vec<String>)> {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|name| env::var_os(name))
        .find_map(parse_editor_value)
}

fn parse_editor_value(raw: OsString) -> Option<(String, Vec<String>)> {
    split_editor_value(&raw.to_string_lossy())
}

fn split_editor_value(raw: &str) -> Option<(String, Vec<String>)> {
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut quote = None;
    let mut escaped = false;

    for character in raw.trim().chars() {
        if escaped {
            part.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            } else {
                part.push(character);
            }
        } else if character.is_whitespace() && quote.is_none() {
            if !part.is_empty() {
                parts.push(std::mem::take(&mut part));
            }
        } else {
            part.push(character);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !part.is_empty() {
        parts.push(part);
    }
    let (command, args) = parts.split_first()?;
    Some((command.clone(), args.to_vec()))
}

/// Error type for external editor resolution.
#[derive(Debug)]
pub enum EditorError {
    NotConfigured,
    SpawnFailed(std::io::Error),
}

impl std::fmt::Display for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => write!(
                f,
                "No external editor configured\n\nSet VISUAL or EDITOR and try again."
            ),
            Self::SpawnFailed(e) => write!(f, "Unable to start editor\n\n{}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_splitting_handles_command_with_flags() {
        let result = split_editor_value("code --wait");
        assert_eq!(result, Some(("code".into(), vec!["--wait".to_string()])));
    }

    #[test]
    fn argument_splitting_handles_plain_command() {
        let result = split_editor_value("nvim");
        assert_eq!(result, Some(("nvim".into(), vec![])));
    }

    #[test]
    fn argument_splitting_rejects_empty() {
        assert!(split_editor_value("").is_none());
        assert!(split_editor_value("   ").is_none());
    }

    #[test]
    fn argument_splitting_handles_quoted_executable_and_arguments() {
        let result = split_editor_value("'/Applications/My Editor' --wait \"a b\"");
        assert_eq!(
            result,
            Some((
                "/Applications/My Editor".into(),
                vec!["--wait".to_string(), "a b".to_string()]
            ))
        );
    }

    #[test]
    fn argument_splitting_rejects_unclosed_quotes() {
        assert!(split_editor_value("code --wait '").is_none());
    }

    #[test]
    fn argument_splitting_handles_escapes_and_nested_quotes() {
        let result = split_editor_value(r#"code a\ b "quoted 'inner'" --flag"#);
        assert_eq!(
            result,
            Some((
                "code".into(),
                vec![
                    "a b".to_string(),
                    "quoted 'inner'".to_string(),
                    "--flag".to_string()
                ]
            ))
        );
        assert!(split_editor_value(r#"code \"#).is_none());
    }

    #[test]
    fn editor_error_display_messages_are_actionable() {
        assert!(EditorError::NotConfigured
            .to_string()
            .contains("Set VISUAL or EDITOR"));
        let err =
            EditorError::SpawnFailed(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
        assert!(err.to_string().contains("Unable to start editor"));
        assert!(err.to_string().contains("missing"));
    }
}
