# OpenCode Go connect profile design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **6 only** (exclusive)  
**Revision:** **6.1** — TUI **always** prompts for API key  
**PRD:** PROV-02  
**Architecture:** Phase 6 / 6.1, decision #19  
**Related:** [connect-command.md](./connect-command.md), [connect-auth-modes.md](./connect-auth-modes.md), [model-providers.md](./model-providers.md)

---

## 1. Problem / context

[OpenCode Go](https://opencode.ai/go) is a low-cost subscription for popular open coding models. OpenCode’s TUI uses **`/connect`**: select OpenCode Go → sign in → **paste API key** → pick models.

Forge Phase 6 ships the same product profile. **Phase 6.1** requires that the **full-screen TUI explicitly asks for an API key** when connecting to OpenCode Go (masked modal), rather than silently using only env or completing connect without a key entry step.

## 2. Goals & non-goals

**Goals**

- Profile id `opencode_go`, title **OpenCode Go**.  
- **`auth_mode = api_key`** with **`tui_always_prompt = true`**.  
- TUI connect path **must** present a dedicated “Enter API key” step (masked).  
- Optional secondary action: “Use existing env key” if `OPENCODE_API_KEY` / `OPENCODE_GO_API_KEY` is set—still after showing the prompt UI.  
- Recommended models after connect; LiteLLM-only inference.

**Non-goals**

- OAuth for OpenCode Go (6.1).  
- OpenCode billing.  
- Every OpenCode provider beyond Go.

## 3. Design

### 3.1 Profile registration

| Field | Value |
|-------|--------|
| `id` | `opencode_go` |
| `title` | OpenCode Go |
| `auth_mode` | **`ApiKey { tui_always_prompt: true, … }`** |
| `api_key_env` | `["OPENCODE_API_KEY", "OPENCODE_GO_API_KEY"]` |
| `auth_url` | OpenCode auth URL (e.g. `https://opencode.ai/auth`) — shown **above** the key prompt |
| `default_base_url` | OpenAI-compatible Go base if required |
| `litellm_provider_prefix` | as required by LiteLLM |
| `default_models` | Recommended Go catalog (§3.2) |

### 3.2 Models

Prefer remote catalog when available; else pinned recommended list from [OpenCode Go docs](https://opencode.ai/docs/go/). After connect, `/model` and TUI model picker include these ids as LiteLLM-routable strings.

### 3.3 TUI connect behavior (normative — Phase 6.1)

```text
/connect → select "OpenCode Go"
  → Overlay "OpenCode Go"
       body:
         "1. Sign in and copy your API key:"
         auth_url (clickable / copyable)
         "2. Paste API key below:"
         [ masked TextInput ]   ← required focus
         [ Connect ]  [ Cancel ]
         optional: [ Use env OPENCODE_API_KEY ] if env present
  → On Connect with non-empty key:
       store key (credentials.toml 0600)
       set default model
       status: "Connected OpenCode Go · model … · key_source=file|provided"
  → On empty key: error "API key required" — remain on overlay
```

**Must not:** complete `opencode_go` connect from TUI without either a submitted key field or an explicit “Use env …” confirmation control.

### 3.4 REPL / CLI

| Surface | Behavior |
|---------|----------|
| REPL `/connect opencode_go` | Print auth_url; prompt `API key: ` (no echo if possible) |
| CLI `forge connect opencode_go` | Require `--key` / `--key-file` or interactive prompt on TTY |
| CLI with key | Store + set model; never print key |

### 3.5 LiteLLM routing

All traffic via `LiteLlmModelClient` + worker. Inject `OPENCODE_API_KEY` (or documented primary) into worker env; optional `api_base` from profile.

### 3.6 Config after connect

```toml
[model]
provider = "litellm"
model = "<go-recommended-model-string>"
```

API key not in `forge.toml`.

## 4. Interfaces

```rust
// TUI
fn open_opencode_go_key_modal() -> Overlay; // always key field

// ConnectService
fn connect_api_key(profile_id, key: &str) -> Result<ConnectOutcome, _>;
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Empty key submit | Stay on modal; “API key required” |
| Invalid key | Verify fails; keep modal or re-prompt |
| Env-only without confirmation in TUI | Not allowed as silent success |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This profile + TUI key prompt | **6** (rev **6.1**) |
| Auth mode framework | [connect-auth-modes.md](./connect-auth-modes.md) |
| LiteLLM path | Phase 5 |

## 7. Acceptance

1. Profile `opencode_go` has `tui_always_prompt = true`.  
2. TUI test / UX checklist: connect path shows API key field.  
3. Connect with fixture key sets model; output never contains key.  
4. Docs show auth_url + key step.

## Related docs

- [connect-command.md](./connect-command.md)  
- [connect-auth-modes.md](./connect-auth-modes.md)  
- [OpenCode Go](https://opencode.ai/go) / [docs](https://opencode.ai/docs/go/) (external)  
