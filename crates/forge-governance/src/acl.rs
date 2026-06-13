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
    pub fn is_allowed(
        &self,
        _principal: &Principal,
        tool: &str,
        _class: SideEffectClass,
    ) -> bool {
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
}
