<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let {
    branches = [],
    current = "",
    error = "",
    loading = false,
    busy = false,
    onClose,
    onSwitch,
    onMerge,
    onCreate,
    onRename,
    onDelete,
  } = $props();

  let newName = $state("");
  let renameName = $state("");
  let filter = $state("");

  const shown = $derived(
    branches.filter((branch) => branch.toLowerCase().includes(filter.trim().toLowerCase())),
  );

  // Start the rename field from the branch that is current, and follow it if
  // a rename lands while the dialog is open.
  $effect(() => {
    renameName = current;
  });
</script>

<div class="modal">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="branches-title" tabindex="-1" use:modal={{ onClose, busy }}>
    <header>
      <h2 id="branches-title">Branches</h2>
      <button class="icon-btn" type="button" aria-label="Close" onclick={onClose}>
        <Icon name="close" size={13} />
      </button>
    </header>

    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if loading}
      <p class="dim">Reading branches…</p>
    {:else}
      <input class="field" bind:value={filter} placeholder="Filter branches" aria-label="Filter branches" spellcheck="false" />
      <ul class="list">
        {#each shown as branch (branch)}
          <li>
            <span class="name mono" class:current={branch === current} title={branch}>{branch}</span>
            {#if branch === current}
              <span class="tag">current</span>
            {:else}
              <button class="btn" type="button" disabled={busy} onclick={() => onSwitch(branch)}>Switch</button>
              <button class="btn ghost" type="button" disabled={busy} onclick={() => onMerge(branch)}>Merge in</button>
              <button class="btn ghost" type="button" disabled={busy} onclick={() => onDelete(branch)}>Delete…</button>
            {/if}
          </li>
        {/each}
        {#if shown.length === 0}
          <li class="dim">{branches.length === 0 ? "No branches yet. Create one below." : "No branches match. Clear the filter to see them all."}</li>
        {/if}
      </ul>
    {/if}

    {#if current}
      <div class="create">
        <input class="field" bind:value={renameName} aria-label="Rename current branch" spellcheck="false" />
        <button
          class="btn"
          type="button"
          disabled={busy || !renameName.trim() || renameName.trim() === current}
          onclick={() => onRename(current, renameName.trim())}
        >
          Rename current
        </button>
      </div>
    {/if}

    <div class="create">
      <input class="field" bind:value={newName} placeholder="New branch name" aria-label="New branch name" spellcheck="false" />
      <button
        class="btn primary"
        type="button"
        disabled={busy || !newName.trim()}
        onclick={() => {
          onCreate(newName.trim());
          newName = "";
        }}
      >
        Create
      </button>
    </div>

    <p class="dim small">
      Switching and merging run in the session's workspace. Uncommitted changes can stop a
      switch; git's reason is shown above.
    </p>
  </div>
</div>

<style>
  .modal {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    z-index: 20;
  }

  .dialog {
    display: flex;
    flex-direction: column;
    gap: 12px;
    width: min(480px, calc(100vw - 48px));
    max-height: 80vh;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex: none;
  }

  h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .field {
    flex: none;
    font-size: var(--text-sm);
  }

  .list {
    flex: 0 1 auto;
    min-height: 56px;
    margin: 0;
    padding: 0;
    overflow: auto;
    list-style: none;
  }

  .list li {
    display: flex;
    align-items: center;
    gap: 4px;
    min-height: 28px;
    font-size: var(--text-sm);
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .name.current {
    color: var(--text);
    font-weight: 500;
  }

  .tag {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .list .btn {
    height: 24px;
    padding: 0 8px;
  }

  .create {
    display: flex;
    gap: 8px;
    flex: none;
  }

  .create .field {
    flex: 1;
    min-width: 0;
  }

  .create .btn {
    height: var(--control-lg);
  }

  .error {
    margin: 0;
    color: var(--error);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }

  .dim {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .small {
    font-size: var(--text-xs);
    line-height: 16px;
  }
</style>
