# Forge

**Let the agent work hard — without wrecking your branch.**  
Open coding-agent harness: crash-safe sessions, fail-closed tools, git worktree isolation.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)

Forge is a **harness** around foundation models for real repo work. You pick the model (via LiteLLM); Forge owns tools, session durability, isolation, and a full-screen TUI.

**[Architecture](./docs/architecture.md)**

---

## Install

**Need:** Rust 1.80+, Python 3 (for live models).

```bash
git clone https://github.com/NorviaLabs/forge.git
cd forge
cargo build --release -p forge-cli
export PATH="$PWD/target/release:$PATH"
pip install -e workers/forge-litellm-worker
```

---

## Auth setup

Do this once before the tutorials. Live models go through the LiteLLM worker (not the LiteLLM Proxy).

### Option A — API key + model env (most providers)

```bash
# Pick one provider key (examples)
export OPENAI_API_KEY=…          # openai/…
# export ANTHROPIC_API_KEY=…     # anthropic/…
# export XAI_API_KEY=…           # if using key-based xAI routes

export FORGE_MODEL_PROVIDER=litellm
export FORGE_MODEL_ID=openai/gpt-4.1-mini   # any LiteLLM model string
```

Optional project file (not required):

```toml
# forge.toml
[model]
provider = "litellm"
model = "openai/gpt-4.1-mini"
```

### Option B — Product profiles (`connect`)

```bash
forge connect list

# OpenCode Go — API key
forge connect opencode_go --key "$OPENCODE_API_KEY"

# xAI Grok — OAuth in the TUI
forge
# then: /connect  →  pick xAI Grok  →  complete OAuth
```

Keys and tokens are stored under `~/.config/forge/credentials.toml` (mode `0600`). Never commit them.

Check what Forge sees:

```bash
forge status
```

---

## Tutorials

### Tutorial 1 — Interactive coding in the TUI

**Goal:** Chat with the agent, run tools, and steer a session at the keyboard.

1. Complete [Auth setup](#auth-setup).
2. From a git project (or any workspace):

```bash
cd /path/to/your/repo
forge
```

3. Type a task and press **Enter**, e.g. `Explain the layout of this crate`.
4. Try slash commands in the main textbox:
   - `/status` — session, model, context  
   - `/tools` — tools the model can see  
   - `/help` — command list  
5. **Ctrl+K** opens the command palette; **/** + **Tab** completes slash suggestions; **↑/↓** recall prior lines when not in the suggest list.
6. Quit with `/quit` or **Ctrl+C**.

<p align="center">
  <img src="docs/ui/images/01-home.png" alt="Forge TUI" width="880" />
</p>

---

### Tutorial 2 — Headless agent in CI (or a script)

**Goal:** Run the same harness as a subprocess with exit codes—no TUI.

1. Complete [Auth setup](#auth-setup) (in CI secrets / env).
2. Run a one-shot task:

```bash
forge run "Summarize what this repository does and list the main crates"
echo "exit=$?"
# 0 = ok · 1 = failed · 2 = awaiting HITL · 3 = canceled · 4 = config
```

3. Capture the session for later resume (printed as `session_id=…`):

```bash
forge run "Fix the failing unit tests" | tee /tmp/forge-out.txt
# note session_id= from the output
```

4. Example CI-shaped snippet:

```bash
export FORGE_MODEL_ID="${FORGE_MODEL_ID}"
export OPENAI_API_KEY="${OPENAI_API_KEY}"
forge run "Address the review comments on the latest commit" || exit 1
```

Use **Tutorial 2** for bots and pipelines; use **Tutorial 1** when a human needs to steer.

---

### Tutorial 3 — Safe experiment in a git worktree

**Goal:** Let the agent edit freely without dirtying your current branch.

1. Complete [Auth setup](#auth-setup).
2. Start from a **git** repository.
3. Run with isolation:

```bash
cd /path/to/your/repo
forge --worktree run "Refactor the error handling in the main module"
```

Or interactively:

```bash
forge --worktree
# then give the agent a risky task in the TUI
```

4. Confirm the primary tree stayed clean:

```bash
git status
# agent files live under .forge/worktrees/<session_id>/ on branch forge/<id>
```

5. In the TUI (or a resumed session), finish deliberately:
   - `/worktree status` — path and branch  
   - `/worktree merge` — bring changes into the base  
   - `/worktree discard --yes` — throw the experiment away  

**Primary checkout stays clean until you merge.**

---

### Tutorial 4 — Resume after a crash or kill

**Goal:** Continue a session without redoing completed tool work.

1. Start a run and note the id:

```bash
forge run "Large multi-step refactor of module X"
# → session_id=<uuid>
```

2. Simulate interruption (**Ctrl+C**) or wait for a CI timeout.
3. Resume:

```bash
# Interactive
forge --resume <uuid>

# Or headless continue
forge --resume <uuid> run "Continue from where you left off"
```

Forge journals model/tool steps **before** side effects. On resume, completed tools are not blindly re-executed. **Resume the agent, not just the chat.**

---

### Tutorial 5 — Connect OpenCode Go or xAI Grok

**Goal:** Use a productized provider profile on the same LiteLLM path.

**OpenCode Go (API key):**

```bash
forge connect list
forge connect opencode_go --key "$OPENCODE_API_KEY"
forge status
forge   # TUI with the connected profile’s models available
```

**xAI Grok (OAuth):**

```bash
forge
# /connect → select xAI Grok → complete OAuth in the UI
# (API-key paste is not the Grok path)
```

Credentials stay in `~/.config/forge/credentials.toml` (`0600`).

---

## Why these paths matter

| Problem | Forge |
|---------|--------|
| Process dies mid-task | Journal + `--resume` without redoing completed tools |
| Bad tool args hit disk/shell | Schema validation **before** side effects |
| Agent trashes your checkout | Session **git worktree** until merge/discard |
| Need automation, not a UI | **`forge run`** + exit codes |

<p align="center">
  <img src="docs/ui/images/02-chat-streaming.png" alt="Chat" width="430" />
  &nbsp;
  <img src="docs/ui/images/03-tool-execution.png" alt="Tools" width="430" />
</p>

---

## Configuration

Optional. Defaults + env + flags are enough (see [Auth setup](#auth-setup)).

| Env | |
|-----|--|
| `FORGE_MODEL_ID` | LiteLLM model string |
| `FORGE_MODEL_PROVIDER` | `litellm` |
| `OPENAI_API_KEY` / … | Provider keys for the worker |
| `FORGE_WORKSPACE` | Project root (default: cwd) |

**Flags:** `--config` · `--workspace` · `--model` · `--worktree` · `--resume` · `--max-turns`

**CLI:** `forge` (TUI) · `run` · `status` · `connect`

---

## Architecture

One agent core for TUI and headless. Live models use a Forge-managed **LiteLLM SDK worker**. Sessions journal for crash-safe resume; tools can run under a session git worktree.

<p align="center">
  <img src="docs/images/architecture.png" alt="Forge architecture" width="880" />
</p>

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
