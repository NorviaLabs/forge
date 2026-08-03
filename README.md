# Forge

[![CI](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml/badge.svg)](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml)
[![Dependency audit](https://github.com/NorviaLabs/forge/actions/workflows/audit.yml/badge.svg)](https://github.com/NorviaLabs/forge/actions/workflows/audit.yml)
[![Latest release](https://img.shields.io/github/v/release/NorviaLabs/forge?include_prereleases&label=release)](https://github.com/NorviaLabs/forge/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust 1.97.1+](https://img.shields.io/badge/rust-1.97.1%2B-orange.svg)](https://www.rust-lang.org/)

Go from idea to verified code without leaving the terminal—Forge unifies an AI
agent, code editor, and shell in one focused workflow.

Forge is an open-source AI coding agent for your terminal. It runs a
full-screen TUI in the repository you are working on, helps inspect and change
files, runs commands with your approval, and keeps a durable session journal so
you can continue work after an interruption.

Forge is alpha software. Review every approval prompt and use it first in a
disposable or backed-up repository.

## What Forge does

- Chats with a configured model while staying inside your terminal.
- Reads and edits workspace files, applies focused patches, searches code, and
  works with Git.
- Runs shell commands only through an approval-aware tool flow.
- Preserves session history and unfinished work in a local SQLite journal.
- Supports provider sign-in and model selection from the TUI.
- Connects configured MCP servers and exposes their tools to the agent.
- Shows files, diffs, command output, activity, diagnostics, and task state in
  one keyboard-driven workspace.

## Install

### Install a prebuilt release

On macOS or Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://raw.githubusercontent.com/NorviaLabs/forge/main/install/forge-installer.sh | sh
```

On Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/NorviaLabs/forge/main/install/forge-installer.ps1 | iex
```

The installers select the current release for your platform and verify its
SHA-256 checksum before installing. To install a specific release, set
`FORGE_VERSION`, for example `v0.1.0-alpha.10`.

### Build from source

You need Git and Rust 1.97.1 or newer.

```sh
git clone https://github.com/NorviaLabs/forge.git
cd forge
cargo build --release --locked --package forge-cli
./target/release/forge --version
```

To run the development build:

```sh
cargo run --locked --package forge-cli
```

## Start Forge

Run Forge from the repository you want the agent to work in:

```sh
forge
```

The default workspace is the current directory. Forge also respects the
`FORGE_WORKSPACE` environment variable.

On first launch, connect a provider with `/connect`, choose a model, and start
typing a task. You can also configure a model before launching Forge with
environment variables or `forge.toml`.

## Providers and models

Forge includes native routes for:

- OpenAI API (`openai/*`)
- Anthropic API (`anthropic/*`)
- xAI Grok through OAuth (`xai/*`)
- OpenAI Codex subscriptions through device login (`openai-codex/*`)
- OpenCode Go (`opencode-go/*`)
- OpenCode Zen (`opencode-zen/*`)
- Ollama running locally (`ollama/*`)

The simplest setup is to export a provider key and model, then launch Forge:

```sh
export OPENAI_API_KEY="..."
export FORGE_MODEL_ID="openai/gpt-4.1-mini"
forge
```

Other API-key environment variables include `ANTHROPIC_API_KEY`,
`OPENCODE_API_KEY`, `OPENCODE_GO_API_KEY`, and `OPENCODE_ZEN_API_KEY`.

For OAuth providers, use `/connect` inside Forge. For Ollama, start the local
Ollama service first, then choose the Ollama route and a locally available
model.

Useful in-app commands:

```text
/connect       Connect or change a provider
/model         Browse or change models
/resume        Browse previous sessions in the current journal
/clear         Clear the visible transcript
/compact       Compact the active context
/disconnect    Remove a saved provider connection
/refresh       Refresh the file explorer's Git state
/edit          Open the active file in your editor
/context-file  Attach the active file to the next message
/theme         Change the presentation theme
/help          Open help
/quit          Exit Forge
```

`/resume <session-id>` resumes a specific session. The command-line form
`forge --resume <session-id>` does the same; bare `forge --resume` resumes the
most recently modified session when one exists.

## Keyboard controls

Forge displays contextual hints for the current focus. The most useful global
controls are:

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab` | Move between visible blocks |
| `Enter` / `i` | Interact with the focused block or control |
| `Esc` | Leave the current interaction level |
| `↑` / `↓` | Navigate a local list or input |
| `Ctrl+B` | Toggle the inspector |
| `Ctrl+P` | Open Quick Open for files |
| `Ctrl+Backtick` | Toggle the bottom panel |
| `Alt+1`–`Alt+4` | Open a bottom-panel tab |
| `Shift+←` / `Shift+→` | Switch the active block's tab |
| `?` | Open help |

## Configuration

Forge reads defaults, a user configuration file, a `forge.toml` in the
working directory, and environment variables. Environment variables override
file configuration.

A minimal project `forge.toml` can look like this:

```toml
[model]
model = "openai/gpt-4.1-mini"

[tui]
theme = "forge-midnight"
file_icons = "unicode"
mouse_capture = true

[journal]
path = ".forge/sessions"
```

Common environment variables are:

```text
FORGE_MODEL_ID
FORGE_MODEL_PROVIDER
FORGE_API_KEY
FORGE_WORKSPACE
FORGE_JOURNAL_PATH
FORGE_MODEL_REQUEST_TIMEOUT_SECS
```

Forge stores session journals in repository-local runtime storage when
possible, or in the platform application-data directory outside a Git
repository. Runtime data is not source code and should not be committed.

### MCP servers

MCP servers can be configured in a trusted user config or an explicitly
managed configuration:

```toml
[[mcp.servers]]
id = "my-tools"
transport = "stdio"
command = "my-mcp-server"
args = ["--stdio"]
```

Project-discovered configuration is intentionally restricted: settings that
could execute code or redirect credentialed requests are not accepted from an
untrusted checked-out repository.

## Built-in tools

Depending on configuration, the agent can use tools for:

- reading and writing files;
- applying validated workspace-confined patches;
- running approved shell commands;
- inspecting Git status and diffs;
- fast file and content search;
- web search;
- configured MCP tools.

Tool arguments are validated before execution. Sensitive or consequential
actions require an approval prompt. Forge does not treat model output as
trusted input.

## Sessions and resume

Every Forge session has a durable identifier and journal. If a process stops
unexpectedly, start Forge with:

```sh
forge --resume
```

or resume a known session directly:

```sh
forge --resume <session-id>
```

Inside the TUI, `/resume` lists previous sessions and shows a title hint from
the first user message when available. Session journals stay separate from
your source files and should never contain API keys or OAuth tokens.

## Safety

Forge can execute commands and modify files on your machine. Treat it like a
shell with an AI interface:

1. Run it only in repositories you trust.
2. Read each approval prompt, including the exact command and consequence.
3. Keep credentials in your environment or trusted user configuration.
4. Do not commit `.forge/`, runtime journals, API keys, OAuth tokens, or
   credential files.
5. Use a disposable clone when evaluating an unfamiliar repository.

See [SECURITY.md](SECURITY.md) for the threat model and private vulnerability
reporting process.

## Development

Forge is a Rust workspace. The main crates are:

- `forge-cli` — command-line entry point;
- `forge-tui` — terminal interface;
- `forge-core` — agent loop and session lifecycle;
- `forge-model` — provider transports and message normalization;
- `forge-connect` — provider profiles, credentials, and model catalog;
- `forge-tools` — built-in tools and validation;
- `forge-durable` — SQLite session journals.

Run the standard checks before submitting a change:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidance.

## License

Forge is available under the [MIT License](LICENSE).
