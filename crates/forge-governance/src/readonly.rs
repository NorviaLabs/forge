//! Map workspace-inspection shell commands onto dedicated tools.
//!
//! Claude Code, Codex, and opencode keep `ls` / glob / grep / git as first-class
//! tools and only send real work to the shell. Forge already has confined
//! `git`, `glob`, `grep`, and `read_file`; this module stops the model from
//! paying a HITL tax for the bash spelling of the same reads.

use forge_types::ToolCall;
use serde_json::{json, Value};

use crate::pattern::is_shell_tool;

const REDIRECT_MESSAGE: &str = "Use dedicated workspace tools instead of bash for listing, \
search, and git reads. Use `ls`, `glob`, `grep`, `read_file`, or `git`.";

/// How a shell-equivalent call should be handled before authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadonlyShellRewrite {
    /// Run this dedicated tool instead of the shell.
    Dedicated { name: String, arguments: Value },
    /// Do not execute the shell; tell the model to use dedicated tools.
    Redirect { message: String },
}

/// Classify a shell-equivalent inspection command.
///
/// Returns `None` for non-shell tools and for commands that are not workspace
/// inspection (compilers, mutating git, pipes, etc.).
pub fn classify_readonly_shell(call: &ToolCall) -> Option<ReadonlyShellRewrite> {
    if !is_shell_tool(&call.name) {
        return None;
    }
    let command = command_argument(&call.arguments)?;
    classify_readonly_command(&command)
}

/// Rewrite a shell-equivalent inspection onto its dedicated tool, or return
/// the original call unchanged.
pub fn rewrite_readonly_shell_call(call: &ToolCall) -> ToolCall {
    match classify_readonly_shell(call) {
        Some(ReadonlyShellRewrite::Dedicated { name, arguments }) => ToolCall {
            id: call.id.clone(),
            name,
            arguments,
        },
        _ => call.clone(),
    }
}

pub fn readonly_shell_redirect_message(call: &ToolCall) -> Option<String> {
    match classify_readonly_shell(call) {
        Some(ReadonlyShellRewrite::Redirect { message }) => Some(message),
        _ => None,
    }
}

fn classify_readonly_command(command: &str) -> Option<ReadonlyShellRewrite> {
    let command = strip_ignored_redirects(command);
    if command.is_empty() {
        return None;
    }
    if let Some(rewrite) = classify_search_pipeline(&command) {
        return Some(rewrite);
    }
    let segments = split_command_segments(&command)?;
    if segments.is_empty() {
        return None;
    }
    if segments.len() == 1 {
        let words = tokenize_words(&segments[0])?;
        return rewrite_argv(&words).or_else(|| {
            (is_readonly_argv(&words) || is_search_argv(&words)).then(|| {
                ReadonlyShellRewrite::Redirect {
                    message: REDIRECT_MESSAGE.into(),
                }
            })
        });
    }
    if segments
        .iter()
        .any(|segment| tokenize_words(segment).is_some_and(|words| is_search_argv(&words)))
    {
        return Some(ReadonlyShellRewrite::Redirect {
            message: REDIRECT_MESSAGE.into(),
        });
    }
    if segments
        .iter()
        .all(|segment| tokenize_words(segment).is_some_and(|words| is_readonly_argv(&words)))
    {
        return Some(ReadonlyShellRewrite::Redirect {
            message: REDIRECT_MESSAGE.into(),
        });
    }
    None
}

fn rewrite_argv(words: &[String]) -> Option<ReadonlyShellRewrite> {
    let (command, rest) = split_command(words)?;
    match command {
        "ls" => rewrite_ls(rest),
        "find" => rewrite_find(rest),
        "rg" | "grep" | "egrep" | "fgrep" => rewrite_grep(command, rest),
        "fd" => rewrite_fd(rest),
        "cat" => rewrite_cat(rest),
        "head" | "tail" => rewrite_head(command, rest),
        "git" => rewrite_git(rest),
        _ => None,
    }
}

fn is_readonly_argv(words: &[String]) -> bool {
    rewrite_argv(words).is_some()
}

fn is_search_argv(words: &[String]) -> bool {
    matches!(
        words.first().map(|word| command_basename(word)),
        Some("rg" | "grep" | "egrep" | "fgrep" | "find" | "fd")
    )
}

fn split_command(words: &[String]) -> Option<(&str, &[String])> {
    let command = command_basename(words.first()?);
    Some((command, &words[1..]))
}

fn command_basename(word: &str) -> &str {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    name.strip_suffix(".exe").unwrap_or(name)
}

/// `rg … | head` is how models paginate search. Rewrite the search side and
/// drop the limiter — dedicated `grep` already caps results.
fn classify_search_pipeline(command: &str) -> Option<ReadonlyShellRewrite> {
    let segments = split_pipeline_segments(command)?;
    if segments.len() < 2 {
        return None;
    }
    let words = segments
        .iter()
        .map(|segment| tokenize_words(segment))
        .collect::<Option<Vec<_>>>()?;
    if !words.iter().any(|argv| is_search_argv(argv)) {
        return None;
    }
    if !words.iter().skip(1).all(|argv| is_result_limiter(argv)) {
        return None;
    }
    let search = words.iter().find(|argv| is_search_argv(argv))?;
    rewrite_argv(search).or_else(|| {
        Some(ReadonlyShellRewrite::Redirect {
            message: REDIRECT_MESSAGE.into(),
        })
    })
}

fn is_result_limiter(words: &[String]) -> bool {
    matches!(
        words.first().map(|word| command_basename(word)),
        Some("head" | "tail" | "wc")
    )
}

/// Drop stderr-to-null and stdout-to-null noise models add to search commands.
fn strip_ignored_redirects(command: &str) -> String {
    let mut out = command.trim().to_string();
    const IGNORED: &[&str] = &[
        "2>/dev/null",
        "2> /dev/null",
        "2>&1",
        ">/dev/null",
        "> /dev/null",
    ];
    loop {
        let trimmed = out.trim_end();
        if let Some(prefix) = IGNORED
            .iter()
            .find_map(|suffix| trimmed.strip_suffix(suffix))
        {
            out = prefix.trim_end().to_string();
            continue;
        }
        break;
    }
    out
}

/// Split on unquoted `|` that is not `||`. Any other control character aborts.
fn split_pipeline_segments(command: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match quote {
            Some('\'') => {
                current.push(ch);
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                current.push(ch);
                if ch == '\\' {
                    i += 1;
                    if i < chars.len() {
                        current.push(chars[i]);
                    }
                } else if ch == '"' {
                    quote = None;
                } else if matches!(ch, '$' | '`') {
                    return None;
                }
            }
            _ => {
                if ch == '\\' {
                    current.push(ch);
                    i += 1;
                    if i < chars.len() {
                        current.push(chars[i]);
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    current.push(ch);
                } else if ch == '|' && chars.get(i + 1) != Some(&'|') {
                    push_segment(&mut segments, &mut current)?;
                } else if matches!(ch, ';' | '&' | '\n' | '\r' | '`' | '$' | '<' | '>')
                    || (ch == '|' && chars.get(i + 1) == Some(&'|'))
                {
                    return None;
                } else {
                    current.push(ch);
                }
            }
        }
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    push_segment(&mut segments, &mut current)?;
    Some(segments)
}

fn rewrite_ls(args: &[String]) -> Option<ReadonlyShellRewrite> {
    let parsed = parse_ls(args)?;
    let mut arguments = serde_json::Map::new();
    if let Some(path) = parsed.path {
        arguments.insert("path".into(), json!(path));
    }
    if parsed.all {
        arguments.insert("all".into(), json!(true));
    }
    Some(dedicated("ls", Value::Object(arguments)))
}

fn rewrite_find(args: &[String]) -> Option<ReadonlyShellRewrite> {
    let parsed = parse_find(args)?;
    Some(dedicated("glob", json!({ "pattern": parsed.query })))
}

fn rewrite_grep(command: &str, args: &[String]) -> Option<ReadonlyShellRewrite> {
    let parsed = parse_grep(args)?;
    let mut arguments = serde_json::Map::new();
    arguments.insert("pattern".into(), json!(parsed.pattern));
    if let Some(path) = parsed.path {
        arguments.insert("path".into(), json!(path));
    }
    if let Some(include) = parsed.include {
        arguments.insert("include".into(), json!(include));
    }
    if grep_should_use_regex(command, parsed.fixed_strings) {
        arguments.insert("mode".into(), json!("regex"));
    }
    Some(dedicated("grep", Value::Object(arguments)))
}

fn grep_should_use_regex(command: &str, fixed_strings: bool) -> bool {
    !fixed_strings && matches!(command, "rg" | "egrep")
}

fn rewrite_fd(args: &[String]) -> Option<ReadonlyShellRewrite> {
    let parsed = parse_fd(args)?;
    Some(dedicated("glob", json!({ "pattern": parsed.query })))
}

fn rewrite_cat(args: &[String]) -> Option<ReadonlyShellRewrite> {
    let path = parse_cat(args)?;
    Some(dedicated("read_file", json!({ "path": path })))
}

fn rewrite_head(command: &str, args: &[String]) -> Option<ReadonlyShellRewrite> {
    let parsed = parse_head(args)?;
    let mut arguments = serde_json::Map::new();
    arguments.insert("path".into(), json!(parsed.path));
    if let Some(limit) = parsed.limit {
        arguments.insert("limit".into(), json!(limit));
    } else if command == "head" || command == "tail" {
        arguments.insert("limit".into(), json!(10));
    }
    Some(dedicated("read_file", Value::Object(arguments)))
}

fn rewrite_git(args: &[String]) -> Option<ReadonlyShellRewrite> {
    let parsed = parse_git(args)?;
    Some(dedicated(
        "git",
        json!({
            "subcommand": parsed.subcommand,
            "args": parsed.args,
        }),
    ))
}

fn dedicated(name: &str, arguments: Value) -> ReadonlyShellRewrite {
    ReadonlyShellRewrite::Dedicated {
        name: name.to_string(),
        arguments,
    }
}

struct LsParsed {
    path: Option<String>,
    all: bool,
}

fn parse_ls(args: &[String]) -> Option<LsParsed> {
    let mut path = None;
    let mut all = false;
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') {
            if arg.starts_with("--") {
                if matches!(
                    arg.as_str(),
                    "--color" | "--color=auto" | "--color=never" | "--color=always"
                ) {
                    continue;
                }
                return None;
            }
            for flag in arg.chars().skip(1) {
                match flag {
                    'a' | 'A' => all = true,
                    'l' | 'h' | '1' | 't' | 'r' | 'S' | 'R' | 'C' => {}
                    _ => return None,
                }
            }
            continue;
        }
        if path.is_some() {
            return None;
        }
        path = Some(arg.clone());
    }
    Some(LsParsed { path, all })
}

struct FindParsed {
    query: String,
}

fn parse_find(args: &[String]) -> Option<FindParsed> {
    let mut path = None;
    let mut name = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-name" | "-iname" => {
                i += 1;
                name = Some(args.get(i)?.clone());
            }
            "-type" | "-maxdepth" | "-mindepth" => {
                i += 1;
                args.get(i)?;
            }
            "-o" | "-or" | "-not" | "!" | "(" | ")" => {}
            flag if flag.starts_with('-') => return None,
            _ if path.is_none() => path = Some(arg.to_string()),
            _ => return None,
        }
        i += 1;
    }
    let query = match (name, path.as_deref()) {
        (Some(name), _) => name,
        (None, None | Some("." | "./")) => "*".into(),
        (None, Some(path)) => path.to_string(),
    };
    Some(FindParsed { query })
}

struct GrepParsed {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
    fixed_strings: bool,
}

fn parse_grep(args: &[String]) -> Option<GrepParsed> {
    let mut pattern = None;
    let mut path = None;
    let mut include = None;
    let mut fixed_strings = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--" => {
                i += 1;
                break;
            }
            "-e" | "--regexp" => {
                i += 1;
                if pattern.is_some() {
                    return None;
                }
                pattern = Some(args.get(i)?.clone());
            }
            "-g" | "--glob" => {
                i += 1;
                include = Some(args.get(i)?.clone());
            }
            "-t" | "--type" | "-A" | "-B" | "-C" | "-m" | "--max-count" | "--type-add" => {
                i += 1;
                args.get(i)?;
            }
            "-F" | "--fixed-strings" => fixed_strings = true,
            "-n" | "-i" | "-w" | "-l" | "-c" | "-H" | "-I" | "--heading" | "--no-heading"
            | "--hidden" | "--color" | "--color=never" | "--color=auto" | "--line-number"
            | "--ignore-case" => {}
            "--pre" | "--pre-glob" | "-f" | "--file" | "--exec" => return None,
            flag if flag.starts_with("--glob=") => {
                include = Some(flag["--glob=".len()..].to_string());
            }
            flag if is_combined_grep_shorts(flag) => {
                if flag.contains('F') {
                    fixed_strings = true;
                }
            }
            flag if flag.starts_with('-') => return None,
            _ => {
                if pattern.is_none() {
                    pattern = Some(arg.to_string());
                } else if path.is_none() {
                    path = Some(arg.to_string());
                } else {
                    return None;
                }
            }
        }
        i += 1;
    }
    while i < args.len() {
        if pattern.is_none() {
            pattern = Some(args[i].clone());
        } else if path.is_none() {
            path = Some(args[i].clone());
        } else {
            return None;
        }
        i += 1;
    }
    Some(GrepParsed {
        pattern: pattern?,
        path,
        include,
        fixed_strings,
    })
}

fn is_combined_grep_shorts(flag: &str) -> bool {
    let Some(rest) = flag.strip_prefix('-') else {
        return false;
    };
    if rest.is_empty() || rest.starts_with('-') {
        return false;
    }
    rest.chars()
        .all(|ch| matches!(ch, 'n' | 'i' | 'F' | 'w' | 'l' | 'c' | 'H' | 'I'))
}

struct FdParsed {
    query: String,
}

fn parse_fd(args: &[String]) -> Option<FdParsed> {
    let mut query = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-t" | "--type" | "-e" | "--extension" | "-d" | "--max-depth" | "-E" | "--exclude" => {
                i += 1;
                args.get(i)?;
            }
            "-H" | "--hidden" | "-I" | "--no-ignore" | "-s" | "--case-sensitive" | "-i"
            | "--ignore-case" => {}
            "--exec" | "-x" | "--exec-batch" | "-X" => return None,
            flag if flag.starts_with('-') => return None,
            _ => {
                if query.is_none() {
                    query = Some(arg.to_string());
                }
            }
        }
        i += 1;
    }
    Some(FdParsed {
        query: query.unwrap_or_else(|| "*".into()),
    })
}

fn parse_cat(args: &[String]) -> Option<String> {
    let mut path = None;
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') {
            return None;
        }
        if path.is_some() {
            return None;
        }
        path = Some(arg.clone());
    }
    path
}

struct HeadParsed {
    path: String,
    limit: Option<u64>,
}

fn parse_head(args: &[String]) -> Option<HeadParsed> {
    let mut path = None;
    let mut limit = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            i += 1;
            continue;
        }
        if arg == "-n" || arg == "--lines" {
            i += 1;
            limit = Some(args.get(i)?.parse().ok()?);
        } else if let Some(digits) = arg
            .strip_prefix('-')
            .filter(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
        {
            limit = Some(digits.parse().ok()?);
        } else if arg.starts_with('-') {
            return None;
        } else if path.is_none() {
            path = Some(arg.to_string());
        } else {
            return None;
        }
        i += 1;
    }
    Some(HeadParsed { path: path?, limit })
}

struct GitParsed {
    subcommand: String,
    args: Vec<String>,
}

fn parse_git(args: &[String]) -> Option<GitParsed> {
    let rest = strip_git_globals(args);
    let subcommand = rest.first()?.to_ascii_lowercase();
    if !is_readonly_git_subcommand(&subcommand) {
        return None;
    }
    if subcommand == "branch" && !branch_is_readonly(&rest[1..]) {
        return None;
    }
    Some(GitParsed {
        subcommand,
        args: rest[1..].to_vec(),
    })
}

fn is_readonly_git_subcommand(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "status" | "diff" | "log" | "show" | "branch" | "rev-parse" | "ls-files" | "blame"
    )
}

/// Whether every `git branch` option is a known read.
///
/// This is an allowlist, not a denylist of the delete/rename/force family. An
/// option this module has never heard of must not be assumed to be a read:
/// `--set-upstream-to=`, `--unset-upstream` and `--edit-description` all mutate
/// repository state and all begin with `-`, so a denylist classified them as
/// reads and rewrote them onto the dedicated `git` tool. That was never
/// exploitable — `GitTool` is `SideEffectClass::Write`, so `authorize` still
/// gates it — but labelling a mutation as a read one layer above a security
/// boundary is a bug in its own right.
///
/// Only the `=` spelling of a valued option needs matching here; the space
/// spelling puts the value in a bare operand, which the caller already rejects.
fn branch_is_readonly(args: &[String]) -> bool {
    if args.iter().any(|arg| !arg.starts_with('-')) {
        return false;
    }
    args.iter().all(|arg| {
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        matches!(
            name,
            "-a" | "--all"
                | "-r"
                | "--remotes"
                | "-v"
                | "-vv"
                | "--verbose"
                | "-l"
                | "--list"
                | "--show-current"
                | "-q"
                | "--quiet"
                | "-i"
                | "--ignore-case"
                | "--omit-empty"
                | "--color"
                | "--no-color"
                | "--column"
                | "--no-column"
                | "--abbrev"
                | "--no-abbrev"
                | "--merged"
                | "--no-merged"
                | "--contains"
                | "--no-contains"
                | "--points-at"
                | "--sort"
                | "--format"
        )
    })
}

fn strip_git_globals(args: &[String]) -> &[String] {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-pager" | "--no-replace-objects" | "--bare" | "--no-optional-locks" => i += 1,
            "-C" | "-c" | "--git-dir" | "--work-tree" => i = i.saturating_add(2),
            flag if flag.starts_with("--git-dir=") || flag.starts_with("--work-tree=") => i += 1,
            _ => break,
        }
    }
    &args[i.min(args.len())..]
}

fn command_argument(args: &Value) -> Option<String> {
    let value = args.get("command").or_else(|| args.get("cmd"))?;
    if let Some(command) = value.as_str() {
        let trimmed = command.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_owned());
    }
    let parts = value.as_array()?;
    let words: Option<Vec<&str>> = parts.iter().map(|part| part.as_str()).collect();
    let words = words?
        .into_iter()
        .map(str::trim)
        .filter(|word| !word.is_empty());
    let joined = words.collect::<Vec<_>>().join(" ");
    (!joined.is_empty()).then_some(joined)
}

/// Split on top-level `&&`, `||`, or `;`. Any other unquoted control
/// character (`|`, `&`, `$`, redirects) means this is not a pure inspection.
fn split_command_segments(command: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match quote {
            Some('\'') => {
                current.push(ch);
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                current.push(ch);
                if ch == '\\' {
                    i += 1;
                    if i < chars.len() {
                        current.push(chars[i]);
                    }
                } else if ch == '"' {
                    quote = None;
                } else if matches!(ch, '$' | '`') {
                    return None;
                }
            }
            _ => {
                if ch == '\\' {
                    current.push(ch);
                    i += 1;
                    if i < chars.len() {
                        current.push(chars[i]);
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    current.push(ch);
                } else if (ch == '&' && chars.get(i + 1) == Some(&'&'))
                    || (ch == '|' && chars.get(i + 1) == Some(&'|'))
                {
                    push_segment(&mut segments, &mut current)?;
                    i += 1;
                } else if ch == ';' {
                    push_segment(&mut segments, &mut current)?;
                } else if matches!(ch, '|' | '&' | '\n' | '\r' | '`' | '$' | '<' | '>') {
                    return None;
                } else {
                    current.push(ch);
                }
            }
        }
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    push_segment(&mut segments, &mut current)?;
    Some(segments)
}

fn push_segment(segments: &mut Vec<String>, current: &mut String) -> Option<()> {
    let trimmed = current.trim();
    if trimmed.is_empty() {
        return None;
    }
    segments.push(trimmed.to_string());
    current.clear();
    Some(())
}

fn tokenize_words(command: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            Some('"') => {
                if ch == '\\' {
                    i += 1;
                    if i < chars.len() {
                        current.push(chars[i]);
                    }
                } else if ch == '"' {
                    quote = None;
                } else if matches!(ch, '$' | '`') {
                    return None;
                } else {
                    current.push(ch);
                }
            }
            _ => {
                if ch.is_whitespace() {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                } else if ch == '\\' {
                    i += 1;
                    if i < chars.len() {
                        current.push(chars[i]);
                    }
                } else if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else if matches!(ch, ';' | '|' | '&' | '\n' | '\r' | '`' | '$' | '<' | '>') {
                    return None;
                } else {
                    current.push(ch);
                }
            }
        }
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        words.push(current);
    }
    (!words.is_empty()).then_some(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(command: &str) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: json!({"command": command}),
        }
    }

    #[test]
    fn rewrites_common_inspection_commands() {
        assert_eq!(
            classify_readonly_shell(&call("ls -la crates")),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "ls".into(),
                arguments: json!({"path": "crates", "all": true}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call(r#"find . -name "*.rs""#)),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "glob".into(),
                arguments: json!({"pattern": "*.rs"}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call(r#"rg -n "Auto|Manual" crates"#)),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "grep".into(),
                arguments: json!({"pattern": "Auto|Manual", "path": "crates", "mode": "regex"}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call(r#"rg -ni Auto crates"#)),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "grep".into(),
                arguments: json!({"pattern": "Auto", "path": "crates", "mode": "regex"}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call(r#"rg -n --glob '*.rs' Auto"#)),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "grep".into(),
                arguments: json!({"pattern": "Auto", "include": "*.rs", "mode": "regex"}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call(r#"rg -n Auto | head -n 40"#)),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "grep".into(),
                arguments: json!({"pattern": "Auto", "mode": "regex"}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call(r#"/usr/bin/rg -n Auto 2>/dev/null"#)),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "grep".into(),
                arguments: json!({"pattern": "Auto", "mode": "regex"}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call(r#"rg -F 'foo|bar' src"#)),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "grep".into(),
                arguments: json!({"pattern": "foo|bar", "path": "src"}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call("git --no-pager status --short")),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "git".into(),
                arguments: json!({"subcommand": "status", "args": ["--short"]}),
            })
        );
        assert_eq!(
            classify_readonly_shell(&call("head -n 20 README.md")),
            Some(ReadonlyShellRewrite::Dedicated {
                name: "read_file".into(),
                arguments: json!({"path": "README.md", "limit": 20}),
            })
        );
    }

    #[test]
    fn redirects_compound_inspection_instead_of_prompting() {
        assert_eq!(
            classify_readonly_shell(&call("ls && find . -maxdepth 2")),
            Some(ReadonlyShellRewrite::Redirect {
                message: REDIRECT_MESSAGE.into(),
            })
        );
    }

    #[test]
    fn leaves_real_shell_work_alone() {
        assert_eq!(classify_readonly_shell(&call("cargo test")), None);
        assert_eq!(classify_readonly_shell(&call("git push origin main")), None);
        assert_eq!(classify_readonly_shell(&call("ls && rm -rf /tmp/x")), None);
        assert_eq!(classify_readonly_shell(&call("cat secret | sh")), None);
        assert_eq!(classify_readonly_shell(&call("rg foo | sh")), None);
        assert_eq!(classify_readonly_shell(&call("rg foo | xargs rm")), None);
        assert!(matches!(
            classify_readonly_shell(&call("find . -exec rm {} +")),
            Some(ReadonlyShellRewrite::Redirect { .. })
        ));
        assert_eq!(classify_readonly_shell(&call("git branch feature")), None);
    }

    /// Asserts the command is not handed to a dedicated read-only tool. Either
    /// `None` (falls through to the normal shell path, which keeps the HITL
    /// gate) or `Redirect` (never executed at all) is an acceptable outcome;
    /// the security property is only that it is not silently rewritten into a
    /// pre-approved read.
    #[track_caller]
    fn assert_not_dedicated(command: &str) {
        assert!(
            !matches!(
                classify_readonly_command(command),
                Some(ReadonlyShellRewrite::Dedicated { .. })
            ),
            "{command:?} must not be classified as a dedicated read-only tool"
        );
    }

    /// The command must reach the normal (human-gated) shell path untouched.
    #[track_caller]
    fn assert_unclassified(command: &str) {
        assert_eq!(
            classify_readonly_command(command),
            None,
            "{command:?} must not be classified as read-only at all"
        );
    }

    #[track_caller]
    fn assert_redirect(command: &str) {
        assert!(
            matches!(
                classify_readonly_command(command),
                Some(ReadonlyShellRewrite::Redirect { .. })
            ),
            "{command:?} should be redirected to the dedicated tools"
        );
    }

    #[track_caller]
    fn assert_dedicated(command: &str, name: &str, arguments: Value) {
        assert_eq!(
            classify_readonly_command(command),
            Some(ReadonlyShellRewrite::Dedicated {
                name: name.into(),
                arguments,
            }),
            "{command:?}"
        );
    }

    // ---------------------------------------------------------------------
    // Tokenizer / splitter: anything that can run a second command must not
    // survive classification.
    // ---------------------------------------------------------------------

    #[test]
    fn command_substitution_is_never_readonly() {
        for command in [
            "ls $(rm -rf /)",
            "ls `rm -rf /`",
            r#"ls "$(rm -rf /)""#,
            r#"ls "`rm -rf /`""#,
            "cat $(echo /etc/passwd)",
            "rg $(cat payload) src",
            r#"rg "${IFS}foo" src"#,
            "ls $(",
            "ls ${HOME}",
            "ls $HOME",
            "head -n $(id -u) f",
        ] {
            assert_unclassified(command);
        }
    }

    #[test]
    fn control_operators_and_redirects_are_never_readonly() {
        for command in [
            "ls a; rm -rf /",
            "ls\nrm -rf /",
            "ls\r\nrm -rf /",
            "ls & rm -rf /",
            "ls > out",
            "ls >> out",
            "cat < in",
            "ls <(rm -rf /)",
            "git status; rm -rf /",
            "ls 2>/dev/null; rm x",
            "rg foo | tee out",
            "rg foo | xargs rm",
            "cat secret | sh",
            "head -n 5 f | rg foo",
            "rg foo | head -n 5 && rm -rf /",
            "ls; rg foo | head",
        ] {
            assert_not_dedicated(command);
        }
    }

    #[test]
    fn unbalanced_and_empty_segments_are_rejected() {
        for command in [
            "rg 'unterminated",
            r#"ls "unterminated"#,
            "ls; ; ls",
            "|ls",
            "ls |",
            "ls &&",
            "&& ls",
            "",
            "   ",
        ] {
            assert_unclassified(command);
        }
    }

    /// Quoting keeps separators literal: the argument is passed as data to the
    /// dedicated tool, never re-parsed by a shell.
    #[test]
    fn quoted_separators_stay_literal_arguments() {
        assert_dedicated("ls '; rm -rf /'", "ls", json!({"path": "; rm -rf /"}));
        assert_dedicated(r#"ls "; rm -rf /""#, "ls", json!({"path": "; rm -rf /"}));
        assert_dedicated("ls '$(rm -rf /)'", "ls", json!({"path": "$(rm -rf /)"}));
        assert_dedicated("ls '|'", "ls", json!({"path": "|"}));
        assert_dedicated(r"ls \$HOME", "ls", json!({"path": "$HOME"}));
        assert_dedicated(r"ls a\ b", "ls", json!({"path": "a b"}));
        assert_dedicated(
            r#"rg "foo\"bar" src"#,
            "grep",
            json!({"pattern": "foo\"bar", "path": "src", "mode": "regex"}),
        );
    }

    /// A backslash-escaped separator is not a separator, but it also is not a
    /// word boundary — `ls\;rm` is one word and no longer names `ls`.
    #[test]
    fn escaped_separator_does_not_split_into_two_commands() {
        assert_unclassified(r"ls\;rm -rf /");
    }

    #[test]
    fn command_name_is_taken_from_the_basename_only() {
        assert_dedicated("/usr/bin/ls src", "ls", json!({"path": "src"}));
        assert_dedicated(r"'C:\tools\ls.exe' src", "ls", json!({"path": "src"}));
        // A wrapper in front of the command is not the command.
        assert_unclassified("sudo ls");
        assert_unclassified("env ls");
        assert_unclassified("GIT_DIR=/x git status");
        assert_unclassified("xargs ls");
    }

    // ---------------------------------------------------------------------
    // Per-command flag tables.
    // ---------------------------------------------------------------------

    #[test]
    fn ls_flag_table_rejects_unknown_options() {
        assert_dedicated("ls", "ls", json!({}));
        assert_dedicated("ls -lhtrSRC1 src", "ls", json!({"path": "src"}));
        assert_dedicated("ls -A", "ls", json!({"all": true}));
        assert_dedicated("ls --color=never src", "ls", json!({"path": "src"}));
        assert_dedicated("ls -- -weird", "ls", json!({"path": "-weird"}));
        for command in [
            "ls -Z",
            "ls -d",
            "ls --hide=x",
            "ls --directory",
            "ls -laZ",
            "ls a b",
        ] {
            assert_not_dedicated(command);
        }
    }

    #[test]
    fn grep_option_smuggling_is_not_rewritten() {
        // `-f`/`--file` reads the pattern list from a file, `--pre`/`--exec`
        // run a program. None may become a pre-approved dedicated search.
        for command in [
            "grep -f /etc/passwd foo",
            "grep --file=x foo",
            "grep --file x foo",
            "rg --pre bash foo",
            "rg --pre-glob '*' foo",
            "rg --exec rm foo",
            "rg -r replacement foo",
            "rg -uu foo",
            "rg -z foo",
            "rg --files",
            "grep -rn foo",
            "grep --exclude=x foo",
            "rg -e foo -e bar",
            "rg foo a b",
        ] {
            assert_not_dedicated(command);
        }
    }

    #[test]
    fn grep_accepts_its_known_flags() {
        assert_dedicated(
            "grep -n foo src",
            "grep",
            json!({"pattern": "foo", "path": "src"}),
        );
        assert_dedicated(
            "grep -nF foo src",
            "grep",
            json!({"pattern": "foo", "path": "src"}),
        );
        // Combined shorts containing F still mean fixed-strings, so no regex mode.
        assert_dedicated(
            "rg -niF 'a|b' src",
            "grep",
            json!({"pattern": "a|b", "path": "src"}),
        );
        assert_dedicated(
            "rg -ni 'a|b' src",
            "grep",
            json!({"pattern": "a|b", "path": "src", "mode": "regex"}),
        );
        // egrep is a regex dialect; plain grep and fgrep are not.
        assert_dedicated(
            "egrep foo",
            "grep",
            json!({"pattern": "foo", "mode": "regex"}),
        );
        assert_dedicated("fgrep foo", "grep", json!({"pattern": "foo"}));
        // Valued flags consume their argument rather than reading it as the pattern.
        assert_dedicated(
            "rg -C 3 -m 10 --type rust foo",
            "grep",
            json!({"pattern": "foo", "mode": "regex"}),
        );
        assert_dedicated(
            "rg --glob=*.rs foo",
            "grep",
            json!({"pattern": "foo", "include": "*.rs", "mode": "regex"}),
        );
        // `--` ends option parsing, so a dash-leading pattern is still a pattern.
        assert_dedicated(
            "rg -n -- -foo src",
            "grep",
            json!({"pattern": "-foo", "path": "src", "mode": "regex"}),
        );
        // A valued flag with a missing argument is not a search at all.
        assert_not_dedicated("rg -C");
        assert_not_dedicated("rg -e");
    }

    #[test]
    fn find_and_fd_actions_are_not_rewritten() {
        for command in [
            "find . -exec rm {} +",
            "find . -execdir rm {} +",
            "find . -name '*.rs' -delete",
            "find . -newer x",
            "find . -printf '%p'",
            "fd -x rm",
            "fd -X rm",
            "fd --exec rm",
            "fd --exec-batch rm",
            "fd --unknown-flag foo",
        ] {
            assert_not_dedicated(command);
        }
    }

    #[test]
    fn find_and_fd_map_onto_glob() {
        assert_dedicated("find . -type f", "glob", json!({"pattern": "*"}));
        assert_dedicated("find", "glob", json!({"pattern": "*"}));
        assert_dedicated("find ./", "glob", json!({"pattern": "*"}));
        assert_dedicated("find src", "glob", json!({"pattern": "src"}));
        assert_dedicated(
            "find src -iname '*.RS' -maxdepth 2",
            "glob",
            json!({"pattern": "*.RS"}),
        );
        // A valued predicate with no value is not a listing.
        assert_not_dedicated("find . -name");
        assert_not_dedicated("find . -maxdepth");
        assert_dedicated("fd", "glob", json!({"pattern": "*"}));
        assert_dedicated(
            "fd -H -I -e rs pattern",
            "glob",
            json!({"pattern": "pattern"}),
        );
    }

    #[test]
    fn cat_and_head_only_take_a_single_plain_path() {
        assert_dedicated("cat f", "read_file", json!({"path": "f"}));
        assert_dedicated("cat -- -weird", "read_file", json!({"path": "-weird"}));
        assert_not_dedicated("cat a b");
        assert_not_dedicated("cat -n a");
        assert_not_dedicated("cat");

        assert_dedicated("head f", "read_file", json!({"path": "f", "limit": 10}));
        assert_dedicated("head -20 f", "read_file", json!({"path": "f", "limit": 20}));
        assert_dedicated("tail -n 5 f", "read_file", json!({"path": "f", "limit": 5}));
        assert_dedicated(
            "head --lines 5 f",
            "read_file",
            json!({"path": "f", "limit": 5}),
        );
        // `-c` is bytes, `-f` follows: neither is a bounded line read.
        assert_not_dedicated("head -c 100 f");
        assert_not_dedicated("tail -f log");
        assert_not_dedicated("head -n -5 f");
        assert_not_dedicated("head -n abc f");
        assert_not_dedicated("head a b");
        assert_not_dedicated("head");
    }

    // ---------------------------------------------------------------------
    // git
    // ---------------------------------------------------------------------

    #[test]
    fn git_readonly_subcommands_map_through() {
        for sub in [
            "status",
            "diff",
            "log",
            "show",
            "rev-parse",
            "ls-files",
            "blame",
        ] {
            assert_dedicated(
                &format!("git {sub}"),
                "git",
                json!({"subcommand": sub, "args": []}),
            );
        }
        // Subcommand matching is case-insensitive.
        assert_dedicated(
            "git STATUS",
            "git",
            json!({"subcommand": "status", "args": []}),
        );
    }

    #[test]
    fn git_mutating_subcommands_stay_on_the_gated_path() {
        for command in [
            "git commit -m x",
            "git push origin main",
            "git checkout main",
            "git switch -c topic",
            "git add .",
            "git reset --hard",
            "git stash",
            "git clean -fd",
            "git config user.email x",
            "git",
        ] {
            assert_unclassified(command);
        }
    }

    #[test]
    fn git_branch_is_readonly_only_without_operands_or_write_flags() {
        assert_dedicated(
            "git branch --list",
            "git",
            json!({"subcommand": "branch", "args": ["--list"]}),
        );
        for command in [
            "git branch feature",
            "git branch -d foo",
            "git branch -D foo",
            "git branch --delete",
            "git branch -m",
            "git branch -M",
            "git branch -c",
            "git branch -C",
            "git branch -f",
            "git branch --move",
            "git branch --copy",
            "git branch --force",
        ] {
            assert_unclassified(command);
        }
    }

    /// Options that mutate the repository are not reads, even though they
    /// begin with `-` and are not part of the delete/rename/force family.
    ///
    /// Note the `git` tool's own argument policy *permits* `-u` and
    /// `--set-upstream-to`, so it is not what stops these — the backstop is
    /// that `GitTool` is `SideEffectClass::Write` and stays HITL-gated. This
    /// module still must not label them reads.
    #[test]
    fn git_branch_upstream_and_description_flags_are_not_reads() {
        assert_unclassified("git branch --set-upstream-to=origin/main");
        assert_unclassified("git branch --unset-upstream");
        assert_unclassified("git branch --edit-description");
        assert_unclassified("git branch --set-upstream");
        // Attached short form: one word, so the bare-operand check cannot see it.
        assert_unclassified("git branch -uorigin/main");
        // The space form is rejected earlier, as a bare word.
        assert_unclassified("git branch -u origin/main");
    }

    /// An option this module has never heard of is not assumed to be a read.
    #[test]
    fn git_branch_unknown_flags_are_not_reads() {
        assert_unclassified("git branch --some-future-flag");
        assert_unclassified("git branch --track");
        assert_unclassified("git branch --create-reflog");
    }

    /// The genuine reads still rewrite, including the `=` spelling of valued
    /// filters — the space spelling stays rejected as a bare operand.
    #[test]
    fn git_branch_read_flags_still_rewrite() {
        for (command, args) in [
            ("git branch", vec![]),
            ("git branch -a", vec!["-a"]),
            ("git branch --all", vec!["--all"]),
            ("git branch -r", vec!["-r"]),
            ("git branch --show-current", vec!["--show-current"]),
            ("git branch -v", vec!["-v"]),
            ("git branch --merged", vec!["--merged"]),
            (
                "git branch --sort=-committerdate",
                vec!["--sort=-committerdate"],
            ),
            (
                "git branch --format=%(refname)",
                vec!["--format=%(refname)"],
            ),
            ("git branch --contains=HEAD", vec!["--contains=HEAD"]),
            ("git branch -a --list", vec!["-a", "--list"]),
        ] {
            assert_dedicated(
                command,
                "git",
                json!({"subcommand": "branch", "args": args}),
            );
        }
        assert_unclassified("git branch --sort -committerdate");
    }

    /// The delete/rename/force family stays rejected — the allowlist rewrite
    /// must not regress what the old denylist already caught.
    #[test]
    fn git_branch_mutating_family_is_still_rejected() {
        for command in [
            "git branch -d",
            "git branch -D",
            "git branch -m",
            "git branch -M",
            "git branch -c",
            "git branch -C",
            "git branch -f",
            "git branch --delete",
            "git branch --move",
            "git branch --copy",
            "git branch --force",
        ] {
            assert_unclassified(command);
        }
    }

    /// Global options are stripped, not forwarded — so `-C dir` and `-c key=val`
    /// cannot redirect the rewritten call at a different repository or override
    /// git config such as `core.pager`.
    #[test]
    fn git_globals_are_stripped_from_the_rewritten_call() {
        for command in [
            "git --no-pager status",
            "git --no-optional-locks status",
            "git --no-replace-objects status",
            "git --bare status",
            "git -C /tmp status",
            "git -c core.pager=sh status",
            "git --git-dir=/tmp/.git status",
            "git --work-tree=/tmp status",
            "git --git-dir /tmp/.git status",
            "git --work-tree /tmp status",
        ] {
            assert_dedicated(command, "git", json!({"subcommand": "status", "args": []}));
        }
        // Globals with no subcommand left behind are not a read.
        assert_unclassified("git -C");
        assert_unclassified("git --no-pager");
    }

    // ---------------------------------------------------------------------
    // Pipelines and compound commands.
    // ---------------------------------------------------------------------

    #[test]
    fn search_pipelines_only_collapse_through_result_limiters() {
        assert_dedicated(
            "rg -n foo | wc -l",
            "grep",
            json!({"pattern": "foo", "mode": "regex"}),
        );
        assert_dedicated(
            "rg -n foo | head -n 5 | wc -l",
            "grep",
            json!({"pattern": "foo", "mode": "regex"}),
        );
        assert_dedicated(
            "find . -name '*.rs' | head",
            "glob",
            json!({"pattern": "*.rs"}),
        );
        // A search whose own argv does not parse still gets redirected rather
        // than executed.
        assert_redirect("rg --files | head");
        // Non-limiter sinks, and pipelines with no search at all, are untouched.
        assert_unclassified("rg foo | sort");
        assert_unclassified("rg foo | grep bar");
        assert_unclassified("ls | head");
        assert_unclassified("cat f | head");
    }

    #[test]
    fn compound_inspection_is_redirected_and_mixed_work_is_not() {
        assert_redirect("ls && ls src && ls crates");
        assert_redirect("ls; ls src");
        assert_redirect("ls || find .");
        assert_redirect("git status && git diff");
        // One non-inspection segment is enough to fall back to the gated path.
        assert_unclassified("ls && rm -rf /tmp/x");
        assert_unclassified("git status && git commit -m x");
        assert_unclassified("cat f && ./configure");
    }

    // ---------------------------------------------------------------------
    // Argument plumbing.
    // ---------------------------------------------------------------------

    #[test]
    fn only_shell_tools_with_a_command_argument_are_classified() {
        let dedicated_ls = Some(ReadonlyShellRewrite::Dedicated {
            name: "ls".into(),
            arguments: json!({"path": "src"}),
        });
        for tool in ["bash", "sh", "shell", "cmd", "powershell", "exec"] {
            assert_eq!(
                classify_readonly_shell(&ToolCall {
                    id: "1".into(),
                    name: tool.into(),
                    arguments: json!({"command": "ls src"}),
                }),
                dedicated_ls,
                "{tool}"
            );
        }
        // A non-shell tool is never inspected, even if it carries a `command`.
        assert_eq!(
            classify_readonly_shell(&ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: json!({"command": "ls src"}),
            }),
            None
        );
        // `cmd` is accepted as an alias for `command`, including array form.
        assert_eq!(
            classify_readonly_shell(&ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: json!({"cmd": ["ls", "", "src"]}),
            }),
            dedicated_ls
        );
        for arguments in [
            json!({}),
            json!({"command": ""}),
            json!({"command": "   "}),
            json!({"command": []}),
            json!({"command": ["", " "]}),
            json!({"command": [1, 2]}),
            json!({"command": 7}),
        ] {
            assert_eq!(
                classify_readonly_shell(&ToolCall {
                    id: "1".into(),
                    name: "bash".into(),
                    arguments: arguments.clone(),
                }),
                None,
                "{arguments}"
            );
        }
    }

    #[test]
    fn trailing_null_redirects_are_stripped_before_classification() {
        for suffix in [
            "2>/dev/null",
            "2> /dev/null",
            "2>&1",
            ">/dev/null",
            "> /dev/null",
            ">/dev/null 2>&1",
            "2>/dev/null 2>/dev/null",
        ] {
            assert_dedicated(&format!("ls src {suffix}"), "ls", json!({"path": "src"}));
        }
        // Only trailing null redirects are noise; a real redirect target is not.
        assert_unclassified("ls src >/dev/nul");
        assert_unclassified("ls src > out 2>&1");
    }

    #[test]
    fn rewrite_helper_only_changes_dedicated_mappings() {
        let rewritten = rewrite_readonly_shell_call(&call("ls"));
        assert_eq!(rewritten.name, "ls");
        let unchanged = rewrite_readonly_shell_call(&call("ls && find ."));
        assert_eq!(unchanged.name, "bash");
        assert_eq!(
            readonly_shell_redirect_message(&call("ls && find .")).as_deref(),
            Some(REDIRECT_MESSAGE)
        );
    }
}
