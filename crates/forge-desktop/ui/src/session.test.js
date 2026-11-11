import { test } from "node:test";
import assert from "node:assert/strict";
import { applyEvent, emptySnapshot, snapshotFrom } from "./session.js";
import { clearShown, defaultView } from "./view.js";

const row = (id, turn_state = "idle") => ({ session_id: id, label: id, turn_state });

// A `session` event as the bridge sends it.
const session = (id, extra = {}) => ({
  type: "session",
  session_id: id,
  turn_state: "idle",
  messages: [{ role: "user", text: `hello from ${id}` }, { role: "assistant", text: `answer in ${id}` }],
  approve_all: false,
  thinking: false,
  workspace: `/work/${id}`,
  branch: `branch-${id}`,
  model: "test/small",
  context_usage: 0.1,
  queued: 0,
  ...extra,
});

function page(selected = "a") {
  return {
    sessionId: selected,
    snapshot: emptySnapshot(),
    sessions: [row("a"), row("b")],
    attention: {},
    error: "",
    lastTurn: "",
    focusDecisionFor: null,
    views: {},
    projectId: "p1",
    sessionOwner: {},
  };
}

test("a snapshot is projected whole, through the session's view record", () => {
  const record = defaultView();
  const shown = snapshotFrom(session("a", {
    pending_approval: { call_id: "c1" },
    tools: 3,
    skills: 1,
    pattern_allows: 2,
    github_issue_number: 7,
    messages: [{ role: "user", text: "first" }, { role: "user", text: "  " }, { role: "assistant", text: "reply" }],
  }), record);
  assert.deepEqual(Object.keys(shown).sort(), Object.keys(emptySnapshot()).sort(), "same fields as the empty value");
  assert.equal(shown.approval.call_id, "c1");
  assert.equal(shown.question, null);
  assert.deepEqual(shown.counts, { tools: 3, skills: 1, allows: 2 });
  assert.deepEqual(shown.promptHistory, ["first"], "blank prompts are not history");
  assert.equal(shown.info.branch, "branch-a");
  assert.equal(shown.githubIssue, 7);

  // What /clear hid stays hidden in the next snapshot.
  clearShown(record, 0);
  const next = snapshotFrom(session("a", { messages: [...Array(3).fill({ role: "user", text: "old" }), { role: "assistant", text: "new" }] }), record);
  assert.deepEqual(next.messages.map((message) => message.text), ["new"]);
});

test("a snapshot for a session the operator has left is not shown", () => {
  const state = page("a");
  applyEvent(state, session("a", { turn_state: "running" }));
  assert.equal(state.snapshot.info.branch, "branch-a");

  assert.deepEqual(applyEvent(state, { type: "selected", session_id: "b" }), ["reset_streaming", "close_session_dialogs", "refresh_files"]);
  assert.equal(state.sessionId, "b");
  assert.deepEqual(state.snapshot, emptySnapshot(), "nothing of A lingers: not its turn state, model or pending request");

  // A's late snapshot updates its row and nothing else.
  assert.deepEqual(applyEvent(state, session("a", { turn_state: "waiting", pending_approval: { call_id: "late" } })), []);
  assert.deepEqual(state.snapshot, emptySnapshot());
  assert.equal(state.sessions.find((entry) => entry.session_id === "a").turn_state, "waiting");

  applyEvent(state, session("b"));
  assert.equal(state.snapshot.info.branch, "branch-b");
  assert.equal(state.snapshot.approval, null);
});

test("selecting the session already on screen changes nothing", () => {
  const state = page("a");
  applyEvent(state, session("a"));
  const before = state.snapshot;
  assert.deepEqual(applyEvent(state, { type: "selected", session_id: "a" }), []);
  assert.equal(state.snapshot, before);
});

test("selecting nothing empties the pane without asking for files", () => {
  const state = page("a");
  applyEvent(state, session("a"));
  assert.deepEqual(applyEvent(state, { type: "selected", session_id: null }), ["reset_streaming", "close_session_dialogs"]);
  assert.equal(state.sessionId, "");
  assert.deepEqual(state.snapshot, emptySnapshot());
});

test("a turn that settles clears the live delta and refreshes the files once", () => {
  const state = page("a");
  assert.deepEqual(applyEvent(state, session("a", { turn_state: "running" })), [], "a running turn keeps its delta");
  assert.deepEqual(applyEvent(state, session("a", { turn_state: "idle" })), ["reset_streaming", "refresh_files"]);
  assert.deepEqual(applyEvent(state, session("a", { turn_state: "idle" })), ["reset_streaming"], "an idle update is not a finished turn");

  // The session selected next was never busy here: its first snapshot does
  // not inherit the previous session's turn.
  applyEvent(state, session("a", { turn_state: "running" }));
  applyEvent(state, { type: "selected", session_id: "b" });
  assert.deepEqual(applyEvent(state, session("b")), ["reset_streaming"]);
});

test("attention marks sessions off screen and is cleared when they stop waiting or are selected", () => {
  const state = page("a");
  applyEvent(state, { type: "attention", session_id: "b", turn_state: "waiting", message: "needs you" });
  assert.deepEqual(state.attention, { b: true });
  assert.equal(state.error, "", "another session's request is not an error");
  assert.equal(state.sessions.find((entry) => entry.session_id === "b").turn_state, "waiting");

  // Still waiting: the mark stays. Answered elsewhere: it goes.
  applyEvent(state, { type: "session_state", session_id: "b", turn_state: "waiting" });
  assert.deepEqual(state.attention, { b: true });
  applyEvent(state, { type: "session_state", session_id: "b", turn_state: "running" });
  assert.deepEqual(state.attention, {});

  applyEvent(state, { type: "attention", session_id: "b", turn_state: "waiting", message: "again" });
  applyEvent(state, { type: "selected", session_id: "b" });
  assert.deepEqual(state.attention, {}, "looking at it is enough");

  // On screen, only a failed turn is reported.
  applyEvent(state, { type: "attention", session_id: "b", turn_state: "waiting", message: "ask" });
  assert.equal(state.error, "");
  assert.deepEqual(state.attention, {});
  applyEvent(state, { type: "attention", session_id: "b", turn_state: "failed", message: "provider refused" });
  assert.equal(state.error, "provider refused");
});

test("a Needs you jump focuses the decision once its snapshot shows one", () => {
  const state = page("a");
  state.focusDecisionFor = "a";
  assert.deepEqual(applyEvent(state, session("a", { turn_state: "waiting", pending_question: { call_id: "q1" } })), ["focus_decision"]);
  assert.equal(state.focusDecisionFor, null);
  // Asked for, but the request was already answered: nothing to focus.
  // (The turn was waiting, so settling also refreshes the files.)
  state.focusDecisionFor = "a";
  assert.deepEqual(applyEvent(state, session("a")), ["reset_streaming", "refresh_files"]);
  assert.equal(state.focusDecisionFor, null);
});

test("a roster records which project owns each session", () => {
  const state = page("a");
  assert.deepEqual(applyEvent(state, { type: "roster", sessions: [row("a"), row("c", "running")] }), ["reconcile_roster"]);
  assert.deepEqual(state.sessionOwner, { a: "p1", c: "p1" });
  assert.deepEqual(state.sessions.map((entry) => entry.session_id), ["a", "c"]);
});

test("errors and lag are reported; other events are left to the page", () => {
  const state = page("a");
  applyEvent(state, { type: "error", message: "disk full" });
  assert.equal(state.error, "disk full");
  applyEvent(state, { type: "lagged", missed: 1 });
  assert.equal(state.error, "Missed 1 update; the view may be stale.");
  applyEvent(state, { type: "lagged", missed: 4 });
  assert.equal(state.error, "Missed 4 updates; the view may be stale.");
  for (const type of ["workspace", "attention_new", "project_error", "project_switched", "stream"]) {
    assert.equal(applyEvent(state, { type }), null);
  }
});
