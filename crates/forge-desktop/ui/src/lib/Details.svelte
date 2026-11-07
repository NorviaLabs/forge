<script>
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";
  let { onClose } = $props();
  let details = $state(null);
  let loading = $state(false);
  let error = $state("");
  async function refresh() {
    loading = true;
    error = "";
    try { details = await invoke("session_details"); }
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
    {#if loading}<p role="status">Loading status and pricing…</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if details}
      <h3>Tokens</h3>
      <p>{details.tokens.prompt.toLocaleString()} input · {details.tokens.completion.toLocaleString()} output · {details.tokens.model_steps} model steps · {details.tokens.message_count} messages</p>
      <p>Cache: {details.tokens.cache_hits.toLocaleString()} read · {details.tokens.cache_writes.toLocaleString()} written</p>
      <h3>Context breakdown <small>(estimated tokens)</small></h3>
      <dl>{#each Object.entries(details.context) as [label, count]}<dt>{label}</dt><dd>{count.toLocaleString()}</dd>{/each}</dl>
      <h3>Cost / provider usage</h3>
      {#each details.cost as line}<p>{line}</p>{/each}
      <p>Estimates use current model rates, not a bill. Model switches, cache discounts and provider billing can change actual cost.</p>
      <h3>Available tools ({details.tools.length})</h3>
      <ul>{#each details.tools as tool}<li class="mono">{tool}</li>{/each}</ul>
      <h3>Loaded skills ({details.skills.length})</h3>
      <dl>{#each details.skills as [name, description]}<dt>{name}</dt><dd>{description}</dd>{/each}</dl>
      {#if !details.skills.length}<p>No skills loaded.</p>{/if}
    {/if}
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; display: grid; place-items: center; background: rgba(5, 7, 9, 0.6); z-index: 30; }
  .details { width: min(40rem, calc(100vw - 48px)); max-height: calc(100vh - 64px); overflow: auto; padding: 18px; border: 1px solid var(--border-strong); border-radius: var(--radius); background: var(--surface); box-shadow: var(--shadow); }
  header { display: flex; align-items: center; gap: 6px; }
  h2 { flex: 1; margin: 0; font-size: 15px; }
  h3 { margin: 20px 0 8px; font-size: 13px; }
  small { font-weight: 400; }
  p, dl, ul { font-size: 12px; color: var(--secondary); line-height: 1.55; }
  p { margin: 6px 0; }
  dl { display: grid; grid-template-columns: minmax(80px, 1fr) 2fr; gap: 4px 12px; }
  dd { margin: 0; overflow-wrap: anywhere; }
  ul { columns: 2; padding-left: 18px; }
  .error { color: var(--error); }
</style>
