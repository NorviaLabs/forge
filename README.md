# Forge

**Let the agent work hard — without wrecking your branch.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.86%2B-orange.svg)](https://www.rust-lang.org/)
[![CI](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml/badge.svg)](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml)

> **Alpha:** Expect bugs and breaking changes. Test in a disposable repository with committed or backed-up work, and read the [alpha testing guide](docs/alpha-testing.md) first.

---

## What is Forge

Forge is an open-source **AI coding-agent harness** written in Rust. You run Forge in your terminal; it owns the agent loop, tools, durable session state, and a full-screen UI. You bring the model — Forge connects natively to OpenAI, Anthropic, xAI Grok, OpenCode (Go + Zen), OpenAI Codex, and local Ollama over Rust HTTP/SSE transports.

```
forge
```

That's it. `forge` launches the full-screen TUI. There are no subcommands — use `--help` or `--version` for CLI info.

**Why Forge**

| Problem | What Forge does |
|---------|-----------------|
| Process dies mid-task | **Event journal** records every model/tool step *before* side effects; `/resume` picks up where you left off |
| Bad tool args hit disk/shell | **Schema validation first** — invalid calls never execute |
| Broad rewrites obscure small edits | **`apply_patch`** validates the full patch, confines paths to the workspace, and applies targeted add/update/delete operations |
| Vendor lock-in | Open **MIT** harness; provider/model selection via env, `forge.toml`, or `/connect` in the TUI |

---

## Quick start

### 1. Install

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

### 2. Connect a provider

Live models connect directly from Forge's native Rust client — no proxy server required.

**Option A — API key + model env (fastest)**

```bash
export OPENAI_API_KEY=…
export FORGE_MODEL_ID=openai/gpt-4.1-mini
forge
```

**Option B — `/connect` inside the TUI**

```bash
forge
# then type a slash command:
#   /connect              → picker of all profiles
#   /connect openai       → paste API key
#   /connect anthropic    → paste API key
#   /connect opencode_go  → paste API key
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

Credentials are stored in `forge/credentials.toml` under your OS user config directory (e.g. `~/.config/forge/credentials.toml` on Linux, `~/Library/Application Support/forge/credentials.toml` on macOS) with mode `0600` on Unix. Never commit them.

Optional project file (not required):

```toml
# forge.toml
[model]
provider = "native"
model = "openai/gpt-4.1-mini"
```

Legacy provider names (`litellm`, `openai_compatible`, `anthropic`, `xai`, `grok`) are still accepted and migrate to `native`.

### 3. Start coding

```bash
cd /path/to/your/repo
forge
```

Type a task and press **Enter**, e.g. `Explain the layout of this crate`.

<p align="center">
  <img src="docs/ui/images/01-home.png" alt="Forge TUI" width="880" />
</p>

---

## TUI slash commands

The full-screen TUI keeps session identity, provider/model, context usage, and connection state in the top status bar. On wide terminals, the sidebar also shows the active session, registered tools, and recent activity. Tool results render as compact cards with expandable detail; validation failures explicitly report that no side effects were applied.

Context compaction and session resume report their before/after or recovery state in the transcript, feedback strip, notices, and activity feed. HITL decisions are made through the approval modal; they are not slash commands.

Slash commands can be typed directly in the textbox or discovered with **Ctrl+K**:

| Command | Description |
|---------|-------------|
| `/status` | Session, model, context, and connection info |
| `/model` | Pick a connected model, then its supported reasoning effort (`/model refresh` re-fetches remote catalogs) |
| `/model openai/gpt-4.1-mini` | Switch to a specific model |
| `/connect` | Open the provider connect picker |
| `/disconnect` | Disconnect current provider and clear stored credentials |
| `/resume` | Pick a previous session to resume |
| `/resume <uuid>` | Resume a specific session by id |
| `/file <path>` | Browse and read a workspace file read-only |
| `/cost` | Token usage breakdown for the session |
| `/compact` | Compact the conversation context |
| `/copy` | Copy last assistant message to clipboard |
| `/clear` | Clear the visible transcript (keeps model context) |
| `/sync` | Stage all changes, commit with a generated message, and push |
| `/quit` | Quit Forge |

**Keybindings**

| Key | Action |
|-----|--------|
| **Enter** | Send message (or enqueue while agent is busy) |
| **Ctrl+K** | Open the command palette |
| **/** + **Tab** | Complete slash command suggestions |
| **↑ / ↓** | Recall prior input lines (when not in the suggest list) |
| **Ctrl+Up / Ctrl+Down** | Select a queued message |
| **Ctrl+Backspace** | Cancel the selected queued message |
| **Page Up / Page Down** | Scroll the conversation |
| **Ctrl+C** | Quit Forge |

You can copy visible output with your terminal's normal mouse selection.

**Reasoning effort at startup**

```bash
FORGE_REASONING_EFFORT=high forge
```

**Resume after a crash or kill**

Start Forge again and use `/resume`. The event journal replays completed tool work so the agent picks up where it left off. Forge reports that replay completed and completed actions were not re-executed.

---

## Skills

Optional instruction packs the agent loads into its system prompt:

| Location | Scope |
|----------|-------|
| `<workspace>/.forge/skills/<name>/SKILL.md` | Project only |
| `~/.config/forge/skills/<name>/SKILL.md` | Global (all projects) |

Project skills override global skills with the same name. Drop a `SKILL.md` in either path and start a new session — no extra config required.

## Configuration

Optional — defaults + env are enough. See [Quick start](#quick-start).

| Env | |
|-----|--|
| `FORGE_MODEL_ID` | Provider/model id, e.g. `openai/gpt-4.1-mini` |
| `FORGE_MODEL_PROVIDER` | `native` (production) or `mock` (offline CI). Legacy aliases migrate to `native` |
| `FORGE_REASONING_EFFORT` | Startup reasoning effort (`auto\|minimal\|low\|medium\|high\|xhigh\|max`) |
| `FORGE_WORKSPACE` | Project root (default: cwd) |
| `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `XAI_API_KEY` / … | Provider credentials for native transports |

`forge.toml` also supports `[mcp.servers]` entries for external MCP tool servers, and `[tools]`/`[model]`/`[journal]`/`[tui]` sections.

---

## How it works

One agent core (`forge-core`) powers the TUI. Sessions append to a **SQLite event journal** so work can resume after a crash or restart. Tools run against your active checkout with schema-validated inputs.

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

Full design notes: [docs/architecture.md](./docs/architecture.md).

---

## Alpha safety and data

- Commit or stash existing work and use a disposable repository. Forge edits the active checkout directly.
- Repository-local sessions, context, and progress are stored under `.forge/`.
- Prompts and selected repository context are sent to your chosen model provider, whose privacy and retention policies apply.
- Forge sends no separate product telemetry. Model providers and explicitly configured observability exporters can still receive network requests.
- Reset, uninstall, testing, and feedback instructions are in the [alpha testing guide](docs/alpha-testing.md).

Want to help? See [CONTRIBUTING.md](./CONTRIBUTING.md) for setup, validation, and pull request guidance.

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
