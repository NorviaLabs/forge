// Computer use in the window: the "Browser open" and "Controlling your
// screen" strips, the takeover note, the settings section, the consent
// card's words, and the palette entries. Pure functions over what the
// backend sends; the components only render them.

// A URL as a person reads it: no scheme, no trailing slash on a bare host.
function shortUrl(url) {
  const text = String(url ?? "");
  const match = /^[a-z][a-z0-9+.-]*:\/\/(?:[^@/?#]*@)?(.*)$/i.exec(text);
  const rest = match ? match[1] : text;
  return rest.replace(/^([^/?#]+)\/$/, "$1");
}

// The "Browser open" strip: a fact about the session, so no state dot.
export function browserStrip(status) {
  const browser = status?.browser;
  if (!browser) return null;
  const tabs = `${browser.tabs} ${browser.tabs === 1 ? "tab" : "tabs"}`;
  return {
    title: "Browser open",
    detail: `${browser.browser} · ${shortUrl(browser.url)} · ${tabs}`,
    dot: null,
  };
}

// The "Controlling your screen" strip: the dot and the word follow the
// session's state (blue running, amber waiting), never the strip's own.
export function screenStrip(status, turnState) {
  if (!status?.controlling_screen) return null;
  const waiting = turnState === "waiting";
  const running = turnState === "running" || turnState === "streaming" || turnState === "queued";
  return {
    title: "Controlling your screen",
    dot: waiting ? "waiting" : running ? "running" : "idle",
    word: waiting ? "waiting for your approval" : running ? "running" : "idle",
    hint: "move the mouse to take it back",
  };
}

const TAKEOVER = "you moved the mouse";

// After the user took the screen back: shown until a desktop tool succeeds
// again (the agent asked, and was allowed).
export function tookBackScreen(messages) {
  for (let index = (messages?.length ?? 0) - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message.role !== "tool" || !String(message.name ?? "").startsWith("desktop_")) continue;
    return String(message.text ?? "").includes(TAKEOVER)
      ? "You took back the screen. The agent will ask again before it uses it."
      : null;
  }
  return null;
}

// Settings → Computer use, ready to render.
export function settingsModel(view) {
  const browser = view?.settings?.browser ?? { enabled: false, executable: null, headless: false };
  const desktop = view?.settings?.desktop ?? { enabled: false };
  const browserOptions = (view?.browsers ?? []).map((found) => ({ value: found.path, label: found.name }));
  const chosen = browser.executable || null;
  if (chosen && !browserOptions.some((option) => option.value === chosen)) {
    browserOptions.push({ value: chosen, label: chosen.split("/").filter(Boolean).at(-1) ?? chosen });
  }
  const selectedBrowser = chosen ?? browserOptions[0]?.value ?? "";
  const browserProblem = view?.browser_problem ?? null;
  const needsBrowser = "Needs a browser. Choose one above.";
  const webCheck = view?.settings?.web_check ?? { enabled: true };
  const webCheckDisabled = !!browserProblem || !!view?.web_check_unsupported;
  return {
    // Shown unchecked while it cannot run; what is saved is kept.
    browserEnabled: !!browser.enabled && !browserProblem,
    browserControlDisabled: !!browserProblem,
    browserNote: browserProblem
      ? needsBrowser
      : "The agent opens a browser with a fresh profile and works through pages. Form submits ask first. Traffic follows your network permissions.",
    // The program row serves browser control and web check alike.
    browserRowDisabled: false,
    browserSubrowsDisabled: !browser.enabled || !!browserProblem,
    webCheckEnabled: webCheck.enabled !== false && !webCheckDisabled,
    webCheckDisabled,
    webCheckNote: browserProblem
      ? needsBrowser
      : view?.web_check_unsupported
        || "The agent can run your project's web app in the sandbox and check its pages in a hidden browser that reaches only that app. It doesn't ask first.",
    browserOptions,
    selectedBrowser,
    browserPath: selectedBrowser,
    browserProblem,
    showWindow: !browser.headless,
    desktopEnabled: !!desktop.enabled,
    desktopDisabled: !!view?.desktop_unsupported,
    desktopNote: view?.desktop_unsupported
      || "The agent can see your screen and use your mouse and keyboard, after you allow it for each turn.",
  };
}

// The risk phrase that opens an approval card's reason.
export function riskLabel(kind) {
  switch (kind) {
    case "screen_control": return "Screen control";
    case "form_submit": return "Form submit";
    case "sandbox_escalation": return "Sandbox escalation";
    case "network_host": return "Network access";
    default: return "";
  }
}

// What a privileged grant's confirmation step says.
export function confirmText(decision, kind, target = null) {
  if (kind === "screen_control") {
    return "The agent will be able to do anything you can do on this computer until control ends. Close windows it should not see first.";
  }
  switch (decision) {
    case "allow_host_for_session":
    case "allow_host_always":
      return "This opens network access the sandbox refused.";
    case "approve_all":
      return target
        ? `Every command in this session will run on ${target} without asking, outside Forge's sandbox.`
        : "Every review prompt in this session will be approved automatically. The sandbox stays on.";
    case "allow_path_for_session":
    case "allow_path_always":
      return "Sandboxed commands will be able to write this directory. The sandbox stays on.";
    case "trust_program":
      return "This program will run outside the sandbox in this project from now on, without asking.";
    default:
      return "This command runs outside the filesystem sandbox.";
  }
}

// Palette entries for the session on screen, only when they would do
// something.
export function paletteItems(status) {
  const items = [];
  if (status?.controlling_screen) items.push({ id: "computer:stop-screen", label: "Stop screen control" });
  if (status?.browser) items.push({ id: "computer:close-browser", label: "Close browser" });
  if (status?.app) {
    items.push({ id: "computer:stop-app", label: "Stop the app", detail: status.app.command });
    items.push({ id: "computer:open-app", label: "Open the app in your browser", detail: shortUrl(status.app.url) });
  }
  return items;
}

// Settings to save: what is saved, with one change applied. Every section
// is sent, so saving one switch never resets another.
export function mergeSettings(settings, change) {
  return {
    browser: { ...settings?.browser, ...(change?.browser ?? {}) },
    desktop: { ...settings?.desktop, ...(change?.desktop ?? {}) },
    web_check: { enabled: true, ...settings?.web_check, ...(change?.web_check ?? {}) },
  };
}

// The running app's task, for Stop.
export function runningAppTask(background) {
  return (background ?? []).find((task) => task.kind === "app" && task.status === "running") ?? null;
}

// What a task card shows under its name: a running app adds its last line.
export function taskDetail(task) {
  const detail = task?.detail ?? "";
  if (task?.kind === "app" && task.status === "running" && task.latest_message) {
    return `${detail}\n${task.latest_message}`;
  }
  return detail;
}

const FINISHED = ["succeeded", "failed", "cancelled"];

// Whether a finished task has a result worth inserting into the prompt. An
// app that was stopped has none.
export function canInsertResult(task) {
  if (!FINISHED.includes(task?.status)) return false;
  if (task.kind === "app" && task.status !== "failed") return false;
  return !!(task.latest_message || task.detail);
}
