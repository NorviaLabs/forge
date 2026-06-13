# Forge

**Forge** is an open-source, enterprise-ready **AI agent harness**: application scaffolding around foundation models for reliable, long-horizon work—especially repository-native software engineering.

It provides plan–act–observe loops, schema-validated tools, durable session recovery, MCP tool integration, and multi-surface access (CLI today; IDE/ACP and multi-channel later).

| | |
|--|--|
| **License** | [MIT](./LICENSE) |
| **Org** | [NorviaLabs/forge](https://github.com/NorviaLabs/forge) |
| **Language** | Rust (Tokio) |
| **Status** | Phase 1 coding agent implemented |

---

## What’s in Phase 1

A complete **local/CI coding agent** product:

| Capability | Design | Status |
|------------|--------|--------|
| TOML + env configuration | [configuration.md](./docs/designs/configuration.md) | ✓ |
| Schema-validated tools (CORE-01) | [tool-protocol.md](./docs/designs/tool-protocol.md) | ✓ |
| Multi-provider models | [model-providers.md](./docs/designs/model-providers.md) | ✓ |
| SQLite event journal + resume (DUR-01/02) | [durable-execution.md](./docs/designs/durable-execution.md) | ✓ |
| Agent loop (sequential tools) | [agent-loop.md](./docs/designs/agent-loop.md) | ✓ |
| MCP tools (CORE-02) | [protocol-mcp.md](./docs/designs/protocol-mcp.md) | ✓ |
| Slash commands | [tui-commands.md](./docs/designs/tui-commands.md) | ✓ |
| Headless CLI + REPL | [surfaces.md](./docs/designs/surfaces.md) | ✓ |

**Not in Phase 1** (later phases): ACP IDE, context handoff/offload, worktrees, durable HITL, vault/ACL enterprise path, Evaluator, OTEL export, multi-channel fleet. See [docs/prd.md](./docs/prd.md) §13.

---

## Requirements

- Rust **1.80+** (edition 2021)
- Optional: network + API key for live models (`FORGE_API_KEY`)
- Optional: `rg` (ripgrep) for the `grep` tool; falls back to system `grep`

---

## Install & build

```bash
git clone git@github.com:NorviaLabs/forge.git
cd forge
cargo build --release -p forge-cli
```

Binary: `./target/release/forge`

---

## Quick start

### Offline (mock model)

```bash
./target/release/forge status
./target/release/forge --mock run "hello"
./target/release/forge --mock repl
```

### Live model

```bash
export FORGE_API_KEY="…"
export FORGE_MODEL_PROVIDER=openai_compatible   # openai_compatible | anthropic | xai
export FORGE_MODEL_ID=gpt-4.1-mini

./target/release/forge run "Summarize the top-level layout of this repo"
./target/release/forge --workspace /path/to/project run "Add a README section"
```

### Resume a session

Sessions journal under `.forge/sessions/<session_id>.db` (relative to workspace):

```bash
./target/release/forge --mock run "first turn"
# note session_id=… in output
./target/release/forge --mock run --resume <session_id> "continue"
```

---

## CLI

```text
forge status                          # version, workspace, model
forge run <prompt>                    # headless one-shot / multi-turn
forge run --resume <uuid> <prompt>    # resume journaled session
forge run --max-turns 16 <prompt>
forge repl                            # interactive REPL (slash commands)
forge repl --resume <uuid>
```

**Global flags:**

| Flag | Meaning |
|------|---------|
| `--config <path>` | `forge.toml` path |
| `--workspace <path>` | Workspace root (default: cwd) |
| `--provider <name>` | `openai_compatible` \| `anthropic` \| `xai` |
| `--model <id>` | Model id |
| `--mock` | Offline mock model (no network) |

**Headless exit codes:** `0` success · `1` failed · `3` canceled · `4` config error  
(`2` reserved for Phase 2 HITL.)

---

## Configuration

Merge order (highest wins): **CLI → env → `./forge.toml` → `~/.config/forge/config.toml` → defaults**.

### Environment

| Variable | Maps to |
|----------|---------|
| `FORGE_MODEL_PROVIDER` | model provider |
| `FORGE_MODEL_ID` | model id |
| `FORGE_API_KEY` | API key (prefer over committing secrets) |
| `FORGE_WORKSPACE` | workspace root |
| `FORGE_JOURNAL_PATH` | journal directory (default `.forge/sessions`) |

### Example `forge.toml`

```toml
workspace_root = "."

[model]
provider = "openai_compatible"
model = "gpt-4.1-mini"
# base_url = "http://127.0.0.1:8000"   # local OpenAI-compatible proxy

[journal]
backend = "sqlite"
path = ".forge/sessions"

[[mcp.servers]]
id = "example"
transport = "stdio"
command = "npx"
args = ["-y", "some-mcp-server"]
```

Full rules: [docs/designs/configuration.md](./docs/designs/configuration.md).

---

## Built-in tools

| Tool | Role |
|------|------|
| `read_file` | Read workspace file |
| `write_file` | Write workspace file |
| `bash` | Shell in workspace cwd |
| `grep` | Search (`rg` or `grep`) |
| `mcp:demo:echo` | Demo MCP-namespaced tool (always registered for CORE-02 path) |

Configured `[[mcp.servers]]` are connected at startup (stdio). Tools are namespaced as `mcp:<server_id>:<tool>`.

Invalid tool arguments are **rejected before side effects**; the model can correct them (CORE-01).

---

## REPL slash commands (Phase 1)

| Command | Action |
|---------|--------|
| `/help` | List commands |
| `/status` | Session status |
| `/resume <uuid>` | Resume journal |
| `/cancel` | Cancel current turn |
| `/model [provider] [model]` | Request model switch (restart to apply) |
| `/journal [n]` | Recent agent events |
| `/tools` | List tools |
| `/quit` | Exit |

Phase 2 commands (`/approve`, `/reset`, `/worktree`, …) return an explicit “requires Phase 2” error. Canonical catalog: [tui-commands.md](./docs/designs/tui-commands.md).

---

## Workspace layout

```text
forge/
├── crates/
│   ├── forge-types/      # shared types
│   ├── forge-config/     # configuration.md
│   ├── forge-tools/      # tool-protocol.md
│   ├── forge-model/      # model-providers.md
│   ├── forge-durable/    # durable-execution.md
│   ├── forge-core/       # agent-loop.md
│   ├── forge-mcp/        # protocol-mcp.md
│   ├── forge-tui/        # tui-commands.md
│   └── forge-cli/        # surfaces.md (binary `forge`)
├── docs/
│   ├── prd.md
│   ├── architecture.md
│   ├── ui.md
│   └── designs/
└── README.md
```

---

## Development

```bash
cargo test                 # full suite
cargo test -p forge-config
cargo build --release -p forge-cli
```

Each Phase 1 design has corresponding unit/integration tests in its crate. Config tests isolate `FORGE_*` env vars so ambient shell vars don’t flaky-fail CI.

---

## Roadmap (product-complete phases)

| Phase | Product | Highlights |
|-------|---------|------------|
| **1** | Coding agent | Tools, MCP, journal, CLI/REPL — **this repo today** |
| **2** | Enterprise long-horizon harness | ACP, context lifecycle, worktrees, HITL, governance |
| **3** | Quality, ops & fleet | Evaluator, OTEL, channels, SCIM/SIEM |

Details: [docs/prd.md](./docs/prd.md) §13 · [docs/architecture.md](./docs/architecture.md) §14 · [docs/designs/README.md](./docs/designs/README.md)

---

## Documentation

| Doc | Description |
|-----|-------------|
| [docs/prd.md](./docs/prd.md) | Product requirements & phase map |
| [docs/architecture.md](./docs/architecture.md) | Architecture, flows, stack decisions |
| [docs/designs/README.md](./docs/designs/README.md) | Design docs (exclusive phase ownership) |
| [docs/ui.md](./docs/ui.md) | TUI mockups & workflows |

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
