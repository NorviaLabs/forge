# Forge — Design documents

**Status:** Aligned with shipped code (as of 2026-07-23)  
**Last updated:** 23 Jul 2026  

| Layer | Document | Owns |
|-------|----------|------|
| Product | [../prd.md](../prd.md) | Outcomes, req IDs |
| Architecture | [../architecture.md](../architecture.md) | System context, flows, stack |
| **Design** | **this folder** | One concern per file |
| UI mockups | [../ui.md](../ui.md) | Visuals |

## Implementation status

| Status | Meaning |
|--------|---------|
| **Shipped (product)** | Wired into `forge` CLI / default TUI and covered by tests |
| **Shipped (library)** | Crate + unit tests exist; **not** exposed as a first-class CLI command |
| **Superseded** | Historical design; production path is elsewhere |

### Shipped (product)

| Document | Req | Notes |
|----------|-----|-------|
| [tool-protocol.md](./tool-protocol.md) | CORE-01 | `forge-tools` registry + validation |
| [agent-loop.md](./agent-loop.md) | loop | `forge-core` AgentSession |
| [durable-execution.md](./durable-execution.md) | DUR-01, DUR-02 | SQLite journal |
| [protocol-mcp.md](./protocol-mcp.md) | CORE-02 | `forge-mcp` (static + config servers) |
| [configuration.md](./configuration.md) | config | TOML/env/CLI; **file optional** |
| [tui-commands.md](./tui-commands.md) | slash catalog | Parsed in TUI / was REPL |
| [surfaces.md](./surfaces.md) | surfaces | **TUI default + headless `run`** (no `repl` CLI) |
| [context-lifecycle.md](./context-lifecycle.md) | CTX-01, CTX-02 | Offload + handoff |
| [workspace-isolation.md](./workspace-isolation.md) | CTX-03 | `--worktree` |
| [durable-hitl.md](./durable-hitl.md) | DUR-03 | TUI HITL overlay (no CLI approve/deny) |
| [governance.md](./governance.md) | SEC-01–03 | ACL, secrets, light sandbox |
| [tui-shell.md](./tui-shell.md) | TUI-01 | Full-screen shell; entry **`forge`** |
| [tui-conversation.md](./tui-conversation.md) | TUI-02 | Chat + tool cards |
| [tui-sidebar.md](./tui-sidebar.md) | TUI-03 | Session / budget / activity |
| [tui-overlays.md](./tui-overlays.md) | TUI-04 | HITL, palette, model, connect |
| [model-providers.md](./model-providers.md) | MDL-01 | Native Rust provider transports |
| [connect-command.md](./connect-command.md) | CONN-01 | `/connect` + `forge connect` |
| [connect-auth-modes.md](./connect-auth-modes.md) | CONN-01 | OAuth vs API key |
| [provider-xai-grok.md](./provider-xai-grok.md) | PROV-01 | OAuth Grok |
| [provider-opencode-go.md](./provider-opencode-go.md) | PROV-02 | API-key Go |
| [tui-input-history.md](./tui-input-history.md) | TUI-05 | ↑/↓ history |
| [tui-slash-inline.md](./tui-slash-inline.md) | TUI-06 | Inline `/cmd` |
| [tui-slash-autocomplete.md](./tui-slash-autocomplete.md) | TUI-07 | Tab + highlight |
| [web-search-tool.md](./web-search-tool.md) | WEB-01 | `web_search` tool |
| [tui-status-feedback.md](./tui-status-feedback.md) | TUI-08 | Feedback strip + banners |
| [tui-session-chrome.md](./tui-session-chrome.md) | TUI-09 | Provider · model · ctx chrome |
| [tui-activity-feed.md](./tui-activity-feed.md) | TUI-10 | Activity feed + busy phase |

### Shipped (library only — not in CLI)

These crates compile and have unit tests. They are **not** product entry points (`forge` does not expose them as subcommands).

| Document | Req | Crate |
|----------|-----|-------|
| [protocol-acp.md](./protocol-acp.md) | CORE-03 | `forge-acp` |
| [feedback-evaluator.md](./feedback-evaluator.md) | EVAL-01 | `forge-feedback` |
| [observability.md](./observability.md) | OBS-01 | `forge-obs` |
| [channels.md](./channels.md) | CH-01 | `forge-channels` |
| [fleet-plugins.md](./fleet-plugins.md) | FLEET-01 | `forge-fleet` |

## Product CLI surface (normative)

```text
forge                    # full-screen TUI (default)
forge run "<prompt>"     # headless
forge status
forge connect …
```

**Flags:** `--config` · `--workspace` · `--model` · `--worktree` · `--resume` · `--max-turns`  

**Removed from CLI (do not document as product):** `repl`, `tui` subcommand, `--mock`, `approve`/`deny`, `feedback`, `channel`, `fleet`, `--provider`.

## Rules

1. **Exclusive phase ownership** on design headers is historical taxonomy only.  
2. **Exclusive req ownership** — one primary design doc per PRD req ID.  
3. Prefer **Status: Shipped** / **Library only** / **Superseded** in design headers over open-ended Draft when code exists.

**Last alignment:** 23 Jul 2026 — match CLI slim + product vs library crates.  
