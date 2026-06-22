# xAI Grok connect profile design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **6 only** (exclusive)  
**PRD:** PROV-01  
**Architecture:** §14 Phase 6, decision #19  
**Related:** [connect-command.md](./connect-command.md), [litellm-providers.md](./litellm-providers.md)

---

## 1. Problem / context

Operators want **Grok** models from **xAI** with a first-class Forge experience. Phase 5 already routes arbitrary LiteLLM model strings (including `xai/…`) through the worker. Phase 6 productizes discovery, credential setup, and recommended models via **`/connect`**.

## 2. Goals & non-goals

**Goals**

- Connect profile id `xai` titled **xAI Grok**.  
- Credential via `XAI_API_KEY` (env) or stored key written for the worker.  
- Default recommended Grok model ids as LiteLLM strings.  
- Smoke path: connected session runs tools through existing agent loop.

**Non-goals**

- Native Rust xAI HTTP client (removed in Phase 5).  
- xAI Console product features (billing, org admin).  
- Guaranteeing every Grok SKU forever—catalog is configurable.

## 3. Design

### 3.1 Profile registration

| Field | Value |
|-------|--------|
| `id` | `xai` |
| `title` | xAI Grok |
| `api_key_env` | `["XAI_API_KEY"]` |
| `auth_url` | `https://console.x.ai/` (or current xAI docs URL) |
| `litellm_provider_prefix` | `xai` |
| `default_models` | See §3.2 |

### 3.2 Recommended models (illustrative; pin at implement time)

LiteLLM-style ids (pass-through to worker):

| Model string | Notes |
|--------------|--------|
| `xai/grok-3` | Default if available via LiteLLM catalog |
| `xai/grok-3-mini` | Lower cost / latency option |
| `xai/grok-2` | Fallback if 3 not listed |

Implementation should read LiteLLM’s model list when possible and fall back to this table.

### 3.3 Connect behavior

1. `/connect` → select **xAI Grok**.  
2. If `XAI_API_KEY` already set: offer “use existing env key” vs replace.  
3. Else prompt for API key; store per [connect-command.md](./connect-command.md) §3.4.  
4. Optional verify: worker `complete` with tiny prompt using default model.  
5. Set session/config `model` to default Grok id; `provider` remains `litellm`.

### 3.4 Worker env

On spawn of `forge-litellm-worker`, ensure `XAI_API_KEY` is present in child env (from store or process env). Do not put the key in NDJSON wire params.

### 3.5 Config example (after connect)

```toml
[model]
provider = "litellm"
model = "xai/grok-3"
```

Credentials stay outside this file.

## 4. Interfaces

- Profile constant / registration in connect registry.  
- No new `ModelClient` type.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Invalid key | Verify fails; remain disconnected |
| LiteLLM missing `xai` provider support | Error with upgrade/pin guidance for `litellm` package |
| Rate limit | Standard Phase 5 model error |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This profile | **6** |
| Transport / worker | **5** |
| `/connect` UX | [connect-command.md](./connect-command.md) |

## 7. Acceptance

1. Registry includes `xai`.  
2. Connect with fixture key store sets model to a `xai/…` string.  
3. Documented env: `XAI_API_KEY`.  
4. Optional live smoke in CI when secret present.

## Related docs

- [connect-command.md](./connect-command.md)  
- [https://docs.x.ai/](https://docs.x.ai/) (external)  
