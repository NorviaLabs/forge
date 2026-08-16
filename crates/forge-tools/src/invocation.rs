//! Human-readable invocation strings for built-in tool calls, rendered under
//! the tool label in the TUI conversation panel (TUI-02). Each formatter reads
//! the same JSON keys its tool's args struct deserializes.

use serde_json::Value;

/// Format the invocation of a built-in tool call for display under the tool
/// card label. Returns `None` for tools with no natural invocation
/// (`apply_patch`, `write_stdin`, `update_plan`) and for unknown/MCP tools.
pub fn tool_invocation(name: &str, args: &Value) -> Option<String> {
    match name {
        "read_file" | "write_file" | "view_image" => str_arg(args, "path"),
        "ls" => Some(str_arg(args, "path").unwrap_or_else(|| ".".into())),
        "bash" | "background_run" => str_arg(args, "command").map(|c| format!("$ {c}")),
        "exec_command" => str_arg(args, "cmd").map(|c| format!("$ {c}")),
        "git" => {
            let subcommand = str_arg(args, "subcommand")?;
            let args = arr_args(args, "args");
            let mut line = format!("git {subcommand}");
            for arg in args {
                line.push(' ');
                line.push_str(&arg);
            }
            Some(line)
        }
        "glob" | "web_search" => str_arg(args, "query"),
        "grep" => str_arg(args, "pattern"),
        "load_skill" => str_arg(args, "name"),
        _ => None,
    }
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn arr_args(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_command_tools() {
        assert_eq!(
            tool_invocation("bash", &json!({"command": "cargo test --lib"})),
            Some("$ cargo test --lib".into())
        );
        assert_eq!(
            tool_invocation("exec_command", &json!({"cmd": "make build"})),
            Some("$ make build".into())
        );
    }

    #[test]
    fn formats_path_and_query_tools() {
        assert_eq!(
            tool_invocation("read_file", &json!({"path": "src/main.rs"})),
            Some("src/main.rs".into())
        );
        assert_eq!(
            tool_invocation("ls", &json!({"path": "crates"})),
            Some("crates".into())
        );
        assert_eq!(tool_invocation("ls", &json!({})), Some(".".into()));
        assert_eq!(
            tool_invocation("view_image", &json!({"path": "docs/shot.png"})),
            Some("docs/shot.png".into())
        );
        assert_eq!(
            tool_invocation("glob", &json!({"query": "tokio"})),
            Some("tokio".into())
        );
        assert_eq!(
            tool_invocation("grep", &json!({"pattern": "async"})),
            Some("async".into())
        );
        assert_eq!(
            tool_invocation("web_search", &json!({"query": "tokio docs"})),
            Some("tokio docs".into())
        );
    }

    #[test]
    fn formats_git_with_args() {
        assert_eq!(
            tool_invocation("git", &json!({"subcommand": "status", "args": ["--short"]})),
            Some("git status --short".into())
        );
        assert_eq!(
            tool_invocation(
                "git",
                &json!({"subcommand": "log", "args": ["-1", "--oneline"]})
            ),
            Some("git log -1 --oneline".into())
        );
        assert_eq!(
            tool_invocation("git", &json!({"subcommand": "status"})),
            Some("git status".into())
        );
    }

    #[test]
    fn render_only_and_unknown_tools_return_none() {
        assert_eq!(tool_invocation("apply_patch", &json!({})), None);
        assert_eq!(tool_invocation("write_stdin", &json!({})), None);
        assert_eq!(tool_invocation("update_plan", &json!({})), None);
        assert_eq!(tool_invocation("mcp_whatever", &json!({})), None);
    }
}
