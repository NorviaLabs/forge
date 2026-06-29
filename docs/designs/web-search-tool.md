# Web search tool design

**Status:** Draft  
**Owner:** Mohit Ranka  
**Last updated:** 23 Jul 2026  
**Phase:** **9 only** (exclusive)  
**PRD:** **WEB-01** (primary)  
**Architecture:** §14 Phase 9, decision #23  
**Related:** [tool-protocol.md](./tool-protocol.md) (Phase 1 CORE-01), [governance.md](./governance.md) (Phase 2 SEC-*), [configuration.md](./configuration.md) (Phase 1 base config merge), [agent-loop.md](./agent-loop.md), [context-lifecycle.md](./context-lifecycle.md)

---

## 1. Problem / context

Phase 1 built-ins cover the **workspace** (`read_file`, `write_file`, `bash`, `grep`). Coding agents frequently need **current public web knowledge** (docs, APIs, package versions, error pages) that is not in the repo and is not provided by MCP unless the operator wires a search server.

Operators should get a **first-class, schema-validated `web_search` tool** that:

- Appears in the model tool list when enabled and ACL-allowed  
- Uses the same CORE-01 validate → journal intent → execute → journal result path as other built-ins  
- Does **not** require a separate MCP server for the default experience  
- Keeps API keys out of prompts, journal model-visible fields, TUI, and default OTEL  

Phase 9 owns this product capability exclusively.

---

## 2. Goals & non-goals

**Goals**

- Built-in tool **`web_search`** with schemars input schema and serializable results.  
- **Pluggable search backends** behind one tool name (Brave / Tavily / Serper / mock).  
- Config + env for enablement and API keys; vault injection remains Phase 2 SEC-01 path.  
- `side_effect_class = network`; ACL and sandbox policies can restrict it (SEC-02 / SEC-03).  
- Large result sets respect CTX-01 offload thresholds.  
- Mock backend for offline CI without network.  
- Visible in TUI tool cards like any other tool (TUI-02 — no new chrome required).

**Non-goals**

- Full browser automation / headless Chrome (separate product if ever).  
- Web **fetch of arbitrary URLs** as a required Phase 9 deliverable (optional stretch: `web_fetch` later).  
- Replacing MCP; orgs may still attach MCP search servers — namespaced, ACL-filtered.  
- Training or fine-tuning models on search results.  
- Scraping Google HTML without a licensed API (unsupported / fragile).  
- Changing the agent loop, journal schema version, or LiteLLM path.  
- New slash command catalog entries (optional later); the **model** invokes the tool.

---

## 3. Design

### 3.1 Tool contract

| Field | Value |
|-------|--------|
| `name` | `web_search` |
| `description` | Model-facing: search the public web for recent documentation, APIs, errors, and references. Returns ranked results with title, URL, and snippet. |
| `side_effect_class` | `network` |
| `idempotent` | `true` for incomplete-intent **retry policy** only; completed results are **not** re-executed on replay (DUR-02 caches `tool_result`) |
| `requires_hitl` | Default **false**; policy packs may require HITL for `network` class |

#### Input (`WebSearchArgs`)

| Field | Type | Required | Notes |
|-------|------|----------|--------|
| `query` | string | yes | Non-empty after trim; max length configurable (default 512) |
| `num_results` | integer | no | Default 5; clamped to `[1, max_results]` from config |
| `site` | string | no | Optional site filter (backend may map to `site:` syntax) |
| `recency_days` | integer | no | Optional freshness hint; backends that lack support ignore |

JSON Schema produced via `schemars` on the Rust args type (CORE-01).

#### Output (`ToolOutput.content`)

Human- and model-readable **markdown** (primary), optionally with a trailing fenced JSON block for structure:

```markdown
## Web search: <query>

1. **Title**
   - URL: https://…
   - Snippet: …

2. …
```

On hard failure (HTTP/auth/timeout): `is_error = true` and a short explanation **without** leaking API keys or full response bodies that may contain secrets.

### 3.2 Backend trait

```rust
// illustrative
#[async_trait]
trait SearchBackend: Send + Sync {
    fn id(&self) -> &str; // "brave" | "tavily" | "serper" | "mock"
    async fn search(
        &self,
        req: SearchRequest,
        secrets: &SearchSecrets, // injected, never logged
    ) -> Result<Vec<SearchHit>, SearchError>;
}

struct SearchHit {
    title: String,
    url: String,
    snippet: String,
    // optional published_at, score — omit if backend lacks them
}
```

| Backend id | Auth | Notes |
|------------|------|--------|
| `mock` | none | Deterministic fixtures from query hash; CI default when network disabled |
| `brave` | `BRAVE_API_KEY` (or config env name) | Brave Search API |
| `tavily` | `TAVILY_API_KEY` | Tavily Search API (agent-oriented) |
| `serper` | `SERPER_API_KEY` | Google via Serper |

**Default product recommendation:** `tavily` or `brave` when a key is present; otherwise tool registration is **skipped** or backend is `mock` depending on config (`enabled` + `require_key`).

Exactly **one** backend is active per process (config). Adding a backend is “impl `SearchBackend` + register in factory,” not a second tool name.

### 3.3 Registration lifecycle

1. Load `[tools.web_search]` from config (see §3.5).  
2. If `enabled = false` → do **not** register `web_search`.  
3. Resolve backend id; if live backend lacks key and `require_key = true` → do not register (or register with fail-closed description — prefer **omit** so model does not invent calls).  
4. Register `WebSearchTool` into `default_builtins()` / `ToolRegistry` with other built-ins.  
5. SEC-02 ACL filter applies before model listing (same as any tool).  
6. On call: CORE-01 validate → DUR-01 journal `tool_intent` → vault/env inject key → HTTP search → journal `tool_result` → context ingest / offload.

```text
Model tool_call web_search
    → validate WebSearchArgs
    → journal tool_intent
    → ACL authorize (network / web_search)
    → inject SearchSecrets (env or vault)
    → SearchBackend::search
    → format ToolOutput
    → journal tool_result
    → offload if oversized (CTX-01)
```

### 3.4 Security & redaction

| Concern | Rule |
|---------|------|
| API keys | Env / vault only; never in tool args, description, journal content, TUI, OTEL attributes |
| Query text | Journaled as tool args (operator-visible); may contain PII — redaction hooks reuse Phase 3 OBS redactors when present |
| Result URLs/snippets | Treated as untrusted text; no automatic code execution |
| Egress | Sandbox profiles that deny network must block execution (SEC-03); fail closed with clear error |
| Rate limits | Backend 429 → error to model with retry-after if present; no infinite retry in tool handler |

### 3.5 Configuration

Phase 9 owns these keys (merge with Phase 1 TOML + env rules):

```toml
[tools.web_search]
enabled = true
provider = "tavily"          # mock | brave | tavily | serper
api_key_env = "TAVILY_API_KEY"
max_results = 8
timeout_ms = 15000
require_key = true           # if true and key missing → tool not registered
max_query_chars = 512
```

Env overrides (illustrative):

| Env | Maps to |
|-----|---------|
| `FORGE_WEB_SEARCH_ENABLED` | `enabled` |
| `FORGE_WEB_SEARCH_PROVIDER` | `provider` |
| `TAVILY_API_KEY` / `BRAVE_API_KEY` / `SERPER_API_KEY` | backend secret (name from `api_key_env`) |

**Never** put API keys in project-committed `forge.toml`.

### 3.6 Context & durability

- Completed searches: replay returns **cached** `tool_result` (DUR-02).  
- Incomplete intent (process crash mid-HTTP): if idempotent, may retry once; prefer fail-safe message if partial network state is unclear.  
- Large combined markdown → CTX-01 offload to `.forge/offload/` with URI stub in the active window when over threshold.

### 3.7 Surfaces

| Surface | Behavior |
|---------|----------|
| Headless / REPL / TUI / ACP | Same tool registry; model decides when to call |
| TUI tool card | Name `web_search`, redacted args (show query; never key), status running/done/error |
| `/tools` | Lists `web_search` when registered |
| Slash commands | **None** required in Phase 9 |

### 3.8 Testing strategy

| Layer | Coverage |
|-------|----------|
| Unit | Args schema validation; mock backend fixtures; output formatting |
| Integration | ToolRegistry registers when enabled; omitted when disabled/missing key |
| Replay | Journal fixture with completed `web_search` does not re-hit network |
| Security | Key never appears in `ToolOutput` or journal event JSON under default redaction |
| Manual smoke | Live backend with real key; TUI shows tool card |

---

## 4. Interfaces (crate sketch)

| Piece | Location |
|-------|----------|
| `WebSearchTool` + args types | `forge-tools` (`builtins` or `web_search` module) |
| `SearchBackend` + Brave/Tavily/Serper/Mock | `forge-tools` (HTTP via `reqwest`) |
| Config struct | `forge-config` `[tools.web_search]` |
| Registration | `default_builtins()` / session bootstrap in `forge-core` |
| Optional vault key name | `forge-governance` inject path (existing SEC-01) |

No new workspace crate is required for v1; split only if HTTP client surface grows large.

---

## 5. Failure modes & edge cases

| Case | Behavior |
|------|----------|
| Empty / whitespace query | Validation error (CORE-01 retry to model) |
| `num_results` out of range | Clamp or validation error (prefer clamp to config max) |
| Missing API key at call time | Execution error; do not panic; message “web_search not configured” |
| Backend timeout | Error tool result; no partial silent success |
| Backend 401/403 | Error; never echo response body that may include key material |
| Zero hits | Success with empty list + “No results” prose |
| ACL deny | Tool absent from list or deny at call (SEC-02) |
| Network sandbox deny | Fail closed with policy message |

---

## 6. Phase ownership

| Item | Phase |
|------|-------|
| This entire document | **9** |
| Tool schema / validation machinery | Phase 1 [tool-protocol.md](./tool-protocol.md) (dependency) |
| ACL / vault / sandbox | Phase 2 [governance.md](./governance.md) (dependency) |
| Payload offload | Phase 2 [context-lifecycle.md](./context-lifecycle.md) (dependency) |
| TUI tool cards | Phase 4 [tui-conversation.md](./tui-conversation.md) (dependency) |

---

## 7. Open questions

1. Whether a follow-on **`web_fetch`** (URL → text/markdown) ships in the same phase or a later increment (default: **later**).  
2. Default live provider when multiple keys are present (prefer explicit `provider` config).  
3. Optional disk cache of search results by query hash (off by default for freshness).

---

## Related docs

- [tool-protocol.md](./tool-protocol.md)  
- [governance.md](./governance.md)  
- [context-lifecycle.md](./context-lifecycle.md)  
- [../architecture.md](../architecture.md) §14 Phase 9  
- [../prd.md](../prd.md) WEB-01  
