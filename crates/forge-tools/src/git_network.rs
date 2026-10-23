//! The `git` tool's network operations under the session host allowlist.
//!
//! The `git` tool runs host `git` directly — not in the sandbox and not through
//! the egress proxy — because it relies on the user's own credential helpers,
//! SSH keys and config. The proxy therefore never sees its traffic, so the
//! host decision is made here instead: before spawning a subcommand that
//! contacts a remote, the remote's URL is resolved locally (config reads only,
//! never the network), its host extracted, and checked against the same
//! session policy a confined shell is held to. A host the policy does not
//! permit is reported as [`ToolError::SandboxDenied`] with `denied_host`, which
//! is what raises the existing "allow this host" prompt; the approved retry
//! re-runs the same call against the updated policy.
//!
//! Resolution fails closed: a remote whose URL cannot be determined, or whose
//! transport this module does not understand, is refused rather than guessed.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::process::Command;

use crate::registry::ToolContext;
use crate::ToolError;

/// Deadline for subcommands that contact a remote.
pub(crate) const GIT_NETWORK_TIMEOUT: Duration = Duration::from_secs(120);
/// Deadline for subcommands that may run repository hooks (a pre-commit hook
/// commonly runs a linter or test suite, which legitimately takes minutes).
pub(crate) const GIT_HOOK_TIMEOUT: Duration = Duration::from_secs(300);
/// Deadline for purely local subcommands, and for the config reads made here.
pub(crate) const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Subcommands that run hooks. `push` and `pull` also contact a remote; the
/// longer of the two deadlines applies.
const GIT_HOOK_SUBCOMMANDS: &[&str] = &["commit", "merge", "rebase", "cherry-pick", "push", "pull"];

/// `git remote` verbs that only read or edit local config. Every other verb is
/// either network (`show`, `prune`, `update`) or unknown and refused.
const GIT_REMOTE_LOCAL_VERBS: &[&str] = &[
    "add",
    "rename",
    "remove",
    "rm",
    "get-url",
    "set-url",
    "set-branches",
    "set-head",
];

/// Where a remote URL points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteLocation {
    /// A path or `file://` URL on this machine: no network.
    Local,
    /// A network host, lowercased, without userinfo or port.
    Host(String),
}

/// The deadline for one invocation of `subcommand`.
pub(crate) fn git_timeout(subcommand: &str, args: &[String]) -> Duration {
    if GIT_HOOK_SUBCOMMANDS.contains(&subcommand) {
        GIT_HOOK_TIMEOUT
    } else if is_network_invocation(subcommand, args) {
        GIT_NETWORK_TIMEOUT
    } else {
        GIT_LOCAL_TIMEOUT
    }
}

fn is_network_invocation(subcommand: &str, operands: &[String]) -> bool {
    match subcommand {
        "fetch" | "pull" | "push" | "clone" => true,
        "remote" => operands
            .iter()
            .find(|token| !token.starts_with('-'))
            .is_some_and(|verb| !GIT_REMOTE_LOCAL_VERBS.contains(&verb.as_str())),
        _ => false,
    }
}

/// Extract where `url` points, following git's own reading of a repository
/// argument (`transport.c`, `connect.c`).
///
/// * `<transport>::<address>` names a remote helper — refused, since the
///   helper decides what it contacts.
/// * `<scheme>://…` — `file` is local; the well-known network schemes yield the
///   authority's host; anything else is a helper and refused.
/// * `[user@]host:path` (a colon before any slash) is SSH's scp-like form.
/// * Anything else is a local path.
pub(crate) fn remote_location(url: &str) -> Result<RemoteLocation, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("the remote URL is empty".into());
    }
    let helper_name_len = url
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || b"-._~".contains(b))
        .count();
    if url[helper_name_len..].starts_with("::") {
        return Err(format!(
            "`{}::` remote helpers are not supported by the git tool",
            &url[..helper_name_len]
        ));
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        return match scheme.as_str() {
            "file" => Ok(RemoteLocation::Local),
            "http" | "https" | "ssh" | "git" | "git+ssh" | "ssh+git" | "ftp" | "ftps" => {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
                host_from_authority(authority).map(RemoteLocation::Host)
            }
            _ => Err(format!(
                "`{scheme}://` remotes are not supported by the git tool"
            )),
        };
    }
    let Some(colon) = url.find(':') else {
        return Ok(RemoteLocation::Local);
    };
    if url.find('/').is_some_and(|slash| slash < colon) {
        return Ok(RemoteLocation::Local);
    }
    // scp-like. A bracketed host may itself contain colons (`[::1]:repo`,
    // `[host:22]:repo`), so the host ends at `]` rather than the first colon.
    let host = match url.find('[') {
        Some(open) if open <= colon => {
            let close = url[open..]
                .find(']')
                .map(|i| open + i)
                .ok_or_else(|| "unterminated `[` in the remote URL".to_string())?;
            if !url[close + 1..].starts_with(':') {
                return Err("malformed scp-like remote URL".into());
            }
            let inner = &url[open + 1..close];
            let inner = inner.rsplit_once('@').map_or(inner, |(_, host)| host);
            // `[host:port]` carries a port; a bare IPv6 literal has several colons.
            match inner.split_once(':') {
                Some((host, port)) if !port.contains(':') => host.to_string(),
                _ => inner.to_string(),
            }
        }
        _ => {
            let authority = &url[..colon];
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host)
                .to_string()
        }
    };
    normalize_host(&host).map(RemoteLocation::Host)
}

/// The host of a URL authority (`user:pass@host:port`, `[v6]:port`).
fn host_from_authority(authority: &str) -> Result<String, String> {
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split_once(']')
            .map(|(host, _)| host)
            .ok_or_else(|| "unterminated `[` in the remote URL".to_string())?
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    normalize_host(host)
}

fn normalize_host(host: &str) -> Result<String, String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err("the remote URL has no host".into());
    }
    Ok(host)
}

/// Refuse `subcommand` when a remote it would contact is outside the session's
/// host allowlist. Runs before git is spawned; only local config is read.
pub(crate) async fn check_git_network(
    ctx: &ToolContext,
    subcommand: &str,
    operands: &[String],
    options: &[String],
) -> Result<(), ToolError> {
    // No egress grant means the session has no sandbox network policy at all
    // (sandbox unavailable, or governance disabled), so there is no allowlist
    // to hold git to. Host git keeps its previous unrestricted behaviour there
    // rather than being cut off from the network entirely.
    let Some(egress) = ctx.egress.as_deref() else {
        return Ok(());
    };
    if !is_network_invocation(subcommand, operands) {
        return Ok(());
    }
    let repo = LocalRepo {
        root: &ctx.workspace_root,
    };
    let targets = remote_targets(&repo, subcommand, operands, options).await?;
    for target in targets {
        let location = remote_location(&target.url).map_err(|why| {
            ToolError::Execution(format!(
                "git {subcommand}: refusing to contact {}: {why}",
                target.label
            ))
        })?;
        if let RemoteLocation::Host(host) = location {
            if !egress.permits_host(&host) {
                return Err(ToolError::SandboxDenied {
                    content: format!(
                        "git {subcommand}: network access to {host} ({}) is not allowed",
                        target.label
                    ),
                    reason: crate::egress::HOST_DENIED_EXPLANATION.to_string(),
                    denied_host: Some(host),
                });
            }
        }
    }
    Ok(())
}

/// One URL a subcommand will contact, with a label that names its origin
/// without echoing the URL (which may embed credentials).
#[derive(Debug)]
struct RemoteTarget {
    label: String,
    url: String,
}

async fn remote_targets(
    repo: &LocalRepo<'_>,
    subcommand: &str,
    operands: &[String],
    options: &[String],
) -> Result<Vec<RemoteTarget>, ToolError> {
    let mut targets = Vec::new();
    match subcommand {
        "clone" => {
            if let Some(source) = operands.first() {
                // Clone reads no repository config, so a rule in the
                // workspace's own `.git/config` must not rewrite its source.
                let url = repo.rewrite_url(source, false, false).await?;
                targets.push(RemoteTarget {
                    label: "the clone source".into(),
                    url,
                });
            }
        }
        "fetch" | "pull" => {
            let all = options.iter().any(|o| o == "--all")
                || (operands.is_empty() && repo.config_bool("fetch.all").await? == Some(true));
            if all {
                for name in repo.remotes().await? {
                    repo.push_fetch_repository(&name, &mut targets).await?;
                }
            } else if let Some(repository) = operands.first() {
                repo.push_fetch_repository(repository, &mut targets).await?;
            } else {
                targets.push(repo.default_fetch_target().await?);
            }
            let recurse_off = options.iter().any(|o| o == "--no-recurse-submodules")
                || repo.config_bool("fetch.recurseSubmodules").await? == Some(false);
            if !recurse_off {
                repo.push_submodule_urls(&mut targets).await?;
            }
        }
        "push" => {
            let name = match operands.first() {
                Some(repository) => repository.clone(),
                None => repo.default_push_remote().await?,
            };
            repo.push_push_repository(&name, &mut targets).await?;
            let recurse = repo.config_get("push.recurseSubmodules").await?;
            let on_demand = match recurse.as_deref() {
                Some(value) => value.eq_ignore_ascii_case("on-demand"),
                None => repo.config_bool("submodule.recurse").await? == Some(true),
            };
            if on_demand {
                repo.push_submodule_urls(&mut targets).await?;
            }
        }
        "remote" => {
            let mut words = operands.iter();
            let verb = words.next().map(String::as_str).unwrap_or_default();
            let names: Vec<&String> = words.collect();
            match verb {
                "show" | "prune" => {
                    for name in names {
                        repo.push_fetch_repository(name, &mut targets).await?;
                    }
                }
                "update" => {
                    if names.is_empty() {
                        match repo.config_get("remotes.default").await? {
                            Some(_) => repo.push_fetch_repository("default", &mut targets).await?,
                            None => {
                                for name in repo.remotes().await? {
                                    repo.push_fetch_repository(&name, &mut targets).await?;
                                }
                            }
                        }
                    } else {
                        for name in names {
                            repo.push_fetch_repository(name, &mut targets).await?;
                        }
                    }
                }
                other => {
                    return Err(ToolError::Execution(format!(
                        "git remote: cannot tell which host `remote {other}` contacts; refusing"
                    )));
                }
            }
        }
        _ => {}
    }
    Ok(targets)
}

/// Local, network-free reads of the workspace repository's configuration.
struct LocalRepo<'a> {
    root: &'a Path,
}

impl LocalRepo<'_> {
    async fn git(&self, args: &[&str]) -> Result<Output, ToolError> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match run_git_bounded(cmd, GIT_LOCAL_TIMEOUT).await? {
            GitRun::Finished(output) => Ok(output),
            GitRun::TimedOut => Err(ToolError::Execution(format!(
                "git: timed out reading local config (`git {}`); refusing to contact the remote",
                args.join(" ")
            ))),
        }
    }

    /// Stdout of a lookup that may legitimately find nothing (exit 1), as
    /// `None`. Any other failure is an error: the answer is unknown, and an
    /// unknown remote must not be treated as a local one.
    async fn lookup(&self, args: &[&str]) -> Result<Option<String>, ToolError> {
        let output = self.git(args).await?;
        match output.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned())),
            Some(1) => Ok(None),
            _ => Err(ToolError::Execution(format!(
                "git: could not read local config (`git {}`): {}; refusing to contact the remote",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }

    async fn config_get(&self, key: &str) -> Result<Option<String>, ToolError> {
        Ok(self
            .lookup(&["config", "--get", key])
            .await?
            .map(|value| value.trim().to_string()))
    }

    async fn config_bool(&self, key: &str) -> Result<Option<bool>, ToolError> {
        // `--type=bool` would reject non-boolean values such as
        // `fetch.recurseSubmodules=on-demand`; anything that is not plainly
        // false counts as on, which over-approximates the hosts checked.
        Ok(self.config_get(key).await?.map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "false" | "no" | "off" | "0"
            )
        }))
    }

    async fn config_all(&self, key: &str) -> Result<Vec<String>, ToolError> {
        Ok(self
            .lookup(&["config", "--get-all", key])
            .await?
            .map(|value| value.lines().map(str::to_string).collect())
            .unwrap_or_default())
    }

    async fn remotes(&self) -> Result<Vec<String>, ToolError> {
        // Outside a repository `git remote` fails; there are then no remotes.
        let output = self.git(&["remote"]).await?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect())
    }

    async fn current_branch(&self) -> Result<Option<String>, ToolError> {
        let output = self
            .git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .await?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|branch| !branch.is_empty()))
    }

    /// `git fetch <repository>`: a remote group, a configured remote, or a URL.
    /// Fetch consults `remotes.<name>` first, so a group wins over a remote.
    async fn push_fetch_repository(
        &self,
        repository: &str,
        targets: &mut Vec<RemoteTarget>,
    ) -> Result<(), ToolError> {
        let members: Vec<String> = self
            .config_all(&format!("remotes.{repository}"))
            .await?
            .iter()
            .flat_map(|line| line.split_whitespace().map(str::to_string))
            .collect();
        if members.is_empty() {
            targets.push(self.fetch_target(repository).await?);
        } else {
            for member in members {
                targets.push(self.fetch_target(&member).await?);
            }
        }
        Ok(())
    }

    /// `ls-remote --get-url` resolves a remote name or URL exactly as fetch
    /// does — including `insteadOf` rewriting — without contacting it.
    async fn fetch_target(&self, repository: &str) -> Result<RemoteTarget, ToolError> {
        let label = self.label_for(repository).await?;
        let url = self
            .lookup(&["ls-remote", "--get-url", "--", repository])
            .await?
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| undetermined(&label))?;
        Ok(RemoteTarget { label, url })
    }

    async fn default_fetch_target(&self) -> Result<RemoteTarget, ToolError> {
        let url = self
            .lookup(&["ls-remote", "--get-url"])
            .await?
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| undetermined("the default remote"))?;
        Ok(RemoteTarget {
            label: "the default remote".into(),
            url,
        })
    }

    /// Mirrors git's `pushremote_for_branch`: the branch's `pushRemote`, then
    /// `remote.pushDefault`, then the branch's `remote`, then the only
    /// configured remote, then `origin`.
    async fn default_push_remote(&self) -> Result<String, ToolError> {
        let branch = self.current_branch().await?;
        if let Some(branch) = &branch {
            if let Some(name) = self
                .config_get(&format!("branch.{branch}.pushRemote"))
                .await?
            {
                return Ok(name);
            }
        }
        if let Some(name) = self.config_get("remote.pushDefault").await? {
            return Ok(name);
        }
        if let Some(branch) = &branch {
            if let Some(name) = self.config_get(&format!("branch.{branch}.remote")).await? {
                return Ok(name);
            }
        }
        let remotes = self.remotes().await?;
        Ok(match remotes.as_slice() {
            [only] => only.clone(),
            _ => "origin".to_string(),
        })
    }

    /// `git push <repository>`: every push URL of a configured remote, or the
    /// URL itself after `pushInsteadOf`/`insteadOf` rewriting.
    async fn push_push_repository(
        &self,
        repository: &str,
        targets: &mut Vec<RemoteTarget>,
    ) -> Result<(), ToolError> {
        if self.remotes().await?.iter().any(|name| name == repository) {
            let label = format!("remote `{repository}`");
            let urls = self
                .lookup(&["remote", "get-url", "--push", "--all", "--", repository])
                .await?
                .unwrap_or_default();
            let urls: Vec<&str> = urls
                .lines()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .collect();
            if urls.is_empty() {
                return Err(undetermined(&label));
            }
            for url in urls {
                targets.push(RemoteTarget {
                    label: label.clone(),
                    url: url.to_string(),
                });
            }
        } else {
            let url = self.rewrite_url(repository, true, true).await?;
            targets.push(RemoteTarget {
                label: "the push URL".into(),
                url,
            });
        }
        Ok(())
    }

    async fn label_for(&self, repository: &str) -> Result<String, ToolError> {
        Ok(
            if self.remotes().await?.iter().any(|name| name == repository) {
                format!("remote `{repository}`")
            } else {
                "the repository URL".into()
            },
        )
    }

    /// Absolute URLs of initialised submodules, which a recursive fetch or push
    /// contacts in addition to the superproject's remote. Relative URLs resolve
    /// against the superproject's remote, which is already checked.
    async fn push_submodule_urls(&self, targets: &mut Vec<RemoteTarget>) -> Result<(), ToolError> {
        let Some(listing) = self
            .lookup(&["config", "-z", "--get-regexp", r"^submodule\..*\.url$"])
            .await?
        else {
            return Ok(());
        };
        for entry in listing.split('\0').filter(|e| !e.is_empty()) {
            let Some((_, url)) = entry.split_once('\n') else {
                continue;
            };
            if url.starts_with("./") || url.starts_with("../") {
                continue;
            }
            targets.push(RemoteTarget {
                label: "a submodule".into(),
                url: url.to_string(),
            });
        }
        Ok(())
    }

    /// Apply `url.<base>.insteadOf` (and, for a push, `pushInsteadOf` first)
    /// to a raw URL the way git does: the longest matching prefix wins.
    /// `include_repo` is false for clone, which reads no repository config.
    async fn rewrite_url(
        &self,
        url: &str,
        push: bool,
        include_repo: bool,
    ) -> Result<String, ToolError> {
        let listing = self
            .lookup(&[
                "config",
                "--show-scope",
                "-z",
                "--get-regexp",
                r"^url\..*\.(push)?insteadof$",
            ])
            .await?
            .unwrap_or_default();
        let rules = parse_rewrite_rules(&listing, include_repo);
        if push {
            if let Some(rewritten) = longest_rewrite(&rules, url, true) {
                return Ok(rewritten);
            }
        }
        Ok(longest_rewrite(&rules, url, false).unwrap_or_else(|| url.to_string()))
    }
}

fn undetermined(label: &str) -> ToolError {
    ToolError::Execution(format!(
        "git: could not determine the URL of {label}; refusing to contact it"
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct RewriteRule {
    push: bool,
    base: String,
    prefix: String,
}

/// Parse `git config --show-scope -z --get-regexp` output:
/// `scope\0key\nvalue\0` per entry.
fn parse_rewrite_rules(listing: &str, include_repo: bool) -> Vec<RewriteRule> {
    let mut rules = Vec::new();
    let mut tokens = listing.split('\0');
    while let (Some(scope), Some(entry)) = (tokens.next(), tokens.next()) {
        if !include_repo && matches!(scope, "local" | "worktree") {
            continue;
        }
        let Some((key, prefix)) = entry.split_once('\n') else {
            continue;
        };
        let Some(rest) = key.strip_prefix("url.") else {
            continue;
        };
        let lower = rest.to_ascii_lowercase();
        let (push, suffix_len) = if lower.ends_with(".pushinsteadof") {
            (true, ".pushinsteadof".len())
        } else if lower.ends_with(".insteadof") {
            (false, ".insteadof".len())
        } else {
            continue;
        };
        rules.push(RewriteRule {
            push,
            base: rest[..rest.len() - suffix_len].to_string(),
            prefix: prefix.to_string(),
        });
    }
    rules
}

fn longest_rewrite(rules: &[RewriteRule], url: &str, push: bool) -> Option<String> {
    rules
        .iter()
        .filter(|rule| rule.push == push && url.starts_with(&rule.prefix))
        .max_by_key(|rule| rule.prefix.len())
        .map(|rule| format!("{}{}", rule.base, &url[rule.prefix.len()..]))
}

/// How a bounded git run ended.
pub(crate) enum GitRun {
    Finished(Output),
    TimedOut,
}

/// Run `cmd` to completion or until `timeout`, whichever comes first. On
/// timeout the whole process group is killed — git forks `ssh`, credential
/// helpers and hooks, and killing only git would leave them running.
pub(crate) async fn run_git_bounded(
    mut cmd: Command,
    timeout: Duration,
) -> Result<GitRun, ToolError> {
    crate::builtins::set_process_group(&mut cmd);
    cmd.kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| ToolError::Execution(format!("failed to run git: {e}")))?;
    let mut guard =
        crate::builtins::ProcessGroupGuard::new(child.id().and_then(|pid| i32::try_from(pid).ok()));
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(output) => {
            // The leader has exited; leave anything it deliberately detached
            // (an SSH ControlMaster, say) alone, as before.
            guard.disarm();
            output
                .map(GitRun::Finished)
                .map_err(|e| ToolError::Execution(format!("failed to run git: {e}")))
        }
        Err(_) => {
            guard.kill();
            Ok(GitRun::TimedOut)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(url: &str) -> String {
        match remote_location(url) {
            Ok(RemoteLocation::Host(host)) => host,
            other => panic!("expected a host for `{url}`, got {other:?}"),
        }
    }

    #[test]
    fn https_hosts_drop_userinfo_port_and_case() {
        assert_eq!(host("https://github.com/o/r.git"), "github.com");
        assert_eq!(host("https://user:tok@GitHub.com:8443/o/r"), "github.com");
        assert_eq!(host("http://example.com"), "example.com");
        assert_eq!(host("https://[2001:db8::1]:443/r"), "2001:db8::1");
        assert_eq!(host("git://git.kernel.org/pub/r.git"), "git.kernel.org");
    }

    #[test]
    fn ssh_url_hosts() {
        assert_eq!(host("ssh://git@github.com/o/r.git"), "github.com");
        assert_eq!(
            host("ssh://git@gitlab.example.com:2222/o/r"),
            "gitlab.example.com"
        );
        assert_eq!(host("git+ssh://me@host.example/r"), "host.example");
    }

    #[test]
    fn scp_like_hosts() {
        assert_eq!(host("git@github.com:o/r.git"), "github.com");
        assert_eq!(host("github.com:o/r.git"), "github.com");
        assert_eq!(host("me@[git.example.com:2222]:r.git"), "git.example.com");
        assert_eq!(host("[::1]:repo"), "::1");
        assert_eq!(host("a@b@host.example:r"), "host.example");
    }

    #[test]
    fn local_paths_and_file_urls_are_local() {
        for url in [
            "file:///srv/repo.git",
            "/srv/repo.git",
            "../sibling",
            "./a:b",
            "repo",
            ".",
        ] {
            assert_eq!(remote_location(url), Ok(RemoteLocation::Local), "{url}");
        }
    }

    #[test]
    fn unknown_transports_fail_closed() {
        for url in [
            "ext::ssh -o ProxyCommand=x host",
            "persistent-https::https://x/r",
            "foo://host/r",
            "https:///r",
            "",
            "git@:r",
        ] {
            assert!(remote_location(url).is_err(), "`{url}` must be refused");
        }
    }

    #[test]
    fn rewrite_rules_take_the_longest_prefix_and_respect_scope() {
        let listing = "global\0url.https://github.com/.insteadof\ngh:\0\
                       local\0url.https://evil.example/.insteadof\ngh:o\0\
                       global\0url.ssh://git@github.com/.pushinsteadof\ngh:\0";
        let all = parse_rewrite_rules(listing, true);
        assert_eq!(
            longest_rewrite(&all, "gh:o/r", false).as_deref(),
            Some("https://evil.example//r")
        );
        assert_eq!(
            longest_rewrite(&all, "gh:o/r", true).as_deref(),
            Some("ssh://git@github.com/o/r")
        );
        let without_repo = parse_rewrite_rules(listing, false);
        assert_eq!(
            longest_rewrite(&without_repo, "gh:o/r", false).as_deref(),
            Some("https://github.com/o/r")
        );
        assert_eq!(longest_rewrite(&without_repo, "x:o/r", false), None);
    }

    #[test]
    fn timeouts_follow_the_subcommand_class() {
        let none: Vec<String> = Vec::new();
        assert_eq!(git_timeout("status", &none), GIT_LOCAL_TIMEOUT);
        assert_eq!(git_timeout("fetch", &none), GIT_NETWORK_TIMEOUT);
        assert_eq!(git_timeout("commit", &none), GIT_HOOK_TIMEOUT);
        assert_eq!(
            git_timeout("remote", &["update".to_string()]),
            GIT_NETWORK_TIMEOUT
        );
        assert_eq!(
            git_timeout("remote", &["add".to_string()]),
            GIT_LOCAL_TIMEOUT
        );
    }

    /// A blocking git invocation (an alias that sleeps) is cut off at the
    /// deadline, and the sleeping grandchild is killed with the group.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_git_run_past_its_deadline_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let mut cmd = Command::new("git");
        cmd.args([
            "-c",
            &format!("alias.hang=!sleep 2 && touch '{}'", marker.display()),
            "hang",
        ])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let started = std::time::Instant::now();
        let run = run_git_bounded(cmd, Duration::from_millis(200))
            .await
            .unwrap();
        assert!(matches!(run, GitRun::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            !marker.exists(),
            "the hook's grandchild must die with the group"
        );
    }
}
