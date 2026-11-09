import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { parse } from "svelte/compiler";
import { escapeReturn } from "./view.js";

function handler(component, name, context) {
  const source = readFileSync(new URL(`./lib/${component}.svelte`, import.meta.url), "utf8");
  const ast = parse(source);
  const declaration = ast.instance.content.body.find((node) => node.type === "FunctionDeclaration" && node.id.name === name);
  return runInNewContext(`(${source.slice(declaration.start, declaration.end)})`, context);
}

test("editor Tab and Shift+Tab leave navigation and document unchanged", () => {
  const keydown = handler("FilePane", "onEditorKeydown", {
    findOpen: false,
    onDraftChange() { assert.fail("navigation must not edit the document"); },
  });
  for (const shiftKey of [false, true]) {
    keydown({ key: "Tab", shiftKey, preventDefault() { assert.fail("Tab must move focus"); } });
  }
});

test("palette arrows select the command that Enter executes", () => {
  const commands = [{ id: "first" }, { id: "second" }];
  let cursor = 0, selected, prevented = 0;
  const keydown = handler("Palette", "keydown", {
    move(delta) { cursor = (cursor + delta + commands.length) % commands.length; },
    get cursor() { return cursor; },
    filtered: commands,
    run(item) { selected = item; },
  });
  const key = (value) => keydown({ key: value, preventDefault() { prevented++; } });
  key("ArrowDown");
  key("Enter");
  assert.equal(selected, commands[1]);
  key("ArrowUp");
  key("Enter");
  assert.equal(selected, commands[0]);
  assert.equal(prevented, 4);
});

function keyRouting({ screen = "session", activeDocId = null, active = null } = {}) {
  const calls = [];
  let focused = null;
  const view = { screen, drawerOpen: screen === "session", activeDocId };
  const context = {
    booted: true,
    currentView: view,
    activeDoc: null,
    document: { activeElement: active },
    escapeReturn,
    tick: () => Promise.resolve(),
    leaveFocus: () => { calls.push("leaveFocus"); view.screen = "session"; },
    openFiles: () => calls.push("openFiles"),
    toggleTerminal: () => calls.push("toggleTerminal"),
    closeTab: (id) => calls.push(`closeTab:${id}`),
    saveActive: () => calls.push("saveActive"),
    openModelPicker: () => calls.push("openModelPicker"),
    paletteOpen: false,
    filesButton: { focus() { focused = "files"; } },
    terminalButton: { focus() { focused = "terminal"; } },
  };
  const keydown = handler("../App", "onKeydown", context);
  const press = async (key, modifiers = {}) => {
    let prevented = false;
    keydown({ key, metaKey: false, ctrlKey: false, ...modifiers, preventDefault() { prevented = true; } });
    await Promise.resolve();
    await Promise.resolve();
    return prevented;
  };
  return { press, calls, view, focused: () => focused };
}

const inside = (selector) => ({ closest: (query) => (query.split(", ").includes(selector) ? {} : null) });
const plainButton = { closest: () => null };

test("Escape on a focus screen returns to the session and refocuses its launcher", async () => {
  const files = keyRouting({ screen: "files", active: plainButton });
  assert.equal(await files.press("Escape"), true);
  assert.deepEqual(files.calls, ["leaveFocus"]);
  assert.equal(files.view.screen, "session");
  assert.equal(files.focused(), "files");

  const terminal = keyRouting({ screen: "terminal", active: plainButton });
  await terminal.press("Escape");
  assert.deepEqual(terminal.calls, ["leaveFocus"]);
  assert.equal(terminal.focused(), "terminal");
});

test("Escape inside an editor, field or the terminal stays with that control", async () => {
  for (const screen of ["session", "files", "terminal"]) {
    for (const selector of ["textarea", "input", ".terminal-host"]) {
      const routing = keyRouting({ screen, active: inside(selector) });
      assert.equal(await routing.press("Escape"), false, `${screen} / ${selector}`);
      assert.deepEqual(routing.calls, []);
      assert.equal(routing.view.screen, screen);
    }
  }
  const primary = keyRouting({ screen: "session", active: plainButton });
  await primary.press("Escape");
  assert.deepEqual(primary.calls, [], "the primary screen has nothing to leave");
});

test("Cmd+W closes only a document on the Files screen", async () => {
  const empty = keyRouting({ screen: "files", activeDocId: null, active: plainButton });
  assert.equal(await empty.press("w", { metaKey: true }), false);
  assert.deepEqual(empty.calls, []);
  const primary = keyRouting({ screen: "session", activeDocId: "file:a.js", active: plainButton });
  await primary.press("w", { metaKey: true });
  assert.deepEqual(primary.calls, [], "Cmd+W never closes the terminal or the session");
  const open = keyRouting({ screen: "files", activeDocId: "file:a.js", active: plainButton });
  assert.equal(await open.press("w", { metaKey: true }), true);
  assert.deepEqual(open.calls, ["closeTab:file:a.js"]);
});

test("Cmd+E opens Files from the session and leaves either focus screen", async () => {
  const primary = keyRouting({ screen: "session", active: plainButton });
  await primary.press("e", { metaKey: true });
  assert.deepEqual(primary.calls, ["openFiles"]);
  const terminal = keyRouting({ screen: "terminal", active: plainButton });
  await terminal.press("e", { metaKey: true });
  assert.deepEqual(terminal.calls, ["leaveFocus"]);
  const typing = keyRouting({ screen: "terminal", active: inside(".terminal-host") });
  assert.equal(await typing.press("e", { ctrlKey: true }), false, "Ctrl+E is terminal input");
  assert.deepEqual(typing.calls, []);
  assert.equal(await typing.press("`", { ctrlKey: true }), true);
  assert.deepEqual(typing.calls, ["toggleTerminal"], "Ctrl+` still leaves the terminal");
});
