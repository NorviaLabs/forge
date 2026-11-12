// Build the UI, then run: node src/audit.browser.mjs [dist directory]
// Requires Playwright with Chromium. PLAYWRIGHT_MODULE may name an installed module path.
// Exercises the production UI with a fake IPC boundary; no repository operations run.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve, extname, sep } from "node:path";
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "playwright");
const root = resolve(process.argv[2] || "dist");
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };
const server = createServer(async (request, response) => {
  try {
    const path = resolve(root, `.${new URL(request.url, "http://localhost").pathname}`);
    if (path !== root && !path.startsWith(root + sep)) throw new Error("Invalid path");
    const file = path === root ? resolve(root, "index.html") : path;
    response.setHeader("Content-Type", types[extname(file)] || "application/octet-stream");
    response.end(await readFile(file));
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise((done) => server.listen(0, "127.0.0.1", done));
let browser;
try {
  browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE });
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 }, reducedMotion: "reduce" });
  page.setDefaultTimeout(5000);
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    const callbacks = new Map();
    let id = 0;
    const initial = {
      session_id: "audit", turn_state: "idle", workspace: "/fixture", branch: "main",
      model: "First", context_usage: 0, queued: 0, pending_approval: null,
      messages: Array.from({ length: 250 }, (_, index) => ({
        role: index === 100 ? "tool" : index % 2 ? "assistant" : "user",
        text: index === 100 ? "Short output…" : `Message ${index}`, thinking: "",
      })),
    };
    const session = { session_id: "audit", label: "Audit fixture", lifecycle: "active", turn_state: "idle", ownership: "primary" };
    const models = ["First", "Second"].map((name) => ({
      id: name, name, provider: "Fixture", profile_id: "fixture", effort: ["low", "high"], effort_default: "low",
    }));
    let channel;
    const events = new Map();
    window.audit = {
      calls: [],
      close: () => callbacks.get(events.get("close_requested"))({ payload: null }),
      append() {
        initial.messages.push({ role: "assistant", text: "New output", thinking: "" });
        channel.onmessage({ type: "session", ...initial });
      },
      clear() { initial.messages = []; channel.onmessage({ type: "session", ...initial }); },
    };
    window.__TAURI_INTERNALS__ = {
      transformCallback(fn) { callbacks.set(++id, fn); return id; },
      unregisterCallback(key) { callbacks.delete(key); },
      async invoke(command, args) {
        window.audit.calls.push({ command, args });
        if (command === "plugin:event|listen") { events.set(args.event, args.handler); return args.handler; }
        if (command === "subscribe") { channel = args.channel; return { session_id: "audit", sessions: [session], initial }; }
        if (command === "current_session") return initial;
        if (command === "trust_status") return { workspace: "/fixture", needs_trust: false, error: "" };
        if (command === "list_themes" || command === "sandbox_grants" || command === "list_resumable_sessions" || command === "git_last_turn_files") return [];
        if (command === "git_status") return { branch: "main", files: [] };
        if (command === "list_files") return [{ path: "example.rs", is_dir: false, depth: 0 }];
        if (command === "list_models") return models;
        if (command === "read_file") return { text: "fn main() {}\n", read_only: "" };
        if (command === "message_detail") return { text: `Expanded ${args.index}`, thinking: "" };
        if (command === "set_unsaved" || command === "quit_app") return null;
        throw new Error(`Unexpected fixture command: ${command}`);
      },
    };
  });
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.locator("main:not([inert])").waitFor();
  if (process.env.AUDIT_SCREENSHOTS) await page.screenshot({ path: `${process.env.AUDIT_SCREENSHOTS}/desktop.png` });

  async function focusVisible(label) {
    const field = page.getByLabel(label, { exact: true }).and(page.locator("input, textarea"));
    await field.focus();
    const outline = await field.evaluate((node) => {
      const style = getComputedStyle(node);
      return { style: style.outlineStyle, width: parseFloat(style.outlineWidth) };
    });
    assert.notEqual(outline.style, "none", `${label} needs a visible outline`);
    assert.ok(outline.width >= 2, `${label} needs a two-pixel outline`);
  }
  await page.keyboard.press("Tab");
  await focusVisible("Filter files");
  await focusVisible("Search file contents");
  await page.keyboard.press("Control+k");
  await focusVisible("Command palette");
  await page.keyboard.press("Escape");

  await page.locator(".model-button").click();
  await focusVisible("Search models");
  const second = page.locator("button.model").filter({ hasText: "Second" });
  await second.click();
  assert.equal(await second.getAttribute("aria-pressed"), "true");
  assert.equal(await page.locator("button.model").filter({ hasText: "First" }).getAttribute("aria-pressed"), "false");
  await page.getByRole("button", { name: "high", exact: true }).click();
  assert.equal(await page.getByRole("button", { name: "high", exact: true }).getAttribute("aria-pressed"), "true");
  if (process.env.AUDIT_SCREENSHOTS) await page.screenshot({ path: `${process.env.AUDIT_SCREENSHOTS}/model-selection.png` });
  await page.keyboard.press("Escape");

  await page.getByRole("button", { name: "example.rs", exact: true }).click();
  await focusVisible("Edit example.rs");
  await page.evaluate(() => window.audit.close());
  await page.locator("#quit-cancel").waitFor();
  assert.equal(await page.evaluate(() => document.activeElement.id), "quit-cancel");
  await page.keyboard.press("Enter");
  await page.locator("#quit-cancel").waitFor({ state: "detached" });
  assert.equal(await page.evaluate(() => window.audit.calls.some((call) => call.command === "quit_app")), false);

  await page.getByRole("button", { name: "Conversation", exact: true }).click();
  assert.equal(await page.locator(".turn").count(), 100);
  await page.getByRole("button", { name: "Older", exact: true }).click();
  await page.getByText("51–150 of 250 messages", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Show full output", exact: true }).click();
  await page.getByText("Expanded 100", { exact: true }).waitFor();
  assert.equal(await page.evaluate(() => window.audit.calls.findLast((call) => call.command === "message_detail").args.index), 100);
  await page.evaluate(() => window.audit.append());
  await page.getByText("51–150 of 251 messages", { exact: true }).waitFor();
  assert.equal(await page.locator(".turn").count(), 100);

  await page.getByRole("button", { name: "Older", exact: true }).click();
  await page.getByText("1–100 of 251 messages", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Latest", exact: true }).click();
  await page.getByText("152–251 of 251 messages", { exact: true }).waitFor();
  assert.equal(await page.locator(".turn").count(), 100);

  await page.evaluate(() => window.audit.clear());
  await page.getByText("Ask Forge to work the repository", { exact: true }).waitFor();
  await page.evaluate(() => window.audit.append());
  await page.getByText("New output", { exact: true }).waitFor();
  assert.equal(await page.locator(".turn").count(), 1);

  for (const theme of ["dark", "light"]) {
    await page.evaluate((theme) => { document.documentElement.dataset.theme = theme; }, theme);
    for (const viewport of [{ width: 1200, height: 800 }, { width: 800, height: 600 }]) {
      await page.setViewportSize(viewport);
      await page.evaluate(() => window.audit.close());
      const dialog = page.getByRole("dialog", { name: "Quit Forge?" });
      await dialog.waitFor();
      await dialog.evaluate((node) => {
        node.style.fontSize = "24px";
        node.querySelector("p").style.fontSize = "24px";
        node.querySelector("p").textContent = "Long warning with unsaved changes. ".repeat(60);
      });
      await page.locator("#quit-cancel").scrollIntoViewIfNeeded();
      const box = await dialog.boundingBox();
      assert.ok(box.y >= 0 && box.y + box.height <= viewport.height, "dialog must fit the window");
      const cancel = await page.locator("#quit-cancel").boundingBox();
      assert.ok(cancel.y >= 0 && cancel.y + cancel.height <= viewport.height, "Cancel must remain reachable");
      if (process.env.AUDIT_SCREENSHOTS) await page.screenshot({ path: `${process.env.AUDIT_SCREENSHOTS}/${theme}-${viewport.width}.png` });
      await page.keyboard.press("Escape");
    }
  }
  assert.deepEqual(errors, []);
  console.log("PASS: focus, safe quit, model state, bounded history, and dialog overflow in Chromium; IPC is mocked.");
} finally {
  await browser?.close();
  await new Promise((done) => server.close(done));
}
