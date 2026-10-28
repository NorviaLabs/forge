# forge-governance/src

## Overview

Policy evaluation for the tool path. `lib.rs` holds the `Governance` facade and
decision order; `acl.rs` is the coarse tool allow-list; `pattern.rs` refines an
already-gated call using its argument subject (command, git subcommand, path,
or host); `audit.rs` is the shared in-memory decision log.

## Interface

- `Governance { principal, acl, hitl_tools, hitl_classes, audit,
  pattern_allow, pattern_deny }` with:
  - `with_principal`, `with_acl`, `require_hitl_for_tool`,
    `with_pattern_rules` (builder helpers),
  - `filter_tools(Vec<ToolDescriptor>) -> Vec<ToolDescriptor>`,
  - `authorize(&ToolCall, SideEffectClass) -> PolicyDecision`,
  - `grant_covers(&ToolCall) -> bool`,
  - `retain_session_patterns_from(&Self)`,
  - `allow_suggested_pattern_for_session(&ToolCall) -> Option<String>`,
  - `allow_pattern_for_session(PatternRule)`,
  - `clear_session_pattern_allows()`, `session_pattern_allow_count()`,
    `session_pattern_allows(&ToolCall) -> bool`,
  - `redact_args(&Value) -> Value`, `record_audit(AuditEvent)`.
- `Default for Governance`: `Principal::local_dev()`, `AclPolicy::allow_all()`,
  `hitl_tools = default_hitl_tools()` (`["mcp:*"]`), empty hitl classes and
  pattern rules, empty session grants.
- `AclRule { pattern, allow }`; `AclPolicy` with `new`, `allow_all`, `allow`,
  `deny`, `is_allowed(&Principal, tool, SideEffectClass) -> bool`.
- `AuditEvent { session_id, principal, tool, args_redacted, decision,
  policy_id, result, duration_ms, trace_id }`; `AuditLog` with `push`, `len`,
  `is_empty`, `snapshot`.
- `PatternRule { raw }` with `parse(&str) -> Option<Self>`, `matches(&ToolCall)`.
- `parse_pattern_rules<S: AsRef<str>>(&[S]) -> Vec<PatternRule>` (malformed
  entries dropped).
- `suggest_pattern(&ToolCall) -> Option<String>`.
- `is_shell_tool(&str) -> bool`, `default_hitl_tools() -> Vec<String>`.

## Invariants

- `authorize` order is fixed: ACL deny → explicit pattern deny (`Hitl`) →
  HITL membership by tool name/glob, side-effect class, or destructive git
  (`reset --hard`, `clean -f/--force`, worktree-touching `restore`,
  `checkout` with `--`/`-f`/`--force`/`-m`/`--merge`/`--ours`/`--theirs`,
  `switch -f/--force/--discard-changes`) → pattern allow → `Hitl`.
- A pattern `deny` is honoured even for tools that are not otherwise gated,
  so a user-written carve-out cannot be silently dropped when defaults change.
- `grant_covers` mirrors `authorize`'s allow/deny ordering and returns false
  when any deny rule matches; gates outside `authorize` must use it rather than
  `session_pattern_allows` to decide whether to skip a prompt.
- Session grants (`Arc<Mutex<Vec<PatternRule>>>`) are deduplicated by raw
  string, survive lock poisoning (poison is recovered), and are intentionally
  process-local.
- `allow_suggested_pattern_for_session` remembers nothing when
  `suggest_pattern` yields no rule that could match the call again.
- Shell-tool pattern rules with an argument pattern never apply to a call
  whose subject has unquoted control or expansion syntax, so a compound
  command cannot inherit a prefix permission. A bare tool rule (no argument
  pattern, e.g. `bash`) matches every call to that exact tool, compound
  commands included.
- Shell-equivalent tool names (`bash`, `sh`, `exec`, `background_run`,
  `exec_command`, …) share one command subject and canonical `bash(...)` rule
  for rules with an argument pattern; a bare tool rule matches only its exact
  tool name and is not unified across them.
- ACL matching is last-match-wins; no rules means deny.
- `redact_args` replaces values whose key contains `key`, `token`, or `secret`
  (case-insensitive) with `"[REDACTED]"`, recursively through objects/arrays.
- Audit events are append-only and must already carry redacted arguments;
  `Governance` never rewrites them.

## Side Effects

- Appends to the shared in-memory `AuditLog`.
- Mutates the shared session pattern-grant vector.
- Acquires mutexes; no filesystem, network, or subprocess access.
- `retain_session_patterns_from` swaps the shared grant handle, so later
  grants made via the old instance are no longer visible.

## Verification

- Run: `cargo test --package forge-governance --locked`
