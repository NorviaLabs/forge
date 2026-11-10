<script>
  import Icon from "./Icon.svelte";
  import FilePane from "./FilePane.svelte";
  import { diffSide, effectiveScope, fileCounts, scopeEntries } from "../view.js";

  // The Files screen's markup. App owns every command; this component only
  // calls the callbacks it is given.
  let {
    files = [],
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
  let collapsed = $state([]);

  const counts = $derived(fileCounts(status));
  const shown = $derived(supportsGit ? effectiveScope(scope, lastTurnFiles) : "all");
  const NO_GIT = "Needs a Git repository";
  const changeByPath = $derived(new Map(status.files.map((file) => [file.path, file])));
  const activeDoc = $derived(tabs.find((tab) => tab.id === activeDocId) ?? null);
  const needle = $derived(query.trim().toLowerCase());

  const tree = $derived.by(() => {
    const matching = needle ? files.filter((entry) => entry.path.toLowerCase().includes(needle)) : files;
    // While filtering, show every match so a hit inside a collapsed folder still appears.
    if (needle) return matching;
    return matching.filter((entry) => !collapsed.some((dir) => entry.path.startsWith(`${dir}/`)));
  });

  const scoped = $derived(
    scopeEntries(shown, { status, lastTurnFiles }).filter((entry) => !needle || entry.path.toLowerCase().includes(needle)),
  );

  const scopes = $derived(supportsGit ? [
    { value: "all", label: "All files" },
    { value: "changed", label: counts.changed ? `Changed (${counts.changed})` : "Changed" },
    { value: "staged", label: counts.staged ? `Staged (${counts.staged})` : "Staged" },
    ...(lastTurnFiles.length ? [{ value: "lastTurn", label: "Last turn" }] : []),
  ] : [{ value: "all", label: "All files" }]);

  const emptyScope = $derived(
    needle ? "No files match." : shown === "staged" ? "Nothing is staged." : shown === "changed" ? "Clean working tree." : "Nothing in this scope.",
  );

  export function jumpToLine(line) {
    pane?.jumpToLine(line);
  }

  export function showNavigation(kind) {
    pane?.showNavigation(kind);
  }

  export function cursorLine() {
    return pane?.cursorLine() ?? 0;
  }

  function toggleDir(path) {
    collapsed = collapsed.includes(path) ? collapsed.filter((entry) => entry !== path) : [...collapsed, path];
  }

  function submitDraft(event) {
    event.preventDefault();
    const draft = fileDraft;
    if (!draft || !draft.value.trim()) return;
    fileDraft = null;
    if (draft.kind === "file") onCreateFile(draft.value.trim());
    else onCreateDir(draft.value.trim());
  }

  function docLabel(tab) {
    if (tab.kind !== "diff") return tab.title;
    const side = { staged: "Staged", unstaged: "Unstaged", last_turn: "Last turn" }[tab.side];
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

<div class="files">
  <div class="git-bar">
    {#if supportsGit}
    <button type="button" class="chip branch" title={onBranches ? "Branches…" : "Branches need a Git repository"} disabled={!branch || !onBranches} onclick={onBranches}>
      <Icon name="branch" size={12} />
      <span>{branch || "No branch"}</span>
    </button>
    <span class="upstream mono" title={upstream || "No upstream configured"}>
      {upstream || "No upstream"}{#if ahead} · ↑{ahead}{/if}{#if behind} · ↓{behind}{/if}
    </span>
    {:else}
    <button type="button" class="chip branch" title="This folder isn't a Git repository: agent edits can't be rolled back" disabled>
      <Icon name="branch" size={12} />
      <span>No Git</span>
    </button>
    {/if}
    <span class="spacer"></span>
    {#if mergeInProgress}
      <span class="merge">Merge in progress · {conflicts.length} unresolved</span>
      <button type="button" class="btn" disabled={gitBusy} onclick={() => onGitOperation({ kind: "abort_merge" })}>Abort merge…</button>
    {/if}
    <button type="button" class="btn" title={supportsGit ? undefined : NO_GIT} disabled={gitBusy || !supportsGit} onclick={() => onGitOperation({ kind: "fetch" })}>Fetch</button>
    <button type="button" class="btn" title={supportsGit ? undefined : NO_GIT} disabled={gitBusy || !supportsGit} onclick={() => onGitOperation({ kind: "pull" })}>Pull…</button>
    <button type="button" class="btn" title={supportsGit ? undefined : NO_GIT} disabled={gitBusy || !upstream || !supportsGit} onclick={() => onGitOperation({ kind: "push" })}>Push…</button>
  </div>

  <div class="body">
    <div class="file-list">
      <div class="list-head">
        <div class="scope-chips" role="group" aria-label="File scope">
          {#each scopes as option (option.value)}
            <button type="button" class="btn" aria-pressed={shown === option.value} onclick={() => (scope = option.value)}>
              {option.label}
            </button>
          {/each}
        </div>
        <span class="head-actions">
          {#if filesLoading}<span class="mini spin"><Icon name="refresh" size={12} /></span>{/if}
          {#if counts.unstaged > 0}
            <button class="btn" type="button" title="Stage every unstaged change" disabled={gitBusy} onclick={onStageAll}>Stage all</button>
          {/if}
          <button class="icon-btn" type="button" title="New file" aria-label="New file" onclick={() => (fileDraft = { kind: "file", value: "" })}>
            <Icon name="plus" size={13} />
          </button>
          <button class="icon-btn" type="button" title="New folder" aria-label="New folder" onclick={() => (fileDraft = { kind: "dir", value: "" })}>
            <Icon name="files" size={13} />
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

      <div class="filter">
        <Icon name="search" size={12} />
        <input bind:value={query} placeholder="Filter files" aria-label="Filter files" spellcheck="false" />
      </div>
      {#if shown === "all"}
        <div class="filter">
          <Icon name="search" size={12} />
          <input bind:value={contents} placeholder="Search contents" aria-label="Search file contents" spellcheck="false" />
          {#if searching}<span class="mini spin"><Icon name="refresh" size={11} /></span>{/if}
        </div>
      {/if}

      {#if shown === "all" && contents.trim()}
        <ul class="hits">
          {#each hits as hit (`${hit.path}:${hit.line}`)}
            <li>
              <button type="button" class="hit" title={`${hit.path}:${hit.line}`} onclick={() => onOpenFile(hit.path, hit.line)}>
                <span class="hit-path mono">{hit.path}:{hit.line}</span>
                <span class="hit-text mono">{hit.text.trim()}</span>
              </button>
            </li>
          {/each}
          {#if hits.length === 0 && !searching}
            <li class="empty">{searchError || "No matches."}</li>
          {/if}
        </ul>
      {:else if shown === "all"}
        {#if filesError}
          <p class="panel-error">{filesError}</p>
        {:else}
          <ul class="tree" aria-label="Files">
            {#each tree as entry (entry.path)}
              <li class="file-row" style="--depth: {entry.depth}">
                {#if entry.is_dir}
                  <button type="button" class="tree-row dir-row" title={entry.path} aria-expanded={!collapsed.includes(entry.path)} onclick={() => toggleDir(entry.path)}>
                    <span class="caret" class:open={!collapsed.includes(entry.path)}><Icon name="chevron" size={10} /></span>
                    <Icon name="files" size={12} />
                    <span class="tree-name">{entry.name}</span>
                  </button>
                {:else}
                  <button type="button" class="tree-row" class:selected={activeDoc?.path === entry.path} title={entry.path} onclick={() => onOpenFile(entry.path)}>
                    <Icon name="file" size={12} />
                    <span class="tree-name">{entry.name}</span>
                    {#if changeByPath.has(entry.path)}
                      <span class="badge {changeByPath.get(entry.path).status.toLowerCase()}">{changeByPath.get(entry.path).status}</span>
                    {/if}
                  </button>
                  <span class="row-actions">
                    {@render gitActions(entry.path)}
                    <button type="button" class="icon-btn" title="Rename" aria-label={`Rename ${entry.path}`} onclick={() => onRenamePath(entry.path)}>
                      <Icon name="pencil" size={11} />
                    </button>
                    <button type="button" class="icon-btn" title="Delete" aria-label={`Delete ${entry.path}`} onclick={() => onDeletePath(entry)}>
                      <Icon name="trash" size={11} />
                    </button>
                  </span>
                {/if}
              </li>
            {/each}
            {#if files.length === 0 && !filesLoading}
              <li class="empty">No files.</li>
            {:else if tree.length === 0}
              <li class="empty">No files match.</li>
            {/if}
          </ul>
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
                {#if entry.status}<span class="badge {entry.status.toLowerCase()}">{entry.status}</span>{/if}
                <span class="tree-name">{entry.path}</span>
              </button>
              <span class="row-actions always">{@render gitActions(entry.path)}</span>
            </li>
          {/each}
          {#if scoped.length === 0}
            <li class="empty">{emptyScope}</li>
          {/if}
        </ul>
      {/if}

      {#if counts.staged > 0}
        <div class="commit-bar">
          <span>{counts.staged} staged</span>
          <button class="btn primary" type="button" disabled={gitBusy} onclick={onCommit}>Commit…</button>
        </div>
      {/if}
    </div>

    <div class="editor-pane">
      {#if tabs.length > 0}
        <div class="doc-tabs" role="tablist" aria-label="Open documents">
          {#each tabs as tab (tab.id)}
            <div class="tab" class:active={tab.id === activeDocId}>
              <button type="button" class="tab-main" role="tab" aria-selected={tab.id === activeDocId} onclick={() => onActivate(tab.id)}>
                <Icon name={tab.kind === "diff" ? "diff" : "file"} size={13} />
                <span>{docLabel(tab)}</span>
                {#if tab.dirty}<span class="dot" aria-label="Unsaved"></span>{/if}
              </button>
              <button type="button" class="tab-close" aria-label={`Close ${docLabel(tab)}`} onclick={() => onClose(tab.id)}>
                <Icon name="close" size={11} />
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
        <p class="placeholder">Open a file or a diff from the list.</p>
      {/if}
    </div>
  </div>
</div>

<style>
  .files { display: flex; flex-direction: column; flex: 1; min-height: 0; }
  .git-bar {
    display: flex; align-items: center; gap: 8px; flex-wrap: wrap;
    padding: 6px 12px; border-bottom: 1px solid var(--border); font-size: 12px;
  }
  .branch { display: inline-flex; align-items: center; gap: 5px; max-width: 220px; border: 1px solid var(--border); background: none; }
  .branch span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .upstream { color: var(--muted); font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .merge { color: var(--warning); }
  .spacer { flex: 1; }
  .body { display: flex; flex: 1; min-height: 0; }
  .file-list {
    display: flex; flex-direction: column; gap: 4px;
    width: 280px; flex: 0 0 280px; min-height: 0; padding: 10px 8px 8px;
    border-right: 1px solid var(--border); background: var(--bg-deep);
  }
  .list-head { display: flex; flex-direction: column; gap: 6px; padding: 0 2px; }
  .scope-chips { display: flex; gap: 3px; flex-wrap: wrap; }
  .scope-chips .btn { font-size: 11px; padding: 3px 7px; }
  .scope-chips .btn[aria-pressed="true"] { background: var(--selection); color: var(--text); border-color: var(--border-strong); }
  .head-actions { display: flex; align-items: center; gap: 2px; justify-content: flex-end; }
  .head-actions .btn { margin-right: auto; font-size: 11px; padding: 3px 7px; }
  .mini { display: inline-flex; color: var(--muted); }
  .new { display: flex; gap: 6px; padding: 0 2px 4px; }
  .new .field { flex: 1; min-width: 0; }
  .filter {
    display: flex; align-items: center; gap: 6px; margin: 0 2px; padding: 0 8px; height: 26px;
    border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--bg); color: var(--muted);
  }
  .filter input { flex: 1; min-width: 0; border: 0; background: none; font-size: 12px; }
  .tree, .hits { list-style: none; margin: 0; padding: 0 0 6px; overflow: auto; min-height: 0; flex: 1; }
  .file-row { display: flex; align-items: center; }
  .tree-row {
    display: flex; align-items: center; gap: 6px; flex: 1; min-width: 0; height: 24px;
    padding: 0 6px 0 calc(6px + var(--depth, 0) * 12px); border: 0; border-radius: 4px;
    background: none; color: var(--secondary); text-align: left; font-family: var(--font-mono); font-size: 11.5px;
  }
  button.tree-row:hover { background: var(--hover); color: var(--text); }
  .tree-row.selected { background: var(--selection); color: var(--text); }
  .dir-row { color: var(--muted); }
  .caret { display: inline-flex; color: var(--muted); transition: transform 120ms ease; }
  .caret.open { transform: rotate(90deg); }
  .tree-name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .flat .tree-name { direction: rtl; text-align: left; }
  .badge {
    display: inline-grid; place-items: center; min-width: 16px; height: 16px; padding: 0 3px;
    border-radius: 4px; background: var(--surface); color: var(--warning);
    font-size: 10px; font-weight: 700; font-family: var(--font-ui);
  }
  .badge.d { color: var(--error); }
  .badge.\?, .badge.a { color: var(--success); }
  .row-actions { display: flex; align-items: center; gap: 1px; margin-right: 2px; visibility: hidden; }
  .row-actions.always, .file-row:hover .row-actions, .file-row:focus-within .row-actions { visibility: visible; }
  .row-actions .icon-btn { width: 22px; height: 22px; }
  .hit { display: flex; flex-direction: column; gap: 1px; width: 100%; padding: 3px 6px; border: 0; border-radius: 4px; background: none; text-align: left; }
  .hit:hover { background: var(--hover); }
  .hit-path { color: var(--accent); font-size: 10.5px; }
  .hit-text { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--secondary); font-size: 11px; }
  .commit-bar {
    display: flex; align-items: center; justify-content: space-between; gap: 8px;
    margin-top: auto; padding: 8px 4px 0; border-top: 1px solid var(--border); color: var(--muted); font-size: 12px;
  }
  .empty, .panel-error { padding: 6px 8px; color: var(--muted); font-size: 12px; }
  .panel-error { color: var(--error); }
  .editor-pane { display: flex; flex-direction: column; flex: 1; min-width: 0; min-height: 0; }
  .doc-tabs {
    display: flex; align-items: center; gap: 2px; height: 36px; padding: 0 10px; flex: 0 0 auto;
    border-bottom: 1px solid var(--border); overflow-x: auto; overflow-y: hidden; scrollbar-width: thin;
  }
  .tab { display: flex; align-items: center; height: 26px; border-radius: var(--radius-sm); color: var(--muted); }
  .tab:hover { background: var(--hover); }
  .tab.active { background: var(--surface); color: var(--text); }
  .tab-main { display: flex; align-items: center; gap: 6px; height: 26px; padding: 0 8px; border: 0; background: none; color: inherit; font-size: 12px; white-space: nowrap; }
  .tab-close { display: inline-grid; place-items: center; width: 20px; height: 20px; margin-right: 3px; padding: 0; border: 0; border-radius: 4px; background: none; color: var(--muted); }
  .tab-close:hover { background: var(--border-strong); color: var(--text); }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); }
  .placeholder { margin: auto; color: var(--muted); font-size: 13px; }
  @media (max-width: 760px) {
    .body { flex-direction: column; }
    .file-list { width: auto; flex: 0 0 auto; max-height: 40%; border-right: 0; border-bottom: 1px solid var(--border); }
  }
</style>
