<script>
  // The ONLY place in the UI that inserts HTML. A config test fails if any
  // other file uses Svelte's raw-HTML tag.
  import { renderMarkdown } from "./markdown.js";
  let { text } = $props();
  const html = $derived(renderMarkdown(text));
</script>

<div class="markdown">{@html html}</div>

<style>
  /* Blocks carry their own top and bottom margins and the wrapper has no
     padding, so margins collapse across neighbouring wrappers: a streamed
     answer (one wrapper per block) lays out exactly like the settled one. */
  .markdown {
    font-size: var(--text-md);
    line-height: 20px;
    overflow-wrap: anywhere;
  }

  .markdown:first-child > :global(:first-child) {
    margin-top: 0;
  }

  .markdown:last-child > :global(:last-child) {
    margin-bottom: 0;
  }

  .markdown :global(p) {
    margin: 8px 0;
    text-wrap: pretty;
  }

  .markdown :global(h1),
  .markdown :global(h2) {
    margin: 24px 0 8px;
    color: var(--text);
    font-size: var(--text-lg);
    font-weight: 600;
    line-height: 24px;
  }

  .markdown :global(h3),
  .markdown :global(h4),
  .markdown :global(h5),
  .markdown :global(h6) {
    margin: 16px 0 4px;
    color: var(--text);
    font-size: var(--text-md);
    font-weight: 600;
    line-height: 20px;
  }

  .markdown :global(strong) {
    color: var(--strong);
    font-weight: 600;
  }

  .markdown :global(a) {
    color: var(--link);
    text-decoration: underline;
    text-underline-offset: 3px;
  }

  .markdown :global(ul),
  .markdown :global(ol) {
    margin: 8px 0;
    padding-left: 20px;
  }

  .markdown :global(li > ul),
  .markdown :global(li > ol) {
    margin: 0;
  }

  .markdown :global(li > p) {
    margin: 0;
  }

  .markdown :global(li > p + p) {
    margin-top: 8px;
  }

  .markdown :global(code),
  .markdown :global(pre) {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
  }

  .markdown :global(code) {
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface);
  }

  .markdown :global(pre) {
    margin: 8px 0;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    line-height: 20px;
    overflow-x: auto;
    overflow-wrap: normal;
    tab-size: 4;
  }

  .markdown :global(pre code) {
    padding: 0;
    border: 0;
    border-radius: 0;
    background: none;
  }

  .markdown :global(blockquote) {
    margin: 8px 0;
    padding-left: 12px;
    border-left: 1px solid var(--border);
    color: var(--secondary);
  }

  .markdown :global(table) {
    display: block;
    max-width: 100%;
    margin: 8px 0;
    border-collapse: collapse;
    overflow-x: auto;
    overflow-wrap: normal;
  }

  .markdown :global(th),
  .markdown :global(td) {
    padding: 8px;
    border: 1px solid var(--border);
    text-align: left;
    vertical-align: top;
  }

  .markdown :global(th) {
    font-weight: 600;
  }

  .markdown :global(hr) {
    height: 0;
    margin: 16px 0;
    border: 0;
    border-top: 1px solid var(--border);
  }

  .markdown :global(img) {
    max-width: 100%;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
</style>
