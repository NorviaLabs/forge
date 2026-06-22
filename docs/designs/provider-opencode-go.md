# OpenCode Go connect profile design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **6 only** (exclusive)  
**PRD:** PROV-02  
**Architecture:** §14 Phase 6, decision #19  
**Related:** [connect-command.md](./connect-command.md), [litellm-providers.md](./litellm-providers.md)

---

## 1. Problem / context

[OpenCode Go](https://opencode.ai/go) is a low-cost subscription that provides reliable access to popular **open coding models** for use with OpenCode or any agent. OpenCode’s own TUI uses **`/connect`** to attach Go (select OpenCode Go → sign in → paste API key → `/models`).

Forge Phase 6 offers the same **operator UX** for OpenCode Go as a first-class connect profile, routing inference through Phase 5 LiteLLM (OpenAI-compatible endpoint and/or LiteLLM model routing as documented by OpenCode Go / LiteLLM at implement time).

## 2. Goals & non-goals

**Goals**

- Connect profile id `opencode_go` titled **OpenCode Go**.  
- Guided signup URL + API key capture.  
- Recommended model list after connect.  
- Tool-using agent turns work once connected.

**Non-goals**

- Implementing OpenCode Go billing or auth server.  
- Supporting all OpenCode providers (GitHub Copilot, Zen, etc.)—only **Go** is required in Phase 6.  
- Forking the OpenCode agent product.  
- Requiring LiteLLM Proxy.

## 3. Design

### 3.1 Profile registration

| Field | Value |
|-------|--------|
| `id` | `opencode_go` |
| `title` | OpenCode Go |
| `api_key_env` | `["OPENCODE_API_KEY", "OPENCODE_GO_API_KEY"]` (first match wins; finalize names at implement against current OpenCode docs) |
| `auth_url` | OpenCode auth / zen URL as published (e.g. `https://opencode.ai/auth` or current docs target) |
| `default_base_url` | OpenCode Go OpenAI-compatible API base if required by LiteLLM custom provider |
| `litellm_provider_prefix` | `opencode` / custom as required by LiteLLM (document exact model string form in release notes) |
| `default_models` | Recommended Go catalog (§3.2) |

### 3.2 Models

OpenCode Go exposes a curated set of open coding models. Forge should:

1. Prefer fetching the remote catalog when an API exists.  
2. Else ship a **pinned recommended list** updated with the release (e.g. models marketed on [opencode.ai/docs/go](https://opencode.ai/docs/go/)).

After connect, `/model` and the TUI model picker show this list. Active model becomes a LiteLLM-routable string (exact encoding: `provider/model` or custom provider id—implementation binds to LiteLLM’s supported form).

### 3.3 Connect behavior (aligned with OpenCode)

```text
/connect
  → OpenCode Go
  → show auth_url (“Sign in, add billing if needed, copy API key”)
  → paste API key (masked)
  → optional verify completion
  → set default recommended model
  → “Connected OpenCode Go”
```

Then operator may run `/model` to switch among Go recommendations (same as OpenCode’s `/models` follow-up).

### 3.4 LiteLLM routing

**Requirement:** All Go traffic uses `LiteLlmModelClient` + worker.

Options (pick one at implement; document):

| Strategy | When |
|----------|------|
| A. LiteLLM built-in / OpenAI-compatible base_url | Go exposes OpenAI-compatible API |
| B. Custom provider entry in worker | Needs LiteLLM custom provider config |

Worker env receives the Go API key; optional `OPENAI_API_BASE` / LiteLLM `api_base` from profile `default_base_url`.

### 3.5 Config example (after connect)

```toml
[model]
provider = "litellm"
model = "<go-recommended-model-string>"

# base_url may be set under [model.litellm] extra or profile-applied runtime only
```

Do not store the API key in `forge.toml`.

## 4. Interfaces

- Profile registration in connect registry.  
- Optional: `forge connect opencode_go` CLI.  
- No second agent loop.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Invalid / expired Go key | Verify fails; clear guidance to re-run `/connect` |
| Subscription inactive | Upstream error message (redact key) |
| Model id not on Go plan | Model error; suggest `/model` list |
| Worker cannot reach Go base URL | Transport error |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This profile | **6** |
| `/connect` | [connect-command.md](./connect-command.md) |
| LiteLLM path | Phase 5 |

## 7. Acceptance

1. Registry includes `opencode_go`.  
2. Connect flow stores key and sets a non-empty model string.  
3. Docs link OpenCode Go public docs for signup.  
4. Smoke with fixture worker or recorded response; live key optional in CI.

## Related docs

- [connect-command.md](./connect-command.md)  
- [OpenCode Go](https://opencode.ai/go) / [docs](https://opencode.ai/docs/go/) (external)  
- [OpenCode providers](https://opencode.ai/docs/providers/) (external; `/connect` pattern)  
