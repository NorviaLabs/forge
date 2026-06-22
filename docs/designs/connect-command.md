# `/connect` command design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **6 only** (exclusive)  
**PRD:** CONN-01 (primary)  
**Architecture:** §14 Phase 6, decision #19  
**Related:** [provider-xai-grok.md](./provider-xai-grok.md), [provider-opencode-go.md](./provider-opencode-go.md), [litellm-providers.md](./litellm-providers.md) (Phase 5), [tui-commands.md](./tui-commands.md) (Phase 1 catalog), [tui-overlays.md](./tui-overlays.md)

---

## 1. Problem / context

Phase 5 unifies all live inference on the LiteLLM SDK worker. Operators still need a **guided way** to attach credentials and pick a productized backend—especially **xAI Grok** and **OpenCode Go**—without hand-editing TOML or memorizing model strings.

OpenCode popularized a **`/connect`** flow: pick a provider → open auth if needed → paste API key → use recommended models. Forge adopts the same operator pattern for its first-class profiles.

## 2. Goals & non-goals

**Goals**

- Slash command **`/connect`** in full-screen TUI and line-mode REPL.  
- Extensible **connect profile** registry (Phase 6 ships Grok + OpenCode Go).  
- Store credentials securely (user config secrets file and/or env; vault when Phase 2 SEC-01 available).  
- Activate a LiteLLM model string + worker env for the session (and optionally persist for next launch).  
- Never print secrets in chat, journal model-visible fields, sidebar, or default OTEL.

**Non-goals**

- A second production HTTP client (Phase 5 LiteLLM path remains sole).  
- Full multi-tenant OAuth server inside Forge.  
- Billing or account management for OpenCode Go / xAI.  
- Replacing `/model` (complementary: connect establishes credentials; model picks id).

## 3. Design

### 3.1 Command surface

| Form | Behavior |
|------|----------|
| `/connect` | Open interactive flow (palette/list of profiles) |
| `/connect list` | List registered profiles + connected? + active? |
| `/connect <profile_id>` | Jump to that profile (e.g. `/connect xai` or `/connect opencode_go`) |
| `/connect status` | Show active profile id, model string, key source (env \| file \| vault)—**never** key material |
| `/connect disconnect [profile_id]` | Clear stored key for profile; keep env-only keys untouched |

Optional CLI (headless): `forge connect [--provider xai|opencode_go] [--key-file …]` — same semantics; no secret in argv logs.

### 3.2 Interactive flow (normative)

```text
1. List connect profiles (id, title, connected badge)
2. Operator selects one
3. If profile needs external signup: print URL (e.g. OpenCode auth) — browser optional
4. Prompt: API key (masked input in TUI; line-mode uses secure read if available)
5. Optional: verify with a cheap model ping via LiteLLM worker
6. Persist credential (see §3.4)
7. Set active model to profile default (or offer model pick)
8. Confirm: "Connected xAI Grok · model xai/grok-…"
```

TUI: reuse overlay patterns from [tui-overlays.md](./tui-overlays.md) (list + modal input). REPL: numbered list + prompts.

### 3.3 Connect profile schema

```rust
struct ConnectProfile {
    id: String,                 // "xai" | "opencode_go"
    title: String,              // "xAI Grok" | "OpenCode Go"
    description: String,
    /// Env vars the worker should see (first present wins for API key).
    api_key_env: Vec<String>,   // e.g. ["XAI_API_KEY"]
    /// Optional OpenAI-compatible or LiteLLM base URL for this profile.
    default_base_url: Option<String>,
    /// LiteLLM model strings recommended after connect.
    default_models: Vec<String>,
    /// Optional docs URL for signup.
    auth_url: Option<String>,
    /// How to map into LiteLLM (usually model prefix or custom provider).
    litellm_provider_prefix: String,
}
```

Built-in profiles are registered at startup; plugins may add more later via the same registry (out of Phase 6 scope unless trivial).

### 3.4 Credential storage

| Priority (read) | Location |
|-----------------|----------|
| 1 | Process env (already set `XAI_API_KEY` etc.) |
| 2 | User secrets file: `~/.config/forge/credentials.toml` (mode 0600) or OS keychain if available later |
| 3 | Phase 2 vault injection into worker env |

**Never** commit credentials to project `forge.toml`. Project config may only reference `model = "xai/…"` after connect.

Wire to worker: parent process sets env for `forge-litellm-worker` spawn (existing Phase 5 rule: keys in env, not wire JSON).

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
| `forge tui` | Full interactive overlay |
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
| Worker missing (no Python) | Error: install Phase 5 worker or use `--mock` |
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
- [litellm-config.md](./litellm-config.md)  
- [tui-commands.md](./tui-commands.md)  
