# Security Policy

## Supported versions

Forge is pre-1.0 alpha software. Only the most recent `0.1.0-alpha.*` release
receives security fixes. There is no backporting to earlier alphas.

## Reporting a vulnerability

**Please do not open a public issue for a security report.** A public issue on a
public repository is a disclosure, and it happens before a fix exists.

Report privately through GitHub Security Advisories:

<https://github.com/NorviaLabs/forge/security/advisories/new>

If that is not available to you, email **security@norvialabs.com** instead.

Please include what you were trying to do, what happened instead, and enough
detail to reproduce it. A minimal reproduction is more useful than a long
description.

What to expect:

- Acknowledgement within **3 business days**.
- For a confirmed high-severity issue, a fix or documented mitigation within
  **30 days**.
- Please allow **90 days** before public disclosure, or until a fixed release is
  out, whichever comes first. If a report needs longer, we will say so and
  explain why.

Credit is offered to reporters who want it, and withheld if you prefer.

## Threat model

Forge executes model-proposed shell commands and filesystem changes on your
machine, using your provider credentials. **Treat it the way you would treat a
shell.**

**The security boundary is the OS sandbox, not the agent's judgement and not
the approval prompt.** Every shell command is confined at spawn — Seatbelt on
macOS, bubblewrap on Linux and WSL2 — to filesystem access inside your
workspace, the per-session temp directory, a read-only Rust toolchain runtime,
and only the command's resolved user-installed executables, with network
access only through a host-filtering egress proxy. Cargo state and XDG client
configuration are session-local; other home data remains outside the
boundary.
Forge does not attempt to classify a command as safe or dangerous before
running it; that judgement is the thing we are trying not to depend on.

The approval prompt is a second, weaker layer that sits on top. It does not
appear for shell commands at all, because the confinement is what makes
running them acceptable. MCP tools still ask; a `deny` pattern re-prompts.
On a host where the OS cannot confine, Forge does not start. It prints why
and exits, rather than running unconfined.

What the sandbox does **not** protect: anything a command can legitimately do
inside your workspace, and anything it can send to a host you have allowed.
A confined command can still corrupt your working tree, or exfiltrate
workspace files it can read through a permitted destination. Hosts start
denied; only a personal `host(...)` allow (or `host(*)`) opens one.

Three consequences worth being explicit about:

**Run Forge only in repositories you trust.** Forge reads instructions and
configuration out of the working directory, and that content influences its
behaviour:

| Path | Effect |
|------|--------|
| `AGENTS.md` | Loaded into the model's system prompt as project instructions |
| `.agents/skills/*/SKILL.md` | Loaded into the system prompt as skills |
| `.forge/agents/*.md` | Custom agents: names and descriptions in the system prompt; instructions in a started agent's prompt |
| `forge.toml` | Project configuration (see the restriction below) |

Keys in a project `forge.toml` that can execute code, redirect a credentialed
request, or move Forge's authority/data boundary — `[[mcp.servers]]`,
`model.base_url`, `model.api_key`, `workspace_root`, and `journal` — are **refused**
from an auto-discovered file and honoured only from your user config or a path
you name with `--config`. Everything else in that file is applied as written.

Custom agents can only narrow what the session may do. An agent's tool list is
enforced on top of the session's own policy, its capabilities cap the classes
of tool it gets, and a caller cannot raise them. An agent without `network`
gets no network gateway for its commands. Agent files are written by Forge,
into directories agent-run commands cannot write.

**Content the model reads is not trusted input.** File contents, tool output, web
results, and output from MCP servers all enter the model's context. Text in any of
them can attempt to steer the agent. The sandbox is what limits where a steered
agent can get to; where a prompt does appear, read it rather than clicking
through.

**MCP servers run outside the sandbox.** They are separate processes that Forge
starts but does not confine, so an MCP tool is as privileged as the server
implementing it. They keep their approval prompt in every mode for that reason.

**Remote control is a network service, off by default.** With it off, Forge
listens on nothing. Turned on in Settings, it serves an HTTPS API on
`127.0.0.1` or on one LAN address you choose (never every interface), so one
paired device can follow sessions, send a prompt, stop a turn, and allow or
deny a pending request:

- TLS 1.3 only, with a self-signed certificate kept in Forge's data folder
  and replaced whenever a device is revoked. The device pins its SHA-256
  fingerprint from the pairing link, so a look-alike server on the network
  cannot stand in.
- "Local network" accepts only a private address (RFC 1918, link-local,
  unique local, or the 100.64.0.0/10 range Tailscale uses), never one
  reachable from the internet.
- A device pairs with a one-time code shown on the desktop: eight random
  characters, held only in memory, valid for five minutes, destroyed after
  five wrong guesses. Redeeming it issues no token: the desktop asks
  "Allow <device> to pair?" and shows a six-digit number the device shows
  too, and a token exists only once you allow it — so a code seen by
  someone else cannot pair their device without you noticing. Pairing
  another device replaces the first.
- The device's bearer token is 256 random bits. Forge stores only its
  SHA-256 and compares in constant time. A missing, wrong or revoked token
  gets the same `401`; revoking ends the device's open connections at once.
- A device can allow a request once or deny it — and only when the request
  offers that without the desktop's second confirmation. Running outside the
  sandbox, opening a host, and remembering a grant stay on the desktop. The
  session checks the kind of request under its lock when the decision
  lands, so an answer meant for a review prompt can never approve the
  sandbox escalation that follows it for the same call.
- Slow or idle connections are closed (10 s for headers and for a body), one
  address holds at most 8 of the 16 connections, and a device gets 30
  prompts, decisions and stops a minute.
- Approvals are journaled with `actor: remote:<device>`; pairing, revoking,
  prompts (never their text), stops, and refused tokens and pairing codes go
  to `remote/activity.jsonl`.
- The remote folder is unreadable to sandboxed commands (a tmpfs over the home
  tree on Linux, the `~/Library` deny on macOS).

Anyone who holds the device holds its access: revoke a lost device in
Settings. On "Local network", anyone on that network can reach the port and
try the pairing code; keep pairing windows short.

**A session on an SSH host is not sandboxed there.** When a session runs on
a host from your SSH config, its commands and file edits happen on that host,
outside Forge's sandbox, with the host's network and whatever credentials its
account holds. The boundary is then the host itself and the approval prompt:

- "Ask before each command" is on by default. Every shell command, background
  job, interactive input and `git` call waits for approval; file tools do not.
  An approval can cover the rest of the session, never future sessions, and
  rules in your personal permissions file do not apply on the host.
- Nothing of this machine is forwarded. Forge forces `BatchMode=yes`, no agent
  or X11 forwarding, no port forwards, and starts `ssh` with an environment of
  only what it needs to find your config, keys and agent, so no provider key or
  token can be sent whatever `SendEnv` says.
- Every command and path is sent as a single-quoted POSIX shell word; the
  connection check refuses a host whose login shell would read it differently.
  File operations are served by a few long-lived helper processes on the host
  that run a fixed set of operations on arguments sent as length-prefixed
  data, never as shell text.
  File tools are confined to the remote folder and the session's private temp
  directory there (resolved on the host, symlinks included), and `.git` and
  `.forge` stay unwritable, as locally.
- The session's local workspace is an empty private directory: the project
  folder, Git, terminal and file panels on this machine are refused for it.
- An approval shows the command, not everything that command will run. File
  edits never ask, so the agent can write a script, a `Makefile` or a
  `package.json` and then ask to run a command that executes it. Read an
  approval with the session's recent edits in mind.
- "Trust all commands for this session" takes a second confirmation: it
  gives up the host's only boundary until the session ends.
- Forge's forced options apply to the connection to the host. A `ProxyJump`
  hop is its own `ssh` connection and uses your config for that hop, so a
  `ForwardAgent yes` you set for a bastion still applies there.
- Host keys are checked as your config says. With `StrictHostKeyChecking no`
  for a host, Forge would connect to whatever answers at its address; keep the
  default (or `yes`), and connect once in a terminal to accept a new key.
- Turning "Ask before each command" off gives the agent that host as it is.
  Do that only for a machine you can throw away.

## Scope

In scope:

- Escaping the sandbox: writing outside the workspace, or reaching a network
  destination that is not on the egress allow-list
- Entering Auto mode on a host with no working sandbox, or a sandbox that
  reports itself available while failing to confine
- Bypassing the approval prompt, or executing an action the user did not approve
- Escaping workspace path confinement
- Reaching remote control without a valid, unrevoked device token; pairing
  without the desktop's code; a remote device making a decision the desktop
  reserves for itself; remote control listening while turned off
- Disclosing credentials — API keys or OAuth tokens — through logs, errors, the
  desktop app, files on disk, or an attacker-chosen network destination
- Privilege escalation, or code execution triggered by repository content alone
- Prompt injection that leads to an unapproved tool call
- For a session on an SSH host: a command run there without approval while
  "Ask before each command" is on; a file tool reaching outside the remote
  folder and session temp; anything of this machine (environment, SSH agent,
  files) reaching the host; a host name or path from the page or the model
  being read by `ssh` or a shell as anything but a name
- Vulnerabilities in our dependency set that are reachable from Forge

Out of scope:

- That Forge runs shell commands you approved — that is the product
- Vulnerabilities in third-party model providers or MCP servers themselves, though
  we do want to hear about weaknesses in how Forge *handles* them
- Issues that require a local attacker who already has your filesystem privileges
- Advisories against dependencies that are present in `Cargo.lock` but not
  reachable in any built artifact. See `deny.toml`, which records the ones we have
  triaged and why.

## Hardening your own use

- Prefer a disposable clone for unfamiliar repositories. The sandbox confines a
  command's reach; it does not make an untrusted build safe to run.
- On Linux, keep `bubblewrap` and `socat` installed — without them Forge cannot
  confine anything and drops to asking about every command.
  bubblewrap also needs unprivileged user namespaces: Ubuntu 24.04 restricts
  them through AppArmor by default (`kernel.apparmor_restrict_unprivileged_userns`),
  and Forge then reports that the host refused to apply confinement.
- Read the command in the approval prompt before accepting it.
- For a session on an SSH host, keep "Ask before each command" on unless the
  host is disposable, and give its account only the access the work needs.
- Keep provider credentials in your user config or environment, not in a project
  `forge.toml`.
- Never commit `forge/credentials.toml` or `.forge/` session data.
