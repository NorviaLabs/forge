<script>
  import Icon from "./Icon.svelte";

  let { items = [], placeholder = "Search sessions, files and actions", onClose } = $props();

  let query = $state("");
  let cursor = $state(0);
  let input = $state();

  const filtered = $derived.by(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return items.slice(0, 200);
    return items
      .filter(
        (item) =>
          item.label.toLowerCase().includes(needle) ||
          (item.detail ?? "").toLowerCase().includes(needle) ||
          (item.group ?? "").toLowerCase().includes(needle),
      )
      .slice(0, 200);
  });

  $effect(() => {
    if (cursor >= filtered.length) cursor = Math.max(0, filtered.length - 1);
  });

  function focusInput(node) {
    node.focus();
    return {};
  }

  function move(delta) {
    if (filtered.length === 0) return;
    cursor = (cursor + delta + filtered.length) % filtered.length;
  }

  function run(item) {
    if (!item) return;
    onClose();
    item.action();
  }

  function keydown(event) {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      move(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      move(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      run(filtered[cursor]);
    }
  }
</script>

<div class="overlay" role="presentation" onclick={(event) => event.target === event.currentTarget && onClose()}>
  <div class="palette" role="dialog" aria-modal="true" aria-label="Command palette">
    <div class="search">
      <Icon name="search" size={14} />
      <input
        use:focusInput
        bind:this={input}
        bind:value={query}
        {placeholder}
        aria-label="Command palette"
        spellcheck="false"
        onkeydown={keydown}
      />
      <kbd>esc</kbd>
    </div>
    <ul class="results">
      {#each filtered as item, index (item.id)}
        <li>
          <button
            type="button"
            class:active={index === cursor}
            onmouseenter={() => (cursor = index)}
            onclick={() => run(item)}
          >
            <span class="group">{item.group ?? ""}</span>
            <span class="label">{item.label}</span>
            {#if item.detail}<span class="detail">{item.detail}</span>{/if}
          </button>
        </li>
      {/each}
      {#if filtered.length === 0}
        <li class="empty">No matches.</li>
      {/if}
    </ul>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    display: flex;
    justify-content: center;
    padding-top: 14vh;
    background: rgba(5, 7, 9, 0.5);
    z-index: 40;
  }

  .palette {
    display: flex;
    flex-direction: column;
    width: min(38rem, calc(100vw - 64px));
    max-height: 26rem;
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
    color: var(--muted);
  }

  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    outline: none;
    font-size: 13.5px;
  }

  kbd {
    padding: 1px 5px;
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--muted);
    font-size: 10px;
  }

  .results {
    list-style: none;
    margin: 0;
    padding: 6px;
    overflow: auto;
  }

  .results li {
    margin: 0;
  }

  .results button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    height: 30px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    text-align: left;
    color: var(--secondary);
    font-size: 12.5px;
  }

  .results button.active {
    background: var(--selection);
    color: var(--text);
  }

  .group {
    width: 5.6rem;
    flex: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .detail {
    max-width: 40%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-family: var(--font-mono);
    font-size: 11px;
  }

  .empty {
    padding: 12px;
    color: var(--muted);
    font-size: 12.5px;
    list-style: none;
  }
</style>
