# web_search — Web Search Backend

## Overview

Implements the `web_search` built-in (WEB-01). The module owns the model-facing
argument struct, the `Tool` implementation, and a pluggable `SearchBackend`
trait. The only backend implemented today is the mock, which is reject-by-default
and never registered for a production session; real providers are added behind
`SearchBackend` without changing the tool surface.

## Interface

- `WebSearchArgs { query, num_results, site, recency_days }` — JSON schema is
  generated with `schemars`; only `query` is required.
- `WebSearchTool` — `try_new(&WebSearchConfig) -> Option<Self>`,
  `new_for_tests(cfg, backend)`, implements `Tool`.
- `should_register_web_search(&WebSearchConfig) -> bool`
- `web_search_tool(&WebSearchConfig) -> Option<Arc<dyn Tool>>` — production
  constructor; `None` when config disables it or selects the mock provider.
- `web_search_tool_for_tests() -> Arc<dyn Tool>` — mock-backed tool.
- `SearchBackend` trait — `id()`, `async search(&SearchRequest, &SearchSecrets)
  -> Result<Vec<SearchHit>, SearchError>`.
- `SearchRequest { query, num_results, recency_days }`
- `SearchHit { title, url, snippet }`
- `SearchSecrets { api_key }` with `from_config(&WebSearchConfig)`.
- `SearchError` — `MissingKey`, `Http`, `Provider`, `Timeout`, `Other`.
- `format_search_results(query, &[SearchHit]) -> String`
- `MockSearchBackend` — deterministic test backend.

## Invariants

- Validation happens in `ToolRegistry` before `call`; `call` still defensively
  deserializes and returns `ToolError::Execution` on a shape mismatch.
- An empty or whitespace-only query after trimming is an error `ToolOutput`,
  not a backend call.
- Query longer than `cfg.max_query_chars` is rejected before dispatch.
- `num_results` defaults to 5 and is clamped to `1..=cfg.max_results.max(1)`.
- A provider whose `needs_api_key()` is true and that has no resolved key
  yields an error `ToolOutput` and never reaches the backend.
- `site` is appended as `site:<value>` only when non-blank and the query does
  not already contain `site:`; blank values are ignored.
- Successful backend output is scanned for the resolved API key; a match is
  replaced by a redaction error. A backend `Err` is returned to the model as
  `web_search failed: <error>` without that scan.
- Backend selection (`build_backend`, crate-private) maps only
  `WebSearchProvider::Mock`; its timeout argument is currently ignored.
- `side_effect_class()` is `Network`; `idempotent()` is `true`.
- The mock provider is test-only: `try_new`/`web_search_tool` must never expose
  fabricated results to a production session.
- `format_search_results` always emits the `## Web search: <query>` header and
  a trailing JSON fence; empty hit lists render `No results.` and no fence.

## Side Effects

- Reads the configured API-key environment variable at call time.
- Network I/O could only happen inside a `SearchBackend`; the only backend,
  `MockSearchBackend`, does none. The tool ignores `ToolContext`, so it does
  not use the session egress grant/gateway that `web_fetch` uses.
- No filesystem writes, no process spawning, and no mutation of tool or
  registry state.

## Verification

- Run: `cargo test --package forge-tools --locked web_search`
