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
macOS, bubblewrap on Linux and WSL2 — to writes inside your workspace and a
per-session temp directory, with network access only through a host-filtering
egress proxy. Forge does not attempt to classify a command as safe or
dangerous before running it; that judgement is the thing we are trying not to
depend on.

The approval prompt is a second, weaker layer that sits on top. It does not
appear for shell commands at all, because the confinement is what makes
running them acceptable. MCP tools still ask; a `deny` pattern re-prompts.
On a host where the OS cannot confine, Forge does not start. It prints why
and exits, rather than running unconfined.

What the sandbox does **not** protect: anything a command can legitimately do
inside your workspace, and anything it can send to a host you have allowed.
A confined command can still corrupt your working tree or exfiltrate what it
can read through a permitted destination. Hosts start denied; only a personal
`host(...)` allow (or `host(*)`) opens one.

Three consequences worth being explicit about:

**Run Forge only in repositories you trust.** Forge reads instructions and
configuration out of the working directory, and that content influences its
behaviour:

| Path | Effect |
|------|--------|
| `AGENTS.md` | Loaded into the model's system prompt as project instructions |
| `.agents/skills/*/SKILL.md` | Loaded into the system prompt as skills |
| `forge.toml` | Project configuration (see the restriction below) |

Keys in a project `forge.toml` that can execute code or redirect a credentialed
request — `[[mcp.servers]]`, `model.base_url`, `model.api_key` — are **refused**
from an auto-discovered file and honoured only from your user config or a path
you name with `--config`. Everything else in that file is applied as written.

**Content the model reads is not trusted input.** File contents, tool output, web
results, and output from MCP servers all enter the model's context. Text in any of
them can attempt to steer the agent. The sandbox is what limits where a steered
agent can get to; where a prompt does appear, read it rather than clicking
through.

**MCP servers run outside the sandbox.** They are separate processes that Forge
starts but does not confine, so an MCP tool is as privileged as the server
implementing it. They keep their approval prompt in every mode for that reason.

## Scope

In scope:

- Escaping the sandbox: writing outside the workspace, or reaching a network
  destination that is not on the egress allow-list
- Entering Auto mode on a host with no working sandbox, or a sandbox that
  reports itself available while failing to confine
- Bypassing the approval prompt, or executing an action the user did not approve
- Escaping workspace path confinement
- Disclosing credentials — API keys or OAuth tokens — through logs, errors, the
  TUI, files on disk, or an attacker-chosen network destination
- Privilege escalation, or code execution triggered by repository content alone
- Prompt injection that leads to an unapproved tool call
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
- Read the command in the approval prompt before accepting it.
- Keep provider credentials in your user config or environment, not in a project
  `forge.toml`.
- Never commit `forge/credentials.toml` or `.forge/` session data.
