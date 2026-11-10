<script>
  import { onMount, tick } from "svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import Conversation from "./lib/Conversation.svelte";
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
  import { otherAttention, prunable } from "./view.js";
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
  let projectsOpen = $state(false);
  let projectBusy = $state(false);
  // A project that can't open yet: { project, kind, message }.
  let projectProblem = $state(null);
  // A busy close: { project, sessions }.
  let closeTarget = $state(null);
  let attentionSound = $state(soundEnabled());
  $effect(() => setSoundEnabled(attentionSound));
  const activeProject = $derived(projects.find((project) => project.id === projectId) ?? null);
  // Requests waiting in every project but this one.
  const elsewhere = $derived(otherAttention(projects, projectId));
  let resumable = $state([]);
  // One view record per session (see view.js): open documents, the active
  // document, the draft and the transcript position.
  let views = $state({});
  const allSessions = $derived([
    ...sessions,
    ...resumable.filter((row) => !sessions.some((session) => session.session_id === row.session_id)),
  ]);
  let turnState = $state("idle");
  let stopping = $state(false);
  let messages = $state([]);
  let promptHistory = $state([]);
  let skillDescriptions = $state([]);
  let streaming = $state("");
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
  let approval = $state(null);
  let question = $state(null);
  let questionDrafts = $state({});
  let approvalDrafts = $state({});
  let approveAll = $state(false);
  let thinking = $state(false);
  let imageInput = $state(false);
  let plan = $state(null);
  let tokens = $state(null);
  let background = $state([]);
  let queue = $state([]);
  let interrupted = $state([]);
  let counts = $state({ tools: 0, skills: 0, allows: 0 });
  let attention = $state({});
  let error = $state("");
  let info = $state({ workspace: "", branch: "", model: "", contextUsage: 0, queued: 0 });
  let githubIssue = $state(null);
  let githubPr = $state("");

  let files = $state([]);
  let filesLoading = $state(false);
  let filesError = $state("");
  let status = $state({ branch: "", files: [] });
  let lastTurnFiles = $state([]);
  // The open file, if any, sent with the next prompt; per session like drafts.
  let fileAttachments = $state({});
  // `/clear` is view-only in the TUI: the model keeps its context.
  let clearedFrom = $state({});
  // Full text of messages the operator expanded, re-applied after each
  // snapshot so a background update does not collapse them.
  let messageDetails = $state({});
  const fullMessageCount = {};

  let creating = $state(false);
  let pendingClose = $state("");
  let pickerOpen = $state(false);
  let providersOpen = $state(false);
  let githubOpen = $state(false);
  let detailsOpen = $state(false);
  let helpOpen = $state(false);
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
    { name: "/theme", help: "Switch the desktop theme", run: () => (helpOpen = true) },
    { name: "/status", help: "Show session status and token usage", run: () => (detailsOpen = true) },
    { name: "/context", help: "Show the context breakdown", run: () => (detailsOpen = true) },
    { name: "/plan", help: "Show the latest execution plan", run: () => (sessionMenu = "plan") },
    { name: "/tasks", help: "Show background tasks and queued prompts", run: () => (sessionMenu = "agents") },
    { name: "/approve-all", help: "Toggle approve-all for this session", run: toggleApproveAll },
    { name: "/thinking", help: "Toggle model reasoning", run: toggleThinking },
    { name: "/compact", help: "Continue in a fresh context", run: compactContext },
    { name: "/continue", help: "Continue the last stopped or failed turn", run: continueTurn },
    { name: "/new", help: "Start a new session", run: () => sidebar?.startNewSession() },
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
          closed ? command("resume_session", { sessionId: session.session_id }) : selectSession(session.session_id),
      };
    });
    for (const file of files) {
      if (file.is_dir) continue;
      items.push({
        id: `file:${file.path}`,
        group: "Files",
        label: basename(file.path),
        detail: file.path,
        action: () => openFile(file.path),
      });
    }
    // Editor items act on the mounted FilePane, which only the Files screen has.
    const editing = currentView.screen === "files" ? activeDoc : null;
    if (editing?.kind === "file") {
      items.push(
        { id: "editor:save", group: "Editor", label: "Save current file", detail: "⌘/Ctrl S", action: saveActive },
        { id: "editor:reload", group: "Editor", label: "Reload current file from disk", action: () => requestFileAction("reload") },
      );
      if (editing.file && !editing.file.read_only) items.push(
        { id: "editor:find", group: "Editor", label: "Find in current file", detail: "⌘/Ctrl F", action: () => filesPane?.showNavigation("find") },
        { id: "editor:replace", group: "Editor", label: "Find and replace in current file", action: () => filesPane?.showNavigation("replace") },
        { id: "editor:line", group: "Editor", label: "Jump to line in current file", detail: "⌘/Ctrl L", action: () => filesPane?.showNavigation("line") },
      );
      const attached = fileAttachments[sessionId]?.path === editing.path;
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
      { id: "action:files", group: "Actions", label: "Open Files", detail: "⌘/Ctrl E", action: openFiles },
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
        action: () => sidebar?.startNewSession(),
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
      { id: "action:help", group: "Actions", label: "Help, shortcuts and theme…", action: () => (helpOpen = true) },
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

  function setRowState(id, state) {
    sessions = sessions.map((session) =>
      session.session_id === id ? { ...session, turn_state: state } : session,
    );
  }

  function applySession(event) {
    sessionId = event.session_id;
    turnState = event.turn_state;
    fullMessageCount[event.session_id] = event.messages.length;
    promptHistory = event.messages.filter((message) => message.role === "user" && message.text.trim())
      .map((message) => message.text).slice(-500);
    skillDescriptions = event.skill_descriptions ?? [];
    const from = clearedFrom[event.session_id] ?? 0;
    messages = event.messages.slice(from).map((message, index) => {
      const full = messageDetails[`${event.session_id}:${from + index}`];
      return full ? { ...message, text: full.text, thinking: full.thinking } : message;
    });
    approval = event.pending_approval;
    question = event.pending_question ?? null;
    approveAll = event.approve_all;
    thinking = event.thinking;
    imageInput = event.image_input ?? false;
    plan = event.plan ?? null;
    tokens = event.tokens ?? null;
    background = event.background ?? [];
    queue = event.queue ?? [];
    interrupted = event.interrupted ?? [];
    counts = { tools: event.tools ?? 0, skills: event.skills ?? 0, allows: event.pattern_allows ?? 0 };
    if (event.turn_state !== "waiting") {
      const { [event.session_id]: _, ...rest } = attention;
      if (Object.keys(rest).length !== Object.keys(attention).length) attention = rest;
    }
    info = {
      workspace: event.workspace,
      branch: event.branch,
      model: event.model,
      contextUsage: event.context_usage,
      queued: event.queued,
    };
    githubIssue = event.github_issue_number ?? null;
    githubPr = event.github_pr_url ?? "";
    // Keep the live delta while the turn runs; a mid-turn snapshot would
    // wipe it. Clear once the turn settles so the final transcript owns it.
    if (!["running", "waiting"].includes(event.turn_state)) streaming = "";
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
      const snapshot = await invoke("current_session");
      if (!snapshot || snapshot.session_id !== sessionId) return;
      const questionId = snapshot.pending_question?.call_id ?? "";
      const approvalId = snapshot.pending_approval?.call_id ?? "";
      const localTotal = messages.length + (clearedFrom[sessionId] ?? 0);
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

  let filesRequest = 0;
  async function refreshFiles() {
    if (!sessionId) return;
    const request = ++filesRequest;
    const owner = sessionId;
    filesLoading = true;
    filesError = "";
    try {
      // A folder that isn't a Git repository has files but no Git status;
      // asking for it would fail and hide the file list too.
      const [entries, nextStatus, written] = await Promise.all([
        invoke("list_files", { sessionId: owner }),
        supportsGit ? invoke("git_status", { sessionId: owner }) : { branch: "", files: [] },
        supportsGit ? invoke("git_last_turn_files", { sessionId: owner }) : [],
      ]);
      if (request !== filesRequest || owner !== sessionId) return;
      files = entries;
      status = nextStatus;
      lastTurnFiles = written;
      markStaleDiffs(owner, nextStatus);
    } catch (cause) {
      if (request === filesRequest && owner === sessionId) filesError = String(cause);
    } finally {
      if (request === filesRequest && owner === sessionId) filesLoading = false;
    }
  }

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
      if (event.session_id === sessionId) streaming += event.event.text;
      return;
    }
    if (event.type === "session") {
      setRowState(event.session_id, event.turn_state);
      if (event.turn_state !== "waiting" && attention[event.session_id]) {
        const { [event.session_id]: _, ...rest } = attention;
        attention = rest;
      }
      if (event.session_id !== sessionId) return;
      viewFor(views, event.session_id);
      const wasBusy = lastTurn === "running" || lastTurn === "waiting";
      lastTurn = event.turn_state;
      applySession(event);
      if (focusDecisionFor === event.session_id) {
        focusDecisionFor = null;
        if (event.pending_question || event.pending_approval) tick().then(() => focusDecision());
      }
      if (wasBusy && !["running", "waiting"].includes(event.turn_state)) refreshFiles();
      return;
    }
    if (event.type === "roster") {
      for (const row of event.sessions) sessionOwner[row.session_id] = projectId;
      sessions = event.sessions;
      reconcileRoster(event.sessions);
      return;
    }
    if (event.type === "selected") {
      const next = event.session_id ?? "";
      if (next !== sessionId) {
        sessionId = next;
        messages = [];
        promptHistory = [];
        skillDescriptions = [];
        streaming = "";
        approval = null;
        question = null;
        plan = null;
        background = [];
        queue = [];
        interrupted = [];
        // These dialogs act on the previously selected session.
        branchesOpen = false;
        commitOpen = false;
        gitConfirm = null;
        fileAction = null;
        childView = null;
        if (next) {
          const { [next]: _, ...rest } = attention;
          attention = rest;
        }
        // The session's record already holds its documents, draft, screen
        // and drawer; selection only changes which record renders.
        if (next) viewFor(views, next);
        // The tree and branch must never show the previous session. Status
        // and last-turn files are kept until the refresh replaces them, so
        // the Files badge and Stage all do not blink off during the switch.
        files = [];
        info = { ...info, branch: "" };
        if (next) refreshFiles();
      }
      return;
    }
    if (event.type === "attention") {
      setRowState(event.session_id, event.turn_state);
      if (event.session_id !== sessionId) attention = { ...attention, [event.session_id]: true };
      // Finishing, stopping or waiting for input is normal; only a failed
      // turn deserves the red banner.
      if (event.session_id === sessionId && event.turn_state === "failed") error = event.message;
      return;
    }
    if (event.type === "error") {
      error = event.message;
      return;
    }
    if (event.type === "lagged") {
      error = `Missed ${event.missed} update${event.missed === 1 ? "" : "s"}; the view may be stale.`;
    }
  }

  const WORKSPACE_COMMANDS = new Set([
    "list_files", "read_file", "read_image", "git_status", "git_diff", "git_last_turn_files",
    "write_file", "create_file", "create_dir", "rename_path", "delete_path", "search_files",
    "git_stage", "git_unstage", "git_commit", "git_branches", "git_switch_branch",
    "git_create_branch", "git_merge_branch", "git_rename_branch", "git_restore", "git_discard_hunk",
    "github_issues", "github_details", "github_logs",
  ]);

  async function command(name, args) {
    const mutatesGit = name.startsWith("git_");
    if (mutatesGit && gitBusy) { error = "Wait for the current Git operation to finish."; return false; }
    // Buffers live in each session's record and survive switches, so only
    // commands that end a session refuse unsaved documents.
    if (["close_session", "archive_session", "remove_worktree"].includes(name) && dirtySession(args?.sessionId)) {
      error = "Save or close unsaved files before changing this session.";
      return false;
    }
    let invokeArgs = args;
    if (WORKSPACE_COMMANDS.has(name) && sessionId && !invokeArgs?.sessionId) {
      invokeArgs = { sessionId, ...(invokeArgs ?? {}) };
    }
    if (mutatesGit) gitBusy = true;
    try {
      const result = await invoke(name, invokeArgs);
      if (name === "select_session") {
        // Selection must update the pane even if channel delivery is stalled.
        onEvent({ type: "selected", session_id: result.session_id });
        onEvent(result);
      }
      if (name === "stop_session" || name === "stop_turn") onEvent(result);
      if (["resume_session", "close_session", "archive_session", "remove_worktree"].includes(name)) loadResumable();
      return true;
    } catch (cause) {
      error = String(cause);
      return false;
    } finally { if (mutatesGit) gitBusy = false; }
  }

  async function loadResumable() {
    try {
      resumable = await invoke("list_resumable_sessions");
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
    files = [];
    status = { branch: "", files: [] };
    lastTurnFiles = [];
    resumable = [];
    rememberProject(id);
    loadResumable();
  }

  // Put `project` on screen, opening it if needed. True when it worked.
  async function switchProject(project) {
    projectsOpen = false;
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
    projectsOpen = false;
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
    archiveRemove = session.lifecycle !== "active" && session.ownership === "managed";
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
        resumable = await invoke("list_resumable_sessions");
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
    const id = sessionId;
    if (fileAttachments[id]?.path === tab.path) {
      const { [id]: _, ...rest } = fileAttachments;
      fileAttachments = rest;
      return;
    }
    fileAttachments = {
      ...fileAttachments,
      [id]: { path: tab.path, line: Math.max(0, filesPane?.cursorLine() ?? 0) },
    };
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
    const attachment = fileAttachments[targetSession] ?? null;
    // Show the operator's line before the supervisor's snapshot arrives.
    // The next SessionUpdated replaces this local copy with the real message.
    const localId = `local:${Date.now()}:${Math.random().toString(36).slice(2)}`;
    const local = targetSession === sessionId && text
      ? { role: "user", text, attachments: [], optimistic: true, localId }
      : null;
    const previousTurnState = turnState;
    if (local) {
      messages = [...messages, local];
      turnState = "running";
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
      messages = messages.filter((message) => message.localId !== localId);
      if (targetSession === sessionId && turnState === "running") turnState = previousTurnState;
      stopping = false;
    }
    if (ok && attachment) {
      const { [targetSession]: _, ...rest } = fileAttachments;
      fileAttachments = rest;
    }
    return ok;
  }

  async function stop() {
    if (stopping || !sessionId) return;
    stopping = true;
    if (!(await command("stop_session", { sessionId }))) stopping = false;
  }

  async function decide(callId, decision, note, confirmed) {
    await command("decide_approval", { callId, decision, note, confirmed });
  }

  async function answerQuestion(callId, answers, skip) {
    await command("answer_question", { callId, answers, skip });
  }

  async function toggleApproveAll() {
    if (!approveAll) {
      approveAllPending = true;
      return;
    }
    await command("set_approve_all", { on: false });
  }

  async function confirmApproveAll() {
    approveAllPending = false;
    await command("set_approve_all", { on: true });
  }

  async function continueTurn() {
    await command("continue_turn");
  }

  async function compactContext() {
    await command("compact_context");
  }

  async function toggleThinking() {
    await command("set_thinking", { enabled: !thinking });
  }

  async function expandMessage(index) {
    const target = sessionId;
    try {
      const absolute = (clearedFrom[target] ?? 0) + index;
      const full = await invoke("message_detail", { sessionId: target, index: absolute });
      const key = `${target}:${absolute}`;
      messageDetails = { ...messageDetails, [key]: full };
      if (target === sessionId) {
        messages = messages.map((message, at) =>
          at === index ? { ...message, text: full.text, thinking: full.thinking } : message);
      }
    } catch (cause) {
      if (target === sessionId) error = String(cause);
    }
  }

  function clearTranscript() {
    if (!sessionId) return;
    clearedFrom = { ...clearedFrom, [sessionId]: fullMessageCount[sessionId] ?? messages.length };
    messages = [];
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
    await command("fork_session");
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
    const ok = await command(kind === "file" ? "create_file" : "create_dir", { path });
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
    const ok = await command("rename_path", { from: target.path, to: target.value.trim() });
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
    const ok = await command("delete_path", { path: entry.path });
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
    if (await command("git_stage", { path })) refreshFiles();
  }

  async function unstagePath(path) {
    if (await command("git_unstage", { path })) refreshFiles();
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
    if (owner === sessionId) refreshFiles();
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
    const ok = await command("git_switch_branch", { name });
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
    const ok = await command("git_merge_branch", { name });
    if (ok) await reloadBranches();
    else { branchesError = error; refreshFiles(); }
  }

  async function createBranch(name) {
    if (dirtyTabs()) { branchesError = "Save or close unsaved files before creating a branch."; return; }
    branchesError = "";
    if (await command("git_create_branch", { name })) {
      for (const tab of [...currentView.tabs]) {
        removeTab(tab.id);
      }
      await reloadBranches();
    } else branchesError = error;
  }

  async function renameBranch(from, to) {
    branchesError = "";
    if (await command("git_rename_branch", { from, to })) await reloadBranches();
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
      if (owner === sessionId) refreshFiles();
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
      if (owner === sessionId) refreshFiles();
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
      if (owner === sessionId) refreshFiles();
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
      await invoke("set_model", { modelId, profileId, effort });
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
      themes = await invoke("list_themes");
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
  <Sidebar
    bind:this={sidebar}
    sessions={allSessions}
    activeId={sessionId}
    project={activeProject?.name || basename(trustWorkspace)}
    projectCount={projects.length}
    {elsewhere}
    onProjects={() => (projectsOpen = true)}
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
    onResume={(session) => { railOpen = false; command("resume_session", { sessionId: session.session_id }); }}
    onHelp={() => (helpOpen = true)}
  />

  <section class="center">
    {#if currentView.screen === "session"}
      <header class="session-head">
        <button bind:this={railButton} type="button" class="icon-btn rail-toggle" aria-label="Sessions" aria-expanded={railOpen} onclick={() => (railOpen = true)}>
          <Icon name="layers" size={14} />
        </button>
        <div class="head-title">
          <strong>{activeSession?.label ?? "No session"}</strong>
          {#if info.workspace}<span class="checkout mono" title={info.workspace}>{basename(info.workspace)}</span>{/if}
        </div>
        <span class="spacer"></span>
        <button bind:this={filesButton} type="button" class="btn head-btn" title="Files (⌘/Ctrl E)" disabled={!sessionId} onclick={openFiles}>
          <Icon name="files" size={13} />
          Files
          {#if fileTotals.changed > 0}<span class="count" aria-label={`${fileTotals.changed} changed`}>{fileTotals.changed}</span>{/if}
        </button>
        {#if needsYou > 0}
          <button type="button" class="btn head-btn needs" onclick={showNeedsYou}>Needs you ({needsYou})</button>
        {/if}
        <button bind:this={terminalButton} type="button" class="btn head-btn" title="Focus terminal" disabled={!sessionId} onclick={async () => { await openTerminal(); await focusTerminal(); }}>
          <Icon name="terminal" size={13} />
          Terminal
        </button>
        <button type="button" class="btn head-btn" aria-haspopup="dialog" disabled={!sessionId} onclick={() => (sessionMenu = "actions")}>Session…</button>
      </header>
    {:else}
      <header class="focus-head">
        <button type="button" class="icon-btn rail-toggle" aria-label="Sessions" aria-expanded={railOpen} onclick={() => (railOpen = true)}>
          <Icon name="layers" size={14} />
        </button>
        <button type="button" class="btn head-btn" onclick={backToSession}>
          <Icon name="arrow-left" size={13} />
          Session
        </button>
        <strong>{currentView.screen === "files" ? "Files" : "Terminal"}</strong>
        <span class="head-session">{activeSession?.label ?? ""}</span>
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
      </header>
    {/if}

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

      <div class="decision">
        {#if question}
          <QuestionCard
            {question}
            onAnswer={answerQuestion}
            savedDrafts={questionDrafts[`${sessionId}:${question.call_id}`] ?? null}
            onDrafts={(drafts) => {
              const key = `${sessionId}:${question.call_id}`;
              if (drafts) questionDrafts = { ...questionDrafts, [key]: drafts };
              else {
                const { [key]: _, ...rest } = questionDrafts;
                questionDrafts = rest;
              }
            }}
          />
        {/if}

        {#if approval}
          <ApprovalCard
            {approval}
            onDecide={decide}
            savedDraft={approvalDrafts[`${sessionId}:${approval.call_id}`] ?? null}
            onDraft={(draft) => {
              const key = `${sessionId}:${approval.call_id}`;
              if (draft) approvalDrafts = { ...approvalDrafts, [key]: draft };
              else {
                const { [key]: _, ...rest } = approvalDrafts;
                approvalDrafts = rest;
              }
            }}
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
        attachment={fileAttachments[sessionId] ?? null}
        onRemoveAttachment={() => {
          const { [sessionId]: _, ...rest } = fileAttachments;
          fileAttachments = rest;
        }}
        onSend={send}
        onStop={stop}
        drawerOpen={currentView.drawerOpen && !!shells[sessionId]}
        onToggleTerminal={toggleTerminal}
        onFocusTerminal={focusTerminal}
        onStopTerminal={() => dropShell(sessionId)}
        contextUsage={info.contextUsage}
        onContext={() => (sessionMenu = "context")}
      />
    {:else if currentView.screen === "files"}
      {#key sessionId}
        <Files
          bind:this={filesPane}
          {files}
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

  {#if sessionMenu !== null}
    <div class="modal">
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
        onClearApprovals={() => command("clear_approvals")}
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
    <Palette items={paletteItems} onClose={() => (paletteOpen = false)} />
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
          {archiveTarget.lifecycle === "active"
            ? `Archive “${archiveTarget.label}”?`
            : `Clean up “${archiveTarget.label}”?`}
        </h2>
        <p>
          {archiveTarget.lifecycle === "active"
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

  {#if projectsOpen}
    <div class="modal">
      <div class="dialog projects-dialog" role="dialog" aria-modal="true" aria-labelledby="projects-title" tabindex="-1" use:modal={{ onClose: () => (projectsOpen = false) }}>
        <h2 id="projects-title">Projects</h2>
        <ul class="project-list">
          {#each projects as project (project.id)}
            <li class:current={project.id === projectId}>
              <button type="button" class="project-row" disabled={projectBusy} onclick={() => switchProject(project)} title={project.root}>
                <span class="state-dot {project.open ? 'open' : 'closed'}"></span>
                <span class="project-name">{project.name}</span>
                {#if project.attention.length}<span class="project-count" aria-label="{project.attention.length} waiting on you">{project.attention.length}</span>{/if}
                {#if project.open}<span class="project-meta">{project.running}/{project.limit} running</span>{/if}
              </button>
              {#if project.open && project.id !== projectId}
                <button type="button" class="btn" onclick={() => closeProject(project)}>Close</button>
              {:else if !project.open}
                <button type="button" class="btn" onclick={() => removeProject(project)} title="Unregister; the folder and its sessions stay">Remove</button>
              {/if}
            </li>
          {/each}
        </ul>
        <div class="dialog-actions">
          <button class="btn primary" type="button" onclick={addProject}><Icon name="plus" size={13} /> Add project…</button>
          <button class="btn" type="button" onclick={() => (projectsOpen = false)}>Done</button>
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
        {githubIssue}
        githubPr={githubPr}
        onCreateSession={createIssueSession}
        onChanged={refreshFiles}
      />
    {/key}
  {/if}
  {#if detailsOpen}
    {#key sessionId}<Details onClose={() => (detailsOpen = false)} />{/key}
  {/if}
  {#if helpOpen}
    <Help bind:theme={themeChoice} bind:sound={attentionSound} {themes} onClose={() => (helpOpen = false)} onProviders={() => { helpOpen = false; providersOpen = true; }} onGithub={supportsGit ? () => { helpOpen = false; githubOpen = true; } : null} />
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
  .child-messages { overflow: auto; min-height: 0; margin-top: 10px; }
  .child-message { padding: 8px 0; border-top: 1px solid var(--border); }
  .child-role { font-size: 11px; letter-spacing: 0.05em; text-transform: uppercase; color: var(--muted); }
  .child-message pre { margin: 4px 0 0; white-space: pre-wrap; word-break: break-word; font-size: 12px; }
  .app {
    display: grid;
    grid-template-columns: 258px minmax(0, 1fr);
    height: 100vh;
    min-height: 0;
    background: var(--bg);
  }

  /* A focus screen hides the rail and spans the window. */
  .app.focus { grid-template-columns: minmax(0, 1fr); }
  .app.focus > :global(.sidebar:not(.open)) { display: none; }

  .session-head,
  .focus-head {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 42px;
    padding: 0 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }

  .head-title { display: flex; align-items: baseline; gap: 8px; min-width: 0; }
  .head-title strong,
  .focus-head strong { font-size: 13px; white-space: nowrap; }
  .head-title strong { overflow: hidden; text-overflow: ellipsis; }
  .checkout,
  .head-session { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); font-size: 11.5px; }
  .head-btn { display: inline-flex; align-items: center; gap: 5px; flex: none; }
  .head-btn .count {
    min-width: 16px; padding: 0 4px; border-radius: 8px;
    background: var(--accent-soft); color: var(--accent); font-size: 10.5px; font-weight: 700; text-align: center;
  }
  .head-btn.needs { color: var(--warning); }
  .rail-toggle { display: none; }

  .current-step {
    display: flex; align-items: center; gap: 8px; margin: 0; padding: 6px 18px;
    color: var(--muted); font-size: 12px;
  }
  .current-step span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .link { padding: 0; border: 0; background: none; color: var(--accent); font-size: 12px; white-space: nowrap; }
  .link:hover { text-decoration: underline; }

  .queue-strip {
    display: flex; align-items: center; gap: 10px; margin: 0 14px 6px; padding: 5px 6px 5px 10px;
    border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--muted); font-size: 12px;
  }
  .queue-text { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .decision { display: contents; }

  .center {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
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

  .chip {
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 8px 24px 0;
    padding: 7px 10px;
    border: 1px solid color-mix(in srgb, var(--error) 45%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--error) 10%, var(--surface));
    color: var(--error);
    font-size: 12px;
  }

  .banner span {
    flex: 1;
    overflow-wrap: anywhere;
  }

  .modal {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: rgba(5, 7, 9, 0.6);
    z-index: 20;
  }

  .dialog {
    width: min(26rem, calc(100vw - 48px));
    max-height: calc(100dvh - 48px);
    overflow: auto;
    padding: 18px;
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .dialog h2 {
    margin: 0 0 6px;
    font-size: 15px;
    font-weight: 600;
  }

  .dialog p {
    margin: 0 0 16px;
    color: var(--secondary);
    font-size: 12.5px;
  }

  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
    flex-wrap: wrap;
  }

  .dialog-actions .btn {
    height: 26px;
  }

  .commit-message {
    min-height: 88px;
    margin-bottom: 10px;
    padding: 8px;
    resize: vertical;
    font-family: var(--font-ui);
  }

  .project-list {
    margin: 0 0 14px;
    padding: 0;
    list-style: none;
  }

  .project-list li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 0;
  }

  .project-list li.current .project-row {
    background: var(--surface-raised, rgba(255, 255, 255, 0.06));
  }

  .project-row {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 6px 8px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
  }

  .project-row.static {
    cursor: default;
  }

  .project-row:hover:not(.static):not(:disabled) {
    background: var(--surface-raised, rgba(255, 255, 255, 0.06));
  }

  .project-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .project-count {
    min-width: 18px;
    padding: 0 5px;
    border-radius: 9px;
    background: var(--warning);
    color: #0e1115;
    font-size: 11px;
    font-weight: 600;
    text-align: center;
  }

  .project-meta {
    color: var(--muted);
    font-size: 11px;
  }

  /* A project's dot: open (running agents allowed) or closed. */
  .project-row .state-dot.open {
    background: var(--success);
  }

  .project-row .state-dot.closed {
    opacity: 0.35;
  }

  .dialog-error {
    margin: 0 0 10px;
    color: var(--error);
    font-size: 12px;
  }

  .danger-line {
    color: var(--error);
  }

  /* Compact: the rail becomes an overlay opened from the header. */
  @media (max-width: 760px) {
    .app {
      grid-template-columns: minmax(0, 1fr);
    }

    .rail-toggle { display: inline-grid; }

    .app > :global(.sidebar.open) { z-index: 15; }
  }
</style>
