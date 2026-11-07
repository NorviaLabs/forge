<script>
  import Icon from "./Icon.svelte";

  let {
    sessions = [],
    activeId = "",
    workspace = "",
    branch = "",
    creating = false,
    files = [],
    filesLoading = false,
    filesError = "",
    changes = new Map(),
    attention = [],
    onSelect,
    onCreate,
    onAttach,
    onArchive,
    onRename,
    onPin,
    onStop,
    onClose,
    onResume,
    onOpenFile,
    onOpenDiff,
    onRefreshFiles,
    onCreateFile,
    onCreateDir,
    onRenamePath,
    onDeletePath,
    onSearch,
    onHelp,
  } = $props();

  let newLabel = $state("");
  let composing = $state(false);
  let filter = $state("");
  let fileDraft = $state(null);
  let query = $state("");
  let hits = $state([]);
  let searching = $state(false);

  const live = $derived(sessions.filter((session) => session.lifecycle === "active"));
  const closed = $derived(sessions.filter((session) => session.lifecycle !== "active"));

  const broken = $derived(
    files.filter((entry) => {
      if (!filter.trim()) return true;
      return entry.path.toLowerCase().includes(filter.trim().toLowerCase());
    }),
  );

  // Exposed to the command palette.
  export function startNewSession() {
    composing = true;
    newLabel = "";
  }

  function submitNew(event) {
    event.preventDefault();
    const label = newLabel.trim();
    if (!label || creating) return;
    onCreate(label);
    newLabel = "";
    composing = false;
  }

  function startDraft(kind) {
    fileDraft = { kind, value: "" };
  }

  function submitDraft(event) {
    event.preventDefault();
    const draft = fileDraft;
    if (!draft || !draft.value.trim()) return;
    fileDraft = null;
    if (draft.kind === "file") onCreateFile(draft.value.trim());
    else onCreateDir(draft.value.trim());
  }

  // Content search is debounced; the tree filter above stays instant.
  let searchTimer;
  $effect(() => {
    const value = query;
    const owner = activeId;
    clearTimeout(searchTimer);
    if (!value.trim() || !onSearch) {
      hits = [];
      searching = false;
      return;
    }
    searching = true;
    searchTimer = setTimeout(async () => {
      try {
        const found = await onSearch(value);
        if (query === value && activeId === owner) hits = found;
      } catch {
        if (query === value && activeId === owner) hits = [];
      } finally {
        if (query === value && activeId === owner) searching = false;
      }
    }, 300);
    return () => clearTimeout(searchTimer);
  });
</script>

<aside class="sidebar" aria-label="Sessions and files">
  <header class="workspace">
    <span class="mark">F</span>
    <div class="names">
      <div class="title">
        <strong>Forge</strong>
        <button class="help" type="button" onclick={onHelp}>Help</button>
      </div>
      <span class="where" title={workspace}>{workspace || "No session"}</span>
    </div>
    {#if branch}
      <span class="chip branch" title="Current branch">
        <Icon name="branch" size={12} />
        {branch}
      </span>
    {/if}
  </header>

  <section class="panel sessions">
    <div class="section-head">
      <span class="label">Sessions</span>
      <span class="head-actions">
        <button
          class="icon-btn"
          type="button"
          title="Attach an existing worktree"
          aria-label="Attach an existing worktree"
          onclick={onAttach}
        >
          <Icon name="layers" size={14} />
        </button>
        <button
          class="icon-btn"
          type="button"
          title="New session"
          aria-label="New session"
          onclick={() => (composing = !composing)}
        >
          <Icon name="plus" size={14} />
        </button>
      </span>
    </div>

    {#if composing}
      <form class="new" onsubmit={submitNew}>
        <input
          class="field"
          bind:value={newLabel}
          placeholder="Session name"
          aria-label="New session name"
        />
        <button class="btn primary" type="submit" disabled={creating || !newLabel.trim()}>
          {creating ? "Creating…" : "Create"}
        </button>
      </form>
    {/if}

    <ul class="session-list">
      {#if creating}
        <li class="creating">
          <span class="mini spin"><Icon name="refresh" size={12} /></span>
          <span>Creating session…</span>
        </li>
      {/if}
      {#each live as session (session.session_id)}
        <li
          class:active={session.session_id === activeId}
          class:needs-you={attention.includes(session.session_id)}
        >
          <button
            type="button"
            class="session"
            onclick={() => onSelect(session.session_id)}
            data-session-id={session.session_id}
          >
            <span class="state-dot {session.turn_state}"></span>
            {#if session.slot}<span class="slot" title="Pinned">{session.slot}</span>{/if}
            <span class="session-name">{session.label}</span>
            <span class="session-state">
              {attention.includes(session.session_id) ? "needs you" : session.turn_state}
            </span>
          </button>
          <span class="row-actions">
            {#if ["running", "waiting"].includes(session.turn_state)}
              <button
                type="button"
                class="icon-btn"
                title="Stop this session's turn"
                aria-label={`Stop ${session.label}`}
                onclick={() => onStop(session)}
              >
                <Icon name="stop" size={12} />
              </button>
            {/if}
            <button
              type="button"
              class="icon-btn"
              title={session.slot ? "Unpin session" : "Pin session"}
              aria-label={session.slot ? `Unpin ${session.label}` : `Pin ${session.label}`}
              onclick={() => onPin(session)}
            >
              <Icon name="pin" size={12} />
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Rename session"
              aria-label={`Rename ${session.label}`}
              onclick={() => onRename(session)}
            >
              <Icon name="pencil" size={12} />
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Stop managing this session (it stays resumable)"
              aria-label={`Close ${session.label}`}
              onclick={() => onClose(session)}
            >
              <Icon name="close" size={12} />
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Archive session"
              aria-label={`Archive ${session.label}`}
              onclick={() => onArchive(session)}
            >
              <Icon name="trash" size={12} />
            </button>
          </span>
        </li>
      {/each}
      {#if live.length === 0 && !creating}
        <li class="empty">No sessions yet.</li>
      {/if}
    </ul>

    {#if closed.length > 0}
      <details class="closed">
        <summary>{closed.length} closed session{closed.length === 1 ? "" : "s"}</summary>
        <ul class="session-list">
          {#each closed as session (session.session_id)}
            <li>
              <button
                type="button"
                class="session closed-row"
                title="Resume this session"
                onclick={() => onResume(session)}
              >
                <Icon name="play" size={11} />
                <span class="session-name">{session.label}</span>
                <span class="session-state">{session.lifecycle}</span>
              </button>
              {#if session.ownership === "managed" && ["archived", "retained"].includes(session.lifecycle)}
                <button
                  type="button"
                  class="icon-btn"
                  title="Archive and remove this session's worktree"
                  aria-label={`Remove the worktree for ${session.label}`}
                  onclick={() => onArchive(session)}
                >
                  <Icon name="trash" size={12} />
                </button>
              {/if}
            </li>
          {/each}
        </ul>
      </details>
    {/if}
  </section>

  <section class="panel files">
    <div class="section-head">
      <span class="label">Files</span>
      <span class="head-actions">
        {#if filesLoading}<span class="mini spin"><Icon name="refresh" size={12} /></span>{/if}
        <button
          class="icon-btn"
          type="button"
          title="New file"
          aria-label="New file"
          onclick={() => startDraft("file")}
        >
          <Icon name="plus" size={13} />
        </button>
        <button
          class="icon-btn"
          type="button"
          title="New folder"
          aria-label="New folder"
          onclick={() => startDraft("dir")}
        >
          <Icon name="files" size={13} />
        </button>
        <button
          class="icon-btn"
          type="button"
          title="Refresh files"
          aria-label="Refresh files"
          onclick={onRefreshFiles}
        >
          <Icon name="refresh" size={13} />
        </button>
      </span>
    </div>

    {#if fileDraft}
      <form class="new" onsubmit={submitDraft}>
        <input
          class="field"
          bind:value={fileDraft.value}
          placeholder={fileDraft.kind === "file" ? "path/new-file.ts" : "path/new-folder"}
          aria-label={fileDraft.kind === "file" ? "New file path" : "New folder path"}
          spellcheck="false"
        />
        <button class="btn primary" type="submit" disabled={!fileDraft.value.trim()}>Add</button>
      </form>
    {/if}

    <div class="filter">
      <Icon name="search" size={12} />
      <input
        bind:value={filter}
        placeholder="Filter files"
        aria-label="Filter files"
        spellcheck="false"
      />
    </div>
    <div class="filter">
      <Icon name="search" size={12} />
      <input
        bind:value={query}
        placeholder="Search contents"
        aria-label="Search file contents"
        spellcheck="false"
      />
      {#if searching}<span class="mini spin"><Icon name="refresh" size={11} /></span>{/if}
    </div>

    {#if query.trim()}
      <ul class="hits">
        {#each hits as hit (`${hit.path}:${hit.line}`)}
          <li>
            <button
              type="button"
              class="hit"
              title={`${hit.path}:${hit.line}`}
              onclick={() => onOpenFile(hit.path, hit.line)}
            >
              <span class="hit-path mono">{hit.path}:{hit.line}</span>
              <span class="hit-text mono">{hit.text.trim()}</span>
            </button>
          </li>
        {/each}
        {#if hits.length === 0 && !searching}
          <li class="empty">No matches.</li>
        {/if}
      </ul>
    {:else}
    {#if filesError}
      <p class="panel-error">{filesError}</p>
    {:else}
      <ul class="tree">
        {#each broken as entry (entry.path)}
          <li class:dir={entry.is_dir} style="--depth: {entry.depth}">
            {#if entry.is_dir}
              <span class="tree-row dir-row" title={entry.path}>
                <Icon name="files" size={12} />
                <span class="tree-name">{entry.name}</span>
              </span>
            {:else}
              <button
                type="button"
                class="tree-row"
                title={entry.path}
                onclick={() => onOpenFile(entry.path)}
              >
                <Icon name="file" size={12} />
                <span class="tree-name">{entry.name}</span>
              </button>
              {#if changes.has(entry.path)}
                <button
                  type="button"
                  class="change {changes.get(entry.path).toLowerCase()}"
                  title={`Show diff for ${entry.path}`}
                  aria-label={`Show diff for ${entry.path}`}
                  onclick={() => onOpenDiff(entry.path)}
                >
                  {changes.get(entry.path)}
                </button>
              {/if}
              <span class="row-actions">
                <button
                  type="button"
                  class="icon-btn"
                  title="Rename"
                  aria-label={`Rename ${entry.path}`}
                  onclick={() => onRenamePath(entry.path)}
                >
                  <Icon name="pencil" size={11} />
                </button>
                <button
                  type="button"
                  class="icon-btn"
                  title="Delete"
                  aria-label={`Delete ${entry.path}`}
                  onclick={() => onDeletePath(entry)}
                >
                  <Icon name="trash" size={11} />
                </button>
              </span>
            {/if}
          </li>
        {/each}
        {#if files.length === 0 && !filesLoading}
          <li class="empty">No files.</li>
        {/if}
      </ul>
    {/if}
    {/if}
  </section>
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg-deep);
    border-right: 1px solid var(--border);
  }

  .workspace {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 12px 12px 10px;
    border-bottom: 1px solid var(--border);
  }

  .mark {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 7px;
    background: var(--accent);
    color: #0e1115;
    font-weight: 700;
  }

  .names {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.25;
  }

  .title {
    display: flex;
    align-items: baseline;
    gap: 7px;
    min-width: 0;
  }

  .names strong {
    font-size: 13px;
    letter-spacing: 0.01em;
  }

  .help {
    padding: 0;
    border: 0;
    background: none;
    color: var(--muted);
    font-size: 11px;
  }

  .help:hover {
    color: var(--text);
    text-decoration: underline;
  }

  .where {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: 11px;
  }

  .branch {
    margin-left: auto;
    max-width: 45%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .panel {
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 10px 8px 6px;
  }

  .sessions {
    max-height: 42%;
    border-bottom: 1px solid var(--border);
  }

  .files {
    flex: 1;
  }

  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 4px 6px;
  }

  .head-actions {
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .label {
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .mini {
    display: inline-flex;
    color: var(--muted);
  }

  .new {
    display: flex;
    gap: 6px;
    padding: 0 4px 8px;
  }

  .new .field {
    flex: 1;
    min-width: 0;
  }

  .session-list,
  .tree {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: auto;
    min-height: 0;
  }

  .session-list li {
    position: relative;
    display: flex;
    align-items: center;
    border-radius: var(--radius-sm);
  }

  .session-list li.active {
    background: var(--selection);
  }

  .session {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    height: 32px;
    padding: 0 8px;
    border: 0;
    background: none;
    text-align: left;
  }

  .session:hover {
    background: var(--hover);
    border-radius: var(--radius-sm);
  }

  .session-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .session-state {
    color: var(--muted);
    font-size: 10px;
    letter-spacing: 0.03em;
  }

  .row-actions {
    display: flex;
    align-items: center;
    gap: 1px;
    margin-right: 3px;
    opacity: 0;
    pointer-events: none;
  }

  li:hover .row-actions,
  li:focus-within .row-actions {
    opacity: 1;
    pointer-events: auto;
  }

  /* Session actions float over the row so they do not squeeze the name when
     hidden; the gradient keeps the icons readable on hover. */
  .session-list .row-actions {
    position: absolute;
    top: 50%;
    right: 3px;
    translate: 0 -50%;
    display: flex;
    align-items: center;
    gap: 1px;
    padding-left: 14px;
    background: linear-gradient(90deg, transparent, var(--hover) 14px);
    opacity: 0;
  }

  .session-list li.active .row-actions {
    background: linear-gradient(90deg, transparent, var(--selection) 14px);
  }

  .session-list li:hover .row-actions,
  .session-list li:focus-within .row-actions {
    opacity: 1;
  }

  li.needs-you .session-state {
    color: var(--warning);
    font-weight: 600;
  }

  .closed {
    margin: 4px 4px 0;
  }

  .closed summary {
    padding: 3px 4px;
    color: var(--muted);
    font-size: 11px;
    cursor: pointer;
  }

  .closed-row {
    gap: 6px;
    color: var(--muted);
    font-size: 12px;
  }

  .slot {
    display: inline-grid;
    place-items: center;
    min-width: 14px;
    height: 14px;
    border-radius: 4px;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: 9.5px;
    font-weight: 700;
  }

  .creating {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 32px;
    padding: 0 8px;
    color: var(--muted);
    font-size: 12px;
  }

  .filter {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 4px 6px;
    padding: 0 8px;
    height: 26px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--muted);
  }

  .filter input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    outline: none;
    font-size: 12px;
  }

  .tree {
    padding: 0 2px 6px;
  }

  .tree li {
    display: flex;
    align-items: center;
    list-style: none;
  }

  .tree-row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1;
    min-width: 0;
    height: 23px;
    padding: 0 6px 0 calc(6px + var(--depth) * 12px);
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--secondary);
    text-align: left;
    font-family: var(--font-mono);
    font-size: 11.5px;
  }

  button.tree-row:hover {
    background: var(--hover);
    color: var(--text);
  }

  .dir-row {
    color: var(--muted);
    cursor: default;
  }

  .tree-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .change {
    display: inline-grid;
    place-items: center;
    min-width: 16px;
    height: 16px;
    margin-right: 4px;
    padding: 0 3px;
    border: 0;
    border-radius: 4px;
    background: var(--surface);
    color: var(--warning);
    font-size: 10px;
    font-weight: 700;
    font-family: var(--font-ui);
  }

  .hits {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin: 0;
    padding: 0 2px;
    overflow: auto;
    list-style: none;
  }

  .hit {
    display: flex;
    flex-direction: column;
    gap: 1px;
    width: 100%;
    padding: 3px 6px;
    border: 0;
    border-radius: 4px;
    background: none;
    text-align: left;
  }

  .hit:hover {
    background: var(--hover);
  }

  .hit-path {
    color: var(--accent);
    font-size: 10.5px;
  }

  .hit-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--secondary);
    font-size: 11px;
  }

  .change.d {
    color: var(--error);
  }

  .change.\? {
    color: var(--success);
  }

  .empty,
  .panel-error {
    padding: 6px 8px;
    color: var(--muted);
    font-size: 12px;
  }

  .panel-error {
    color: var(--error);
  }
</style>
