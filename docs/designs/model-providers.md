# Model providers design

**Status:** Superseded (production)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **1** (historical)  
**PRD:** Multi-provider portability (historical)  
**Architecture:** §4.3, decision #11 / #18  
**Related:** [litellm-providers.md](./litellm-providers.md) (current), [agent-loop.md](./agent-loop.md)  

**Do not implement native OpenAI/Anthropic/xAI HTTP adapters.** Production uses LiteLLM only ([litellm-providers.md](./litellm-providers.md)). This file is retained only for the historical `ModelClient` / stream-envelope contract that LiteLLM still satisfies.

---

## 1. Problem / context

Providers differ in APIs, streaming shapes, and tool-call encodings. The harness needs one loop and one journal schema regardless of vendor.

## 2. Goals & non-goals

**Goals**

- Unified `ModelClient` abstraction; switch via **config only**.  
- Normalized stream events for core and surfaces.  
- Phase 1 adapters: OpenAI-compatible, Anthropic, xAI.  
- Credentials never appear in prompts, journal model-visible fields, or default OTEL attributes.

**Non-goals**

- Training or hosting models.  
- Vendor-specific branching in the agent loop.  
- Guaranteeing identical quality across providers.

## 3. Design

### 3.1 Configuration

```toml
[model]
provider = "anthropic"          # openai_compatible | anthropic | xai | …
model = "claude-sonnet"
# base_url optional for openai_compatible / local
# api keys: env / vault — not committed in plaintext ideally
```

Env overrides e.g. `FORGE_MODEL_PROVIDER`, `FORGE_MODEL_ID`, `FORGE_API_KEY` (dev). Production prefers vault injection for the HTTP client, not chat.

### 3.2 Trait sketch

```rust
#[async_trait]
trait ModelClient: Send + Sync {
    async fn complete(
        &self,
        req: ModelRequest,
    ) -> Result<ModelResponse, ModelError>;

    /// Streaming variant yields normalized events.
    async fn complete_stream(
        &self,
        req: ModelRequest,
    ) -> Result<BoxStream<ModelStreamEvent>, ModelError>;
}
```

`ModelRequest` includes: messages (already assembled), tool definitions (JSON schemas), sampling params, session correlation ids.

### 3.3 Normalized stream events

| Event | Meaning |
|-------|---------|
| `text_delta` | Assistant text chunk |
| `tool_call_start` / `tool_call_delta` / `tool_call_end` | Structured tool invocation |
| `usage` | Prompt/completion tokens for budgeting |
| `message_end` | Turn complete |
| `error` | Provider/transport failure |

Adapters map vendor SSE/JSON streams → this envelope.

### 3.4 Tool-call encoding

- Core always works in a **canonical tool_call** structure: `id`, `name`, `arguments` (JSON object or string parsed to object).  
- Adapters convert to/from provider-specific formats (OpenAI `tool_calls`, Anthropic `tool_use`, etc.).

### 3.5 Phase 1 provider matrix

| Provider | Adapter | Notes |
|----------|---------|-------|
| OpenAI-compatible | yes | Also local vLLM / many proxies |
| Anthropic | yes | |
| xAI | yes | |
| Google ADK / Ollama | later | Same trait |

### 3.6 Secrets

- HTTP auth headers injected by client construction (env/vault).  
- Redact Authorization and api_key fields from logs and OTEL.  
- Model request bodies must not embed API keys.

## 4. Interfaces

- Factory: `ModelClient::from_config(&ModelConfig) -> Box<dyn ModelClient>`.  
- Health: optional `ping` for headless startup checks.  
- `/model` TUI command switches config for the session (or next session—see open questions).

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Auth failure | Terminal model error; surface-visible; no tool exec |
| Rate limit | Retry with backoff (bounded); then fail |
| Malformed tool JSON from model | Validation layer handles after stream end |
| Partial stream drop | Surface error; journal error event |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document (native adapters + early matrix) | **1** |
| Exit (Phase 1) | Config switch among OpenAI-compatible, Anthropic, xAI without code changes |
| Production path after Phase 5 | **LiteLLM only** — natives deleted; see [litellm-providers.md](./litellm-providers.md) |
| Vault-backed secrets | [governance.md](./governance.md) Phase 2 (Phase 1 uses env) |

## 7. Open questions

1. Mid-session `/model` switch: immediate vs next session only.  
2. Default temperature/max_tokens product defaults.  
3. Prompt caching headers per vendor (optional optimizations).

## Related docs

- [configuration.md](./configuration.md)  
- [agent-loop.md](./agent-loop.md)  
- [observability.md](./observability.md)  
