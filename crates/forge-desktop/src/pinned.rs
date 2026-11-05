// The pinned content-security policy, shared by the embedded-config check
// (`--self-test`) and the config tests.
//
// No `frame-src` on purpose: with the isolation pattern, Tauri appends its
// per-build isolation scheme (`isolation-<uuid>:`) to `default-src` only, and
// an explicit `frame-src` would override that and block the isolation frame.
// Any other frame falls back to `default-src 'none'` and is blocked.
#[allow(dead_code)] // the release binary does not read it; self-test and tests do
pub const PINNED_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: asset:; font-src 'self'; connect-src ipc: http://ipc.localhost; worker-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'";
