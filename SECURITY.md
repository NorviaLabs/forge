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
shell.** The security boundary is the approval prompt, not the agent's judgement.

Two consequences worth being explicit about:

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
them can attempt to steer the agent. Approval prompts exist so that a consequential
action still needs you; read them rather than clicking through.

## Scope

In scope:

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

- Prefer a disposable clone for unfamiliar repositories.
- Read the command in the approval prompt before accepting it.
- Keep provider credentials in your user config or environment, not in a project
  `forge.toml`.
- Never commit `forge/credentials.toml` or `.forge/` session data.
