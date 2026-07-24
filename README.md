# Forge

**Let the agent work hard — without wrecking your branch.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.86%2B-orange.svg)](https://www.rust-lang.org/)
[![CI](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml/badge.svg)](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml)

> **Alpha:** Expect bugs and breaking changes. Test in a disposable repository with committed or backed-up work, and read the [alpha testing guide](docs/alpha-testing.md) first.

---

## What is Forge

Forge is an open-source **AI coding-agent harness**: application scaffolding around foundation models for real repository work. You provide the model; Forge owns the native Rust provider transport, agent loop, tools, durable session state, optional git worktree isolation, and a full-screen terminal UI.

**Product surface**

| Command | Role |
|---------|------|
| `forge` | Full-screen TUI (default) |
| `forge run "…"` | Headless agent (CI / scripts) |
| `forge status` | Version, workspace, model |
| `forge connect …` | Provider profiles (e.g. Grok, OpenCode Go) |

**How it fits together**

One agent core serves both interactive and headless use. Live inference uses native Rust HTTP/SSE transports for OpenAI, Anthropic, xAI, OpenCode, Ollama, and OpenAI Codex subscriptions. Sessions use an append-only **journal** so work can resume after a crash. Tools can run against a **session git worktree** so the primary checkout stays clean until you merge or discard.

<p align="center">
  <img src="docs/images/architecture.png" alt="Forge architecture: surfaces, agent session, journal, governance, model providers, workspace and tools" width="880" />
</p>

| Layer | Role |
|-------|------|
| **You** | TUI or headless CLI |
| **Harness** | Plan–act–observe loop, tools, governance hooks, context lifecycle |
| **Durability** | SQLite event journal — resume without redoing completed steps |
| **Models** | Native Rust provider transports; same tools and journal regardless of vendor |
| **Workspace** | Your repo, optional `.forge/worktrees/<session>/` isolation |
| **Tools** | `read_file` · `write_file` · `apply_patch` · `bash` · `grep` · **`git`** (allowlisted subcommands) · `web_search` · MCP |

Full design notes: [docs/architecture.md](./docs/architecture.md).

---

## Why Forge

Most coding agents edit **your current checkout** and treat chat history as “memory.” That fails when the process dies, a tool call is invalid, or a runaway refactor hits the branch you care about.

| Problem | What Forge does |
|---------|-----------------|
| Process dies mid-task | **Event journal** records model/tool steps *before* side effects; **`--resume`** continues without blindly replaying completed work |
| Bad tool args hit disk/shell | **Schema validation first** — invalid calls never execute |
| Broad rewrites obscure small edits | **`apply_patch`** validates the full patch, confines paths to the workspace, and applies targeted add/update/delete operations |
| Agent pollutes your branch | Optional **git worktree isolation** — edits stay in a session worktree until `/worktree merge` or discard |
| Automation needs a subprocess | **`forge run`** with exit codes (`0` ok · `1` failed · `2` HITL · `3` canceled · `4` config) |
| Vendor lock-in | Open **MIT** harness; provider/model selection via config or environment |

**Crash-safe sessions** — Resume the agent, not just the chat:

```bash
forge --resume <session-id>
```

**Fail-closed tools** — Declared input schemas; failures go back to the model, not half-written files. For precise edits, `apply_patch` validates every operation before writing and rejects paths outside the active workspace.

**Disposable workspaces** — `forge --worktree` binds file tools to `.forge/worktrees/<session_id>/` on `forge/<id>`; your primary tree stays clean until you choose merge or discard.

---

## Install

Download the archive for your platform from [GitHub Releases](https://github.com/NorviaLabs/forge/releases).

| Platform | Architecture | Artifact target |
|----------|--------------|-----------------|
| macOS | Apple Silicon | `aarch64-apple-darwin` |
| macOS | Intel | `x86_64-apple-darwin` |
| Linux | x86_64 (glibc) | `x86_64-unknown-linux-gnu` |

```bash
tar -xzf forge-v0.1.0-alpha.1-<target>.tar.gz
mkdir -p ~/.local/bin
install -m 755 forge-v0.1.0-alpha.1-<target>/forge ~/.local/bin/forge
forge --version
```

Replace the example version and `<target>` with the downloaded release. Ensure `~/.local/bin` is on `PATH`. Each release includes `SHA256SUMS` for archive verification.

Alpha binaries are not yet code-signed or notarized. macOS may require you to approve Forge in **System Settings → Privacy & Security** after the first launch. Windows and Linux architectures other than x86_64 are not yet included.

To build from source, install Rust 1.86 or newer and Git:

```bash
git clone https://github.com/NorviaLabs/forge.git
cd forge
cargo build --release --locked --package forge-cli
export PATH="$PWD/target/release:$PATH"
```

For a first test, connect a provider and start Forge in a disposable Git repository with worktree isolation:

```bash
forge connect list
export ANTHROPIC_API_KEY="your-key"
forge connect anthropic
cd /path/to/disposable-repository
forge --worktree
```

---

## Auth setup

Do this once before the tutorials. Live models connect directly from Forge's Rust client.

### Option A — API key + model env (most providers)

```bash
export OPENAI_API_KEY=…          # openai/…
# export ANTHROPIC_API_KEY=…     # anthropic/…
# export XAI_API_KEY=…           # if using key-based xAI routes

export FORGE_MODEL_PROVIDER=native
export FORGE_MODEL_ID=openai/gpt-4.1-mini
```

Optional project file (not required):

```toml
# forge.toml
[model]
provider = "native"
model = "openai/gpt-4.1-mini"
```

### Option B — Product profiles (`connect`)

```bash
forge connect list

# API-key providers
forge connect openai --key "$OPENAI_API_KEY"
forge connect anthropic --key "$ANTHROPIC_API_KEY"
forge connect opencode_go --key "$OPENCODE_API_KEY"
forge connect opencode_zen --key "$OPENCODE_API_KEY"   # same key family; Zen base URL

# Local Ollama (no key; requires `ollama serve`)
forge connect ollama

# xAI Grok — OAuth in the TUI
forge
# then: /connect  →  pick xAI Grok  →  complete OAuth
```

Keys and tokens are stored in `forge/credentials.toml` under your operating system's user config directory with mode `0600` on Unix. For example, this is normally `~/.config/forge/credentials.toml` on Linux and `~/Library/Application Support/forge/credentials.toml` on macOS. Never commit them.

```bash
forge status
```

## Alpha safety and data

- Commit or stash existing work and use a disposable repository. `--worktree` reduces accidental branch edits but is not a security boundary.
- Repository-local sessions, context, progress, and worktrees are stored under `.forge/`.
- Prompts and selected repository context are sent to your chosen model provider, whose privacy and retention policies apply.
- Forge sends no separate product telemetry. Model providers and explicitly configured observability exporters can still receive network requests.
- Reset, uninstall, testing, and feedback instructions are in the [alpha testing guide](docs/alpha-testing.md).

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
   - `/diff` — tool activity and file changes
   - `/effort high` — set reasoning effort (`auto|minimal|low|medium|high|xhigh|max`)
   - `/model` — choose a connected model
5. **Ctrl+K** opens the command palette; **/** + **Tab** completes slash suggestions; **↑/↓** recall prior lines when not in the suggest list.
6. Select visible output with the mouse and use your terminal’s normal copy shortcut. Use **Page Up/Page Down** to scroll the conversation, or `/copy` to copy the last assistant answer.
7. Quit with `/quit` or **Ctrl+C**.

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

5. Review the isolated branch and worktree with TUI `/status` or `git worktree list`, then use normal Git commands to merge, cherry-pick, or remove it when finished.

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

---

### Tutorial 5 — Connect a provider profile

**Goal:** Attach credentials for a productized provider on the native Rust path.

| Profile | Auth | Example |
|---------|------|---------|
| `openai` | API key | `forge connect openai --key "$OPENAI_API_KEY"` |
| `anthropic` | API key | `forge connect anthropic --key "$ANTHROPIC_API_KEY"` |
| `opencode_go` | API key | `forge connect opencode_go --key "$OPENCODE_API_KEY"` |
| `opencode_zen` | API key | `forge connect opencode_zen --key "$OPENCODE_API_KEY"` (pay-per-use Zen catalog) |
| `ollama` | Local (no key) | `forge connect ollama` then pull a model in Ollama |
| `xai` | OAuth | TUI `/connect` → xAI Grok |

```bash
forge connect list
forge connect openai --key "$OPENAI_API_KEY"
forge status
forge
# Live catalog after connect:
#   /model              → picker (remote models + defaults)
#   /model refresh      → re-fetch catalogs
#   /model openai/gpt-4.1-mini
#   /model ollama llama3.2

# Reasoning effort (current TUI session):
#   /effort             → show current level
#   /effort high        → auto|minimal|low|medium|high|xhigh|max
#   FORGE_REASONING_EFFORT=high forge   → set the startup level

# Message queue (TUI only — no slash commands):
#   while processing → type + Enter enqueues (shows above input)
#   Ctrl+Up/Down → select a queued message
#   Ctrl+Backspace → cancel the selected message
#   when idle → empty Enter sends the next queued message
```

**xAI Grok (OAuth):**

```bash
forge
# /connect → select xAI Grok → complete OAuth in the UI
# (API-key paste is not the Grok path)
```

---

## Configuration

Optional. Defaults + env + flags are enough (see [Auth setup](#auth-setup)).

| Env | |
|-----|--|
| `FORGE_MODEL_ID` | Provider/model id, e.g. `openai/gpt-4.1-mini` |
| `FORGE_MODEL_PROVIDER` | `native` (`litellm` remains a legacy alias) |
| `FORGE_REASONING_EFFORT` | Startup reasoning effort (`auto|minimal|low|medium|high|xhigh|max`) |
| `OPENAI_API_KEY` / … | Provider credentials for native transports |
| `FORGE_WORKSPACE` | Project root (default: cwd) |

**Flags:** `--config` · `--workspace` · `--model` · `--worktree` · `--resume` · `--max-turns`

**CLI:** `forge` (TUI) · `run` · `status` · `connect`

---

## Maintainer alpha release

GitHub Actions builds alpha binaries for the supported platforms. To publish an alpha:

1. Ensure CI passes on `main` and update the workspace version if needed.
2. Create an annotated tag matching `v<version>-alpha.<number>`: `git tag -a v0.1.0-alpha.1 -m "Forge 0.1.0 alpha 1"`.
3. Push it: `git push origin v0.1.0-alpha.1`.
4. The [Alpha Release workflow](.github/workflows/release.yml) builds archives, generates `SHA256SUMS`, and publishes a GitHub prerelease with generated notes.
5. Download one archive, verify its checksum and `forge --version`, then complete a provider connection and small task in a disposable repository.

Only tags matching `v*-alpha.*` trigger this workflow, and releases are always marked as prereleases.

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
