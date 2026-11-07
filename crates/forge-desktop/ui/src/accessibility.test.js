import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { parse } from "svelte/compiler";

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

test("session panel moves focus to its close control and back to its launcher", async () => {
  let open = false, focused;
  const toggle = handler("../App", "toggleInspector", {
    get inspectorOpen() { return open; },
    set inspectorOpen(value) { open = value; },
    tick: () => Promise.resolve(),
    inspectorClose: { focus() { focused = "close"; } },
    inspectorToggle: { focus() { focused = "launcher"; } },
  });
  await toggle();
  assert.equal(open, true);
  assert.equal(focused, "close");
  await toggle();
  assert.equal(open, false);
  assert.equal(focused, "launcher");
});
