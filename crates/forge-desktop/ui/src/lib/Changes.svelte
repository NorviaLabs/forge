<script>
  // The session's changes beside its conversation: what the last turn touched
  // and everything uncommitted, with a diff preview that never leaves the
  // session. Editing still happens on the Files screen.
  import { invoke } from "../ipc.js";
  import { highlight } from "../editor.js";
  import Icon from "./Icon.svelte";

  let {
    sessionId,
    status = { branch: "", files: [] },
    lastTurnFiles = [],
    supportsGit = true,
    loading = false,
    error = "",
    onOpenDiff,
    onOpenFiles,
  } = $props();

  const PREVIEW_LINES = 400;
  const STATUS_WORDS = { M: "modified", A: "added", D: "deleted", "?": "new", U: "conflict", R: "renamed" };

  let tab = $state("turn");
  let selected = $state("");
  let preview = $state({ path: "", side: "", lines: [], more: 0, loading: false, error: "" });

  const byPath = $derived(new Map(status.files.map((file) => [file.path, file])));
  const turnRows = $derived(
    lastTurnFiles.map((path) => byPath.get(path) ?? { path, status: "", staged: false, unstaged: false }),
  );
  // With nothing from the last turn, the tab that has content leads.
  const activeTab = $derived(tab === "turn" && turnRows.length === 0 && status.files.length > 0 ? "all" : tab);
  const rows = $derived(activeTab === "turn" ? turnRows : status.files);
  const side = $derived(activeTab === "turn" ? "last_turn" : "combined");

  function name(path) {
    return path.slice(path.lastIndexOf("/") + 1);
  }

  function folder(path) {
    const cut = path.lastIndexOf("/");
    return cut < 0 ? "" : path.slice(0, cut);
  }

  function statusWord(file) {
    return STATUS_WORDS[file.status] ?? (activeTab === "turn" ? "changed" : "modified");
  }

  // A diff line as its sign, its indentation in columns, and its code, so a
  // wrapped line continues at its own indent.
  function parts(line) {
    if (lineClass(line) === "hunk") return { sign: "", indent: 0, code: line };
    const sign = line.startsWith("+") || line.startsWith("-") ? line[0] : "";
    const body = line.slice(1);
    const lead = body.match(/^[ \t]*/)[0];
    const indent = [...lead].reduce((width, char) => width + (char === "\t" ? 4 : 1), 0);
    return { sign, indent, code: body.slice(lead.length) };
  }

  function lineClass(line) {
    if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") || line.startsWith("index ")) return "meta";
    if (line.startsWith("@@")) return "hunk";
    if (line.startsWith("+")) return "add";
    if (line.startsWith("-")) return "del";
    return "";
  }

  // The preview follows the selection. A reply for a row the operator has
  // left (another file, tab or session) is dropped.
  let request = 0;
  async function show(path, diffSide) {
    const ticket = ++request;
    selected = path;
    preview = { path, side: diffSide, lines: [], more: 0, loading: true, error: "" };
    try {
      const diff = await invoke("git_diff", { sessionId, path, side: diffSide });
      if (ticket !== request) return;
      // The header names the file already; the patch's own headers repeat it.
      const lines = (diff.text ?? "").replace(/\n$/, "").split("\n").filter((line) => lineClass(line) !== "meta");
      const added = lines.filter((line) => lineClass(line) === "add").length;
      const removed = lines.filter((line) => lineClass(line) === "del").length;
      preview = {
        path,
        side: diffSide,
        lines: lines.slice(0, PREVIEW_LINES),
        more: Math.max(0, lines.length - PREVIEW_LINES) + (diff.truncated ? 1 : 0),
        added,
        removed,
        loading: false,
        error: "",
      };
    } catch (cause) {
      if (ticket !== request) return;
      preview = { path, side: diffSide, lines: [], more: 0, loading: false, error: String(cause) };
    }
  }

  // Keep a file selected while there is one: the first row when the
  // selection is gone, and a fresh diff when the list changes under it.
  let shownKey = "";
  $effect(() => {
    const paths = rows.map((row) => row.path);
    const key = `${sessionId}|${side}|${paths.join("\n")}`;
    if (key === shownKey) return;
    shownKey = key;
    if (!sessionId || !supportsGit || paths.length === 0) {
      request += 1;
      selected = "";
      preview = { path: "", side: "", lines: [], more: 0, loading: false, error: "" };
      return;
    }
    show(paths.includes(selected) ? selected : paths[0], side);
  });

  function onListKeydown(event) {
    if (!["ArrowDown", "ArrowUp"].includes(event.key)) return;
    const paths = rows.map((row) => row.path);
    const at = paths.indexOf(selected);
    const next = paths[Math.min(paths.length - 1, Math.max(0, at + (event.key === "ArrowDown" ? 1 : -1)))];
    if (!next || next === selected) return;
    event.preventDefault();
    show(next, side);
    event.currentTarget.querySelector(`[data-path="${CSS.escape(next)}"]`)?.focus();
  }
</script>

<aside class="changes" aria-label="Changes">
  <header class="changes-head">
    <h2>Changes</h2>
    {#if supportsGit && status.files.length > 0}
      <span class="total">{status.files.length} {status.files.length === 1 ? "file" : "files"}</span>
    {/if}
  </header>

  {#if !supportsGit}
    <div class="empty">
      <strong>No Git repository here</strong>
      <p>Changes are tracked through Git. This folder is a plain directory, so there is nothing to compare.</p>
      <button type="button" class="btn browse" disabled={!sessionId} onclick={onOpenFiles}>Browse all files</button>
    </div>
  {:else}
    <div class="tabs" role="tablist" aria-label="Which changes">
      <button type="button" role="tab" id="changes-tab-turn" aria-selected={activeTab === "turn"} aria-controls="changes-list" onclick={() => (tab = "turn")}>
        This turn <span class="tab-count">{turnRows.length}</span>
      </button>
      <button type="button" role="tab" id="changes-tab-all" aria-selected={activeTab === "all"} aria-controls="changes-list" onclick={() => (tab = "all")}>
        All changes <span class="tab-count">{status.files.length}</span>
      </button>
    </div>

    {#if error}
      <p class="problem" role="alert">{error}</p>
    {/if}

    {#if rows.length === 0}
      <div class="empty">
        {#if loading}
          <strong>Reading the worktree…</strong>
        {:else if activeTab === "turn"}
          <strong>The last turn changed no files</strong>
          <p>When the agent edits files, they are listed here with their diffs.</p>
        {:else}
          <strong>The worktree is clean</strong>
          <p>Nothing is uncommitted on {status.branch || "this branch"}.</p>
        {/if}
        {#if !loading}
          <button type="button" class="btn browse" disabled={!sessionId} onclick={onOpenFiles}>Browse all files</button>
        {/if}
      </div>
    {:else}
      <ul id="changes-list" class="file-list" role="tabpanel" aria-labelledby={`changes-tab-${activeTab}`} onkeydown={onListKeydown}>
        {#each rows as file (file.path)}
          <li>
            <button
              type="button"
              class="file-row"
              class:current={file.path === selected}
              aria-current={file.path === selected ? "true" : undefined}
              data-path={file.path}
              title={file.path}
              onclick={() => show(file.path, side)}
              ondblclick={() => onOpenDiff?.(file.path, side)}
            >
              <span class="file-name">{name(file.path)}</span>
              <span class="file-dir">{folder(file.path)}</span>
              <span class="file-status" data-status={file.status}>{statusWord(file)}</span>
            </button>
          </li>
        {/each}
      </ul>

      <section class="preview" aria-label={preview.path ? `Diff of ${preview.path}` : "Diff"}>
        <header class="preview-head">
          <span class="preview-path mono" title={preview.path}>{preview.path}</span>
          {#if !preview.loading && !preview.error && (preview.added || preview.removed)}
            <span class="stat mono" aria-label={`${preview.added ?? 0} lines added, ${preview.removed ?? 0} removed`}>+{preview.added ?? 0} −{preview.removed ?? 0}</span>
          {/if}
          <button type="button" class="icon-btn" title="Open the full diff in Files" aria-label="Open the full diff in Files" disabled={!preview.path} onclick={() => onOpenDiff?.(preview.path, preview.side)}><Icon name="expand" size={13} /></button>
        </header>
        {#if preview.loading}
          <p class="note" role="status">Loading the diff…</p>
        {:else if preview.error}
          <p class="problem" role="alert">{preview.error}</p>
        {:else if preview.lines.join("").trim() === ""}
          <p class="note">No textual diff for this file (it may be binary, or unchanged since the turn).</p>
        {:else}
          <pre class="diff" tabindex="0">{#each preview.lines as line, index (index)}{@const part = parts(line)}<span class="line {lineClass(line)}"><span class="sign">{part.sign || " "}</span><span class="code" style:padding-left="{part.indent}ch">{#if part.sign !== "" || lineClass(line) === ""}{#each highlight(part.code, preview.path) as token, at (at)}<span class={token.kind}>{token.text}</span>{/each}{:else}{part.code}{/if}{part.code ? "" : " "}</span></span>{/each}</pre>
          {#if preview.more > 0}
            <p class="note">The preview stops at {PREVIEW_LINES} lines. <button type="button" class="link" onclick={() => onOpenDiff?.(preview.path, preview.side)}>Open the full diff</button></p>
          {/if}
        {/if}
      </section>
    {/if}
  {/if}

</aside>

<style>
  .changes {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    border-left: 1px solid var(--border);
    background: var(--surface);
  }

  .changes-head {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 8px 0 16px;
  }

  h2 {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 600;
  }

  .total,
  .tab-count {
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .spacer {
    flex: 1;
  }

  .tabs {
    display: flex;
    gap: 16px;
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
  }

  .tabs button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 32px;
    padding: 0;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: none;
    color: var(--muted);
    font-size: var(--text-sm);
    transition: color var(--fast) var(--ease), border-color var(--fast) var(--ease);
  }

  .tabs button:hover {
    color: var(--text);
  }

  .tabs button[aria-selected="true"] {
    border-bottom-color: var(--accent);
    color: var(--text);
    font-weight: 500;
  }

  .file-list {
    flex: 0 1 auto;
    max-height: 34%;
    min-height: 0;
    margin: 0;
    padding: 8px;
    overflow: auto;
    list-style: none;
  }

  .file-row {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    height: 28px;
    padding: 0 8px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    font-size: var(--text-sm);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .file-row:hover {
    background: var(--hover);
  }

  .file-row.current {
    border-color: var(--border);
    background: var(--raised);
  }

  .file-row:focus-visible {
    outline-offset: -2px;
  }

  .file-name {
    font-family: var(--font-mono);
    font-weight: 500;
    white-space: nowrap;
  }

  .file-dir {
    overflow: hidden;
    color: var(--muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file-status {
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .file-status[data-status="A"],
  .file-status[data-status="?"] {
    color: var(--success);
  }

  .file-status[data-status="D"],
  .file-status[data-status="U"] {
    color: var(--error);
  }

  .preview {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-height: 0;
    border-top: 1px solid var(--border);
    background: var(--bg);
  }

  .preview-head {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 32px;
    flex: none;
    padding: 0 8px 0 16px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }

  .preview-path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .stat {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .preview-head .icon-btn {
    flex: none;
    width: 24px;
    height: 24px;
  }

  .diff {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 8px 0;
    overflow: auto;
    font-size: var(--text-sm);
    line-height: 20px;
    tab-size: 4;
  }

  .diff:focus-visible {
    outline-offset: -2px;
  }

  /* The sign has its own column; the code wraps at its own indent. */
  .line {
    display: grid;
    grid-template-columns: 16px minmax(0, 1fr);
    padding: 0 16px;
  }

  .sign {
    color: var(--muted);
    user-select: none;
  }

  /* The same lexical colours as the editor. */
  .code .keyword {
    color: var(--accent);
  }

  .code .string {
    color: var(--success);
  }

  .code .number {
    color: var(--warning);
  }

  .code .comment {
    color: var(--muted);
  }

  .code {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .line.add {
    background: var(--add);
  }

  .line.del {
    background: var(--remove);
  }

  .line.hunk {
    margin: 4px 0;
    background: var(--surface);
    color: var(--muted);
  }

  .line.meta {
    color: var(--muted);
  }

  .empty {
    display: flex;
    flex: 1;
    flex-direction: column;
    justify-content: center;
    gap: 4px;
    padding: 24px;
  }

  .browse {
    align-self: flex-start;
    margin-top: 12px;
  }

  .empty strong {
    font-weight: 600;
  }

  .empty p,
  .note {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 18px;
  }

  .note {
    padding: 12px 16px;
  }

  .problem {
    margin: 8px 16px;
    color: var(--error);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }

  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    font-size: inherit;
    text-decoration: underline;
    text-underline-offset: 3px;
  }

  @media (prefers-reduced-motion: reduce) {
    .tabs button,
    .file-row {
      transition: none;
    }
  }
</style>
