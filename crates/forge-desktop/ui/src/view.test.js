import { test } from "node:test";
import assert from "node:assert/strict";
import {
  closeDocument,
  attentionLabel,
  streamSegments,
  flattenTree,
  foldersToLoad,
  windowRange,
  projectStatus,
  prunable,
  defaultView,
  diffSide,
  dirtyAnywhere,
  dirtyIn,
  effectiveScope,
  escapeReturn,
  fileCounts,
  openDocument,
  scopeEntries,
  terminalHostClass,
  updateDocument,
  viewFor,
} from "./view.js";

const doc = (id, extra = {}) => ({ id, kind: "file", path: id.slice(5), title: id, dirty: false, ...extra });

test("a default view is a fresh record with nothing open", () => {
  const first = defaultView(), second = defaultView();
  assert.deepEqual(first, {
    screen: "session",
    drawerOpen: false,
    tabs: [],
    activeDocId: null,
    fileScope: "all",
    fileQuery: "",
    draft: "",
    scroll: 0,
    pinned: true,
    expanded: [],
  });
  assert.notEqual(first, second);
  assert.notEqual(first.tabs, second.tabs, "records must not share the tab list");
  assert.notEqual(first.expanded, second.expanded, "records must not share open folders");
  first.tabs.push(doc("file:a"));
  assert.deepEqual(second.tabs, []);
});

test("viewFor creates the record in place and returns the same one after", () => {
  const views = {};
  const created = viewFor(views, "s1");
  assert.deepEqual(views.s1, defaultView());
  assert.equal(viewFor(views, "s1"), created, "a second call keeps the record");
  const other = viewFor(views, "s2");
  assert.notEqual(other, created);
  other.draft = "s2 draft";
  assert.equal(created.draft, "", "sessions keep separate records");
});

test("openDocument appends, refreshes by id and becomes active", () => {
  let record = { ...defaultView(), tabs: [doc("file:a")], activeDocId: "file:a" };
  const before = record;
  record = openDocument(record, doc("file:b"));
  assert.deepEqual(record.tabs.map((tab) => tab.id), ["file:a", "file:b"]);
  assert.equal(record.activeDocId, "file:b");
  assert.deepEqual(before.tabs.map((tab) => tab.id), ["file:a"], "the input record is left alone");

  const reloaded = doc("file:a", { dirty: false, loading: false, file: { text: "fresh" } });
  const refreshed = openDocument(record, reloaded);
  assert.deepEqual(refreshed.tabs.map((tab) => tab.id), ["file:a", "file:b"], "refreshing keeps the order");
  assert.equal(refreshed.tabs[0], reloaded, "the refreshed document replaces the old entry");
  assert.equal(refreshed.activeDocId, "file:a");
});

test("closeDocument falls back to the last open document and ignores unknown ids", () => {
  const record = { ...defaultView(), tabs: [doc("file:a"), doc("file:b"), doc("file:c")], activeDocId: "file:c" };
  const closed = closeDocument(record, "file:c");
  assert.deepEqual(closed.tabs.map((tab) => tab.id), ["file:a", "file:b"]);
  assert.equal(closed.activeDocId, "file:b");

  const inactive = closeDocument(closed, "file:a");
  assert.equal(inactive.activeDocId, "file:b", "closing an inactive document keeps the active one");

  assert.equal(closeDocument(record, "file:missing"), record, "an unknown id returns the record unchanged");
  const emptied = closeDocument({ ...defaultView(), tabs: [doc("file:a")], activeDocId: "file:a" }, "file:a");
  assert.deepEqual(emptied.tabs, []);
  assert.equal(emptied.activeDocId, null);
});

test("updateDocument patches one document and leaves the rest identical", () => {
  const record = { ...defaultView(), tabs: [doc("file:a"), doc("file:b")], activeDocId: "file:a" };
  const updated = updateDocument(record, "file:b", { dirty: true, draft: "edited" });
  assert.equal(updated.tabs[1].dirty, true);
  assert.equal(updated.tabs[1].draft, "edited");
  assert.equal(updated.tabs[0], record.tabs[0], "untouched documents keep their identity");
  assert.equal(record.tabs[1].dirty, false, "the input record is left alone");
  assert.equal(updateDocument(record, "file:missing", { dirty: true }), record);
});

test("dirty checks cover empty records, empty stores and tolerates a bare record", () => {
  assert.equal(dirtyIn(defaultView()), false);
  assert.equal(dirtyIn({}), false);
  assert.equal(dirtyIn({ ...defaultView(), tabs: [doc("file:a", { dirty: true })] }), true);
  assert.equal(dirtyAnywhere({}), false);
  assert.equal(dirtyAnywhere({ s1: defaultView(), s2: defaultView() }), false);
  assert.equal(dirtyAnywhere({ s1: defaultView(), s2: { ...defaultView(), draft: "x", tabs: [doc("file:a", { dirty: true })] } }), true);
});

test("terminal host class covers every row of the table", () => {
  const views = {
    a: { ...defaultView(), screen: "session", drawerOpen: false },
    b: { ...defaultView(), screen: "session", drawerOpen: true },
    c: { ...defaultView(), screen: "files", drawerOpen: true },
    d: { ...defaultView(), screen: "terminal", drawerOpen: false },
    e: { ...defaultView(), screen: "terminal", drawerOpen: true },
  };
  assert.equal(terminalHostClass(views, "b", "a"), "hidden", "another session's host is hidden");
  assert.equal(terminalHostClass(views, "a", "a"), "hidden", "session screen, drawer closed");
  assert.equal(terminalHostClass(views, "b", "b"), "drawer");
  assert.equal(terminalHostClass(views, "c", "c"), "hidden", "Files hides the drawer");
  assert.equal(terminalHostClass(views, "d", "d"), "focused");
  assert.equal(terminalHostClass(views, "e", "e"), "focused", "focus wins over an open drawer");
  assert.equal(terminalHostClass(views, "x", "x"), "hidden", "no record yet");
});

const status = {
  branch: "main",
  files: [
    { path: "a.js", status: "M", staged: true, unstaged: true },
    { path: "b.js", status: "M", staged: false, unstaged: true },
    { path: "c.js", status: "A", staged: true, unstaged: false },
  ],
};

test("file scopes filter existing data and list partial staging twice", () => {
  const files = [{ path: "a.js" }, { path: "z.js" }];
  assert.equal(scopeEntries("all", { files, status }), files);
  assert.deepEqual(scopeEntries("changed", { files, status }).map((f) => f.path), ["a.js", "b.js", "c.js"]);
  assert.deepEqual(scopeEntries("staged", { files, status }).map((f) => f.path), ["a.js", "c.js"]);
  const last = scopeEntries("lastTurn", { files, status, lastTurnFiles: ["b.js", "gone.js"] });
  assert.deepEqual(last.map((f) => f.path), ["b.js", "gone.js"]);
  assert.equal(last[0].unstaged, true, "a last-turn row keeps its git state");
  assert.equal(last[1].staged, false, "a committed last-turn file has no git state");
  assert.equal(diffSide("staged"), "staged");
  assert.equal(diffSide("lastTurn"), "last_turn");
  assert.equal(diffSide("changed"), "combined");
  assert.equal(diffSide("all"), "combined");
});

test("Last turn is gated on a non-empty list and Stage all on unstaged files", () => {
  assert.equal(effectiveScope("lastTurn", []), "all");
  assert.equal(effectiveScope("lastTurn", ["a.js"]), "lastTurn");
  assert.equal(effectiveScope("staged", []), "staged");
  assert.deepEqual(fileCounts(status), { changed: 3, staged: 2, unstaged: 2 });
  const allStaged = { files: [{ path: "c.js", status: "A", staged: true, unstaged: false }] };
  assert.equal(fileCounts(allStaged).unstaged, 0, "Stage all disappears when nothing is unstaged");
  assert.deepEqual(fileCounts({ files: [] }), { changed: 0, staged: 0, unstaged: 0 });
  assert.deepEqual(fileCounts(undefined), { changed: 0, staged: 0, unstaged: 0 });
});

test("Escape leaves a focus screen only when no field or terminal has focus", () => {
  const inside = (selector) => ({ closest: (query) => (query.split(", ").includes(selector) ? {} : null) });
  const button = { closest: () => null };
  assert.equal(escapeReturn("session", button), null, "the primary screen leaves Escape to its control");
  assert.equal(escapeReturn("files", button), "files");
  assert.equal(escapeReturn("terminal", button), "terminal");
  assert.equal(escapeReturn("files", null), "files", "nothing focused");
  assert.equal(escapeReturn("files", inside("textarea")), null, "the editor keeps Escape");
  assert.equal(escapeReturn("files", inside("input")), null, "the filter keeps Escape");
  assert.equal(escapeReturn("terminal", inside(".terminal-host")), null, "Escape is terminal input");
});

test("a roster prunes only its own project's sessions", () => {
  const owners = { a1: "A", a2: "A", b1: "B" };
  const present = new Set(["a1"]);
  // b1 belongs to project B: switching to A must keep its draft and shell.
  assert.deepEqual(prunable(["a1", "a2", "b1", "new"], owners, "A", present), ["a2", "new"]);
  assert.deepEqual(prunable(["a1", "a2", "b1"], owners, "B", new Set(["b1"])), []);
});

test("a project row says whether it is closed, waiting on you, working or idle", () => {
  const project = (patch) => ({ open: true, attention: [], busy: 0, ...patch });
  assert.equal(projectStatus(project({ open: false, attention: [{}] })), "closed");
  // A request outranks running agents: the agent is blocked on you.
  assert.equal(projectStatus(project({ attention: [{}], busy: 2 })), "waiting");
  assert.equal(projectStatus(project({ busy: 1 })), "running");
  assert.equal(projectStatus(project({})), "idle");
  assert.equal(attentionLabel("child_approval"), "Agent approval");
  assert.equal(attentionLabel("unknown"), "Request");
});

test("the tree shows open folders' children, hides ignored entries, and marks gaps", () => {
  const entry = (path, is_dir, ignored = false) => ({ path, name: path.split("/").pop(), is_dir, ignored });
  const listings = {
    "": { entries: [entry("src", true), entry("zig-out", true, true), entry("build.zig", false)], truncated: false },
    src: { entries: [entry("src/big", true), entry("src/main.zig", false)], truncated: true },
  };
  const rows = (expanded, showIgnored) =>
    flattenTree(listings, expanded, showIgnored).map((row) => [row.path, row.kind, row.depth]);
  assert.deepEqual(rows([], false), [["src", "dir", 0], ["build.zig", "file", 0]]);
  assert.deepEqual(rows(["src", "src/big"], false), [
    ["src", "dir", 0],
    ["src/big", "dir", 1],
    ["src/big/…", "loading", 2],
    ["src/main.zig", "file", 1],
    ["src/+", "more", 1],
    ["build.zig", "file", 0],
  ]);
  assert.deepEqual(rows([], true).map(([path]) => path), ["src", "zig-out", "build.zig"]);
});

test("a refresh reloads only folders that are open and visible", () => {
  assert.deepEqual(foldersToLoad([]), [""]);
  // `a/b` is open but `a` is not: it is hidden, so it is not reloaded.
  assert.deepEqual(foldersToLoad(["x", "x/y", "a/b"]), ["", "x", "x/y"]);
});

test("only the rows on screen, plus overscan, are rendered", () => {
  assert.deepEqual(windowRange(10_000, 0, 240, 24, 5), { start: 0, end: 15 });
  assert.deepEqual(windowRange(10_000, 24_000, 240, 24, 5), { start: 995, end: 1015 });
  assert.deepEqual(windowRange(12, 0, 2400, 24, 5), { start: 0, end: 12 });
  assert.deepEqual(windowRange(0, 0, 240, 24), { start: 0, end: 0 });
});

test("streamed markdown splits at blank lines outside code fences", () => {
  const text = "# Title\n\nFirst para\nstill first\n\n```js\nconst a = 1;\n\nconst b = 2;\n```\n\nTail grow";
  const segments = streamSegments(text);
  assert.equal(segments.join(""), text, "nothing is lost or duplicated");
  assert.deepEqual(segments, [
    "# Title\n\n",
    "First para\nstill first\n\n",
    "```js\nconst a = 1;\n\nconst b = 2;\n```\n\n",
    "Tail grow",
  ]);
  // Inside an open fence nothing is final yet.
  assert.deepEqual(streamSegments("```\na\n\nb"), ["```\na\n\nb"]);
  assert.deepEqual(streamSegments(""), [""]);
  // Earlier blocks keep their exact text as the tail grows, so they are not re-rendered.
  const before = streamSegments("One\n\nTwo");
  const after = streamSegments("One\n\nTwo and more");
  assert.equal(before[0], after[0]);
});
