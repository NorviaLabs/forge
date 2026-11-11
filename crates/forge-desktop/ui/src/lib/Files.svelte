<script>
  import Icon from "./Icon.svelte";
  import FilePane from "./FilePane.svelte";
  import { diffSide, effectiveScope, fileCounts, scopeEntries, windowRange } from "../view.js";

  // The Files screen's markup. App owns every command; this component only
  // calls the callbacks it is given.
  let {
    // Visible rows of the lazily loaded tree (`flattenTree` in view.js).
    tree = [],
    showIgnored = $bindable(false),
    filesLoading = false,
    filesError = "",
    status = { branch: "", files: [] },
    lastTurnFiles = [],
    scope = $bindable("all"),
    query = $bindable(""),
    tabs = [],
    activeDocId = null,
    branch = "",
    upstream = "",
    ahead = 0,
    behind = 0,
    gitBusy = false,
    // False for a folder that isn't a Git repository: only "All files", and
    // the Git actions stay visible but disabled with the reason.
    supportsGit = true,
    mergeInProgress = false,
    conflicts = [],
    onOpenFile,
    onOpenDiff,
    onRefreshFiles,
    onCreateFile,
    onCreateDir,
    onRenamePath,
    onDeletePath,
    onSearch,
    // `onFindFiles(query)`: files by fuzzy name, from the workspace index.
    onFindFiles,
    onToggleDir,
    onDraftChange,
    onActivate,
    onClose,
    onStage,
    onUnstage,
    onStageAll,
    onCommit,
    onBranches,
    onGitOperation,
  } = $props();

  let pane = $state();
  let fileDraft = $state(null);
  let contents = $state("");
  let hits = $state([]);
  let searching = $state(false);
  let searchError = $state("");
  // Files whose name matches the filter, from the index rather than the
  // loaded folders, so a file inside a folder never opened is still found.
  let named = $state([]);
  let naming = $state(false);
  // The tree renders only the rows on screen: a folder can hold thousands.
  const ROW = 28;
  let scrollTop = $state(0);
  let viewport = $state(600);
  const visible = $derived(windowRange(tree.length, scrollTop, viewport, ROW));

  const counts = $derived(fileCounts(status));
  const shown = $derived(supportsGit ? effectiveScope(scope, lastTurnFiles) : "all");
  const NO_GIT = "Needs a Git repository";
  const changeByPath = $derived(new Map(status.files.map((file) => [file.path, file])));
  const activeDoc = $derived(tabs.find((tab) => tab.id === activeDocId) ?? null);
  const needle = $derived(query.trim().toLowerCase());

  const scoped = $derived(
    scopeEntries(shown, { status, lastTurnFiles }).filter((entry) => !needle || entry.path.toLowerCase().includes(needle)),
  );

  const scopes = $derived(supportsGit ? [
    { value: "all", label: "All files" },
    { value: "changed", label: "Changed", count: counts.changed },
    { value: "staged", label: "Staged", count: counts.staged },
    ...(lastTurnFiles.length ? [{ value: "lastTurn", label: "Last turn" }] : []),
  ] : [{ value: "all", label: "All files" }]);

  const emptyScope = $derived(
    needle
      ? "No files match. Try a shorter filter."
      : shown === "staged"
        ? "Nothing is staged. Stage a change to commit it."
        : shown === "changed"
          ? "Clean working tree. Edited files are listed here."
          : "Nothing in this scope.",
  );

  const STATUS_WORDS = { M: "modified", A: "added", D: "deleted", "?": "new", U: "conflict", R: "renamed" };

  // Content hits arrive one per line; the list shows them under their file.
  const hitGroups = $derived.by(() => {
    const groups = new Map();
    for (const hit of hits) {
      if (!groups.has(hit.path)) groups.set(hit.path, []);
      groups.get(hit.path).push(hit);
    }
    return [...groups].map(([path, matches]) => ({ path, matches }));
  });

  // [before, match, after] around the first place the searched text occurs
  // in a hit's line; no match part when it cannot be located.
  function matchParts(text) {
    const lower = text.toLowerCase();
    for (const term of [contents, contents.trim()]) {
      const at = term && lower.length === text.length ? lower.indexOf(term.toLowerCase()) : -1;
      if (at >= 0) return [text.slice(0, at), text.slice(at, at + term.length), text.slice(at + term.length)];
    }
    return [text, "", ""];
  }

  export function jumpToLine(line) {
    pane?.jumpToLine(line);
  }

  export function showNavigation(kind) {
    pane?.showNavigation(kind);
  }

  export function cursorLine() {
    return pane?.cursorLine() ?? 0;
  }

  let nameTimer;
  $effect(() => {
    const value = query.trim();
    clearTimeout(nameTimer);
    if (!value || shown !== "all" || !onFindFiles) {
      named = [];
      naming = false;
      return;
    }
    naming = true;
    nameTimer = setTimeout(async () => {
      try {
        const found = await onFindFiles(value);
        if (query.trim() === value) named = found;
      } catch {
        if (query.trim() === value) named = [];
      } finally {
        if (query.trim() === value) naming = false;
      }
    }, 120);
    return () => clearTimeout(nameTimer);
  });

  function submitDraft(event) {
    event.preventDefault();
    const draft = fileDraft;
    if (!draft || !draft.value.trim()) return;
    fileDraft = null;
    if (draft.kind === "file") onCreateFile(draft.value.trim());
    else onCreateDir(draft.value.trim());
  }

  function docSide(tab) {
    if (tab.kind !== "diff") return "";
    return { staged: "Staged", unstaged: "Unstaged", last_turn: "Last turn" }[tab.side] ?? "";
  }

  function docLabel(tab) {
    const side = docSide(tab);
    return side ? `${side}: ${tab.title}` : tab.title;
  }

  // Content search is debounced; the tree filter stays instant.
  let searchTimer;
  $effect(() => {
    const value = contents;
    clearTimeout(searchTimer);
    if (!value.trim() || !onSearch) {
      hits = [];
      searching = false;
      searchError = "";
      return;
    }
    searching = true;
    searchError = "";
    searchTimer = setTimeout(async () => {
      try {
        const found = await onSearch(value);
        if (contents === value) hits = found;
      } catch (cause) {
        if (contents === value) {
          hits = [];
          searchError = String(cause);
        }
      } finally {
        if (contents === value) searching = false;
      }
    }, 300);
    return () => clearTimeout(searchTimer);
  });
</script>

{#snippet gitActions(path)}
  {@const change = changeByPath.get(path)}
  {#if change}
    <button type="button" class="icon-btn" title="Open diff" aria-label={`Open diff for ${path}`} onclick={() => onOpenDiff(path, diffSide(shown))}>
      <Icon name="diff" size={12} />
    </button>
    {#if change.unstaged}
      <button type="button" class="icon-btn" title="Stage" aria-label={`Stage ${path}`} disabled={gitBusy} onclick={() => onStage(path)}>
        <Icon name="plus" size={12} />
      </button>
    {/if}
    {#if change.staged}
      <button type="button" class="icon-btn" title="Unstage" aria-label={`Unstage ${path}`} disabled={gitBusy} onclick={() => onUnstage(path)}>
        <Icon name="arrow-left" size={12} />
      </button>
    {/if}
  {:else if shown === "lastTurn"}
    <button type="button" class="icon-btn" title="Open last-turn diff" aria-label={`Open last-turn diff for ${path}`} onclick={() => onOpenDiff(path, "last_turn")}>
      <Icon name="diff" size={12} />
    </button>
  {/if}
{/snippet}

{#snippet statusBadge(path)}
  {@const change = changeByPath.get(path)}
  {#if change?.status}
    <span class="badge {change.status.toLowerCase()}">{STATUS_WORDS[change.status] ?? change.status}</span>
  {/if}
{/snippet}

<div class="files">
  <div class="body">
    <div class="file-list">
      <div class="scope-chips" role="group" aria-label="File scope">
        {#each scopes as option (option.value)}
          <button type="button" class="btn scope-tab" aria-pressed={shown === option.value} onclick={() => (scope = option.value)}>
            {option.label}{#if option.count}<span class="tab-count">{option.count}</span>{/if}
          </button>
        {/each}
      </div>

      <div class="filters">
        <div class="filter">
          <Icon name="search" size={12} />
          <input class="field" bind:value={query} placeholder="Filter files" aria-label="Filter files" spellcheck="false" />
        </div>
        {#if shown === "all"}
          <div class="filter">
            <Icon name="search" size={12} />
            <input class="field" bind:value={contents} placeholder="Search contents" aria-label="Search file contents" spellcheck="false" />
            {#if searching}<span class="mini spin"><Icon name="refresh" size={12} /></span>{/if}
          </div>
        {/if}
      </div>

      <div class="list-head">
        <span class="list-label">
          {#if filesLoading}<span class="mini spin"><Icon name="refresh" size={12} /></span>Loading files…{:else}Files{/if}
        </span>
        <span class="head-actions">
          <button class="icon-btn" type="button" title="New file" aria-label="New file" onclick={() => (fileDraft = { kind: "file", value: "" })}>
            <Icon name="plus" size={13} />
          </button>
          <button class="icon-btn" type="button" title="New folder" aria-label="New folder" onclick={() => (fileDraft = { kind: "dir", value: "" })}>
            <Icon name="files" size={13} />
          </button>
          <button
            class="icon-btn"
            type="button"
            title={showIgnored ? "Hide ignored files" : "Show ignored files (dependencies, build output)"}
            aria-label="Show ignored files"
            aria-pressed={showIgnored}
            onclick={() => (showIgnored = !showIgnored)}
          >
            <Icon name="layers" size={13} />
          </button>
          <button class="icon-btn" type="button" title="Refresh files" aria-label="Refresh files" onclick={onRefreshFiles}>
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

      {#if shown === "all" && contents.trim()}
        <ul class="hits">
          {#each hitGroups as group (group.path)}
            <li class="hit-group">
              <span class="hit-path mono" title={group.path}>{group.path}</span>
              <ul class="hit-list">
                {#each group.matches as hit (`${hit.path}:${hit.line}`)}
                  {@const parts = matchParts(hit.text.trim())}
                  <li>
                    <button type="button" class="hit" title={`${hit.path}:${hit.line}`} onclick={() => onOpenFile(hit.path, hit.line)}>
                      <span class="hit-line mono">{hit.line}</span>
                      <span class="hit-text mono">{parts[0]}{#if parts[1]}<mark class="hit-match">{parts[1]}</mark>{/if}{parts[2]}</span>
                    </button>
                  </li>
                {/each}
              </ul>
            </li>
          {/each}
          {#if hits.length === 0}
            {#if searching}
              <li class="empty">Searching…</li>
            {:else}
              <li class="empty" class:problem={!!searchError}>{searchError || "No matches. Try a different search."}</li>
            {/if}
          {/if}
        </ul>
      {:else if shown === "all"}
        {#if filesError}
          <p class="panel-error">{filesError}</p>
        {:else}
          {#if needle}
            <ul class="hits" aria-label="Files matching the filter">
              {#each named as file (file.path)}
                <li class="file-row flat">
                  <button type="button" class="tree-row" class:selected={activeDoc?.path === file.path} title={file.path} onclick={() => onOpenFile(file.path)}>
                    <Icon name="file" size={12} />
                    <span class="tree-name">{file.path}</span>
                    {@render statusBadge(file.path)}
                  </button>
                </li>
              {/each}
              {#if named.length === 0}
                <li class="empty">{naming ? "Finding files…" : "No files match. Try a shorter filter."}</li>
              {/if}
            </ul>
          {:else}
          <ul
            class="tree"
            aria-label="Files"
            bind:clientHeight={viewport}
            onscroll={(event) => (scrollTop = event.currentTarget.scrollTop)}
          >
            <li class="spacer" aria-hidden="true" style="height: {visible.start * ROW}px"></li>
            {#each tree.slice(visible.start, visible.end) as entry (entry.path)}
              <li class="file-row" class:ignored={entry.ignored} style="--depth: {entry.depth}">
                {#if entry.kind === "dir"}
                  <button type="button" class="tree-row dir-row" title={entry.path} aria-expanded={entry.open} onclick={() => onToggleDir(entry.path)}>
                    <span class="caret" class:open={entry.open}><Icon name="chevron" size={12} /></span>
                    <span class="tree-name">{entry.name}</span>
                  </button>
                {:else if entry.kind === "loading"}
                  <span class="tree-row note"><span class="mini spin"><Icon name="refresh" size={12} /></span>Loading…</span>
                {:else if entry.kind === "more"}
                  <span class="tree-row note">More entries than can be listed; use the filter to find one.</span>
                {:else}
                  <button type="button" class="tree-row" class:selected={activeDoc?.path === entry.path} title={entry.path} onclick={() => onOpenFile(entry.path)}>
                    <Icon name="file" size={12} />
                    <span class="tree-name">{entry.name}</span>
                    {@render statusBadge(entry.path)}
                  </button>
                  <span class="row-actions">
                    {@render gitActions(entry.path)}
                    <button type="button" class="icon-btn" title="Rename" aria-label={`Rename ${entry.path}`} onclick={() => onRenamePath(entry.path)}>
                      <Icon name="pencil" size={12} />
                    </button>
                    <button type="button" class="icon-btn" title="Delete" aria-label={`Delete ${entry.path}`} onclick={() => onDeletePath(entry)}>
                      <Icon name="trash" size={12} />
                    </button>
                  </span>
                {/if}
              </li>
            {/each}
            <li class="spacer" aria-hidden="true" style="height: {(tree.length - visible.end) * ROW}px"></li>
            {#if tree.length === 0 && !filesLoading}
              <li class="empty">No files here yet. Add one with New file.</li>
            {/if}
          </ul>
          {/if}
        {/if}
      {:else}
        <ul class="tree" aria-label="Files">
          {#each scoped as entry (entry.path)}
            <li class="file-row flat">
              <button
                type="button"
                class="tree-row"
                class:selected={activeDoc?.path === entry.path}
                title={entry.path}
                onclick={() => onOpenDiff(entry.path, diffSide(shown))}
              >
                <span class="tree-name">{entry.path}</span>
                {#if entry.status}<span class="badge {entry.status.toLowerCase()}">{STATUS_WORDS[entry.status] ?? entry.status}</span>{/if}
              </button>
              <span class="row-actions always">{@render gitActions(entry.path)}</span>
            </li>
          {/each}
          {#if scoped.length === 0}
            <li class="empty">{emptyScope}</li>
          {/if}
        </ul>
      {/if}

      <div class="git-bar">
        {#if mergeInProgress}
          <div class="merge">
            <span class="state-dot {conflicts.length > 0 ? 'failed' : 'waiting'}"></span>
            <span class="merge-text">Merge in progress · {conflicts.length} unresolved</span>
            <button type="button" class="btn" disabled={gitBusy} onclick={() => onGitOperation({ kind: "abort_merge" })}>Abort merge…</button>
          </div>
        {/if}
        {#if counts.staged > 0 || counts.unstaged > 0}
          <div class="commit-bar">
            <span>{counts.staged} staged</span>
            {#if counts.unstaged > 0}
              <button class="btn" type="button" title="Stage every unstaged change" disabled={gitBusy} onclick={onStageAll}>Stage all</button>
            {/if}
            {#if counts.staged > 0}
              <button class="btn primary" type="button" disabled={gitBusy} onclick={onCommit}>Commit…</button>
            {/if}
          </div>
        {/if}
        <div class="git-row">
          {#if supportsGit}
          <button type="button" class="chip branch" title={onBranches ? "Branches…" : "Branches need a Git repository"} disabled={!branch || !onBranches} onclick={onBranches}>
            <Icon name="branch" size={12} />
            <span class="mono">{branch || "No branch"}</span>
          </button>
          <span class="upstream mono" title={upstream || "No upstream configured"}>
            {upstream || "No upstream"}{#if ahead}<span class="track" aria-label="{ahead} ahead"><Icon name="arrow-up" size={11} />{ahead}</span>{/if}{#if behind}<span class="track" aria-label="{behind} behind"><Icon name="arrow-down" size={11} />{behind}</span>{/if}
          </span>
          {:else}
          <button type="button" class="chip branch" title="This folder isn't a Git repository: agent edits can't be rolled back" disabled>
            <Icon name="branch" size={12} />
            <span>No Git</span>
          </button>
          {/if}
        </div>
        <div class="git-row sync">
          <button type="button" class="btn" title={supportsGit ? undefined : NO_GIT} disabled={gitBusy || !supportsGit} onclick={() => onGitOperation({ kind: "fetch" })}>Fetch</button>
          <button type="button" class="btn" title={supportsGit ? undefined : NO_GIT} disabled={gitBusy || !supportsGit} onclick={() => onGitOperation({ kind: "pull" })}>Pull…</button>
          <button type="button" class="btn" title={supportsGit ? undefined : NO_GIT} disabled={gitBusy || !upstream || !supportsGit} onclick={() => onGitOperation({ kind: "push" })}>Push…</button>
        </div>
      </div>
    </div>

    <div class="editor-pane">
      {#if tabs.length > 0}
        <div class="doc-tabs" class:single={tabs.length === 1} role="tablist" aria-label="Open documents">
          {#each tabs as tab (tab.id)}
            <div class="tab" class:active={tab.id === activeDocId}>
              <button type="button" class="tab-main" role="tab" aria-selected={tab.id === activeDocId} onclick={() => onActivate(tab.id)}>
                <Icon name={tab.kind === "diff" ? "diff" : "file"} size={12} />
                <span>{#if docSide(tab)}{docSide(tab)}: {/if}<span class="mono">{tab.title}</span></span>
                {#if tab.dirty}<span class="dot" aria-label="Unsaved"></span>{/if}
              </button>
              <button type="button" class="tab-close icon-btn" aria-label={`Close ${docLabel(tab)}`} onclick={() => onClose(tab.id)}>
                <Icon name="close" size={12} />
              </button>
            </div>
          {/each}
        </div>
      {/if}
      {#if activeDoc}
        <FilePane
          bind:this={pane}
          tab={activeDoc}
          file={activeDoc.file}
          diff={activeDoc.diff}
          loading={activeDoc.loading}
          error={activeDoc.error ?? ""}
          draft={activeDoc.draft ?? ""}
          dirty={activeDoc.dirty ?? false}
          {onDraftChange}
        />
      {:else}
        <div class="placeholder">
          <strong>No document open</strong>
          <p>Open a file or a diff from the list.</p>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .files {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }

  .body {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  /* Navigator */
  .file-list {
    display: flex;
    flex-direction: column;
    width: 320px;
    flex: 0 0 320px;
    min-height: 0;
    border-right: 1px solid var(--border);
    background: var(--surface);
  }

  .scope-chips {
    display: flex;
    gap: 12px;
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
    overflow: hidden;
  }

  /* Scope buttons keep their group and pressed semantics; they read as the
     underline tabs of the changes column. */
  .scope-chips .scope-tab {
    gap: 4px;
    height: 32px;
    padding: 0;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: none;
    color: var(--muted);
    font-weight: 400;
    transition: color var(--fast) var(--ease), border-color var(--fast) var(--ease);
  }

  .scope-chips .scope-tab:hover:not(:disabled) {
    border-bottom-color: transparent;
    background: none;
    color: var(--text);
  }

  .scope-chips .scope-tab[aria-pressed="true"],
  .scope-chips .scope-tab[aria-pressed="true"]:hover:not(:disabled) {
    border-bottom-color: var(--accent);
    color: var(--text);
    font-weight: 500;
  }

  .scope-chips .scope-tab:focus-visible {
    outline-offset: -2px;
  }

  .tab-count {
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
  }

  .filters {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: none;
    padding: 12px 12px 4px;
  }

  .filter {
    position: relative;
    display: flex;
    align-items: center;
    color: var(--muted);
  }

  .filter > :global(svg) {
    position: absolute;
    left: 8px;
    pointer-events: none;
  }

  .filter .field {
    padding: 0 24px;
    font-size: var(--text-sm);
  }

  .filter .mini {
    position: absolute;
    right: 8px;
  }

  .mini {
    display: inline-flex;
    color: var(--muted);
  }

  .list-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    flex: none;
    height: var(--control-lg);
    padding: 0 8px 0 16px;
  }

  .list-label {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .head-actions {
    display: flex;
    align-items: center;
  }

  .head-actions .icon-btn[aria-pressed="true"] {
    background: var(--raised);
    color: var(--text);
  }

  .new {
    display: flex;
    gap: 8px;
    flex: none;
    padding: 0 12px 8px;
  }

  .new .field {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
  }

  /* Tree and lists */
  .tree,
  .hits {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 0 8px 8px;
    overflow: auto;
    list-style: none;
  }

  .file-row {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 28px;
  }

  .tree .spacer {
    display: block;
  }

  .tree-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    height: 28px;
    padding: 0 8px 0 calc(8px + var(--depth, 0) * 16px);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--text);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .tree-row :global(svg) {
    flex: none;
    color: var(--muted);
  }

  button.tree-row:hover {
    background: var(--hover);
  }

  .tree-row.selected,
  button.tree-row.selected:hover {
    border-color: var(--border);
    background: var(--raised);
  }

  button.tree-row:focus-visible {
    outline-offset: -2px;
  }

  .file-row.ignored .tree-row {
    color: var(--muted);
  }

  .tree-row.note {
    color: var(--muted);
    font-family: var(--font-ui);
  }

  .caret {
    display: inline-flex;
    flex: none;
    color: var(--muted);
  }

  /* The one chevron glyph points right; an open folder turns it down. */
  .caret.open {
    transform: rotate(90deg);
  }

  .tree-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Whole paths keep their file name in view when they are cut short. */
  .flat .tree-name {
    direction: rtl;
    text-align: left;
  }

  .badge {
    flex: none;
    color: var(--muted);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
  }

  .badge.a,
  .badge.\? {
    color: var(--success);
  }

  .badge.d,
  .badge.u {
    color: var(--error);
  }

  .row-actions {
    display: none;
    align-items: center;
    flex: none;
  }

  .row-actions.always,
  .file-row:hover .row-actions,
  .file-row:focus-within .row-actions {
    display: flex;
  }

  .row-actions .icon-btn {
    width: 24px;
    height: 24px;
  }

  /* Content search results */
  .hit-group + .hit-group {
    margin-top: 8px;
  }

  .hit-path {
    display: block;
    padding: 4px 8px;
    overflow: hidden;
    color: var(--text);
    font-weight: 500;
    line-height: 16px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .hit-list {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .hit {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    height: 24px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .hit:hover {
    background: var(--hover);
  }

  .hit:focus-visible {
    outline-offset: -2px;
  }

  .hit-line {
    flex: none;
    width: 32px;
    color: var(--muted);
    text-align: right;
  }

  .hit-text {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .hit-match {
    background: var(--selection);
    color: var(--text);
  }

  .empty,
  .panel-error {
    margin: 0;
    padding: 8px;
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 16px;
    overflow-wrap: anywhere;
  }

  .panel-error {
    padding: 8px 16px;
  }

  .panel-error,
  .empty.problem {
    color: var(--error);
  }

  /* Git area, pinned under the list */
  .git-bar {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: none;
    margin-top: auto;
    padding: 12px;
    border-top: 1px solid var(--border);
    font-size: var(--text-sm);
  }

  .merge {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: var(--control-lg);
    padding: 0 4px 0 8px;
    border: 1px solid color-mix(in srgb, var(--warning) 45%, var(--border));
    border-radius: var(--radius-sm);
  }

  .merge-text {
    flex: 1;
    min-width: 0;
    color: var(--text);
  }

  .merge .btn {
    height: 24px;
    padding: 0 8px;
  }

  .commit-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--muted);
  }

  .commit-bar span {
    flex: 1;
    min-width: 0;
  }

  .git-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .git-row.sync .btn {
    flex: 1;
  }

  .branch {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 60%;
    color: var(--text);
    transition: background var(--fast) var(--ease);
  }

  .branch:hover:not(:disabled) {
    background: var(--hover);
  }

  .branch:disabled {
    color: var(--muted);
  }

  .branch :global(svg) {
    flex: none;
    color: var(--muted);
  }

  .branch span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .upstream {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--muted);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Document area */
  .editor-pane {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    min-height: 0;
    background: var(--bg);
  }

  .doc-tabs {
    display: flex;
    flex: none;
    height: 36px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
  }

  /* One document needs no switcher: the top bar names it and closes it. */
  .doc-tabs.single {
    display: none;
  }

  .doc-tabs::-webkit-scrollbar {
    display: none;
  }

  .tab {
    display: flex;
    align-items: center;
    flex: none;
    padding-right: 8px;
    border-right: 1px solid var(--border);
    color: var(--muted);
    transition: background var(--fast) var(--ease), color var(--fast) var(--ease);
  }

  .tab:hover {
    color: var(--text);
  }

  .tab.active {
    background: var(--bg);
    color: var(--text);
  }

  .tab-main {
    display: flex;
    align-items: center;
    gap: 8px;
    align-self: stretch;
    padding: 0 8px 0 12px;
    border: 0;
    background: none;
    color: inherit;
    font-size: var(--text-sm);
    white-space: nowrap;
  }

  .tab-main:focus-visible,
  .tab-close:focus-visible {
    outline-offset: -2px;
  }

  .tab-close {
    width: 20px;
    height: 20px;
  }

  .dot {
    width: 8px;
    height: 8px;
    flex: none;
    border-radius: 50%;
    background: var(--accent);
  }

  .placeholder {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: auto;
    padding: 24px;
    text-align: center;
  }

  .placeholder strong {
    font-size: var(--text-md);
    font-weight: 600;
  }

  .placeholder p {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  @media (max-width: 760px) {
    .body {
      flex-direction: column;
    }

    .file-list {
      width: auto;
      flex: 0 0 auto;
      max-height: 50%;
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .scope-chips .scope-tab,
    .tree-row,
    .hit,
    .branch,
    .tab {
      transition: none;
    }
  }

  .track {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    margin-left: 8px;
  }
</style>
