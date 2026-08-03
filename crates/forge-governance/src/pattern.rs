//! Pattern-based rules that narrow the `hitl_tools`/`hitl_classes` gate in
//! [`crate::Governance::authorize`], matched against the actual call
//! (command string for shell-like tools, path for file tools, host for
//! fetch-like tools) rather than just the tool name.
//!
//! These rules are strictly a narrowing of an already-gated call: they can
//! turn a `Hitl` decision into `Allow` (an `allow` rule) or hold it at
//! `Hitl` even where a broader `allow` rule would otherwise match (a `deny`
//! rule, for carving out an exception like "allow `cargo *` but still ask
//! for `cargo publish`"). A pattern rule can never turn a call into
//! `PolicyDecision::Deny`, and never applies to a call that wasn't gated in
//! the first place — that stays [`crate::AclPolicy`]'s job, checked first,
//! unconditionally, same as today.

use forge_types::ToolCall;

/// A single parsed `tool(pattern)` (or bare `tool`) rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternRule {
    pub raw: String,
    tool: String,
    argument_pattern: Option<String>,
}

impl PatternRule {
    /// Parse `tool(argument-pattern)` or a bare `tool`, which matches any
    /// call to that tool regardless of arguments. Returns `None` for a rule
    /// that can't be parsed (empty tool name, unbalanced parens) so callers
    /// can skip and report a malformed line rather than fail closed on the
    /// whole file.
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (tool, argument_pattern) = match trimmed.strip_suffix(')') {
            Some(rest) => {
                let (tool, pattern) = rest.split_once('(')?;
                (tool.trim(), Some(pattern.trim().to_string()))
            }
            None if trimmed.contains('(') => return None,
            None => (trimmed, None),
        };
        if tool.is_empty() {
            return None;
        }
        Some(Self {
            raw: trimmed.to_string(),
            tool: tool.to_string(),
            argument_pattern,
        })
    }

    pub fn matches(&self, call: &ToolCall) -> bool {
        if self.tool != call.name {
            return false;
        }
        let Some(pattern) = &self.argument_pattern else {
            return true;
        };
        match subject_for(&call.name, &call.arguments) {
            Some(subject) => glob_match_anywhere(pattern, &subject),
            None => false,
        }
    }
}

/// Parse a list of raw pattern strings, silently dropping ones that don't
/// parse (a malformed line in a hand-edited file shouldn't take down every
/// other rule in it).
pub fn parse_pattern_rules<S: AsRef<str>>(raw: &[S]) -> Vec<PatternRule> {
    raw.iter()
        .filter_map(|s| PatternRule::parse(s.as_ref()))
        .collect()
}

fn subject_for(tool: &str, args: &serde_json::Value) -> Option<String> {
    if is_shell_tool(tool) {
        return args
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
    }
    if is_file_tool(tool) {
        return args.get("path").and_then(|v| v.as_str()).map(str::to_owned);
    }
    if is_fetch_tool(tool) {
        return args
            .get("url")
            .and_then(|v| v.as_str())
            .and_then(extract_host);
    }
    None
}

fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool,
        "bash" | "sh" | "shell" | "cmd" | "powershell" | "exec"
    )
}

fn is_file_tool(tool: &str) -> bool {
    tool.contains("file") || tool == "apply_patch"
}

fn is_fetch_tool(tool: &str) -> bool {
    tool.contains("fetch")
        || tool.contains("http")
        || tool.contains("web")
        || matches!(tool, "curl" | "wget")
}

fn extract_host(url: &str) -> Option<String> {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = without_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Lite glob: a single wildcard matches any run of characters, supporting a
/// leading and/or trailing `*` (`cargo test *`, `*.env`, `src/**`). This is
/// not a full glob engine — consecutive `*`s collapse to one, so `**` behaves
/// identically to `*` rather than distinguishing recursive-vs-single-segment
/// matches. That's enough for the patterns this file format targets without
/// pulling in a dedicated glob dependency.
fn glob_match_anywhere(pattern: &str, subject: &str) -> bool {
    let mut collapsed = String::with_capacity(pattern.len());
    let mut last_was_star = false;
    for ch in pattern.chars() {
        if ch == '*' {
            if !last_was_star {
                collapsed.push(ch);
            }
            last_was_star = true;
        } else {
            collapsed.push(ch);
            last_was_star = false;
        }
    }
    match collapsed.split_once('*') {
        None => collapsed == subject,
        Some((prefix, suffix)) => {
            subject.starts_with(prefix)
                && subject.ends_with(suffix)
                && subject.len() >= prefix.len() + suffix.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[test]
    fn bare_tool_matches_regardless_of_arguments() {
        let rule = PatternRule::parse("background_run").unwrap();
        assert!(rule.matches(&call("background_run", json!({"command": "anything"}))));
        assert!(!rule.matches(&call("bash", json!({"command": "anything"}))));
    }

    #[test]
    fn shell_tool_pattern_matches_command_prefix() {
        let rule = PatternRule::parse("bash(cargo test *)").unwrap();
        assert!(rule.matches(&call("bash", json!({"command": "cargo test --all"}))));
        assert!(!rule.matches(&call("bash", json!({"command": "cargo publish"}))));
        assert!(!rule.matches(&call("bash", json!({"command": "rm -rf /"}))));
    }

    #[test]
    fn shell_tool_pattern_requires_a_command_argument() {
        let rule = PatternRule::parse("bash(cargo test *)").unwrap();
        assert!(!rule.matches(&call("bash", json!({}))));
        assert!(!rule.matches(&call("bash", json!({"command": 42}))));
    }

    #[test]
    fn file_tool_pattern_matches_path_glob() {
        let rule = PatternRule::parse("write_file(src/**)").unwrap();
        assert!(rule.matches(&call("write_file", json!({"path": "src/lib.rs"}))));
        assert!(!rule.matches(&call("write_file", json!({"path": ".env"}))));
    }

    #[test]
    fn fetch_tool_pattern_matches_host_with_subdomain_wildcard() {
        let rule = PatternRule::parse("fetch(*.example.com)").unwrap();
        assert!(rule.matches(&call(
            "fetch",
            json!({"url": "https://docs.example.com/page"})
        )));
        assert!(!rule.matches(&call("fetch", json!({"url": "https://example.com/page"}))));
        assert!(!rule.matches(&call(
            "fetch",
            json!({"url": "https://attacker.example.net"})
        )));
    }

    #[test]
    fn fetch_tool_pattern_ignores_userinfo_and_port() {
        let rule = PatternRule::parse("fetch(example.com)").unwrap();
        assert!(rule.matches(&call(
            "fetch",
            json!({"url": "https://user:pass@example.com:8443/x"})
        )));
    }

    #[test]
    fn unparseable_patterns_are_dropped_not_fatal() {
        let rules = parse_pattern_rules(&["bash(cargo test *)", "", "(no-tool)", "   "]);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].raw, "bash(cargo test *)");
    }

    #[test]
    fn parse_rejects_unbalanced_parens() {
        assert!(PatternRule::parse("bash(cargo test *").is_none());
    }
}
