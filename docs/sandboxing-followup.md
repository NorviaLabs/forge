# Follow-up: OS-level sandboxing for the bash tool

Not implemented as part of the HITL/permission-model redesign
(`docs/hitl-permission-model-research.md`, phases 1–4: inline approval card,
pattern-based `Ask` rules + `permissions.toml`, spectrum remember +
reject-with-feedback, named permission modes). This doc is the note that
work's phase-4 scope explicitly deferred, describing what a sandboxing
follow-up would need to hook into.

## Why this matters now

Phases 1–4 make it easier to loosen how often Forge asks before running a
command — pattern-based auto-allow, named permission modes. (A third,
stricter `Locked` mode — deny instead of ask, for scripted/CI runs — was
scoped out of phase 4 itself: Forge has no non-interactive entry point yet
for "nothing can answer a prompt" to apply to. It'll want the same sandbox
floor described here once it exists.) None of this changes what a mistake,
or an approved-but-malicious command, can actually do to the machine. Today
the only thing standing
between an approved `bash` call and the rest of your filesystem/network is
the command itself. Every other harness surveyed in the research doc that
offers a comparably loose mode (Claude Code's `acceptEdits`/`bypassPermissions`,
Codex CLI's `workspace-write`) pairs it with an OS-level sandbox specifically
so that "ask less" doesn't also mean "risk more" — the sandbox is what makes
the loosening in phases 2–4 safe to actually use day to day, rather than
just theoretically safer than a raw shell.

## What exists to hook into

- `crates/forge-tools/src/builtins.rs` — the `bash` tool's `Tool` impl is
  where a command actually gets `std::process::Command`-spawned. This is the
  single execution choke point; a sandbox wrapper belongs here, not
  scattered across callers.
- `crates/forge-governance::Governance::authorize` — already the place that
  decides `Allow`/`Hitl`/`Deny` per phase 2–4's pattern/mode rules. A
  sandbox is a *different axis* from this (see below) and should stay a
  separate enforcement point, not be folded into `authorize`'s return value.
- `forge-config`'s `permissions.toml` loader
  (`crates/forge-config/src/permissions.rs`) is the natural place to add a
  `[sandbox]` table (writable paths, network allowlist) once there's an
  enforcement mechanism to configure — schema and precedence (repo file
  can't loosen personal file, same as the existing `allow`/`deny` split)
  should follow the same pattern already established there.

## The two axes, and where sandboxing sits

The research doc's core framing (section 3) still applies: **how often
you're asked** (rule lists, modes — what phases 1–4 built) and **what
happens if a wrong action slips through** (sandboxing — what's deferred)
are separate axes. A sandbox doesn't replace `hitl_tools`/pattern rules/
modes; it caps the blast radius underneath all of them, so that even a
mistaken `pattern_allow` rule fails safe at the OS level instead of relying
entirely on the rule being correct.

## What a follow-up would need to decide

- **Mechanism per platform**: Seatbelt (`sandbox-exec`) on macOS,
  bubblewrap/seccomp on Linux — both are what Claude Code and Codex CLI use;
  no Windows story surveyed yet.
- **Scope**: filesystem writes confined to the workspace root (plus
  explicitly always-read-only paths like `.git`, matching Codex CLI's
  treatment of `.git`/`.codex` as read-only even inside `workspace-write`);
  network egress off by default with an explicit domain allowlist.
- **Interaction with modes**: does the sandbox scope change per
  `PermissionMode`, or stay fixed regardless of mode (the latter is safer
  and matches the "two axes" framing above — modes tune prompting
  frequency, not blast radius)?
- **Failure behavior**: a sandboxed command that's denied by the OS (e.g.
  writes outside the workspace) needs a clear tool-result message back to
  the agent, distinct from an HITL denial — the agent should be able to
  tell "you weren't allowed to ask" apart from "this technically succeeded
  but was blocked by the sandbox," similar in spirit to the reject-with-
  feedback message added in phase 3.

This is intentionally a scoping note, not a design — the actual mechanism
selection and implementation is its own effort.
