<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let { workspace = "", error = "", busy = false, onTrust, onQuit } = $props();
</script>

<div class="trust" role="dialog" aria-modal="true" aria-labelledby="trust-title" tabindex="-1" use:modal={{ onClose: onQuit, busy }}>
  <div class="card">
    <span class="badge"><Icon name="alert" size={14} /> Untrusted workspace</span>
    <h1 id="trust-title">Trust this folder?</h1>
    <p class="path mono">{workspace}</p>
    <p>
      Forge has never run here. Trusting it lets the agent read and write files in this folder and
      run commands through approvals. Nothing is written until you trust it. Commands requiring
      approval still ask; trusting a folder does not enable approve-all mode.
    </p>
    <p class="dim small">
      Trusting here is the same as trusting in the terminal: it is remembered for this folder and
      its subfolders.
    </p>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="actions">
      <button type="button" class="btn primary" disabled={busy} onclick={onTrust}>
        {busy ? "Trusting…" : "Trust this folder and open it"}
      </button>
      <button type="button" class="btn" onclick={onQuit}>Quit</button>
    </div>
  </div>
</div>

<style>
  .trust {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--bg-deep);
    z-index: 40;
  }

  .card {
    width: min(32rem, 100%);
    padding: 20px 22px;
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--warning);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  h1 {
    margin: 8px 0 6px;
    font-size: 18px;
    font-weight: 600;
  }

  p {
    margin: 0 0 10px;
    color: var(--secondary);
    font-size: 12.5px;
    line-height: 1.55;
  }

  .path {
    overflow-wrap: anywhere;
    color: var(--text);
    font-size: 12px;
  }

  .dim {
    color: var(--muted);
  }

  .small {
    font-size: 11.5px;
  }

  .error {
    color: var(--error);
  }

  .actions {
    display: flex;
    gap: 6px;
    margin-top: 12px;
  }
</style>
