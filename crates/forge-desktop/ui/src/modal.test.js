import { test } from "node:test";
import assert from "node:assert/strict";
import { modal } from "./modal.js";

test("modal traps Tab, respects busy Escape and restores launcher focus", () => {
  const button = () => ({ isConnected: true, getClientRects: () => [1], focus() { document.activeElement = this; } });
  const launcher = button(), first = button(), last = button();
  globalThis.document = { activeElement: launcher };
  let listener, closed = 0, prevented = false;
  const node = { querySelectorAll: () => [first, last], addEventListener: (_, handler) => (listener = handler), removeEventListener() {} };
  const action = modal(node, { onClose: () => closed++ });
  assert.equal(document.activeElement, first);
  listener({ key: "Tab", shiftKey: true, stopPropagation() {}, preventDefault: () => (prevented = true) });
  assert.equal(document.activeElement, last);
  assert.ok(prevented);
  let stopped = false;
  listener({ key: "s", metaKey: true, stopPropagation: () => (stopped = true) });
  assert.ok(stopped, "workspace shortcuts must not escape the modal");
  let enterStopped = false;
  listener({ key: "Enter", stopPropagation: () => (enterStopped = true) });
  assert.equal(enterStopped, false, "plain keys must still reach the dialog");
  action.update({ busy: true, onClose: () => closed++ });
  listener({ key: "Escape", stopPropagation() {} });
  assert.equal(closed, 0);
  action.update({ onClose: () => closed++ });
  listener({ key: "Escape", stopPropagation() {} });
  assert.equal(closed, 1);
  action.destroy();
  assert.equal(document.activeElement, launcher);
  delete globalThis.document;
});

test("dialog can initially focus Cancel without changing Tab order", () => {
  const button = () => ({ isConnected: true, getClientRects: () => [1], focus() { document.activeElement = this; } });
  const launcher = button(), quit = button(), cancel = button();
  globalThis.document = { activeElement: launcher };
  let listener;
  const node = {
    querySelector: (selector) => selector === "#quit-cancel" ? cancel : null,
    querySelectorAll: () => [quit, cancel],
    addEventListener: (_, handler) => (listener = handler),
    removeEventListener() {},
  };
  try {
    const action = modal(node, { initialFocus: "#quit-cancel" });
    assert.equal(document.activeElement, cancel);
    listener({ key: "Tab", stopPropagation() {}, preventDefault() {} });
    assert.equal(document.activeElement, quit);
    action.destroy();
    assert.equal(document.activeElement, launcher);

    const fallback = modal(node, { initialFocus: "#missing" });
    assert.equal(document.activeElement, quit);
    fallback.destroy();
  } finally {
    delete globalThis.document;
  }
});
