<script>
  import { onMount } from "svelte";
  import { invoke } from "../ipc.js";
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";
  let { sessionId, onClose } = $props();
  let details = $state(null);
  let loading = $state(false);
  let error = $state("");
  async function refresh() {
    loading = true;
    error = "";
    try { details = await invoke("session_details", { sessionId }); }
    catch (cause) { error = String(cause); }
    finally { loading = false; }
  }
  onMount(() => { refresh(); });
</script>

<div class="overlay">
  <div class="details" role="dialog" aria-modal="true" aria-labelledby="status-title" tabindex="-1" use:modal={{ onClose }}>
    <header><h2 id="status-title">Session status</h2>
      <button class="btn" type="button" disabled={loading} onclick={refresh}>Refresh</button>
      <button class="icon-btn" type="button" aria-label="Close status" onclick={onClose}><Icon name="close" size={13} /></button>
    </header>
    <div class="body">
    {#if loading}<p role="status">Loading status and pricing…</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if details}
      <h3>Tokens</h3>
      <dl>
        <dt>Input</dt><dd>{details.tokens.prompt.toLocaleString()}</dd>
        <dt>Output</dt><dd>{details.tokens.completion.toLocaleString()}</dd>
        <dt>Model steps</dt><dd>{details.tokens.model_steps}</dd>
        <dt>Messages</dt><dd>{details.tokens.message_count}</dd>
        <dt>Cache read</dt><dd>{details.tokens.cache_hits.toLocaleString()}</dd>
        <dt>Cache written</dt><dd>{details.tokens.cache_writes.toLocaleString()}</dd>
      </dl>
      <h3>Context breakdown <small>(estimated tokens)</small></h3>
      <dl>{#each Object.entries(details.context) as [label, count]}<dt>{label}</dt><dd>{count.toLocaleString()}</dd>{/each}</dl>
      <h3>Cost / provider usage</h3>
      {#each details.cost as line}<p class="mono">{line}</p>{/each}
      <p>Estimates use current model rates, not a bill. Model switches, cache discounts and provider billing can change actual cost.</p>
      <h3>Available tools ({details.tools.length})</h3>
      <ul>{#each details.tools as tool}<li class="mono">{tool}</li>{/each}</ul>
      <h3>Loaded skills ({details.skills.length})</h3>
      <dl class="skills">{#each details.skills as [name, description]}<dt>{name}</dt><dd>{description}</dd>{/each}</dl>
      {#if !details.skills.length}<p>No skills loaded.</p>{/if}
    {/if}
    </div>
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

  .details {
    display: flex;
    flex-direction: column;
    width: min(640px, calc(100vw - 48px));
    max-height: calc(100vh - 64px);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
  }

  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .body {
    flex: 1;
    min-height: 0;
    padding: 0 16px 16px;
    overflow: auto;
  }

  /* Section labels. */
  h3 {
    margin: 16px 0 8px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
    letter-spacing: 0;
  }

  small {
    font-size: inherit;
    font-weight: 400;
  }

  p {
    margin: 8px 0;
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 18px;
    overflow-wrap: anywhere;
  }

  .body > p:first-child {
    margin-top: 16px;
  }

  /* Label at left, value in mono at right. */
  dl {
    display: grid;
    grid-template-columns: minmax(120px, 1fr) 2fr;
    gap: 4px 16px;
    margin: 0;
    font-size: var(--text-sm);
    line-height: 20px;
  }

  dt {
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  dd {
    margin: 0;
    color: var(--text);
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
  }

  /* Skills: the name is the datum, the description is prose. */
  dl.skills dt {
    color: var(--text);
    font-family: var(--font-mono);
  }

  dl.skills dd {
    color: var(--secondary);
    font-family: var(--font-ui);
  }

  ul {
    columns: 2;
    column-gap: 16px;
    margin: 0;
    padding: 0;
    color: var(--secondary);
    line-height: 20px;
    list-style: none;
  }

  li {
    overflow-wrap: anywhere;
  }

  .error {
    color: var(--error);
  }
</style>
