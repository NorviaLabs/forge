//! Command-aware compression for common dev-tool output.
//!
//! Applied before `ContextEngine::maybe_offload_tool_content`'s generic
//! size-threshold offload, so a well-known output shape keeps its signal
//! instead of being cut off at an arbitrary byte offset — the first 500
//! characters of a failing `cargo test` run are rarely the useful part.
//!
//! Both filters here are deliberately conservative: they only ever drop
//! lines a human skimming the same output would also skip past (a test that
//! passed, a diff's own bookkeeping headers), never a line carrying content
//! someone would actually read. Neither filter runs when it would collapse
//! nothing, so recognizing a command never *adds* text to short output.

use serde_json::Value;

/// Recognizes the command behind `bash`, `exec_command`, `background_run`,
/// and the structured `git` tool, and applies the matching filter to
/// `output`. Returns `None` when the tool/command isn't recognized, or the
/// matching filter found nothing to collapse — either way, the caller
/// passes `output` through unmodified.
pub fn compress_tool_output(tool_name: &str, args: &Value, output: &str) -> Option<String> {
    let cmd = shell_command_text(tool_name, args)?;
    compress_command_output(&cmd, output)
}

/// The command line a tool call actually ran, reconstructed from its
/// arguments — `bash`/`background_run`'s `command`, `exec_command`'s `cmd`,
/// or `git`'s `subcommand` + `args` rejoined into one line.
fn shell_command_text(tool_name: &str, args: &Value) -> Option<String> {
    match tool_name {
        "bash" | "background_run" => args
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string),
        "exec_command" => args.get("cmd").and_then(Value::as_str).map(str::to_string),
        "git" => {
            let subcommand = args.get("subcommand").and_then(Value::as_str)?;
            let extra: Vec<&str> = args
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            Some(format!("git {subcommand} {}", extra.join(" ")))
        }
        _ => None,
    }
}

const TEST_COMMAND_PREFIXES: &[&str] = &[
    "cargo test",
    "cargo nextest",
    "pytest",
    "python -m pytest",
    "python3 -m pytest",
    "npm test",
    "npm run test",
    "yarn test",
    "pnpm test",
    "go test",
    "jest",
];

fn compress_command_output(cmd: &str, output: &str) -> Option<String> {
    let cmd = cmd.trim_start();
    if TEST_COMMAND_PREFIXES.iter().any(|p| cmd.starts_with(p)) {
        return collapse_passing_tests(output);
    }
    // `--stat`/`--numstat`/`--name-only`/`--shortstat` are already one line
    // per file — stripping headers from an already-minimal summary has
    // nothing to gain and a real chance of mangling it.
    if cmd.starts_with("git diff")
        && !cmd.contains("--stat")
        && !cmd.contains("--numstat")
        && !cmd.contains("--name-only")
        && !cmd.contains("--shortstat")
    {
        return strip_diff_headers(output);
    }
    None
}

/// True for a line that unambiguously reports one passing test, across the
/// handful of output shapes `cargo test`, `pytest -v`, `go test -v`, and
/// jest/mocha all use. Deliberately narrow — a marker that doesn't match one
/// of these exact shapes is kept, not guessed at.
fn is_passing_test_line(line: &str) -> bool {
    let trimmed = line.trim();
    (trimmed.starts_with("test ") && trimmed.ends_with("... ok"))
        || trimmed.starts_with("--- PASS:")
        || trimmed.ends_with("PASSED")
        || trimmed.starts_with("✓ ")
        || trimmed.starts_with("PASS ")
}

/// Drops every passing-test line, keeping everything else (failures, their
/// stack traces, and summary footers like `test result: FAILED. 3 passed;
/// 2 failed`) untouched, with one line noting how many were collapsed.
fn collapse_passing_tests(output: &str) -> Option<String> {
    let mut kept = Vec::new();
    let mut collapsed = 0usize;
    for line in output.lines() {
        if is_passing_test_line(line) {
            collapsed += 1;
        } else {
            kept.push(line);
        }
    }
    if collapsed == 0 {
        return None;
    }
    kept.push("");
    let note = format!("[{collapsed} passing test line(s) collapsed]");
    kept.push(&note);
    Some(kept.join("\n"))
}

/// Drops `diff --git a/x b/x` and `index <hash>..<hash> <mode>` lines — pure
/// bookkeeping that duplicates the `--- a/x` / `+++ b/x` lines immediately
/// below them. Every hunk header, context, and changed line is untouched.
fn strip_diff_headers(output: &str) -> Option<String> {
    let mut kept = Vec::new();
    let mut dropped = 0usize;
    for line in output.lines() {
        if line.starts_with("diff --git ") || line.starts_with("index ") {
            dropped += 1;
        } else {
            kept.push(line);
        }
    }
    if dropped == 0 {
        return None;
    }
    Some(kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_command_text_reads_bash_and_background_run_command_field() {
        for name in ["bash", "background_run"] {
            let args = serde_json::json!({"command": "cargo test"});
            assert_eq!(
                shell_command_text(name, &args).as_deref(),
                Some("cargo test")
            );
        }
    }

    #[test]
    fn shell_command_text_reads_exec_command_cmd_field() {
        let args = serde_json::json!({"cmd": "cargo test --workspace"});
        assert_eq!(
            shell_command_text("exec_command", &args).as_deref(),
            Some("cargo test --workspace")
        );
    }

    #[test]
    fn shell_command_text_rebuilds_git_subcommand_and_args() {
        let args = serde_json::json!({"subcommand": "diff", "args": ["--stat", "HEAD~1"]});
        assert_eq!(
            shell_command_text("git", &args).as_deref(),
            Some("git diff --stat HEAD~1")
        );
    }

    #[test]
    fn shell_command_text_is_none_for_an_unrecognized_tool() {
        let args = serde_json::json!({"path": "src/lib.rs"});
        assert!(shell_command_text("read_file", &args).is_none());
    }

    #[test]
    fn collapses_cargo_test_passes_and_keeps_failure_context() {
        let output = "\
running 3 tests
test foo::a ... ok
test foo::b ... FAILED
test foo::c ... ok

failures:

---- foo::b stdout ----
thread panicked at 'assertion failed'

failures:
    foo::b

test result: FAILED. 2 passed; 1 failed";
        let compressed = compress_command_output("cargo test", output).unwrap();
        assert!(!compressed.contains("test foo::a ... ok"));
        assert!(!compressed.contains("test foo::c ... ok"));
        assert!(compressed.contains("test foo::b ... FAILED"));
        assert!(compressed.contains("assertion failed"));
        assert!(compressed.contains("test result: FAILED. 2 passed; 1 failed"));
        assert!(compressed.contains("[2 passing test line(s) collapsed]"));
    }

    #[test]
    fn recognizes_pytest_go_test_and_jest_pass_markers() {
        assert!(is_passing_test_line("test_foo.py::test_bar PASSED"));
        assert!(is_passing_test_line("--- PASS: TestFoo (0.00s)"));
        assert!(is_passing_test_line("✓ renders the button (12ms)"));
        assert!(is_passing_test_line("PASS src/foo.test.js"));
        assert!(!is_passing_test_line("test_foo.py::test_bar FAILED"));
        assert!(!is_passing_test_line("--- FAIL: TestFoo (0.00s)"));
    }

    #[test]
    fn all_passing_output_collapses_to_just_the_summary_line() {
        let output = "test a ... ok\ntest b ... ok\ntest result: ok. 2 passed; 0 failed";
        let compressed = compress_command_output("cargo test", output).unwrap();
        assert!(!compressed.contains("test a ... ok"));
        assert!(compressed.contains("test result: ok. 2 passed; 0 failed"));
        assert!(compressed.contains("[2 passing test line(s) collapsed]"));
    }

    #[test]
    fn no_passing_lines_returns_none_rather_than_touching_output() {
        let output = "error: could not compile `demo`";
        assert!(compress_command_output("cargo test", output).is_none());
    }

    #[test]
    fn unrecognized_command_returns_none() {
        let output = "hello world";
        assert!(compress_command_output("echo hi", output).is_none());
    }

    #[test]
    fn strips_git_diff_bookkeeping_headers_but_keeps_hunks() {
        let output = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
-old line
+new line
 context line";
        let compressed = compress_command_output("git diff", output).unwrap();
        assert!(!compressed.contains("diff --git"));
        assert!(!compressed.contains("index 1234567"));
        assert!(compressed.contains("--- a/src/lib.rs"));
        assert!(compressed.contains("+++ b/src/lib.rs"));
        assert!(compressed.contains("-old line"));
        assert!(compressed.contains("+new line"));
        assert!(compressed.contains(" context line"));
    }

    #[test]
    fn git_diff_stat_is_left_untouched() {
        let output = " src/lib.rs | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)";
        assert!(compress_command_output("git diff --stat", output).is_none());
    }

    #[test]
    fn compress_tool_output_wires_git_tool_args_through_to_the_diff_filter() {
        let args = serde_json::json!({"subcommand": "diff", "args": []});
        let output =
            "diff --git a/x b/x\nindex 111..222 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b";
        let compressed = compress_tool_output("git", &args, output).unwrap();
        assert!(!compressed.contains("diff --git"));
        assert!(compressed.contains("-a"));
    }

    #[test]
    fn compress_tool_output_is_none_for_a_non_shell_tool() {
        let args = serde_json::json!({"path": "src/lib.rs"});
        assert!(compress_tool_output("read_file", &args, "content").is_none());
    }
}
