# Forge

**Forge** is an open-source, enterprise-ready **AI agent harness**: application scaffolding around foundation models for reliable, long-horizon work—especially repository-native software engineering.

| | |
|--|--|
| **License** | [MIT](./LICENSE) |
| **Repo** | [NorviaLabs/forge](https://github.com/NorviaLabs/forge) |
| **Language** | Rust (Tokio) |
| **Status** | **Phase 1 + Phase 2 implemented** |

---

## Product phases

| Phase | Product | Status |
|-------|---------|--------|
| **1** | Coding agent (tools, MCP, journal, CLI/REPL) | ✓ |
| **2** | Enterprise long-horizon harness (ACP, context, worktree, HITL, governance) | ✓ |
| **3** | Quality, ops & fleet (Evaluator, OTEL, channels, SCIM/SIEM) | Planned |

Details: [docs/prd.md](./docs/prd.md) §13 · [docs/designs/README.md](./docs/designs/README.md)

---

## Requirements

- Rust **1.80+**
- Optional: `FORGE_API_KEY` for live models; `rg` for `grep` tool
- Phase 2 worktree isolation requires a **git** repository

---

## Install

```bash
git clone git@github.com:NorviaLabs/forge.git
cd forge
cargo build --release -p forge-cli
./target/release/forge status
```

---

## Quick start

### Offline

```bash
./target/release/forge --mock run "hello"
./target/release/forge --mock repl
```

### Live model

```bash
export FORGE_API_KEY="…"
export FORGE_MODEL_PROVIDER=openai_compatible   # openai_compatible | anthropic | xai
export FORGE_MODEL_ID=gpt-4.1-mini
./target/release/forge run "Summarize this repo"
```

### Phase 2 features

```bash
# Git worktree isolation (CTX-03)
./target/release/forge --worktree --mock run "edit in isolation"

# Resume + HITL (exit code 2 when awaiting approval)
./target/release/forge --mock run --resume <session_id> "continue"
./target/release/forge approve --session <session_id>
./target/release/forge deny --session <session_id>
```

---

## CLI

```text
forge status
forge run [--resume UUID] [--max-turns N] <prompt>
forge repl [--resume UUID]
forge approve --session UUID      # Phase 2 HITL
forge deny --session UUID
```

| Flag | Meaning |
|------|---------|
| `--config` | Path to `forge.toml` |
| `--workspace` | Workspace root (default cwd) |
| `--provider` / `--model` | Model selection |
| `--mock` | Offline mock model |
| `--worktree` | Isolate file edits in a git worktree |

**Exit codes:** `0` success · `1` failed · `2` awaiting HITL · `3` canceled · `4` config error

---

## Configuration

Merge order: **CLI → env → `./forge.toml` → `~/.config/forge/config.toml` → defaults**.

| Env | Purpose |
|-----|---------|
| `FORGE_MODEL_PROVIDER` | `openai_compatible` \| `anthropic` \| `xai` |
| `FORGE_MODEL_ID` | Model id |
| `FORGE_API_KEY` | API key |
| `FORGE_WORKSPACE` | Workspace root |
| `FORGE_JOURNAL_PATH` | Journal dir (default `.forge/sessions`) |

Example `forge.toml`:

```toml
workspace_root = "."

[model]
provider = "openai_compatible"
model = "gpt-4.1-mini"

[journal]
path = ".forge/sessions"

[[mcp.servers]]
id = "example"
transport = "stdio"
command = "npx"
args = ["-y", "some-mcp-server"]
```

---

## Slash commands

**Phase 1:** `/help` `/status` `/resume` `/cancel` `/model` `/journal` `/tools` `/quit`  

**Phase 2:** `/approve` `/deny` `/reset` `/compact` `/cost` `/worktree status|merge|discard [--yes]`

---

## Crates

### Phase 1

| Crate | Design |
|-------|--------|
| `forge-config` | configuration.md |
| `forge-tools` | tool-protocol.md |
| `forge-model` | model-providers.md |
| `forge-durable` | durable-execution.md (+ HITL journal events in Phase 2) |
| `forge-core` | agent-loop.md (+ Phase 2 hooks) |
| `forge-mcp` | protocol-mcp.md |
| `forge-tui` | tui-commands.md |
| `forge-cli` | surfaces.md |

### Phase 2

| Crate | Design |
|-------|--------|
| `forge-governance` | governance.md (SEC-01/02/03) |
| `forge-context` | context-lifecycle.md (CTX-01/02) |
| `forge-workspace` | workspace-isolation.md (CTX-03) |
| `forge-acp` | protocol-acp.md (CORE-03) |

---

## Development

```bash
cargo test
cargo build --release -p forge-cli
```

Each design has unit tests in its crate. Config tests isolate ambient `FORGE_*` env vars.

---

## Documentation

| Doc | Description |
|-----|-------------|
| [docs/prd.md](./docs/prd.md) | Product requirements |
| [docs/architecture.md](./docs/architecture.md) | Architecture & implementation order |
| [docs/designs/README.md](./docs/designs/README.md) | Design docs by phase |
| [docs/ui.md](./docs/ui.md) | TUI mockups |

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
