# LiteLLM worker design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **5 only** (exclusive)  
**PRD:** MDL-01 (supporting — process boundary)  
**Architecture:** §14 Phase 5, decision #18  
**Related:** [litellm-providers.md](./litellm-providers.md), [litellm-wire.md](./litellm-wire.md), [litellm-config.md](./litellm-config.md)

---

## 1. Problem / context

LiteLLM’s provider matrix lives in **Python**. Forge must call the **SDK library** (`import litellm`) without embedding CPython in the main binary and without running LiteLLM **Proxy**.

A **Forge-managed worker process** is the process boundary: Rust owns lifecycle; Python owns provider HTTP.

## 2. Goals & non-goals

**Goals**

- Isolated Python process that only speaks the wire protocol ([litellm-wire.md](./litellm-wire.md)).  
- Prefer **long-lived** process (LiteLLM import / cold-start cost).  
- Inherit credentials from **environment** only (vault → env at parent spawn).  
- Deterministic package layout under `workers/forge-litellm-worker`.  
- Clear diagnostics when Python or `litellm` is missing.

**Non-goals**

- Running gunicorn / uvicorn LiteLLM Proxy.  
- Sharing memory with the Rust process (PyO3 in-process LiteLLM).  
- Multi-tenant worker serving many Forge sessions as a network service.  
- Shipping `fast-litellm` as a hard dependency (optional later).

## 3. Design

### 3.1 Layout

```text
workers/forge-litellm-worker/
  pyproject.toml          # depends on litellm (pin major.minor)
  README.md               # install + run notes
  src/forge_litellm_worker/
    __init__.py
    __main__.py           # python -m forge_litellm_worker
    server.py             # read stdin lines, write stdout lines
    litellm_bridge.py     # completion / stream wrappers
    normalize.py          # optional; may live here or pure map in Rust
```

Entry:

```bash
python -m forge_litellm_worker
# or: config model.litellm_python + model.litellm_worker
```

### 3.2 Process lifecycle

| Mode | Behavior |
|------|----------|
| **Long-lived (default)** | Spawn once per `LiteLlmModelClient` (or per CLI process); reuse for all completes |
| **Per-call (debug)** | Spawn → one request → exit; config flag for diagnosis only |

```text
LiteLlmModelClient::from_config
  → Command::new(python).arg("-m").arg(module)
  → set env: PATH, provider keys already in parent env (filtered if needed)
  → pipe stdin/stdout; stderr → tracing at warn (never log secrets)
  → send initialize / ping
  → ready

Drop / shutdown
  → send shutdown (best effort) → kill after grace timeout
```

### 3.3 Concurrency model

- **One in-flight `complete` or `complete_stream` per worker** (v1).  
- Agent loop is sequential model turns; parallel sessions = parallel clients/workers or a later pool.  
- Streaming: worker writes event frames until `message_end` / `error`; Rust must not interleave a second request.

### 3.4 Dependencies

| Package | Role |
|---------|------|
| `litellm` | **Required** — SDK only |
| Python | **3.10+** recommended (document exact floor in `pyproject.toml`) |
| `fast-litellm` | **Optional** — not installed by default |

Pin `litellm` in `pyproject.toml` / lockfile so CI and operators get reproducible installs.

Install (operator):

```bash
cd workers/forge-litellm-worker
pip install -e .
# or: uv sync
```

### 3.5 Environment & secrets

| Rule | Detail |
|------|--------|
| Keys | Standard LiteLLM env vars (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `XAI_API_KEY`, …) |
| Parent | May inject vault-resolved secrets into child env at spawn (Phase 2 vault) |
| Child | Must not print env values; errors may mention missing *name* of env var only |
| Wire | Never send API keys in JSON-RPC params |

### 3.6 Resource limits (guidance)

| Limit | Default intent |
|-------|----------------|
| Startup timeout | ~10–30s first import |
| Request timeout | Configurable; default align with Phase 1 HTTP client |
| Max stderr buffer | Bounded; drop/truncate with warning |
| Memory | No hard cap in v1; document operator ulimit if needed |

### 3.7 Health

On start, Rust sends `ping`. Worker responds `pong` with:

- `litellm_version` string  
- `python_version` string  

If ping fails: surface install path, not a generic “model error.”

### 3.8 Packaging with Forge release

| Channel | Expectation |
|---------|-------------|
| Source / git | Worker tree in monorepo; docs link install |
| Binary release | Document optional worker install; Forge binary remains pure Rust |
| CI | Unit tests mock worker; optional job with real litellm + recorded keys off |

## 4. Interfaces

- Process: stdin/stdout UTF-8 NDJSON or length-prefixed JSON per [litellm-wire.md](./litellm-wire.md).  
- CLI: `python -m forge_litellm_worker` exits 0 only after clean shutdown.  
- No public HTTP port.

## 5. Failure modes

| Case | Behavior |
|------|----------|
| Module not found | Exit non-zero; Rust maps to config error |
| Import litellm fails | Report on stderr once; fail ping |
| Uncaught exception in request | Wire `error` response; worker stays up if possible |
| Broken pipe | Rust treats as transport error; may restart worker |
| Zombie after parent exit | Parent must kill on Drop; OS reaps if parent dies |

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **5** |
| Wire framing | [litellm-wire.md](./litellm-wire.md) |
| Response mapping | [litellm-normalization.md](./litellm-normalization.md) |

## Related docs

- [litellm-providers.md](./litellm-providers.md)  
- [litellm-wire.md](./litellm-wire.md)  
- [governance.md](./governance.md) (vault → env injection, Phase 2)  
