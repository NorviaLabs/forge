<script>
  import { onMount } from "svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import Conversation from "./lib/Conversation.svelte";
  import Composer from "./lib/Composer.svelte";
  import Sidebar from "./lib/Sidebar.svelte";
  import Inspector from "./lib/Inspector.svelte";
  import ApprovalCard from "./lib/ApprovalCard.svelte";
  import QuestionCard from "./lib/QuestionCard.svelte";
  import FilePane from "./lib/FilePane.svelte";
  import ModelPicker from "./lib/ModelPicker.svelte";
  import Palette from "./lib/Palette.svelte";
  import BranchPicker from "./lib/BranchPicker.svelte";
  import TrustScreen from "./lib/TrustScreen.svelte";
  import Providers from "./lib/Providers.svelte";
  import Github from "./lib/Github.svelte";
  import Details from "./lib/Details.svelte";
  import Help from "./lib/Help.svelte";
  import { modal } from "./modal.js";
  import Icon from "./lib/Icon.svelte";

  let sessionId = $state("");
  let sessions = $state([]);
  let resumable = $state([]);
  // Per-session tabs, open file and prompt, like the TUI's view state.
  let views = $state({});
  const allSessions = $derived([
    ...sessions,
    ...resumable.filter((row) => !sessions.some((session) => session.session_id === row.session_id)),
  ]);
  let turnState = $state("idle");
  let messages = $state([]);
  let streaming = $state("");
  let approval = $state(null);
  let question = $state(null);
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
  let draft = $state("");
  let pendingClose = $state("");
  let pendingSwitch = $state("");
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
  let filePane = $state();
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
  let commitAll = $state(false);
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

  let tabs = $state([{ id: "chat", kind: "chat", title: "Conversation" }]);
  let activeTabId = $state("chat");
  // Sessions whose shell was opened. Each keeps its terminal mounted, so
  // switching away does not kill the PTY or forget its scrollback.
  let shells = $state({});

  let lastTurn = "";

  const activeTab = $derived(tabs.find((tab) => tab.id === activeTabId) ?? tabs[0]);
  const terminalTab = $derived(tabs.find((tab) => tab.kind === "terminal"));
  const activeSession = $derived(sessions.find((session) => session.session_id === sessionId));
  const changeMap = $derived(new Map(status.files.map((file) => [file.path, file.status])));
  const branch = $derived(status.branch || info.branch);

  // Tell Rust whether a close would discard work; it asks before quitting.
  // Before the trust screen is answered there is no session to guard.
  function dirtyAnywhere() {
    return tabs.some((tab) => tab.dirty)
      || Object.values(views).some((view) => view.tabs?.some((tab) => tab.dirty));
  }

  $effect(() => {
    if (!booted) return;
    const unsaved = dirtyAnywhere();
    const busy = sessions.some((session) => ["running", "waiting"].includes(session.turn_state))
      || Object.keys(attention).length > 0;
    invoke("set_unsaved", { unsaved, busy }).catch(() => {});
  });

  // A session waiting out of sight is still a session waiting: the window
  // title is the only surface a background tab has.
  $effect(() => {
    const waiting = Object.keys(attention).length;
    document.title = waiting ? `(${waiting}) Forge — needs you` : "Forge";
  });

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
    if (activeTab?.kind === "file") {
      items.push(
        { id: "editor:save", group: "Editor", label: "Save current file", detail: "⌘/Ctrl S", action: saveActive },
        { id: "editor:reload", group: "Editor", label: "Reload current file from disk", action: () => requestFileAction("reload") },
      );
      if (activeTab.file && !activeTab.file.read_only) items.push(
        { id: "editor:find", group: "Editor", label: "Find in current file", detail: "⌘/Ctrl F", action: () => filePane?.showNavigation("find") },
        { id: "editor:replace", group: "Editor", label: "Find and replace in current file", action: () => filePane?.showNavigation("replace") },
        { id: "editor:line", group: "Editor", label: "Jump to line in current file", detail: "⌘/Ctrl L", action: () => filePane?.showNavigation("line") },
      );
      const attached = fileAttachments[sessionId]?.path === activeTab.path;
      items.push({
        id: "editor:attach",
        group: "Editor",
        label: attached ? "Remove attached file context" : "Attach current file to the next prompt",
        action: () => toggleFileAttachment(activeTab),
      });
    } else if (activeTab?.kind === "diff") {
      if (activeTab.side !== "last_turn") {
        items.push({ id: "editor:restore", group: "Editor", label: "Restore current file to HEAD…", action: () => requestFileAction("restore") });
      }
      // Only the combined view's hunk order matches the working-tree reverse
      // apply; a staged review can omit hunks and shift the index.
      if (activeTab.side === "combined" && !activeTab.diff?.truncated) {
        hunkLabels(activeTab.diff?.text ?? "").forEach((label, index) => items.push({
          id: `hunk:${index}`,
          group: "Git",
          label: `Discard hunk ${index + 1} (${label})…`,
          action: () => requestFileAction("hunk", index),
        }));
      }
    }
    if (activeTab && (activeTab.kind === "file" || activeTab.kind === "diff")) {
      items.push(
        { id: "diff:combined", group: "Git", label: "Review HEAD to working tree", action: () => openDiff(activeTab.path, "combined") },
        { id: "diff:staged", group: "Git", label: "Review staged changes", action: () => openDiff(activeTab.path, "staged") },
        { id: "diff:unstaged", group: "Git", label: "Review unstaged changes", action: () => openDiff(activeTab.path, "unstaged") },
        { id: "git:stage", group: "Git", label: `Stage ${basename(activeTab.path)}`, action: () => stagePath(activeTab.path) },
        { id: "git:unstage", group: "Git", label: `Unstage ${basename(activeTab.path)}`, action: () => unstagePath(activeTab.path) },
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
    return items;
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

  function updateTab(id, patch) {
    tabs = tabs.map((tab) => (tab.id === id ? { ...tab, ...patch } : tab));
  }

  function applySession(event) {
    sessionId = event.session_id;
    turnState = event.turn_state;
    fullMessageCount[event.session_id] = event.messages.length;
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
    streaming = "";
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

  let filesRequest = 0;
  async function refreshFiles() {
    if (!sessionId) return;
    const request = ++filesRequest;
    const owner = sessionId;
    filesLoading = true;
    filesError = "";
    try {
      const [entries, nextStatus, written] = await Promise.all([
        invoke("list_files"),
        invoke("git_status"),
        invoke("git_last_turn_files"),
      ]);
      if (request !== filesRequest || owner !== sessionId) return;
      files = entries;
      status = nextStatus;
      lastTurnFiles = written;
    } catch (cause) {
      if (request === filesRequest && owner === sessionId) filesError = String(cause);
    } finally {
      if (request === filesRequest && owner === sessionId) filesLoading = false;
    }
  }

  function onEvent(event) {
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
      const wasBusy = lastTurn === "running" || lastTurn === "waiting";
      lastTurn = event.turn_state;
      applySession(event);
      if (wasBusy && !["running", "waiting"].includes(event.turn_state)) refreshFiles();
      return;
    }
    if (event.type === "roster") {
      sessions = event.sessions;
      return;
    }
    if (event.type === "selected") {
      const next = event.session_id ?? "";
      if (next !== sessionId) {
        const previous = sessionId;
        if (previous) views[previous] = { tabs, activeTabId, draft };
        sessionId = next;
        messages = [];
        approval = null;
        question = null;
        plan = null;
        background = [];
        queue = [];
        interrupted = [];
        streaming = "";
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
        // A session's tabs, open file and prompt come back with it. Its
        // terminal stays mounted out of sight and returns as a tab.
        const kept = next ? views[next] : null;
        if (kept) {
          tabs = kept.tabs;
          activeTabId = kept.activeTabId;
          draft = kept.draft;
          // Only parked sessions live in `views`; the active one is `tabs`.
          const { [next]: ignored, ...remaining } = views;
          views = remaining;
        } else {
          tabs = [{ id: "chat", kind: "chat", title: "Conversation" }];
          activeTabId = "chat";
          draft = "";
        }
        if (shells[next] && !tabs.some((tab) => tab.kind === "terminal")) {
          tabs = [...tabs, { id: "terminal", kind: "terminal", title: "Terminal" }];
        }
        if (!tabs.some((tab) => tab.id === activeTabId)) activeTabId = "chat";
        files = [];
        status = { branch: "", files: [] };
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

  async function command(name, args) {
    const mutatesGit = name.startsWith("git_");
    if (mutatesGit && gitBusy) { error = "Wait for the current Git operation to finish."; return false; }
    // These commands can select another workspace and replace the tab set.
    if (["create_session", "resume_session", "fork_session", "attach_worktree"].includes(name) && dirtyTabs()) {
      error = "Save or close unsaved files before changing this session.";
      return false;
    }
    if (["close_session", "archive_session", "remove_worktree"].includes(name) && dirtySession(args?.sessionId)) {
      error = "Save or close unsaved files before changing this session.";
      return false;
    }
    if (mutatesGit) gitBusy = true;
    try {
      await invoke(name, args);
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

  async function selectSession(id) {
    if (id === sessionId) return;
    if (dirtyTabs()) {
      pendingSwitch = id;
      return;
    }
    await command("select_session", { sessionId: id });
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
    const removeTerminal = (list) => list.filter((tab) => tab.kind !== "terminal");
    if (views[id]) {
      views[id] = {
        ...views[id],
        tabs: removeTerminal(views[id].tabs),
        activeTabId: views[id].activeTabId === "terminal" ? "chat" : views[id].activeTabId,
      };
    }
    if (id === sessionId) {
      tabs = removeTerminal(tabs);
      if (activeTabId === "terminal") activeTabId = "chat";
    }
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
      [id]: { path: tab.path, line: Math.max(0, filePane?.cursorLine() ?? 0) },
    };
  }

  async function send(text, images = [], targetSession = sessionId) {
    error = "";
    const attachment = fileAttachments[targetSession] ?? null;
    let ok;
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
      } catch (cause) { error = String(cause); return false; }
    }
    if (ok && attachment) {
      const { [targetSession]: _, ...rest } = fileAttachments;
      fileAttachments = rest;
    }
    return ok;
  }

  async function stop() {
    await command("stop_turn");
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
    try {
      const absolute = (clearedFrom[sessionId] ?? 0) + index;
      const full = await invoke("message_detail", { sessionId, index: absolute });
      messageDetails = { ...messageDetails, [`${sessionId}:${absolute}`]: full };
      messages = messages.map((message, at) =>
        at === index ? { ...message, text: full.text, thinking: full.thinking } : message);
    } catch (cause) {
      error = String(cause);
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
    if (draft.trim()) {
      error = "Finish or clear the composer before inserting a background result.";
      return;
    }
    draft = (task.latest_message || task.detail || "").trim();
  }

  async function editQueued(item, position) {
    if (draft.trim()) {
      error = "Finish or clear the composer before editing queued text.";
      return;
    }
    if (await command("cancel_queued", {
      sessionId,
      queueId: item.id,
      position,
      source: item.source,
    })) draft = item.text;
  }

  function restoreInterrupted(text) {
    if (draft.trim()) {
      error = "Finish or clear the composer before restoring a prompt.";
      return;
    }
    draft = text;
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
    const doomed = tabs.filter((tab) => tab.path === path || tab.path?.startsWith(`${path}/`));
    for (const tab of doomed) {
      if (tab.dirty) continue;
      removeTab(tab.id);
    }
  }

  async function stagePath(path) {
    if (await command("git_stage", { path })) refreshFiles();
  }

  async function unstagePath(path) {
    if (await command("git_unstage", { path })) refreshFiles();
  }

  function openCommit() {
    commitOpen = true;
    commitMessage = "";
    commitError = "";
    commitAll = !status.files.some((file) => file.staged);
  }

  async function submitCommit(event) {
    event.preventDefault();
    commitError = "";
    try {
      if (gitBusy) return;
      gitBusy = true;
      if (commitAll) await invoke("git_operation", { sessionId, operation: { kind: "commit_all", message: commitMessage } });
      else await invoke("git_commit", { message: commitMessage });
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
        for (const tab of tabs.filter((entry) => ["file", "diff"].includes(entry.kind) && !entry.dirty)) await reloadTab(tab.id);
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
    try {
      branches = await invoke("git_branches");
    } catch (cause) {
      branchesError = String(cause);
    } finally {
      branchesLoading = false;
    }
  }

  async function reloadBranches() {
    try {
      branches = await invoke("git_branches");
    } catch (cause) {
      branchesError = String(cause);
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
      for (const tab of tabs.filter((entry) => entry.kind !== "chat" && entry.kind !== "terminal")) {
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
      for (const tab of tabs.filter((entry) => entry.kind !== "chat" && entry.kind !== "terminal")) {
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

  async function reloadTab(id) {
    const tab = tabs.find((entry) => entry.id === id);
    if (!tab) return;
    updateTab(id, { loading: true, error: "" });
    try {
      if (tab.kind === "file" && isImage(tab.path)) {
        const image = await invoke("read_image", { path: tab.path });
        updateTab(id, { image, loading: false });
      } else if (tab.kind === "file") {
        const file = await invoke("read_file", { path: tab.path });
        updateTab(id, { file, draft: file.text, dirty: false, loading: false });
      } else {
        const diff = await invoke("git_diff", { path: tab.path, side: tab.side });
        updateTab(id, { diff, loading: false });
      }
    } catch (cause) {
      updateTab(id, { error: String(cause), loading: false });
    }
  }

  function dirtyTabs() {
    return tabs.filter((tab) => tab.dirty).length > 0;
  }

  function dirtySession(id) {
    if (!id || id === sessionId) return dirtyTabs();
    return !!views[id]?.tabs?.some((tab) => tab.dirty);
  }

  function changeDraft(value) {
    const tab = activeTab;
    if (!tab || tab.kind !== "file" || tab.file?.read_only) return;
    updateTab(tab.id, { draft: value, dirty: value !== (tab.file?.text ?? "") });
  }

  async function saveActive() {
    const tab = activeTab;
    if (!tab || tab.kind !== "file" || tab.file?.read_only || !tab.dirty || tab.saving) return;
    const text = tab.draft;
    updateTab(tab.id, { saving: true });
    try {
      const file = await invoke("write_file", {
        path: tab.path,
        text,
        fingerprint: tab.file.fingerprint,
        crlf: tab.file.crlf,
      });
      const current = tabs.find((entry) => entry.id === tab.id);
      // Keep typing that happened while the write was in flight; only the
      // saved baseline moves.
      if (current && current.draft !== text) {
        updateTab(tab.id, { file, dirty: current.draft !== file.text, error: "", saving: false });
      } else {
        updateTab(tab.id, { file, draft: file.text, dirty: false, error: "", saving: false });
      }
      refreshFiles();
    } catch (cause) {
      updateTab(tab.id, { error: String(cause), saving: false });
    }
  }

  function hunkLabels(text) {
    return text.split("\n")
      .filter((line) => line.startsWith("@@"))
      .map((line) => line.slice(2).replace(/\s+@@.*$/, "").trim());
  }

  function requestFileAction(kind, index = null) {
    const tab = activeTab;
    if (!tab || !["file", "diff"].includes(tab.kind)) return;
    if (kind === "reload" && !tab.dirty) { reloadTab(tab.id); return; }
    fileAction = { kind, id: tab.id, path: tab.path, index };
  }

  async function confirmFileAction() {
    const target = fileAction;
    fileAction = null;
    if (!target || !tabs.some((tab) => tab.id === target.id)) return;
    if (target.kind === "reload") await reloadTab(target.id);
    else if (target.kind === "hunk") await discardHunk(target);
    else await restoreTab(target.id);
  }

  async function discardHunk(target) {
    const tab = tabs.find((entry) => entry.id === target.id);
    if (!tab || gitBusy) return;
    gitBusy = true;
    error = "";
    try {
      await invoke("git_discard_hunk", { path: target.path, hunkIndex: target.index });
      updateTab(target.id, { diff: null, loading: true, error: "" });
      await loadTab(target.id);
      refreshFiles();
    } catch (cause) {
      updateTab(target.id, { error: String(cause) });
    } finally {
      gitBusy = false;
    }
  }

  async function restoreTab(id) {
    const tab = tabs.find((entry) => entry.id === id);
    if (!tab || tab.kind !== "diff") return;
    try {
      await invoke("git_restore", { path: tab.path });
      updateTab(tab.id, { diff: null, loading: true, error: "" });
      await loadTab(tab.id);
      refreshFiles();
    } catch (cause) {
      updateTab(tab.id, { error: String(cause) });
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

  function removeTab(id) {
    const index = tabs.findIndex((tab) => tab.id === id);
    tabs = tabs.filter((tab) => tab.id !== id);
    if (activeTabId === id) {
      const next = tabs[Math.max(0, index - 1)] ?? tabs[0];
      activeTabId = next.id;
    }
    // Closing the terminal tab ends this session's shell; other sessions'
    // shells keep running.
    if (id === "terminal") dropShell(sessionId);
  }

  function closeTab(id) {
    if (id === "chat") return;
    const tab = tabs.find((entry) => entry.id === id);
    if (tab?.dirty) {
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

  async function discardAndSwitch() {
    const id = pendingSwitch;
    pendingSwitch = "";
    pendingClose = "";
    // The dialog promises to discard; do not park the dirty tabs for later.
    tabs = tabs.filter((tab) => !tab.dirty);
    if (!tabs.some((tab) => tab.id === activeTabId)) activeTabId = "chat";
    await command("select_session", { sessionId: id });
  }

  async function openTab(kind, path, side = "combined") {
    const id = `${sessionId}:${kind}:${side}:${path}`;
    if (!tabs.some((tab) => tab.id === id)) {
      tabs = [
        ...tabs,
        { id, kind, path, side, title: basename(path), loading: true, error: "" },
      ];
    }
    activeTabId = id;
    await loadTab(id);
  }

  async function loadTab(id) {
    const tab = tabs.find((entry) => entry.id === id);
    if (!tab || tab.kind === "chat" || tab.file || tab.diff) return;
    updateTab(id, { loading: true, error: "" });
    try {
      if (tab.kind === "file" && isImage(tab.path)) {
        const image = await invoke("read_image", { path: tab.path });
        updateTab(id, { image, loading: false });
      } else if (tab.kind === "file") {
        const file = await invoke("read_file", { path: tab.path });
        updateTab(id, { file, loading: false, draft: file.text, dirty: false });
      } else {
        const diff = await invoke("git_diff", { path: tab.path, side: tab.side });
        updateTab(id, { diff, loading: false });
      }
    } catch (cause) {
      updateTab(id, { error: String(cause), loading: false });
    }
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
    filePane?.jumpToLine(line);
  }

  function openDiff(path, side = "combined") {
    openTab("diff", path, side);
  }

  async function openTerminal() {
    if (!terminalTab) {
      tabs = [...tabs, { id: "terminal", kind: "terminal", title: "Terminal" }];
    }
    activeTabId = "terminal";
    if (!shells[sessionId]) shells = { ...shells, [sessionId]: true };
    if (!TerminalComponent) {
      TerminalComponent = (await import("./lib/Terminal.svelte")).default;
    }
  }

  function toggleTerminal() {
    if (activeTabId === "terminal") {
      activeTabId = "chat";
    } else {
      openTerminal();
    }
  }

  function onKeydown(event) {
    if (!booted) return;
    // The terminal owns Ctrl chords (Ctrl+W deletes a word, Ctrl+K kills a
    // line, Ctrl+S is XOFF). Only Ctrl+` still toggles the terminal.
    if (event.ctrlKey && !event.metaKey && activeTabId === "terminal") {
      if (event.key === "`") {
        event.preventDefault();
        toggleTerminal();
      }
      return;
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "w") {
      if (activeTabId !== "chat") {
        event.preventDefault();
        closeTab(activeTabId);
      }
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
      if (activeTab?.kind === "file") {
        event.preventDefault();
        saveActive();
      }
    }
    if (event.ctrlKey && event.key === "`") {
      event.preventDefault();
      toggleTerminal();
    }
    if (event.metaKey && event.key.toLowerCase() === "j") {
      event.preventDefault();
      toggleTerminal();
    }
    if (event.metaKey && event.key.toLowerCase() === "m") {
      event.preventDefault();
      openModelPicker();
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
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
        needsTrust = status.needs_trust;
        trustError = status.error;
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
      unlisten?.();
    };
  });
</script>

<main class="app" data-turn-state={turnState} inert={!booted}>
  <Sidebar
    bind:this={sidebar}
    sessions={allSessions}
    activeId={sessionId}
    workspace={basename(info.workspace)}
    {branch}
    {creating}
    {files}
    {filesLoading}
    {filesError}
    changes={changeMap}
    attention={Object.keys(attention)}
    onSelect={selectSession}
    onCreate={createSession}
    onAttach={openAttach}
    onArchive={archiveSession}
    onRename={startRename}
    onPin={pinSession}
    onStop={stopSession}
    onClose={closeSession}
    onResume={(session) => command("resume_session", { sessionId: session.session_id })}
    onOpenFile={(path, line) => line ? openFileAt(path, line) : openFile(path)}
    onOpenDiff={openDiff}
    onRefreshFiles={refreshFiles}
    onCreateFile={(path) => createPath("file", path)}
    onCreateDir={(path) => createPath("dir", path)}
    onRenamePath={startRenamePath}
    onDeletePath={requestDelete}
    onSearch={(value) => invoke("search_files", { query: value })}
    onHelp={() => (helpOpen = true)}
  />

  <section class="center">
    <nav class="tabs" aria-label="Open views">
      {#each tabs as tab (tab.id)}
        <div class="tab" class:active={tab.id === activeTabId}>
          <button type="button" class="tab-main" onclick={() => (activeTabId = tab.id)}>
            <Icon name={tab.kind === "chat" ? "chat" : tab.kind === "diff" ? "diff" : "file"} size={13} />
            <span>{tab.title}</span>
          </button>
          {#if tab.kind !== "chat"}
            <button type="button" class="tab-close" aria-label="Close tab" onclick={() => closeTab(tab.id)}>
              <Icon name="close" size={11} />
            </button>
          {/if}
        </div>
      {/each}
      <span class="spacer"></span>
      <button
        type="button"
        class="icon-btn"
        title="Terminal (⌃`)"
        aria-label="Terminal"
        onclick={openTerminal}
      >
        <Icon name="terminal" size={14} />
      </button>
      {#if activeSession}
        <span class="chip">
          <span class="state-dot {activeSession.turn_state}"></span>
          {activeSession.label}
        </span>
      {/if}
    </nav>

    {#if error}
      <div class="banner" role="alert">
        <Icon name="alert" size={14} />
        <span>{error}</span>
        <button type="button" class="icon-btn" aria-label="Dismiss" onclick={() => (error = "")}>
          <Icon name="close" size={12} />
        </button>
      </div>
    {/if}

    {#if activeTab?.kind === "chat"}
      <Conversation {messages} {streaming} {turnState} onSuggest={(text) => (draft = text)} onExpand={expandMessage} onOpenImage={openFile} />
    {:else if activeTab?.kind === "terminal"}
      <!-- The terminal pane below stays mounted while another tab is shown. -->
    {:else}
      <FilePane
        bind:this={filePane}
        tab={activeTab}
        file={activeTab.file}
        diff={activeTab.diff}
        loading={activeTab.loading}
        error={activeTab.error ?? ""}
        draft={activeTab.draft ?? ""}
        dirty={activeTab.dirty ?? false}
        onDraftChange={changeDraft}
      />
    {/if}

    {#if TerminalComponent}
      {#each Object.keys(shells) as id (id)}
        <div
          class="terminal-host"
          class:hidden={!(activeTab?.kind === "terminal" && id === sessionId)}
        >
          <TerminalComponent sessionId={id} themeMode={activeTheme?.id ?? "dark"} />
        </div>
      {/each}
    {/if}

    {#if question}
      <QuestionCard {question} onAnswer={answerQuestion} />
    {/if}

    {#if approval}
      <ApprovalCard {approval} onDecide={decide} />
    {/if}

    <Composer
      bind:prompt={draft}
      {turnState}
      {sessionId}
      {imageInput}
      attachment={fileAttachments[sessionId] ?? null}
      onRemoveAttachment={() => {
        const { [sessionId]: _, ...rest } = fileAttachments;
        fileAttachments = rest;
      }}
      onSend={send}
      onStop={stop}
    />
  </section>

  <Inspector
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
    changes={status.files}
    onOpenDiff={openDiff}
    onPickModel={openModelPicker}
    onDetails={() => (detailsOpen = true)}
    onStage={stagePath}
    onUnstage={unstagePath}
    onCommit={openCommit}
    onBranches={openBranches}
    {gitBusy}
    upstream={status.upstream}
    ahead={status.ahead ?? 0}
    behind={status.behind ?? 0}
    mergeInProgress={status.merge_in_progress ?? false}
    conflicts={status.conflicts ?? []}
    onGitOperation={(operation) => operation.kind === "fetch" ? performGitOperation(operation) : requestGitOperation(operation)}
    onToggleApproveAll={toggleApproveAll}
    onToggleThinking={toggleThinking}
    onClearApprovals={() => command("clear_approvals")}
    onContinue={continueTurn}
    onCompact={compactContext}
    onStopSession={() => activeSession && stopSession(activeSession)}
    onCancelQueued={(sessionIdOf, queueId, position, source) =>
      command("cancel_queued", { sessionId: sessionIdOf, queueId, position, source })}
    onEditQueued={editQueued}
    onRestoreInterrupted={restoreInterrupted}
    onCancelBackground={(task) => command("cancel_background", { sessionId, taskId: task.id, runId: task.run_id })}
    onOpenChild={openChild}
    onInsertTask={insertTask}
    onResolveChild={(taskId, approve, callId, requestId, confirmed) =>
      command("resolve_child_approval", { sessionId, taskId, approve, callId, requestId, confirmed })}
    onFork={forkSession}
    onCloseSession={() => activeSession && closeSession(activeSession)}
    onRename={() => activeSession && startRename(activeSession)}
    onPin={() => activeSession && pinSession(activeSession)}
  />

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
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="quit-title" tabindex="-1" use:modal={{ onClose: () => (quitRequested = false) }}>
        <h2 id="quit-title">Quit Forge?</h2>
        <p>
          {#if dirtyAnywhere()}The editor has unsaved changes. {/if}
          {#if sessions.some((session) => ["running", "waiting"].includes(session.turn_state)) || Object.keys(attention).length > 0}
            Work is still running, queued or waiting for an answer in another session.
          {/if}
          Quitting stops every session; the agent's file changes on disk are kept.
        </p>
        <div class="dialog-actions">
          <button class="btn danger" type="button" onclick={() => invoke("quit_app")}>
            Quit Forge
          </button>
          <button class="btn" type="button" onclick={() => (quitRequested = false)}>Cancel</button>
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
          <h2 id="commit-title">{commitAll ? "Stage all and commit" : "Commit staged changes"}</h2>
          <p>{commitAll ? "Every tracked and untracked change in this session's workspace will be staged and committed." : "Only what is staged is committed. Unstaged files stay in the working tree."}</p>
          <label><input type="checkbox" bind:checked={commitAll} disabled={gitBusy} /> Stage all files before committing</label>
          <textarea
            class="field commit-message"
            bind:value={commitMessage}
            placeholder="What changed, and why"
            aria-label="Commit message"
          ></textarea>
          {#if commitError}<p class="dialog-error" role="alert">{commitError}</p>{/if}
          <div class="dialog-actions">
            <button class="btn primary" type="submit" disabled={!commitMessage.trim() || gitBusy}>{gitBusy ? "Committing…" : commitAll ? "Stage all and commit" : "Commit"}</button>
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
          <input class="field" bind:value={renameLabel} aria-label="Session name" />
          <div class="dialog-actions">
            <button class="btn primary" type="submit" disabled={!renameLabel.trim()}>Rename</button>
            <button class="btn" type="button" onclick={() => (renameTarget = null)}>Cancel</button>
          </div>
        </form>
      </div>
    </div>
  {/if}

  {#if pendingClose || pendingSwitch}
    <div class="modal">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="discard-title" tabindex="-1" use:modal={{ onClose: () => { pendingClose = ""; pendingSwitch = ""; } }}>
        <h2 id="discard-title">Discard unsaved changes?</h2>
        <p>
          {pendingSwitch
            ? "The open file has changes that are not saved. Switching sessions closes its tabs."
            : "This file has changes that are not saved."}
        </p>
        <div class="dialog-actions">
          <button
            class="btn danger"
            type="button"
            onclick={pendingSwitch ? discardAndSwitch : discardAndClose}
          >
            Discard
          </button>
          <button
            class="btn"
            type="button"
            onclick={() => {
              pendingClose = "";
              pendingSwitch = "";
            }}
          >
            Cancel
          </button>
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
    <Help bind:theme={themeChoice} {themes} onClose={() => (helpOpen = false)} onProviders={() => { helpOpen = false; providersOpen = true; }} onGithub={() => { helpOpen = false; githubOpen = true; }} />
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
    grid-template-columns: 258px minmax(0, 1fr) 254px;
    height: 100vh;
    min-height: 0;
    background: var(--bg);
  }

  .center {
    display: flex;
    flex-direction: column;
    min-width: 0;
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

  .tabs {
    display: flex;
    align-items: center;
    gap: 2px;
    height: 38px;
    padding: 0 10px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
    overflow: hidden;
  }

  .tab {
    display: flex;
    align-items: center;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--muted);
  }

  .tab:hover {
    background: var(--hover);
  }

  .tab.active {
    background: var(--surface);
    color: var(--text);
  }

  .tab-main {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 8px;
    border: 0;
    background: none;
    color: inherit;
    font-size: 12px;
    white-space: nowrap;
  }

  .tab-close {
    display: inline-grid;
    place-items: center;
    width: 20px;
    height: 20px;
    margin-right: 3px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--muted);
  }

  .tab-close:hover {
    background: var(--border-strong);
    color: var(--text);
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

  .dialog-error {
    margin: 0 0 10px;
    color: var(--error);
    font-size: 12px;
  }

  .danger-line {
    color: var(--error);
  }

  @media (max-width: 1120px) {
    .app {
      grid-template-columns: 240px minmax(0, 1fr);
    }

    .app :global(.inspector) {
      display: none;
    }
  }

  @media (max-width: 760px) {
    .app {
      grid-template-columns: 200px minmax(0, 1fr);
    }
  }
</style>
