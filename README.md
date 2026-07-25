# Forge

**Let the agent work hard — without wrecking your branch.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.86%2B-orange.svg)](https://www.rust-lang.org/)
[![CI](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml/badge.svg)](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml)

> **Alpha:** Expect bugs and breaking changes. Test in a disposable repository with committed or backed-up work, and read the [alpha testing guide](docs/alpha-testing.md) first.

---

## What is Forge

Forge is an open-source **AI coding-agent harness** written in Rust. It owns the agent loop, tools, durable session state, and a full-screen terminal UI, while you bring the model. Inference uses **native Rust HTTP/SSE transports** for OpenAI, Anthropic, xAI Grok, OpenCode (Go + Zen), OpenAI Codex, and local Ollama.

**Product surfaces**

| Command | Role |
|---------|------|
| `forge` | Full-screen TUI (default when run with no arguments) |

**How it fits together**

One agent core (`forge-core`) powers the interactive TUI. Sessions append to a **SQLite event journal** so work can resume after a crash or restart. The TUI exposes provider connection, model selection, and session steering via slash commands.

<p align="center">
  <img src="docs/images/architecture.png" alt="Forge architecture: agent session, journal, governance, model providers, workspace and tools" width="880" />
</p>

| Layer | Role |
|-------|------|
| **You** | TUI |
| **Harness** | Plan–act–observe loop, tools, governance hooks, context lifecycle |
| **Durability** | SQLite event journal — resume without redoing completed steps |
| **Models** | Native Rust provider transports; same tools and journal regardless of vendor |
| **Workspace** | Your repo, the active checkout |
| **Tools** | `read_file` · `write_file` · `apply_patch` · `bash` · `fffind` · `ffgrep` · **`git`** (allowlisted subcommands) · `web_search` (optional) · MCP |
| **Skills** | Optional `SKILL.md` packs from `~/.config/forge/skills/` or `.forge/skills/` injected into the system prompt |

Full design notes: [docs/architecture.md](./docs/architecture.md).

Want to help? See [CONTRIBUTING.md](./CONTRIBUTING.md) for setup, validation, and pull request guidance.

---

## Why Forge

Most coding agents edit **your current checkout** and treat chat history as "memory." That fails when the process dies, a tool call is invalid, or a runaway refactor hits the branch you care about.

| Problem | What Forge does |
|---------|-----------------|
| Process dies mid-task | **Event journal** records model/tool steps *before* side effects; the durable journal replays completed work |
| Bad tool args hit disk/shell | **Schema validation first** — invalid calls never execute |
| Broad rewrites obscure small edits | **`apply_patch`** validates the full patch, confines paths to the workspace, and applies targeted add/update/delete operations |
| Vendor lock-in | Open **MIT** harness; provider/model selection via config or environment |

**Crash-safe sessions** — Resume the agent, not just the chat:

**Fail-closed tools** — Declared input schemas; failures go back to the model, not half-written files. For precise edits, `apply_patch` validates every operation before writing and rejects paths outside the active workspace.

---

## Install

Download the archive for your platform from [GitHub Releases](https://github.com/NorviaLabs/forge/releases).

| Platform | Architecture | Artifact target |
|----------|--------------|-----------------|
| macOS | Apple Silicon | `aarch64-apple-darwin` |
| macOS | Intel | `x86_64-apple-darwin` |
| Linux | x86_64 (glibc) | `x86_64-unknown-linux-gnu` |

```bash
tar -xzf forge-v0.1.0-alpha.5-<target>.tar.gz
mkdir -p ~/.local/bin
install -m 755 forge-v0.1.0-alpha.5-<target>/forge ~/.local/bin/forge
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

---

## Auth setup

Do this once before the tutorials. Live models connect directly from Forge's native Rust client — no proxy server required.

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

Legacy provider names (`litellm`, `openai_compatible`, `anthropic`, `xai`, `grok`) are still accepted and migrate to `native`.

### Option B — Product profiles (TUI `/connect`)

```bash
forge
# then in the TUI:
#   /connect              → picker of all profiles
#   /connect openai       → paste API key
#   /connect anthropic    → paste API key
#   /connect opencode_go  → paste API key
#   /connect opencode_zen → paste API key (same key family; Zen base URL)
#   /connect ollama       → local (no key; requires `ollama serve`)
#   /connect xai          → OAuth flow for xAI Grok
```

| Profile | Auth | Notes |
|---------|------|-------|
| `openai` | API key | `openai/…` models |
| `openai_codex` | OAuth | OpenAI Codex subscriptions |
| `anthropic` | API key | `anthropic/…` models |
| `xai` | OAuth (TUI) | xAI Grok; API-key paste is not the Grok path |
| `opencode_go` | API key | OpenCode Go |
| `opencode_zen` | API key | OpenCode Zen (pay-per-use catalog) |
| `ollama` | Local (no key) | Requires `ollama serve` |

Keys and tokens are stored in `forge/credentials.toml` under your operating system's user config directory with mode `0600` on Unix (for example `~/.config/forge/credentials.toml` on Linux, `~/Library/Application Support/forge/credentials.toml` on macOS). Never commit them.

## Alpha safety and data

- Commit or stash existing work and use a disposable repository. Forge edits the active checkout directly.
- Repository-local sessions, context, and progress are stored under `.forge/`.
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
   - `/model` — choose a connected model (`/model refresh` re-fetches catalogs)
   - `/file <path>` — browse and read a workspace file read-only
5. **Ctrl+K** opens the command palette; **/** + **Tab** completes slash suggestions; **↑/↓** recall prior lines when not in the suggest list.
6. Select visible output with the mouse and use your terminal's normal copy shortcut. Use **Page Up/Page Down** to scroll the conversation, or `/copy` to copy the last assistant answer.
7. Quit with `/quit` or **Ctrl+C**.

<p align="center">
  <img src="docs/ui/images/01-home.png" alt="Forge TUI" width="880" />
</p>

---

### Tutorial 2 — Resume after a crash or kill

**Goal:** Continue a session without redoing completed tool work.

1. Start the TUI and run a task. The session id is shown in `/status`.
2. Quit or interrupt the process (**Ctrl+C**).
3. Start Forge again and use the `/resume` slash command to pick a previous session:

```bash
forge
# then in the TUI:
#   /resume          → picker of recent sessions
#   /resume <uuid>   → resume a specific session
```

The durable event journal replays completed tool work so the agent picks up where it left off.

---

### Tutorial 3 — Connect a provider profile

**Goal:** Attach credentials for a productized provider on the native Rust path, all from inside the TUI.

```bash
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
# /connect xai → complete OAuth in the TUI
# (API-key paste is not the Grok path)
```

---

## Skills

Optional instruction packs the agent loads into its system prompt:

| Location | Scope |
|----------|-------|
| `<workspace>/.forge/skills/<name>/SKILL.md` | Project only |
| `~/.config/forge/skills/<name>/SKILL.md` | Global (all projects) |

Project skills override global skills with the same name. Drop a `SKILL.md` in either path and start a new session — no extra config flag is required.

---

## Configuration

Optional. Defaults + env + flags are enough (see [Auth setup](#auth-setup)).

| Env | |
|-----|--|
| `FORGE_MODEL_ID` | Provider/model id, e.g. `openai/gpt-4.1-mini` |
| `FORGE_MODEL_PROVIDER` | `native` (production) or `mock` (offline CI). Legacy aliases migrate to `native` |
| `FORGE_REASONING_EFFORT` | Startup reasoning effort (`auto|minimal|low|medium|high|xhigh|max`) |
| `FORGE_WORKSPACE` | Project root (default: cwd) |
| `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `XAI_API_KEY` / … | Provider credentials for native transports |

**CLI:** `forge` launches the full-screen TUI; use `--help` or `--version` for CLI info.

`forge.toml` also supports `[mcp.servers]` entries for external MCP tool servers, and `[tools]`/`[model]`/`[journal]`/`[tui]` sections.

---

## Maintainer alpha release

GitHub Actions builds alpha binaries for the supported platforms. To publish an alpha:

1. Ensure CI passes on `main` and update the workspace version if needed.
2. Create an annotated tag matching `v<version>-alpha.<number>`: `git tag -a v0.1.0-alpha.5 -m "Forge 0.1.0 alpha 5"`.
3. Push it: `git push origin v0.1.0-alpha.5`.
4. The [Alpha Release workflow](.github/workflows/release.yml) builds archives, generates `SHA256SUMS`, and publishes a GitHub prerelease with generated notes.
5. Download one archive, verify its checksum and `forge --version`, then complete a provider connection and small task in a disposable repository.

Only tags matching `v*-alpha.*` trigger this workflow, and releases are always marked as prereleases.

---

## License

[MIT](./LICENSE) © 2026 NorviaLabs
