# forge-model native transports

## Overview

Native Rust provider transports. Each module builds a wire request for one
provider protocol, opens an HTTP stream with `reqwest`, parses SSE lines, and
normalizes the result into `forge_types::ModelResponse`. `mod.rs` owns the
shared client, credential/cache plumbing, SSE line splitting, and per-route
dispatch; the provider modules are private (`anthropic`, `codex`, `openai`) and
expose only `pub(super)` seams to it.

## Interface

- `process_sse_lines(pending: &mut String, consume: impl FnMut(&str) -> Result<(), ModelError>) -> Result<(), ModelError>` — splits buffered SSE bytes into complete lines, consumes them, and leaves any trailing partial line in `pending`. Test-only sibling module `test_support` (behind `#[cfg(test)]`) provides `serve_once`.
- `NativeModelClient` — implements `crate::ModelClient`.
  - `from_config(&Config) -> Result<Self, ModelError>` — builds a `reqwest::Client` with `model.request_timeout_secs.max(1)`.
  - `resolved_base_url(&[&str], default: &str) -> String` — configured base URL, else injected/env var, else default.
  - `load_image_ref(&Path, &ImageRef) -> Result<Arc<LoadedImage>, String>` — caches up to `IMAGE_CACHE_MAX_ENTRIES` (16) loaded images keyed by resolved path.
  - Private helpers `model_for`, `credential`, `injected_or_env`.
- `anthropic` (private):
  - `MessagesAuth::{Anthropic{api_key}, Opencode{api_key, session_id, user_agent}}`.
  - `messages_endpoint(base_url) -> String` — `{base}/messages`, inserting `/v1` unless the base already ends in `/v1`.
  - `complete(...)`, `complete_messages(...)`, `messages_body(...)`.
- `codex` (private): `pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api"`; `complete`, `request_body(client, req, model, aliases)`, `CodexStream`, `tool_aliases(req) -> BTreeMap<String, String>`.
- `openai` (private): `complete`, plus internal Responses-API path `complete_responses` and model-specific routing between chat/completions, `/responses`, and Anthropic-style messages.
  - Private `route(client, model)` resolves base URL/key/model for built-in prefixes (`openai`, `xai`/`grok`, `opencode-go`/`opencode`, `opencode-zen`, `ollama`/`ollama_chat`; an unprefixed id is treated as `openai`); any other prefix is accepted only when `forge_connect::loaded_registry().profile_for_model(model)` returns a user-origin (`is_user()`) spec, in which case `custom_route(client, profile, model)` builds the route.
- Route helpers: `spec_for_route(route_id)` (looks the route up in `forge_connect::loaded_registry()`), `transport_for_route(route_id)` (defaults to `OpenaiCompat` when unknown), `canonical_model_for_route(route_id, model)` (rewrites to `{spec.model_provider_prefix}/{bare}`).

## Invariants

- Dispatch is by route spec, never by string matching on the model name: an unknown or absent `route_id` falls back to `OpenaiCompat`, and the model id is prefixed from the registered spec's `model_provider_prefix`.
- An empty `req.model` falls back to `cfg.model.model`; if both are blank the request fails with `ModelError::Other("model id is required")`.
- Custom (user-registered) providers route only through `custom_route`: base URL is the spec's `default_base_url` (missing is a `ModelError::Provider`), the model id is sent with its namespace intact after stripping the provider prefix, the key comes from the spec's `api_key_env` via the normal credential precedence and then from `CredentialStore::user_default().get_api_key(profile.id)`, and `opencode_session` is always false. A non-user spec with an unknown prefix is still rejected as `unsupported native model provider prefix`.
- Credential precedence: injected map → process env → `cfg.model.api_key`. Blank values are filtered out at every step. `clear_provider_env` clears only the injected map, so a configured key becomes visible again.
- Base URL precedence: configured `base_url` → env var → hardcoded default.
- Every transport streams (`"stream": true`) and reports a non-2xx response as `ModelError::ProviderStatus { status, detail }`, never as a decoded body.
- A truncated OpenAI-compatible stream is an error, not a partial success: a `finish_reason` of `length` or any unknown reason, or a stream that ends without `[DONE]` and without a completed finish, returns `ModelError::Provider`/`ModelError::Transport` telling the caller to continue.
- Tool names for the Codex/Responses wire are sanitized to `[A-Za-z0-9_-]`, capped at 64 chars, and disambiguated with a SHA-256-prefixed suffix on collision; `tool_aliases` is the only mapping used to translate calls back.
- Anthropic requests always send `anthropic-version: 2023-06-01` and `max_tokens: 8192`; the beta header `interleaved-thinking-2025-05-14` is sent only on the direct Anthropic route, not the OpenCode gateway.
- Anthropic route semantics: `input_tokens` is the uncached remainder, so `usage_from_provider` is called with `InputTokens::UncachedOnly`; OpenAI-shaped routes pass `InputTokens::Total`.
- The image cache is invalidated per entry by `(len, mtime)`; the whole cache is dropped once it reaches 16 entries.
- `prompt_wire` must reflect only tools/system/instructions/messages/input — never model, effort, route, or `cache_control`.

## Side Effects

- Opens outbound HTTPS connections (Anthropic Messages, Codex `/codex/responses`, OpenCode/OpenAI `/chat/completions` and `/responses`).
- Reads image files from the workspace (`load_image_ref`); read errors surface as `String` and usually become protocol/transport errors or missing-image notes upstream.
- Mutates in-process state: the injected credential map (`apply_provider_env`/`clear_provider_env`), the image cache, and the process-wide `FALLBACK_OPENCODE_SESSION` `OnceLock` in `openai.rs`.
- Reads process environment variables for credentials and base URLs.
- `spec_for_route` and the custom-provider branch of `openai::route` read the user `providers.toml` (via `forge_connect::loaded_registry()`); custom routes may read the `forge-connect` credentials file (read-only).
- Sends `x-opencode-session` (session id or generated process-stable id) and a `forge/<version>` user agent on OpenCode routes.

## Verification

- Run: `cargo test --package forge-model --locked`
