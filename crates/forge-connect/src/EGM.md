# forge-connect src — Connect Profiles, Credentials, Catalog

## Overview

Implements `/connect` and `/model` support: built-in provider definitions
embedded from `providers/*.toml`, auth kinds (API key, key-optional, OAuth
device code in RFC 8628 and ChatGPT flavors), a 0600 credential store plus a
per-process `Credentials` handle, a resolver that snapshots every profile's
route and credential, non-secret preference storage, provider-spec registry
loading, remote model catalogs with on-disk caching, picker projection,
reasoning-effort rules, legacy-id normalization, and usage/cost reporting.
The profile id is the only provider identity and is also the model-id
prefix (`<profile_id>/<model>`).

## Interface

- `lib.rs` `pub use` lists are the public surface below;
  crate-internal `register_builtin_profiles` registers every
  `builtin::builtin_specs()` entry.
- Built-ins (`builtin.rs`, public module): `BUILTIN_PROVIDERS` (`(file
  name, include_str!)` for `anthropic`, `ollama`, `openai`, `opencode`,
  `xai` `.toml`, sorted), `builtin_specs()` (parsed once; seven profiles),
  `builtin_profile(id)` (panics on an unknown id), `profile_ids::{ANTHROPIC,
  OLLAMA, OPENAI, OPENAI_CODEX, OPENCODE_GO, OPENCODE_ZEN, XAI}`.
- Provider specs (`profile.rs`): `ProviderSpec` (`ConnectProfile` is a type
  alias), `SpecOrigin` (`Builtin`, `User`), `ConnectOutcome { profile_id,
  model, key_source, verified }`, `ConnectStatus`, `KeySource` (`Env`,
  `File`, `Provided`, `Oauth`). Schema types: `AuthSpec` (`kind`, `flavor`,
  `issuer`, `client_id`, `scopes`, `prompt`), `AuthKind` (`ApiKey`,
  `ApiKeyOptional`, `OauthDevice`), `OauthFlavor` (`Rfc8628`, `Chatgpt`),
  `AuthPrompt` (`Tui`, `Browser`, `Never`), `WireKind` (`ChatCompletions`,
  `Responses`, `AnthropicMessages`), `WireOverride`, `Attach` (`Bearer`,
  `XApiKey`, `BearerAccountId`, `None`), `StaticHeader`,
  `SessionHeaderKind` (`Opencode`), `CatalogSpec` (`kind`, `filter`),
  `CatalogKind` (`OpenaiModels`, `AnthropicModels`, `OllamaTags`,
  `ChatgptModels`, `Static`), `CatalogFilter` (`Chat`), `VerifySpec`/
  `VerifyKind` (`ModelsProbe`, `None`), `EffortFamily` (`None`, `Openai`,
  `Xai`), `EditableField` (`BaseUrl`). `ProviderSpec` fields: `id`, `title`,
  `description`, `auth`, `api_key_env`, `base_url_env`, `credential_from`,
  `base_url`, `editable`, `wire`, `wire_overrides`, `attach`,
  `static_headers`, `session_header`, `default_models`, `manual_models`,
  `catalog`, `verify`, `models_dev_providers`, `auth_url`, `import_aliases`,
  `guidance`, `min_key_len`, `billing_url`/`billing_note`, `effort_family`,
  `model_provider_prefix`, `provider`, `provider_label`, `label`, `origin`.
  Methods: `default_model`, `has_multiple_routes(registry)`,
  `needs_tui_api_key_prompt`, `rejects_api_key_cli`, `is_user`,
  `effective_wire` (default `chat-completions`), `effective_auth` (default
  TUI-prompted API key), `effective_catalog_kind` (default from wire),
  `wire_for_model(bare)`.
- Auth (`auth.rs`): `OauthTokens` with `needs_refresh(skew)`, `OauthPending`
  (`start`, `start_stub`, `open_url`, `operator_instructions`; carries
  `flavor`).
- Credential store (`store.rs`): `CredentialStore` (`new`, `user_default`,
  `path`, `read_count`, `get_api_key`, `set_api_key`, `get_oauth`,
  `set_oauth`, `clear`, `clear_all`, `is_connected`,
  `needs_authentication(profile_id)`, `list_profile_ids`), `StoreError`.
  Crate-internal: `mark_auth_required`, `resolve_key`, `resolve_connected`,
  `CREDENTIALS_SCHEMA_VERSION`.
- Credentials handle (`credentials.rs`): `Credentials` (`new`,
  `user_default`, `shared() -> Arc<Self>` process-wide, `store`,
  `slot_for(profile)`, `set_session_key`, `set_api_key`, `set_oauth`,
  `is_flagged`, `clear`, `clear_all`, `ensure_fresh_blocking`).
- Resolver (`resolve.rs`): `resolve(registry, &Arc<Credentials>) ->
  RouteTable`; `RouteTable` (`single`, `get`, `profiles`,
  `profile_for_request(model) -> (profile, bare)`, `credential_env_names`);
  `ResolvedProfile { definition, credential }`; `RouteDefinition`
  (`from_spec`, `wire_for_model`; fields incl. `base_url`, `wire`,
  `attach`, `catalog_kind`, `auth_kind`, `oauth_flavor`, `effort_family`);
  `Credential` (`Key`, `OptionalKey`, `Oauth`, `Static`; `current`,
  `current_with_source`, `ensure_fresh_blocking`); `CredentialValue {
  secret, account_id }`; `CredentialError` (`Missing`, `AuthRequired`,
  `Store`); `env_names_for(profile) -> (key names, base-URL names)`;
  `winning_env_var(registry, profile_id)`.
- Normalization (`normalize.rs`): `normalize_saved_model_id` (legacy prefix
  or alias → `<profile_id>/…`), `old_prefix_hint`,
  `profile_id_for_stored_route` (old route ids such as `openai-chatgpt` →
  profile id). Crate-internal: `legacy_prefixes`, `is_reserved_id`,
  `env_profile_id`, `normalize_model(s)`, `wire_from_legacy_transport`.
- Effort (`effort.rs`, public module): `EffortLevel` (`Minimal`…`Max`),
  `EffortSupport`, `effort(def, bare_model, catalog)`,
  `effort_for_model(model)` (resolves the profile via the loaded registry).
- Preferences (`preferences.rs`): `PreferenceStore` — `new`,
  `user_default`, `path`, `last_selection`,
  `set_last_selection`, `last_selection_struct(registry)`, `last_effort`,
  `set_last_effort`, `clear_last_selection`, `previous_selection`,
  `previous_effort`, `record_switch`, `quick_switch`.
- Registry (`registry.rs`): `ConnectRegistry` — `new`, `register`, `profiles`,
  `get` (case-insensitive), `profile_for_model` (id, prefix, or legacy
  prefix), `ids`, `load_warnings`; free functions `builtin_registry`,
  `loaded_registry`, `loaded_registry_from(path)`, `credential_env_names`,
  `save_custom_provider(id, base_url, model, wire) -> Result<ProviderSpec,
  String>`, `update_custom_provider(id, base_url, model, details:
  Option<&ProviderDetails>) -> Result<ProviderSpec, String>`,
  `ProviderDetails { title, label, api_key_env }`,
  `save_manual_model(profile_id, model)`; crate-internal
  `user_providers_path` (`<config_dir>/forge/providers.toml`).
- Service (`service.rs`): `ConnectService { registry, credentials,
  preferences, active_profile_id, active_model }` (`list_lines`, `status`,
  `status_message`, `connect`, `connect_api_key`, `connect_oauth`,
  `start_oauth`, `poll_oauth_once`, `complete_oauth_device_flow`,
  `connect_start_oauth`, `disconnect`, `profile`, `ensure_oauth_fresh`,
  `connected_profiles`, `report_auth_failure`, `report_auth_success`),
  `ConnectAction`, `ConnectError`, `parse_connect_args`,
  `handle_connect_action`, `has_connected_profile`, `format_connected`,
  `needs_tui_api_key_prompt`, `needs_tui_oauth`.
- Catalog (`catalog.rs`): `ModelCatalogCache` (`new`, `user_default`,
  `path`, `with_ttl`, `is_fresh`, `registry_is_fresh`, `get_cached`, `put`,
  `clear_profile`, `get_registry_cached`, `get_registry_cost`,
  `model_limits`, `model_accepts_image_input`, `image_input_ready`,
  `model_effort_options`, `model_supports_thinking_off`, `effort_ready`),
  `CatalogEntry`, `CatalogSource`, `CatalogCost`, `CatalogLimits`,
  `CatalogError` (`is_auth_failure`, `rejected_credential`,
  `is_retryable`), `fetch_remote_models`, `refresh_profile_catalog`,
  `refresh_models_dev_registry`, `models_for_picker`,
  `runnable_models_for_picker`, `DEFAULT_TTL_SECS`, `MODELS_DEV_TTL_SECS`;
  crate-internal `credential_for_catalog`.
- Routes (`route.rs`): `ModelPickerEntry`, `ModelRoute { model_id,
  profile_id, display_id, source }`, `route_model_id`, `group_routes`,
  `normalize_model_id`.
- Selection (`selection.rs`): `ModelSelection { provider, model,
  profile_id, effort }`.
- Discovery/import (`import.rs`): `DiscoveredLogin`, `DiscoveredSecret`,
  `discover_logins`, `discover_logins_in`, `login_for_profile`,
  `xdg_data_home`.
- Cost/usage (`cost.rs`): `provider_cost_report`.
- Atomic writes (`atomic.rs`, crate-internal): `write_atomic(path, bytes,
  mode)` and `write_atomic_checked(.., before_rename)` (temp file in the same
  directory, synced, renamed over the symlink-resolved target); used by the
  credential store, preferences, catalog cache, and registry writers.
- Verification (`verify.rs`): `VerifyError` — typed empty/short/
  unauthorized/rejected/status/malformed/unhealthy/unreachable failures;
  crate-internal probes `probe_openai_models`, `verify_anthropic_key`,
  `verify_ollama_reachable`.
- OAuth: `oauth_dispatch::OauthError` (dispatch by `auth.flavor` is
  crate-internal); `oauth_rfc8628`: `Rfc8628Client`, `Rfc8628Error`,
  `try_open_browser`, `DEFAULT_CLIENT_ID`/`DEFAULT_ISSUER`/`DEFAULT_SCOPES`;
  `oauth_chatgpt` (crate-private module): ChatGPT device login, account-id
  extraction, and usage report.
- Tests: `provider_tests.rs` (cfg(test), per-built-in key checks);
  `tests/builtin_providers.rs`, `tests/legacy_providers.rs`,
  `tests/no_profile_id_branches.rs`.

## Invariants

- Built-in provider behavior lives in `providers/*.toml`; there are no
  per-provider modules or `*_profile()` helpers. A malformed embedded file
  panics at first parse (caught by `tests/builtin_providers.rs`).
- Guard test `tests/no_profile_id_branches.rs` fails on any string literal
  equal to a built-in id (or starting with `<id>/`) in non-test production
  code across every crate's `src/`. It skips any file named `builtin.rs` or
  `normalize.rs` (in every crate), `*tests.rs` files, any directory named
  `tests` or `providers`, files that fail to parse, macro bodies, and
  `cfg(test)` on mod/fn/impl/const/static/struct/use items only.
- The profile id is the model prefix: on load `model_provider_prefix` is set
  to the id (a custom entry's differing older value is kept as
  `legacy_prefix` for normalization only). `RouteTable::profile_for_request`
  routes by the prefix alone. There are no route ids; old route ids, old
  prefixes, and aliases are read-only via `normalize.rs`.
- Writers emit only the current schema: `base_url`, `provider`,
  `provider_label`, `label`. Old names (`default_base_url`, `vendor_id`,
  `vendor_label`, `route_label`) are read-only aliases; legacy `auth_mode`
  and `transport` are read (mapped into `auth` / `wire` on load) and never
  written; `model_provider_prefix` and `origin` are `skip_serializing`.
- Credential storage is a TOML file at the platform config dir, written with
  mode `0600`; reads reject more-open permissions and refuse schema versions
  newer than `CREDENTIALS_SCHEMA_VERSION`.
- Only successful credential parses are cached; cache identity is the file's
  modification time plus length. Permission/schema errors are re-reported on
  every read, never latched.
- Secrets never appear in status/display APIs, error messages, debug output
  (`CredentialValue`, `Credential`, `Credentials` `Debug` redact), or
  discovered-login `Debug` implementations.
- Credential slots: `Credentials::slot_for` is the `credential_from` target
  else the profile id, so `opencode_zen` reads and writes `opencode_go`'s
  slot; `clear` removes both the target slot and the profile's own old
  slot. Session keys (typed in the TUI) are per-process, never written.
- Resolver precedence (first non-blank, trimmed): API key = session key →
  `FORGE_<ID>_API_KEY` → `credential_from` target's generated name →
  `api_key_env` → stored key (target slot, then own slot); OAuth =
  `api_key_env` → stored token (fixture tokens read as absent); base URL =
  `FORGE_<ID>_BASE_URL` → `base_url_env` → user override → definition.
  A flagged stored credential is `CredentialError::AuthRequired`; a missing
  key on a key-optional profile is `Ok(None)`. `Credential::current` does
  no network I/O; `ensure_fresh_blocking` refreshes OAuth tokens expiring
  within five minutes, single-flight per profile.
- `RouteDefinition` applies the catalog kind's request-base convention:
  Ollama bases end in `/v1`, Anthropic bases are host-only. `attach`
  defaults to `x-api-key` for `anthropic-messages`, else bearer.
- `auth_required` is a persisted per-profile (per-slot) flag in the
  credentials file: a flagged profile keeps its stored key/tokens but
  `is_connected` is false and resolution yields none across restarts.
  `set_api_key`, `set_oauth`, and `clear` remove the flag; `clear_all`
  empties it. Only `report_auth_failure`/`report_auth_success` write it.
- `connect_api_key` rejects OAuth profiles; a key-optional profile (Ollama)
  connects without a key. Before saving, `verify_profile_key` checks
  `min_key_len`, then probes by catalog kind (`openai-models` →
  `GET <base>/models`, `anthropic-models` → `verify_anthropic_key`,
  `ollama-tags` → `verify_ollama_reachable`; ChatGPT/static/`verify.kind =
  none`/OAuth skip). An auth-failure result saves nothing and, on 401 or
  `Unauthorized`, flags the profile; any other failure still saves a
  provided key with `ConnectOutcome.verified = false`. A verified connect
  clears the flag. `FORGE_CONNECT_SKIP_VERIFY` skips the network probe.
- Connecting (API key or OAuth) stores credentials only: it does not select a
  model, write `last_selection`, or refresh the catalog. `ConnectOutcome.model`
  is a non-authoritative hint (profile default, then first cached model, then
  `<prefix>/default`), and `format_connected` tells the user to choose a model
  (appending "not verified" when applicable).
- `poll_oauth_once` returns `Ok(None)` for any stub session (`client_id ==
  "stub"`); `complete_oauth_device_flow` errors on one. OAuth flows dispatch
  on `auth.flavor` (`rfc8628` or `chatgpt`), never on profile id.
- Preferences live in a separate non-sensitive file with no permission gate and
  no parse cache. `set_last_selection`, `last_selection`, and
  `previous_selection` normalize saved model ids to the current prefix;
  `record_switch` and `quick_switch` store and return ids as given (trimmed
  by `record_switch` only).
- Built-in registry has exactly seven profiles. User `providers.toml` entries
  only add providers: each sets only custom-allowed keys, uses
  `chat-completions` or `anthropic-messages` wire, `api-key` or
  `api-key-optional` auth (no OAuth, no `credential_from`), a checked
  `base_url`, valid `api_key_env` names (uppercase, `_KEY`/`_TOKEN`, not a
  built-in or `FORGE_*_API_KEY` name), and must not reuse an existing or
  reserved id (`normalize.rs`), env id, or prefix. Every profile sharing a
  `provider` must use the same `provider_label`. A custom entry's
  `default_models` are moved into `manual_models`. Invalid entries,
  unreadable files, and malformed TOML become `load_warnings` and never hide
  valid providers. `[manual_models]` extends
  any profile's `manual_models`; `[provider_overrides.<id>] base_url`
  applies to user specs and to built-ins whose `editable` lists `base_url`
  (Ollama); stale overrides warn. Model lists are normalized on load: one
  own/legacy prefix is removed and the profile id re-added, duplicates
  dropped.
- `save_custom_provider` validates wire, id (`[a-z0-9_-]+`, not reserved),
  base URL (absolute http/https, no userinfo, query, fragment, or
  backslash), and duplicates before writing; it appends to `providers.toml`
  without rewriting existing bytes, refuses layouts that cannot accept an
  array-table append, aborts if the file changed while saving, and never
  writes secrets. The model is stored as a manual model, namespaced
  `<id>/<model>` unless already so.
- `update_custom_provider` edits user-origin providers and built-ins with
  editable `base_url` (base URL plus an optional manual model). With
  `details` (custom providers only) it also replaces `title`, `label`, and
  `api_key_env`; it checks the env names' shape and the loader's
  built-in/`FORGE_*_API_KEY` reservation before any write;
  `save_manual_model` rejects empty, `/`-terminated, or control-character
  ids and namespaces with the profile's prefix.
- `CatalogSource::is_runnable` is true only for `Live`, `Cached`, and
  `Configured`; registry/default rows are metadata or fallbacks, not proof of
  entitlement.
- A model missing from `registry_image_input` is treated as not accepting image
  input (fail-closed).
- Catalog fetch is chosen by `effective_catalog_kind`, never profile id.
  `openai-models` lists `{base}/models` with a bearer key (none needed for
  key-optional; otherwise a missing key is `CatalogError::CredentialRequired`);
  ids are prefixed `<prefix>/` unless already so, so `org/model` survives.
  Picker merge per profile: live (or cached on failure), models.dev rows when
  `models_dev_providers` is set (not for `chatgpt-models`), `default_models`
  only when nothing else came back (`Configured` for static catalogs), and
  `manual_models` always as `Configured`.
- models.dev rows are stamped under each profile's own prefix from its
  `models_dev_providers`; there is no cross-profile alias.
- Catalog cache file carries `version = 2`; a file with another version
  loads empty.
- A catalog HTTP 401 (`rejected_credential`) marks a credential rejected; a
  403 refuses the request, not the credential.
- Catalog GETs follow redirects only within the same scheme and authority;
  a cross-origin redirect is refused so credentials are never forwarded.
  Non-2xx responses report status without echoing the response body.
  Verification and models.dev requests never follow redirects.
- `refresh_profile_catalog` caches whatever a successful fetch returns,
  including an empty list; a failed fetch leaves the existing cache intact.
- `normalize_model_id` never truncates an already-namespaced id and prefixes
  with the default only when no slash is present.
- `profile_id_for_stored_route` returns unknown ids unchanged.
- Discovery is read-only: `import` reports candidate logins (mapped to
  built-ins by `import_aliases`) and never adopts one without an explicit
  caller import.

## Side Effects

- Reads and writes `credentials.toml` (creating parent directories, enforcing
  0600; includes the `auth_required` set), `preferences.toml`, and the model
  catalog cache `<config_dir>/forge/model-catalog.toml`.
- Reads user `providers.toml`; appends custom providers and rewrites the
  `[manual_models]` / `[provider_overrides.<id>]` sections for manual models
  and endpoint edits (atomic checked write, re-checked against the original
  before the swap). A details edit cuts the provider's `[[providers]]` entry
  and re-appends a rewritten copy at the end of the file (comments inside it
  are lost), converting legacy keys (`transport`, `auth_mode`, `route_id`,
  `catalog_mode`, old field names) to the current schema.
- `ConnectService::disconnect` also clears the saved last selection in
  `preferences.toml` when it names the disconnected profile.
- Reads environment variables for API keys (`FORGE_<ID>_API_KEY`,
  `api_key_env`), base URLs (`FORGE_<ID>_BASE_URL`, `base_url_env`), OAuth
  stub/fixture switches, `FORGE_CONNECT_SKIP_VERIFY`, and the models.dev URL
  override (`FORGE_MODELS_DEV_URL`).
- Network I/O: provider model catalogs, models.dev, credential verification
  probes, OAuth device-code/token/refresh endpoints, and ChatGPT usage
  reporting.
- Opens a browser (`auth.prompt = browser`) or prints device-code
  instructions for OAuth.
- Reads other CLIs' local auth files during login discovery, and the Codex
  CLI model cache for `chatgpt-models` profiles.

## Verification

- Run: `cargo test --package forge-connect --locked`
