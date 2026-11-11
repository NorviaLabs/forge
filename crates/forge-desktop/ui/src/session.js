// The on-screen session's snapshot and the page's reaction to backend
// events. Plain module with no Svelte imports: the rules about which session
// an event belongs to are unit-tested here, away from the page.

import { shownMessages, viewFor } from "./view.js";

// What the page shows before a session's first snapshot arrives. Selecting
// a session installs this, so nothing of the previous session lingers.
export function emptySnapshot() {
  return {
    turnState: "idle",
    messages: [],
    promptHistory: [],
    skillDescriptions: [],
    approval: null,
    question: null,
    waitingOn: null,
    approveAll: false,
    hostQuestions: [],
    thinking: false,
    imageInput: false,
    thinkingSwitchable: false,
    plan: null,
    tokens: null,
    background: [],
    queue: [],
    interrupted: [],
    counts: { tools: 0, skills: 0, allows: 0 },
    info: { workspace: "", branch: "", model: "", contextUsage: 0, queued: 0 },
    githubIssue: null,
    githubPr: "",
  };
}

// The backend's `session` event as the page shows it. `record` is that
// session's view record: it decides which messages are visible.
export function snapshotFrom(event, record) {
  return {
    turnState: event.turn_state,
    messages: shownMessages(record, event.messages),
    promptHistory: event.messages
      .filter((message) => message.role === "user" && message.text.trim())
      .map((message) => message.text)
      .slice(-500),
    skillDescriptions: event.skill_descriptions ?? [],
    approval: event.pending_approval ?? null,
    question: event.pending_question ?? null,
    waitingOn: event.waiting_on ?? null,
    approveAll: event.approve_all,
    hostQuestions: event.host_questions ?? [],
    thinking: event.thinking,
    imageInput: event.image_input ?? false,
    thinkingSwitchable: event.thinking_switchable ?? false,
    plan: event.plan ?? null,
    tokens: event.tokens ?? null,
    background: event.background ?? [],
    queue: event.queue ?? [],
    interrupted: event.interrupted ?? [],
    counts: { tools: event.tools ?? 0, skills: event.skills ?? 0, allows: event.pattern_allows ?? 0 },
    info: {
      workspace: event.workspace,
      branch: event.branch,
      model: event.model,
      contextUsage: event.context_usage,
      queued: event.queued,
    },
    githubIssue: event.github_issue_number ?? null,
    githubPr: event.github_pr_url ?? "",
  };
}

// What a waiting session needs when no decision card says so: the words to
// show above the composer and, when there is somewhere to go, where. Null
// when the session is not waiting or a card already asks.
export function waitingNote(snapshot) {
  if (snapshot.turnState !== "waiting" || snapshot.approval || snapshot.question) return null;
  if (snapshot.background.some((task) => task.awaiting_approval || task.approval)) {
    return { text: "A background task of this session needs your approval.", action: "agents", label: "Review" };
  }
  // A waiting session takes no new prompt, so every other case ends the
  // same way: stop it, then carry on.
  const waiting = snapshot.waitingOn;
  const what = waiting?.kind === "reply"
    ? "Forge is waiting for a reply it has no way to collect here."
    : waiting?.kind === "configuration"
      ? `Forge is waiting for a missing setting: ${waiting.detail}.`
      : waiting?.kind === "external"
        ? `Forge is waiting for something outside Forge: ${waiting.detail}`
        : "This session is waiting on a request that is no longer open.";
  return { text: `${what} Stop the session to continue with a new prompt.`, action: "stop", label: "Stop session" };
}

const busy = (turnState) => turnState === "running" || turnState === "waiting";

function setRowState(page, id, turnState) {
  page.sessions = page.sessions.map((session) =>
    session.session_id === id ? { ...session, turn_state: turnState } : session);
}

// A session that is no longer waiting no longer needs the operator.
function settleAttention(page, id, turnState) {
  if (turnState === "waiting" || !page.attention[id]) return;
  const { [id]: _, ...rest } = page.attention;
  page.attention = rest;
}

// Applies a backend event to the page's state and returns what the page
// must then do: any of "refresh_files", "focus_decision", "reset_streaming",
// "close_session_dialogs", "reconcile_roster". Returns null for an event
// this module does not handle.
//
// `page` holds `sessionId`, `snapshot`, `sessions`, `attention`, `error`,
// `views`, `lastTurn`, `focusDecisionFor`, `projectId` and `sessionOwner`.
export function applyEvent(page, event) {
  switch (event.type) {
    case "session_state":
      // A session not on screen changed state; the bridge sends only this.
      setRowState(page, event.session_id, event.turn_state);
      settleAttention(page, event.session_id, event.turn_state);
      return [];
    case "session": {
      setRowState(page, event.session_id, event.turn_state);
      settleAttention(page, event.session_id, event.turn_state);
      // A snapshot for a session the operator has left is not shown.
      if (event.session_id !== page.sessionId) return [];
      const effects = [];
      const wasBusy = busy(page.lastTurn);
      page.lastTurn = event.turn_state;
      page.snapshot = snapshotFrom(event, viewFor(page.views, event.session_id));
      if (page.focusDecisionFor === event.session_id) {
        page.focusDecisionFor = null;
        if (event.pending_question || event.pending_approval) effects.push("focus_decision");
      }
      // Keep the live delta while the turn runs; a mid-turn snapshot would
      // wipe it. Clear once the turn settles so the transcript owns it.
      if (!busy(event.turn_state)) {
        effects.push("reset_streaming");
        if (wasBusy) effects.push("refresh_files");
      }
      return effects;
    }
    case "roster":
      for (const row of event.sessions) page.sessionOwner[row.session_id] = page.projectId;
      page.sessions = event.sessions;
      return ["reconcile_roster"];
    case "selected": {
      const next = event.session_id ?? "";
      if (next === page.sessionId) return [];
      page.sessionId = next;
      page.snapshot = emptySnapshot();
      page.lastTurn = "";
      const effects = ["reset_streaming", "close_session_dialogs"];
      if (next) {
        const { [next]: _, ...rest } = page.attention;
        page.attention = rest;
        // The session's record already holds its documents, draft, files
        // and drawer; selection only changes which record renders.
        viewFor(page.views, next);
        effects.push("refresh_files");
      }
      return effects;
    }
    case "attention":
      setRowState(page, event.session_id, event.turn_state);
      if (event.session_id !== page.sessionId) {
        page.attention = { ...page.attention, [event.session_id]: true };
      } else if (event.turn_state === "failed") {
        // Finishing, stopping or waiting for input is normal; only a failed
        // turn deserves the red banner.
        page.error = event.message;
      }
      return [];
    case "error":
      page.error = event.message;
      return [];
    case "lagged":
      page.error = `Missed ${event.missed} update${event.missed === 1 ? "" : "s"}; the view may be stale.`;
      return [];
    default:
      return null;
  }
}
