# Forge

**Let the agent work hard — without wrecking your branch.**  
Open coding-agent harness: crash-safe sessions, fail-closed tools, git worktree isolation.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)

Forge is a **harness** around foundation models for real repo work. You pick the model (via LiteLLM); Forge owns tools, session durability, isolation, and a full-screen TUI.

**[Architecture](./docs/architecture.md)**

---

## Use cases

### 1. Interactive coding (TUI)

**When:** You’re at the keyboard — explore a bug, steer the agent, approve risky steps.

```bash
export OPENAI_API_KEY=…   # or ANTHROPIC_API_KEY / XAI_API_KEY / …
export FORGE_MODEL_ID=openai/gpt-4.1-mini

forge                 # opens the full-screen TUI
```

Use the chat for tasks; slash commands (`/status`, `/tools`, `/connect`, `/worktree …`) and HITL overlays stay in the same session.

<p align="center">
  <img src="docs/ui/images/01-home.png" alt="Forge TUI" width="880" />
</p>

---

### 2. Headless / CI automation

**When:** No human at the terminal — PR bots, scheduled jobs, monorepo pipelines.

```bash
forge run "Fix failing tests in crates/foo"
echo $?   # 0 ok · 1 failed · 2 awaiting HITL · 3 canceled · 4 config
```

Same agent loop as the TUI, as a **subprocess**: set env, run, parse `session_id` / logs, fail the job on non-zero exit. Prefer this over the TUI for batch and CI.

```bash
# Example shape in CI
export FORGE_MODEL_ID=…
export OPENAI_API_KEY=…
forge run "Address review comments on the last commit"
```

---

### 3. Safe experiments (git worktree)

**When:** You want autonomy without polluting the branch you’re on.

```bash
forge --worktree run "Refactor auth aggressively"
# or interactive, still isolated:
forge --worktree
```

| Step | What happens |
|------|----------------|
| Start | Worktree at `.forge/worktrees/<session_id>/`, branch `forge/<id>` |
| During the run | File tools write **only** in that worktree — **primary tree stays clean** |
| Finish | `/worktree status` · `/worktree merge` · `/worktree discard --yes` |

**Autonomy with an explicit merge/discard boundary** — not “hope it stayed on a feature branch.”

---

### 4. Resume after a crash or kill

**When:** Laptop sleep, CI timeout, or process death mid-task.

Forge journals model/tool steps **before** side effects. Completed work is not blindly replayed.

```bash
# after a failed or interrupted run, note session_id from logs/output
forge --resume <session-id>              # back in the TUI
forge run "continue" --resume <session-id>   # if you pass resume via global flag
```

Global flag: `forge --resume <uuid> …` works with the default TUI and with `run`.

**Resume the agent, not just the chat.**

---

### 5. Connect a provider (Grok / OpenCode Go)

**When:** You want productized auth for specific backends on the same LiteLLM path.

```bash
forge connect list
forge connect opencode_go --key "$OPENCODE_API_KEY"
# xAI Grok: OAuth via /connect in the TUI
```

Credentials: `~/.config/forge/credentials.toml` (mode `0600`).

---

## Why these hold up

| Problem | Forge |
|---------|--------|
| Process dies mid-task | Event journal + `--resume` without redoing completed tools |
| Bad tool args hit disk/shell | Schema validation **before** side effects |
| Agent trashes your checkout | Session **git worktree** until merge/discard |
| Need automation, not a UI | **`forge run`** + exit codes for CI |

<p align="center">
  <img src="docs/ui/images/02-chat-streaming.png" alt="Chat" width="430" />
  &nbsp;
  <img src="docs/ui/images/03-tool-execution.png" alt="Tools" width="430" />
</p>

---

## Install

**Need:** Rust 1.80+, Python 3 (live models).

```bash
git clone https://github.com/NorviaLabs/forge.git
cd forge
cargo build --release -p forge-cli
export PATH="$PWD/target/release:$PATH"
pip install -e workers/forge-litellm-worker   # live models only
```

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
| *(none)* | Interactive TUI |
| `run <prompt>` | Headless / CI |
| `status` | Version / workspace / model |
| `connect [profile]` | Provider profiles |

**Flags:** `--config` · `--workspace` · `--model` · `--worktree` · `--resume` · `--max-turns`

**Exit codes:** `0` ok · `1` failed · `2` awaiting HITL · `3` canceled · `4` config

---

## Architecture

One agent core for TUI and headless. Live models use a Forge-managed **LiteLLM SDK worker**. Sessions journal for crash-safe resume; tools can run under a session git worktree.

<p align="center">
  <img src="docs/images/architecture.png" alt="Forge architecture" width="880" />
</p>

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
