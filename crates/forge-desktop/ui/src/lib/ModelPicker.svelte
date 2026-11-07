<script>
  import Icon from "./Icon.svelte";

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

  const selected = $derived(
    models.find((model) => model.id === selectedId) ?? models[0] ?? null,
  );

  const groups = $derived.by(() => {
    const needle = query.trim().toLowerCase();
    const filtered = needle
      ? models.filter(
          (model) =>
            model.name.toLowerCase().includes(needle) ||
            model.id.toLowerCase().includes(needle) ||
            model.provider.toLowerCase().includes(needle),
        )
      : models;
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
  <div class="picker" role="dialog" aria-modal="true" aria-label="Choose a model">
    <header>
      <h2>Model</h2>
      <button class="icon-btn" type="button" aria-label="Close" onclick={onClose}>
        <Icon name="close" size={13} />
      </button>
    </header>

    <div class="search">
      <Icon name="search" size={13} />
      <input
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
              onclick={() => pick(model)}
            >
              <span class="name">{model.name}</span>
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

    <footer>
      <button class="btn" type="button" onclick={onProviders}>Providers…</button>
      {#if selected}
        <span class="picked" title={selected.id}>{selected.name}</span>
        {#if selected.effort.length > 1}
          <span class="levels" aria-label="Reasoning effort">
            {#each selected.effort as level}
              <button
                type="button"
                class="level"
                class:on={level === effort}
                onclick={() => (effort = level)}
              >
                {level}
              </button>
            {/each}
          </span>
        {/if}
      {/if}
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
    background: rgba(5, 7, 9, 0.55);
    z-index: 30;
  }

  .picker {
    display: flex;
    flex-direction: column;
    width: min(34rem, calc(100vw - 64px));
    max-height: min(36rem, calc(100vh - 96px));
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 14px 8px;
  }

  h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }

  .search {
    display: flex;
    align-items: center;
    gap: 7px;
    margin: 0 14px 8px;
    padding: 0 9px;
    height: 30px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-deep);
    color: var(--muted);
  }

  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    outline: none;
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 0 8px 8px;
  }

  .group {
    padding: 10px 8px 4px;
    color: var(--muted);
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .model {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    height: 30px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--secondary);
    text-align: left;
    font-size: 12.5px;
  }

  .model:hover {
    background: var(--hover);
    color: var(--text);
  }

  .model.selected {
    background: var(--selection);
    color: var(--text);
  }

  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: 12px;
  }

  .effort {
    color: var(--muted);
    font-size: 10.5px;
  }

  .current {
    padding: 0 6px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: 10px;
    letter-spacing: 0.03em;
  }

  .state {
    margin: 0;
    padding: 18px 16px;
    color: var(--muted);
    font-size: 12.5px;
  }

  .state.error {
    color: var(--error);
  }

  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    border-top: 1px solid var(--border);
  }

  .picked {
    max-width: 12rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--secondary);
    font-family: var(--font-mono);
    font-size: 11.5px;
  }

  .levels {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 6px;
    background: var(--bg-deep);
  }

  .level {
    height: 20px;
    padding: 0 7px;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--muted);
    font-size: 10.5px;
  }

  .level.on {
    background: var(--accent-soft);
    color: var(--accent);
  }

  .spacer {
    flex: 1;
  }
</style>
