import { test } from "node:test";
import assert from "node:assert/strict";
import {
  browserStrip, canInsertResult, confirmText, mergeSettings, paletteItems, riskLabel, runningAppTask,
  screenStrip, settingsModel, taskDetail, tookBackScreen,
} from "./computer.js";
import { CAPABILITIES } from "./agents.js";

const browser = { browser: "Google Chrome", url: "http://localhost:5173/signup", title: "Sign up", tabs: 1 };

test("the browser strip names the browser, the page and the tabs, without a dot", () => {
  assert.equal(browserStrip({ browser: null, controlling_screen: false }), null);
  assert.equal(browserStrip(null), null);
  const strip = browserStrip({ browser, controlling_screen: false });
  assert.equal(strip.title, "Browser open");
  assert.equal(strip.detail, "Google Chrome · localhost:5173/signup · 1 tab");
  assert.equal(strip.dot, null, "an open browser is a fact, not a state");
  const two = browserStrip({ browser: { ...browser, tabs: 2, url: "https://example.com/" }, controlling_screen: false });
  assert.equal(two.detail, "Google Chrome · example.com · 2 tabs");
});

test("the screen strip follows the session state in dot and word", () => {
  assert.equal(screenStrip({ browser: null, controlling_screen: false }, "running"), null);
  const running = screenStrip({ browser: null, controlling_screen: true }, "running");
  assert.deepEqual(
    { title: running.title, dot: running.dot, word: running.word },
    { title: "Controlling your screen", dot: "running", word: "running" },
  );
  assert.match(running.hint, /move the mouse/);
  const waiting = screenStrip({ browser: null, controlling_screen: true }, "waiting");
  assert.equal(waiting.dot, "waiting");
  assert.equal(waiting.word, "waiting for your approval");
  assert.equal(screenStrip({ controlling_screen: true }, "idle").dot, "idle");
});

test("the takeover note shows after the user took the screen back, until control resumes", () => {
  const tool = (name, text) => ({ role: "tool", name, text });
  assert.equal(tookBackScreen([]), null);
  assert.equal(tookBackScreen([tool("desktop_click", "(1, 2) left")]), null);
  const taken = [tool("desktop_click", "(1, 2) left"), tool("desktop_click", "stopped: you moved the mouse")];
  assert.equal(tookBackScreen(taken), "You took back the screen. The agent will ask again before it uses it.");
  assert.equal(tookBackScreen([...taken, tool("bash", "ok")]), tookBackScreen(taken), "other tools do not clear it");
  assert.equal(tookBackScreen([...taken, tool("desktop_screenshot", "2560 × 1440 → 1280 × 720")]), null);
});

test("the settings section disables what cannot apply and says why", () => {
  const view = {
    settings: { browser: { enabled: true, executable: null, headless: false }, desktop: { enabled: false } },
    browsers: [{ name: "Google Chrome", path: "/usr/bin/google-chrome" }, { name: "Brave", path: "/usr/bin/brave-browser" }],
    browser_problem: null,
    desktop_unsupported: null,
  };
  const model = settingsModel(view);
  assert.equal(model.browserSubrowsDisabled, false);
  assert.deepEqual(model.browserOptions.map((o) => o.label), ["Google Chrome", "Brave"]);
  assert.equal(model.selectedBrowser, "/usr/bin/google-chrome", "the first found when none is chosen");
  assert.equal(model.browserPath, "/usr/bin/google-chrome");
  assert.equal(model.showWindow, true);
  assert.equal(model.desktopDisabled, false);
  assert.match(model.desktopNote, /after you allow it for each turn/);

  const off = settingsModel({ ...view, settings: { ...view.settings, browser: { enabled: false, executable: null, headless: true } } });
  assert.equal(off.browserSubrowsDisabled, true);
  assert.equal(off.showWindow, false);

  const none = settingsModel({ ...view, browsers: [], browser_problem: "No Chrome, Chromium, Brave, Edge or Vivaldi found.", desktop_unsupported: "Needs an X11 session. This is a Wayland session." });
  assert.equal(none.browserProblem, "No Chrome, Chromium, Brave, Edge or Vivaldi found.");
  assert.equal(none.desktopDisabled, true);
  assert.equal(none.desktopNote, "Needs an X11 session. This is a Wayland session.");

  const chosen = settingsModel({ ...view, settings: { ...view.settings, browser: { enabled: true, executable: "/opt/vivaldi/vivaldi", headless: false } } });
  assert.equal(chosen.selectedBrowser, "/opt/vivaldi/vivaldi");
  assert.ok(chosen.browserOptions.some((o) => o.value === "/opt/vivaldi/vivaldi"), "a chosen program is listed");
});

test("consent cards say what they are, and the screen grant asks to confirm in words", () => {
  assert.equal(riskLabel("screen_control"), "Screen control");
  assert.equal(riskLabel("form_submit"), "Form submit");
  assert.equal(riskLabel("sandbox_escalation"), "Sandbox escalation");
  assert.equal(riskLabel("network_host"), "Network access");
  assert.equal(riskLabel("governance"), "");
  assert.match(confirmText("allow_consent_for_session", "screen_control"), /anything you can do on this computer/);
  assert.match(confirmText("allow_host_for_session", "network_host"), /network access/);
  assert.match(confirmText("approve_all", "governance", "devbox"), /run on devbox/);
});

test("the palette offers stop and close only when there is something to stop or close", () => {
  assert.deepEqual(paletteItems({ browser: null, controlling_screen: false }), []);
  assert.deepEqual(
    paletteItems({ browser, controlling_screen: true }).map((item) => [item.id, item.label]),
    [["computer:stop-screen", "Stop screen control"], ["computer:close-browser", "Close browser"]],
  );
});

test("custom agents can be given the browser and the desktop", () => {
  const ids = CAPABILITIES.map((capability) => capability.id);
  assert.deepEqual(ids.slice(-2), ["browser", "desktop"]);
  const desktop = CAPABILITIES.find((capability) => capability.id === "desktop");
  assert.match(desktop.hint, /you allow it/);
});

const webView = (changes = {}) => ({
  settings: {
    browser: { enabled: false, executable: null, headless: false },
    desktop: { enabled: false },
    web_check: { enabled: true },
  },
  browsers: [{ name: "Google Chrome", path: "/usr/bin/google-chrome" }],
  browser_problem: null,
  desktop_unsupported: null,
  web_check_unsupported: null,
  ...changes,
});

test("web check is on by default, and the browser row serves both features", () => {
  const model = settingsModel(webView());
  assert.equal(model.webCheckEnabled, true);
  assert.equal(model.webCheckDisabled, false);
  assert.match(model.webCheckNote, /reaches only that app/);
  assert.match(model.webCheckNote, /doesn't ask first/);
  assert.equal(model.browserRowDisabled, false, "the program can be chosen with browser control off");
  assert.equal(model.browserSubrowsDisabled, true, "show window follows browser control");

  const off = settingsModel(webView({ settings: { ...webView().settings, web_check: { enabled: false } } }));
  assert.equal(off.webCheckEnabled, false);
  assert.equal(off.webCheckDisabled, false);
});

test("with no browser both features are off and say what they need", () => {
  const none = settingsModel(webView({
    settings: { ...webView().settings, browser: { enabled: true, executable: null, headless: false } },
    browsers: [],
    browser_problem: "No Chrome, Chromium, Brave, Edge or Vivaldi found.",
  }));
  assert.equal(none.browserRowDisabled, false, "a program can still be chosen");
  assert.equal(none.webCheckDisabled, true);
  assert.equal(none.webCheckEnabled, false, "shown unchecked; the saved setting is kept");
  assert.equal(none.webCheckNote, "Needs a browser. Choose one above.");
  assert.equal(none.browserControlDisabled, true);
  assert.equal(none.browserEnabled, false);
  assert.equal(none.browserNote, "Needs a browser. Choose one above.");
  assert.equal(none.browserSubrowsDisabled, true);
});

test("a machine that cannot run web check says why", () => {
  const model = settingsModel(webView({ web_check_unsupported: "Needs socat, which relays the app out of the sandbox." }));
  assert.equal(model.webCheckDisabled, true);
  assert.equal(model.webCheckEnabled, false);
  assert.equal(model.webCheckNote, "Needs socat, which relays the app out of the sandbox.");
  assert.equal(model.browserControlDisabled, false, "browser control does not need socat");
  assert.match(model.browserNote, /Form submits ask first/);
});

test("saving one switch keeps the others, web check included", () => {
  const settings = webView({ settings: { ...webView().settings, web_check: { enabled: false } } }).settings;
  const browserOn = mergeSettings(settings, { browser: { enabled: true } });
  assert.deepEqual(browserOn.web_check, { enabled: false }, "turning browser control on keeps web check off");
  assert.equal(browserOn.browser.enabled, true);
  assert.equal(browserOn.browser.headless, false);
  assert.deepEqual(mergeSettings(settings, { web_check: { enabled: true } }).web_check, { enabled: true });
  assert.deepEqual(mergeSettings({ browser: {}, desktop: {} }, {}).web_check, { enabled: true }, "absent means on");
});

const app = { command: "npm run dev", url: "http://127.0.0.1:43817/" };

test("the palette can stop and open a running app", () => {
  assert.deepEqual(
    paletteItems({ browser: null, controlling_screen: false, app }).map((item) => [item.id, item.label, item.detail]),
    [
      ["computer:stop-app", "Stop the app", "npm run dev"],
      ["computer:open-app", "Open the app in your browser", "127.0.0.1:43817"],
    ],
  );
  assert.deepEqual(paletteItems({ browser: null, controlling_screen: false, app: null }), []);
});

const appTask = (changes = {}) => ({
  id: 3, run_id: "r", kind: "app", status: "running", label: "App · npm run dev",
  detail: "127.0.0.1:43817 → sandbox port 5173", latest_message: "VITE v5.4.2  ready in 412 ms",
  url: "http://127.0.0.1:43817/", ...changes,
});

test("the running app task is found for Stop", () => {
  const shell = { id: 1, kind: "shell", status: "running" };
  assert.equal(runningAppTask([shell, appTask({ id: 2, status: "succeeded" }), appTask()]).id, 3);
  assert.equal(runningAppTask([shell, appTask({ status: "failed" })]), null);
  assert.equal(runningAppTask(undefined), null);
});

test("a running app's card shows its address and its last line", () => {
  assert.equal(taskDetail(appTask()), "127.0.0.1:43817 → sandbox port 5173\nVITE v5.4.2  ready in 412 ms");
  assert.equal(taskDetail(appTask({ latest_message: "" })), "127.0.0.1:43817 → sandbox port 5173");
  assert.equal(taskDetail(appTask({ status: "succeeded", detail: "stopped" })), "stopped");
  assert.equal(taskDetail({ kind: "shell", status: "running", detail: "cargo test", latest_message: "x" }), "cargo test");
});

test("a stopped app has no result to insert; a failed one does", () => {
  assert.equal(canInsertResult(appTask({ status: "succeeded", detail: "stopped" })), false);
  assert.equal(canInsertResult(appTask({ status: "failed", detail: "exited with code 1" })), true);
  assert.equal(canInsertResult(appTask()), false, "not while it runs");
  assert.equal(canInsertResult({ kind: "shell", status: "succeeded", detail: "ok", latest_message: "" }), true);
  assert.equal(canInsertResult({ kind: "shell", status: "cancelled", detail: "", latest_message: "" }), false);
});
