# Forge

[![CI](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml/badge.svg)](https://github.com/NorviaLabs/forge/actions/workflows/ci.yml)
[![Dependency audit](https://github.com/NorviaLabs/forge/actions/workflows/audit.yml/badge.svg)](https://github.com/NorviaLabs/forge/actions/workflows/audit.yml)
[![Latest release](https://img.shields.io/github/v/release/NorviaLabs/forge?include_prereleases&label=release)](https://github.com/NorviaLabs/forge/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust 1.97.1+](https://img.shields.io/badge/rust-1.97.1%2B-orange.svg)](https://www.rust-lang.org/)

_Run, supervise, and resume AI coding work across your projects in one desktop app._

Forge is an open-source desktop coding environment for engineering with AI
agents. It combines agent conversations, a file explorer, editing, an
interactive shell, diffs, approvals, model selection, and multiple durable
sessions across several projects in one window.

Managed sessions keep their own durable state and isolated Git worktree, so one
session can keep working while you inspect, edit, or direct another. Agent shell
commands run under an OS sandbox, with network egress controlled by explicit
host rules.

Forge is alpha software. Review every approval prompt and use it first in a
disposable or backed-up repository.

## Why this exists

Most terminal coding agents optimize the prompt-to-patch loop. Real engineering
work is wider than that: you inspect code, edit some parts yourself, delegate
others, run commands, review diffs, verify behavior, switch tasks, and come back
later.

Forge keeps that whole loop in one workspace. The agent is part of the
environment rather than the entire environment: you can work directly in the
editor and shell, supervise long-running agent work, switch between independent
sessions, and resume durable state after an interruption.

## What Forge does

- Runs multiple independent coding sessions across several projects in one
  window; managed sessions use
  isolated linked Git worktrees and session-local state.
- Chats with configured models, reads and edits files, searches code, works
  with Git, and runs tools until the task completes or you interrupt it.
- Gives you a built-in file explorer, editor, interactive shell,
  diffs, diagnostics, activity, and task state.
- Runs agent shell commands inside an OS sandbox confined to the workspace,
  with network egress denied by default and opened through explicit host rules.
- Preserves transcripts, task state, queued follow-ups, and unfinished work in
  a local SQLite journal so sessions can survive process restarts.
- Supports provider sign-in, per-session model selection, reasoning effort, and
  model switching in the app.
- Connects configured MCP servers and exposes their tools to the agent.
- Supports child agents with durable state and isolated worktrees for delegated
  work.

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
`FORGE_VERSION`, for example `v0.1.0-beta.8`.

On Linux the installer also installs the sandbox's dependencies —
`bubblewrap` and `socat` — if they are not already present, prompting once
for `sudo`. It supports apt, dnf, yum, pacman, zypper, and apk. If that fails
it says so and continues: forge still runs, but without a sandbox it must ask
before every command. Set `FORGE_SKIP_DEPS=1` to manage them yourself.

macOS needs nothing extra; its sandbox is built into the OS.

Windows runs forge under WSL2, where it uses the Linux sandbox. There is no
native Windows build — the PowerShell installer detects WSL2 and points you
at the Linux installer inside it. The sandbox suite runs against WSL2 in CI.

### Build from source

You need Git and Rust 1.97.1 or newer.

You also need Node.js and pnpm (through Corepack) for the desktop UI, and on
Linux the WebKitGTK and GTK development packages
(`libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev` on Debian and Ubuntu).

```sh
git clone https://github.com/NorviaLabs/forge.git
cd forge
cargo build --release --locked
./target/release/forge-desktop
```

`cargo build` builds both binaries: `forge-desktop`, the app, and `forge`, the
command line that opens it. The desktop crate's build script builds the web UI
(`crates/forge-desktop/ui`) when its sources change. Set `FORGE_SKIP_UI_BUILD=1`
to use a UI you built yourself.

## Start Forge

Run Forge from the repository you want the agent to work in:

```sh
forge
```

The default workspace is the current directory. Forge also respects the
`FORGE_WORKSPACE` environment variable.

`forge` opens the desktop app; `forge PATH` opens it on another folder. A launch
from the Dock or Finder reopens the last project, or asks for a folder.

In desktop windows at or below 1120px wide, **Session details** opens the
inspector with background-task approvals, queued prompts, and Git controls.
Use **Close session details** or `Esc` to close it. In the desktop file editor,
`Tab` and `Shift+Tab` move focus without changing the file.

On first launch, Forge walks through directory trust and provider connect.
Trust is stored per directory (children inherit). Later folders only ask for
trust.

### GitHub issue workflow

**GitHub…** (Help, or the command palette) browses open issues, starts an
isolated issue session, and shows its pull request and checks. Starting or
completing an issue never pushes, creates a PR, or merges on its own; each of
those is an explicit action. GitHub reads require `gh` and `gh auth login`.

### Remote control

Remote control lets one paired device follow a session live, send it a
prompt, stop a turn, and allow or deny a pending request. It is off by
default, and while it is off nothing listens.

Turn it on in **Settings → Remote control** and choose where it is reachable
from: **This computer only** (`127.0.0.1`) or **Local network** (one address
of this computer; anyone on that network can reach the port, only a paired
device gets in). Then **Pair a device**: the desktop shows a one-time code
and a pairing link that also carries the certificate fingerprint the device
pins. The code works once and expires after five minutes. When the device
enters it, the desktop asks **Allow <device> to pair?** and shows a
six-digit number: allow only if the device shows the same number. Pairing another
device replaces the first; **Revoke** disconnects it at once and replaces the
certificate, so nothing pinned to the old one connects again. The footer
shows **Remote on** while it listens and names the device while it is
connected.

From a device, you can allow a request once or deny it, with or without a
note. Anything that would run outside the sandbox, open a host, or remember
a grant stays on the desktop. The HTTPS API a client implements is in
[`crates/forge-remote/PROTOCOL.md`](crates/forge-remote/PROTOCOL.md).

### Automation and benchmark runs

Forge also has a frontend-independent entry point for automation. Pipe one
prompt to `forge bench` to use the same session assembly, provider routes,
credentials, journal, sandbox, governance, and tool loop as the desktop app:

```sh
printf '%s\n' 'Inspect the repository and summarize it.' | \
  forge bench \
    --workspace /path/to/repository \
    --journal /tmp/forge-journal \
    --model openai_codex/gpt-5.6-luna \
    --effort max \
    --approve-all
```

`--model` is required and names the provider by its prefix
(`<provider>/<model>`). The command emits one JSON response on stdout and
keeps logs on stderr.
`--approve-all` is intended only for an isolated evaluation checkout; without
it, a tool request that needs human approval stops with an error.

### Which model does best

Every finished turn is recorded with the model, provider and effort that ran
it and the kind of task it was: `main` (a top-level session), `subagent` (a
built-in subagent, writer or read-only) or `custom:<name>` (a custom agent).
`forge models report` ranks the models each kind of task used in a
workspace:

```sh
forge models report --workspace /path/to/repository
forge models report --kind custom:code-reviewer --since 30d --json
```

For each kind, turns are grouped by model, provider and effort and compared on
completion rate, escalation rate, verification pass rate, approval rate, how
often you queued a message mid-turn, median tokens and median active time
(time spent waiting on you is not counted). The score is the mean of a group's
rank on those seven measures, lower is better; a measure a group has no data
for (no verification commands run, no approvals asked) counts as a middle
rank. Turns a restart interrupted are left out. Groups with fewer turns than
`--min-turns` (default 5) are listed with their numbers but not ranked.
`--since` takes a date (`2025-06-01`) or a number of days or hours (`7d`,
`24h`); `--journal` reads a journal directory instead of the workspace's own.
The report only reads the journals: it creates and changes nothing.

The workspace's own journals are read only once you have trusted the
workspace in Forge: Forge runs no sessions in an untrusted folder, so journals
found there (committed to a repository, say) are not yours and are refused.
`--journal` names a directory explicitly and needs no trust. Journals that are
symbolic links, that cannot be read, or that are committed to the repository
(so someone else's, not sessions run here) are skipped with a warning, and
names are printed with control characters escaped.

### How a subagent's model is chosen

When the agent starts a subagent without naming a tier, a model or an effort,
Forge picks the subagent's model and effort from this ranking, for that kind
of task (`subagent`, or the custom agent's own `custom:<name>`):

- only models on the parent's provider that your account can run are
  candidates, and they are ranked among themselves over the last 90 days;
- the leader is picked when there is enough evidence: at least 20 turns of
  that kind on those models, and at least two of them with 5 or more turns
  each. Otherwise the subagent runs on the parent's model, as it always did;
- about one spawn in ten keeps the parent's model anyway, so every model
  keeps being tried (recorded as `exploration`);
- the ranking is built in the background; starting a subagent never waits
  for it.

Every such subagent records why it runs on its model: the agent sees it in
the `spawn_agent` result (`model_choice`), the subagent's journal keeps the
full decision with the evidence behind it, and each of its turns carries a
`model_chosen` signal. A workspace you have not trusted never steers the
choice (its journals are not read; trust is checked again each time the
ranking is rebuilt), nor do journals committed to the repository, and only
known effort names are ever picked. To always use the parent's model, set it
in your user config:

```toml
[subagents]
model_selection = "inherit"   # default: "ranked"
```

### Auto: Forge picks the model for each turn

In the model picker, **Auto** lets Forge choose the model and reasoning
effort of each turn of the session; the model and effort selected when you
turn it on are its fallback. Each turn starts on the ranked best for
main-session work among the fallback provider's models your account can run
(the same ranking and evidence rules as for subagents), or on the fallback
when the evidence is thin.

When work goes badly Forge climbs a **step-up ladder**, effort before model:
after a failed turn, the next turn starts one step higher — first a higher
reasoning effort on the same model; only once that model is at its highest
effort (or has no effort setting) the provider's strong-tier model, then its
higher efforts. A failed turn is never re-run by itself. After two successful
turns in a row on a raised step, Forge steps back down one. Provider errors,
denied approvals, cancellations and turns on a tier you asked for don't move
the ladder. Mind the cost: the ladder's top is the strong-tier model at its
highest effort, whatever your fallback costs, and work that keeps failing
(for example tests that cannot pass) keeps Auto there until two turns
succeed. Each turn's label shows where it is; picking a model ends it.

You stay in charge: picking a model turns Auto off, and asking for a tier in
a message ("use your strongest model") still wins for that turn. Each Auto
turn shows the model and effort it ran on and why (ranked best, fallback,
stepped up after a failed turn, stepped down after two good turns), and the
turn's record keeps it as an `auto_model` signal. Auto, its fallback and
where it stands on the ladder are saved with the session and survive a
restart.

## Providers and models

Forge includes native routes for:

- OpenAI API (`openai/*`)
- Anthropic API (`anthropic/*`)
- xAI Grok through OAuth (`xai/*`)
- OpenAI Codex subscriptions through device login (`openai_codex/*`)
- OpenCode Go (`opencode_go/*`)
- OpenCode Zen (`opencode_zen/*`)
- Ollama running locally (`ollama/*`)

A model id's prefix is its provider id. Ids saved with an older prefix
(`openai-codex/`, `opencode-go/`, `opencode-zen/`) are converted when Forge
reads them; typing one gives an error naming the current prefix.

The simplest setup is to export a provider key, launch Forge, and pick a
model with `/model`:

```sh
export OPENAI_API_KEY="..."
forge
```

Forge remembers the last model you chose and starts new sessions on it;
resumed sessions keep their own model.

Each key provider reads `FORGE_<ID>_API_KEY` first (for example
`FORGE_OPENAI_API_KEY` or `FORGE_OPENCODE_GO_API_KEY`), then its usual
variables such as `ANTHROPIC_API_KEY`, `OPENCODE_API_KEY`,
`OPENCODE_GO_API_KEY`, and `OPENCODE_ZEN_API_KEY`. `FORGE_<ID>_BASE_URL`
overrides the endpoint. An environment key wins over a key saved with
`/connect`, and `/disconnect` cannot remove it: unset the variable instead.
For xAI, an `XAI_API_KEY` in the environment is used instead of the login.

For OAuth providers, use `/connect` inside Forge. For Ollama, start the local
Ollama service first, then choose the Ollama route and a locally available
model.

OpenCode Go serves its models on more than one protocol. Forge routes each
model to the endpoint OpenCode publishes for it — Chat Completions, Responses,
or the Anthropic Messages API — so models such as `opencode_go/union-alpha`
work without extra configuration. Union Alpha on OpenCode Zen
(`opencode_zen/union-alpha`) also uses the Messages API.

Custom OpenAI- or Anthropic-compatible providers are added with
`/connect` → **+ Add custom provider**, which saves them to
`forge/providers.toml` in the platform config directory. Keys stay in the
separate `credentials.toml`. Provider entries saved by this version use the
current field names, which older Forge builds cannot read.

On Chat Completions routes (including OpenCode Go's DeepSeek models), a
provider output limit or a stream that ends without a completion signal is
reported as an incomplete-turn error rather than a successful answer. Use
Continue to resume the task; tools from the incomplete response are not executed.

Useful in-app commands (type `/` in the composer):

```text
/connect       Connect a provider or add a model
/disconnect    Disconnect a provider
/model         Switch model for future turns
/effort        Set the model's reasoning effort
/new           Start a new session
/sessions      Switch to another session
/resume        Switch to a previous session
/fork          Fork this conversation into a new session
/continue      Continue the last stopped or failed turn
/compact       Continue in a fresh context
/clear         Clear the transcript view
/context-file  Attach the open file to the next prompt
/terminal      Open the terminal panel
/theme         Switch the desktop theme
/status        Show session status and token usage
/help          Show help and keyboard shortcuts
```

## Keyboard controls

| Key | Action |
| --- | --- |
| ⌘ / Ctrl K | Command palette: files, sessions and actions |
| Enter / ⌘ ↵ | Send prompt (Shift+Enter for a new line) |
| ↑ / ↓ | Cycle this session's prompt history at the first/last line |
| Esc | Close the open overlay; leave Files or Terminal when no editor, field or terminal has focus; restore your draft when browsing prompt history |
| ⌘ / Ctrl E | Open Files, or return to the session |
| / then Tab | Complete a command or installed skill |
| ⌘ / Ctrl S | Save current file |
| ⌘ / Ctrl W | Close the current document in Files |
| ⌘ M | Choose model and reasoning effort |
| ⌘ J / Ctrl ` | Show or hide the terminal drawer; leave the Terminal screen |
| ⌘ / Ctrl F | Find in current editor |
| ⌘ / Ctrl G | Next match (Shift for previous) |
| ⌘ / Ctrl L | Jump to line in current editor |
| ⌘ / Ctrl V | Paste image into composer (image-capable models) |

## Configuration

Forge reads defaults, a user configuration file, a `forge.toml` in the
working directory, and environment variables. Environment variables override
file configuration.

A minimal project `forge.toml` can look like this:

```toml
[journal]
path = ".forge/sessions"

[tools.agents]
max_depth = 2
min_wait_ms = 100
default_wait_ms = 10000
max_wait_ms = 120000
```

`forge.toml` does not choose the provider or model: `[model] provider`,
`model`, `base_url`, and `api_key` are ignored with a startup notice. Use
`/connect` and `/model` instead. `[model] request_timeout_secs` still applies.

Agent orchestration is enabled by default. The model can use `spawn_agent`,
`send_message`, and `wait_agent`. Each child runs in an isolated Git worktree and inherits the
parent model, governance, and workspace policy. `spawn_agent` can narrow a
child's tools with `tool_allowlist`.

`max_depth` bounds nested work. `send_message` queues a
mailbox message without waking its target; with `wake: true` it starts an idle
or completed descendant's next turn, and with `interrupt: true` (no message) it
stops the descendant's current turn. `wait_agent` returns every descendant's
status; pass `since_revision: 0` to list them without waiting. Agent IDs are opaque session identifiers, and operations
are limited to descendants of the calling agent. Child journals, statuses,
summaries, and worktree locations are durable. A child a restart interrupts
is reopened from its own journal — a writer in its worktree, a read-only child
in the repository it shares — recorded as interrupted, and kept for a
follow-up (it is not re-run); completed children, writers and read-only alike,
remain available for a later follow-up without reopening their historical
task. A read-only child never owns a checkout: removing its parent never
removes the repository it ran in.

### Themes

Pick a theme in Help (or `/theme`). Forge ships `forge-dark` (default) and
`forge-light`, and `System` follows the OS. Drop custom `.toml` theme files into
`.forge/themes/` in your workspace or `~/.config/forge/themes/` to add or
override themes. See `crates/forge-desktop/themes/` for the schema and examples.

Common environment variables are:

```text
FORGE_WORKSPACE
FORGE_JOURNAL_PATH
FORGE_DATA_DIR
FORGE_MODEL_REQUEST_TIMEOUT_SECS
```

Forge stores session journals in repository-local runtime storage when
possible, or in the platform application-data directory outside a Git
repository. `FORGE_DATA_DIR` replaces that directory; `cargo test` and
`cargo run` in this repository set it to `target/forge-data`. Runtime data is not source code and should not be committed.

Forge does not use a `.forge/local` that holds anything Git tracks: files
committed there are not sessions this machine ran (a repository could
otherwise hand you a session list and histories it made up). It keeps that
repository's runtime state in the application-data directory instead, says
so when the project opens (and in `forge models report`), and changes
nothing in `.forge/local`. To use it again, run
`git rm -r --cached .forge/local`, commit, and restart Forge. The same
applies when Git cannot tell what it tracks there, and when `.forge` or
`.forge/local` is a symbolic link (Forge does not follow one for its runtime
state). Folders that are not Git repositories keep their `.forge/local` as
it is.

### MCP servers

MCP servers can be configured in a trusted user config or an explicitly
managed configuration:

```toml
[[mcp.servers]]
id = "my-tools"
transport = "stdio"
command = "my-mcp-server"
args = ["--stdio"]
env_vars = ["MY_TOOLS_TOKEN"]   # passed through from Forge's environment

[mcp.servers.env]
LOG_LEVEL = "info"
```

Every server runs in the same sandbox as shell commands: it reads the disk
minus the secrets deny-list, writes only its own temp directory (and the
workspace with `sandbox_workspace = "read_write"`), and reaches the network
only through Forge's gateway, which asks you about a host it does not know.
`sandbox_read_paths` adds directories it may read. `sandbox = false` runs a
server as an ordinary process.

A server never inherits Forge's environment. It gets `PATH`, `HOME`, the user,
locale and terminal variables, plus what its `env` sets and the names its
`env_vars` lists, so an API key reaches only the server you give it to.

Project-discovered configuration is intentionally restricted: settings that
could execute code or redirect credentialed requests are not accepted from an
untrusted checked-out repository.

### The sandbox

Every shell command the agent runs, every git Forge runs for you, and every
MCP server is confined by macOS Seatbelt (`sandbox-exec`). Confinement is
applied at spawn, so it does not depend on classifying a command correctly
beforehand: forge does not decide whether `rm -rf` is dangerous, it removes
the reach that would make it matter. If the OS cannot confine, forge does not
start.

Inside the sandbox a command can:

- **read** the disk except a deny-list of secrets (`~/.ssh`, `~/.aws`,
  `~/.gnupg`, `~/.config/gh`, `~/.netrc`, keychains, browser profiles, other
  projects' `.forge/`, …) and personal folders (Documents, Desktop,
  Downloads, Mail, Messages, iCloud Drive, …). A workspace inside one of them
  is still readable;
- **write** the workspace, a per-session temp directory and a shared package
  cache. `.forge` stays read-only. `.git` is writable, hooks and config
  included: everything that later runs them is confined too, and a call that
  changes a hook or the config says so in its output;
- **reach the network** only through Forge's gateway (below);
- **use your git identity without holding it**: credentials stay outside. The
  gateway adds them to requests for code hosts, SSH runs through a broker in
  the Forge process, and commits and tags are signed by a broker with your
  own git signing setup.

  On Linux the brokers and the per-command gateway are not yet reachable
  from inside the sandbox, so confined git over SSH, `git://` or the
  gateway, and commit signing, do not work there yet.

When a command is refused something outside that boundary, the card offers,
most narrow first:

- **Allow writing `<folder>`** for this session, or always in this project.
  The same command runs again, still sandboxed, with only that folder added.
  Your home folder, folders on your `PATH`, system folders, launch agents,
  credential folders and Forge's own config are never offered;
- **Approve command**: this exact command, once, outside the sandbox;
- **Always run `<program>` outside the sandbox in this project**, offered only
  for a single simple command such as `docker build .`. It is a full escape
  for that program; chained commands, paths and `$`-expansion are never
  covered.

Approve-all never asks and never widens the sandbox: a command that needs
more fails, and the agent is told why.

### Network egress

The gateway lets through code hosts (github.com, gitlab.com, bitbucket.org)
and package registries without asking. Any other host is held while you
decide: allow it once, for the session, or always for the project, or refuse
it. The command that opened the connection carries on when you allow it;
nothing is run twice.

For code hosts the gateway terminates TLS with a per-session certificate
authority that only confined processes trust, so it can tell a fetch from a
push. A push, or any other change made with your identity, to a repository
other than the project's own remotes is asked about first. A token the
command brought itself is refused.

Everything allowed "always" is listed in **Settings → Sandbox permissions**,
per project, where each entry can be removed. Removing a host or push target
also ends it in open sessions.

### Permission rules

`permissions.toml` can re-prompt a call (`deny`) or skip a prompt that would
otherwise appear (`allow`). Pattern rules match the actual call — a command
prefix for shell tools, a path glob for file tools, a host for fetch-style
tools. `host(...)` rules feed the gateway instead: a host they allow is never
asked about. Shell does not prompt by default, so `allow` rules for shell do
nothing to prompting. MCP still prompts unless allowed:

```toml
allow = [
  "bash(cargo test *)",
  "host(api.example.com)",
]
deny = ["bash(cargo publish*)"]
```

`host(*)` is unrestricted network through the gateway (the filesystem sandbox
stays on). A `host(...)` deny wins over a broader allow, the built-in list
included.

A `deny` entry carves an exception out of a broader `allow` entry (`cargo *`
allowed, but `cargo publish` still asks); it never blocks a call outright —
that's still the ACL's job. Two files are read and merged:

- `<user config dir>/forge/permissions.toml` — personal, trusted.
- `.forge/permissions.toml` in the workspace — repo-committed, and for the
  same reason project-discovered MCP config is restricted above, its `allow`
  entries are ignored. A checked-out repository cannot grant itself a wider
  blast radius than a human approved locally. Its `deny` entries are always
  honored, since narrowing approval further is safe regardless of source.

### Approvals

There is no permission-mode setting. Shell commands and file writes run
without a prompt: the sandbox is the boundary. MCP tools prompt by default,
because a server's tools act through whatever the server was given. A `deny`
pattern re-prompts even for shell. An ACL deny is a hard block, not a prompt.

### Approving shell commands

When a command needs approval, use the inline menu:

- **↑ / ↓** — move selection (default: Run once)
- **Enter** — confirm
- **Esc** — don’t run (or back out of “Deny with note”)

Optional text aliases still work: `yes`, `no`, `remember`, `always`,
`no <note>`.

## Built-in tools

Depending on configuration, the agent can use tools for:

- reading and writing files;
- applying validated workspace-confined patches;
- running shell commands inside the sandbox;
- inspecting Git status and diffs;
- fast file and content search;
- coordinating isolated child agents and reviewing their status;
- starting [custom agents](#custom-agents) by name;
- configured MCP tools.

Tool arguments are validated before execution, and shell commands are confined
by the OS sandbox rather than judged for danger beforehand. Forge does not
treat model output as trusted input.

## Custom agents

A custom agent is a specialist the model can start by name: a description of
when to use it, the capabilities and tools it gets, and instructions. Create,
edit and delete them in **Settings → Custom agents → Manage** (or `Custom agents` in
the command palette). Forge writes the files; you never need to edit them by
hand.

Each agent is one markdown file, `<name>.md`, in one of two places:

- **Project** — `<project>/.forge/agents/`, for the project on screen. A Git
  project's agents live in its main checkout, so every session (each in its
  own worktree) sees them. Forge writes them only in a trusted project. Whether
  they are committed and shared depends on your `.gitignore`.
- **User** — `<user config dir>/forge/agents/` (macOS: `~/Library/Application
  Support/forge/agents`), for all your projects. `FORGE_USER_AGENTS_DIR`
  points it elsewhere.

A project agent overrides a user agent with the same name; Settings shows both,
marked. Neither directory is writable by agent-run commands: the sandbox keeps
`.forge` read-only and never exposes Forge's own config directory for writing.

```markdown
---
name: code-reviewer
description: Reviews the current diff for correctness, risk and missing tests.
mode:
- read-only
tools:
- read_file
- grep
---

You are a careful code reviewer. Report findings as a list.
```

- `name` — 1–64 lowercase letters, digits and single hyphens; the file name.
- `description` — what the model reads to decide when to use the agent.
- `mode` — a list of capabilities. Reading is always allowed:

  | Capability | Adds | Runs |
  |---|---|---|
  | `read-only` | nothing; must be the only value | alongside other agents, in the session's workspace |
  | `write` | `edit`, `write_file`, `apply_patch`, `git` | in its own worktree, one writer at a time |
  | `exec` | `bash`, `exec_command`, `write_stdin` | in its own worktree; its commands have **no network** unless `network` is listed |
  | `network` | `web_fetch` and network MCP tools; with `exec`, the session's network gateway for its commands | |

  `exec` without `write` removes the file-editing tools, but the agent's shell
  commands can still change files in its own worktree.
- `tools` — an explicit allow-list. The agent gets only listed tools its
  capabilities allow, and never more than the session itself may use. It can
  never start further agents or background jobs.
- The body — instructions added to the agent's system prompt when it starts.

The model sees every agent's name, capabilities and description (never the
instructions) and starts one with `spawn_agent` and `agent: "<name>"`. Ask for
a model or reasoning effort in your prompt ("use the code-reviewer agent on
anthropic/… at high effort") and the model passes `model` and
`reasoning_effort`; Forge checks both against your providers and the model's
supported levels. The agent's card in **Activity** shows the agent, model and
effort it actually runs on. An agent's file is read again each time it
starts, and open sessions pick up new, edited and deleted agents from their
next message (at once when you save in Settings); the list in the system
prompt is rewritten only when it changed.

A file that cannot be loaded never breaks a session: it is skipped, the
session shows a notice saying which file and why, and Settings lists it under
**Couldn't load**, where you can fix its text or delete it. A listed tool that
does not exist or that the capabilities do not allow is dropped with a
warning. A child agent's instructions, tool limit, network and model are saved
with it, so it keeps them after a restart.

## Sessions and resume

Forge supports multiple durable managed sessions per project, and several
projects in one window. Each session has its own identifier, transcript, queued
follow-ups, model selection, runtime state, and isolated linked Git worktree. A
session keeps streaming or running tools in the background while you switch to
another session or project.

The sidebar lists every project at the top, with what each is doing and any
request waiting on you, then the sessions of the project on screen. Start a
session with **New session** or `/new`; switch with the sidebar or `⌘K`.

Forge reopens the last selected session when it starts. Closed sessions stay in
the sidebar and can be resumed; `/fork` starts a new session seeded with the
current conversation. Session journals stay separate from your source files and
should never contain API keys or OAuth tokens.

### Sessions on another machine (SSH)

A session can run on a host from your SSH config instead of this machine: in
**New session**, pick the host under **Run on** and give the folder on it (an
absolute path). Every shell command, file tool, search and `git` call then runs
there; this machine only relays. Forge uses your own `ssh` client and
`~/.ssh/config` (`ProxyJump`, identities, agent and `known_hosts` all apply),
over one connection per session.

- **Forge checks the host before the session exists.** The host must be
  reachable without prompting (keys or agent; connect once in a terminal to
  accept its host key), Linux or macOS with `bash`, `realpath` and `mktemp`,
  and the folder must exist. Whatever is wrong is the dialog's error.
- **Commands run outside Forge's sandbox**, with the host's network and
  credentials. **Ask before each command** is on by default: every command and
  `git` call waits for your approval, which can be widened for the session
  (**Allow this kind of call**, **Trust all commands**) but is never saved as
  an "always" rule. Rules in your personal permissions file are about this
  machine and do not cover the host. Turn asking off only for a machine you
  can throw away. File edits never ask.
- **Nothing of this machine goes to the host**: no agent or X11 forwarding,
  no port forwards, and none of Forge's environment (provider keys included).
  A `ProxyJump` hop follows your own config for that hop.
- An approval shows the command, not what it runs: file edits never ask, so
  a command can run a script the agent just wrote. **Trust all commands**
  asks for a second confirmation.
- **Files, Changes, Git, GitHub and the terminal show this machine**, so they
  are off for an SSH session; the header's **Runs on** chip says where it runs.
- A lost connection fails the running command with
  "Lost the connection to *host* while running *command*"; the command may
  still be running there, and Forge reconnects on the next one. Stopping a turn
  kills the command on the host too.
- Read-only agents work alongside an SSH session; a writing agent, which needs
  a checkout of its own, does not.
- Resuming reconnects to the same host and folder. Closing the session
  removes its temp directory on the host.

## Safety

Forge can execute commands and modify files on your machine. Treat it like a
shell with an AI interface:

1. Run it only in repositories you trust. The sandbox confines what a command
   can reach; it does not make an untrusted repository safe to build.
2. Read each approval prompt, including the exact command and consequence.
   In Auto mode most shell commands do not prompt — the sandbox is what
   stands in for that review, so check that it is active (the footer's mode
   chip says when the mode is capped).
3. Keep credentials in your environment or trusted user configuration. The
   sandbox restricts where a command may connect, but a command that legitimately
   reaches an allow-listed host can carry whatever it can read.
4. Do not commit `.forge/`, runtime journals, API keys, OAuth tokens, or
   credential files.
5. Use a disposable clone when evaluating an unfamiliar repository.
6. A session on an SSH host is not sandboxed there. Keep **Ask before each
   command** on unless the host is disposable.

See [SECURITY.md](SECURITY.md) for the threat model and private vulnerability
reporting process.

## Development

Forge is a Rust workspace. The main crates are:

- `forge-desktop` — the desktop app (Tauri);
- `forge-cli` — the `forge` command, `forge bench` and `forge models report`;
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
