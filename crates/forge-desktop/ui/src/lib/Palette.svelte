<script>
  import { tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  // `onQuery(query)` returns more items for a query, such as files found by
  // the workspace index; they follow the matching static items.
  let { items = [], placeholder = "Search sessions, files and actions", onQuery, onClose } = $props();

  let query = $state("");
  let cursor = $state(0);
  let input = $state();
  let found = $state({ query: "", items: [] });

  let queryTimer;
  $effect(() => {
    const value = query;
    clearTimeout(queryTimer);
    if (!onQuery || !value.trim()) {
      found = { query: value, items: [] };
      return;
    }
    queryTimer = setTimeout(async () => {
      const items = await onQuery(value);
      if (query === value) found = { query: value, items };
    }, 80);
    return () => clearTimeout(queryTimer);
  });

  const filtered = $derived.by(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return items.slice(0, 200);
    const matching = items.filter(
      (item) =>
        item.label.toLowerCase().includes(needle) ||
        (item.detail ?? "").toLowerCase().includes(needle) ||
        (item.group ?? "").toLowerCase().includes(needle),
    );
    const ids = new Set(matching.map((item) => item.id));
    const extra = found.query === query ? found.items.filter((item) => !ids.has(item.id)) : [];
    return [...matching, ...extra].slice(0, 200);
  });

  $effect(() => {
    if (cursor >= filtered.length) cursor = Math.max(0, filtered.length - 1);
  });

  $effect(() => {
    const active = filtered.length ? `palette-option-${cursor}` : null;
    tick().then(() => input?.closest(".palette")?.querySelector(`#${active}`)?.scrollIntoView({ block: "nearest" }));
  });

  // A detail is a keyboard hint, a path, or a plain word.
  function isShortcut(detail) {
    return /[⌘⌃⌥⇧]|Ctrl/.test(detail);
  }

  function isPath(item) {
    return item.group === "Files" || item.detail.includes("/");
  }

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
  <div class="palette" role="dialog" aria-modal="true" aria-label="Command palette" tabindex="-1" use:modal={{ onClose }}>
    <div class="search">
      <Icon name="search" size={14} />
      <input
        use:focusInput
        bind:this={input}
        bind:value={query}
        {placeholder}
        aria-label="Command palette"
        role="combobox"
        aria-autocomplete="list"
        aria-expanded="true"
        aria-controls="palette-results"
        aria-activedescendant={filtered.length ? `palette-option-${cursor}` : undefined}
        spellcheck="false"
        onkeydown={keydown}
      />
      <kbd>esc</kbd>
    </div>
    <ul class="results" id="palette-results" role="listbox" aria-label="Commands">
      {#each filtered as item, index (item.id)}
        {#if item.group && (index === 0 || filtered[index - 1].group !== item.group)}
          <li class="group-label" role="presentation" aria-hidden="true">{item.group}</li>
        {/if}
        <li role="presentation">
          <button
            type="button"
            id={`palette-option-${index}`}
            role="option"
            aria-selected={index === cursor}
            tabindex="-1"
            class:active={index === cursor}
            onmouseenter={() => (cursor = index)}
            onclick={() => run(item)}
          >
            <span class="group sr-only">{item.group ?? ""}</span>
            <span class="label">{item.label}</span>
            {#if item.detail}
              {#if isShortcut(item.detail)}
                <span class="detail hint">{item.detail}</span>
              {:else}
                <span class="detail" class:path={isPath(item)}>{item.detail}</span>
              {/if}
            {/if}
          </button>
        </li>
      {/each}
      {#if filtered.length === 0}
        <li class="empty" role="presentation">No matches.</li>
      {/if}
    </ul>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 12vh;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    z-index: 40;
  }

  .palette {
    display: flex;
    flex-direction: column;
    width: min(560px, calc(100vw - 48px));
    max-height: min(432px, calc(88vh - 24px));
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
    color: var(--muted);
  }

  .search input {
    flex: 1;
    align-self: stretch;
    min-width: 0;
    padding: 0;
    border: 0;
    background: none;
    font-size: var(--text-md);
  }

  .search input::placeholder {
    color: var(--muted);
  }

  /* Focus is shown on the palette's own edge, not as a box inside its
     header. */
  .search input:focus-visible {
    outline: none;
  }

  .palette:focus-within {
    border-color: var(--accent);
    outline: 1px solid var(--accent);
  }

  kbd {
    flex: none;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 16px;
  }

  .results {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 0 8px 8px;
    overflow: auto;
    list-style: none;
  }

  .results li {
    margin: 0;
  }

  .group-label {
    padding: 12px 8px 4px;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .results button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    height: var(--control-lg);
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--secondary);
    font-size: var(--text-md);
    text-align: left;
  }

  /* The row the arrow keys (or the pointer) are on. */
  .results button.active {
    background: var(--hover);
    color: var(--text);
  }

  .results button:focus-visible {
    outline-offset: -2px;
  }

  .label {
    flex: none;
    max-width: 70%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .detail {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--muted);
    font-size: var(--text-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .detail.path {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }

  /* A keyboard hint sits at the right edge of its row. */
  .detail.hint {
    flex: none;
    margin-left: auto;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 16px;
  }

  .empty {
    padding: 16px 8px 8px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
</style>
