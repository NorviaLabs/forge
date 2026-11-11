//! Which requests change something on a host under the user's identity.
//!
//! The gateway supplies the user's credential to a confined process's
//! requests, so it is also the place that can tell a read from a write: a
//! push, an API call that creates or deletes. Reads go through. A write is
//! named by its *target* (`github.com/owner/repo`, or just the host when the
//! request is not about one repository) and goes through when that target is
//! recorded for the project or the user allows it.

/// A write, as the user is asked about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    /// `host/owner/repo`, or `host` for a write not tied to one repository.
    pub target: String,
    /// What is being done, for the question.
    pub summary: String,
}

/// `owner/repo` from a repository path as a remote spells it
/// (`/owner/repo.git`, `owner/repo`). `None` when it is not two or more
/// plain segments.
fn repository(path: &str) -> Option<String> {
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let plain = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-~".contains(c))
    };
    (path.split('/').count() >= 2 && path.split('/').all(plain)).then(|| path.to_ascii_lowercase())
}

/// The write target a git remote URL pushes to: `https://host/o/r.git`,
/// `ssh://git@host/o/r`, `git@host:o/r.git`.
pub fn target_for_remote_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = host.split(':').next()?;
        (host, path)
    } else {
        // scp-like: [user@]host:path
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        (host, path)
    };
    let valid_host = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c));
    valid_host
        .then(|| repository(path))
        .flatten()
        .map(|repo| format!("{}/{repo}", host.to_ascii_lowercase()))
}

/// The write target of a git push over SSH: `host` and the quoted path of
/// `git-receive-pack '<path>'`.
pub fn target_for_ssh_push(host: &str, command: &str) -> String {
    let host = host.to_ascii_lowercase();
    let path = command
        .split_once(' ')
        .map_or("", |(_, path)| path)
        .replace(r"'\''", "'")
        .replace(r"'\!'", "!");
    let path = path.trim_matches('\'');
    match repository(path) {
        Some(repo) => format!("{host}/{repo}"),
        None => host,
    }
}

/// How an HTTP request to a credentialed host is treated.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Classified {
    Read,
    Write(Write),
    /// A GraphQL request: a query or a mutation, depending on its body.
    GraphQl,
}

/// Classify a request to `request_host`, made with the credential registered
/// for `credential_host` (the same host, or its parent: `api.github.com`
/// uses `github.com`'s). A push names the host git spoke to, as the remote's
/// URL does; an API call names the host the credential belongs to.
pub(crate) fn classify(
    credential_host: &str,
    request_host: &str,
    method: &str,
    target: &str,
) -> Classified {
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        return Classified::Read;
    }
    let path = target.split(['?', '#']).next().unwrap_or(target);
    if method == "POST" {
        // A fetch is a POST too.
        if path.ends_with("/git-upload-pack") {
            return Classified::Read;
        }
        if let Some(repo) = path.strip_suffix("/git-receive-pack").and_then(repository) {
            return Classified::Write(Write {
                target: format!("{}/{repo}", request_host.to_ascii_lowercase()),
                summary: format!("git push to {repo}"),
            });
        }
        if path == "/graphql" || path.ends_with("/api/graphql") {
            return Classified::GraphQl;
        }
    }
    // REST: `/repos/{owner}/{repo}/...` names the repository.
    let rest_repo = path
        .strip_prefix("/repos/")
        .or_else(|| path.strip_prefix("/api/v3/repos/"))
        .and_then(|rest| {
            let mut segments = rest.split('/');
            repository(&format!("{}/{}", segments.next()?, segments.next()?))
        });
    let summary = format!("{method} {path}");
    Classified::Write(Write {
        target: match rest_repo {
            Some(repo) => format!("{credential_host}/{repo}"),
            None => credential_host.to_string(),
        },
        summary,
    })
}

/// Classify a GraphQL request body: a document with a mutation is a write
/// to the host; a body that cannot be read is treated as one.
pub(crate) fn classify_graphql(credential_host: &str, body: &[u8]) -> Classified {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let query = parsed
        .as_ref()
        .and_then(|value| value.get("query"))
        .and_then(serde_json::Value::as_str);
    match query {
        Some(query) if !has_mutation(query) => Classified::Read,
        _ => {
            let name = parsed
                .as_ref()
                .and_then(|value| value.get("operationName"))
                .and_then(serde_json::Value::as_str)
                .filter(|name| name.len() <= 80 && name.chars().all(char::is_alphanumeric));
            Classified::Write(Write {
                target: credential_host.to_string(),
                summary: match name {
                    Some(name) => format!("API change ({name})"),
                    None => "API change".to_string(),
                },
            })
        }
    }
}

/// Whether a GraphQL document defines a mutation (or subscription): the
/// keyword at the top level, outside any selection set, string or comment.
fn has_mutation(document: &str) -> bool {
    let bytes = document.as_bytes();
    let (mut depth, mut i) = (0usize, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                let block = bytes[i..].starts_with(b"\"\"\"");
                i += if block { 3 } else { 1 };
                while i < bytes.len() {
                    if block && bytes[i..].starts_with(b"\"\"\"") {
                        i += 2;
                        break;
                    }
                    if !block && bytes[i] == b'"' {
                        break;
                    }
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth = depth.saturating_sub(1),
            byte if depth == 0 && byte.is_ascii_alphabetic() => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if matches!(&document[start..i], "mutation" | "subscription") {
                    return true;
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_name_their_repository() {
        for url in [
            "https://github.com/NorviaLabs/forge.git",
            "https://token@github.com/NorviaLabs/forge",
            "ssh://git@github.com/NorviaLabs/forge.git",
            "ssh://git@github.com:22/NorviaLabs/forge.git",
            "git@github.com:NorviaLabs/forge.git",
            "github.com:NorviaLabs/forge",
        ] {
            assert_eq!(
                target_for_remote_url(url).as_deref(),
                Some("github.com/norvialabs/forge"),
                "{url}"
            );
        }
        assert_eq!(
            target_for_remote_url("git@gitlab.example:group/sub/project.git").as_deref(),
            Some("gitlab.example/group/sub/project")
        );
        for url in [
            "/local/path",
            "../relative",
            "file:///srv/repo.git",
            "https://github.com/",
        ] {
            assert_eq!(target_for_remote_url(url), None, "{url}");
        }
    }

    #[test]
    fn an_ssh_push_names_the_same_target_as_its_remote() {
        assert_eq!(
            target_for_ssh_push("GitHub.com", "git-receive-pack 'NorviaLabs/forge.git'"),
            "github.com/norvialabs/forge"
        );
        assert_eq!(
            target_for_ssh_push("host.test", "git-receive-pack '/srv/git/it'\\''s'"),
            "host.test"
        );
    }

    fn write(target: &str, summary: &str) -> Classified {
        Classified::Write(Write {
            target: target.into(),
            summary: summary.into(),
        })
    }

    #[test]
    fn reads_fetches_and_writes_are_told_apart() {
        let host = "github.com";
        for (method, target) in [
            (
                "GET",
                "/NorviaLabs/forge.git/info/refs?service=git-receive-pack",
            ),
            ("POST", "/NorviaLabs/forge.git/git-upload-pack"),
            ("GET", "/repos/NorviaLabs/forge/pulls"),
            ("HEAD", "/user"),
        ] {
            assert_eq!(
                classify(host, host, method, target),
                Classified::Read,
                "{method} {target}"
            );
        }
        assert_eq!(
            classify(host, host, "POST", "/NorviaLabs/forge.git/git-receive-pack"),
            write(
                "github.com/norvialabs/forge",
                "git push to norvialabs/forge"
            )
        );
        assert_eq!(
            classify(
                host,
                "api.github.com",
                "POST",
                "/repos/NorviaLabs/forge/issues?x=1"
            ),
            write(
                "github.com/norvialabs/forge",
                "POST /repos/NorviaLabs/forge/issues"
            )
        );
        assert_eq!(
            classify(host, host, "DELETE", "/user/keys/1"),
            write("github.com", "DELETE /user/keys/1")
        );
        assert_eq!(
            classify(host, host, "POST", "/gists"),
            write("github.com", "POST /gists")
        );
        assert_eq!(
            classify(host, host, "POST", "/graphql"),
            Classified::GraphQl
        );
        // A git host under the credential's domain is named as the remote
        // names it.
        assert_eq!(
            classify(
                "example.test",
                "git.example.test",
                "POST",
                "/o/r.git/git-receive-pack"
            ),
            write("git.example.test/o/r", "git push to o/r")
        );
    }

    #[test]
    fn a_graphql_query_is_a_read_and_a_mutation_is_not() {
        let body = |query: &str| {
            serde_json::json!({ "query": query })
                .to_string()
                .into_bytes()
        };
        for query in [
            "query Viewer { viewer { login } }",
            "{ viewer { login } }",
            "# mutation in a comment\nquery { repository(name: \"mutation\") { id } }",
            "query Q($a: String = \"mutation\") { x(a: $a) }\nfragment F on User { mutation }",
        ] {
            assert_eq!(
                classify_graphql("github.com", &body(query)),
                Classified::Read,
                "{query}"
            );
        }
        for query in [
            "mutation { addStar(input: {}) { clientMutationId } }",
            "query A { viewer { login } }\nmutation B { x }",
            "subscription { x }",
        ] {
            assert!(
                matches!(
                    classify_graphql("github.com", &body(query)),
                    Classified::Write(_)
                ),
                "{query}"
            );
        }
        let named = serde_json::json!({"query": "mutation PullRequestCreate { x }", "operationName": "PullRequestCreate"});
        assert_eq!(
            classify_graphql("github.com", named.to_string().as_bytes()),
            write("github.com", "API change (PullRequestCreate)")
        );
        // Unreadable: assume the worst.
        assert!(matches!(
            classify_graphql("github.com", b"not json"),
            Classified::Write(_)
        ));
    }
}
