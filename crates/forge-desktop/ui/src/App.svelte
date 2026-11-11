<script>
  import { onMount, tick } from "svelte";
  import { Channel } from "@tauri-apps/api/core";
  import { invoke } from "./ipc.js";
  import { failureAction } from "./failure.js";
  import { chord, isMac } from "./keys.js";
  import { applyEvent, emptySnapshot, snapshotFrom, waitingNote } from "./session.js";
  import { listen } from "@tauri-apps/api/event";
  import Conversation from "./lib/Conversation.svelte";
  import Changes from "./lib/Changes.svelte";
  import Settings from "./lib/Settings.svelte";
  import Composer from "./lib/Composer.svelte";
  import Sidebar from "./lib/Sidebar.svelte";
  import Inspector from "./lib/Inspector.svelte";
  import ApprovalCard from "./lib/ApprovalCard.svelte";
  import QuestionCard from "./lib/QuestionCard.svelte";
  import Files from "./lib/Files.svelte";
  import ModelPicker from "./lib/ModelPicker.svelte";
  import Palette from "./lib/Palette.svelte";
  import BranchPicker from "./lib/BranchPicker.svelte";
  import TrustScreen from "./lib/TrustScreen.svelte";
  import Providers from "./lib/Providers.svelte";
  import Github from "./lib/Github.svelte";
  import Details from "./lib/Details.svelte";
  import Help from "./lib/Help.svelte";
  import { modal } from "./modal.js";
  import { chime, setSoundEnabled, soundEnabled, unlockOnFirstGesture } from "./chime.js";
  import {
    applyRefresh, attentionLabel, beginRefresh, clearShown, failRefresh, flattenTree, foldersToLoad,
    keepDraft, projectStatus, prunable,
  } from "./view.js";
  import Icon from "./lib/Icon.svelte";
  import { withSkills } from "./slash.js";
  import {
    closeDocument,
    defaultView,
    dirtyAnywhere,
    dirtyIn,
    escapeReturn,
    fileCounts,
    openDocument,
    terminalHostClass,
    updateDocument,
    viewFor,
  } from "./view.js";

  let sessionId = $state("");
  let sessions = $state([]);
  // False for a folder that isn't a Git repository: worktree, branch, issue
  // and pull-request actions stay listed but disabled, with the reason.
  let supportsGit = $state(true);
  const NEEDS_GIT = "needs a Git repository";

  // Projects (doc 2). `projectId` is the project on screen. Each session
  // belongs to one project, recorded in `sessionOwner`, so a roster prunes
  // only its own project's views and shells: switching keeps the others'
  // drafts, scroll positions and running terminals (D9).
  let projectId = $state("");
  let projects = $state([]);
  const sessionOwner = {};
  let projectBusy = $state(false);
  // A project that can't open yet: { project, kind, message }.
  let projectProblem = $state(null);
  // A busy close: { project, sessions }.
  let closeTarget = $state(null);
  let attentionSound = $state(soundEnabled());
  $effect(() => setSoundEnabled(attentionSound));

  let resumable = $state([]);
  // One view record per session (see view.js): open documents, the active
  // document, the draft and the transcript position.
  let views = $state({});
  const allSessions = $derived([
    ...sessions,
    ...resumable.filter((row) => !sessions.some((session) => session.session_id === row.session_id)),
  ]);
  // The on-screen session as the backend last described it (session.js),
  // replaced whole by each snapshot and emptied by a selection.
  let snapshot = $state(emptySnapshot());
  const turnState = $derived(snapshot.turnState);
  let stopping = $state(false);
  const messages = $derived(snapshot.messages);
  const promptHistory = $derived(snapshot.promptHistory);
  const skillDescriptions = $derived(snapshot.skillDescriptions);
  let streaming = $state("");
  // Deltas arrive once per token; the page applies them at most once per
  // frame, so a burst costs one render instead of one per token.
  let pendingDelta = "";
  let deltaFrame = 0;
  function queueDelta(text) {
    pendingDelta += text;
    if (deltaFrame) return;
    deltaFrame = requestAnimationFrame(() => {
      deltaFrame = 0;
      const started = performance.now();
      streaming += pendingDelta;
      pendingDelta = "";
      // Timing diagnostics: a streamed update that costs more than a frame
      // is what makes output fall behind the model.
      tick().then(() => {
        const ms = performance.now() - started;
        if (ms > 16) console.debug(`forge timing: streamed update rendered in ${ms.toFixed(1)} ms (${streaming.length} chars)`);
      });
    });
  }
  function resetStreaming() {
    if (deltaFrame) cancelAnimationFrame(deltaFrame);
    deltaFrame = 0;
    pendingDelta = "";
    streaming = "";
  }
  // The Session menu destination, or null when the menu is closed.
  let sessionMenu = $state(null);
  // At compact width the rail is an overlay opened from the header.
  let railOpen = $state(false);
  // A Needs you row asks for the decision card to take focus once the
  // selected session's snapshot shows it.
  let focusDecisionFor = $state(null);
  let filesButton = $state();
  let terminalButton = $state();
  let railButton = $state();
  let screenAnnouncement = $state("");
  const approval = $derived(snapshot.approval);
  const question = $derived(snapshot.question);
  const approveAll = $derived(snapshot.approveAll);
  const thinking = $derived(snapshot.thinking);
  const imageInput = $derived(snapshot.imageInput);
  const plan = $derived(snapshot.plan);
  const tokens = $derived(snapshot.tokens);
  const background = $derived(snapshot.background);
  const queue = $derived(snapshot.queue);
  const interrupted = $derived(snapshot.interrupted);
  const counts = $derived(snapshot.counts);
  let attention = $state({});
  let error = $state("");
  const info = $derived(snapshot.info);
  const githubIssue = $derived(snapshot.githubIssue);
  const githubPr = $derived(snapshot.githubPr);

  // The Files tree, one folder at a time: folder path ("" is the root) ->
  // `{ entries, truncated }`. Open folders live in the session's view record.
  // It is fetched per session and kept in that session's view record, with
  // its Git status and last-turn files.
  const listings = $derived(currentView.files.listings);
  const filesLoading = $derived(currentView.files.loading);
  const filesError = $derived(currentView.files.error);
  const status = $derived(currentView.files.status);
  const lastTurnFiles = $derived(currentView.files.lastTurnFiles);
  // Show entries .gitignore excludes (dependencies, build output).
  let showIgnored = $state(false);
  const fileTree = $derived(flattenTree(listings, currentView.expanded ?? [], showIgnored));

  let creating = $state(false);
  let pendingClose = $state("");
  let pickerOpen = $state(false);
  let providersOpen = $state(false);
  let githubOpen = $state(false);
  let detailsOpen = $state(false);
  let helpOpen = $state(false);
  let settingsOpen = $state(false);
  // The New session dialog: its name field, and whether it is open.
  let newSessionOpen = $state(false);
  let newSessionName = $state("");
  function openNewSession() {
    newSessionName = "";
    newSessionOpen = true;
  }
  async function submitNewSession(event) {
    event.preventDefault();
    const label = newSessionName.trim();
    if (!label || creating) return;
    newSessionOpen = false;
    await createSession(label);
  }
  const DARK_QUERY = "(prefers-color-scheme: dark)";
  let themes = $state([]);
  let themeChoice = $state("forge-dark");
  let systemDark = $state(window.matchMedia(DARK_QUERY).matches);
  let themeReady = $state(false);
  // `system` is resolved here: only the webview can observe the OS preference.
  const systemTheme = $derived(
    themes.find((entry) => entry.id === (systemDark ? "forge-dark" : "forge-light")) ??
      themes.find((entry) => entry.dark === systemDark) ??
      null,
  );
  const activeTheme = $derived(
    themeChoice === "system"
      ? systemTheme
      : (themes.find((entry) => entry.id === themeChoice) ?? systemTheme),
  );
  $effect(() => {
    const palette = activeTheme;
    if (!themeReady || !palette) return;
    const root = document.documentElement;
    for (const [name, value] of Object.entries(palette.colors)) {
      root.style.setProperty(`--${name}`, value);
    }
    root.dataset.theme = palette.dark ? "dark" : "light";
    root.style.colorScheme = palette.dark ? "dark" : "light";
    try { localStorage.setItem("forge.desktop.theme", themeChoice); } catch {}
  });
  let models = $state([]);
  let modelsLoading = $state(false);
  let modelsError = $state("");
  let paletteOpen = $state(false);
  let sidebar = $state();
  let filesPane = $state();
  let fileAction = $state(null);
  let archiveTarget = $state(null);
  let archiveRemove = $state(false);
  let archiveDiscard = $state(false);
  let attachOpen = $state(false);
  let attachPath = $state("");
  let attachLabel = $state("");
  let attachBranch = $state("");
  let attachBusy = $state(false);
  let renameTarget = $state(null);
  let renameLabel = $state("");
  let pathTarget = $state(null);
  let deleteTarget = $state(null);
  let commitOpen = $state(false);
  let commitMessage = $state("");
  let commitError = $state("");
  let gitBusy = $state(false);
  let gitConfirm = $state(null);
  let branchesOpen = $state(false);
  let branches = $state([]);
  let branchesLoading = $state(false);
  let branchesError = $state("");
  let quitRequested = $state(false);
  let approveAllPending = $state(false);
  let childView = $state(null);
  let TerminalComponent = $state(null);

  // Sessions whose shell was opened. Each keeps its terminal mounted, so
  // switching away does not kill the PTY or forget its scrollback.
  let shells = $state({});

  let lastTurn = "";

  const activeSession = $derived(sessions.find((session) => session.session_id === sessionId));
  const branch = $derived(status.branch || info.branch);
  // The record every block renders from. Before a session is selected this
  // is a throwaway record so a binding never writes through undefined.
  const currentView = $derived(views[sessionId] ?? defaultView());
  const activeDoc = $derived(currentView.tabs.find((tab) => tab.id === currentView.activeDocId) ?? null);

  // Relevance rules: each one gates exactly one control.
  const fileTotals = $derived(fileCounts(status));
  const currentStep = $derived(plan?.items?.find((item) => item.status === "in_progress") ?? null);
  const hasQueue = $derived(queue.length > 0);
  const hasDecision = $derived(!!(question || approval));
  const needsYou = $derived(Object.keys(attention).length);
  const terminalShown = $derived(currentView.drawerOpen && !!shells[sessionId]);
  const waitNote = $derived(waitingNote(snapshot));
  const currentProject = $derived(projects.find((project) => project.id === projectId) ?? null);
  // The word that always accompanies a state dot.
  function turnWord(state) {
    return {
      idle: "idle", running: "running", streaming: "running", waiting: "waiting on you", queued: "queued",
      completed: "done", failed: "failed", cancelled: "stopped", interrupted: "interrupted",
    }[state] ?? state;
  }

  // Document writes go to the record of the session that started them, so a
  // load or save that lands after a switch is kept, not dropped.
  function editRecord(owner, change) {
    if (!owner) return;
    views[owner] = change(viewFor(views, owner));
  }

  function patchDoc(owner, id, patch) {
    editRecord(owner, (record) => updateDocument(record, id, patch));
  }

  function docIn(owner, id) {
    return views[owner]?.tabs.find((tab) => tab.id === id) ?? null;
  }

  // Tell Rust whether a close would discard work; it asks before quitting.
  // Before the trust screen is answered there is no session to guard.
  $effect(() => {
    if (!booted) return;
    const unsaved = dirtyAnywhere(views);
    const busy = sessions.some((session) => ["running", "waiting"].includes(session.turn_state))
      || Object.keys(attention).length > 0;
    invoke("set_unsaved", { unsaved, busy }).catch(() => {});
  });

  // The Stop button reports its request until the turn reaches a terminal
  // state, so a slow cancellation is visible instead of silent.
  $effect(() => {
    if (turnState !== "running" && turnState !== "waiting") stopping = false;
  });

  // A session waiting out of sight is still a session waiting: the window
  // title is the only surface a background tab has.
  $effect(() => {
    const waiting = Object.keys(attention).length;
    document.title = waiting ? `(${waiting}) Forge — needs you` : "Forge";
  });

  // Slash commands the desktop can run. Autocomplete only offers these, and a
  // submitted exact match runs here instead of reaching the model.
  const slashCommands = [
    { name: "/help", help: "Show help and keyboard shortcuts", run: () => (helpOpen = true) },
    { name: "/connect", help: "Connect a provider or add a model", run: () => (providersOpen = true) },
    { name: "/disconnect", help: "Disconnect a provider", run: () => (providersOpen = true) },
    { name: "/model", help: "Switch model for future turns", run: openModelPicker },
    { name: "/effort", help: "Set the model's reasoning effort", run: openModelPicker },
    { name: "/theme", help: "Switch the desktop theme", run: () => (settingsOpen = true) },
    { name: "/status", help: "Show session status and token usage", run: () => (detailsOpen = true) },
    { name: "/context", help: "Show the context breakdown", run: () => (detailsOpen = true) },
    { name: "/plan", help: "Show the latest execution plan", run: () => (sessionMenu = "plan") },
    { name: "/tasks", help: "Show background tasks and queued prompts", run: () => (sessionMenu = "agents") },
    { name: "/approve-all", help: "Toggle approve-all for this session", run: toggleApproveAll },
    { name: "/thinking", help: "Toggle model reasoning", run: toggleThinking },
    { name: "/compact", help: "Continue in a fresh context", run: compactContext },
    { name: "/continue", help: "Continue the last stopped or failed turn", run: continueTurn },
    { name: "/new", help: "Start a new session", run: openNewSession },
    { name: "/sessions", help: "Switch to another session", run: () => (paletteOpen = true) },
    { name: "/resume", help: "Switch to a previous session", run: () => (paletteOpen = true) },
    { name: "/fork", help: "Fork this conversation into a new session", run: forkSession },
    { name: "/terminal", help: "Open the terminal panel", run: openTerminal },
    { name: "/clear", help: "Clear the transcript view", run: clearTranscript },
    {
      name: "/context-file",
      help: "Attach the open file to the next prompt",
      run: () => {
        if (activeDoc?.kind === "file") toggleFileAttachment(activeDoc);
        else error = "Open a file before attaching it.";
      },
    },
  ];

  const composerCommands = $derived(withSkills(slashCommands, skillDescriptions));

  const paletteItems = $derived.by(() => {
    const items = sessions.map((session) => {
      const closed = session.lifecycle !== "active";
      return {
        id: `session:${session.session_id}`,
        group: "Sessions",
        label: closed ? `${session.label} (closed)` : session.label,
        detail: closed ? "resume" : session.turn_state,
        action: () =>
          closed ? command("resume_session", { sessionId: session.session_id, currentSessionId: sessionId }) : selectSession(session.session_id),
      };
    });
    // Editor items act on the mounted FilePane, which only the Files screen has.
    const editing = currentView.screen === "files" ? activeDoc : null;
    if (editing?.kind === "file") {
      items.push(
        { id: "editor:save", group: "Editor", label: "Save current file", detail: chord("S"), action: saveActive },
        { id: "editor:reload", group: "Editor", label: "Reload current file from disk", action: () => requestFileAction("reload") },
      );
      if (editing.file && !editing.file.read_only) items.push(
        { id: "editor:find", group: "Editor", label: "Find in current file", detail: chord("F"), action: () => filesPane?.showNavigation("find") },
        { id: "editor:replace", group: "Editor", label: "Find and replace in current file", action: () => filesPane?.showNavigation("replace") },
        { id: "editor:line", group: "Editor", label: "Jump to line in current file", detail: chord("L"), action: () => filesPane?.showNavigation("line") },
      );
      const attached = currentView.attachment?.path === editing.path;
      items.push({
        id: "editor:attach",
        group: "Editor",
        label: attached ? "Remove attached file context" : "Attach current file to the next prompt",
        action: () => toggleFileAttachment(editing),
      });
    } else if (editing?.kind === "diff") {
      if (editing.side !== "last_turn") {
        items.push({ id: "editor:restore", group: "Editor", label: "Restore current file to HEAD…", action: () => requestFileAction("restore") });
      }
      // Only the combined view's hunk order matches the working-tree reverse
      // apply; a staged review can omit hunks and shift the index.
      if (editing.side === "combined" && !editing.diff?.truncated) {
        hunkLabels(editing.diff?.text ?? "").forEach((label, index) => items.push({
          id: `hunk:${index}`,
          group: "Git",
          label: `Discard hunk ${index + 1} (${label})…`,
          action: () => requestFileAction("hunk", index),
        }));
      }
    }
    if (activeDoc) {
      const path = activeDoc.path;
      items.push(
        { id: "diff:combined", group: "Git", label: "Review HEAD to working tree", action: () => openDiff(path, "combined") },
        { id: "diff:staged", group: "Git", label: "Review staged changes", action: () => openDiff(path, "staged") },
        { id: "diff:unstaged", group: "Git", label: "Review unstaged changes", action: () => openDiff(path, "unstaged") },
        { id: "git:stage", group: "Git", label: `Stage ${basename(path)}`, action: () => stagePath(path) },
        { id: "git:unstage", group: "Git", label: `Unstage ${basename(path)}`, action: () => unstagePath(path) },
      );
    }
    for (const path of lastTurnFiles) {
      items.push({
        id: `lastturn:${path}`,
        group: "Git",
        label: `Last turn: ${basename(path)}`,
        detail: path,
        action: () => openDiff(path, "last_turn"),
      });
    }
    items.push(
      {
        id: "action:terminal",
        group: "Actions",
        label: "Toggle terminal",
        detail: "⌃`",
        action: toggleTerminal,
      },
      { id: "action:files", group: "Actions", label: "Open Files", detail: chord("E"), action: openFiles },
      { id: "action:focus-terminal", group: "Actions", label: "Focus terminal", action: async () => { await openTerminal(); await focusTerminal(); } },
      { id: "menu:plan", group: "Session", label: "Plan", action: () => (sessionMenu = "plan") },
      { id: "menu:agents", group: "Session", label: "Agents & tasks", action: () => (sessionMenu = "agents") },
      { id: "menu:queue", group: "Session", label: "Queue", action: () => (sessionMenu = "queue") },
      { id: "menu:context", group: "Session", label: "Context & usage", action: () => (sessionMenu = "context") },
      {
        id: "action:model",
        group: "Actions",
        label: "Choose model",
        detail: "⌘M",
        action: openModelPicker,
      },
      {
        id: "action:refresh",
        group: "Actions",
        label: "Refresh files and changes",
        action: refreshFiles,
      },
      {
        id: "action:new",
        group: "Actions",
        label: "New session…",
        action: openNewSession,
      },
      {
        id: "action:approve-all",
        group: "Actions",
        label: approveAll ? "Stop trusting every command" : "Trust every command this session",
        detail: approveAll ? "on" : "off",
        action: toggleApproveAll,
      },
      {
        id: "action:thinking",
        group: "Actions",
        label: thinking ? "Hide model reasoning" : "Show model reasoning",
        detail: thinking ? "on" : "off",
        action: toggleThinking,
      },
      {
        id: "action:continue",
        group: "Actions",
        label: "Continue the last turn",
        action: continueTurn,
      },
      {
        id: "action:compact",
        group: "Actions",
        label: "Compact context",
        action: compactContext,
      },
      {
        id: "action:clear",
        group: "Actions",
        label: "Clear the transcript view",
        detail: "the model keeps its context",
        action: clearTranscript,
      },
      {
        id: "action:providers",
        group: "Actions",
        label: "Connect providers / add model…",
        action: () => (providersOpen = true),
      },
      {
        id: "action:github",
        group: "Actions",
        label: "GitHub issues / pull requests / CI…",
        action: () => (githubOpen = true),
      },
      { id: "action:status", group: "Actions", label: "Token, cost, tool and skill details…", action: () => (detailsOpen = true) },
      { id: "action:settings", group: "Actions", label: "Settings: theme, sound, providers, GitHub…", action: () => (settingsOpen = true) },
      { id: "action:help", group: "Actions", label: "Help and shortcuts…", action: () => (helpOpen = true) },
      { id: "git:fetch", group: "Git", label: "Fetch upstream", action: () => performGitOperation({ kind: "fetch" }) },
      { id: "git:pull", group: "Git", label: "Pull upstream…", action: () => requestGitOperation({ kind: "pull" }) },
      { id: "git:push", group: "Git", label: "Push upstream…", action: () => requestGitOperation({ kind: "push" }) },
      { id: "git:commit", group: "Git", label: "Commit staged changes…", action: openCommit },
      { id: "git:branches", group: "Git", label: "Branches and merges…", action: openBranches },
      {
        id: "action:fork",
        group: "Actions",
        label: "Fork this session",
        action: forkSession,
      },
      {
        id: "action:attach",
        group: "Sessions",
        label: "Attach an existing worktree…",
        action: openAttach,
      },
      {
        id: "action:rename",
        group: "Actions",
        label: "Rename this session…",
        action: () => activeSession && startRename(activeSession),
      },
    );
    // Every project, so switching works from the keyboard too.
    for (const project of projects) {
      if (project.id === projectId) continue;
      const status = projectStatus(project);
      items.push({
        id: `project:${project.id}`,
        group: "Projects",
        label: `Switch to ${project.name}`,
        detail: status === "waiting"
          ? `${project.attention.length} waiting on you`
          : status === "closed" ? "closed" : status === "running" ? `${project.running}/${project.limit} running` : project.root,
        action: () => switchProject(project),
      });
    }
    items.push({ id: "project:add", group: "Projects", label: "Add a project…", action: addProject });
    if (supportsGit) return items;
    return items.map((item) =>
      item.group === "Git" || item.id === "action:github" || item.id === "action:attach"
        ? { ...item, detail: NEEDS_GIT, action: () => (error = `${item.label.replace(/…$/, "")}: ${NEEDS_GIT}`) }
        : item,
    );
  });

  function basename(path) {
    const parts = path.split("/");
    return parts[parts.length - 1] || path;
  }

  // Show a snapshot of the session on screen (boot and focus resync; live
  // updates arrive through `onEvent`).
  function applySession(event) {
    snapshot = snapshotFrom(event, viewFor(views, event.session_id));
    if (!["running", "waiting"].includes(event.turn_state)) resetStreaming();
  }

  // The TUI watches the filesystem; a light poll keeps the tree and status
  // honest while the agent writes, without a watcher per session.
  $effect(() => {
    if (!booted) return;
    const timer = setInterval(() => {
      if (!document.hidden) refreshFiles();
    }, 3000);
    return () => clearInterval(timer);
  });

  // A missed event leaves the page on a stale turn state. The bridge resyncs
  // broadcast lag, but also fetch the live snapshot when the window returns
  // to the user.
  let syncing = false;
  async function syncSession() {
    if (!booted || !sessionId || syncing) return;
    syncing = true;
    try {
      const snapshot = await invoke("current_session", { sessionId });
      if (!snapshot || snapshot.session_id !== sessionId) return;
      const questionId = snapshot.pending_question?.call_id ?? "";
      const approvalId = snapshot.pending_approval?.call_id ?? "";
      const localTotal = messages.length + currentView.clearedFrom;
      const remoteLast = snapshot.messages[snapshot.messages.length - 1];
      const localLast = messages[messages.length - 1];
      const transcriptMoved = snapshot.messages.length !== localTotal
        || (remoteLast?.text ?? "") !== (localLast?.text ?? "")
        || (remoteLast?.role ?? "") !== (localLast?.role ?? "");
      if (snapshot.turn_state !== turnState
          || questionId !== (question?.call_id ?? "")
          || approvalId !== (approval?.call_id ?? "")
          || transcriptMoved) {
        applySession(snapshot);
      }
    } catch {}
    finally { syncing = false; }
  }

  // Refresh one session's workspace into its own record. The session need
  // not be on screen when the reply arrives.
  async function refreshFiles(owner = sessionId) {
    if (!owner) return;
    const record = viewFor(views, owner);
    const ticket = beginRefresh(record.files);
    try {
      // Reload the root and every open folder, never the whole tree. A
      // folder that has gone away (deleted, renamed) is simply closed.
      const folders = foldersToLoad(record.expanded);
      // A folder that isn't a Git repository has files but no Git status;
      // asking for it would fail and hide the file list too.
      const [loaded, nextStatus, written] = await Promise.all([
        Promise.all(folders.map((path) => invoke("list_dir", { sessionId: owner, path }).catch((cause) => (path ? null : Promise.reject(cause))))),
        supportsGit ? invoke("git_status", { sessionId: owner }) : { branch: "", files: [] },
        supportsGit ? invoke("git_last_turn_files", { sessionId: owner }) : [],
      ]);
      // The record may have been replaced (a document opened) meanwhile.
      const current = viewFor(views, owner);
      if (applyRefresh(current, ticket, { folders, loaded, status: nextStatus, lastTurnFiles: written })) {
        markStaleDiffs(owner, nextStatus);
      }
    } catch (cause) {
      failRefresh(viewFor(views, owner).files, ticket, String(cause));
    }
  }

  // Open or close a folder in the Files tree, loading it on first open.
  async function toggleDir(path) {
    const owner = sessionId;
    const record = viewFor(views, owner);
    if (record.expanded.includes(path)) {
      record.expanded = record.expanded.filter((entry) => entry !== path);
      return;
    }
    record.expanded = [...record.expanded, path];
    if (record.files.listings[path]) return;
    try {
      const listing = await invoke("list_dir", { sessionId: owner, path });
      viewFor(views, owner).files.listings[path] = listing;
    } catch (cause) {
      const current = viewFor(views, owner);
      current.expanded = current.expanded.filter((entry) => entry !== path);
      current.files.error = String(cause);
    }
  }

  // Quick Open items for the palette, from the workspace index.
  async function findFileItems(query) {
    if (!sessionId || !query.trim()) return [];
    const found = await invoke("find_files", { sessionId, query }).catch(() => []);
    return found.map((file) => ({
      id: `file:${file.path}`,
      group: "Files",
      label: basename(file.path),
      detail: file.path,
      action: () => openFile(file.path),
    }));
  }

  // The page state session.js reads and writes, by name.
  const page = {
    get sessionId() { return sessionId; }, set sessionId(value) { sessionId = value; },
    get snapshot() { return snapshot; }, set snapshot(value) { snapshot = value; },
    get sessions() { return sessions; }, set sessions(value) { sessions = value; },
    get attention() { return attention; }, set attention(value) { attention = value; },
    get error() { return error; }, set error(value) { error = value; },
    get lastTurn() { return lastTurn; }, set lastTurn(value) { lastTurn = value; },
    get focusDecisionFor() { return focusDecisionFor; }, set focusDecisionFor(value) { focusDecisionFor = value; },
    get views() { return views; },
    get projectId() { return projectId; },
    sessionOwner,
  };

  function onEvent(event) {
    if (event.type === "workspace") {
      projects = event.projects;
      return;
    }
    if (event.type === "attention_new") {
      if (event.sound && attentionSound) chime();
      return;
    }
    if (event.type === "project_error") {
      // The on-screen project's errors arrive as "error"; these come from
      // projects in the background, so they say which.
      error = `${event.project_name}: ${event.message}`;
      return;
    }
    if (event.type === "project_switched") {
      switchedTo(event.project_id, event.supports_git);
      return;
    }
    if (event.type === "stream" && event.event.type === "text_delta") {
      if (event.session_id === sessionId) queueDelta(event.event.text);
      return;
    }
    // Everything about sessions is decided in session.js; the page only
    // carries out what it asks for.
    for (const effect of applyEvent(page, event) ?? []) {
      if (effect === "reset_streaming") resetStreaming();
      else if (effect === "refresh_files") refreshFiles();
      else if (effect === "focus_decision") tick().then(() => focusDecision());
      else if (effect === "reconcile_roster") reconcileRoster(event.sessions);
      else if (effect === "close_session_dialogs") {
        // These dialogs act on the previously selected session.
        branchesOpen = false;
        commitOpen = false;
        gitConfirm = null;
        fileAction = null;
        childView = null;
      }
    }
  }

  async function command(name, args) {
    const mutatesGit = name.startsWith("git_");
    if (mutatesGit && gitBusy) { error = "Wait for the current Git operation to finish."; return false; }
    // Buffers live in each session's record and survive switches, so only
    // commands that end a session refuse unsaved documents.
    if (["close_session", "archive_session", "remove_worktree"].includes(name) && dirtySession(args?.sessionId)) {
      error = "Save or close unsaved files before changing this session.";
      return false;
    }
    // Every session command names its session; nothing is filled in here.
    const invokeArgs = args;
    if (mutatesGit) gitBusy = true;
    try {
      const result = await invoke(name, invokeArgs);
      if (name === "select_session") {
        // Selection must update the pane even if channel delivery is stalled.
        onEvent({ type: "selected", session_id: result.session_id });
        onEvent(result);
      }
      if (name === "stop_session") onEvent(result);
      if (["resume_session", "close_session", "archive_session", "remove_worktree"].includes(name)) loadResumable();
      return true;
    } catch (cause) {
      const action = failureAction(cause);
      // A session no open project knows: its row is stale, so drop it
      // instead of reporting an error the operator cannot act on.
      if (action === "forget") {
        resumable = resumable.filter((row) => row.session_id !== invokeArgs?.sessionId);
        loadResumable();
      } else if (action === "show") error = String(cause);
      return false;
    } finally { if (mutatesGit) gitBusy = false; }
  }

  async function loadResumable() {
    try {
      resumable = sessionId ? await invoke("list_resumable_sessions", { sessionId }) : [];
    } catch {
      resumable = [];
    }
  }

  function rememberProject(id) {
    if (!id) return;
    try { localStorage.setItem("forge.desktop.lastProject", id); } catch {}
  }

  // The bridge says another project is on screen; its roster, selection and
  // session follow as ordinary events.
  function switchedTo(id, git) {
    projectId = id;
    supportsGit = git !== false;
    attention = {};
    resumable = [];
    rememberProject(id);
    loadResumable();
  }

  // Put `project` on screen, opening it if needed. True when it worked.
  async function switchProject(project) {
    if (project.id === projectId) return true;
    projectBusy = true;
    try {
      await invoke("project_switch", { projectId: project.id });
      return true;
    } catch (failure) {
      projectFailed(project, failure);
      return false;
    } finally {
      projectBusy = false;
    }
  }

  function projectFailed(project, failure) {
    const kind = failure?.kind ?? "error";
    const message = failure?.message ?? String(failure);
    if (kind === "needs_trust" || kind === "root_missing") projectProblem = { project, kind, message };
    else if (kind === "busy") closeTarget = { project, sessions: failure.sessions ?? [] };
    else error = message;
  }

  async function addProject() {
    try {
      const id = await invoke("project_add");
      if (id) await switchProject({ id, name: "" });
    } catch (failure) {
      error = failure?.message ?? String(failure);
    }
  }

  async function trustProject() {
    const { project } = projectProblem;
    projectProblem = null;
    try {
      await invoke("project_trust", { projectId: project.id });
    } catch (failure) {
      error = failure?.message ?? String(failure);
      return;
    }
    await switchProject(project);
  }

  async function locateProject() {
    const { project } = projectProblem;
    try {
      if (!(await invoke("project_relocate", { projectId: project.id }))) return;
    } catch (failure) {
      error = failure?.message ?? String(failure);
      return;
    }
    projectProblem = null;
    await switchProject(project);
  }

  async function removeProject(project) {
    projectProblem = null;
    try {
      await invoke("project_remove", { projectId: project.id });
    } catch (failure) {
      error = failure?.message ?? String(failure);
    }
  }

  async function closeProject(project, stopRunning = false) {
    // The window always shows a project, so the one on screen is closed by
    // putting another open project there first. With no other, say so.
    if (project.id === projectId) {
      const other = projects.find((entry) => entry.open && entry.id !== projectId);
      if (!other) {
        projectProblem = { project, kind: "only_project", message: "" };
        return;
      }
      if (!(await switchProject(other))) return;
    }
    try {
      await invoke("project_close", { projectId: project.id, stopRunning });
      closeTarget = null;
    } catch (failure) {
      projectFailed(project, failure);
    }
  }

  // A request in another project: put that project on screen, then open the
  // session at its decision card.
  async function openElsewhere(item) {
    const project = projects.find((entry) => entry.id === item.project_id) ?? { id: item.project_id, name: item.project_name };
    if (!(await switchProject(project))) return;
    await openRequest(item.session_id);
  }

  async function selectSession(id) {
    railOpen = false;
    if (id === sessionId) return;
    await command("select_session", { sessionId: id });
  }

  // A Needs you row selects the session on its primary screen and asks the
  // decision card to take focus once the snapshot shows one.
  async function openRequest(id) {
    viewFor(views, id).screen = "session";
    if (id === sessionId) {
      railOpen = false;
      if (hasDecision) await tick().then(() => focusDecision());
      return;
    }
    focusDecisionFor = id;
    await selectSession(id);
  }

  function focusDecision() {
    document.querySelector(".decision button:not(:disabled)")?.focus();
  }

  async function createSession(label) {
    creating = true;
    const ok = await command("create_session", { label, githubIssueNumber: null });
    creating = false;
    if (ok) {
      // The roster and selection arrive on the event channel.
    }
  }

  async function createIssueSession(number, title) {
    creating = true;
    await command("create_session", {
      label: `#${number} ${title}`,
      githubIssueNumber: number,
    });
    creating = false;
  }

  async function archiveSession(session) {
    // Opening the confirmation is the whole action; `confirmArchive` runs it.
    // A row that is already closed was opened from its cleanup button, so its
    // dialog starts with removal ticked; an active row archives by default.
    archiveRemove = ["archived", "retained"].includes(session.lifecycle) && session.ownership === "managed";
    archiveDiscard = false;
    archiveTarget = session;
  }

  async function confirmArchive() {
    const session = archiveTarget;
    if (!session) return;
    const ok = archiveRemove
      ? await command("remove_worktree", {
          sessionId: session.session_id,
          discardDirty: archiveDiscard,
        })
      : await command("archive_session", { sessionId: session.session_id });
    if (!ok) return;
    archiveTarget = null;
    forgetAttention(session.session_id);
    dropShell(session.session_id);
  }

  function openAttach() {
    if (dirtyTabs()) {
      error = "Save or close unsaved files before attaching a worktree.";
      return;
    }
    attachPath = "";
    attachLabel = "";
    attachBranch = "";
    attachOpen = true;
  }

  async function submitAttach(event) {
    event.preventDefault();
    attachBusy = true;
    try {
      if (await command("attach_worktree", {
        workspace: attachPath.trim(),
        label: attachLabel.trim(),
        branch: attachBranch.trim(),
      })) {
        attachOpen = false;
      }
    } finally {
      attachBusy = false;
    }
  }

  // Unmount a session's terminal, which closes its shell.
  function dropShell(id) {
    if (!shells[id]) return;
    const { [id]: _, ...rest } = shells;
    shells = rest;
    const view = views[id];
    if (!view) return;
    view.drawerOpen = false;
    if (view.screen === "terminal") view.screen = "session";
  }

  // A session the supervisor dropped takes its shell and view record with it,
  // but a resumable session keeps both while its shell ends. Deleting a record
  // loses that session's draft and open documents, so confirm against a fresh
  // resumable list before deleting anything the roster no longer names.
  let reconcileRun = 0;
  async function reconcileRoster(rows) {
    const run = ++reconcileRun;
    const ids = [...Object.keys(shells), ...Object.keys(views)];
    const live = () => new Set([
      ...rows.map((row) => row.session_id),
      ...resumable.map((row) => row.session_id),
    ]);
    let present = live();
    if (prunable(ids, sessionOwner, projectId, present).length) {
      try {
        resumable = sessionId ? await invoke("list_resumable_sessions", { sessionId }) : [];
      } catch {
        return;
      }
      if (run !== reconcileRun) return;
      present = live();
    }
    for (const id of prunable(Object.keys(shells), sessionOwner, projectId, present)) dropShell(id);
    for (const id of prunable(Object.keys(views), sessionOwner, projectId, present)) delete views[id];
  }

  function toggleFileAttachment(tab) {
    const record = viewFor(views, sessionId);
    record.attachment = record.attachment?.path === tab.path
      ? null
      : { path: tab.path, line: Math.max(0, filesPane?.cursorLine() ?? 0) };
  }

  async function send(text, images = [], targetSession = sessionId) {
    error = "";
    if (!images.length) {
      const command = slashCommands.find((entry) => entry.name === text.trim());
      if (command) {
        command.run();
        return true;
      }
    }
    const attachment = views[targetSession]?.attachment ?? null;
    // Show the operator's line before the supervisor's snapshot arrives.
    // The next SessionUpdated replaces this local copy with the real message.
    const localId = `local:${Date.now()}:${Math.random().toString(36).slice(2)}`;
    const local = targetSession === sessionId && text
      ? { role: "user", text, attachments: [], optimistic: true, localId }
      : null;
    const previousTurnState = turnState;
    if (local) {
      snapshot.messages = [...messages, local];
      snapshot.turnState = "running";
      stopping = false;
    }
    let ok = false;
    try {
      if (!images.length) {
        ok = await command("submit_prompt", { sessionId: targetSession, text, attachment });
      } else {
        try {
          const encoded = await Promise.all(images.map((image) => new Promise((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => resolve(String(reader.result).split(",")[1]);
            reader.onerror = () => reject(new Error("Cannot read pasted image"));
            reader.readAsDataURL(image);
          })));
          ok = await command("submit_images", { sessionId: targetSession, text, images: encoded, attachment });
        } catch (cause) {
          error = String(cause);
          ok = false;
        }
      }
    } catch (cause) {
      error = String(cause);
      ok = false;
    }
    if (!ok && local) {
      if (targetSession === sessionId) {
        snapshot.messages = messages.filter((message) => message.localId !== localId);
        if (turnState === "running") snapshot.turnState = previousTurnState;
      }
      stopping = false;
    }
    if (ok && attachment && views[targetSession]) views[targetSession].attachment = null;
    return ok;
  }

  async function stop() {
    if (stopping || !sessionId) return;
    stopping = true;
    if (!(await command("stop_session", { sessionId }))) stopping = false;
  }

  async function decide(callId, decision, note, confirmed) {
    await command("decide_approval", { sessionId, callId, decision, note, confirmed });
  }

  async function answerQuestion(callId, answers, skip) {
    await command("answer_question", { sessionId, callId, answers, skip });
  }

  async function toggleApproveAll() {
    if (!approveAll) {
      approveAllPending = true;
      return;
    }
    await command("set_approve_all", { sessionId, on: false });
  }

  async function confirmApproveAll() {
    approveAllPending = false;
    await command("set_approve_all", { sessionId, on: true });
  }

  async function continueTurn() {
    await command("continue_turn", { sessionId });
  }

  async function compactContext() {
    await command("compact_context", { sessionId });
  }

  async function toggleThinking() {
    await command("set_thinking", { sessionId, enabled: !thinking });
  }

  async function expandMessage(index) {
    const target = sessionId;
    try {
      const record = viewFor(views, target);
      // Bound before the request: a `/clear` while it is in flight must
      // not shift which message this is.
      const absolute = record.clearedFrom + index;
      const full = await invoke("message_detail", { sessionId: target, index: absolute });
      record.messageDetails[absolute] = full;
      if (target === sessionId) {
        snapshot.messages = messages.map((message, at) =>
          at === index ? { ...message, text: full.text, thinking: full.thinking, expanded: true } : message);
      }
    } catch (cause) {
      if (target === sessionId) error = String(cause);
    }
  }

  function clearTranscript() {
    if (!sessionId) return;
    clearShown(viewFor(views, sessionId), messages.length);
    snapshot.messages = [];
  }

  async function openChild(task) {
    try {
      const messages = await invoke("child_transcript", {
        sessionId,
        taskId: task.id,
        childSessionId: task.child_session_id,
      });
      childView = { label: task.label, messages };
    } catch (cause) {
      error = String(cause);
    }
  }

  function insertTask(task) {
    if (currentView.draft.trim()) {
      error = "Finish or clear the composer before inserting a background result.";
      return;
    }
    currentView.draft = (task.latest_message || task.detail || "").trim();
  }

  async function editQueued(item, position) {
    const owner = sessionId;
    if (currentView.draft.trim()) {
      error = "Finish or clear the composer before editing queued text.";
      return;
    }
    if (await command("cancel_queued", {
      sessionId,
      queueId: item.id,
      position,
      source: item.source,
    })) viewFor(views, owner).draft = item.text;
  }

  function restoreInterrupted(text) {
    if (currentView.draft.trim()) {
      error = "Finish or clear the composer before restoring a prompt.";
      return;
    }
    currentView.draft = text;
  }

  async function stopSession(session) {
    await command("stop_session", { sessionId: session.session_id });
  }

  function forgetAttention(id) {
    if (!attention[id]) return;
    const { [id]: ignored, ...rest } = attention;
    attention = rest;
  }

  async function closeSession(session) {
    if (await command("close_session", { sessionId: session.session_id })) {
      forgetAttention(session.session_id);
      dropShell(session.session_id);
    }
  }

  async function forkSession() {
    await command("fork_session", { sessionId });
  }

  async function startRename(session) {
    renameTarget = session;
    renameLabel = session.label;
  }

  async function confirmRename(event) {
    event?.preventDefault();
    const session = renameTarget;
    const label = renameLabel.trim();
    renameTarget = null;
    if (session && label) await command("rename_session", { sessionId: session.session_id, label });
  }

  async function pinSession(session) {
    // A pinned session takes the first free slot; pinning again unpins.
    if (session.slot) {
      await command("pin_session", { sessionId: session.session_id, slot: null });
      return;
    }
    const taken = new Set(sessions.map((entry) => entry.slot).filter((slot) => slot != null));
    let slot = 1;
    while (taken.has(slot) && slot < 9) slot += 1;
    if (taken.has(slot)) {
      error = "Every pin slot is taken; unpin one first.";
      return;
    }
    await command("pin_session", { sessionId: session.session_id, slot });
  }

  async function createPath(kind, path) {
    const ok = await command(kind === "file" ? "create_file" : "create_dir", { sessionId, path });
    if (ok) {
      refreshFiles();
      if (kind === "file") openFile(path);
    }
  }

  function startRenamePath(path) {
    pathTarget = { path, value: path };
  }

  async function confirmRenamePath(event) {
    event.preventDefault();
    const target = pathTarget;
    pathTarget = null;
    if (!target || !target.value.trim() || target.value === target.path) return;
    const ok = await command("rename_path", { sessionId, from: target.path, to: target.value.trim() });
    if (ok) {
      closePathTabs(target.path);
      refreshFiles();
    }
  }

  function requestDelete(entry) {
    deleteTarget = entry;
  }

  async function confirmDelete() {
    const entry = deleteTarget;
    deleteTarget = null;
    if (!entry) return;
    const ok = await command("delete_path", { sessionId, path: entry.path });
    if (ok) {
      closePathTabs(entry.path);
      refreshFiles();
    }
  }

  function closePathTabs(path) {
    const owner = sessionId;
    const doomed = currentView.tabs.filter((tab) => tab.path === path || tab.path?.startsWith(`${path}/`));
    for (const tab of doomed) {
      if (tab.dirty) continue;
      removeTab(tab.id, owner);
    }
  }

  async function stagePath(path) {
    if (await command("git_stage", { sessionId, path })) refreshFiles();
  }

  async function unstagePath(path) {
    if (await command("git_unstage", { sessionId, path })) refreshFiles();
  }

  // One click from a fully unstaged tree: stage each path in order and stop
  // at the first failure, so the error names the file that refused.
  async function stageAll() {
    if (gitBusy) { error = "Wait for the current Git operation to finish."; return; }
    const owner = sessionId;
    const paths = status.files.filter((file) => file.unstaged).map((file) => file.path);
    gitBusy = true;
    try {
      for (const path of paths) {
        try {
          await invoke("git_stage", { sessionId: owner, path });
        } catch (cause) {
          if (owner === sessionId) error = `Could not stage ${path}: ${cause}`;
          break;
        }
      }
    } finally {
      gitBusy = false;
    }
    refreshFiles(owner);
  }

  // A comparison whose side no longer exists keeps its tab but says so,
  // instead of showing a diff that a commit or unstage just emptied.
  function markStaleDiffs(owner, nextStatus) {
    const record = views[owner];
    if (!record) return;
    const byPath = new Map(nextStatus.files.map((file) => [file.path, file]));
    for (const tab of record.tabs) {
      if (tab.kind !== "diff" || tab.side === "last_turn" || tab.loading) continue;
      const change = byPath.get(tab.path);
      const gone = tab.side === "staged" ? !change?.staged : !change;
      if (gone && tab.error !== "No comparison is available") {
        patchDoc(owner, tab.id, { error: "No comparison is available" });
      }
    }
  }

  function openCommit() {
    commitOpen = true;
    commitMessage = "";
    commitError = "";
  }

  async function submitCommit(event) {
    event.preventDefault();
    commitError = "";
    try {
      if (gitBusy) return;
      gitBusy = true;
      await invoke("git_commit", { sessionId, message: commitMessage });
      commitOpen = false;
      refreshFiles();
    } catch (cause) {
      commitError = String(cause);
    } finally { gitBusy = false; }
  }

  function requestGitOperation(operation) {
    if (gitBusy) return;
    if (["pull", "abort_merge"].includes(operation.kind) && dirtyTabs()) {
      error = "Save or close unsaved editor files before changing the working tree.";
      return;
    }
    gitConfirm = { operation, sessionId, workspace: info.workspace, upstream: status.upstream };
  }

  async function performGitOperation(operation, owner = sessionId) {
    if (gitBusy) return;
    gitBusy = true;
    branchesError = "";
    error = "";
    try {
      await invoke("git_operation", { sessionId: owner, operation });
      if (owner === sessionId && ["pull", "abort_merge"].includes(operation.kind)) {
        for (const tab of viewFor(views, owner).tabs.filter((entry) => !entry.dirty)) await reloadTab(tab.id, owner);
      }
    } catch (cause) { branchesError = String(cause); error = String(cause); }
    finally {
      gitBusy = false;
      if (owner === sessionId) { await reloadBranches(); }
    }
  }

  function confirmGitOperation() {
    const target = gitConfirm;
    gitConfirm = null;
    if (target) performGitOperation(target.operation, target.sessionId);
  }

  async function openBranches() {
    branchesOpen = true;
    branchesError = "";
    branchesLoading = true;
    const owner = sessionId;
    try {
      const names = await invoke("git_branches", { sessionId: owner });
      if (owner === sessionId) branches = names;
    } catch (cause) {
      if (owner === sessionId) branchesError = String(cause);
    } finally {
      if (owner === sessionId) branchesLoading = false;
    }
  }

  async function reloadBranches() {
    const owner = sessionId;
    try {
      const names = await invoke("git_branches", { sessionId: owner });
      if (owner === sessionId) branches = names;
    } catch (cause) {
      if (owner === sessionId) branchesError = String(cause);
    }
    refreshFiles();
  }

  async function switchBranch(name) {
    if (dirtyTabs()) { branchesError = "Save or close unsaved files before switching branches."; return; }
    branchesError = "";
    const ok = await command("git_switch_branch", { sessionId, name });
    if (ok) {
      // Another branch's files replace this one's; tabs would point at
      // content that no longer exists.
      for (const tab of [...currentView.tabs]) {
        removeTab(tab.id);
      }
      await reloadBranches();
    } else branchesError = error;
  }

  async function mergeBranch(name) {
    branchesError = "";
    const ok = await command("git_merge_branch", { sessionId, name });
    if (ok) await reloadBranches();
    else { branchesError = error; refreshFiles(); }
  }

  async function createBranch(name) {
    if (dirtyTabs()) { branchesError = "Save or close unsaved files before creating a branch."; return; }
    branchesError = "";
    if (await command("git_create_branch", { sessionId, name })) {
      for (const tab of [...currentView.tabs]) {
        removeTab(tab.id);
      }
      await reloadBranches();
    } else branchesError = error;
  }

  async function renameBranch(from, to) {
    branchesError = "";
    if (await command("git_rename_branch", { sessionId, from, to })) await reloadBranches();
    else branchesError = error;
  }

  // Reads a document's content into the record of the session that asked,
  // even when the result arrives after a switch.
  async function readDoc(id, owner) {
    const tab = docIn(owner, id);
    if (!tab) return;
    patchDoc(owner, id, { loading: true, error: "" });
    try {
      if (tab.kind === "file" && isImage(tab.path)) {
        const image = await invoke("read_image", { sessionId: owner, path: tab.path });
        patchDoc(owner, id, { image, loading: false });
      } else if (tab.kind === "file") {
        const file = await invoke("read_file", { sessionId: owner, path: tab.path });
        patchDoc(owner, id, { file, draft: file.text, dirty: false, loading: false });
      } else {
        const diff = await invoke("git_diff", { sessionId: owner, path: tab.path, side: tab.side });
        patchDoc(owner, id, { diff, loading: false });
      }
    } catch (cause) {
      patchDoc(owner, id, { error: String(cause), loading: false });
    }
  }

  function reloadTab(id, owner = sessionId) {
    return readDoc(id, owner);
  }

  function dirtyTabs() {
    return dirtyIn(currentView);
  }

  function dirtySession(id) {
    if (!id) return dirtyTabs();
    return views[id] ? dirtyIn(views[id]) : false;
  }

  function changeDraft(value) {
    const tab = activeDoc;
    if (!tab || tab.kind !== "file" || tab.file?.read_only) return;
    patchDoc(sessionId, tab.id, { draft: value, dirty: value !== (tab.file?.text ?? "") });
  }

  async function saveActive() {
    const tab = activeDoc;
    if (!tab || tab.kind !== "file" || tab.file?.read_only || !tab.dirty || tab.saving) return;
    const owner = sessionId;
    const text = tab.draft;
    patchDoc(owner, tab.id, { saving: true });
    try {
      const file = await invoke("write_file", {
        sessionId: owner,
        path: tab.path,
        text,
        fingerprint: tab.file.fingerprint,
        crlf: tab.file.crlf,
      });
      const current = docIn(owner, tab.id);
      // Keep typing that happened while the write was in flight; only the
      // saved baseline moves.
      if (current && current.draft !== text) {
        patchDoc(owner, tab.id, { file, dirty: current.draft !== file.text, error: "", saving: false });
      } else {
        patchDoc(owner, tab.id, { file, draft: file.text, dirty: false, error: "", saving: false });
      }
      refreshFiles(owner);
    } catch (cause) {
      patchDoc(owner, tab.id, { error: String(cause), saving: false });
    }
  }

  function hunkLabels(text) {
    return text.split("\n")
      .filter((line) => line.startsWith("@@"))
      .map((line) => line.slice(2).replace(/\s+@@.*$/, "").trim());
  }

  function requestFileAction(kind, index = null) {
    const tab = activeDoc;
    if (!tab) return;
    if (kind === "reload" && !tab.dirty) { reloadTab(tab.id); return; }
    fileAction = { kind, id: tab.id, path: tab.path, index, owner: sessionId };
  }

  async function confirmFileAction() {
    const target = fileAction;
    fileAction = null;
    if (!target || !docIn(target.owner, target.id)) return;
    if (target.kind === "reload") await reloadTab(target.id, target.owner);
    else if (target.kind === "hunk") await discardHunk(target);
    else await restoreTab(target.id, target.owner);
  }

  async function discardHunk(target) {
    if (!docIn(target.owner, target.id) || gitBusy) return;
    const owner = target.owner;
    gitBusy = true;
    error = "";
    try {
      await invoke("git_discard_hunk", { sessionId: owner, path: target.path, hunkIndex: target.index });
      patchDoc(owner, target.id, { diff: null });
      await readDoc(target.id, owner);
      refreshFiles(owner);
    } catch (cause) {
      if (owner === sessionId) error = String(cause);
      patchDoc(owner, target.id, { error: String(cause) });
    } finally {
      gitBusy = false;
    }
  }

  async function restoreTab(id, owner = sessionId) {
    const tab = docIn(owner, id);
    if (!tab || tab.kind !== "diff") return;
    try {
      await invoke("git_restore", { sessionId: owner, path: tab.path });
      patchDoc(owner, id, { diff: null });
      await readDoc(id, owner);
      refreshFiles(owner);
    } catch (cause) {
      patchDoc(owner, id, { error: String(cause) });
    }
  }

  async function openModelPicker() {
    pickerOpen = true;
    modelsLoading = true;
    modelsError = "";
    try {
      models = await invoke("list_models");
    } catch (cause) {
      modelsError = String(cause);
    } finally {
      modelsLoading = false;
    }
  }

  async function applyModel(modelId, profileId, effort) {
    try {
      await invoke("set_model", { sessionId, modelId, profileId, effort });
      pickerOpen = false;
      return true;
    } catch (cause) {
      modelsError = String(cause);
      return false;
    }
  }

  function removeTab(id, owner = sessionId) {
    editRecord(owner, (record) => closeDocument(record, id));
  }

  function closeTab(id) {
    const tab = docIn(sessionId, id);
    if (!tab) return;
    if (tab.dirty) {
      pendingClose = id;
      return;
    }
    removeTab(id);
  }

  function discardAndClose() {
    const id = pendingClose;
    pendingClose = "";
    removeTab(id);
  }

  // Documents are keyed per record, so the id needs no session prefix.
  async function openTab(kind, path, side = "combined") {
    const owner = sessionId;
    if (!owner) return;
    const id = kind === "diff" ? `diff:${path}@${side}` : `file:${path}`;
    const existing = docIn(owner, id);
    editRecord(owner, (record) => existing
      ? { ...record, activeDocId: id }
      : openDocument(record, { id, kind, path, side: kind === "diff" ? side : null, title: basename(path), loading: true, error: "" }));
    openFiles();
    if (!existing?.file && !existing?.diff && !existing?.image) await readDoc(id, owner);
  }

  // Sniffing happens in Rust; this only decides which command to ask.
  const IMAGE_FILE = /\.(png|jpe?g|gif|webp)$/i;
  function isImage(path) {
    return IMAGE_FILE.test(path);
  }

  function openFile(path) {
    openTab("file", path);
  }

  async function openFileAt(path, line) {
    await openTab("file", path);
    await tick();
    filesPane?.jumpToLine(line);
  }

  function openDiff(path, side = "combined") {
    openTab("diff", path, side);
  }

  // One terminal host per session: the first open mounts the shell, and every
  // later show, hide or focus reuses that host, so the PTY is never restarted.
  async function ensureShell() {
    if (!shells[sessionId]) shells = { ...shells, [sessionId]: true };
    if (!TerminalComponent) {
      TerminalComponent = (await import("./lib/Terminal.svelte")).default;
    }
  }

  async function openTerminal() {
    if (!sessionId) return;
    const view = viewFor(views, sessionId);
    view.drawerOpen = true;
    if (view.screen === "files") view.screen = "session";
    await ensureShell();
  }

  async function focusTerminal() {
    if (!sessionId) return;
    await ensureShell();
    viewFor(views, sessionId).screen = "terminal";
    screenAnnouncement = "Terminal";
    await tick();
    document.querySelector(".terminal-host.focused textarea")?.focus();
  }

  function toggleTerminal() {
    if (!sessionId) return;
    const view = viewFor(views, sessionId);
    if (view.screen === "terminal") leaveFocus();
    else if (view.screen === "session" && view.drawerOpen) view.drawerOpen = false;
    else openTerminal();
  }

  function openFiles() {
    if (!sessionId) return;
    const view = viewFor(views, sessionId);
    if (view.screen === "files") return;
    view.screen = "files";
    screenAnnouncement = "Files";
  }

  function leaveFocus() {
    if (!sessionId) return;
    viewFor(views, sessionId).screen = "session";
    screenAnnouncement = "Session";
  }

  // The header's ‹ Session control: the same return as Escape, including
  // where focus lands.
  async function backToSession() {
    const from = currentView.screen;
    leaveFocus();
    await tick();
    (from === "terminal" ? terminalButton : filesButton)?.focus();
  }

  async function returnToDecision() {
    leaveFocus();
    await tick();
    focusDecision();
  }

  // Needs you is a rail section, not a panel: bring it into view, opening the
  // rail overlay when the window is compact.
  async function showNeedsYou() {
    if (currentView.screen !== "session") leaveFocus();
    if (window.matchMedia("(max-width: 760px)").matches) railOpen = true;
    await tick();
    const section = document.querySelector(".needs-section");
    section?.scrollIntoView({ block: "nearest" });
    section?.querySelector("button")?.focus();
  }

  function hostClass(id) {
    return terminalHostClass(views, id, sessionId);
  }

  function onKeydown(event) {
    if (!booted) return;
    const view = currentView;
    const key = event.key.toLowerCase();
    // Escape leaves a focus screen unless an editor, field or the terminal
    // has focus; there it belongs to that control. Modals, the palette and
    // the find bar handle their own Escape before it arrives here.
    if (event.key === "Escape") {
      const target = escapeReturn(view.screen, document.activeElement);
      if (!target) return;
      event.preventDefault();
      leaveFocus();
      tick().then(() => (target === "terminal" ? terminalButton : filesButton)?.focus());
      return;
    }
    // The terminal owns Ctrl chords (Ctrl+W deletes a word, Ctrl+E moves to
    // the line end, Ctrl+S is XOFF). Only Ctrl+` still toggles the terminal.
    if (event.ctrlKey && !event.metaKey && document.activeElement?.closest?.(".terminal-host")) {
      if (event.key === "`") {
        event.preventDefault();
        toggleTerminal();
      }
      return;
    }
    if ((event.metaKey || event.ctrlKey) && key === "e") {
      event.preventDefault();
      if (view.screen === "session") openFiles();
      else leaveFocus();
      return;
    }
    if ((event.metaKey || event.ctrlKey) && key === "w") {
      if (view.screen === "files" && view.activeDocId) {
        event.preventDefault();
        closeTab(view.activeDocId);
      }
      return;
    }
    if ((event.metaKey || event.ctrlKey) && key === "s") {
      if (view.screen === "files" && activeDoc?.kind === "file") {
        event.preventDefault();
        saveActive();
      }
      return;
    }
    if ((event.ctrlKey && event.key === "`") || (event.metaKey && key === "j")) {
      event.preventDefault();
      toggleTerminal();
      return;
    }
    if (event.metaKey && key === "m") {
      event.preventDefault();
      openModelPicker();
    }
    if ((event.metaKey || event.ctrlKey) && key === "k") {
      event.preventDefault();
      paletteOpen = true;
    }
  }

  // The workspace may not be trusted yet: then the page shows the trust
  // screen and subscribes only after the operator says yes, because a
  // session does not exist before that.
  let checked = $state(false);
  let booted = $state(false);
  let needsTrust = $state(false);
  let needsFolder = $state(false);
  let trustNoGit = $state(false);
  // The folder isn't a project yet; accepting adds it (D5).
  let trustConfirm = $state(false);
  // Registered projects, offered on the folder screen of a launch with no
  // folder (the Dock, Finder).
  let folderProjects = $state([]);
  let trustWorkspace = $state("");
  let trustError = $state("");
  let trusting = $state(false);
  let channel = null;

  async function loadThemes() {
    try {
      themes = await invoke("list_themes", { sessionId: sessionId || null });
    } catch {}
    finally {
      themeReady = true;
    }
  }

  async function boot() {
    channel = new Channel();
    channel.onmessage = onEvent;
    const hello = await invoke("subscribe", { channel });
    sessionId = hello.session_id;
    sessions = hello.sessions;
    supportsGit = hello.supports_git !== false;
    projectId = hello.project_id ?? "";
    projects = hello.projects ?? [];
    for (const row of hello.sessions) sessionOwner[row.session_id] = projectId;
    rememberProject(projectId);
    loadResumable();
    sessionId = hello.initial.session_id;
    applySession(hello.initial);
    channel.onmessage = onEvent;
    // Theme discovery needs the session state, which exists only after trust.
    if (!themes.length) await loadThemes();
    booted = true;
    refreshFiles();
    if (import.meta.env.MODE === "self-test" && hello.self_test) {
      const { runSelfTest } = await import("./selftest.js");
      runSelfTest(hello.self_test, hello.session_id);
    }
  }

  // A launch with no folder (Dock, Finder): pick one, then either trust it or,
  // if it is already trusted, open it straight away.
  // From the folder screen: run `name`, then follow where it leads (still
  // the folder screen, the trust screen, or an open project). True only when
  // a project opened and the page booted.
  async function fromFolderScreen(name, args) {
    trusting = true;
    trustError = "";
    try {
      const status = await invoke(name, args);
      trustWorkspace = status.workspace;
      trustError = status.error;
      trustNoGit = status.no_git;
      trustConfirm = status.needs_confirmation;
      needsFolder = status.needs_folder;
      if (needsFolder || status.needs_trust) return false;
      await boot();
      needsTrust = false;
      return true;
    } catch (cause) {
      trustError = String(cause);
      return false;
    } finally {
      trusting = false;
    }
  }

  const chooseFolder = () => fromFolderScreen("choose_folder");
  const openRegistered = (project) => fromFolderScreen("open_project", { projectId: project.id });

  function lastProjectId() {
    try { return localStorage.getItem("forge.desktop.lastProject"); } catch { return null; }
  }

  async function acceptTrust() {
    trusting = true;
    trustError = "";
    try {
      await invoke("trust_workspace");
      await boot();
      needsTrust = false;
    } catch (cause) {
      trustError = String(cause);
    } finally {
      trusting = false;
    }
  }

  onMount(() => {
    try {
      const stored = localStorage.getItem("forge.desktop.theme");
      themeChoice = stored === "light" ? "forge-light" : stored === "dark" ? "forge-dark" : stored || "forge-dark";
    } catch {}
    window.matchMedia(DARK_QUERY).addEventListener("change", (event) => (systemDark = event.matches));
    loadThemes();
    window.addEventListener("keydown", onKeydown);
    unlockOnFirstGesture();
    window.addEventListener("focus", syncSession);
    let unlisten = null;
    listen("close_requested", () => {
      quitRequested = true;
    }).then((stop) => {
      unlisten = stop;
    });
    (async () => {
      try {
        const status = await invoke("trust_status");
        trustWorkspace = status.workspace;
        needsFolder = status.needs_folder;
        trustNoGit = status.no_git;
        trustConfirm = status.needs_confirmation;
        needsTrust = status.needs_trust || status.needs_folder;
        trustError = status.error;
        if (status.needs_folder) {
          folderProjects = await invoke("list_projects").catch(() => []);
          // Reopen the last project (D9); if it can't open, the folder
          // screen stays with the reason.
          const last = folderProjects.find((project) => project.id === lastProjectId());
          if (last && (await fromFolderScreen("open_project", { projectId: last.id }))) return;
        }
      } catch (cause) {
        error = String(cause);
      } finally {
        checked = true;
      }
      if (needsTrust) return;
      try {
        await boot();
      } catch (cause) {
        error = String(cause);
      }
    })();
    return () => {
      window.removeEventListener("keydown", onKeydown);
      window.removeEventListener("focus", syncSession);
      unlisten?.();
    };
  });
</script>

<main class="app" class:focus={currentView.screen !== "session"} data-turn-state={turnState} inert={!booted}>
  <header class={currentView.screen === "session" ? "topbar session-head" : "topbar focus-head"} class:mac={isMac()} data-tauri-drag-region="deep">
    <div class="brand">
      <button bind:this={railButton} type="button" class="icon-btn rail-toggle" aria-label="Sessions" aria-expanded={railOpen} onclick={() => (railOpen = true)}>
        <Icon name="layers" size={14} />
      </button>
      {#if currentProject}
        <span class="wordmark" title={currentProject.root}>{currentProject.name}</span>
      {:else}
        <span class="wordmark">Forge</span>
      {/if}
    </div>
    {#if currentView.screen === "session"}
      <div class="head-title">
        {#if activeSession}<span class="state-dot {turnState}" aria-hidden="true"></span>{/if}
        <strong>{activeSession?.label ?? "No session"}</strong>
        {#if activeSession}<span class="head-state">{turnWord(turnState)}</span>{/if}
        {#if branch}<span class="chip mono head-branch" title={`${branch} · ${info.workspace}`}><Icon name="branch" size={11} /><span>{branch}</span></span>
        {:else if info.workspace}<span class="chip mono head-branch checkout" title={info.workspace}><span>{basename(info.workspace)}</span></span>{/if}
      </div>
      <span class="spacer"></span>
      {#if needsYou > 0}
        <button type="button" class="btn head-btn needs" onclick={showNeedsYou}>Needs you ({needsYou})</button>
      {/if}
      <!-- Icons for the three everyday actions; each has its name as a tooltip
           and an accessible label. -->
      <button bind:this={filesButton} type="button" class="icon-btn head-icon" title={`Files (${chord("E")})`} aria-label="Files" disabled={!sessionId} onclick={openFiles}>
        <Icon name="files" size={15} />
        {#if fileTotals.changed > 0}<span class="count" aria-label={`${fileTotals.changed} changed`}>{fileTotals.changed}</span>{/if}
      </button>
      <button
        bind:this={terminalButton}
        type="button"
        class="icon-btn head-icon"
        class:on={terminalShown}
        title={`${terminalShown ? "Hide" : "Show"} terminal (${isMac() ? "⌘J" : "Ctrl+`"})`}
        aria-label={terminalShown ? "Hide terminal" : "Show terminal"}
        aria-pressed={terminalShown}
        disabled={!sessionId}
        onclick={toggleTerminal}
      >
        <Icon name="terminal" size={15} />
      </button>
      <button type="button" class="icon-btn head-icon" title="New session" aria-label="New session" aria-haspopup="dialog" onclick={openNewSession}>
        <Icon name="plus" size={15} />
      </button>
      <span class="head-rule" aria-hidden="true"></span>
      <button type="button" class="btn head-btn" aria-haspopup="dialog" disabled={!sessionId} onclick={() => (sessionMenu = "actions")}>Session…</button>
    {:else}
      <button type="button" class="btn head-btn" onclick={backToSession}>
        <Icon name="arrow-left" size={13} />
        Session
      </button>
      <strong>{currentView.screen === "files" ? "Files" : "Terminal"}</strong>
      <span class="head-session">{activeSession?.label ?? ""}</span>
      {#if currentView.screen === "files" && currentView.tabs.length === 1}
        {@const only = currentView.tabs[0]}
        <span class="chip mono head-doc" title={only.path}>{only.title}</span>
        <button type="button" class="icon-btn" aria-label={`Close ${only.title}`} title={`Close document (${chord("W")})`} onclick={() => closeTab(only.id)}>
          <Icon name="close" size={12} />
        </button>
      {/if}
      <span class="spacer"></span>
      {#if needsYou > 0}
        <button type="button" class="btn head-btn needs" onclick={showNeedsYou}>Needs you ({needsYou})</button>
      {/if}
      {#if hasDecision}
        <button type="button" class="btn head-btn needs" onclick={returnToDecision}>
          <Icon name="alert" size={13} />
          {question ? "Answer needed" : "Approval needed"}
        </button>
      {/if}
    {/if}
  </header>

  <Sidebar
    bind:this={sidebar}
    sessions={allSessions}
    activeId={sessionId}
    {projects}
    {projectId}
    {projectBusy}
    onSwitchProject={(project) => { railOpen = false; switchProject(project); }}
    onAddProject={addProject}
    onCloseProject={(project) => closeProject(project)}
    onRemoveProject={removeProject}
    onElsewhere={openElsewhere}
    {creating}
    open={railOpen}
    attention={Object.keys(attention)}
    onCloseRail={async () => { railOpen = false; await tick(); railButton?.focus(); }}
    onRequest={openRequest}
    onSelect={selectSession}
    onCreate={createSession}
    onAttach={openAttach}
    onArchive={archiveSession}
    onRename={startRename}
    onPin={pinSession}
    onStop={stopSession}
    onClose={closeSession}
    onResume={(session) => { railOpen = false; command("resume_session", { sessionId: session.session_id, currentSessionId: sessionId }); }}
    onHelp={() => (helpOpen = true)}
    onSettings={() => (settingsOpen = true)}
    onNew={openNewSession}
  />

  <section class="center" class:deciding={hasDecision}>
    {#if error}
      <div class="banner" role="alert">
        <Icon name="alert" size={14} />
        <span>{error}</span>
        <button type="button" class="icon-btn" aria-label="Dismiss" onclick={() => (error = "")}>
          <Icon name="close" size={12} />
        </button>
      </div>
    {/if}

    {#if currentView.screen === "session"}
      {#key sessionId}
        <div class="chat-host">
          <Conversation
            {messages}
            {streaming}
            {turnState}
            bind:scroll={currentView.scroll}
            bind:pinned={currentView.pinned}
            onSuggest={(text) => (currentView.draft = text)}
            onExpand={expandMessage}
            onOpenImage={openFile}
          />
        </div>
      {/key}

      {#if currentStep}
        <p class="current-step">
          <span>Current step: {currentStep.step}</span>
          <button type="button" class="link" onclick={() => (sessionMenu = "plan")}>Open plan</button>
        </p>
      {/if}

      {#if hasQueue}
        <div class="queue-strip">
          <span class="queue-text">{queue.length} queued · {queue[0].text}</span>
          <button type="button" class="btn" onclick={() => (sessionMenu = "queue")}>View</button>
        </div>
      {/if}

      {#if waitNote}
        <p class="wait-note" role="status">
          <span class="state-dot waiting" aria-hidden="true"></span>
          <span>{waitNote.text}</span>
          {#if waitNote.action}
            <button type="button" class="btn" onclick={() => (sessionMenu = waitNote.action)}>{waitNote.label}</button>
          {/if}
        </p>
      {/if}

      <div class="decision">
        {#if question}
          <QuestionCard
            {question}
            onAnswer={answerQuestion}
            savedDrafts={currentView.questionDrafts[question.call_id] ?? null}
            onDrafts={(drafts) => keepDraft(viewFor(views, sessionId).questionDrafts, question.call_id, drafts)}
          />
        {/if}

        {#if approval}
          <ApprovalCard
            {approval}
            onDecide={decide}
            savedDraft={currentView.approvalDrafts[approval.call_id] ?? null}
            onDraft={(draft) => keepDraft(viewFor(views, sessionId).approvalDrafts, approval.call_id, draft)}
          />
        {/if}
      </div>

      <Composer
        bind:prompt={currentView.draft}
        {turnState}
        {stopping}
        {sessionId}
        commands={composerCommands}
        history={promptHistory}
        {imageInput}
        attachment={currentView.attachment}
        onRemoveAttachment={() => { viewFor(views, sessionId).attachment = null; }}
        onSend={send}
        onStop={stop}
        drawerOpen={currentView.drawerOpen && !!shells[sessionId]}
        onToggleTerminal={toggleTerminal}
        onFocusTerminal={focusTerminal}
        onStopTerminal={() => dropShell(sessionId)}
        model={info.model}
        onModel={openModelPicker}
        contextUsage={info.contextUsage}
        onContext={() => (detailsOpen = true)}
      />
    {:else if currentView.screen === "files"}
      {#key sessionId}
        <Files
          bind:this={filesPane}
          tree={fileTree}
          bind:showIgnored
          {filesLoading}
          {filesError}
          {status}
          {lastTurnFiles}
          bind:scope={currentView.fileScope}
          {supportsGit}
          bind:query={currentView.fileQuery}
          tabs={currentView.tabs}
          activeDocId={currentView.activeDocId}
          {branch}
          upstream={status.upstream}
          ahead={status.ahead ?? 0}
          behind={status.behind ?? 0}
          {gitBusy}
          mergeInProgress={status.merge_in_progress ?? false}
          conflicts={status.conflicts ?? []}
          onOpenFile={(path, line) => line ? openFileAt(path, line) : openFile(path)}
          onOpenDiff={openDiff}
          onRefreshFiles={refreshFiles}
          onCreateFile={(path) => createPath("file", path)}
          onCreateDir={(path) => createPath("dir", path)}
          onRenamePath={startRenamePath}
          onDeletePath={requestDelete}
          onSearch={(value) => invoke("search_files", { sessionId, query: value })}
          onFindFiles={(value) => invoke("find_files", { sessionId, query: value })}
          onToggleDir={toggleDir}
          onDraftChange={changeDraft}
          onActivate={(id) => (currentView.activeDocId = id)}
          onClose={closeTab}
          onStage={stagePath}
          onUnstage={unstagePath}
          onStageAll={stageAll}
          onCommit={openCommit}
          onBranches={supportsGit ? openBranches : null}
          onGitOperation={(operation) => operation.kind === "fetch" ? performGitOperation(operation) : requestGitOperation(operation)}
        />
      {/key}
    {/if}

    {#if TerminalComponent}
      {#each Object.keys(shells) as id (id)}
        <div class="terminal-host {hostClass(id)}">
          <TerminalComponent
            sessionId={id}
            themeMode={activeTheme?.id ?? "dark"}
            onExit={() => {
              dropShell(id);
              if (id === sessionId) screenAnnouncement = "The shell exited.";
            }}
          />
        </div>
      {/each}
    {/if}
  </section>

  {#if currentView.screen === "session"}
    {#key sessionId}
      <Changes
        {sessionId}
        {status}
        {lastTurnFiles}
        {supportsGit}
        loading={filesLoading}
        error={filesError}
        onOpenDiff={(path, side) => { openFiles(); openDiff(path, side); }}
        onOpenFiles={openFiles}
      />
    {/key}
  {/if}

  {#if sessionMenu !== null}
    <div class="modal popover" role="presentation" onclick={(event) => event.target === event.currentTarget && (sessionMenu = null)}>
      <Inspector
        destination={sessionMenu}
        onClose={() => (sessionMenu = null)}
        sessionId={sessionId}
        workspace={info.workspace}
        {branch}
        model={info.model}
        contextUsage={info.contextUsage}
        queued={info.queued}
        {turnState}
        {approveAll}
        {thinking}
        {plan}
        {tokens}
        {background}
        {queue}
        {interrupted}
        {counts}
        onPickModel={() => { sessionMenu = null; openModelPicker(); }}
        onDetails={() => { sessionMenu = null; detailsOpen = true; }}
        onToggleApproveAll={toggleApproveAll}
        onToggleThinking={toggleThinking}
        onClearApprovals={() => command("clear_approvals", { sessionId })}
        onContinue={continueTurn}
        onCompact={compactContext}
        onStopSession={() => activeSession && stopSession(activeSession)}
        onCancelQueued={(sessionIdOf, queueId, position, source) =>
          command("cancel_queued", { sessionId: sessionIdOf, queueId, position, source })}
        onEditQueued={(item, position) => { sessionMenu = null; editQueued(item, position); }}
        onRestoreInterrupted={(text) => { sessionMenu = null; restoreInterrupted(text); }}
        onCancelBackground={(task) => command("cancel_background", { sessionId, taskId: task.id, runId: task.run_id })}
        onOpenChild={(task) => { sessionMenu = null; openChild(task); }}
        onInsertTask={(task) => { sessionMenu = null; insertTask(task); }}
        onResolveChild={(taskId, approve, callId, requestId, confirmed) =>
          command("resolve_child_approval", { sessionId, taskId, approve, callId, requestId, confirmed })}
        onFork={() => { sessionMenu = null; forkSession(); }}
        onCloseSession={() => { sessionMenu = null; if (activeSession) closeSession(activeSession); }}
        onArchive={() => { sessionMenu = null; if (activeSession) archiveSession(activeSession); }}
        onRename={() => { sessionMenu = null; if (activeSession) startRename(activeSession); }}
        onPin={() => activeSession && pinSession(activeSession)}
      />
    </div>
  {/if}

  <div class="sr-only" role="status" aria-live="polite" aria-atomic="true">{screenAnnouncement}</div>

  <div class="sr-only" role="status" aria-live="polite" aria-atomic="true">
    {question ? "Forge needs your answer." : approval ? "Forge needs your approval." : background.some((task) => task.awaiting_approval) ? "A background task needs your approval. Open the Session menu to review it." : `Forge: ${turnState}.`}
  </div>

  {#if pickerOpen}
    <ModelPicker
      {models}
      currentModel={info.model}
      loading={modelsLoading}
      error={modelsError}
      onClose={() => (pickerOpen = false)}
      onProviders={() => (providersOpen = true)}
      onApply={applyModel}
    />
  {/if}

  {#if paletteOpen}
    <Palette items={paletteItems} onQuery={findFileItems} onClose={() => (paletteOpen = false)} />
  {/if}

  {#if childView}
    <div class="modal">
      <div class="dialog child-dialog" role="dialog" aria-modal="true" aria-labelledby="child-title" tabindex="-1" use:modal={{ onClose: () => (childView = null) }}>
        <header class="child-head">
          <h2 id="child-title">Agent · {childView.label}</h2>
          <button class="icon-btn" type="button" aria-label="Close" onclick={() => (childView = null)}>
            <Icon name="close" size={13} />
          </button>
        </header>
        <div class="child-messages">
          {#each childView.messages as message, index (index)}
            <article class="child-message {message.role}">
              <span class="child-role">{message.name || message.role}</span>
              <pre class="mono">{message.text}</pre>
            </article>
          {/each}
          {#if childView.messages.length === 0}<p class="dim small">No saved messages yet.</p>{/if}
        </div>
      </div>
    </div>
  {/if}

  {#if approveAllPending}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="approve-all-title" tabindex="-1" use:modal={{ onClose: () => (approveAllPending = false) }}>
        <h2 id="approve-all-title">Auto-approve review prompts?</h2>
        <p>
          Every review prompt in this session is approved without being shown.
          The filesystem sandbox and network rules still apply, and privileged
          host or sandbox requests are not auto-approved.
        </p>
        <div class="dialog-actions">
          <button class="btn danger" type="button" onclick={confirmApproveAll}>Auto-approve</button>
          <button class="btn" type="button" onclick={() => (approveAllPending = false)}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}

  {#if quitRequested}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="quit-title" tabindex="-1" use:modal={{ onClose: () => (quitRequested = false), initialFocus: "#quit-cancel" }}>
        <h2 id="quit-title">Quit Forge?</h2>
        <p>
          {#if dirtyAnywhere(views)}The editor has unsaved changes. {/if}
          {#if sessions.some((session) => ["running", "waiting"].includes(session.turn_state)) || Object.keys(attention).length > 0}
            Work is still running, queued or waiting for an answer in another session.
          {/if}
          {#if projects.some((project) => project.id !== projectId && project.busy > 0)}
            Agents are also working in {projects.filter((project) => project.id !== projectId && project.busy > 0).map((project) => project.name).join(", ")}.
          {/if}
          Quitting stops every session; the agent's file changes on disk are kept.
        </p>
        <div class="dialog-actions">
          <button class="btn danger" type="button" onclick={() => invoke("quit_app")}>
            Quit Forge
          </button>
          <button id="quit-cancel" class="btn" type="button" onclick={() => (quitRequested = false)}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}

  {#if archiveTarget}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="archive-title" tabindex="-1" use:modal={{ onClose: () => (archiveTarget = null) }}>
        <h2 id="archive-title">
          {["active", "closed"].includes(archiveTarget.lifecycle)
            ? `Archive “${archiveTarget.label}”?`
            : `Clean up “${archiveTarget.label}”?`}
        </h2>
        <p>
          {["active", "closed"].includes(archiveTarget.lifecycle)
            ? "It disappears from the switcher, but its journal is kept and it can be resumed later. A running turn is stopped first."
            : "This session is already closed. Only its git worktree can be cleaned up from here."}
        </p>
        {#if archiveTarget.ownership === "managed"}
          <label>
            <input type="checkbox" bind:checked={archiveRemove} />
            Remove its git worktree after archiving
          </label>
          {#if archiveRemove}
            <label class="danger-line">
              <input type="checkbox" bind:checked={archiveDiscard} />
              Remove even if it holds uncommitted work (that work is lost)
            </label>
          {/if}
        {:else}
          <p>{archiveTarget.ownership === "attached" ? "This worktree belongs to you; only the session is archived." : "The primary worktree is never removed."}</p>
        {/if}
        {#if error}<p class="dialog-error" role="alert">{error}</p>{/if}
        <div class="dialog-actions">
          <button class="btn danger" type="button" onclick={confirmArchive}>
            {archiveRemove ? "Archive and remove worktree" : "Archive"}
          </button>
          <button class="btn" type="button" onclick={() => (archiveTarget = null)}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}

  {#if attachOpen}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="attach-title" tabindex="-1" use:modal={{ onClose: () => (attachOpen = false), busy: attachBusy }}>
        <form onsubmit={submitAttach}>
          <h2 id="attach-title">Attach an existing worktree</h2>
          <p>
            Enter an absolute path that Git already lists as a worktree of this repository, plus
            the branch checked out there. A new session opens in place; the checkout is never
            modified or removed.
          </p>
          <input class="field" bind:value={attachPath} aria-label="Worktree path" placeholder="/absolute/path/to/worktree" required />
          <input class="field" bind:value={attachBranch} aria-label="Checked-out branch" placeholder="branch checked out there" required />
          <input class="field" bind:value={attachLabel} aria-label="Session name" placeholder="Session name (defaults to the folder name)" />
          {#if error}<p class="dialog-error" role="alert">{error}</p>{/if}
          <div class="dialog-actions">
            <button class="btn primary" type="submit" disabled={attachBusy || !attachPath.trim() || !attachBranch.trim()}>
              {attachBusy ? "Attaching…" : "Attach"}
            </button>
            <button class="btn" type="button" onclick={() => (attachOpen = false)}>Cancel</button>
          </div>
        </form>
      </div>
    </div>
  {/if}

  {#if commitOpen}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="commit-title" tabindex="-1" use:modal={{ onClose: () => (commitOpen = false), busy: gitBusy }}>
        <form onsubmit={submitCommit}>
          <h2 id="commit-title">Commit staged changes</h2>
          <p>Only what is staged is committed. Unstaged files stay in the working tree.</p>
          <textarea
            class="field commit-message"
            bind:value={commitMessage}
            placeholder="What changed, and why"
            aria-label="Commit message"
          ></textarea>
          {#if commitError}<p class="dialog-error" role="alert">{commitError}</p>{/if}
          <div class="dialog-actions">
            <button class="btn primary" type="submit" disabled={!commitMessage.trim() || gitBusy}>{gitBusy ? "Committing…" : "Commit"}</button>
            <button class="btn" type="button" onclick={() => (commitOpen = false)}>Cancel</button>
          </div>
        </form>
      </div>
    </div>
  {/if}

  {#if pathTarget}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="path-title" tabindex="-1" use:modal={{ onClose: () => (pathTarget = null) }}>
        <form onsubmit={confirmRenamePath}>
          <h2 id="path-title">Rename “{pathTarget.path}”</h2>
          <input class="field" bind:value={pathTarget.value} aria-label="New path" spellcheck="false" />
          <div class="dialog-actions">
            <button class="btn primary" type="submit" disabled={!pathTarget.value.trim()}>Rename</button>
            <button class="btn" type="button" onclick={() => (pathTarget = null)}>Cancel</button>
          </div>
        </form>
      </div>
    </div>
  {/if}

  {#if deleteTarget}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="delete-title" tabindex="-1" use:modal={{ onClose: () => (deleteTarget = null) }}>
        <h2 id="delete-title">Delete “{deleteTarget.path}”?</h2>
        <p>
          {deleteTarget.is_dir
            ? "Only an empty folder can be deleted from here."
            : "The file is removed from the working tree. Git can still restore it if it was committed."}
        </p>
        <div class="dialog-actions">
          <button class="btn danger" type="button" onclick={confirmDelete}>Delete</button>
          <button class="btn" type="button" onclick={() => (deleteTarget = null)}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}

  {#if branchesOpen}
    <BranchPicker
      {branches}
      current={branch}
      error={branchesError}
      loading={branchesLoading}
      busy={gitBusy}
      onClose={() => (branchesOpen = false)}
      onSwitch={switchBranch}
      onMerge={mergeBranch}
      onCreate={createBranch}
      onRename={renameBranch}
      onDelete={(name) => requestGitOperation({ kind: "delete_branch", name, force: false })}
    />
  {/if}

  {#if renameTarget}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="rename-title" tabindex="-1" use:modal={{ onClose: () => (renameTarget = null) }}>
        <form onsubmit={confirmRename}>
          <h2 id="rename-title">Rename session</h2>
          <input class="field" bind:value={renameLabel} aria-label="Session name" onkeydown={(event) => {
            if (event.key === "Enter" && !event.isComposing) confirmRename(event);
          }} />
          <div class="dialog-actions">
            <button class="btn primary" type="submit" disabled={!renameLabel.trim()}>Rename</button>
            <button class="btn" type="button" onclick={() => (renameTarget = null)}>Cancel</button>
          </div>
        </form>
      </div>
    </div>
  {/if}

  {#if pendingClose}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="discard-title" tabindex="-1" use:modal={{ onClose: () => (pendingClose = "") }}>
        <h2 id="discard-title">Discard unsaved changes?</h2>
        <p>This file has changes that are not saved.</p>
        <div class="dialog-actions">
          <button class="btn danger" type="button" onclick={discardAndClose}>Discard</button>
          <button class="btn" type="button" onclick={() => (pendingClose = "")}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}
  {#if fileAction}
    <div class="modal">
      <div class="dialog file-action-dialog" role="dialog" aria-modal="true" aria-labelledby="file-action-title" tabindex="-1" use:modal={{ onClose: () => (fileAction = null) }}>
        <h2 id="file-action-title">
          {fileAction.kind === "reload" ? "Discard edits and reload?" : fileAction.kind === "hunk" ? "Discard this hunk?" : "Restore file to HEAD?"}
        </h2>
        <p>{fileAction.path}{fileAction.kind === "hunk" ? ` · hunk ${fileAction.index + 1}` : ""}</p>
        <p>
          {fileAction.kind === "reload"
            ? "Unsaved editor changes will be discarded. The file on disk is unchanged."
            : fileAction.kind === "hunk"
              ? "Git will reverse-apply this hunk in the working tree. Other hunks and the index are unchanged; this cannot be undone in the editor."
              : "Git will discard this file's staged and unstaged changes, restoring the last committed version. This cannot be undone in the editor."}
        </p>
        <div class="dialog-actions">
          <button class="btn" type="button" onclick={() => (fileAction = null)}>Cancel</button>
          <button class="btn danger" type="button" onclick={confirmFileAction}>
            {fileAction.kind === "reload" ? "Discard edits and reload" : fileAction.kind === "hunk" ? "Discard hunk" : "Restore file"}
          </button>
        </div>
      </div>
    </div>
  {/if}

  {#if projectProblem}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="project-problem-title" tabindex="-1" use:modal={{ onClose: () => (projectProblem = null) }}>
        {#if projectProblem.kind === "needs_trust"}
          <h2 id="project-problem-title">Trust {projectProblem.project.name}?</h2>
          <p class="mono">{projectProblem.project.root}</p>
          <p>Forge has never run here. Trusting it lets agents read and write files in this folder and run commands through approvals. Nothing is written until you trust it.</p>
          <div class="dialog-actions">
            <button class="btn primary" type="button" onclick={trustProject}>Trust and open</button>
            <button class="btn" type="button" onclick={() => (projectProblem = null)}>Cancel</button>
          </div>
        {:else if projectProblem.kind === "only_project"}
          <h2 id="project-problem-title">{projectProblem.project.name} is the only open project</h2>
          <p>Forge always shows one project. Open another one first; then this project can be closed, and once closed, removed from the list. Its folder and saved sessions are never deleted.</p>
          <div class="dialog-actions">
            <button class="btn" type="button" onclick={() => (projectProblem = null)}>Cancel</button>
            <button class="btn primary" type="button" onclick={() => { projectProblem = null; addProject(); }}>Open another project…</button>
          </div>
        {:else}
          <h2 id="project-problem-title">{projectProblem.project.name} has moved</h2>
          <p>{projectProblem.message}</p>
          <div class="dialog-actions">
            <button class="btn primary" type="button" onclick={locateProject}>Locate…</button>
            <button class="btn danger" type="button" onclick={() => removeProject(projectProblem.project)}>Remove</button>
            <button class="btn" type="button" onclick={() => (projectProblem = null)}>Cancel</button>
          </div>
        {/if}
      </div>
    </div>
  {/if}

  {#if closeTarget}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="close-project-title" tabindex="-1" use:modal={{ onClose: () => (closeTarget = null) }}>
        <h2 id="close-project-title">{closeTarget.project.name} is still working</h2>
        <p>These sessions would stop. Prompts still queued are kept for when you reopen the project.</p>
        <ul class="project-list">
          {#each closeTarget.sessions as busy (busy.session_id)}
            <li>
              <span class="project-row static"><span class="state-dot {busy.turn_state}"></span><span class="project-name">{busy.label || "Session"}</span><span class="project-meta">{busy.turn_state}</span></span>
              <button type="button" class="btn" onclick={() => { const target = closeTarget; closeTarget = null; openElsewhere({ project_id: target.project.id, project_name: target.project.name, session_id: busy.session_id }); }}>Go to session</button>
            </li>
          {/each}
        </ul>
        <div class="dialog-actions">
          <button class="btn danger" type="button" onclick={() => closeProject(closeTarget.project, true)}>Stop all and close</button>
          <button class="btn" type="button" onclick={() => (closeTarget = null)}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}

  {#if providersOpen}
    <Providers onClose={() => (providersOpen = false)} onChanged={async () => { models = await invoke("list_models"); }} />
  {/if}
  {#if githubOpen}
    {#key sessionId}
      <Github
        onClose={() => (githubOpen = false)}
        sessionId={sessionId}
        sessionIssue={githubIssue}
        sessionPr={githubPr}
        onCreateSession={createIssueSession}
        onChanged={refreshFiles}
      />
    {/key}
  {/if}
  {#if detailsOpen}
    {#key sessionId}<Details {sessionId} onClose={() => (detailsOpen = false)} />{/key}
  {/if}
  {#if helpOpen}
    <Help onClose={() => (helpOpen = false)} onSettings={() => { helpOpen = false; settingsOpen = true; }} />
  {/if}
  {#if settingsOpen}
    <Settings bind:theme={themeChoice} bind:sound={attentionSound} {themes} onClose={() => (settingsOpen = false)} onProviders={() => { settingsOpen = false; providersOpen = true; }} onGithub={supportsGit ? () => { settingsOpen = false; githubOpen = true; } : null} />
  {/if}
  {#if newSessionOpen}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="new-session-title" tabindex="-1" use:modal={{ onClose: () => (newSessionOpen = false) }}>
        <form onsubmit={submitNewSession}>
          <h2 id="new-session-title">New session</h2>
          <p>{supportsGit ? "The session gets its own Git worktree and branch, so it can work while others do." : "The session works in this folder."}</p>
          <input class="field new-session-name" bind:value={newSessionName} placeholder="What is this session for?" aria-label="New session name" />
          <div class="dialog-actions">
            {#if supportsGit}
              <button class="btn ghost attach-instead" type="button" onclick={() => { newSessionOpen = false; openAttach(); }}>Attach an existing worktree…</button>
            {/if}
            <button class="btn" type="button" onclick={() => (newSessionOpen = false)}>Cancel</button>
            <button class="btn primary" type="submit" disabled={creating || !newSessionName.trim()}>Create</button>
          </div>
        </form>
      </div>
    </div>
  {/if}

  {#if gitConfirm}
    <div class="modal git-confirm">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="git-confirm-title" tabindex="-1" use:modal={{ onClose: () => (gitConfirm = null) }}>
        <h2 id="git-confirm-title">{gitConfirm.operation.kind === "delete_branch" ? `Delete branch “${gitConfirm.operation.name}”?` : gitConfirm.operation.kind === "abort_merge" ? "Abort this merge?" : `${gitConfirm.operation.kind === "push" ? "Push to" : "Pull from"} upstream?`}</h2>
        <p class="mono">{gitConfirm.workspace}</p>
        {#if gitConfirm.operation.kind === "delete_branch"}
          <p>The branch reference is removed; its worktree is not deleted.</p>
          <label><input type="checkbox" bind:checked={gitConfirm.operation.force} /> Delete even if the branch contains unmerged commits</label>
        {:else if gitConfirm.operation.kind === "abort_merge"}
          <p>Git will abandon the in-progress merge and attempt to restore the pre-merge working tree.</p>
        {:else}
          <p>{gitConfirm.upstream || "No upstream configured"}. {gitConfirm.operation.kind === "push" ? "This publishes your local commits to the configured upstream." : "This downloads and merges upstream changes. Conflicts remain available for you to resolve."}</p>
        {/if}
        <div class="dialog-actions">
          <button class="btn" type="button" onclick={() => (gitConfirm = null)}>Cancel</button>
          <button class="btn danger" type="button" onclick={confirmGitOperation}>{gitConfirm.operation.kind === "delete_branch" ? "Delete branch" : gitConfirm.operation.kind === "abort_merge" ? "Abort merge" : gitConfirm.operation.kind === "push" ? "Push" : "Pull"}</button>
        </div>
      </div>
    </div>
  {/if}
</main>

{#if !booted && checked}
  {#if needsTrust}
    <TrustScreen
      workspace={trustWorkspace}
      error={trustError}
      busy={trusting}
      {needsFolder}
      noGit={trustNoGit}
      needsConfirmation={trustConfirm}
      onChoose={chooseFolder}
      projects={folderProjects}
      onOpenProject={openRegistered}
      onTrust={acceptTrust}
      onQuit={() => invoke("quit_app").catch(() => {})}
    />
  {:else}
    <div class="modal">
      <div class="dialog" role="alert">
        <h2>Unable to open workspace</h2>
        <p>{trustError || error || "Starting Forge…"}</p>
        <button class="btn" type="button" onclick={() => boot().catch((cause) => (error = String(cause)))}>Retry</button>
        <button class="btn" type="button" onclick={() => invoke("quit_app")}>Quit</button>
      </div>
    </div>
  {/if}
{/if}

<style>
  .git-confirm { z-index: 45; }
  .child-dialog { width: min(58rem, calc(100vw - 48px)); max-height: 82vh; display: flex; flex-direction: column; }
  .child-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .child-messages { overflow: auto; min-height: 0; margin-top: 12px; }
  .child-message { padding: 8px 0; border-top: 1px solid var(--border); }
  .child-role { color: var(--muted); font-size: var(--text-xs); }
  .child-message pre { margin: 4px 0 0; white-space: pre-wrap; word-break: break-word; font-size: var(--text-sm); line-height: 20px; }
  .app {
    display: grid;
    grid-template-columns: 280px minmax(0, 1fr) 400px;
    grid-template-rows: var(--bar) minmax(0, 1fr);
    height: 100vh;
    min-height: 0;
    background: var(--bg);
  }

  /* A focus screen hides the rail and the changes column and spans the window. */
  .app.focus { grid-template-columns: minmax(0, 1fr); }
  .app.focus > :global(.sidebar:not(.open)) { display: none; }

  .topbar {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    padding: 0 12px 0 0;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 280px;
    flex: none;
    min-width: 0;
    padding: 0 16px;
  }
  .app.focus .brand { width: auto; }
  /* macOS draws the window's close, minimise and zoom buttons over the
     bar's left end; the brand starts clear of them. */
  .topbar.mac .brand { padding-left: 84px; }
  .wordmark { overflow: hidden; font-size: var(--text-lg); font-weight: 600; letter-spacing: -0.01em; text-overflow: ellipsis; white-space: nowrap; }

  /* The session's name keeps its room; the branch gives way first and
     never wraps. */
  .head-title { display: flex; align-items: center; gap: 8px; flex: 0 1 auto; min-width: 0; padding-left: 12px; }
  .head-title strong,
  .focus-head strong { font-size: var(--text-md); font-weight: 600; white-space: nowrap; }
  .head-title strong { flex: 0 1 auto; min-width: 96px; overflow: hidden; text-overflow: ellipsis; }
  .head-title .state-dot { flex: none; }
  .head-state { flex: none; color: var(--muted); font-size: var(--text-sm); white-space: nowrap; }
  .head-branch { flex: 0 10 auto; min-width: 0; max-width: 200px; white-space: nowrap; }
  .head-branch span { overflow: hidden; text-overflow: ellipsis; }
  .head-branch :global(svg) { flex: none; }
  .head-icon { position: relative; flex: none; width: var(--control-lg); height: var(--control-lg); color: var(--secondary); }
  .head-icon.on { background: var(--raised); color: var(--text); }
  .head-icon .count { position: absolute; top: 0; right: 0; min-width: 14px; padding: 0 3px; border-radius: 7px; background: var(--accent-soft); color: var(--accent); font-size: var(--text-xs); font-weight: 600; line-height: 14px; text-align: center; }
  .head-rule { flex: none; width: 1px; height: 16px; margin: 0 8px; background: var(--border); }
  .focus-head > strong { margin-left: 12px; }
  .head-session { margin-left: 4px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); font-size: var(--text-sm); }
  .head-doc { flex: none; max-width: 280px; margin-left: 8px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .head-btn { flex: none; }
  .head-btn .count {
    min-width: 16px; padding: 0 4px; border-radius: 8px;
    background: var(--accent-soft); color: var(--accent); font-size: var(--text-xs); font-weight: 600; line-height: 16px; text-align: center;
  }
  .head-btn.needs { border-color: color-mix(in srgb, var(--warning) 50%, var(--border)); color: var(--warning); }
  .model-name { max-width: 200px; overflow: hidden; text-overflow: ellipsis; }
  .rail-toggle { display: none; }

  /* The strips between the conversation and the composer share its column. */
  .current-step,
  .queue-strip,
  .banner {
    width: calc(100% - 68px);
    max-width: 720px;
    margin-left: auto;
    margin-right: auto;
  }

  .current-step {
    display: flex; align-items: center; gap: 8px; margin-top: 0; margin-bottom: 8px;
    color: var(--muted); font-size: var(--text-sm);
  }
  .current-step span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .link { padding: 0; border: 0; background: none; color: var(--accent); font-size: var(--text-sm); white-space: nowrap; text-decoration: underline; text-underline-offset: 3px; }

  .queue-strip {
    display: flex; align-items: center; gap: 8px; margin-top: 0; margin-bottom: 8px; padding: 4px 4px 4px 12px;
    border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--surface);
    color: var(--muted); font-size: var(--text-sm);
  }
  .queue-text { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .queue-strip .btn { height: 24px; padding: 0 8px; }

  /* A wait with no card of its own: what the session needs, in words. */
  .wait-note {
    display: flex; align-items: center; gap: 8px;
    width: calc(100% - 68px); max-width: 720px; margin: 0 auto 8px; padding: 8px 8px 8px 12px;
    border: 1px solid color-mix(in srgb, var(--warning) 45%, var(--border)); border-radius: var(--radius-sm);
    background: var(--surface); color: var(--secondary); font-size: var(--text-sm);
  }
  .wait-note span:nth-child(2) { flex: 1; min-width: 0; }
  .wait-note .btn { height: 24px; padding: 0 8px; flex: none; }
  .head-btn.on { background: var(--raised); color: var(--text); }
  .new-session-name { margin-bottom: 16px; }
  .attach-instead { margin-right: auto; }

  .decision { display: contents; }

  .center {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    background: var(--bg);
  }

  /* One accent fill at a time: while a decision is docked, its first
     button is the primary action and Send steps back. */
  .center.deciding :global(.composer .btn.primary) {
    border-color: var(--border);
    background: var(--raised);
    color: var(--text);
  }

  .chat-host {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  .terminal-host {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  .terminal-host.hidden {
    display: none;
  }

  .terminal-host.drawer {
    flex: 0 0 auto;
    height: 18rem;
  }

  .terminal-host.focused {
    flex: 1;
  }

  .spacer {
    flex: 1;
  }

  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 12px;
    margin-bottom: 0;
    padding: 4px 4px 4px 12px;
    border: 1px solid color-mix(in srgb, var(--error) 45%, var(--border));
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--error);
    font-size: var(--text-sm);
  }

  .banner span {
    flex: 1;
    overflow-wrap: anywhere;
  }

  /* Dialogs: one shell for every confirmation the page asks. */
  .modal {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    z-index: 20;
  }

  /* The Session panel hangs under its button with no scrim: it holds facts
     and routine actions, and a click anywhere else puts it away. */
  .modal.popover {
    place-items: start end;
    padding: calc(var(--bar) + 4px) 12px 12px;
    background: transparent;
  }

  .modal.popover > :global(*) {
    max-height: calc(100dvh - var(--bar) - 16px);
  }

  .dialog {
    width: min(26rem, calc(100vw - 48px));
    max-height: calc(100dvh - 48px);
    overflow: auto;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .dialog h2 {
    margin: 0 0 8px;
    font-size: var(--text-lg);
    font-weight: 600;
    line-height: 24px;
  }

  .dialog p {
    margin: 0 0 16px;
    color: var(--secondary);
    font-size: var(--text-md);
  }

  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    flex-wrap: wrap;
  }

  .commit-message {
    height: auto;
    min-height: 88px;
    margin-bottom: 12px;
    padding: 8px 12px;
    resize: vertical;
    font-family: var(--font-ui);
    line-height: 20px;
  }

  .project-list {
    margin: 0 0 16px;
    padding: 0;
    list-style: none;
  }

  .project-list li {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .project-row {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    height: var(--control-lg);
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text);
    font-size: var(--text-md);
    text-align: left;
    cursor: pointer;
    transition: background var(--fast) var(--ease);
  }

  .project-row.static {
    cursor: default;
  }

  .project-row:hover:not(.static):not(:disabled) {
    background: var(--hover);
  }

  .project-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .project-meta {
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .dialog-error {
    margin: 0 0 12px;
    color: var(--error);
    font-size: var(--text-sm);
  }

  .danger-line {
    color: var(--error);
  }

  @media (prefers-reduced-motion: reduce) {
    .project-row { transition: none; }
  }

  /* Narrow: the changes column gives its room to the conversation; Files
     in the header still reaches every change. */
  @media (max-width: 1180px) {
    .app { grid-template-columns: 280px minmax(0, 1fr); }
    .app > :global(.changes) { display: none; }
  }

  /* Compact: the rail becomes an overlay opened from the header. */
  @media (max-width: 760px) {
    .app {
      grid-template-columns: minmax(0, 1fr);
    }

    .brand { width: auto; padding: 0 8px; }
    /* The state word outlasts the branch chip: a dot alone says nothing. */
    .head-title .chip { display: none; }

    .rail-toggle { display: inline-grid; }

    .app > :global(.sidebar.open) { z-index: 15; }
  }
</style>
