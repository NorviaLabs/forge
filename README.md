# Forge

**Let the agent work hard — without wrecking your branch.**  
Open coding-agent harness: crash-safe sessions, fail-closed tools, git worktree isolation.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)

Forge is a **harness** around foundation models for real repo work. You pick the model (via LiteLLM); Forge owns tools, session durability, isolation, and a full-screen TUI.

```bash
forge              # full-screen TUI
forge run "…"      # headless
forge status
```

**[Architecture](./docs/architecture.md)**

---

## Why Forge

Most coding agents edit **your current checkout** and treat chat history as “memory.” That’s fine until a crash, a bad tool call, or a runaway refactor hits the tree you’re on.

| Problem | What Forge does |
|---------|-----------------|
| Process dies mid-task | **Event journal** records model/tool steps *before* side effects; **resume without redoing completed work** |
| Bad tool args hit disk/shell | **Schema validation first** — invalid calls never execute |
| Agent pollutes your branch | Optional **git worktree isolation** — edits land in a session worktree until you merge or discard |
| Vendor lock-in | Open MIT harness; live models through **LiteLLM** (config/env switch) |

### Crash-safe sessions

Forge journals intent before tools and model steps complete. After a kill or crash:

```bash
forge --resume <session-id>
```

Completed tool results are reused; the agent doesn’t blindly replay the whole run. **Resume the agent, not just the chat.**

### Fail-closed tools

Every tool has a declared input schema. Invalid arguments are rejected **before** side effects. The model can correct and retry — your tree doesn’t get half-written garbage from a bad call.

### Git worktree isolation

Give the agent a **disposable workspace** bound to the session:

```bash
forge --worktree run "Refactor auth aggressively"
# or open the TUI with isolation
forge --worktree
```

What happens:

1. Git worktree under `.forge/worktrees/<session_id>/` on branch `forge/<id>`  
2. File tools resolve paths against that root — **primary working tree stays clean**  
3. You decide:  
   - `/worktree status` — path and branch  
   - `/worktree merge` — bring work into the base  
   - `/worktree discard --yes` — throw it away  

Autonomy without “hope it stayed on a feature branch.” Experiment, review, then **merge or discard**.

---

## Screenshots

<p align="center">
  <img src="docs/ui/images/01-home.png" alt="Forge TUI" width="880" />
</p>

<p align="center">
  <img src="docs/ui/images/02-chat-streaming.png" alt="Chat" width="430" />
  &nbsp;
  <img src="docs/ui/images/03-tool-execution.png" alt="Tools" width="430" />
</p>

---

## Install

**Need:** Rust 1.80+, Python 3 (for live models).

```bash
git clone https://github.com/NorviaLabs/forge.git
cd forge
cargo build --release -p forge-cli
export PATH="$PWD/target/release:$PATH"
pip install -e workers/forge-litellm-worker   # live models only
```

---

## Quick start

```bash
export OPENAI_API_KEY=…                    # or ANTHROPIC_API_KEY / XAI_API_KEY / …
export FORGE_MODEL_ID=openai/gpt-4.1-mini  # any LiteLLM model string

forge                              # TUI
forge run "Summarize this repo"    # headless
forge --resume <session-id>        # resume after a crash
forge --worktree run "…"           # isolate edits in a worktree
```

### Connect Grok or OpenCode Go

```bash
forge connect list
forge connect opencode_go --key "$OPENCODE_API_KEY"
# xAI Grok: OAuth via /connect in the TUI
```

Credentials: `~/.config/forge/credentials.toml` (mode `0600`).

---

## Configuration

Optional. Defaults work; env and flags override. Files (if present): `~/.config/forge/config.toml`, `./forge.toml`.

| Env | |
|-----|--|
| `FORGE_MODEL_ID` | LiteLLM model (`openai/…`, `anthropic/…`, `xai/…`) |
| `FORGE_MODEL_PROVIDER` | `litellm` |
| `OPENAI_API_KEY` / … | Provider keys for the worker |
| `FORGE_WORKSPACE` | Project root (default: cwd) |

---

## CLI

```text
forge [OPTIONS] [COMMAND]
```

| Command | |
|---------|--|
| *(none)* | Open the TUI |
| `run <prompt>` | Headless agent |
| `status` | Version / workspace / model |
| `connect [profile]` | Provider profiles |

**Flags:** `--config` · `--workspace` · `--model` · `--worktree` · `--resume` · `--max-turns`

**Exit codes:** `0` ok · `1` failed · `2` awaiting HITL · `3` canceled · `4` config

---

## Architecture

One agent core for TUI and headless. Live models use a Forge-managed **LiteLLM SDK worker**. Sessions use an append-only journal; tools can run under a session git worktree.

<p align="center">
  <img src="docs/images/architecture.png" alt="Forge architecture" width="880" />
</p>

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
