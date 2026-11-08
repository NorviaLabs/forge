import { test } from "node:test";
import assert from "node:assert/strict";
import { historyKeyAction } from "./history.js";

const state = {
  entries: ["first", "second"], value: "unfinished draft",
  start: 0, end: 0, index: null, draft: "",
};

test("Up and Down cycle prompts, Escape restores the untouched draft", () => {
  const newest = historyKeyAction({ key: "ArrowUp" }, state);
  assert.deepEqual(newest, { value: "second", index: 1, draft: "unfinished draft" });
  const oldest = historyKeyAction({ key: "ArrowUp" }, { ...state, ...newest });
  assert.equal(oldest.value, "first");
  assert.equal(historyKeyAction({ key: "ArrowUp" }, { ...state, ...oldest }).value, "second");
  assert.equal(historyKeyAction({ key: "ArrowDown" }, { ...state, ...newest }).value, "first");
  assert.deepEqual(historyKeyAction({ key: "Escape" }, { ...state, ...oldest }), {
    value: "unfinished draft", index: null, draft: "",
  });
});

test("empty history, live Down, selections and modified keys keep native editing", () => {
  assert.equal(historyKeyAction({ key: "ArrowUp" }, { ...state, entries: [] }), null);
  assert.equal(historyKeyAction({ key: "ArrowDown" }, state), null);
  assert.equal(historyKeyAction({ key: "Escape" }, state), null);
  assert.equal(historyKeyAction({ key: "ArrowUp" }, { ...state, end: 1 }), null);
  for (const modifier of ["shiftKey", "ctrlKey", "metaKey", "altKey", "isComposing"]) {
    assert.equal(historyKeyAction({ key: "ArrowUp", [modifier]: true }, state), null);
  }
});

test("multiline prompts only recall at the first or last line", () => {
  const multiline = { ...state, value: "top\nbottom", index: 1, draft: "saved" };
  assert.equal(historyKeyAction({ key: "ArrowUp" }, { ...multiline, start: 6, end: 6 }), null);
  assert.equal(historyKeyAction({ key: "ArrowDown" }, { ...multiline, start: 1, end: 1 }), null);
  assert.equal(historyKeyAction({ key: "ArrowUp" }, { ...multiline, start: 2, end: 2 }).value, "first");
  assert.equal(historyKeyAction({ key: "ArrowDown" }, { ...multiline, start: 10, end: 10 }).value, "first");
});

test("a single history entry wraps without losing an empty draft", () => {
  const single = { ...state, entries: ["only"], value: "" };
  const recalled = historyKeyAction({ key: "ArrowUp" }, single);
  assert.equal(historyKeyAction({ key: "ArrowUp" }, { ...single, ...recalled }).value, "only");
  assert.equal(historyKeyAction({ key: "Escape" }, { ...single, ...recalled }).value, "");
});
