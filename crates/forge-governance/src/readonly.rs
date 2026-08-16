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

fn branch_is_readonly(args: &[String]) -> bool {
    if args.iter().any(|arg| !arg.starts_with('-')) {
        return false;
    }
    !args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "-d" | "-D"
                | "-m"
                | "-M"
                | "-c"
                | "-C"
                | "-f"
                | "--delete"
                | "--move"
                | "--copy"
                | "--force"
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
