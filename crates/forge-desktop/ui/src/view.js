// Per-session view state: one record per session, no park or restore step.
// Plain module with no Svelte imports so the record rules stay unit-testable.

export function defaultView() {
  return {
    screen: "session",
    drawerOpen: false,
    tabs: [],
    activeDocId: null,
    fileScope: "all",
    fileQuery: "",
    draft: "",
    scroll: 0,
    pinned: true,
  };
}

// Returns the record for a session, creating one in place when it is absent.
export function viewFor(views, sessionId) {
  if (!views[sessionId]) views[sessionId] = defaultView();
  return views[sessionId];
}

// Opens a document, or refreshes it in place when the id is already open.
export function openDocument(record, doc) {
  const tabs = record.tabs.some((tab) => tab.id === doc.id)
    ? record.tabs.map((tab) => (tab.id === doc.id ? doc : tab))
    : [...record.tabs, doc];
  return { ...record, tabs, activeDocId: doc.id };
}

// Closing the active document falls back to the last one still open.
export function closeDocument(record, id) {
  const tabs = record.tabs.filter((tab) => tab.id !== id);
  if (tabs.length === record.tabs.length) return record;
  const activeDocId = record.activeDocId === id ? (tabs.at(-1)?.id ?? null) : record.activeDocId;
  return { ...record, tabs, activeDocId };
}

export function updateDocument(record, id, patch) {
  let found = false;
  const tabs = record.tabs.map((tab) => {
    if (tab.id !== id) return tab;
    found = true;
    return { ...tab, ...patch };
  });
  return found ? { ...record, tabs } : record;
}

export function dirtyIn(record) {
  return (record?.tabs ?? []).some((tab) => tab.dirty);
}

export function dirtyAnywhere(views) {
  return Object.values(views).some((record) => dirtyIn(record));
}

// Which way a session's terminal host renders. The rows are exhaustive: the
// host is never unmounted while its session lives, only shown or hidden.
export function terminalHostClass(views, id, sessionId) {
  const view = views[id];
  if (id !== sessionId || !view) return "hidden";
  if (view.screen === "terminal") return "focused";
  if (view.screen === "session" && view.drawerOpen) return "drawer";
  return "hidden";
}

// The Files scopes are filters over data the app already has. A partially
// staged file is listed under both Changed and Staged.
export function scopeEntries(scope, { files = [], status = { files: [] }, lastTurnFiles = [] }) {
  if (scope === "changed") return status.files;
  if (scope === "staged") return status.files.filter((file) => file.staged);
  if (scope === "lastTurn") {
    const byPath = new Map(status.files.map((file) => [file.path, file]));
    return lastTurnFiles.map((path) => byPath.get(path) ?? { path, status: "", staged: false, unstaged: false });
  }
  return files;
}

// The diff a row opens depends on the scope it was opened from.
export function diffSide(scope) {
  if (scope === "staged") return "staged";
  if (scope === "lastTurn") return "last_turn";
  return "combined";
}

export function fileCounts(status) {
  const files = status?.files ?? [];
  return {
    changed: files.length,
    staged: files.filter((file) => file.staged).length,
    unstaged: files.filter((file) => file.unstaged).length,
  };
}

// The Last turn scope only exists while there is a last turn to show; a
// scope chip that vanished must not leave the list filtered by it.
export function effectiveScope(scope, lastTurnFiles) {
  return scope === "lastTurn" && lastTurnFiles.length === 0 ? "all" : scope;
}

// Escape leaves a focus screen only when no editor, field or terminal has
// focus; otherwise it belongs to that control. Returns the header control
// that takes focus afterwards, or null when Escape is not ours.
export function escapeReturn(screen, active) {
  if (screen === "session") return null;
  if (active?.closest?.("textarea, input, .terminal-host")) return null;
  return screen === "terminal" ? "terminal" : "files";
}

// Sessions belong to one project. Views and shells of other projects are kept
// across a switch (D9), so a roster may only prune its own project's records.
// `owners` maps session id -> project id; an unknown session is treated as
// the current project's.
export function prunable(ids, owners, project, present) {
  return ids.filter((id) => (owners[id] ?? project) === project && !present.has(id));
}

// Requests waiting in projects other than the one on screen, oldest first.
export function otherAttention(projects, activeId) {
  return projects
    .filter((project) => project.id !== activeId)
    .flatMap((project) =>
      project.attention.map((item) => ({ ...item, project_id: project.id, project_name: project.name })),
    )
    .sort((a, b) => a.since.localeCompare(b.since));
}
