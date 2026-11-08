<script>
  import { tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { findText, highlight, lineOffset, replaceAllText } from "../editor.js";

  let {
    tab,
    file = null,
    diff = null,
    loading = false,
    error = "",
    draft = "",
    dirty = false,
    onDraftChange,
  } = $props();

  const MAX_LINES = 4000;
  let editor = $state();
  let syntax = $state();
  let searchInput = $state();
  let lineInput = $state();
  let replaceInput = $state();
  let findOpen = $state(false);
  let query = $state("");
  let replacement = $state("");
  let line = $state("");
  let navigationError = $state("");
  const colored = $derived(highlight(draft, tab.path));

  const readOnly = $derived(file?.read_only ?? "");
  const editable = $derived(tab.kind === "file" && !!file && !file.read_only);
  const lines = $derived(
    (tab.kind === "diff" ? diff?.text : file?.text)?.split("\n") ?? [],
  );
  const shown = $derived(lines.slice(0, MAX_LINES));
  const clipped = $derived(lines.length > MAX_LINES);

  $effect(() => {
    void tab.id;
    navigationError = "";
    tick().then(() => {
      if (!editor) return;
      editor.setSelectionRange(0, 0);
      editor.scrollTop = 0;
      editor.scrollLeft = 0;
      syncScroll();
    });
  });

  function diffClass(line) {
    if (line.startsWith("+++") || line.startsWith("---")) return "meta";
    if (line.startsWith("@@")) return "hunk";
    if (line.startsWith("+")) return "add";
    if (line.startsWith("-")) return "del";
    return "";
  }

  function onEditorKeydown(event) {
    if ((event.metaKey || event.ctrlKey) && ["f", "g", "l"].includes(event.key.toLowerCase())) {
      event.preventDefault();
      if (event.key.toLowerCase() === "g") find(event.shiftKey);
      else showNavigation(event.key.toLowerCase() === "l" ? "line" : "find");
      return;
    }
    if (event.key === "Escape" && findOpen) { closeNavigation(); return; }
  }

  function select(start, end = start, focus = true) {
    if (focus) editor?.focus();
    editor?.setSelectionRange(start, end);
    if (editor) {
      const row = draft.slice(0, start).split("\n").length - 1;
      const height = parseFloat(getComputedStyle(editor).lineHeight);
      editor.scrollTop = Math.max(0, row * height - editor.clientHeight / 2);
      syncScroll();
    }
  }

  function find(backwards = false, focus = true) {
    if (!query) return;
    const from = backwards ? (editor?.selectionStart ?? 0) - 1 : editor?.selectionEnd ?? 0;
    const range = findText(draft, query, from, backwards);
    navigationError = range ? "" : "No matches.";
    if (range) select(...range, focus);
  }

  function jump() {
    const offset = lineOffset(draft, Number(line));
    navigationError = offset === null ? "Enter an existing line number." : "";
    if (offset !== null) select(offset);
  }

  function replaceNext() {
    if (!query) return;
    const start = editor?.selectionStart ?? 0;
    const end = editor?.selectionEnd ?? 0;
    const range = draft.slice(start, end).toLowerCase() === query.toLowerCase()
      ? [start, end]
      : findText(draft, query, end);
    if (!range) { navigationError = "No matches."; return; }
    const [from, to] = range;
    onDraftChange(`${draft.slice(0, from)}${replacement}${draft.slice(to)}`);
    navigationError = "";
    tick().then(() => {
      // Keep the caret in the replace field so Enter repeats.
      select(from + replacement.length, from + replacement.length, false);
      find(false, false);
    });
  }

  function replaceAll() {
    const { text, count } = replaceAllText(draft, query, replacement);
    if (!count) { navigationError = "No matches."; return; }
    onDraftChange(text);
    navigationError = `${count} replacement${count === 1 ? "" : "s"}`;
  }

  function syncScroll() {
    if (!syntax || !editor) return;
    syntax.scrollTop = editor.scrollTop;
    syntax.scrollLeft = editor.scrollLeft;
  }

  export function showNavigation(mode = "find") {
    findOpen = true;
    tick().then(() => mode === "line" ? lineInput?.focus() : mode === "replace" ? replaceInput?.focus() : searchInput?.focus());
  }

  /** Select a 1-based line, used when a search hit opens its file. */
  export function jumpToLine(number) {
    line = String(number);
    tick().then(jump);
  }

  /** 0-based line under the caret, for prompt attachment. */
  export function cursorLine() {
    const offset = editor?.selectionStart ?? 0;
    return draft.slice(0, offset).split("\n").length - 1;
  }

  function formatBytes(bytes) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  function closeNavigation() {
    findOpen = false;
    editor?.focus();
  }

  function navigationKeydown(event, mode) {
    if (event.key === "Escape") { event.preventDefault(); closeNavigation(); }
    else if (event.key === "Enter") {
      event.preventDefault();
      if (mode === "line") jump();
      else if (mode === "replace") {
        if (event.shiftKey) replaceAll();
        else replaceNext();
      } else find(event.shiftKey);
    }
  }
</script>

<section class="file-pane" aria-label={tab.title}>
  <header>
    <Icon name={tab.kind === "diff" ? "diff" : "file"} size={14} />
    <span class="path mono">{tab.path}</span>
    {#if tab.kind === "diff"}<span class="chip">{tab.side === "staged" ? "staged" : tab.side === "unstaged" ? "working tree" : tab.side === "last_turn" ? "last turn" : "staged + working tree"}</span>{/if}
    {#if file?.read_only}<span class="chip">read-only</span>{/if}
    {#if dirty}<span class="chip dirty">Unsaved</span>{/if}
    {#if loading}<span class="mini spin"><Icon name="refresh" size={13} /></span>{/if}
  </header>

  {#if error}
    <div class="banner" role="alert">
      <Icon name="alert" size={14} />
      <span>{error}</span>
      {#if tab.kind === "file"}
        <span>Use “Reload current file from disk” in the command palette (⌘/Ctrl K).</span>
      {/if}
    </div>
  {/if}

  {#if findOpen && editable}
    <div class="navigation">
      <input class="field" bind:this={searchInput} bind:value={query} aria-label="Find in file" placeholder="Find in file" onkeydown={(event) => navigationKeydown(event, "find")} />
      <input class="field" bind:this={replaceInput} bind:value={replacement} aria-label="Replace with" placeholder="Replace with" onkeydown={(event) => navigationKeydown(event, "replace")} />
      <span>Enter: next · Shift Enter: previous</span>
      <span>Enter: replace · Shift Enter: all</span>
      <input class="field line-number" bind:this={lineInput} bind:value={line} aria-label="Line number" placeholder="Line" inputmode="numeric" onkeydown={(event) => navigationKeydown(event, "line")} />
      <span>Enter: go · Esc: close</span>
      {#if navigationError}<span role="status">{navigationError}</span>{/if}
    </div>
  {/if}

  {#if loading && lines.length === 0}
    <div class="state">Loading…</div>
  {:else if tab.kind === "diff" && !diff?.text?.trim()}
    <div class="state">No working-tree changes in this file.</div>
  {:else if tab.image}
    <figure class="image-preview">
      <img src={tab.image.data_url} alt={tab.path} />
      <figcaption>
        {tab.image.mime} · {formatBytes(tab.image.byte_len)}
        {#if tab.image.width}· {tab.image.width}×{tab.image.height}{/if}
      </figcaption>
    </figure>
  {:else if editable}
    <div class="editor-wrap">
      <pre class="syntax" aria-hidden="true" bind:this={syntax}>{#each colored as token, index (index)}<span class={token.kind}>{token.text}</span>{/each}{"\n"}</pre>
    <textarea
      class="editor"
      bind:this={editor}
      aria-label="Edit {tab.path}"
      spellcheck="false"
      wrap="off"
      value={draft}
      oninput={(event) => onDraftChange(event.currentTarget.value)}
      onkeydown={onEditorKeydown}
      onscroll={syncScroll}
    ></textarea>
    </div>
  {:else if readOnly}
    <p class="notice">{readOnly}</p>
    <pre class="code">{#each shown as line, index (index)}<span class="line"><span class="ln">{index + 1}</span><span class="src">{line || " "}</span></span>{/each}</pre>
  {:else if tab.kind === "diff"}
    <pre class="code diff">{#each shown as line, index (index)}<span class="line {diffClass(line)}">{line || " "}</span>{/each}</pre>
  {:else}
    <div class="state">Empty file.</div>
  {/if}

  {#if clipped && !editable}
    <p class="clipped">Showing the first {MAX_LINES} lines.</p>
  {/if}
</section>

<style>
  .file-pane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1;
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--border);
    color: var(--muted);
    font-size: 12px;
  }

  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--secondary);
  }

  .mini {
    display: inline-flex;
  }

  .dirty {
    color: var(--warning);
  }

  .editor-wrap { position: relative; flex: 1; min-height: 0; background: var(--bg-deep); }
  .image-preview { display: grid; place-items: center; gap: 10px; flex: 1; min-height: 0; margin: 0; padding: 24px; overflow: auto; }
  .image-preview img { max-width: 100%; max-height: 100%; object-fit: contain; border-radius: var(--radius-sm); background: var(--bg-deep); }
  .image-preview figcaption { color: var(--muted); font-size: 12px; }
  .navigation { display: flex; flex-wrap: wrap; gap: 5px; align-items: center; padding: 6px 16px; border-bottom: 1px solid var(--border); }
  .navigation .field { width: 160px; }
  .navigation .line-number { width: 64px; }
  .navigation span { color: var(--muted); font-size: 12px; }
  .navigation [role="status"] { color: var(--warning); }
  .editor, .syntax {
    position: absolute;
    inset: 0;
    flex: 1;
    min-height: 0;
    width: 100%;
    padding: 12px 16px;
    border: 0;
    height: 100%;
    margin: 0;
    box-sizing: border-box;
    background: transparent;
    color: transparent;
    caret-color: var(--text);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.6;
    resize: none;
    outline: none;
    tab-size: 2;
    white-space: pre;
    overflow: auto;
    overflow-wrap: normal;
    letter-spacing: normal;
    font-variant-ligatures: none;
  }
  .syntax { color: var(--secondary); pointer-events: none; scrollbar-width: none; }
  .editor:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .syntax::-webkit-scrollbar { display: none; }
  .syntax .keyword { color: var(--accent); }
  .syntax .string { color: var(--success); }
  .syntax .number { color: var(--warning); }
  .syntax .comment { color: var(--muted); }
  .editor::selection { background: var(--accent-soft); color: transparent; }
  @media (forced-colors: active) { .syntax { display: none; } .editor { color: CanvasText; } }

  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 16px;
    border-bottom: 1px solid color-mix(in srgb, var(--error) 45%, var(--border));
    background: color-mix(in srgb, var(--error) 9%, var(--surface));
    color: var(--error);
    font-size: 11.5px;
  }

  .banner span {
    flex: 1;
    overflow-wrap: anywhere;
  }

  .notice {
    margin: 0;
    padding: 7px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    color: var(--warning);
    font-size: 11.5px;
  }

  .code {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 10px 0 18px;
    overflow: auto;
    background: var(--bg-deep);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.6;
  }

  .line {
    display: block;
    padding: 0 16px 0 0;
    min-height: 1.6em;
    white-space: pre;
  }

  .ln {
    display: inline-block;
    width: 3.6rem;
    padding-right: 12px;
    color: var(--muted);
    text-align: right;
    user-select: none;
  }

  .src {
    color: var(--secondary);
  }

  .diff .line {
    padding-left: 16px;
  }

  .diff .add {
    background: var(--add);
    color: var(--success);
  }

  .diff .del {
    background: var(--remove);
    color: var(--error);
  }

  .diff .hunk {
    color: var(--accent);
    background: var(--accent-soft);
  }

  .diff .meta {
    color: var(--muted);
  }

  .state {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    margin: 0;
    padding: 24px;
    color: var(--muted);
    font-size: 13px;
  }

  .state.error {
    color: var(--error);
  }

  .clipped {
    margin: 0;
    padding: 6px 16px;
    border-top: 1px solid var(--border);
    color: var(--muted);
    font-size: 11px;
  }
</style>
