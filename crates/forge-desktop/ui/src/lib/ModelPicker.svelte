<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let {
    models = [],
    currentModel = "",
    loading = false,
    error = "",
    onClose,
    onApply,
    onProviders,
  } = $props();

  let query = $state("");
  let selectedId = $state("");
  let effort = $state("");
  let applying = $state(false);
  let localError = $state("");

  const filtered = $derived.by(() => {
    const needle = query.trim().toLowerCase();
    return needle
      ? models.filter(
          (model) =>
            model.name.toLowerCase().includes(needle) ||
            model.id.toLowerCase().includes(needle) ||
            model.provider.toLowerCase().includes(needle),
        )
      : models;
  });

  // Only a model visible under the current filter can be applied.
  const selected = $derived(filtered.find((model) => model.id === selectedId) ?? null);

  const groups = $derived.by(() => {
    const map = new Map();
    for (const model of filtered) {
      const provider = model.provider || model.profile_id;
      if (!map.has(provider)) map.set(provider, []);
      map.get(provider).push(model);
    }
    return [...map.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  });

  $effect(() => {
    if (!selectedId || !models.some((model) => model.id === selectedId)) {
      selectedId = currentModel || models[0]?.id || "";
    }
    if (selected) {
      if (!selected.effort.includes(effort)) {
        effort = selected.effort_default || selected.effort[0] || "";
      }
    }
  });

  async function apply() {
    if (!selected) return;
    applying = true;
    localError = "";
    const ok = await onApply(selected.id, selected.profile_id, effort || null);
    applying = false;
    if (!ok) localError = "The model switch failed; see the error above.";
  }

  function pick(model) {
    selectedId = model.id;
    effort = model.effort_default || model.effort[0] || "";
  }
</script>

<div class="overlay" role="presentation" onclick={(event) => event.target === event.currentTarget && onClose()}>
  <div class="picker" role="dialog" aria-modal="true" aria-label="Choose a model" tabindex="-1" use:modal={{ onClose, busy: applying }}>
    <header>
      <h2>Model</h2>
      <button class="icon-btn" type="button" aria-label="Close" onclick={onClose}>
        <Icon name="close" size={13} />
      </button>
    </header>

    <div class="search">
      <Icon name="search" size={13} />
      <input
        class="field"
        bind:value={query}
        placeholder="Search models"
        aria-label="Search models"
        spellcheck="false"
      />
    </div>

    {#if loading}
      <p class="state">Loading models…</p>
    {:else if error || localError}
      <p class="state error">{error || localError}</p>
    {:else if models.length === 0}
      <p class="state">No runnable models. Open Providers to connect one or add a model.</p>
    {:else}
      <div class="list">
        {#each groups as [provider, entries] (provider)}
          <div class="group">{provider}</div>
          {#each entries as model (model.id)}
            <button
              type="button"
              class="model"
              class:selected={model.id === selectedId}
              aria-pressed={model.id === selectedId}
              onclick={() => pick(model)}
            >
              <span class="name">{model.name}</span>
              {#if model.id !== model.name}<span class="id mono">{model.id}</span>{/if}
              <span class="spacer"></span>
              {#if model.effort.length > 1}
                <span class="effort">{model.effort.join(" · ")}</span>
              {/if}
              {#if model.id === currentModel}
                <span class="current">current</span>
              {/if}
            </button>
          {/each}
        {/each}
      </div>
    {/if}

    {#if selected}
      <div class="choice">
        <span class="picked" title={selected.id}>{selected.name}</span>
        {#if selected.effort.length > 1}
          <span class="choice-label" aria-hidden="true">Reasoning effort</span>
          <span class="levels" aria-label="Reasoning effort">
            {#each selected.effort as level}
              <button
                type="button"
                class="btn level"
                class:on={level === effort}
                aria-pressed={level === effort}
                onclick={() => (effort = level)}
              >
                {level}
              </button>
            {/each}
          </span>
        {/if}
      </div>
    {/if}

    <footer>
      <button class="btn" type="button" onclick={onProviders}>Providers…</button>
      <span class="spacer"></span>
      <button class="btn" type="button" onclick={onClose}>Cancel</button>
      <button class="btn primary" type="button" disabled={!selected || applying} onclick={apply}>
        {applying ? "Switching…" : "Switch"}
      </button>
    </footer>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    z-index: 30;
  }

  .picker {
    display: flex;
    flex-direction: column;
    width: min(560px, calc(100vw - 48px));
    max-height: min(576px, calc(100vh - 96px));
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
  }

  h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .search {
    position: relative;
    display: flex;
    align-items: center;
    flex: none;
    padding: 16px 16px 8px;
    color: var(--muted);
  }

  /* The icon sits inside the field's left padding. */
  .search :global(svg) {
    position: absolute;
    left: 28px;
    pointer-events: none;
  }

  .search input {
    padding-left: 32px;
  }

  /* The browser audit requires a real two-pixel outline on this input, so
     it takes the global focus outline in place of the field's ring. */
  .search input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    box-shadow: none;
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 0 8px 8px;
  }

  .group {
    padding: 12px 8px 4px;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .model {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    height: var(--control-lg);
    padding: 0 8px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--text);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .model:hover {
    background: var(--hover);
  }

  .model.selected {
    border-color: var(--border);
    background: var(--raised);
  }

  .model:focus-visible {
    outline-offset: -2px;
  }

  .name {
    flex: none;
    max-width: 60%;
    overflow: hidden;
    font-size: var(--text-md);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .id {
    min-width: 0;
    overflow: hidden;
    color: var(--muted);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .effort,
  .current {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .current {
    color: var(--secondary);
  }

  .state {
    margin: 0;
    padding: 16px;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .state.error {
    color: var(--error);
    overflow-wrap: anywhere;
  }

  .choice,
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-top: 1px solid var(--border);
  }

  .picked {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--secondary);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .choice-label {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  /* Segmented control: adjacent buttons sharing their borders. */
  .levels {
    display: flex;
    flex: none;
  }

  .level {
    position: relative;
    border-radius: 0;
  }

  .level + .level {
    margin-left: -1px;
  }

  .level:first-child {
    border-radius: var(--radius-sm) 0 0 var(--radius-sm);
  }

  .level:last-child {
    border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
  }

  .level:only-child {
    border-radius: var(--radius-sm);
  }

  .level.on,
  .level.on:hover {
    z-index: 1;
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--text);
  }

  .level:focus-visible {
    z-index: 2;
  }

  .spacer {
    flex: 1;
  }

  @media (prefers-reduced-motion: reduce) {
    .model {
      transition: none;
    }
  }
</style>
