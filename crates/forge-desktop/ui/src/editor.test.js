import { test } from "node:test";
import assert from "node:assert/strict";
import { findText, highlight, lineOffset, replaceAllText } from "./editor.js";

test("find wraps in both directions and keeps textarea offsets", () => {
  assert.deepEqual(findText("😀 One one", "ONE", 0), [3, 6]);
  assert.deepEqual(findText("one one", "one", 4), [4, 7]);
  assert.deepEqual(findText("one one", "one", 7), [0, 3]);
  assert.deepEqual(findText("one one", "one", -1, true), [4, 7]);
  assert.equal(findText("hello", "no"), null);
  assert.equal(findText("hello", ""), null);
  assert.deepEqual(findText("İ x [one]", "[one]"), [4, 9]);
});

test("replace all is literal, case-insensitive and counts each match", () => {
  assert.deepEqual(replaceAllText("One one ONE", "one", "two"), { text: "two two two", count: 3 });
  assert.deepEqual(replaceAllText("a.b a.b", "a.b", "$1"), { text: "$1 $1", count: 2 });
  assert.deepEqual(replaceAllText("same", "", "x"), { text: "same", count: 0 });
});

test("line positions include empty final lines and reject invalid input", () => {
  assert.equal(lineOffset("a\n😀\n", 3), 5);
  for (const line of [0, -1, 1.5, NaN, Infinity, 4]) assert.equal(lineOffset("a\nb", line), null);
});

test("syntax colors preserve source exactly, without HTML insertion", () => {
  const source = 'const x = "<script>"; /* comment\nmore */\nreturn 42;';
  const tokens = highlight(source, "test.ts");
  assert.equal(tokens.map((token) => token.text).join(""), source);
  assert.deepEqual(tokens.filter((token) => token.kind).map((token) => token.kind), ["keyword", "string", "comment", "keyword", "number"]);
  assert.equal(highlight("# hi", "test.py")[0].kind, "comment");
  assert.deepEqual(highlight(source, "test.txt"), [{ text: source, kind: "" }]);
});
