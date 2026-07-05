# LiteLLM configuration design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **5 only** (exclusive)  
**PRD:** MDL-01 (supporting — config surface + migration)  
**Architecture:** §9, Phase 5, decision #12 / #18  
**Related:** [litellm-providers.md](./litellm-providers.md), [litellm-worker.md](./litellm-worker.md), [configuration.md](./configuration.md) (Phase 1 merge rules only)

---

## 1. Problem / context

Phase 5 makes LiteLLM the **only** production model backend and **removes** native provider adapters. Config must:

1. Express LiteLLM model strings cleanly.  
2. **Migrate** Phase 1 `provider = anthropic | xai | openai_compatible` configs so operators are not stranded.  
3. Keep **mock** for CI without Python.  
4. Avoid a second “native vs litellm” switch.

## 2. Goals & non-goals

**Goals**

- One live config shape: model string (+ optional worker settings).  
- Explicit migration from Phase 1 provider enums.  
- Same merge precedence as Phase 1 (CLI > env > project TOML > user TOML > defaults).  
- Fail fast if live mode cannot start the worker—**no** fallback to deleted natives.

**Non-goals**

- Parallel `provider=openai_compatible` production path.  
- LiteLLM Proxy URL as required setting.  
- Storing API keys in TOML as recommended practice.

## 3. Design

### 3.1 Canonical Phase 5 TOML

```toml
[model]
# Live (default for non-mock runs once Phase 5 ships):
provider = "litellm"   # or omit if litellm is the sole live default
model = "anthropic/claude-sonnet-4-20250514"

# Offline CI:
# provider = "mock"
# model = "mock"

[model.litellm]
python = "python3"
module = "forge_litellm_worker"
request_timeout_secs = 120
startup_timeout_secs = 30
lifecycle = "long_lived"   # long_lived | per_call
```

### 3.2 Provider enum (post–Phase 5)

| Value | Client | Notes |
|-------|--------|-------|
| **`litellm`** | `LiteLlmModelClient` | **Sole production** path |
| **`mock`** | `MockModelClient` | CI only |
| `openai_compatible` | — | **Removed**; migrate |
| `anthropic` | — | **Removed**; migrate |
| `xai` | — | **Removed**; migrate |

### 3.3 Migration from Phase 1 configs

On load, if deprecated provider is seen:

| Old `provider` | Old `model` (example) | Migrated |
|----------------|----------------------|----------|
| `openai_compatible` | `gpt-4.1-mini` | `provider=litellm`, `model=openai/gpt-4.1-mini` (or pass-through if already `org/model`) |
| `anthropic` | `claude-sonnet` | `provider=litellm`, `model=anthropic/<model>` |
| `xai` | `grok-…` | `provider=litellm`, `model=xai/<model>` |

**Policy (implementation choice, pick one and document in release notes):**

1. **Auto-migrate with warning** (preferred for minor UX pain), or  
2. **Hard error** with exact replacement TOML printed.

Do **not** silently call removed native HTTP clients.

`base_url` for openai-compatible locals: map to LiteLLM custom provider / `api_base` via worker `extra` or LiteLLM env—document in worker README (not a second Rust client).

### 3.4 Env overrides

| Env | Maps to |
|-----|---------|
| `FORGE_MODEL_PROVIDER` | `model.provider` (`litellm` \| `mock` only after Phase 5) |
| `FORGE_MODEL_ID` | `model.model` (LiteLLM string) |
| `FORGE_LITELLM_PYTHON` | `model.litellm.python` |
| `FORGE_LITELLM_MODULE` | `model.litellm.module` |
| `FORGE_LITELLM_LIFECYCLE` | `model.litellm.lifecycle` |
| `FORGE_LITELLM_REQUEST_TIMEOUT_SECS` | timeouts |
| `FORGE_LITELLM_STARTUP_TIMEOUT_SECS` | timeouts |

Credentials: LiteLLM’s env vars (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `XAI_API_KEY`, …).

### 3.5 CLI

| Flag | Behavior |
|------|----------|
| `--provider litellm` | Live path |
| `--provider mock` | Mock client |
| `--model <string>` | LiteLLM model id |
| `--provider anthropic` (etc.) | Reject or migrate per §3.3 |

### 3.6 Defaults

| Key | Default |
|-----|---------|
| Live provider | `litellm` |
| `python` | `python3` |
| `module` | `forge_litellm_worker` |
| `lifecycle` | `long_lived` |
| timeouts | 120s request / 30s startup |

### 3.7 Validation

| Condition | Result |
|-----------|--------|
| Live + empty model | Config error |
| Live + worker ping fails | Startup error + install hint; **no** native fallback |
| Deprecated provider without migration | Error or warn+migrate |
| `provider=mock` | Skip worker entirely |

## 4. Interfaces

```rust
pub enum ModelProviderKind {
    Litellm,
    Mock,
    // Deprecated variants may exist only for parse+migrate, then collapse
}

pub struct LitellmConfig {
    pub python: String,
    pub module: String,
    pub worker_path: Option<PathBuf>,
    pub request_timeout_secs: u64,
    pub startup_timeout_secs: u64,
    pub lifecycle: LitellmLifecycle,
}
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Old binary vs new config | Fine |
| New binary vs old provider enum | Migrate or error—never native HTTP |
| Missing Python on live run | Clear install message |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This document | **5** |
| Base merge order | **1** ([configuration.md](./configuration.md)) |

## Related docs

- [litellm-providers.md](./litellm-providers.md)  
- [configuration.md](./configuration.md)  
