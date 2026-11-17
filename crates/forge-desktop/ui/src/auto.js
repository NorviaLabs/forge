// Auto for the main session, as the page shows it: the model picker's
// label, and each Auto turn's model, effort and why. The bridge sends
// `auto` (the fallback and ladder position, or null) and `auto_turns` (one
// decision per turn Auto chose the model of).

const SOURCE_TEXT = {
  ranked: "ranked best",
  fallback: "fallback",
  stepped_up: "stepped up after a failed turn",
  stepped_down: "stepped down after two good turns",
};

/** A model id without its provider: `anthropic/claude-opus-4-1` → `claude-opus-4-1`. */
export function modelName(id) {
  if (!id) return "";
  const slash = id.indexOf("/");
  return slash < 0 ? id : id.slice(slash + 1);
}

function withEffort(model, effort) {
  return `${modelName(model)} @ ${effort || "default"}`;
}

/** One Auto turn's label: "Auto · claude-opus-4-1 @ high · ranked best". */
export function autoTurnLabel(decision) {
  const why = SOURCE_TEXT[decision.source] || decision.source;
  return `Auto · ${withEffort(decision.model, decision.reasoning_effort)} · ${why}`;
}

/** The full reason, for the label's title. */
export function autoTitle(decision) {
  return decision.reason || "";
}

/**
 * Labels by the user message (1 = the first) each Auto turn answers. A turn
 * decided again keeps its latest decision.
 */
export function autoLabelsByMessage(decisions) {
  const labels = new Map();
  for (const decision of decisions || []) {
    labels.set(decision.user_message, autoTurnLabel(decision));
  }
  return labels;
}

/** Reasons by user message, alongside `autoLabelsByMessage`. */
export function autoTitlesByMessage(decisions) {
  const titles = new Map();
  for (const decision of decisions || []) {
    titles.set(decision.user_message, autoTitle(decision));
  }
  return titles;
}

/** The picker's label: Auto and its fallback, or the session's model. */
export function pickerLabel(info) {
  if (info && info.auto) {
    const fallback = info.auto.fallback || {};
    return `Auto (fallback: ${withEffort(fallback.model, fallback.reasoning_effort)})`;
  }
  return modelName(info && info.model);
}

/** The arguments of `set_auto_model`: the selected model and effort as the fallback. */
export function autoRequest(sessionId, modelId, profileId, effort) {
  return { sessionId, modelId, profileId, effort: effort || null };
}

/**
 * For each message on screen (`messages.slice(from)`): its Auto label and
 * reason, or null. Labels belong to user messages, counted from the very
 * first — so a label stays with its turn after `/clear` hides earlier ones.
 */
export function autoLabelsForShown(messages, from, decisions) {
  const labels = autoLabelsByMessage(decisions);
  const titles = autoTitlesByMessage(decisions);
  let userMessages = 0;
  const all = messages.map((message) => {
    if (message.role !== "user") return null;
    userMessages += 1;
    const label = labels.get(userMessages);
    return label ? { label, title: titles.get(userMessages) } : null;
  });
  return all.slice(from);
}
