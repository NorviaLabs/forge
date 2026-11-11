import { test } from "node:test";
import assert from "node:assert/strict";
import { chord, isChord, isMac } from "./keys.js";

test("shortcut labels name the running platform's modifier, not both", () => {
  assert.equal(chord("K", { mac: true }), "⌘K");
  assert.equal(chord("K", { mac: false }), "Ctrl+K");
  assert.equal(chord("A", { shift: true, mac: true }), "⇧⌘A");
  assert.equal(chord("A", { shift: true, mac: false }), "Ctrl+Shift+A");
});

test("the platform is read from its name", () => {
  assert.equal(isMac("MacIntel"), true);
  assert.equal(isMac("Linux x86_64"), false);
  assert.equal(isMac(""), false);
});

test("a chord needs exactly its modifiers", () => {
  const event = (extra) => ({ key: "a", metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...extra });
  assert.equal(isChord(event({ metaKey: true, shiftKey: true }), "A", { shift: true, mac: true }), true);
  assert.equal(isChord(event({ key: "A", ctrlKey: true, shiftKey: true }), "A", { shift: true, mac: false }), true);
  // Without Shift, with Alt, or with the other platform's modifier: no.
  assert.equal(isChord(event({ metaKey: true }), "A", { shift: true, mac: true }), false);
  assert.equal(isChord(event({ metaKey: true, shiftKey: true, altKey: true }), "A", { shift: true, mac: true }), false);
  assert.equal(isChord(event({ ctrlKey: true, shiftKey: true }), "A", { shift: true, mac: true }), false);
  assert.equal(isChord(event({ metaKey: true, ctrlKey: true, shiftKey: true }), "A", { shift: true, mac: true }), false);
  // Plain typing never matches.
  assert.equal(isChord(event({ shiftKey: true }), "A", { shift: true, mac: true }), false);
});
