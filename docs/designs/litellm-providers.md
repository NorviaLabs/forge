# LiteLLM universal providers design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **5 only** (exclusive)  
**PRD:** MDL-01 (primary owner)  
**Architecture:** §4.3, Phase 5, decision #18  
**Related (Phase 5):** [litellm-worker.md](./litellm-worker.md), [litellm-wire.md](./litellm-wire.md), [litellm-normalization.md](./litellm-normalization.md), [litellm-config.md](./litellm-config.md)  
**Related (Phase 1):** [model-providers.md](./model-providers.md) (historical; **superseded for production**)

---

## 1. Problem / context

Phase 1 shipped a thin Rust `ModelClient` with **native** HTTP adapters (OpenAI-compatible, Anthropic, xAI). That was enough for an early coding agent. Operators still need the **full** provider catalog, and maintaining **two** production paths (hand-rolled Rust + LiteLLM) duplicates streaming, tool encoding, retries, and auth with no operator benefit.

Phase 5 **consolidates** on the [LiteLLM](https://docs.litellm.ai/) **Python library (SDK)**—not the Proxy—and **removes** the native adapters from the product path. One client, one config surface, one failure model.

Supporting designs:

| Doc | Concern |
|-----|---------|
| **This doc** | Product architecture, sole production client, removal of natives, factory |
| [litellm-worker.md](./litellm-worker.md) | Python process, deps, lifecycle |
| [litellm-wire.md](./litellm-wire.md) | stdio JSON-RPC |
| [litellm-normalization.md](./litellm-normalization.md) | LiteLLM → Forge envelope |
| [litellm-config.md](./litellm-config.md) | Config + migration from Phase 1 provider enums |

## 2. Goals & non-goals

**Goals**

- **Single production `ModelClient`:** `LiteLlmModelClient` only.  
- **Remove** Phase 1 native HTTP adapters from `forge-model` product builds.  
- Config-only model selection via LiteLLM model strings (covers former natives: OpenAI, Anthropic, xAI, plus long tail).  
- Same `ModelClient` trait and stream/tool events for the agent loop.  
- Secrets via env/vault into the worker—not model-visible journal fields.  
- **Mock** client retained for offline unit/CI tests (no Python required for mock).  
- Agent loop never imports LiteLLM or branches on vendor strings.

**Non-goals**

- Keeping dual native+LiteLLM production stacks “for performance.”  
- Requiring **LiteLLM Proxy**.  
- Guaranteeing identical quality across vendors.  
- Pure-Rust reimplementation of the full LiteLLM catalog.  
- **`fast-litellm`** as primary backend (see §8).  
- Changing tool, journal, or agent-loop contracts.

## 3. Design

### 3.1 Why not two paths?

| Dual stack cost | Single LiteLLM path |
|-----------------|---------------------|
| Two stream parsers / tool encodings | One normalization layer |
| Two auth/retry behaviors | One worker policy |
| Docs and `/model` picker list both | One model-string surface |
| Bugs fixed twice | Fixes land once in worker + wire |

Phase 1 natives were a bootstrap. Phase 5 makes LiteLLM the product—not an optional overlay.

### 3.2 System context

```text
AgentSession → dyn ModelClient
                 ├─ MockModelClient          (CI / --mock only)
                 └─ LiteLlmModelClient       (sole production path)
                        │ stdio JSON-RPC
                        ▼
                   forge-litellm-worker
                        │ import litellm  # SDK, not Proxy
                        ▼
                   all upstream providers
```

### 3.3 Components

| Piece | Role |
|-------|------|
| `ModelClient` trait | Unchanged (Phase 1) |
| `LiteLlmModelClient` | **Only** production implementation |
| `MockModelClient` | Offline tests / `--mock` |
| Factory | `mock` → Mock; else → LiteLLM (see [litellm-config.md](./litellm-config.md)) |
| Native OpenAI/Anthropic/xAI modules | **Deleted** in Phase 5 implementation |

### 3.4 Rust client responsibilities

Same as before: spawn/reuse worker, wire RPC, map to Forge types, map errors, never log keys.

### 3.5 Factory behavior (Phase 5)

```text
if mock flag or provider == mock:
    MockModelClient
else:
    LiteLlmModelClient::from_config   # includes former openai/anthropic/xai
```

There is **no** branch that constructs native HTTP clients.

### 3.6 Removal checklist (implementation)

1. Delete or gate out `openai_compat` / native Anthropic HTTP code paths.  
2. Factory + config reject or migrate old `provider` values (see config design).  
3. CLI help/README: live mode needs Python + `litellm`.  
4. Tests that hit real HTTP adapters move to worker fixtures or recorded LiteLLM shapes.  
5. TUI model picker lists LiteLLM-oriented models, not three hard-coded native rows only.

### 3.7 Interaction with other phases

Unchanged harness contracts (journal, tools, HITL, OTEL, TUI). OTEL attributes: `provider=litellm` (or `mock`), model string.

## 4. Interfaces

```rust
impl ModelClient for LiteLlmModelClient { /* complete + complete_stream */ }
impl LiteLlmModelClient {
    pub fn from_config(cfg: &Config) -> Result<Self, ModelError>;
}
// Mock remains; no OpenAiCompatibleClient / AnthropicClient public API
```

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Live config without Python/litellm | Startup error + install hint (no silent native fallback) |
| Worker crash | Transport error; optional restart |
| Upstream auth / rate limit | Model errors; same product semantics as Phase 1 |
| Old config `provider=anthropic` | Migrate or hard error with mapping help ([litellm-config.md](./litellm-config.md)) |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| MDL-01 + native removal | **5** |
| Historical native adapter design | **1** ([model-providers.md](./model-providers.md)) — superseded |
| Exit | Architecture Phase 5 (single path) |

## 7. Acceptance

1. No production native HTTP provider clients remain.  
2. Three LiteLLM model strings (e.g. openai / anthropic / xai or others) complete tool-using turns.  
3. Stream/tool envelope matches Phase 1.  
4. Mock works without Python.  
5. Live path requires worker; no Proxy required.  
6. Migration docs for old provider enums.

## 8. Ecosystem evaluation

| Project | Fit |
|---------|-----|
| **fast-litellm** | Not primary; optional worker accel only |
| **litellm-rust** | Not a dual production stack; limited catalog—not MDL-01 substitute |
| **Gateway / Proxy** | Out of scope |

**Conclusion:** One production path = Python LiteLLM SDK worker. Natives removed.

## Related docs

- [litellm-worker.md](./litellm-worker.md)  
- [litellm-wire.md](./litellm-wire.md)  
- [litellm-normalization.md](./litellm-normalization.md)  
- [litellm-config.md](./litellm-config.md)  
- [model-providers.md](./model-providers.md)  
