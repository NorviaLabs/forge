// Self-test builds only (`vite build --mode self-test`): the page's half of
// `forge-desktop --self-test`. It drives one turn, probes the security
// boundary from inside the engine, and reports by navigating to
// `/?selftest=<json>`; Rust adds its own checks and decides.
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(predicate, ms) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await sleep(50);
  }
  return false;
}

// A call counts as blocked if it rejects (the ACL refused it) or never
// settles (the isolation hook dropped it).
async function blocked(call, ms = 1500) {
  return Promise.race([
    call().then(() => false, () => true),
    sleep(ms).then(() => true),
  ]);
}

export async function runSelfTest(params) {
  const violations = [];
  document.addEventListener("securitypolicyviolation", (event) => {
    violations.push({ directive: event.effectiveDirective, uri: event.blockedURI });
  });
  const results = {};

  // 1. The journey: a prompt with markdown and hostile HTML goes through
  //    IPC, the turn completes, and the transcript renders it safely.
  await invoke("submit_prompt", {
    text: 'self-test **ping** <script>window.__forge_xss=1</script> <iframe src="https://example.invalid/x"></iframe>',
  });
  const rendered = await waitFor(
    () =>
      document.querySelector('[data-role="user"] strong')?.textContent === "ping" &&
      document.querySelector('[data-role="assistant"]') !== null,
    20000,
  );
  results.rendered_markdown =
    rendered &&
    window.__forge_xss === undefined &&
    document.querySelector(".transcript iframe, .transcript script") === null;

  // 2. Commands outside the allowlist, and an event the page may not emit.
  results.unknown_command_blocked = await blocked(() => invoke("not_a_forge_command"));
  results.emit_blocked = await blocked(() => emit(params.emit_probe_event, { probe: true }));

  // 3. Runtime proof of the hardening (not just the config files).
  results.no_global_tauri = window.__TAURI__ === undefined;
  results.prototype_frozen = Object.isFrozen(Object.prototype);
  const script = document.createElement("script");
  script.textContent = "window.__forge_inline_ran = true";
  document.body.append(script);
  try {
    (0, eval)("window.__forge_eval_ran = true");
    results.eval_blocked = false;
  } catch {
    results.eval_blocked = window.__forge_eval_ran === undefined;
  }
  const frame = document.createElement("iframe");
  frame.src = "https://example.invalid/injected";
  document.body.append(frame);

  // 4. The egress proxy is a live loopback listener; the page must not reach it.
  const port = params.egress_port;
  results.egress_fetch_blocked =
    typeof port === "number" &&
    (await blocked(() => fetch(`http://127.0.0.1:${port}/`)));
  try {
    const socket = new WebSocket(`ws://127.0.0.1:${port}/`);
    await sleep(500);
    results.egress_websocket_blocked = socket.readyState !== WebSocket.OPEN;
  } catch {
    results.egress_websocket_blocked = true;
  }

  await sleep(500);
  results.inline_script_blocked =
    window.__forge_inline_ran === undefined &&
    violations.some((v) => v.directive.startsWith("script-src"));
  results.iframe_blocked = violations.some(
    (v) => v.directive === "frame-src" || v.uri.includes("example.invalid/injected"),
  );
  if (typeof port === "number") {
    const hit = (scheme) => violations.some((v) => v.uri.startsWith(`${scheme}://127.0.0.1:${port}`));
    results.egress_fetch_blocked = results.egress_fetch_blocked && hit("http");
    results.egress_websocket_blocked = results.egress_websocket_blocked && hit("ws");
  }

  location.replace(`/?selftest=${encodeURIComponent(JSON.stringify(results))}`);
}
