# LiteLLM worker wire protocol design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **5 only** (exclusive)  
**PRD:** MDL-01 (supporting — IPC contract)  
**Architecture:** Phase 5  
**Related:** [litellm-worker.md](./litellm-worker.md), [litellm-normalization.md](./litellm-normalization.md), [litellm-providers.md](./litellm-providers.md)

---

## 1. Problem / context

Rust and Python need a **stable, versioned, line-oriented** IPC so either side can evolve without silent breakage. The protocol carries model requests and streamed events only—never secrets as first-class fields.

## 2. Goals & non-goals

**Goals**

- One request / response correlation id.  
- Support non-streaming complete and streaming complete.  
- Schema version on every message.  
- Easy to mock in Rust tests (fake worker script).

**Non-goals**

- gRPC / Cap’n Proto in Phase 5 v1.  
- Multiplexed concurrent RPCs on one connection (v1 is serial).  
- Bidirectional unsolicited worker→client events except stream frames for an open stream.

## 3. Design

### 3.1 Transport

| Property | Value |
|----------|-------|
| Channel | Child process **stdin** / **stdout** |
| Encoding | UTF-8 |
| Framing | **NDJSON**: one JSON object per line (`\n` terminated) |
| stderr | Logs only; not protocol |

### 3.2 Envelope

Every line:

```json
{
  "v": 1,
  "id": "uuid-or-monotonic-string",
  "type": "request | response | event | error",
  "method": "optional for request/response",
  "params": {},
  "result": {},
  "error": { "code": "string", "message": "string", "data": {} }
}
```

| Field | Required | Notes |
|-------|----------|-------|
| `v` | yes | Protocol version; currently `1` |
| `id` | yes | Correlate request → response / events |
| `type` | yes | Discriminator |
| `method` | request | See §3.3 |
| `params` | request | Method-specific |
| `result` | response | Method-specific success body |
| `error` | error type or failed response | Machine `code` + human `message` |

### 3.3 Methods

| Method | Direction | Description |
|--------|-----------|-------------|
| `ping` | C→W | Liveness / versions |
| `shutdown` | C→W | Graceful exit |
| `complete` | C→W | Non-streaming chat + tools |
| `complete_stream` | C→W | Streaming; followed by `event` lines then final `response` |

**Client (C)** = Rust. **Worker (W)** = Python.

### 3.4 `ping`

Request:

```json
{"v":1,"id":"1","type":"request","method":"ping","params":{}}
```

Response:

```json
{
  "v": 1,
  "id": "1",
  "type": "response",
  "method": "ping",
  "result": {
    "ok": true,
    "python_version": "3.12.x",
    "litellm_version": "1.x.y"
  }
}
```

### 3.5 `complete` params

```json
{
  "model": "anthropic/claude-sonnet-4-20250514",
  "messages": [
    {"role": "system", "content": "..."},
    {"role": "user", "content": "..."},
    {"role": "assistant", "content": "...", "tool_calls": []},
    {"role": "tool", "tool_call_id": "...", "content": "..."}
  ],
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "read_file",
        "description": "...",
        "parameters": { "type": "object", "properties": {} }
      }
    }
  ],
  "temperature": null,
  "max_tokens": null,
  "extra": {}
}
```

**Message roles** use Forge/OpenAI-shaped roles. Worker maps to LiteLLM as needed.

**Response `result`:**

```json
{
  "text": "assistant text or empty",
  "tool_calls": [
    {
      "id": "call_…",
      "name": "read_file",
      "arguments": { "path": "README.md" }
    }
  ],
  "usage": {
    "prompt_tokens": 0,
    "completion_tokens": 0,
    "total_tokens": 0
  },
  "finish_reason": "stop | tool_calls | length | null",
  "raw_model": "optional upstream model id"
}
```

`arguments` must be a **JSON object** (worker parses string args if LiteLLM returns strings).

### 3.6 `complete_stream`

Request: same `params` as `complete`.

Then worker emits zero or more **events** with same `id`:

```json
{"v":1,"id":"9","type":"event","params":{"kind":"text_delta","text":"Hel"}}
{"v":1,"id":"9","type":"event","params":{"kind":"tool_call_start","tool_call_id":"c1","name":"bash"}}
{"v":1,"id":"9","type":"event","params":{"kind":"tool_call_delta","tool_call_id":"c1","arguments_delta":"{\"c"}}
{"v":1,"id":"9","type":"event","params":{"kind":"tool_call_end","tool_call_id":"c1","name":"bash","arguments":{"command":"ls"}}}
{"v":1,"id":"9","type":"event","params":{"kind":"usage","prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}
```

Terminal:

```json
{"v":1,"id":"9","type":"response","method":"complete_stream","result":{"ok":true,"finish_reason":"stop"}}
```

Or:

```json
{"v":1,"id":"9","type":"error","error":{"code":"upstream_auth","message":"…"}}
```

Event `kind` values align with Forge `ModelStreamEvent` names (see [litellm-normalization.md](./litellm-normalization.md)). Prefer worker emitting Forge-shaped kinds so Rust does thin validation; worker may also emit OpenAI-ish chunks if normalization is implemented on either side—**single place of truth: document chooses worker emits Forge-shaped events** (simpler Rust).

### 3.7 Error codes

| Code | Meaning |
|------|---------|
| `protocol` | Bad JSON / unknown method / version mismatch |
| `invalid_params` | Schema validation on params |
| `upstream_auth` | 401/403 from provider |
| `upstream_rate_limit` | 429 |
| `upstream` | Other provider HTTP/API error |
| `internal` | Worker bug / unexpected exception |
| `cancelled` | Future: cancel support |

### 3.8 Versioning

- Bump `v` on breaking changes.  
- Worker rejects unsupported `v` with `protocol` error.  
- Additive optional fields allowed without bump if receivers ignore unknowns.

### 3.9 Security

- No `api_key` fields in params.  
- Truncate huge tool outputs only after policy at Forge context layer (not wire’s job beyond reasonable max line size, e.g. reject lines > 16 MiB).

## 4. Interfaces

- Formal schema: optional JSON Schema files under `workers/forge-litellm-worker/schemas/` for CI validation.  
- Rust: internal types `WireRequest`, `WireEvent` in `forge-model` (not public API).  
- Golden tests: fixed NDJSON fixtures in both languages.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Partial line on crash | Discard; fail open stream |
| Unknown event kind | Rust → `ModelError` or ignore with log (prefer fail on unknown in strict mode) |
| Response without matching id | Log + ignore; fail pending waiter by timeout |
| Double response for id | Ignore second; log error |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **5** |
| Semantic mapping of tool args | [litellm-normalization.md](./litellm-normalization.md) |

## Related docs

- [litellm-worker.md](./litellm-worker.md)  
- [litellm-normalization.md](./litellm-normalization.md)  
- [model-providers.md](./model-providers.md) (§3.3 stream events)  
