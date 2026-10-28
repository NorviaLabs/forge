# forge-model source

## Overview

The model layer: a provider-agnostic `ModelClient` trait, a copy-on-write
transcript type shared with the session, wire normalization between Forge
messages and OpenAI-shaped JSON, prompt-wire snapshotting for cache-prefix
diagnostics, prompt-cache breakpoints/usage parsing, image attachment
freezing/loading, and the native/mock transports. Routing comes from
`forge-connect`: the native client resolves each request through a
`forge_connect::RouteTable` keyed by the model id's provider prefix.

## Interface

Submodules are private. The crate-external surface is the items defined in
`lib.rs` plus its re-exports (`RouteTable` from `forge_connect`,
`apply_missing_image_notes`, `freeze_attachments`, `MockModelClient`,
`NativeModelClient`, `complete_result_from_value`, `forge_messages_to_wire`,
`tools_to_openai_functions`, `common_prefix_len`, `first_json_pointer`,
`part_pointer_at`, `prompt_wire`, `snapshot_prompt`, `strip_cache_control`,
`PromptSnapshot`, `PromptTransport`). Other `pub`/`pub(crate)` items listed
under the module bullets below are crate-internal seams used by `native/`.

- `ModelError` (`#[non_exhaustive]`): `Http`, `Provider`, `ProviderStatus{status, detail}`, `ProviderAuth{status, message}`, `MissingApiKey`, `UnknownRoute` (no profile serves the model id's prefix), `Transport`, `Protocol`, `Other`; `status() -> Option<u16>`, `is_auth_failure()`, `is_rate_limited()`, `is_retryable()`.
- `SharedMessages` — `Arc`-backed `Vec<Message>` with `Deref`/`DerefMut`, `Clone` (O(1)), `Default`, `shared()`, `shares_storage_with(&Self) -> bool`, `storage_id() -> u64`, `From<Vec<Message>>`, `IntoIterator for &SharedMessages`.
- `ModelRequest { messages, tools, model, workspace_root, session_id, reasoning_effort, thinking_enabled, prompt_cache }` — no route or profile field; `model` (`<provider>/<model>`) selects the profile.
- `ModelClient` trait (`Send + Sync`): required `complete`; defaulted `complete_with_stream(req, Option<StreamEventTx>)`, `set_route_table(RouteTable)` (no-op), `prompt_wire(&ModelRequest) -> Value` (OpenAI-compat projection), `prompt_transport_key(&ModelRequest) -> &'static str` (`"mock"`).
- `StreamEventTx = std::sync::mpsc::Sender<ModelStreamEvent>`.
- `client_from_config(&Config) -> Result<Box<dyn ModelClient>, ModelError>` — `cfg.model.mock` → scripted `MockModelClient`; otherwise `NativeModelClient::from_config`.
- `normalize`: `complete_result_from_value`, `forge_messages_to_wire`, `tools_to_openai_functions` (re-exported); `forge_messages_to_wire_in(messages, workspace)` and `forge_messages_to_wire_in_cached(messages, workspace, client)` (crate-internal; the latter loads images through the native client's cache).
- `prompt_wire`: `PromptTransport{OpenaiCompat, Anthropic, Codex, Mock}` with `as_str()` (`"openai_compat"`, `"anthropic_messages"`, `"codex"`, `"mock"`); `PromptSnapshot{bytes, sha256}`; `prompt_wire`, `strip_cache_control`, `snapshot_prompt`, `common_prefix_len`, `part_pointer_at`, `first_json_pointer` (re-exported); `prompt_object_from_body`, `openai_compat_prompt`, `openai_compat_prompt_cached` (crate-internal).
- `prompt_cache` (crate-internal): `InputTokens{Total, UncachedOnly}`, `usage_from_provider(raw, input, completion, semantics) -> Usage`, `cache_tokens_from_usage(&Value) -> (u32, u32)`, `prompt_cache_key(&Path) -> String`, `apply_anthropic_prompt_cache(&mut Value)` (adds Anthropic `cache_control` breakpoints to a request body).
- `image`: crate-internal `LoadedImage{mime, bytes}`, `resolve_image_path`, `load_image_ref`, and per-wire image-part builders (`openai_`, `anthropic_`, `codex_input_image_part_with_loaded`); re-exported `freeze_attachments(workspace, cache_dir, content, attachments) -> Vec<ImageRef>`, `apply_missing_image_notes(&mut [Message], workspace)`.
- `mock`: `MockModelClient::{script, stream_error, stream_error_kind, stream_error_then, last_request}`.
- Subdirectory: [native](./native/EGM.md) — native wire-protocol transports and per-profile dispatch.

## Invariants

- `SharedMessages` mutation is copy-on-write: cloning is O(1), and a `DerefMut` while another copy exists clones the storage and assigns a fresh `storage_id`. `storage_id` is therefore a valid identity for a specific transcript allocation.
- `ModelError::ProviderStatus` `Display` is byte-identical to `"provider error: HTTP {StatusCode}: {detail}"`; `ProviderAuth` keeps its custom message verbatim. Status classification must never depend on parsing text.
- `is_auth_failure` covers `MissingApiKey`, `ProviderAuth`, and `ProviderStatus` 401 only — a 403 refuses the request, not the credential. `is_retryable` covers `Transport`, 429, and ≥500 — a rejected credential is never retryable.
- `complete_result_from_value` accepts both an already-normalized Forge shape (`text`/`tool_calls` keys) and a raw OpenAI chat completion (`choices[0].message`); the Forge shape wins when present.
- `snapshot_prompt` encodes `tools`, then system/instructions, then each message as one JSON document per line, and strips `cache_control` at any depth before hashing. `part_pointer_at` relies on that line-per-part encoding.
- The free `prompt_wire(req, transport)` returns the OpenAI-compat shape for every transport; the exact Anthropic/Responses prompt comes only from the native client's `ModelClient::prompt_wire`, which projects the body it would send via `prompt_object_from_body` (only tools/system/instructions/messages/input survive).
- `InputTokens::UncachedOnly` adds cache reads and writes back into `prompt_tokens` so `Usage::prompt_tokens` always means total input tokens; `Total` takes the provider number as-is.
- `prompt_cache_key` hashes the workspace path with SHA-256 and returns `forge-` plus hex, truncated to 24 chars total, so no directory name leaves the machine and every session in a workspace shares one cache key.
- `freeze_attachments` is meant to run once at message insert; unreadable attachments are dropped and replaced by an ``image at `<path>` is no longer available`` note appended to `content`.
- `client_from_config` returns the mock only when `cfg.model.mock` is set and never silently downgrades a native config to the mock.

## Side Effects

- Reads image bytes from disk and writes frozen copies into a caller-supplied cache directory (`freeze_attachments`).
- The default `complete_with_stream` emits `ThinkingDelta`, `TextDelta`, and `MessageEnd` on the channel and ignores send failures.
- Allocates monotonically increasing storage ids from a process-global atomic.
- `NativeModelClient::from_config` builds an HTTP client (and thus a connection pool) and resolves an initial route table from the user provider registry and shared credentials; it sends no network requests.

## Verification

- Run: `cargo test --package forge-model --locked`
