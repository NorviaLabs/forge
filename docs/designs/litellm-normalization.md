# LiteLLM → Forge envelope normalization design

**Status:** Shipped (product)  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **5 only** (exclusive)  
**PRD:** MDL-01 (supporting — type mapping)  
**Architecture:** §4.3 LLM stream events  
**Related:** [litellm-wire.md](./litellm-wire.md), [litellm-providers.md](./litellm-providers.md), [model-providers.md](./model-providers.md)

---

## 1. Problem / context

LiteLLM returns OpenAI-like (and occasionally provider-native) chat completions and streams. Forge’s agent loop, journal, TUI, and tools require a **single canonical envelope** defined in Phase 1 ([model-providers.md](./model-providers.md)).

Normalization must land **once** so `forge-core` never sees LiteLLM-specific shapes.

## 2. Goals & non-goals

**Goals**

- Map complete + stream outputs to Forge `ModelResponse` / `ModelStreamEvent`.  
- Canonical tool calls: `id`, `name`, `arguments` as JSON object.  
- Preserve usage tokens when LiteLLM/upstream provides them.  
- Fail closed on unparseable tool arguments (validation retry path still applies).

**Non-goals**

- Perfect parity of every LiteLLM provider quirk.  
- Changing CORE-01 tool schemas.  
- Exposing raw LiteLLM response objects to surfaces.

## 3. Design

### 3.1 Canonical Forge types (Phase 1 — unchanged)

**Stream events** (architecture §4.3):

| Event | Payload |
|-------|---------|
| `text_delta` | `text: string` |
| `tool_call_start` | `tool_call_id`, `name` |
| `tool_call_delta` | `tool_call_id`, `arguments_delta` (string fragment) |
| `tool_call_end` | `tool_call_id`, `name`, `arguments: object` |
| `usage` | `prompt_tokens`, `completion_tokens`, `total_tokens` (optional fields) |
| `message_end` | `finish_reason` optional |
| `error` | `message`, `code` optional |

**Complete response:**

| Field | Type |
|-------|------|
| `text` | string |
| `tool_calls` | list of `{ id, name, arguments }` |
| `usage` | optional token counts |

### 3.2 Where normalization runs

**Decision:** Worker emits **Forge-shaped** wire events/results ([litellm-wire.md](./litellm-wire.md) §3.5–3.6). Rust performs validation and type conversion only.

Rationale: one Python bridge next to LiteLLM stream iterators; Rust stays thin.

### 3.3 Complete path mapping

| LiteLLM / OpenAI-ish field | Forge |
|----------------------------|-------|
| `choices[0].message.content` (str or list parts) | Concatenate text parts → `text` |
| `choices[0].message.tool_calls[]` | Each → canonical tool call |
| `tool_calls[].id` | `id` (generate stable id if missing) |
| `tool_calls[].function.name` | `name` |
| `tool_calls[].function.arguments` | Parse JSON string → object; on failure → empty object + surface validation later **or** fail request with `invalid_params` |
| `usage.prompt_tokens` etc. | `usage` |
| `choices[0].finish_reason` | `finish_reason` |

**Content blocks:** If content is a list of `{type: text, text: ...}`, join texts. Ignore non-text blocks in v1 (log count).

### 3.4 Stream path mapping

| LiteLLM stream chunk | Forge event(s) |
|----------------------|----------------|
| `delta.content` non-empty | `text_delta` |
| First tool call index N with name | `tool_call_start` |
| `delta.tool_calls[].function.arguments` fragment | `tool_call_delta` |
| Stream end for tool index N | `tool_call_end` with fully assembled JSON object |
| Final usage chunk | `usage` |
| Stream finished successfully | Wire final `response` + Rust emits `message_end` if not already |
| Provider error mid-stream | Wire `error` → Forge stream `error` / `ModelError` |

**Assembly:** Buffer argument fragments per `tool_call_id` / index until parseable JSON object or stream end; on end with invalid JSON → `tool_call_end` with `arguments: {}` and let tool validation fail, **or** fail stream—prefer **fail stream with clear error** for incomplete tool JSON in Phase 5 v1 (stricter, fewer half-tools).

### 3.5 Tools sent to LiteLLM

Forge tool descriptors → OpenAI function tools:

```json
{
  "type": "function",
  "function": {
    "name": "<forge tool name>",
    "description": "<desc>",
    "parameters": <json schema object>
  }
}
```

MCP namespaced names pass through unchanged. ACL-filtered list only (same as Phase 1).

### 3.6 Messages sent to LiteLLM

| Forge message role | Wire / LiteLLM |
|--------------------|----------------|
| system | system |
| user | user |
| assistant (+ optional tool_calls) | assistant |
| tool (+ tool_call_id) | tool |

Do not include vault secrets or Authorization headers in messages.

### 3.7 Errors

| Source | Forge `ModelError` class (illustrative) |
|--------|----------------------------------------|
| Wire `upstream_auth` | Auth |
| Wire `upstream_rate_limit` | RateLimit (retry eligible) |
| Wire `upstream` | Provider |
| Wire `protocol` / `internal` | Transport / Internal |
| Normalization failure | Protocol |

### 3.8 Observability fields (non-secret)

Safe to attach on spans/logs:

- `provider = "litellm"`  
- `model` = configured LiteLLM model string  
- `finish_reason`  
- token counts from `usage`  

Never: API keys, full Authorization headers, raw env dumps.

## 4. Interfaces

- Python: `normalize.complete_result(litellm_response) -> dict` matching wire `result`.  
- Python: async generator mapping stream chunks → wire `event` params.  
- Rust: `fn parse_complete_result(value: Value) -> Result<ModelResponse, ModelError>`.  
- Unit tests: fixtures of LiteLLM-like JSON → expected Forge structs (both sides).

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Empty choices | Error `upstream` / empty response policy (error) |
| Parallel tool_calls in one message | All mapped; agent loop sequential tool exec (Phase 1) |
| Provider returns only tool_calls, no text | `text = ""` OK |
| Streaming then error | Emit error; do not invent tool_call_end for unfinished calls |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **5** |
| Envelope definition | Phase 1 [model-providers.md](./model-providers.md) |

## Related docs

- [model-providers.md](./model-providers.md)  
- [litellm-wire.md](./litellm-wire.md)  
- [tool-protocol.md](./tool-protocol.md)  
