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

const turnState = () => document.querySelector("main")?.dataset.turnState;
const said = (text) =>
  [...document.querySelectorAll('[data-role="assistant"]')].some(
    (message) => message.textContent.trim() === text,
  );
const approvalFor = (callId) => document.querySelector(`.approval[data-call-id="${callId}"]`);
const button = (root, label) =>
  [...(root?.querySelectorAll("button") ?? [])].find((b) => b.textContent.trim() === label);

// Type into the composer and press Send, as a user would.
async function send(text) {
  const box = document.querySelector('textarea[aria-label="Prompt"]');
  const form = box.closest("form");
  box.value = text;
  box.dispatchEvent(new Event("input", { bubbles: true }));
  await sleep(50);
  button(form, "Send").click();
}

// Answer the scripted approval `callId` with the button `label`, then wait
// for the turn to finish with the scripted `answer`.
async function decide(prompt, callId, label, answer) {
  await send(prompt);
  if (!(await waitFor(() => approvalFor(callId), 20000))) return false;
  button(approvalFor(callId), label).click();
  return waitFor(
    () => turnState() === "completed" && said(answer) && approvalFor(callId) === null,
    20000,
  );
}

// The scripted model holds its answer mid-stream; Stop must end the turn
// without it.
async function stopDuringHold() {
  await send("stop journey");
  const streaming = () =>
    document.querySelector('[data-role="assistant-streaming"]')?.textContent.includes("working on it");
  if (!(await waitFor(() => streaming() && turnState() === "running", 20000))) return false;
  await sleep(300);
  if (turnState() !== "running") return false;
  button(document.querySelector("textarea[aria-label=\"Prompt\"]").closest("form"), "Stop").click();
  const stopped = await waitFor(() => ["cancelled", "interrupted"].includes(turnState()), 20000);
  await sleep(300);
  return stopped && !said("held answer");
}

export async function runSelfTest(params) {
  const violations = [];
  document.addEventListener("securitypolicyviolation", (event) => {
    violations.push({ directive: event.effectiveDirective, uri: event.blockedURI });
  });
  const results = {};

  // A real shell must keep process state and its original emulator across
  // session selection. Closing the extra session releases its egress proxy.
  results.shell_survives_switch = false;
  const activeSession = () => document.querySelector('.session-list li.active [data-session-id]')?.dataset.sessionId;
  const primary = activeSession();
  let extra = null;
  try {
    document.querySelector('button[aria-label="Terminal"]').click();
    await waitFor(() => document.querySelector(`.terminal[data-session="${primary}"] .xterm`), 10000);
    const original = document.querySelector(`.terminal[data-session="${primary}"]`);
    // shell_open returns after spawn; wait for input to become available.
    await sleep(300);
    await invoke("shell_write", { sessionId: primary, data: "FORGE_DESKTOP_CHECK=42\n" });
    await invoke("create_session", { label: "terminal self-test" });
    await waitFor(() => activeSession() && activeSession() !== primary, 15000);
    extra = activeSession();
    if (extra === primary) throw new Error("Session creation failed");
    await invoke("shell_write", { sessionId: primary, data: 'printf "forge-shell-%s\\n" "$FORGE_DESKTOP_CHECK"\n' });
    await invoke("select_session", { sessionId: primary });
    await waitFor(() => activeSession() === primary, 10000);
    document.querySelector('button[aria-label="Terminal"]').click();
    results.shell_survives_switch = await waitFor(() => original === document.querySelector(`.terminal[data-session="${primary}"]`) && original.textContent.includes("forge-shell-42"), 10000);
    document.querySelector('.tab.active button[aria-label="Close tab"]').click();
  } catch (error) { console.error("terminal self-test", error); }
  finally {
    if (extra && extra !== primary) await invoke("close_session", { sessionId: extra });
  }

  results.editor_navigation_and_reload = false;
  try {
    await waitFor(() => document.querySelector('.tree button.tree-row[title="README.md"]'), 10000);
    document.querySelector('.tree button.tree-row[title="README.md"]').click();
    await waitFor(() => document.querySelector('textarea[aria-label="Edit README.md"]'), 10000);
    const editor = document.querySelector('textarea[aria-label="Edit README.md"]');
    editor.value = "first\nFind ME\nlast\n";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await sleep(50);
    const key = (element, value, modifiers = {}) => element.dispatchEvent(new KeyboardEvent("keydown", { key: value, bubbles: true, cancelable: true, ...modifiers }));
    key(editor, "f", { metaKey: true });
    await sleep(50);
    const query = document.querySelector('input[aria-label="Find in file"]');
    query.value = "find me";
    query.dispatchEvent(new Event("input", { bubbles: true }));
    await sleep(50);
    key(query, "Enter");
    const found = editor.selectionStart === 6 && editor.selectionEnd === 13;
    const line = document.querySelector('input[aria-label="Line number"]');
    line.value = "3";
    line.dispatchEvent(new Event("input", { bubbles: true }));
    await sleep(50);
    key(line, "Enter");
    const jumped = editor.selectionStart === 14;
    const file = await invoke("read_file", { path: "README.md" });
    await invoke("write_file", { path: "README.md", text: "external change\n", fingerprint: file.fingerprint, crlf: file.crlf });
    key(editor, "s", { metaKey: true });
    await waitFor(() => document.querySelector(".file-pane .banner"), 5000);
    key(window, "k", { metaKey: true });
    await waitFor(() => document.querySelector('input[aria-label="Command palette"]'), 5000);
    const palette = document.querySelector('input[aria-label="Command palette"]');
    palette.value = "Reload current file from disk";
    palette.dispatchEvent(new Event("input", { bubbles: true }));
    await sleep(50);
    key(palette, "Enter");
    await sleep(50);
    const guarded = editor.value === "first\nFind ME\nlast\n";
    button(document.querySelector(".file-action-dialog"), "Discard edits and reload").click();
    const reloaded = await waitFor(() => editor.value === "external change\n", 5000);
    const buttonless = document.querySelectorAll(".file-pane button").length === 0;
    results.editor_navigation_and_reload = found && jumped && guarded && reloaded && buttonless;
    results.editor_debug = JSON.stringify({ found, jumped, guarded, reloaded, buttonless });
    document.querySelector('.tab.active button[aria-label="Close tab"]').click();
  } catch (error) { results.editor_debug = `${error.message}\n${error.stack || ""}`; }

  results.theme_switches = false;
  try {
    button(document, "Help").click();
    await sleep(50);
    const select = document.querySelector(".help select");
    const previous = select.value;
    select.value = "forge-light";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await sleep(50);
    results.theme_switches = document.documentElement.dataset.theme === "light" && getComputedStyle(document.body).color === "rgb(32, 41, 54)";
    select.value = previous;
    select.dispatchEvent(new Event("change", { bubbles: true }));
    document.querySelector('button[aria-label="Close help"]').click();
  } catch (error) { console.error("theme self-test", error); }

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

  // 2. The approval journey: approve once, deny, then Stop during a held
  //    model call. Rust checks the effects on disk independently.
  results.approved_turn_completed = await decide("approve journey", "approve-me", "Run once", "after approve");
  results.denied_turn_completed = await decide("deny journey", "deny-me", "Don't run", "after deny");
  results.stopped_during_hold = await stopDuringHold();

  // 3. Commands outside the allowlist, and an event the page may not emit.
  results.unknown_command_blocked = await blocked(() => invoke("not_a_forge_command"));
  results.emit_blocked = await blocked(() => emit(params.emit_probe_event, { probe: true }));

  // 4. Runtime proof of the hardening (not just the config files).
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

  // 5. The egress proxy is a live loopback listener; the page must not reach it.
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
