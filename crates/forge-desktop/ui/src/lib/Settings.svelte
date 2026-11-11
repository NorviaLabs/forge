<script>
  // App settings: how Forge looks and sounds, and the two connections it
  // keeps (model providers and GitHub). Help holds only help.
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let { onClose, onProviders, onGithub, themes = [], theme = $bindable("forge-dark"), sound = $bindable(true) } = $props();
</script>

<div class="overlay">
  <div class="settings" role="dialog" aria-modal="true" aria-labelledby="settings-title" tabindex="-1" use:modal={{ onClose }}>
    <header>
      <h2 id="settings-title">Settings</h2>
      <button class="icon-btn" type="button" aria-label="Close settings" onclick={onClose}><Icon name="close" size={13} /></button>
    </header>
    <div class="body">
      <h3>Appearance</h3>
      <label>
        <span>Theme</span>
        <select class="field" bind:value={theme}>
          <option value="system">System</option>
          {#each themes as entry}<option value={entry.id}>{entry.name}</option>{/each}
        </select>
      </label>

      <h3>Notifications</h3>
      <label>
        <span>Play a sound when a session needs you</span>
        <input type="checkbox" bind:checked={sound} />
      </label>

      <h3>Connections</h3>
      <div class="row">
        <span class="what">
          <span>Model providers</span>
          <span class="note">Sign in, add a key, or add a custom provider.</span>
        </span>
        <button class="link" type="button" onclick={onProviders}>Manage</button>
      </div>
      <div class="row">
        <span class="what">
          <span>GitHub</span>
          <span class="note">{onGithub ? "Issues, pull requests and checks for this repository." : "Needs a Git repository: this project is a plain folder."}</span>
        </span>
        <button class="link" type="button" disabled={!onGithub} onclick={onGithub}>Open</button>
      </div>
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

  .settings {
    display: flex;
    flex-direction: column;
    width: min(520px, calc(100vw - 48px));
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
    justify-content: space-between;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
  }

  h2 {
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

  h3 {
    margin: 16px 0 4px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
  }

  /* One setting per row: what it is at the left, its control at the right. */
  label,
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: var(--bar);
    padding: 4px 0;
    font-size: var(--text-md);
  }

  .what {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .note {
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 18px;
  }

  select {
    width: 200px;
    height: var(--control);
    flex: none;
    padding: 0 8px;
    font-size: var(--text-sm);
  }

  input[type="checkbox"] {
    width: 16px;
    height: 16px;
    flex: none;
    margin: 0;
    accent-color: var(--accent);
  }
</style>
