# Surfaces design (TUI + headless)

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **1** (historical label; product surfaces as of 2026-07)  
**PRD:** Multi-surface product slice  
**Architecture:** §8  
**Related:** [tui-shell.md](./tui-shell.md), [agent-loop.md](./agent-loop.md), [configuration.md](./configuration.md)

---

## 1. Problem / context

Operators need one agent core with two primary ways to use it: interactive terminal UI and headless automation.

## 2. Goals & non-goals

**Goals**

- TUI and headless are adapters only (no second agent loop).  
- Same session control API (`AgentSession`).  
- Redacted tool display; no secrets in chat.

**Non-goals (product CLI)**

- Line-mode `repl` subcommand (removed; use TUI).  
- Channel gateway as a CLI product (library crate only — [channels.md](./channels.md)).  
- ACP IDE as a CLI product (library crate only — [protocol-acp.md](./protocol-acp.md)).

## 3. Design

| Surface | How to invoke | Input | Output |
|---------|---------------|-------|--------|
| **TUI** | `forge` (default) | keys, slash cmds | ratatui full screen |
| **Headless** | `forge run "…"` | CLI prompt | logs, exit codes, `session_id` |
| **Status** | `forge status` | — | version / workspace / model |
| **Connect** | `forge connect …` | profile / key | credential store + hints |

### Headless exit codes

| Code | Meaning |
|------|---------|
| 0 | success |
| 1 | failed |
| 2 | awaiting HITL |
| 3 | canceled |
| 4 | config error |

### Configuration

No config file required. Defaults + env (`FORGE_MODEL_ID`, provider keys) + flags (`--model`, `--workspace`, `--resume`, `--worktree`, `--max-turns`).

## 4. Phase ownership

| Item | |
|------|--|
| This document | Surfaces product contract |
| ACP / channels | Library-only designs |

## Related docs

- [tui-shell.md](./tui-shell.md)  
- [../architecture.md](../architecture.md)  
