# LiteLLM universal providers design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **5 only** (exclusive)  
**PRD:** MDL-01  
**Architecture:** §4.3, §14 Phase 5, decision #18  
**Related:** [model-providers.md](./model-providers.md) (Phase 1 native adapters only)

---

## 1. Problem / context

Phase 1 ships a thin Rust `ModelClient` with a few first-party adapters. Operators need the **long-tail** of cloud and local providers without a growing set of hand-written Rust clients. [LiteLLM](https://docs.litellm.ai/) exposes 100+ providers through one Python API.

Forge is Rust. This phase integrates the **LiteLLM library (SDK)**—not the LiteLLM **Proxy** gateway—so provider coverage expands without running an always-on LLM gateway product.

## 2. Goals & non-goals

**Goals**

- Config-only switch onto LiteLLM model strings (`provider/model` as LiteLLM expects).  
- Same `ModelClient` trait and normalized stream/tool events as Phase 1.  
- Secrets via process env / vault injection into the worker—not into model-visible journal fields.  
- Phase 1 native adapters remain usable with **zero Python**.

**Non-goals**

- Requiring or shipping **LiteLLM Proxy** (gateway, virtual keys, admin UI).  
- Guaranteeing identical model quality across vendors.  
- Replacing the agent loop, tools, or journal.  
- Pure-Rust reimplementation of every LiteLLM provider.

## 3. Design

### 3.1 Components

| Piece | Role |
|-------|------|
| `LiteLlmModelClient` (Rust, `forge-model`) | Implements `ModelClient`; owns worker lifecycle |
| `forge-litellm-worker` (Python) | Calls `litellm.completion` / streaming; JSON-RPC over stdio |
| Config | `provider = "litellm"`, `model = "<litellm model string>"` |

```text
ModelRequest (Forge)
    → LiteLlmModelClient
        → worker: complete | complete_stream
            → litellm.completion / acompletion (SDK)
    ← ModelResponse / ModelStreamEvent (Forge envelope)
```

### 3.2 Configuration

```toml
[model]
provider = "litellm"
model = "anthropic/claude-sonnet-4-20250514"
# optional:
# litellm_python = "python3"
# litellm_worker = "forge-litellm-worker"  # module or path
```

Env: provider keys LiteLLM already documents (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `XAI_API_KEY`, …). Production prefers vault → env injection for the worker process only.

### 3.3 Worker protocol (sketch)

| Method | Purpose |
|--------|---------|
| `complete` | Non-streaming chat + tools → single response |
| `complete_stream` | Yield normalized events until `message_end` / `error` |
| `ping` | Optional health for startup |

Request payload carries messages, tool schemas, model id, sampling params. Response maps tool calls into Forge’s canonical `id` / `name` / `arguments`.

**Lifecycle:** Prefer a **long-lived** worker (LiteLLM import cost). Debug mode may spawn per call.

### 3.4 Explicitly not Proxy

| LiteLLM mode | Phase 5 |
|--------------|---------|
| Python SDK in-process to worker (`import litellm`) | **In scope** |
| LiteLLM Proxy as network gateway | **Out of scope** |
| Routing all traffic through a team LiteLLM Proxy URL | Optional operator choice only if they point `base_url` themselves—not product requirement |

### 3.5 Failure modes

| Case | Behavior |
|------|----------|
| Python / litellm missing | Clear config error; suggest install or native provider |
| Auth / rate limit from upstream | Surface as model error; same retry policy as Phase 1 |
| Worker crash | Fail request; restart worker on next call |
| Malformed tool JSON | Validation layer after complete (unchanged) |

## 4. Interfaces

- Factory: when `provider == litellm`, return `LiteLlmModelClient`.  
- `/model` and TUI model picker may list LiteLLM strings when backend is litellm (surfaces already config-driven).  
- No changes to journal schema version solely for this backend.

## 5. Phase ownership

| Item | Phase |
|------|-------|
| This entire document / MDL-01 | **5** |
| Native OpenAI-compatible / Anthropic / xAI adapters | **1** ([model-providers.md](./model-providers.md)) |
| Exit | Config-only multi-provider smoke via LiteLLM SDK; native path without Python |

## 6. Acceptance (implementation later)

1. Smoke: three LiteLLM model strings complete a tool-using turn.  
2. Stream events match Phase 1 envelope.  
3. CI default remains mock/native without Python.  
4. Docs never state Proxy as required.
