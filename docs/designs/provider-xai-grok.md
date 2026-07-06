# xAI Grok connect profile design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **6 only** (exclusive)  
**Revision:** **6.1** — OAuth-only connect (not API key paste)  
**PRD:** PROV-01  
**Architecture:** Phase 6 / 6.1, decision #19  
**Related:** [connect-command.md](./connect-command.md), [connect-auth-modes.md](./connect-auth-modes.md), [litellm-providers.md](./litellm-providers.md)

---

## 1. Problem / context

Operators want **Grok** models from **xAI** with a first-class Forge experience. Phase 5 routes LiteLLM model strings (`xai/…`) through the worker. Phase 6 productizes connect; **Phase 6.1** corrects auth: **xAI Grok is connected via OAuth** (browser / device-code login against xAI account infrastructure), **not** by pasting a console API key.

This matches product intent for SuperGrok / X Premium+ style account login (see community patterns against `accounts.x.ai`) rather than the developer-console API-key path.

## 2. Goals & non-goals

**Goals**

- Connect profile id `xai` titled **xAI Grok**.  
- **`auth_mode = oauth`** (device code and/or system browser).  
- Persist OAuth tokens (access + refresh as provided by xAI) under the credential store—**never** treat API key paste as the primary UX.  
- Default recommended Grok model ids as LiteLLM strings.  
- Smoke path: after OAuth, session runs tools through the agent loop.

**Non-goals**

- Primary UX of “paste `XAI_API_KEY`” for this profile (Phase 6.0 draft; **superseded**).  
- Native Rust xAI HTTP client (Phase 5).  
- xAI billing / console admin.  
- Guaranteeing every Grok SKU forever.

## 3. Design

### 3.1 Profile registration

| Field | Value |
|-------|--------|
| `id` | `xai` |
| `title` | xAI Grok |
| `auth_mode` | **`oauth`** |
| `oauth_auth_server` | `https://accounts.x.ai` (or current xAI OAuth host) |
| `api_endpoint` | `https://api.x.ai/v1` (OpenAI-compatible chat) |
| `api_key_env` | **empty for primary path** — optional fallback only if product later re-enables key mode (default **off**) |
| `auth_url` | Start-login / docs link shown before OAuth |
| `litellm_provider_prefix` | `xai` |
| `default_models` | See §3.2 |

### 3.2 Recommended models (illustrative; pin at implement time)

| Model string | Notes |
|--------------|--------|
| `xai/grok-3` | Default if available via LiteLLM catalog |
| `xai/grok-3-mini` | Lower cost / latency |
| `xai/grok-2` | Fallback |

### 3.3 Connect behavior (OAuth — normative)

```text
1. /connect → select "xAI Grok"
2. Show short copy: "Sign in with your xAI / SuperGrok account (OAuth). API keys are not used."
3. Start OAuth:
   a. Prefer system browser open to authorize URL, or
   b. Device-code flow: display user_code + verification_uri; poll token endpoint
4. On success: store tokens in credentials store under profile `xai`
   - access_token (secret)
   - refresh_token (secret, if issued)
   - expires_at
5. Optional: verify with tiny completion via LiteLLM worker using token auth
6. Set active model to default Grok LiteLLM id; provider remains litellm
7. Confirm: "Connected xAI Grok · model xai/… · key_source=oauth"
   (never print tokens)
```

**TUI:** OAuth progress overlay (waiting for browser / device code), **no** API-key text field for this profile.  
**REPL:** print device code + URL; block/poll until done or timeout.  
**CLI:** `forge connect xai` starts OAuth (no `--key` required; `--key` is **rejected** or ignored with warning for this profile).

### 3.4 Token storage & worker

| Item | Behavior |
|------|----------|
| Store file | `~/.config/forge/credentials.toml` (0600) section per profile, or adjacent oauth blob — still never in `forge.toml` |
| Refresh | Background or on-demand refresh before worker spawn if near expiry |
| Worker env | Inject credentials for LiteLLM in the form LiteLLM/xAI expect for OAuth-backed sessions (e.g. bearer token env or short-lived key derived by adapter). **Do not** put tokens on the NDJSON wire. |
| Disconnect | Clear stored OAuth tokens for `xai`; env-only leftovers are operator-managed |

Exact LiteLLM env var names for OAuth tokens are implementation details; document in worker README at ship time.

### 3.5 Explicitly not API-key connect

| Mode | Allowed for `xai`? |
|------|--------------------|
| OAuth (browser / device code) | **Yes — required primary** |
| Paste API key in TUI | **No** |
| `forge connect xai --key …` | **No** (error: use OAuth) |
| Pre-set `XAI_API_KEY` env | Optional **escape hatch** for CI/power users only; not advertised in TUI as the connect path |

### 3.6 Config after connect

```toml
[model]
provider = "litellm"
model = "xai/grok-3"
```

Tokens stay outside project config.

## 4. Interfaces

```rust
// Profile flag
auth_mode: AuthMode::Oauth { device_code: true, browser: true }

async fn connect_xai_oauth(store: &CredentialStore) -> Result<ConnectOutcome, ConnectError>;
```

No new `ModelClient` type.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| User cancels OAuth | Remain disconnected; clear message |
| Device-code timeout | Error; allow retry |
| Refresh failure | Force re-`/connect`; do not fall back to prompting for API key |
| LiteLLM cannot use token | Error with pin guidance for worker/litellm version |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This profile + OAuth semantics | **6** (rev 6.1) |
| Generic auth modes | [connect-auth-modes.md](./connect-auth-modes.md) |
| Transport | Phase 5 |

## 7. Acceptance

1. Registry `xai` has `auth_mode = oauth`.  
2. TUI connect path for `xai` shows OAuth UI, **not** an API-key field.  
3. Successful mock OAuth fixture stores tokens and sets `xai/…` model.  
4. Status reports `key_source=oauth` without token material.  
5. `forge connect xai --key anything` fails closed with OAuth guidance.

## Related docs

- [connect-command.md](./connect-command.md)  
- [connect-auth-modes.md](./connect-auth-modes.md)  
- [https://accounts.x.ai](https://accounts.x.ai) / [https://docs.x.ai/](https://docs.x.ai/) (external)  
