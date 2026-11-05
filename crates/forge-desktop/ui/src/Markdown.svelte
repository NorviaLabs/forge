<script>
  // The ONLY place in the UI that inserts HTML. A config test fails if any
  // other file uses Svelte's raw-HTML tag.
  import { renderMarkdown } from "./markdown.js";
  let { text } = $props();
  const html = $derived(renderMarkdown(text));
</script>

<div class="markdown">{@html html}</div>
