//! The SSH hosts a session can run on: the `Host` aliases in the user's SSH
//! client config, and the check a new session's host and folder pass before
//! anything connects.

use std::path::{Path, PathBuf};

use forge_types::ExecutionTarget;

/// The user's SSH client config, `~/.ssh/config`.
pub fn user_config() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".ssh").join("config"))
}

/// The concrete `Host` aliases in `config` and the files it includes, in
/// order, each once. Patterns (`*`, `?`, `!negated`) name no one host and are
/// left out; so is anything that could not be passed to `ssh` as a host.
/// A missing or unreadable file lists nothing.
pub fn hosts_in(config: &Path) -> Vec<String> {
    let base = config.parent().unwrap_or(Path::new("/")).to_path_buf();
    let mut hosts = Vec::new();
    let mut seen_files = std::collections::HashSet::new();
    collect(config, &base, 0, &mut seen_files, &mut hosts);
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|host| seen.insert(host.clone()));
    hosts
}

/// How deep `Include` may nest (as `ssh` itself limits it).
const MAX_INCLUDE_DEPTH: usize = 16;
/// A config file bigger than this is not read.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

fn collect(
    file: &Path,
    base: &Path,
    depth: usize,
    seen_files: &mut std::collections::HashSet<PathBuf>,
    hosts: &mut Vec<String>,
) {
    if depth > MAX_INCLUDE_DEPTH {
        return;
    }
    let canonical = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    if !seen_files.insert(canonical) {
        return;
    }
    let Ok(meta) = std::fs::metadata(file) else {
        return;
    };
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    for line in text.lines() {
        let Some((keyword, rest)) = split_keyword(line) else {
            continue;
        };
        match keyword.to_ascii_lowercase().as_str() {
            "host" => hosts.extend(words(rest).into_iter().filter(|alias| is_concrete(alias))),
            "include" => {
                for pattern in words(rest) {
                    for included in expand_include(&pattern, base) {
                        collect(&included, base, depth + 1, seen_files, hosts);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `Keyword value` or `Keyword=value`, without comments or blank lines.
fn split_keyword(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim();
    Some((keyword, rest))
}

/// Whitespace-separated words; a double-quoted word may hold spaces.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// One host `ssh` would be given as is: no pattern, negation or spaces, and
/// nothing it would read as an option.
fn is_concrete(alias: &str) -> bool {
    !alias.is_empty()
        && !alias.starts_with('-')
        && !alias.starts_with('!')
        && !alias.contains(['*', '?'])
        && !alias.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The files an `Include` names: `~/` is the home directory, a relative
/// path is under the user's `~/.ssh` (here, `base`), and `*`/`?` in the
/// last part match file names, in sorted order.
fn expand_include(pattern: &str, base: &Path) -> Vec<PathBuf> {
    let path = if let Some(rest) = pattern.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => return Vec::new(),
        }
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        base.join(pattern)
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut matched: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| wildcard(&name, &entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    matched.sort();
    matched
}

/// `*` and `?` matching, as `ssh` does for file names.
fn wildcard(pattern: &str, name: &str) -> bool {
    fn go(pattern: &[char], name: &[char]) -> bool {
        match (pattern.first(), name.first()) {
            (None, None) => true,
            (Some('*'), _) => {
                go(&pattern[1..], name) || (!name.is_empty() && go(pattern, &name[1..]))
            }
            (Some('?'), Some(_)) => go(&pattern[1..], &name[1..]),
            (Some(p), Some(n)) if p == n => go(&pattern[1..], &name[1..]),
            _ => false,
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    go(&pattern, &name)
}

/// The target a new session asked for, checked: one of the user's own
/// `known` host aliases (what [`hosts_in`] lists), never another name the
/// page sends, and an absolute folder.
pub fn remote_target(
    host: &str,
    path: &str,
    ask: bool,
    known: &[String],
) -> Result<ExecutionTarget, String> {
    let host = host.trim();
    let path = path.trim();
    if !is_concrete(host) || host.len() > 255 || !known.iter().any(|alias| alias == host) {
        return Err(format!(
            "`{}` is not a host from your SSH config",
            host.escape_debug()
        ));
    }
    if path.is_empty() {
        return Err(format!("Enter the folder on {host}."));
    }
    if path.chars().any(char::is_control) {
        return Err("The folder cannot contain line breaks or control characters.".into());
    }
    if !path.starts_with('/') {
        return Err("Use an absolute path, starting with /.".into());
    }
    Ok(ExecutionTarget::Ssh {
        host: host.to_string(),
        path: path.to_string(),
        ask,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn aliases_come_in_config_order_without_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        write(
            &config,
            "# my hosts\n\
             Host devbox\n  HostName 10.0.0.5\n  User me\n\
             \n\
             Host staging prod-eu\n\
             Host *.internal !bastion web?\n\
             Host=ci\n\
             host   \"qbox\"\n\
             \tHost\ttabbed\n\
             Host -oProxyCommand=evil\n\
             Host *\n  ServerAliveInterval 30\n\
             Match host foo\n  User x\n\
             Host devbox\n",
        );
        assert_eq!(
            hosts_in(&config),
            ["devbox", "staging", "prod-eu", "ci", "qbox", "tabbed"]
        );
    }

    #[test]
    fn includes_are_followed_relative_to_the_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        let elsewhere = dir.path().join("other.conf");
        write(&elsewhere, "Host gamma\n");
        write(
            &ssh.join("config"),
            &format!(
                "Include config.d/*\nHost main\nInclude {}\nInclude missing/*\n",
                elsewhere.display()
            ),
        );
        write(&ssh.join("config.d/b"), "Host beta\n");
        write(
            &ssh.join("config.d/a"),
            "Host alpha\n# loops back to the top\nInclude config\n",
        );
        assert_eq!(
            hosts_in(&ssh.join("config")),
            ["alpha", "beta", "main", "gamma"]
        );
    }

    #[test]
    fn a_missing_config_lists_no_hosts() {
        let dir = tempfile::tempdir().unwrap();
        assert!(hosts_in(&dir.path().join("nope")).is_empty());
    }

    fn known() -> Vec<String> {
        [
            "devbox",
            "dev box",
            "web*",
            "-oProxyCommand=touch /tmp/x",
            "",
        ]
        .map(String::from)
        .to_vec()
    }

    /// Only a host from the user's own SSH config: a page that was made to
    /// send another name (`ssh://…`, `user@…`) cannot point a session at it.
    #[test]
    fn only_a_host_from_the_users_config_is_accepted() {
        let known = vec!["devbox".to_string()];
        for host in [
            "evil.example",
            "ssh://evil.example:2222",
            "me@evil.example",
            "devbox2",
        ] {
            let error = remote_target(host, "/srv/app", true, &known).expect_err(host);
            assert!(error.contains("SSH config"), "{host}: {error}");
        }
        assert!(remote_target("devbox", "/srv/app", true, &known).is_ok());
    }

    #[test]
    fn a_remote_target_is_checked_before_anything_connects() {
        assert_eq!(
            remote_target("devbox", "/srv/app", true, &known()),
            Ok(ExecutionTarget::Ssh {
                host: "devbox".into(),
                path: "/srv/app".into(),
                ask: true,
            })
        );
        assert_eq!(
            remote_target(" devbox ", " /srv/app ", false, &known()),
            Ok(ExecutionTarget::Ssh {
                host: "devbox".into(),
                path: "/srv/app".into(),
                ask: false,
            }),
            "surrounding space is trimmed"
        );
        for (host, path, says) in [
            ("", "/srv/app", "host"),
            ("-oProxyCommand=touch /tmp/x", "/srv/app", "host"),
            ("dev box", "/srv/app", "host"),
            ("web*", "/srv/app", "host"),
            ("devbox", "", "folder"),
            ("devbox", "srv/app", "absolute"),
            ("devbox", "~/app", "absolute"),
            ("devbox", "/srv/app\nrm -rf /", "folder"),
            ("devbox", "/srv/\0app", "folder"),
        ] {
            let error = remote_target(host, path, true, &known()).expect_err(host);
            assert!(error.contains(says), "{host:?} {path:?}: {error}");
        }
    }
}
