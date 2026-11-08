<script>
  import { tick } from "svelte";
  import Markdown from "../Markdown.svelte";
  import Icon from "./Icon.svelte";

  let { messages = [], streaming = "", turnState = "idle", onSuggest, onExpand, onOpenImage } = $props();

  let scroller = $state();
  let pinned = $state(true);
  const PAGE_SIZE = 100;
  let pageEnd = $state(null);
  const end = $derived(Math.min(pageEnd ?? messages.length, messages.length));
  const start = $derived(Math.max(0, end - PAGE_SIZE));
  const visible = $derived(messages.slice(start, end));

  $effect(() => {
    if (messages.length === 0) {
      pageEnd = null;
      pinned = true;
    }
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
      <div class="intro-mark"><Icon name="chat" size={20} /></div>
      <h2>Ask Forge to work the repository</h2>
      <p>It can inspect files, change code, run commands, and review diffs. Every risky step waits for your approval.</p>
      <div class="suggestions">
        {#each suggestions as suggestion}
          <button type="button" class="suggestion" onclick={() => onSuggest(suggestion)}>
            {suggestion}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  {#each visible as message, index (start + index)}
    <div class="turn {message.role}">
      <div class="who">
        {#if message.role === "user"}You{:else if message.role === "assistant"}Forge{:else}Tool{/if}
        {#if message.role === "tool"}<span class="tool-name">{toolName(message)}</span>{/if}
      </div>
      {#if message.thinking}
        <details class="thinking">
          <summary>Reasoning</summary>
          <pre>{message.thinking}</pre>
          {#if onExpand && message.thinking.endsWith("…")}
            <button class="expand" type="button" onclick={() => onExpand(start + index)}>Show full reasoning</button>
          {/if}
        </details>
      {/if}
      <article
        class="message {message.role}"
        data-role={message.role}
        data-optimistic={message.optimistic ? "true" : undefined}
      >
        {#if message.role === "tool"}
          <pre class:empty={!message.text.trim()}>{message.text.trim() ? message.text : "No output."}</pre>
          {#if onExpand && message.text.endsWith("…")}
            <button class="expand" type="button" onclick={() => onExpand(start + index)}>Show full output</button>
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
        <Markdown text={streaming} />
      </article>
    </div>
  {/if}
</div>

<style>
  .history { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 16px; color: var(--muted); font-size: 12px; }
  .transcript {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 18px 28px 8px;
    scroll-behavior: smooth;
  }

  .intro {
    max-width: 34rem;
    margin: 12vh auto 0;
    text-align: center;
    color: var(--secondary);
  }

  .intro-mark {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    margin: 0 auto 14px;
    border: 1px solid var(--border-strong);
    border-radius: 12px;
    background: var(--surface);
    color: var(--accent);
  }

  .intro h2 {
    margin: 0 0 6px;
    color: var(--text);
    font-size: 17px;
    font-weight: 600;
  }

  .intro p {
    margin: 0 auto;
    max-width: 30rem;
    font-size: 13px;
  }

  .suggestions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 6px;
    margin-top: 18px;
  }

  .suggestion {
    height: 30px;
    padding: 0 11px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface);
    color: var(--secondary);
    font-size: 12px;
    transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
  }

  .suggestion:hover {
    background: var(--hover);
    border-color: var(--border-strong);
    color: var(--text);
  }

  .turn {
    max-width: 52rem;
    margin: 0 auto 18px;
  }

  .who {
    display: flex;
    align-items: baseline;
    gap: 7px;
    margin-bottom: 5px;
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .tool-name {
    color: var(--secondary);
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0;
    text-transform: none;
  }

  .attachments {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }

  .attachment {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    max-width: 100%;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface);
    color: var(--secondary);
    font-size: 12px;
  }

  .attachment:hover {
    background: var(--hover);
    border-color: var(--border-strong);
    color: var(--text);
  }

  .attachment .size {
    color: var(--muted);
  }

  .message {
    font-size: 13.5px;
    line-height: 1.62;
    overflow-wrap: anywhere;
  }

  .message.user {
    padding: 10px 13px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  .message.assistant :global(p) {
    margin: 0 0 0.7em;
  }

  .message.assistant :global(p:last-child) {
    margin-bottom: 0;
  }

  .message.assistant :global(strong) {
    color: var(--strong);
    font-weight: 600;
  }

  .message.assistant :global(em) {
    color: var(--strong);
  }

  .message.assistant :global(a) {
    color: #4fc9df;
    text-decoration: none;
  }

  .message.assistant :global(code),
  .message.assistant :global(pre) {
    font-family: var(--font-mono);
    font-size: 12px;
  }

  .message.assistant :global(code) {
    padding: 1px 5px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-deep);
  }

  .message.assistant :global(pre) {
    margin: 0.6em 0;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-deep);
    overflow: auto;
  }

  .message.assistant :global(pre code) {
    padding: 0;
    border: 0;
    background: none;
  }

  .message.assistant :global(ul),
  .message.assistant :global(ol) {
    margin: 0.4em 0 0.7em;
    padding-left: 1.3em;
  }

  .message.assistant :global(blockquote) {
    margin: 0.6em 0;
    padding-left: 10px;
    border-left: 2px solid var(--border-strong);
    color: var(--secondary);
  }

  .message.assistant :global(h1),
  .message.assistant :global(h2),
  .message.assistant :global(h3) {
    margin: 1em 0 0.4em;
    font-size: 14px;
    font-weight: 600;
  }

  .message.tool {
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-deep);
    overflow: hidden;
  }

  .message.tool pre {
    margin: 0;
    padding: 10px 12px;
    max-height: 260px;
    overflow: auto;
    color: var(--secondary);
    font-family: var(--font-mono);
    font-size: 11.5px;
    line-height: 1.5;
    white-space: pre-wrap;
  }

  .message.tool pre.empty {
    color: var(--muted);
    font-style: italic;
  }

  .message.tool .expand {
    margin: 0 12px 10px;
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--secondary);
    font-size: 11px;
    cursor: pointer;
  }

  .thinking {
    margin: 2px 0 4px;
    color: var(--muted);
    font-size: 11.5px;
  }

  .thinking summary {
    cursor: pointer;
  }

  .thinking pre {
    margin: 6px 0 0;
    padding: 8px 10px;
    max-height: 220px;
    overflow: auto;
    border-left: 2px solid var(--border-strong);
    color: var(--secondary);
    font-family: var(--font-ui);
    white-space: pre-wrap;
  }

  .thinking .expand {
    margin: 6px 0 0 10px;
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--secondary);
    font-size: 11px;
    cursor: pointer;
  }

  .message.streaming::after {
    content: "";
    display: inline-block;
    width: 7px;
    height: 14px;
    margin-left: 2px;
    background: var(--accent);
    vertical-align: -2px;
    animation: blink 1s steps(2, start) infinite;
  }

  @keyframes blink {
    50% {
      opacity: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .transcript { scroll-behavior: auto; }
    .message.streaming::after { animation: none; }
  }
</style>
