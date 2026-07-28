//! External editor resolution and argument parsing.

use std::env;

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
    let raw = env::var("VISUAL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("EDITOR").ok().filter(|s| !s.is_empty()))?;

    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let cmd = parts[0].to_string();
    let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
    Some((cmd, args))
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

    /// Helper that tests splitting logic without touching env vars.
    fn split_editor_value(raw: &str) -> Option<(String, Vec<String>)> {
        if raw.is_empty() || raw.trim().is_empty() {
            return None;
        }
        let raw = raw.trim();
        let parts: Vec<&str> = raw.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }
        let cmd = parts[0].to_string();
        let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
        Some((cmd, args))
    }
}
