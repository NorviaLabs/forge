# `/connect` command design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **6 only** (exclusive)  
**Revision:** **6.1** — OAuth vs API-key auth modes  
**PRD:** CONN-01 (primary)  
**Architecture:** Phase 6 / 6.1, decision #19  
**Related:** [connect-auth-modes.md](./connect-auth-modes.md), [provider-xai-grok.md](./provider-xai-grok.md), [provider-opencode-go.md](./provider-opencode-go.md), [model-providers.md](./model-providers.md), [tui-commands.md](./tui-commands.md), [tui-overlays.md](./tui-overlays.md)

---

## 1. Problem / context

Phase 5 unifies all live inference on the LiteLLM SDK worker. Operators still need a **guided way** to attach credentials and pick a productized backend—especially **xAI Grok** and **OpenCode Go**—without hand-editing TOML or memorizing model strings.

OpenCode popularized **`/connect`**. Forge adopts that pattern with **Phase 6.1** auth modes: **xAI Grok = OAuth**; **OpenCode Go = API key with mandatory TUI prompt**.

## 2. Goals & non-goals

**Goals**

- Slash command **`/connect`** in full-screen TUI and line-mode REPL.  
- Extensible **connect profile** registry (Grok + OpenCode Go).  
- Store API keys **and** OAuth tokens securely (`credentials.toml` 0600 / env / vault).  
- Activate a LiteLLM model string + worker env for the session.  
- Never print secrets in chat, journal model-visible fields, sidebar, or default OTEL.  
- Branch UX by **`auth_mode`** ([connect-auth-modes.md](./connect-auth-modes.md)).

**Non-goals**

- A second production HTTP client (Phase 5 LiteLLM path remains sole).  
- Full multi-tenant OAuth *authorization server* inside Forge (Forge is OAuth *client* only).  
- Billing or account management for OpenCode Go / xAI.  
- Replacing `/model`.  
- API-key paste as primary path for **xAI Grok**.

## 3. Design

### 3.1 Command surface

| Form | Behavior |
|------|----------|
| `/connect` | Open interactive flow (palette/list of profiles) |
| `/connect list` | List registered profiles + connected? + active? |
| `/connect <profile_id>` | Jump to that profile (e.g. `/connect xai` or `/connect opencode_go`) |
| `/connect status` | Show active profile id, model string, key source (env \| file \| vault)—**never** key material |
| `/connect disconnect [profile_id]` | Clear stored key for profile; keep env-only keys untouched |

Optional CLI: `forge connect <profile>` — OAuth profiles start login; API-key profiles accept `--key` / `--key-file` or TTY prompt. **No secrets in logs.**

### 3.2 Interactive flow (normative)

```text
1. List connect profiles (id, title, auth mode badge, connected badge)
2. Operator selects one
3. Branch on auth_mode (Phase 6.1):
   A. Oauth (xAI Grok):
      - Show auth instructions + start browser and/or device-code flow
      - Do NOT show API-key text field
      - On success store OAuth tokens
   B. ApiKey with tui_always_prompt (OpenCode Go):
      - Show signup URL
      - TUI MUST open masked "Enter API key" modal (required)
      - Optional secondary: use existing env key
      - Persist API key
4. Optional: verify with a cheap model ping via LiteLLM worker
5. Set active model to profile default (or offer model pick)
6. Confirm without secrets: "Connected <title> · model … · key_source=oauth|file|env|provided"
```

TUI: overlays from [tui-overlays.md](./tui-overlays.md) + auth-mode-specific modals ([connect-auth-modes.md](./connect-auth-modes.md)). REPL: OAuth poll vs secure key read.

### 3.3 Connect profile schema

```rust
struct ConnectProfile {
    id: String,                 // "xai" | "opencode_go"
    title: String,              // "xAI Grok" | "OpenCode Go"
    description: String,
    auth_mode: AuthMode,        // Phase 6.1 — see connect-auth-modes.md
    /// Env names for ApiKey mode (first present wins). Empty for pure OAuth profiles.
    api_key_env: Vec<String>,
    default_base_url: Option<String>,
    default_models: Vec<String>,
    auth_url: Option<String>,
    litellm_provider_prefix: String,
}
```

Built-in profiles registered at startup.

### 3.4 Credential storage

| Priority (read) | Location |
|-----------------|----------|
| 1 | Process env (API-key profiles) |
| 2 | User secrets: `~/.config/forge/credentials.toml` (mode 0600) — API keys **and** OAuth token blobs |
| 3 | Phase 2 vault injection into worker env |

**Never** commit credentials to project `forge.toml`.

Credentials are injected into the native model client and process environment; they are never model-visible payload fields.

### 3.5 Relationship to `/model`

| Command | Role |
|---------|------|
| `/connect` | Auth + profile + default model |
| `/model` | Switch among known models (including those from connected profiles) |

After connect, `/model` list includes profile `default_models`.

### 3.6 Slash parser

```text
/connect
/connect list
/connect status
/connect xai
/connect opencode_go
/connect disconnect
/connect disconnect xai
```

Unknown profile id → error listing known ids.

### 3.7 Surfaces

| Surface | Support |
|---------|---------|
| `forge` | Full interactive overlay |
| `forge repl` | Line prompts |
| ACP | Optional later: same command string |
| Headless CI | Use env keys; `/connect` not required |

## 4. Interfaces

```rust
trait ConnectRegistry {
    fn profiles(&self) -> &[ConnectProfile];
    fn get(&self, id: &str) -> Option<&ConnectProfile>;
}

trait CredentialStore {
    fn get_api_key(&self, profile_id: &str) -> Option<SecretString>;
    fn set_api_key(&self, profile_id: &str, key: SecretString) -> Result<(), ConnectError>;
    fn clear(&self, profile_id: &str) -> Result<(), ConnectError>;
}

async fn run_connect_flow(
    profile: &ConnectProfile,
    store: &dyn CredentialStore,
    session: &mut AgentSession, // or config mutator
) -> Result<ConnectOutcome, ConnectError>;
```

`ConnectOutcome { profile_id, model, key_source }`.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Empty key | Reject; stay disconnected |
| Verify ping fails (auth) | Surface error; do not mark connected |
| Worker missing (no Python) | Error: install Phase 5 worker |
| Disconnect with only env key | Report “using env; clear shell env to remove” |
| Secret file permissions too open | Refuse write; warn |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| `/connect` command + registry + store | **6** (this doc) |
| Grok profile details | [provider-xai-grok.md](./provider-xai-grok.md) |
| OpenCode Go profile details | [provider-opencode-go.md](./provider-opencode-go.md) |
| LiteLLM transport | Phase 5 |

## 7. Acceptance

1. Unit tests: parse `/connect` variants; profile registry contains `xai` and `opencode_go`.  
2. Integration (mock store): connect → active model set; status hides secrets.  
3. Manual/smoke with real keys optional in CI secrets.  

## Related docs

- [provider-xai-grok.md](./provider-xai-grok.md)  
- [provider-opencode-go.md](./provider-opencode-go.md)  
- [model-providers.md](./model-providers.md)
- [tui-commands.md](./tui-commands.md)  
