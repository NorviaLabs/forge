# Connect authentication modes design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **6 only** (exclusive)  
**Revision:** **6.1**  
**PRD:** CONN-01 (supporting — auth modes), PROV-01 / PROV-02 (consumers)  
**Architecture:** Phase 6.1, decision #19  
**Related:** [connect-command.md](./connect-command.md), [provider-xai-grok.md](./provider-xai-grok.md), [provider-opencode-go.md](./provider-opencode-go.md)

---

## 1. Problem / context

Phase 6 `/connect` originally assumed a single “paste API key” path. **Phase 6.1** requires:

1. **xAI Grok** → **OAuth** (no API-key primary UX).  
2. **OpenCode Go** → **API key**, and the **TUI must explicitly prompt** for the key when connecting.

Profiles therefore declare an **auth mode**; the shared connect UI branches on that mode.

## 2. Goals & non-goals

**Goals**

- Normative `AuthMode` on `ConnectProfile`.  
- TUI/REPL/CLI dispatch: OAuth flow vs API-key flow.  
- Secrets still never rendered in chat, journal, or default OTEL.  
- Still one production model path (Phase 5 LiteLLM).

**Non-goals**

- Implementing every OAuth provider on earth.  
- OAuth for OpenCode Go in 6.1 (API key only).  
- API-key primary UX for xAI Grok in 6.1.

## 3. Design

### 3.1 AuthMode

```rust
enum AuthMode {
    /// Browser and/or device-code OAuth; store access/refresh tokens.
    Oauth {
        device_code: bool,
        system_browser: bool,
        auth_server: String,   // e.g. https://accounts.x.ai
    },
    /// Operator pastes or supplies API key; TUI must prompt explicitly.
    ApiKey {
        /// If true, TUI always opens masked key input when connecting
        /// (even if env already has a key — offer "use existing" + "enter new key").
        tui_always_prompt: bool,
        env_names: Vec<String>,
    },
}
```

### 3.2 Profile mapping (Phase 6.1)

| Profile | AuthMode |
|---------|----------|
| `xai` (xAI Grok) | `Oauth { device_code: true, system_browser: true, … }` |
| `opencode_go` | `ApiKey { tui_always_prompt: true, env_names: ["OPENCODE_API_KEY", "OPENCODE_GO_API_KEY"] }` |

### 3.3 TUI flow branching

```text
/connect → list profiles → select profile P
  match P.auth_mode:
    Oauth:
      show OAuth overlay (device code + "Open browser" / waiting)
      NO api-key TextInput
      on success → store tokens → set model
    ApiKey if tui_always_prompt:
      show modal: "Enter OpenCode Go API key"
      masked input required (or explicit "Use env OPENCODE_API_KEY" secondary action)
      on submit → store key → set model
```

### 3.4 CLI / REPL

| Profile | CLI |
|---------|-----|
| `xai` | `forge connect xai` → OAuth; `--key` / `--key-file` **error** |
| `opencode_go` | `forge connect opencode_go` without key → prompt on TTY; or `--key` / `--key-file` |

### 3.5 Credential store shapes

```toml
# credentials.toml (0600) — illustrative

[keys]
# ApiKey profiles only
opencode_go = "sk-…"

[oauth.xai]
access_token = "…"
refresh_token = "…"
expires_at = "…"
```

Status surfaces report `key_source=oauth|env|file` never token/key values.

### 3.6 LiteLLM worker injection

| Auth | Worker receives |
|------|-----------------|
| OAuth (xai) | Token material as required by LiteLLM/xAI adapter (env), refreshed if needed |
| ApiKey (opencode_go) | Primary env name = key value |

## 4. Interfaces

- Extend `ConnectProfile` with `auth_mode`.  
- `handle_connect_action` / TUI overlay select path by mode.  
- Unit tests: xai profile is Oauth; opencode_go is ApiKey with `tui_always_prompt`.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| OAuth cancel/timeout | Disconnected; no partial active profile |
| ApiKey empty submit | Stay on prompt; error “API key required” |
| Wrong mode CLI flags | Clear usage error |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This document | **6** (rev **6.1**) |
| `/connect` command surface | [connect-command.md](./connect-command.md) |
| Profile details | PROV-01 / PROV-02 designs |

## Related docs

- [connect-command.md](./connect-command.md)  
- [provider-xai-grok.md](./provider-xai-grok.md)  
- [provider-opencode-go.md](./provider-opencode-go.md)  
