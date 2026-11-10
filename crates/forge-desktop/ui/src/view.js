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
    // Folders open in the Files tree; restored with the session (D9).
    expanded: [],
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

// One word for a project's row: closed, waiting on you, working, or idle.
// Waiting wins over working: a request blocks an agent until you answer.
export function projectStatus(project) {
  if (!project.open) return "closed";
  if (project.attention.length) return "waiting";
  if (project.busy > 0) return "running";
  return "idle";
}

const ATTENTION_KIND = { approval: "Approval", question: "Question", child_approval: "Agent approval" };

export function attentionLabel(kind) {
  return ATTENTION_KIND[kind] ?? "Request";
}

// The Files tree loads one folder at a time. `listings` maps a folder path
// ("" is the workspace root) to its `{ entries, truncated }` from `list_dir`.
// Returns the visible rows in tree order: an open folder's children follow it,
// a folder still loading shows a `loading` row, and a folder cut at the
// per-folder cap ends with a `more` row. Ignored entries are left out unless
// `showIgnored`.
export function flattenTree(listings, expanded, showIgnored) {
  const open = new Set(expanded);
  const rows = [];
  const walk = (folder, depth) => {
    const listing = listings[folder];
    if (!listing) {
      rows.push({ path: `${folder}/…`, kind: "loading", depth });
      return;
    }
    for (const entry of listing.entries) {
      if (entry.ignored && !showIgnored) continue;
      const isOpen = entry.is_dir && open.has(entry.path);
      rows.push({ ...entry, kind: entry.is_dir ? "dir" : "file", depth, open: isOpen });
      if (isOpen) walk(entry.path, depth + 1);
    }
    if (listing.truncated) rows.push({ path: `${folder}/+`, kind: "more", depth });
  };
  walk("", 0);
  return rows;
}

// Folders a refresh reloads: the root and every open folder whose parents are
// all open too (a closed parent hides them, so they need no request).
export function foldersToLoad(expanded) {
  const open = new Set(expanded);
  const visible = expanded.filter((path) => {
    const parts = path.split("/");
    return parts.slice(1).every((_, index) => open.has(parts.slice(0, index + 1).join("/")));
  });
  return ["", ...visible];
}

// Rows to render for a fixed-height list scrolled to `scrollTop` in a
// `viewport`-tall box, with `overscan` extra rows on each side.
export function windowRange(count, scrollTop, viewport, rowHeight, overscan = 8) {
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan);
  const end = Math.min(count, Math.ceil((scrollTop + viewport) / rowHeight) + overscan);
  return { start, end: Math.max(start, end) };
}
