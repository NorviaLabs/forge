import { test } from "node:test";
import assert from "node:assert/strict";
import { filterSlashCommands, slashKeyAction, withSkills } from "./slash.js";

const commands = [
  { name: "/help", help: "Help" },
  { name: "/model", help: "Model" },
  { name: "/compact", help: "Compact" },
  { name: "/compose", help: "Compose" },
];

test("slash completion only offers matching command tokens", () => {
  assert.deepEqual(filterSlashCommands(commands, "/co").map((c) => c.name), ["/compact", "/compose"]);
  assert.deepEqual(filterSlashCommands(commands, "/").map((c) => c.name), ["/help", "/model", "/compact", "/compose"]);
});

test("slash completion stops once an argument starts", () => {
  assert.deepEqual(filterSlashCommands(commands, "/model "), []);
  assert.deepEqual(filterSlashCommands(commands, "/model gpt"), []);
  assert.deepEqual(filterSlashCommands(commands, "hello"), []);
  assert.deepEqual(filterSlashCommands(commands, "/nope"), []);
});

test("slash keys move, complete, hide, or submit an exact command", () => {
  const state = { open: true, matches: commands, index: 0, value: "/co" };
  assert.deepEqual(slashKeyAction({ key: "ArrowDown" }, state), { type: "move", index: 1 });
  assert.deepEqual(slashKeyAction({ key: "ArrowUp" }, state), { type: "move", index: 3 });
  assert.deepEqual(slashKeyAction({ key: "Tab" }, state), { type: "complete", command: commands[0] });
  assert.deepEqual(slashKeyAction({ key: "Escape" }, state), { type: "hide" });
  assert.deepEqual(slashKeyAction({ key: "Enter" }, state), { type: "complete", command: commands[0] });
  assert.deepEqual(slashKeyAction({ key: "Enter" }, { ...state, value: "/help" }), { type: "submit" });
  assert.deepEqual(slashKeyAction({ key: "Enter", metaKey: true }, state), { type: "complete", command: commands[0] });
  assert.equal(slashKeyAction({ key: "Enter", metaKey: true }, { ...state, value: "/help" }), null);
  assert.equal(slashKeyAction({ key: "Enter" }, { ...state, open: false }), null);
});

test("installed skills complete alongside commands without shadowing builtins", () => {
  const combined = withSkills(commands, [["review", "Review changes"], ["help", "A skill"]]);
  assert.equal(combined.length, commands.length + 1);
  assert.deepEqual(filterSlashCommands(combined, "/rev"), [{ name: "/review", help: "Review changes" }]);
  assert.equal(filterSlashCommands(combined, "/help")[0], commands[0]);
  const matches = filterSlashCommands(combined, "/rev");
  assert.deepEqual(slashKeyAction({ key: "Tab" }, { open: true, matches, index: 0, value: "/rev" }), {
    type: "complete", command: matches[0],
  });
  assert.deepEqual(withSkills(commands, []), commands);
});

test("Shift+Enter and IME composition bypass slash completion", () => {
  const state = { open: true, matches: commands, index: 0, value: "/help" };
  assert.equal(slashKeyAction({ key: "Enter", shiftKey: true }, state), null);
  assert.equal(slashKeyAction({ key: "Enter", isComposing: true }, state), null);
});
