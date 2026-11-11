<script>
  import { onMount, tick } from "svelte";
  import Markdown from "../Markdown.svelte";
  import Icon from "./Icon.svelte";
  import { streamSegments } from "../view.js";

  let {
    messages = [],
    streaming = "",
    turnState = "idle",
    scroll = $bindable(0),
    pinned = $bindable(true),
    onSuggest,
    onExpand,
    onOpenImage,
  } = $props();

  let scroller = $state();
  const PAGE_SIZE = 100;
  let pageEnd = $state(null);
  const end = $derived(Math.min(pageEnd ?? messages.length, messages.length));
  const start = $derived(Math.max(0, end - PAGE_SIZE));
  const visible = $derived(messages.slice(start, end));

  $effect(() => {
    if (messages.length === 0) {
      pageEnd = null;
    }
  });

  // The transcript is remounted per session, so restore where the reader was.
  onMount(async () => {
    await tick();
    if (scroller) scroller.scrollTop = scroll;
  });

  async function page(direction) {
    pinned = false;
    pageEnd = Math.min(messages.length, Math.max(PAGE_SIZE, end + direction * PAGE_SIZE));
    await tick();
    if (scroller) scroller.scrollTop = 0;
  }

  async function latest() {
    pageEnd = null;
    pinned = true;
    await tick();
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  }

  const suggestions = [
    "Explain the architecture of this codebase",
    "Find the riskiest bug in the changes",
    "Run the tests and diagnose failures",
  ];

  // Follow new output unless the reader scrolled up.
  $effect(() => {
    void messages.length;
    void streaming.length;
    if (!pinned || pageEnd !== null || !scroller) return;
    tick().then(() => {
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    });
  });

  function onScroll() {
    if (!scroller) return;
    // Paging is explicit; layout-induced scroll events must not hide live output.
    const distance = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    pinned = distance < 48;
    scroll = scroller.scrollTop;
  }

  // A short single-line result sits on the tool's own row.
  function oneLine(message) {
    const text = message.text.trim();
    return !text.includes("\n") && text.length <= 96 && !message.text.endsWith("…");
  }

  function toolName(message) {
    return message.name || "tool";
  }

  function imageName(path) {
    return path.split("/").pop() || path;
  }

  function formatBytes(bytes) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
</script>

<div class="transcript" bind:this={scroller} onscroll={onScroll} role="log" aria-label="Transcript">
  {#if messages.length > PAGE_SIZE || (pageEnd !== null && pageEnd < messages.length)}
    <nav class="history" aria-label="Transcript history">
      <button class="btn" type="button" disabled={start === 0} onclick={() => page(-1)}>Older</button>
      <span>{start + 1}–{end} of {messages.length} messages</span>
      <button class="btn" type="button" disabled={end === messages.length} onclick={() => page(1)}>Newer</button>
      <button class="btn" type="button" onclick={latest}>Latest</button>
    </nav>
  {/if}
  {#if messages.length === 0 && !streaming}
    <div class="intro">
      <h2>Ask Forge to work the repository</h2>
      <p>It can inspect files, change code, run commands, and review diffs. Every risky step waits for your approval.</p>
      <div class="suggestions">
        {#each suggestions as suggestion}
          <button type="button" class="btn suggestion" onclick={() => onSuggest(suggestion)}>
            {suggestion}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  {#each visible as message, index (start + index)}
    <div class="turn {message.role}" class:multi={message.role === "tool" && !oneLine(message)} class:expanded={message.expanded}>
      <div class="who">
        {#if message.role === "user"}You{:else if message.role === "assistant"}Forge{:else}<span class="sr-only">Tool</span>
          <span class="tool-name">{toolName(message)}</span>
          {#if oneLine(message)}<span class="tool-result" class:empty={!message.text.trim()}>{message.text.trim() || "No output."}</span>{/if}
        {/if}
      </div>
      {#if message.thinking}
        <details class="thinking">
          <summary>Reasoning</summary>
          <pre>{message.thinking}</pre>
          {#if onExpand && message.thinking.endsWith("…")}
            <button class="btn expand" type="button" onclick={() => onExpand(start + index)}>Show full reasoning</button>
          {/if}
        </details>
      {/if}
      <article
        class="message {message.role}"
        data-role={message.role}
        data-optimistic={message.optimistic ? "true" : undefined}
      >
        {#if message.role === "tool"}
          {#if !oneLine(message)}<pre>{message.text}</pre>{/if}
          {#if onExpand && message.text.endsWith("…")}
            <button class="btn expand" type="button" onclick={() => onExpand(start + index)}>Show full output</button>
          {/if}
        {:else}
          <Markdown text={message.text} />
        {/if}
      </article>
      {#if message.attachments?.length}
        <div class="attachments">
          {#each message.attachments as image}
            <button
              type="button"
              class="attachment"
              title={image.path}
              onclick={() => onOpenImage?.(image.path)}
            >
              <Icon name="file" size={12} />
              <span>{imageName(image.path)}</span>
              <span class="size">
                {image.mime} · {formatBytes(image.byte_len)}{#if image.width} · {image.width}×{image.height}{/if}
              </span>
            </button>
          {/each}
        </div>
      {/if}
    </div>
  {/each}

  {#if streaming && end === messages.length}
    <div class="turn assistant">
      <div class="who">Forge</div>
      <article class="message assistant streaming" data-role="assistant-streaming">
        <!-- Finished blocks keep their text, so only the growing tail re-renders. -->
        {#each streamSegments(streaming) as segment, index (index)}
          <Markdown text={segment} />
        {/each}
      </article>
    </div>
  {/if}
</div>

<style>
  /* The scroller fills the column; everything in it sits in one centred
     720px column. */
  .transcript {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 24px 24px 16px;
    /* Reserved on both sides, so the column stays centred over the cards
       below whether or not the transcript scrolls. */
    scrollbar-gutter: stable both-edges;
    scroll-behavior: smooth;
  }

  .history,
  .intro,
  .turn {
    max-width: 720px;
    margin-right: auto;
    margin-left: auto;
  }

  .history {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-bottom: 16px;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .intro {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 100%;
    color: var(--muted);
    text-align: center;
  }

  .intro h2 {
    margin: 0 0 4px;
    color: var(--text);
    font-size: var(--text-lg);
    font-weight: 600;
    line-height: 24px;
  }

  .intro p {
    max-width: 480px;
    margin: 0;
    font-size: var(--text-md);
  }

  .suggestions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    max-width: 100%;
    margin-top: 16px;
  }

  .suggestion {
    display: inline-block;
    max-width: 100%;
    overflow: hidden;
    line-height: 24px;
    text-overflow: ellipsis;
  }

  /* A work log: every message is a block under a one-line header. */
  .turn + .turn {
    margin-top: 20px;
  }

  .who {
    display: flex;
    align-items: baseline;
    gap: 8px;
    height: 16px;
    margin-bottom: 8px;
    color: var(--text);
    font-size: var(--text-sm);
    font-weight: 600;
    line-height: 16px;
  }

  .message {
    font-size: var(--text-md);
    line-height: 20px;
    overflow-wrap: anywhere;
  }

  .message.user {
    padding: 12px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  /* Tool calls: one 24px row each. Neighbouring tool blocks join into a
     single container, with the radius and padding only at its two ends. */
  .turn.tool {
    padding: 4px 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  .turn.tool + .turn.tool {
    margin-top: 0;
    border-top-left-radius: 0;
    border-top-right-radius: 0;
  }

  .turn.tool:has(+ .turn.tool) {
    border-bottom: 0;
    border-bottom-right-radius: 0;
    border-bottom-left-radius: 0;
  }

  .turn.tool .who {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    height: 24px;
    margin: 0;
    padding: 0 12px;
    color: var(--muted);
    font-family: var(--font-mono);
    font-weight: 400;
    line-height: 24px;
  }

  .tool-name {
    overflow: hidden;
    color: var(--text);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The output sits under the tool's name, in the same column: no box
     inside the box. */
  .tool-result {
    overflow: hidden;
    color: var(--muted);
    font-size: var(--text-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Padding, not margin: a margin would collapse through the card's edge. */
  .message.tool {
    margin: 0 12px 0 116px;
    overflow: hidden;
  }

  .message.tool pre {
    margin-bottom: 4px;
  }

  /* Asked for in full: no height cap, so the rest is on the page and not
     inside a scrolling box. */
  .turn.expanded pre {
    max-height: none;
  }

  /* Longer output starts on the tool's own row, beside its name. */
  .turn.tool.multi .message.tool {
    margin-top: -22px;
  }

  .message.tool pre {
    max-height: 160px;
    margin: 0;
    padding: 0;
    overflow: auto;
    color: var(--muted);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: 20px;
    white-space: pre-wrap;
  }

  .message.tool .expand {
    margin: 4px 0;
  }

  .turn.tool .thinking,
  .turn.tool .attachments {
    margin: 4px 12px 8px;
  }

  .thinking {
    margin: 0 0 8px;
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 16px;
  }

  .thinking summary {
    cursor: pointer;
  }

  .thinking pre {
    max-height: 240px;
    margin: 8px 0 0;
    padding: 0 0 0 12px;
    overflow: auto;
    border-left: 1px solid var(--border);
    color: var(--muted);
    font-family: var(--font-ui);
    font-size: var(--text-sm);
    line-height: 20px;
    white-space: pre-wrap;
  }

  .thinking .expand {
    margin: 8px 0 0 12px;
  }

  .attachments {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 8px;
  }

  .attachment {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    max-width: 100%;
    height: var(--control);
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--secondary);
    font-size: var(--text-sm);
    white-space: nowrap;
    transition: background var(--fast) var(--ease), border-color var(--fast) var(--ease), color var(--fast) var(--ease);
  }

  .attachment span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .attachment:hover {
    background: var(--hover);
    color: var(--text);
  }

  .attachment .size {
    color: var(--muted);
    font-size: var(--text-xs);
  }

  /* The streaming mark hangs below the text, out of flow, so the answer
     does not move when it settles. */
  .message.streaming {
    position: relative;
  }

  .message.streaming::after {
    content: "";
    position: absolute;
    top: 100%;
    left: 0;
    width: 8px;
    height: 8px;
    margin-top: 4px;
    background: var(--accent);
    animation: blink 1s steps(2, start) infinite;
  }

  @keyframes blink {
    50% {
      opacity: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .transcript {
      scroll-behavior: auto;
    }

    .attachment {
      transition: none;
    }

    .message.streaming::after {
      animation: none;
    }
  }
</style>
