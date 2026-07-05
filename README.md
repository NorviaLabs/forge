# Forge

**AI coding agent for your terminal.** Durable sessions, real tools, any model — one command.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)

Forge runs a coding agent with schema-validated tools, crash-safe resume, and a full-screen TUI. You provide the model (via LiteLLM); Forge handles tools, context, approvals, and recovery.

```bash
forge          # open the TUI
forge status   # version, workspace, model
```

**[Architecture](./docs/architecture.md) · [TUI reference](./docs/ui.md) · [PRD](./docs/prd.md)**

---

## Screenshots

<p align="center">
  <img src="docs/ui/images/01-home.png" alt="Forge TUI home" width="880" />
</p>

<p align="center">
  <img src="docs/ui/images/02-chat-streaming.png" alt="Streaming chat" width="430" />
  &nbsp;
  <img src="docs/ui/images/03-tool-execution.png" alt="Tool cards" width="430" />
</p>

<p align="center">
  <img src="docs/ui/images/04-hitl-approval.png" alt="HITL approval" width="430" />
  &nbsp;
  <img src="docs/ui/images/07-slash-commands.png" alt="Slash commands" width="430" />
</p>

---

## Install

**Need:** Rust 1.80+, Python 3 (for live models).

```bash
git clone https://github.com/NorviaLabs/forge.git
cd forge
cargo build --release -p forge-cli
export PATH="$PWD/target/release:$PATH"

# Live models (LiteLLM Python SDK worker — not the LiteLLM Proxy)
pip install -e workers/forge-litellm-worker
```

---

## Quick start

```bash
# Model credentials (example: OpenAI via LiteLLM)
export OPENAI_API_KEY=…
export FORGE_MODEL_ID=openai/gpt-4.1-mini   # any LiteLLM model string

forge                              # full-screen TUI
forge run "Summarize this repo"    # headless
forge repl                         # line-mode chat
forge --resume <session-id>        # resume after a crash
```

### Connect Grok or OpenCode Go

```bash
forge connect list
forge connect opencode_go --key "$OPENCODE_API_KEY"
# xAI Grok uses OAuth in the TUI: /connect
```

Keys go to `~/.config/forge/credentials.toml` (mode `0600`).

### TUI cheatsheet

| Input | Action |
|-------|--------|
| Task + **Enter** | Run the agent |
| `/status` `/tools` `/cost` `/help` `/connect` | Slash commands in the textbox |
| `/` · **↑/↓** · **Tab** | Suggest and complete |
| **Ctrl+K** | Command palette |
| **↑/↓** | History (when not in suggestions) |

Status bar shows **provider · model · context**. Failures show in a feedback line and chat banners; wide layouts show an **ACTIVITY** feed.

---

## Tools

| Tool | Role |
|------|------|
| `read_file` / `write_file` | Edit the workspace |
| `bash` | Shell in the project |
| `grep` | Search code |
| `web_search` | Public web (optional API) |

Plus tools from configured **MCP** servers. Optional **git worktree** isolation: `forge --worktree run "…"`.

**Web search** defaults to offline fixture results. For live search:

```bash
export TAVILY_API_KEY=…   # or BRAVE_API_KEY / SERPER_API_KEY
# forge.toml: [tools.web_search] provider = "tavily"
```

---

## Configuration

Defaults ← `~/.config/forge/config.toml` ← `./forge.toml` ← env ← flags.

| Env | Purpose |
|-----|---------|
| `FORGE_MODEL_ID` | LiteLLM model id (`openai/…`, `anthropic/…`, `xai/…`, …) |
| `FORGE_MODEL_PROVIDER` | `litellm` (production) |
| `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `XAI_API_KEY` | Provider keys for the worker |
| `FORGE_WORKSPACE` | Project root (default: cwd) |

```toml
# forge.toml
[model]
provider = "litellm"
model = "openai/gpt-4.1-mini"

[model.litellm]
python = "python3"
module = "forge_litellm_worker"
```

---

## CLI

```text
forge [OPTIONS] [COMMAND]
```

| | |
|--|--|
| *(no command)* | Open the TUI |
| `run <prompt>` | Headless agent |
| `repl` | Line-mode chat |
| `status` | Version / workspace / model |
| `connect` | Provider profiles (Grok, OpenCode Go) |
| `approve` / `deny` | HITL for a session |
| `feedback` / `channel` / `fleet` | Ops / quality hooks |

**Flags:** `--config` · `--workspace` · `--provider` · `--model` · `--worktree` · `--resume` · `--max-turns`

**Exit codes:** `0` ok · `1` failed · `2` awaiting HITL · `3` canceled · `4` config

---

## Architecture

One agent core for TUI, REPL, and headless. Live inference uses a Forge-managed **LiteLLM SDK worker**. Sessions use an append-only journal for crash-safe resume.

<p align="center">
  <img src="docs/images/architecture.png" alt="Forge architecture diagram" width="880" />
</p>

Details: [docs/architecture.md](./docs/architecture.md) · diagram source: [docs/images/architecture.html](./docs/images/architecture.html)

---

## Docs & development

| | |
|--|--|
| [docs/prd.md](./docs/prd.md) | Product requirements |
| [docs/architecture.md](./docs/architecture.md) | System design |
| [docs/ui.md](./docs/ui.md) | TUI layouts & mockups |
| [docs/designs/](./docs/designs/README.md) | Design specs |
| [workers/forge-litellm-worker](./workers/forge-litellm-worker/README.md) | Model worker |

```bash
cargo test
cargo build --release -p forge-cli
```

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
