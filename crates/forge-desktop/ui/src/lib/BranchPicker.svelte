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
      <ul class="list">
        {#each branches as branch (branch)}
          <li>
            <span class="name mono" class:current={branch === current}>
              {branch}{#if branch === current}<span class="tag">current</span>{/if}
            </span>
            {#if branch !== current}
              <button class="btn" type="button" disabled={busy} onclick={() => onSwitch(branch)}>Switch</button>
              <button class="btn" type="button" disabled={busy} onclick={() => onMerge(branch)}>Merge in</button>
              <button class="btn" type="button" disabled={busy} onclick={() => onDelete(branch)}>Delete…</button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}

    <div class="create">
      <input class="field" bind:value={newName} placeholder="New branch name" aria-label="New branch name" />
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

    {#if current}
      <div class="create">
        <input class="field" bind:value={renameName} aria-label="Rename current branch" />
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
    background: rgba(5, 7, 9, 0.6);
    z-index: 20;
  }

  .dialog {
    width: min(30rem, calc(100vw - 48px));
    max-height: 80vh;
    overflow: auto;
    padding: 16px 18px;
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }

  h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0 0 12px;
    padding: 0;
    list-style: none;
  }

  .list li {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 28px;
  }

  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--secondary);
    font-size: 12px;
  }

  .name.current {
    color: var(--text);
  }

  .tag {
    margin-left: 6px;
    padding: 1px 5px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent);
    font-family: var(--font-ui);
    font-size: 9.5px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .list .btn {
    height: 22px;
    padding: 0 7px;
    font-size: 11px;
  }

  .create {
    display: flex;
    gap: 6px;
    margin-bottom: 8px;
  }

  .error {
    margin: 0 0 8px;
    color: var(--error);
    font-size: 12px;
  }

  .dim {
    color: var(--muted);
  }

  .small {
    margin: 10px 0 0;
    font-size: 11px;
  }
</style>
