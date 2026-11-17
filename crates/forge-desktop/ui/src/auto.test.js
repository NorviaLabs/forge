import { test } from "node:test";
import assert from "node:assert/strict";
import {
  autoLabelsByMessage,
  autoLabelsForShown,
  autoRequest,
  autoTitle,
  autoTurnLabel,
  modelName,
  pickerLabel,
} from "./auto.js";

const decision = (source, extra = {}) => ({
  task_id: 3,
  user_message: 2,
  model: "anthropic/claude-opus-4-1",
  profile_id: "anthropic",
  reasoning_effort: "high",
  source,
  step: source === "stepped_up" ? 1 : 0,
  steps: 4,
  reason: "anthropic/claude-opus-4-1 @ high ranks 1 of 2 anthropic models for main work",
  ...extra,
});

test("a model id is shown without its provider", () => {
  assert.equal(modelName("anthropic/claude-opus-4-1"), "claude-opus-4-1");
  assert.equal(modelName("mock"), "mock");
  assert.equal(modelName(""), "");
});

test("each Auto turn says what it ran on and why", () => {
  assert.equal(autoTurnLabel(decision("ranked")), "Auto · claude-opus-4-1 @ high · ranked best");
  assert.equal(autoTurnLabel(decision("fallback")), "Auto · claude-opus-4-1 @ high · fallback");
  assert.equal(
    autoTurnLabel(decision("stepped_up")),
    "Auto · claude-opus-4-1 @ high · stepped up after a failed turn",
  );
  assert.equal(
    autoTurnLabel(decision("stepped_down")),
    "Auto · claude-opus-4-1 @ high · stepped down after two good turns",
  );
  assert.equal(
    autoTurnLabel(decision("ranked", { reasoning_effort: null })),
    "Auto · claude-opus-4-1 @ default · ranked best",
  );
});

test("a source this page does not know is still shown", () => {
  assert.equal(autoTurnLabel(decision("something_new")), "Auto · claude-opus-4-1 @ high · something_new");
});

test("the full reason is the label's title", () => {
  assert.equal(autoTitle(decision("ranked")), decision("ranked").reason);
});

test("labels are keyed by the user message that started each turn", () => {
  const labels = autoLabelsByMessage([
    decision("ranked", { user_message: 1 }),
    decision("stepped_up", { user_message: 2 }),
  ]);
  assert.equal(labels.get(1), "Auto · claude-opus-4-1 @ high · ranked best");
  assert.equal(labels.get(2), "Auto · claude-opus-4-1 @ high · stepped up after a failed turn");
  assert.equal(labels.get(3), undefined);
  assert.equal(autoLabelsByMessage(undefined).size, 0);
});

test("a turn decided twice keeps its latest decision", () => {
  const labels = autoLabelsByMessage([
    decision("ranked", { user_message: 1 }),
    decision("stepped_up", { user_message: 1 }),
  ]);
  assert.equal(labels.get(1), "Auto · claude-opus-4-1 @ high · stepped up after a failed turn");
});

test("the picker shows Auto and its fallback, or the model", () => {
  assert.equal(
    pickerLabel({
      model: "anthropic/claude-opus-4-1",
      auto: { fallback: { model: "anthropic/claude-sonnet-4", reasoning_effort: "medium" }, position: 1 },
    }),
    "Auto (fallback: claude-sonnet-4 @ medium)",
  );
  assert.equal(
    pickerLabel({ model: "x/y", auto: { fallback: { model: "x/z", reasoning_effort: null }, position: 0 } }),
    "Auto (fallback: z @ default)",
  );
  assert.equal(pickerLabel({ model: "anthropic/claude-sonnet-4", auto: null }), "claude-sonnet-4");
  assert.equal(pickerLabel({ model: "anthropic/claude-sonnet-4" }), "claude-sonnet-4");
});

test("choosing Auto sends the selected model and effort as the fallback", () => {
  assert.deepEqual(autoRequest("s1", "anthropic/claude-sonnet-4", "anthropic", "medium"), {
    sessionId: "s1",
    modelId: "anthropic/claude-sonnet-4",
    profileId: "anthropic",
    effort: "medium",
  });
  assert.deepEqual(autoRequest("s1", "x/y", "x", ""), {
    sessionId: "s1",
    modelId: "x/y",
    profileId: "x",
    effort: null,
  });
});

test("labels line up with the messages on screen, counting every user message", () => {
  const messages = [
    { role: "user", text: "one" },
    { role: "assistant", text: "a" },
    { role: "user", text: "two" },
    { role: "tool", text: "t" },
    { role: "assistant", text: "b" },
  ];
  const turns = [decision("ranked", { user_message: 1 }), decision("stepped_up", { user_message: 2 })];
  assert.deepEqual(autoLabelsForShown(messages, 0, turns), [
    { label: "Auto · claude-opus-4-1 @ high · ranked best", title: turns[0].reason },
    null,
    { label: "Auto · claude-opus-4-1 @ high · stepped up after a failed turn", title: turns[1].reason },
    null,
    null,
  ]);
  // After /clear hid the first two messages, the second user message is
  // still the second: its label stays with it.
  assert.deepEqual(autoLabelsForShown(messages, 2, turns), [
    { label: "Auto · claude-opus-4-1 @ high · stepped up after a failed turn", title: turns[1].reason },
    null,
    null,
  ]);
  assert.deepEqual(autoLabelsForShown(messages, 0, []), [null, null, null, null, null]);
  assert.deepEqual(autoLabelsForShown(messages, 0, undefined), [null, null, null, null, null]);
});
