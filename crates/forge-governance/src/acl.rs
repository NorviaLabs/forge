use forge_types::{Principal, SideEffectClass};

#[derive(Debug, Clone)]
pub struct AclRule {
    pub pattern: String,
    pub allow: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AclPolicy {
    rules: Vec<AclRule>,
}

impl AclPolicy {
    pub fn new() -> Self {
        Self { rules: vec![] }
    }

    pub fn allow_all() -> Self {
        let mut p = Self::new();
        p.allow("*".into());
        p
    }

    pub fn allow(&mut self, pattern: String) {
        self.rules.push(AclRule {
            pattern,
            allow: true,
        });
    }

    pub fn deny(&mut self, pattern: String) {
        self.rules.push(AclRule {
            pattern,
            allow: false,
        });
    }

    /// Last matching rule wins; default deny if no rules, allow if allow_all.
    pub fn is_allowed(&self, _principal: &Principal, tool: &str, _class: SideEffectClass) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let mut allowed = false;
        for r in &self.rules {
            if match_pattern(&r.pattern, tool) {
                allowed = r.allow;
            }
        }
        allowed
    }
}

fn match_pattern(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern == name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_overrides_allow_star() {
        let mut p = AclPolicy::allow_all();
        p.deny("bash".into());
        let pr = Principal::local_dev();
        assert!(p.is_allowed(&pr, "read_file", SideEffectClass::Read));
        assert!(!p.is_allowed(&pr, "bash", SideEffectClass::Exec));
    }

    /// A policy with no rules at all denies by default rather than allowing —
    /// distinct from `allow_all()`, which explicitly adds an allow-`*` rule.
    #[test]
    fn empty_policy_denies_by_default() {
        let p = AclPolicy::new();
        let pr = Principal::local_dev();
        assert!(!p.is_allowed(&pr, "read_file", SideEffectClass::Read));
        assert!(!p.is_allowed(&pr, "anything", SideEffectClass::Exec));
    }

    /// Rule order is the whole policy semantics: the *last* matching rule wins,
    /// so re-ordering two rules flips the outcome. A regression that made the
    /// first match win would silently widen `allow_all() + deny(...)` policies.
    #[test]
    fn last_matching_rule_wins_in_both_orders() {
        let pr = Principal::local_dev();

        let mut deny_last = AclPolicy::new();
        deny_last.allow("bash".into());
        deny_last.deny("bash".into());
        assert!(!deny_last.is_allowed(&pr, "bash", SideEffectClass::Exec));

        let mut allow_last = AclPolicy::new();
        allow_last.deny("bash".into());
        allow_last.allow("bash".into());
        assert!(allow_last.is_allowed(&pr, "bash", SideEffectClass::Exec));

        // A narrow rule only wins because it comes later, not because it is
        // narrower: a trailing broad allow re-opens an earlier specific deny.
        let mut broad_last = AclPolicy::new();
        broad_last.deny("bash".into());
        broad_last.allow("*".into());
        assert!(broad_last.is_allowed(&pr, "bash", SideEffectClass::Exec));
    }

    /// Non-matching rules must not disturb the decision carried by an earlier
    /// matching rule.
    #[test]
    fn non_matching_rules_do_not_reset_the_decision() {
        let pr = Principal::local_dev();
        let mut p = AclPolicy::new();
        p.allow("read_file".into());
        p.deny("bash".into());
        p.deny("write_file".into());
        assert!(p.is_allowed(&pr, "read_file", SideEffectClass::Read));
        // A tool that matches nothing at all falls back to deny.
        assert!(!p.is_allowed(&pr, "glob", SideEffectClass::Read));
    }

    /// The `mcp:*` shape is the reason `match_pattern` has a prefix wildcard at
    /// all: one rule must cover every tool from a server without covering
    /// look-alike names.
    #[test]
    fn trailing_star_matches_by_prefix_only() {
        let pr = Principal::local_dev();
        let mut p = AclPolicy::allow_all();
        p.deny("mcp:*".into());
        assert!(!p.is_allowed(&pr, "mcp:github:create_issue", SideEffectClass::Write));
        assert!(!p.is_allowed(&pr, "mcp:", SideEffectClass::Write));
        // Prefix, not substring or suffix.
        assert!(p.is_allowed(&pr, "mcp", SideEffectClass::Write));
        assert!(p.is_allowed(&pr, "not_mcp:x", SideEffectClass::Write));
        assert!(p.is_allowed(&pr, "MCP:github", SideEffectClass::Write));
    }

    /// A bare `*` is the match-everything rule; only a *trailing* star is a
    /// wildcard, so a star anywhere else is matched literally.
    #[test]
    fn star_is_only_special_as_a_whole_pattern_or_a_suffix() {
        let pr = Principal::local_dev();

        let mut interior = AclPolicy::allow_all();
        interior.deny("mcp:*:read".into());
        assert!(interior.is_allowed(&pr, "mcp:github:read", SideEffectClass::Read));
        assert!(!interior.is_allowed(&pr, "mcp:*:read", SideEffectClass::Read));

        let mut everything = AclPolicy::new();
        everything.allow("*".into());
        assert!(everything.is_allowed(&pr, "", SideEffectClass::Meta));

        // `**` strips one trailing star and prefix-matches on the remaining one.
        let mut double = AclPolicy::new();
        double.allow("**".into());
        assert!(double.is_allowed(&pr, "*anything", SideEffectClass::Meta));
        assert!(!double.is_allowed(&pr, "anything", SideEffectClass::Meta));
    }

    /// Exact patterns must not match by prefix.
    #[test]
    fn exact_patterns_require_the_whole_name() {
        let pr = Principal::local_dev();
        let mut p = AclPolicy::new();
        p.allow("git".into());
        assert!(p.is_allowed(&pr, "git", SideEffectClass::Write));
        assert!(!p.is_allowed(&pr, "github", SideEffectClass::Write));
        assert!(!p.is_allowed(&pr, "gi", SideEffectClass::Write));
    }

    /// The side-effect class and principal are accepted for signature stability
    /// but do not participate in the decision; only the tool name does.
    #[test]
    fn decision_ignores_principal_and_side_effect_class() {
        let mut p = AclPolicy::allow_all();
        p.deny("bash".into());
        let pr = Principal::local_dev();
        for class in [
            SideEffectClass::Read,
            SideEffectClass::Write,
            SideEffectClass::Exec,
            SideEffectClass::Meta,
        ] {
            assert!(!p.is_allowed(&pr, "bash", class));
            assert!(p.is_allowed(&pr, "read_file", class));
        }
    }

    /// A deny-only policy must not turn into an allow-list for everything it
    /// does not mention. The absence of an allow rule is still fail-closed.
    #[test]
    fn deny_only_policy_denies_every_unmatched_tool() {
        let mut p = AclPolicy::new();
        p.deny("bash".into());
        let pr = Principal::local_dev();

        assert!(!p.is_allowed(&pr, "bash", SideEffectClass::Exec));
        for tool in ["read_file", "mcp:github:create_issue", "", "BASH"] {
            assert!(
                !p.is_allowed(&pr, tool, SideEffectClass::Meta),
                "deny-only policy must not allow unmatched tool {tool:?}"
            );
        }
    }

    /// Prefix rules are deliberately simple name prefixes. They must not
    /// become substring, case-folded, or boundary-aware matching by accident.
    #[test]
    fn prefix_allowlist_rejects_lookalike_tool_names() {
        let mut p = AclPolicy::new();
        p.allow("mcp:*".into());
        let pr = Principal::local_dev();

        for tool in ["mcp:", "mcp:github:create_issue", "mcp:server/tool"] {
            assert!(p.is_allowed(&pr, tool, SideEffectClass::Write));
        }
        for tool in [
            "mcp",
            "mcpx:github:create_issue",
            "not_mcp:github:create_issue",
            "MCP:github:create_issue",
            "m\u{441}p:github:create_issue",
        ] {
            assert!(
                !p.is_allowed(&pr, tool, SideEffectClass::Write),
                "prefix rule must not allow lookalike tool {tool:?}"
            );
        }
    }

    /// A later broad rule can reopen a denied name, while a later exact rule
    /// can carve one name back out. This pins ordering across wildcard forms,
    /// not only two identical exact rules.
    #[test]
    fn overlapping_rules_follow_insertion_order() {
        let pr = Principal::local_dev();
        let mut p = AclPolicy::new();
        p.allow("*".into());
        p.deny("mcp:*".into());
        p.allow("mcp:trusted".into());

        assert!(p.is_allowed(&pr, "read_file", SideEffectClass::Read));
        assert!(!p.is_allowed(&pr, "mcp:github", SideEffectClass::Write));
        assert!(p.is_allowed(&pr, "mcp:trusted", SideEffectClass::Write));
        // `mcp:*` is a prefix rule, so the name without its prefix remains
        // governed by the earlier match-everything rule.
        assert!(p.is_allowed(&pr, "mcp", SideEffectClass::Write));

        let mut deny_last = AclPolicy::new();
        deny_last.allow("mcp:trusted".into());
        deny_last.deny("mcp:*".into());
        assert!(!deny_last.is_allowed(&pr, "mcp:trusted", SideEffectClass::Write));
    }

    /// Tool names are identifiers supplied by the registry, not free-form
    /// user text. ACL matching therefore must not trim or case-fold them.
    #[test]
    fn exact_rules_do_not_normalize_tool_names() {
        let mut p = AclPolicy::new();
        p.allow("bash".into());
        let pr = Principal::local_dev();

        assert!(p.is_allowed(&pr, "bash", SideEffectClass::Exec));
        for lookalike in ["BASH", " bash", "bash ", "bash\n"] {
            assert!(
                !p.is_allowed(&pr, lookalike, SideEffectClass::Exec),
                "exact ACL rule must not normalize tool name {lookalike:?}"
            );
        }
    }
}
