import { test } from "node:test";
import assert from "node:assert/strict";
import { CommandFailure, commandFailure, failureAction } from "./failure.js";

test("a typed rejection prints as its message and keeps its fields", () => {
  const failure = commandFailure({ kind: "busy", message: "2 session(s) are still busy", sessions: [{ label: "a" }] });
  assert.ok(failure instanceof CommandFailure);
  assert.equal(String(failure), "2 session(s) are still busy");
  assert.equal(`Could not close: ${failure}`, "Could not close: 2 session(s) are still busy");
  assert.equal(failure.kind, "busy");
  assert.equal(failure.message, "2 session(s) are still busy");
  assert.deepEqual(failure.sessions, [{ label: "a" }]);
});

test("a string or an Error passes through unchanged", () => {
  assert.equal(commandFailure("plain"), "plain");
  const error = new Error("boom");
  assert.equal(commandFailure(error), error);
  assert.equal(commandFailure(null), null);
  assert.deepEqual(commandFailure({ message: "no kind" }), { message: "no kind" });
});

test("the page ignores a closing project, forgets an unknown session, shows the rest", () => {
  assert.equal(failureAction(commandFailure({ kind: "shutting_down", message: "Forge is shutting down" })), "ignore");
  assert.equal(failureAction(commandFailure({ kind: "unknown_session", message: "that session no longer exists" })), "forget");
  assert.equal(failureAction(commandFailure({ kind: "error", message: "nope" })), "show");
  assert.equal(failureAction("plain"), "show");
});
