# Forge

**Forge** is an open-source, enterprise-ready **AI agent harness**: scaffolding around foundation models for reliable, long-horizon software engineering.

| | |
|--|--|
| **License** | [MIT](./LICENSE) |
| **Repo** | [NorviaLabs/forge](https://github.com/NorviaLabs/forge) |
| **Language** | Rust (Tokio) |
| **Status** | **Phases 1–8 implemented** |

---

## Product phases

| Phase | Product | Status |
|-------|---------|--------|
| **1** | Coding agent (tools, MCP, journal, CLI/REPL) | ✓ |
| **2** | Enterprise long-horizon (ACP, context, worktree, HITL, governance) | ✓ |
| **3** | Quality, ops & fleet (Evaluator, OTEL, channels, SCIM/SIEM) | ✓ |
| **4** | Full-screen terminal TUI (shell, conversation, sidebar, overlays) | ✓ |
| **5** | Universal providers via LiteLLM SDK (sole production path) | ✓ |
| **6** | `/connect` + xAI Grok + OpenCode Go profiles | ✓ |
| **7** | TUI command history (Up/Down arrows) | ✓ |
| **8** | Inline slash commands in main TUI textbox | ✓ |

---

## Install

```bash
git clone git@github.com:NorviaLabs/forge.git
cd forge
cargo build --release -p forge-cli
# Live model path also needs the Python worker:
pip install -e workers/forge-litellm-worker
./target/release/forge status   # forge 0.8.0 phase8
```

---

## Quick start

```bash
# Offline (no Python / LiteLLM required)
./target/release/forge --mock run "hello"
./target/release/forge --mock repl

# Phase 4 full-screen TUI (ratatui)
./target/release/forge --mock tui

# Live model (Phase 5: LiteLLM SDK worker — not Proxy)
pip install -e workers/forge-litellm-worker
export OPENAI_API_KEY=…   # or ANTHROPIC_API_KEY / XAI_API_KEY / …
export FORGE_MODEL_PROVIDER=litellm
export FORGE_MODEL_ID=openai/gpt-4.1-mini
./target/release/forge run "Summarize this repo"

# Phase 6: connect product profiles (keys stored under ~/.config/forge/credentials.toml)
./target/release/forge connect list
./target/release/forge connect xai --key "$XAI_API_KEY"
./target/release/forge connect opencode_go --key "$OPENCODE_API_KEY"
# or in TUI/REPL: /connect list · /connect xai

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
| `connect` | 6 | Connect xAI Grok or OpenCode Go (`list` / profile / key) |
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

### Phase 5
| Piece | Design |
|-------|--------|
| `workers/forge-litellm-worker` | [litellm-worker.md](./docs/designs/litellm-worker.md) |
| `forge-model` (`LiteLlmModelClient`) | [litellm-providers.md](./docs/designs/litellm-providers.md) |
| Config `provider=litellm` | [litellm-config.md](./docs/designs/litellm-config.md) |

Native OpenAI/Anthropic/xAI HTTP adapters removed; production uses LiteLLM only. `--mock` for CI.

### Phase 6
| Piece | Design |
|-------|--------|
| `forge-connect` | [connect-command.md](./docs/designs/connect-command.md) |
| xAI Grok profile | [provider-xai-grok.md](./docs/designs/provider-xai-grok.md) |
| OpenCode Go profile | [provider-opencode-go.md](./docs/designs/provider-opencode-go.md) |

### Phase 7
| Piece | Design |
|-------|--------|
| `InputHistory` + Up/Down in `forge tui` | [tui-input-history.md](./docs/designs/tui-input-history.md) |

### Phase 8
| Piece | Design |
|-------|--------|
| Inline `/command` in main textbox; Ctrl+K palette | [tui-slash-inline.md](./docs/designs/tui-slash-inline.md) |

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
