# Release notes

## Unreleased (after 0.1.0-beta.11)

This release lets the agent work outside the editor — in a browser, on your
screen, and in the web app it is building — and tightens what a sandboxed
command can leave behind.

### New: browser control

The agent can drive an installed Chromium-family browser (Google Chrome,
Chromium, Brave, Microsoft Edge or Vivaldi) page by page: `browser_navigate`,
`browser_snapshot`, `browser_screenshot`, `browser_click` (including double
clicks), `browser_type`, `browser_press_key`, `browser_scroll`,
`browser_select`, `browser_submit` and `browser_tabs`.

- Off by default; turn it on in **Settings → Computer use** or with
  `[browser] enabled = true` in your user config. A project's `forge.toml`
  cannot turn it on.
- The browser starts with a fresh profile (none of your cookies, logins or
  extensions) and is driven over a private pipe, not a debugging port.
- All of its traffic goes through the session's network gateway, `localhost`
  included: hosts your network permissions do not allow are asked about or
  blocked, as for shell commands. No direct WebRTC, DNS or QUIC.
- **Submitting a form asks you first**, showing what would be sent where. A
  click or Enter that would submit is refused. The agent never types into a
  password field, and downloads are refused.
- A "Browser open" strip above the composer, **Close browser** there and in
  the command palette.

### New: desktop control (Linux, X11)

The agent can see your primary display and use your mouse and keyboard:
`desktop_screenshot`, `desktop_click`, `desktop_move`, `desktop_scroll`,
`desktop_type` and `desktop_key`.

- Off by default; needs a model that can see images.
- The first use in a turn asks you; control lasts until the turn ends.
  Approve all never answers it, and child agents never inherit it.
- **Move the mouse to take control back.** **Stop** above the composer and
  the command palette end it too. One session holds the screen at a time.
- The agent can never click or type into Forge's own window, nor into a
  window whose owner cannot be told (needs the X-Resource extension).
- Wayland and macOS are reported as unsupported.

### New: web check — the agent proves its web UI works

After changing a web app, the agent can run it and check its pages.

- **`app_start {command, port}`** starts the app **in the sandbox** (same
  network permissions and approval rules as `bash`), waits until it answers,
  and offers it to this machine on a random `127.0.0.1` port while it runs.
  If it exits or never answers, the agent gets its last output lines and a
  hint when the output names another port.
- **`web_check`** opens pages of that app in a separate headless browser that
  can reach **only the app** (other hosts and local services are blocked and
  listed, not counted). Per page it waits for the page to settle, runs
  optional steps (`click`, `type`, `press`, by CSS selector or `text=Label`)
  and expectations (visible text, visible selector, title), at 1280×800 and
  optionally 390×844. It fails on an HTTP error, a console error, an uncaught
  exception, a failed request to the app, a step that cannot be done or an
  unmet expectation, and answers with the evidence and a screenshot. Results
  are recorded as verification steps in turn records.
- **`app_stop`** stops the app and its checker browser.
- The app appears in **Activity** with **Stop** and **Open in your browser**;
  the command palette has **Stop the app** and **Open the app in your
  browser**. One app runs at a time; it stops with the session.
- **On by default** where it can run: Linux, with a Chrome-family browser and
  `socat`. Turn it off in **Settings → Computer use** or with
  `[web_check] enabled = false` in your user config.
- A project can describe its app in `forge.toml` (shown to the agent as the
  project's own words; only plain values are accepted):

  ```toml
  [web]
  start = "npm run dev"
  port = 5173
  path = "/"
  ready_timeout_secs = 60
  ```

### Sandbox: nothing a command starts outlives it (Linux)

Every sandboxed command — `bash`, `exec_command`, `background_run`, confined
git, MCP servers and apps — now runs in its own process space. Whatever it
starts, a `&` or `setsid` child included, stops when the command returns, is
cancelled or stopped, or when Forge exits. Commands also see only their own
processes in `/proc`, not the host's.

**Behaviour change:** a server started in the background with `&` inside a
`bash` call stops when that call returns. It was never reachable from later
commands (each command has its own network namespace); use `background_run`
for long-running work and `app_start` for a web app.

### Smaller requests

- Built-in tool schemas are compacted (no generator boilerplate): about 700
  tokens less on every request, with validation unchanged.
- Computer-use and web check tool definitions are kept short (web check adds
  about 665 tokens per request while it is offered); old screenshots are
  dropped from requests in batches (a request carries the latest three to
  five), so the provider's prompt cache is reused.

### Desktop app

- **Settings → Computer use**: the browser program (shared by browser control
  and web check), Web check, Browser control with Show the browser window,
  and Desktop control, each saying why when it cannot run here.
- Approval cards for form submits and screen control, the "Browser open" and
  "Controlling your screen" strips, and palette entries for each.
- Custom agents can be given the `browser` and `desktop` capabilities. The web
  app tools are never given to child or custom agents.

### Security

- Screenshots from browser control, desktop control and web check are written
  to a private folder no sandbox can see, never through a link or over
  another file.
- The web app's relay only serves connections made by your own user account;
  its socket lives in a fresh private directory outside the sandbox's reach.
- The checker browser reaches only the app's exact address.
- See SECURITY.md for the full boundaries.

### Requirements

- Browser control and web check: an installed Chromium-family browser.
- Web check: Linux with bubblewrap and socat (the installer and CI already
  provide both).
- Desktop control: Linux with X11 and the X-Resource extension (Xorg has it).

### Known limitations

- Web check and desktop control are Linux only. On macOS, a sandboxed
  command's process group is stopped, but a child that leaves the group can
  outlive it (no process namespaces there).
- App output and console messages are marked as untrusted data for the model
  but are not scrubbed of secrets, like command output.
