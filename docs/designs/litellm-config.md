# LiteLLM configuration design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **5 only** (exclusive)  
**PRD:** MDL-01 (supporting — config surface)  
**Architecture:** §9, §14 Phase 5, decision #12 / #18  
**Related:** [litellm-providers.md](./litellm-providers.md), [litellm-worker.md](./litellm-worker.md), [configuration.md](./configuration.md) (Phase 1 only)

---

## 1. Problem / context

Phase 1 [configuration.md](./configuration.md) owns TOML/env merge for early keys and must not grow multi-phase ownership. Phase 5 needs **exclusive** keys for the LiteLLM backend, worker paths, and timeouts—without forcing Python on default installs.

## 2. Goals & non-goals

**Goals**

- Config-only switch: `provider = "litellm"` + LiteLLM model string.  
- Same merge precedence as Phase 1 (CLI > env > project TOML > user TOML > defaults).  
- Sensible defaults for python/module paths.  
- Fail fast with install hints when LiteLLM path is selected but unusable.

**Non-goals**

- Remote config service.  
- Storing API keys in TOML as the recommended path.  
- LiteLLM Proxy URL as a first-class required setting.

## 3. Design

### 3.1 TOML keys (Phase 5)

```toml
[model]
provider = "litellm"   # new enum variant
model = "openai/gpt-4.1-mini"   # LiteLLM model string when provider=litellm

# Phase 5 optional block (ignored unless provider=litellm)
[model.litellm]
python = "python3"                    # executable
module = "forge_litellm_worker"       # python -m …
# worker_path = "/abs/path/to/__main__.py"  # alternative to module
request_timeout_secs = 120
startup_timeout_secs = 30
lifecycle = "long_lived"              # long_lived | per_call
# extra_env = { "LITELLM_LOG" = "ERROR" }  # optional non-secret
```

**Model string:** Pass through to LiteLLM unchanged (e.g. `anthropic/…`, `gemini/…`, `openrouter/…`).

### 3.2 Provider enum

Extend Phase 1 `ModelProviderKind` (name illustrative):

| Value | Client |
|-------|--------|
| `openai_compatible` | Phase 1 |
| `anthropic` | Phase 1 |
| `xai` | Phase 1 |
| `mock` | Phase 1 / CLI `--mock` |
| **`litellm`** | **Phase 5** `LiteLlmModelClient` |

Unknown values: config error at load/start.

### 3.3 Env overrides

| Env | Maps to |
|-----|---------|
| `FORGE_MODEL_PROVIDER=litellm` | `model.provider` |
| `FORGE_MODEL_ID` | `model.model` (LiteLLM string when provider is litellm) |
| `FORGE_LITELLM_PYTHON` | `model.litellm.python` |
| `FORGE_LITELLM_MODULE` | `model.litellm.module` |
| `FORGE_LITELLM_LIFECYCLE` | `model.litellm.lifecycle` |
| `FORGE_LITELLM_REQUEST_TIMEOUT_SECS` | `model.litellm.request_timeout_secs` |
| `FORGE_LITELLM_STARTUP_TIMEOUT_SECS` | `model.litellm.startup_timeout_secs` |

Provider credentials remain LiteLLM’s own env vars (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, …)—not re-defined here. `FORGE_API_KEY` may continue as a dev convenience mapped only for native clients; document that LiteLLM path prefers vendor-specific env names.

### 3.4 CLI

| Flag | Behavior |
|------|----------|
| `--provider litellm` | Sets provider |
| `--model <string>` | Model id / LiteLLM string |
| `--mock` | Forces mock client; wins over litellm for offline tests |

No `forge litellm-proxy` command.

### 3.5 Defaults

| Key | Default |
|-----|---------|
| `python` | `python3` |
| `module` | `forge_litellm_worker` |
| `lifecycle` | `long_lived` |
| `request_timeout_secs` | `120` |
| `startup_timeout_secs` | `30` |

### 3.6 Validation rules

| Condition | Result |
|-----------|--------|
| `provider=litellm` and empty `model` | Config error |
| `lifecycle` not in enum | Config error |
| Timeouts ≤ 0 | Config error |
| `provider=litellm` and worker ping fails | Runtime/startup error with install hint (not silent fallback to native) |
| `provider!=litellm` | Ignore `[model.litellm]` block (warn if present in strict mode) |

### 3.7 TUI / `/model`

Phase 4 model picker and `/model` may set provider + model labels. Applying `litellm` mid-session: same policy as Phase 1 open question (prefer next session or re-create client). Document chosen behavior at implementation time; default recommendation: **rebuild client for session** when provider/model changes if no in-flight turn.

## 4. Interfaces

```rust
// sketch — forge-config
pub enum ModelProviderKind {
    OpenaiCompatible,
    Anthropic,
    Xai,
    Mock,
    Litellm, // Phase 5
}

pub struct LitellmConfig {
    pub python: String,
    pub module: String,
    pub worker_path: Option<PathBuf>,
    pub request_timeout_secs: u64,
    pub startup_timeout_secs: u64,
    pub lifecycle: LitellmLifecycle,
}

pub enum LitellmLifecycle {
    LongLived,
    PerCall,
}
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| TOML has litellm keys but old binary | Unknown key warn / ignore until upgrade |
| Env sets litellm without model | Fail at session open |
| Python path not executable | Startup error |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document / litellm keys | **5** |
| Base merge order & Phase 1 keys | **1** ([configuration.md](./configuration.md)) |

## Related docs

- [configuration.md](./configuration.md)  
- [litellm-providers.md](./litellm-providers.md)  
- [litellm-worker.md](./litellm-worker.md)  
