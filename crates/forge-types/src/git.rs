//! Facts about git that more than one crate has to agree on.
//!
//! [`is_readonly_git_subcommand`] had two identical copies: one in
//! `forge-governance`, deciding whether a `bash git …` call can be rewritten
//! onto the confined `git` tool without a HITL prompt, and one in
//! `forge-transcript`, deciding whether a `git` call is presented as
//! exploration or as a change. They must not drift — a subcommand the
//! transcript calls read-only while governance treats it as mutating (or the
//! reverse) is a user-visible inconsistency between what the UI says happened
//! and what was actually allowed to happen without asking.

/// True when a git subcommand only reads the repository.
///
/// Deliberately conservative and matched on the subcommand alone: a
/// subcommand absent from this list is treated as mutating. Callers that need
/// more than the subcommand — `git branch -d` reads as `branch` here but
/// deletes — must apply their own argument checks on top.
pub fn is_readonly_git_subcommand(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "status" | "diff" | "log" | "show" | "branch" | "rev-parse" | "ls-files" | "blame"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_inspection_subcommands() {
        for subcommand in [
            "status",
            "diff",
            "log",
            "show",
            "branch",
            "rev-parse",
            "ls-files",
            "blame",
        ] {
            assert!(
                is_readonly_git_subcommand(subcommand),
                "`{subcommand}` should be read-only"
            );
        }
    }

    #[test]
    fn refuses_mutating_and_unknown_subcommands() {
        for subcommand in [
            "commit", "add", "push", "pull", "merge", "rebase", "checkout", "switch", "restore",
            "stash", "reset", "clean", "tag", "init", "clone", "fetch", "remote", "",
        ] {
            assert!(
                !is_readonly_git_subcommand(subcommand),
                "`{subcommand}` should not be read-only"
            );
        }
    }

    /// The list is matched exactly: callers lowercase the subcommand before
    /// asking, and neither an abbreviation nor a different case is accepted.
    #[test]
    fn matches_the_subcommand_exactly() {
        assert!(!is_readonly_git_subcommand("STATUS"));
        assert!(!is_readonly_git_subcommand("Status"));
        assert!(!is_readonly_git_subcommand("stat"));
        assert!(!is_readonly_git_subcommand("status --short"));
        assert!(!is_readonly_git_subcommand(" status"));
    }
}
