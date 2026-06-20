# LiteLLM universal providers design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **5 only** (exclusive)  
**PRD:** MDL-01 (primary owner)  
**Architecture:** §4.3, §14 Phase 5, decision #18  
**Related (Phase 5):** [litellm-worker.md](./litellm-worker.md), [litellm-wire.md](./litellm-wire.md), [litellm-normalization.md](./litellm-normalization.md), [litellm-config.md](./litellm-config.md)  
**Related (Phase 1):** [model-providers.md](./model-providers.md)

---

## 1. Problem / context

Phase 1 ships a thin Rust `ModelClient` with a few first-party adapters. Operators need the **long-tail** of cloud and local providers without a growing set of hand-written Rust clients. [LiteLLM](https://docs.litellm.ai/) exposes 100+ providers through one Python API.

Forge is Rust. This phase integrates the **LiteLLM library (SDK)**—not the LiteLLM **Proxy** gateway—so provider coverage expands without running an always-on LLM gateway product.

Supporting designs split implementation concerns:

| Doc | Concern |
|-----|---------|
| **This doc** | Product architecture, Rust client, factory, ownership |
| [litellm-worker.md](./litellm-worker.md) | Python process, deps, lifecycle, packaging |
| [litellm-wire.md](./litellm-wire.md) | stdio JSON-RPC messages |
| [litellm-normalization.md](./litellm-normalization.md) | LiteLLM → Forge stream/tool envelope |
| [litellm-config.md](./litellm-config.md) | Phase 5 TOML / env keys only |

## 2. Goals & non-goals

**Goals**

- Config-only switch onto LiteLLM model strings (`provider/model` as LiteLLM expects).  
- Same `ModelClient` trait and normalized stream/tool events as Phase 1.  
- Secrets via process env / vault injection into the worker—not into model-visible journal fields.  
- Phase 1 native adapters remain usable with **zero Python**.  
- Agent loop and surfaces never import LiteLLM or branch on vendor strings.

**Non-goals**

- Requiring or shipping **LiteLLM Proxy** (gateway, virtual keys, admin UI).  
- Guaranteeing identical model quality across vendors.  
- Replacing the agent loop, tools, or journal.  
- Pure-Rust reimplementation of every LiteLLM provider.  
- Adopting **`fast-litellm`** as the multi-provider backend (see §8).  
- Changing Phase 1–4 crate public contracts beyond factory + config.

## 3. Design

### 3.1 System context

```text
                    ┌─────────────────────────────────────┐
                    │ forge-cli / tui / acp / channels     │
                    └─────────────────┬───────────────────┘
                                      │ AgentSession
                                      ▼
                    ┌─────────────────────────────────────┐
                    │ forge-core                          │
                    │   dyn ModelClient                   │
                    └───┬─────────────────────┬───────────┘
                        │                     │
           Phase 1      │                     │  Phase 5
           Mock /       │                     │  LiteLlmModelClient
           OpenAI-compat│                     │
           Anthropic    │                     │ stdio JSON-RPC
           xAI          │                     ▼
                        │            ┌────────────────────┐
                        │            │ forge-litellm-worker│
                        │            │  import litellm    │
                        │            │  (SDK, not Proxy)  │
                        │            └─────────┬──────────┘
                        │                      ▼
                        │            upstream provider APIs
                        ▼
                   (direct HTTP — Phase 1 only)
```

### 3.2 Components

| Piece | Crate / package | Role |
|-------|-----------------|------|
| `ModelClient` trait | `forge-model` (Phase 1) | Unchanged contract |
| `LiteLlmModelClient` | `forge-model` | Phase 5 adapter; owns worker handle |
| Factory `client_from_config` | `forge-model` | When `provider == litellm` → LiteLLM client |
| `forge-litellm-worker` | `workers/forge-litellm-worker` | Python; LiteLLM SDK only |
| Config keys | `forge-config` | See [litellm-config.md](./litellm-config.md) |

### 3.3 Rust client responsibilities

`LiteLlmModelClient` must:

1. Start or attach a worker ([litellm-worker.md](./litellm-worker.md)).  
2. Translate `ModelRequest` → wire `complete` / `complete_stream` ([litellm-wire.md](./litellm-wire.md)).  
3. Translate worker responses → `ModelResponse` / `ModelStreamEvent` ([litellm-normalization.md](./litellm-normalization.md)).  
4. Map worker/transport failures to `ModelError` (auth, rate limit, transport, protocol).  
5. Never log raw API keys; never put keys into request bodies beyond what the worker inherits from env.

### 3.4 Factory behavior

```text
match config.model.provider:
  mock | openai_compatible | anthropic | xai  → Phase 1 clients
  litellm                                     → LiteLlmModelClient::from_config
  other                                       → ConfigError
```

Startup when `provider = litellm`:

1. Validate Python path / worker entry (config).  
2. Optional `ping` with timeout; fail with actionable install message if worker or `litellm` missing.  
3. Prefer long-lived worker for process lifetime of the CLI/session.

### 3.5 Interaction with other phases

| Phase capability | Behavior with LiteLLM backend |
|------------------|-------------------------------|
| Journal (1) | Unchanged; model request/response metadata only (no secrets) |
| Tools / validation (1) | Unchanged; tool schemas still Forge-side |
| HITL / governance (2) | Unchanged; ACL applies before tools run |
| OTEL (3) | `model.complete` span attributes: `provider=litellm`, model string; no keys |
| TUI `/model` (4) | May set provider/model labels; full session re-bind semantics as Phase 1 open question |

### 3.6 Configuration (summary)

Full keys: [litellm-config.md](./litellm-config.md).

```toml
[model]
provider = "litellm"
model = "anthropic/claude-sonnet-4-20250514"
```

## 4. Interfaces

```rust
// forge-model (sketch)
pub struct LiteLlmModelClient { /* worker handle */ }

impl ModelClient for LiteLlmModelClient {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError>;
    async fn complete_stream(
        &self,
        req: ModelRequest,
    ) -> Result<BoxStream<'static, Result<ModelStreamEvent, ModelError>>, ModelError>;
}

impl LiteLlmModelClient {
    pub fn from_config(cfg: &Config) -> Result<Self, ModelError>;
}
```

Public surface stays behind `dyn ModelClient` / factory used by `forge-cli` and tests.

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Python missing | Config/startup error: install Python 3.x |
| `litellm` package missing | Startup error: `pip install litellm` (or project lockfile) |
| Worker crash mid-stream | `ModelError::transport`; next call may restart worker |
| Upstream auth / rate limit | Mapped model error; retries per Phase 1 policy if any |
| Unsupported LiteLLM feature | Fail closed with clear message; do not partial-apply tools |
| Provider set to litellm in CI without Python | Fail only when that config is used; default mock path unaffected |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| MDL-01 primary design | **5** (this doc) |
| Worker / wire / normalize / config | **5** (sibling designs) |
| Native adapters | **1** ([model-providers.md](./model-providers.md)) |
| Exit | PRD §13 Phase 5 exit criteria |

## 7. Acceptance (implementation)

Aligns with PRD Phase 5 exit:

1. Three LiteLLM model strings complete a tool-using turn (live or recorded).  
2. Stream/tool events match Phase 1 envelope.  
3. Default CI / mock / native path needs **no** Python.  
4. Docs never require LiteLLM Proxy.  
5. No API keys in journal model-visible fields, TUI, or default OTEL attrs.

## 8. Ecosystem evaluation (pre-implementation)

| Project | What it is | Fit for Forge Phase 5 |
|---------|------------|------------------------|
| **[fast-litellm](https://github.com/neul-labs/fast-litellm)** | PyO3 **acceleration** of Python LiteLLM | **Not primary.** Optional later inside worker only. |
| **[litellm-rust](https://github.com/avivsinai/litellm-rust)** | Minimal pure-Rust SDK (few providers) | **Not MDL-01.** Limited catalog; optional future native work. |
| **litellm-rs** / official Rust gateway | Gateway/proxy path | **Out of scope.** |

**Conclusion:** Phase 5 = **Python LiteLLM library in a Forge-managed worker**.

## Related docs

- [litellm-worker.md](./litellm-worker.md)  
- [litellm-wire.md](./litellm-wire.md)  
- [litellm-normalization.md](./litellm-normalization.md)  
- [litellm-config.md](./litellm-config.md)  
- [model-providers.md](./model-providers.md)  
- [configuration.md](./configuration.md) (Phase 1 keys only)  
