# Forge

**Forge** is an open-source, enterprise-ready **AI agent harness**: scaffolding around foundation models for reliable, long-horizon software engineering.

| | |
|--|--|
| **License** | [MIT](./LICENSE) |
| **Repo** | [NorviaLabs/forge](https://github.com/NorviaLabs/forge) |
| **Language** | Rust (Tokio) |
| **Status** | **Phases 1–4 implemented** |

---

## Product phases

| Phase | Product | Status |
|-------|---------|--------|
| **1** | Coding agent (tools, MCP, journal, CLI/REPL) | ✓ |
| **2** | Enterprise long-horizon (ACP, context, worktree, HITL, governance) | ✓ |
| **3** | Quality, ops & fleet (Evaluator, OTEL, channels, SCIM/SIEM) | ✓ |
| **4** | Full-screen terminal TUI (shell, conversation, sidebar, overlays) | ✓ |

---

## Install

```bash
git clone git@github.com:NorviaLabs/forge.git
cd forge
cargo build --release -p forge-cli
./target/release/forge status   # forge 0.4.0 phase4
```

---

## Quick start

```bash
# Offline
./target/release/forge --mock run "hello"
./target/release/forge --mock repl

# Phase 4 full-screen TUI (ratatui)
./target/release/forge --mock tui

# Live model
export FORGE_API_KEY=…
export FORGE_MODEL_PROVIDER=openai_compatible
export FORGE_MODEL_ID=gpt-4.1-mini
./target/release/forge run "Summarize this repo"

# Phase 2
./target/release/forge --worktree --mock run "edit safely"
./target/release/forge approve --session <uuid>

# Phase 3
./target/release/forge feedback --sensor "echo ok"
./target/release/forge --mock channel --kind webhook "status please"
./target/release/forge fleet
```

---

## CLI reference

| Command | Phase | Description |
|---------|-------|-------------|
| `status` | 1 | Version, workspace, model |
| `run` / `repl` | 1 | Headless / interactive agent |
| `tui` | 4 | Full-screen ratatui TUI (`/` palette, HITL modal) |
| `approve` / `deny` | 2 | HITL for a session |
| `feedback` | 3 | Run dual-sensor gate (EVAL-01) |
| `channel` | 3 | Restricted-ACL channel ingress (CH-01) |
| `fleet` | 3 | SCIM + SIEM plugins + obs export (FLEET-01/OBS-01) |

**Flags:** `--config` · `--workspace` · `--provider` · `--model` · `--mock` · `--worktree`  

**Exit codes:** `0` success · `1` failed · `2` awaiting HITL · `3` canceled · `4` config error

---

## Crates by phase

### Phase 1
`forge-types` · `forge-config` · `forge-tools` · `forge-model` · `forge-durable` · `forge-core` · `forge-mcp` · `forge-tui` · `forge-cli`

### Phase 2
`forge-governance` · `forge-context` · `forge-workspace` · `forge-acp`

### Phase 3
| Crate | Design |
|-------|--------|
| `forge-feedback` | [feedback-evaluator.md](./docs/designs/feedback-evaluator.md) |
| `forge-obs` | [observability.md](./docs/designs/observability.md) |
| `forge-channels` | [channels.md](./docs/designs/channels.md) |
| `forge-fleet` | [fleet-plugins.md](./docs/designs/fleet-plugins.md) |

### Phase 4
Full-screen UI in `forge-tui` + `forge tui` CLI entry:

| Design | Module(s) |
|--------|-----------|
| [tui-shell.md](./docs/designs/tui-shell.md) | `layout`, `theme`, `widgets/{status,input,footer}` |
| [tui-conversation.md](./docs/designs/tui-conversation.md) | `conversation` |
| [tui-sidebar.md](./docs/designs/tui-sidebar.md) | `sidebar` |
| [tui-overlays.md](./docs/designs/tui-overlays.md) | `overlays`, `app` |

---

## Development

```bash
cargo test
cargo build --release -p forge-cli
```

---

## Documentation

| Doc | Description |
|-----|-------------|
| [docs/prd.md](./docs/prd.md) | Product requirements & phase map |
| [docs/architecture.md](./docs/architecture.md) | Architecture |
| [docs/designs/README.md](./docs/designs/README.md) | Design docs |
| [docs/ui.md](./docs/ui.md) | TUI mockups |

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
