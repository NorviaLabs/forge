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
  listener({ key: "Tab", shiftKey: true, preventDefault: () => (prevented = true) });
  assert.equal(document.activeElement, last);
  assert.ok(prevented);
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
