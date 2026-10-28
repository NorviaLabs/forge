# forge-model native transports

## Overview

Native Rust provider transports. Each wire module builds a request for one
wire protocol (`forge_connect::WireKind`), opens an HTTP stream with
`reqwest`, parses SSE lines, and normalizes the result into
`forge_types::ModelResponse`. `mod.rs` owns the shared client, the routing
snapshot (`forge_connect::RouteTable`), request-target resolution
(profile, credential, headers), the image cache, SSE line splitting, and
shared HTTP-status error mapping. `protocol.rs` dispatches by `WireKind`
with a `match`. The wire modules are private (`chat_completions`,
`responses`, `anthropic_messages`, `protocol`) and expose only `pub(super)`
seams; there are no per-provider modules.

## Interface

- `process_sse_lines(pending: &mut String, consume: impl FnMut(&str) -> Result<(), ModelError>) -> Result<(), ModelError>` — splits buffered SSE bytes into complete (trimmed) lines, consumes them, and leaves any trailing partial line in `pending`. Test-only sibling module `test_support` (behind `#[cfg(test)]`) provides `serve_once`.
- `NativeModelClient` — implements `crate::ModelClient`.
  - `from_config(&Config) -> Result<Self, ModelError>` — builds a `reqwest::Client` with `model.request_timeout_secs.max(1)` and `redirect::Policy::none()`; seeds its route table with `forge_connect::resolve(&loaded_registry(), &Credentials::shared())`.
  - `set_route_table(RouteTable)` (trait method) — replaces the routing snapshot behind a `RwLock`.
  - `load_image_ref(&Path, &ImageRef) -> Result<Arc<LoadedImage>, String>` — caches up to `IMAGE_CACHE_MAX_ENTRIES` (16) loaded images keyed by resolved path.
  - `prompt_wire` → `protocol::prompt_projection`; `prompt_transport_key` → `protocol::cache_key`, both for the request's wire (`wire_for_request`).
  - Private helpers `model_for`, `route_table`, `set_routes`, `wire_for_request`, `target_for`.
- `Target` (`pub(super)`) — the resolved destination: `profile_id`, `base_url`, `model` (`<model_provider_prefix>/<bare>`), `bare_model` (sent upstream), plus private `attach`, `optional`, `credential`, `static_headers`, `session_header`, `definition`.
  - `effort_support()` — `forge_connect::effort::effort(definition, bare_model, &ModelCatalogCache::user_default())`.
  - `authorize(RequestBuilder, &ModelRequest) -> Result<RequestBuilder, ModelError>` — adds static headers, the OpenCode session header, and the credential per `Attach::{None, Bearer, XApiKey, BearerAccountId}`.
- `async fn status_error(reqwest::Response, &Target) -> ModelError` (`pub(super)`, callers `.await` it) — shared non-2xx mapping used by every wire.
- Private `unknown_route(&RouteTable, model)` and `credential_error(CredentialError, profile_id)` build routing/credential errors.
- `protocol` (private):
  - `complete(client, req, target, wire, tx)` — `ChatCompletions` → `chat_completions::complete`; `AnthropicMessages` → `anthropic_messages::complete`; `Responses` with `Attach::BearerAccountId` → `responses::complete_chatgpt`, else `responses::complete_gateway`.
  - `prompt_projection(client, req, wire) -> Value` and `cache_key(wire) -> &'static str` (`"anthropic_messages"`, `"codex"`, `"openai_compat"`).
- `chat_completions` (private): `complete` — POSTs `<base>/chat/completions`.
- `responses` (private): `complete_chatgpt` (POSTs `<base>/codex/responses`), `complete_gateway` (POSTs `<base>/responses`), `request_body(client, req, model, aliases)`, `CodexStream{text, thinking, tool_calls, usage}` with `consume`/`finish`, `tool_aliases(req) -> BTreeMap<String, String>`.
- `anthropic_messages` (private): `messages_endpoint(base_url) -> String` — `{base}/messages`, inserting `/v1` unless the base already ends in `/v1`; `complete(...)`, `messages_body(req, Option<client>)`, `prompt_projection(client, req)`.

## Invariants

- Dispatch is by the model id's provider prefix, never by provider-id branches: `RouteTable::profile_for_request(model)` picks the profile and bare id, and the profile definition's `wire_for_model(bare)` picks the wire. There is no route id on `ModelRequest`.
- No configured default model: an empty/blank `req.model` fails with `ModelError::Other("model id is required")`. For prompt projection and cache keys only, a missing model or unknown prefix falls back to `WireKind::ChatCompletions`.
- A model whose prefix no profile serves fails with `ModelError::UnknownRoute`; an id under an old prefix (`forge_connect::old_prefix_hint`) names its current form, otherwise the message lists the known profile ids.
- Each in-flight request clones the `RouteTable` and releases the lock before awaiting, so a turn keeps its snapshot even if `set_route_table` runs mid-turn.
- OAuth refresh (`ensure_fresh_blocking`) runs on `tokio::task::spawn_blocking` before reading the credential. `CredentialError::Missing` → `MissingApiKey`; `AuthRequired` → `ProviderAuth{status: 401}` telling the user to `/connect <profile>`; `Store` → `Other`.
- A profile with `Attach::None` sends no credential regardless of auth kind. Otherwise a missing credential fails before sending (`MissingApiKey`) unless the profile's auth kind is `ApiKeyOptional`; `BearerAccountId` also requires an `account_id`.
- Base URL is the profile definition's `base_url`; there is no env/default fallback in this module.
- Redirects are never followed (reqwest would forward `x-api-key`, `chatgpt-account-id`, or the prompt body); a 3xx becomes `ProviderStatus` naming the target host.
- Every transport streams (`"stream": true`) and reports a non-2xx response through `status_error`: 401 → `ProviderAuth` naming the profile; 403 → `ProviderStatus` (a refusal for this model, not a bad credential); others → `ProviderStatus { status, detail }` with the body. The ChatGPT Responses flavor keeps its bespoke 401 message (`/connect openai_codex`).
- A truncated stream is an error, not a partial success. Chat Completions: `finish_reason` `length` or any reason other than `stop`/`tool_calls` → `Provider`; an in-stream `error` object → `Provider`; ending without `[DONE]` and without a completed finish → `Transport`. Responses: `response.incomplete` (output-limit cut-off), `error`, or `response.failed` → `Provider`; no `response.completed` → `Transport`. Anthropic Messages: `stop_reason = max_tokens` → `Provider`; no `message_stop` → `Transport`.
- Tool names on the Responses wire: a name that is already valid (non-empty, ≤64 chars, `[A-Za-z0-9_-]`) is sent as-is; any other name is sanitized to `[A-Za-z0-9_-]` on a stem capped at 55 chars and always gets an 8-hex SHA-256 suffix. An alias that collides with another tool's is cut to 51 chars plus a 12-hex SHA-256 suffix. `tool_aliases` is the only mapping used to translate calls back.
- Anthropic Messages requests always send `anthropic-version: 2023-06-01` and `max_tokens: 8192`; any other fixed headers come from the profile's `static_headers`.
- Usage semantics: Anthropic Messages passes `InputTokens::UncachedOnly` to `usage_from_provider`; Chat Completions and Responses pass `InputTokens::Total`.
- `prompt_cache_key` is sent on Chat Completions and Responses when `req.prompt_cache`; Anthropic Messages instead gets `apply_anthropic_prompt_cache` breakpoints. The ChatGPT Responses body never carries inline `cache_control`.
- The image cache is invalidated per entry by `(len, mtime)`; the whole cache is dropped once it reaches 16 entries.
- `prompt_wire` must reflect only tools/system/instructions/messages/input — never model, effort, route, or (after `snapshot_prompt`) `cache_control`.

## Side Effects

- Opens outbound HTTPS connections (`<base>/chat/completions`, `<base>/responses`, `<base>/codex/responses`, `<base>/v1/messages`).
- Reads image files from the workspace (`load_image_ref`); read errors surface as `String` and usually become protocol/transport errors or missing-image notes upstream.
- Mutates in-process state: the route table (`set_route_table`), the image cache, and the process-wide `FALLBACK_OPENCODE_SESSION` `OnceLock` in `mod.rs`.
- `from_config` reads the user provider registry (`forge_connect::loaded_registry()`) and the shared credential handle; requests may refresh OAuth credentials (blocking, off the async runtime) via `forge-connect`.
- `Target::effort_support` and the Responses and Chat Completions thinking-off checks read the user model-catalog cache (`ModelCatalogCache::user_default()`).
- Profiles with `SessionHeaderKind::Opencode` send `x-opencode-session` (the Forge session id, else a process-stable UUID) and a `forge/<version>` user agent. The ChatGPT Responses flavor sends `session-id` (session id, else the per-request id), `x-client-request-id`, `originator: forge`, `User-Agent: forge`, and `accept: text/event-stream`. Both Responses flavors (ChatGPT and gateway) send `OpenAI-Beta: responses=experimental`.

## Verification

- Run: `cargo test --package forge-model --locked`
