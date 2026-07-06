# Workspace isolation design (git worktree)

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 22 Jul 2026  
**Phase:** **2 only** (exclusive)  
**PRD:** CTX-03  
**Architecture:** Phase 2  
**Related:** [context-lifecycle.md](./context-lifecycle.md), [tool-protocol.md](./tool-protocol.md)

---

## 1. Problem / context

Agents editing the primary working tree can pollute uncommitted user work. Isolated **git worktrees** let experimental mutations land aside until merge or discard.

## 2. Goals & non-goals

**Goals**

- File-editing tools can run in a temporary worktree bound to the session.  
- Primary working tree remains clean during unapproved experimental work.  
- Explicit merge or discard via `/worktree` (and APIs).

**Non-goals**

- Full VM isolation (that is sandbox/governance).  
- Replacing code-review requirements on merge.  
- Multi-repo superproject orchestration in Phase 2.

## 3. Design

### 3.1 Lifecycle

```text
session start with isolation=worktree
  → git worktree add .forge/worktrees/<session_id> -b forge/<session_id>-…
  → tools resolve paths relative to worktree root
  → on /worktree merge → merge branch to target + remove worktree
  → on /worktree discard → confirm → git worktree remove --force
```

### 3.2 Path resolution

| Tool path input | Resolution |
|-----------------|------------|
| Relative | Under **active root** (worktree if enabled, else workspace) |
| Absolute under workspace | Allowed if policy permits; rewrite into worktree mirror when isolated |
| Absolute outside workspace | Deny by default |

### 3.3 Config

```toml
[workspace]
isolation = "off"       # off | worktree
worktree_dir = ".forge/worktrees"
```

Per-session override via CLI/session flags.

### 3.4 Interaction with other systems

| System | Interaction |
|--------|-------------|
| Journal | Record worktree path + branch in `session_created` / patches |
| Handoff | `progress.json` `workspace_ref` includes worktree id / git sha |
| Sandbox | Light profile: cwd = worktree root |
| HITL | merge may require HITL in strict policies |

### 3.5 Merge policy (initial)

- Default merge target: branch that was current when session started (or `main` if detached—**open**).  
- Conflicts → fail merge; report files; leave worktree intact.  
- Discard always requires confirmation in TUI.

## 4. Interfaces

```rust
struct WorktreeManager { … }
impl WorktreeManager {
    async fn ensure(&self, session: &Session) -> Result<PathBuf, WtError>;
    async fn merge(&self, session: &Session) -> Result<(), WtError>;
    async fn discard(&self, session: &Session) -> Result<(), WtError>;
    fn active_root(&self, session: &Session) -> PathBuf;
}
```

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Not a git repo | Isolation unavailable; error or fall back to off |
| Worktree add fails | Session error; no silent primary edits |
| Orphan worktrees | `forge doctor` / startup GC optional later |
| User edits primary mid-session | Document risk; isolation does not lock primary |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **2** |
| TUI commands | `/worktree status\|merge\|discard` |
| Exit | File edits isolated until merge/discard (CTX-03) |

## 7. Open questions

1. Default branch naming scheme.  
2. Auto-merge on session success vs always explicit.  
3. Sparse-checkout worktrees for huge monorepos.

## Related docs

- [governance.md](./governance.md)  
- [tui-commands.md](./tui-commands.md)  
