# forge-governance

## Overview

Tool-path governance for Forge: allow-list filtering, human-in-the-loop (HITL)
gating, pattern-based narrowing of that gate, argument redaction, and an
in-memory audit trail. The crate is pure policy: it decides `Allow` / `Hitl` /
`Deny` for a `ToolCall` and never executes tools or touches the filesystem.

Shipped default policy: MCP tools ask for approval (`mcp:*`), shell and file
writes run without a prompt (the OS sandbox is the boundary), and an explicit
`deny` pattern re-prompts.

## Subdirectories & System Boundaries

- `src`: Component package with the `Governance` facade (`lib.rs`), ACL rules
  (`acl.rs`), audit events (`audit.rs`), and pattern rules (`pattern.rs`).
  Contract: [`./src/EGM.md`](./src/EGM.md).

## Interface

- Child component contract: [`./src/EGM.md`](./src/EGM.md) — the public
  symbols, invariants, and side effects implemented by this crate.
- Crate-level façade exports and verification target are documented there.

## Invariants

- `AclPolicy` is evaluated before any other gate; a deny there can never be
  overridden by a pattern rule.
- Pattern `allow` rules can only turn an already-gated call into `Allow`; they
  never gate a call that the ACL and HITL policy already allowed. Pattern
  `deny` rules may hold any call at `Hitl`, including one that is not otherwise
  gated, but can never produce `Deny`.
- `AclPolicy` matches last-rule-wins; an empty rule list denies everything
  (`allow_all()` installs `*`).
- Session pattern grants are shared across clones of `Governance` (parent and
  subagents see the same set) and are never persisted.
- Every `AuditLog` clone shares one append-only buffer, so events recorded by
  any clone are visible to all.
- Decisions are deterministic functions of the policy fields plus the call;
  no clock, randomness, or I/O is involved.

## Side Effects

- In-memory audit log growth (the only mutable state; unbounded within a
  process).
- Mutex acquisition for session grants and the audit log.
- No filesystem, network, or subprocess access.

## Verification

- Run: `cargo test --package forge-governance --locked`
