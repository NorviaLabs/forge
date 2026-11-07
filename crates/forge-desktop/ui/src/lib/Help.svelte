<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";
  let { onClose, onProviders, onGithub, theme = $bindable("dark") } = $props();
  const shortcuts = [
    ["⌘ / Ctrl K", "Command palette: files, sessions and actions"],
    ["⌘ / Ctrl Enter", "Send prompt (in composer)"],
    ["⌘ / Ctrl S", "Save current file"],
    ["⌘ / Ctrl W", "Close current file or terminal tab"],
    ["⌘ M", "Choose model and reasoning effort"],
    ["⌘ J / Ctrl `", "Show or hide terminal"],
    ["⌘ / Ctrl F", "Find in current editor"],
    ["⌘ / Ctrl G", "Next match (Shift for previous)"],
    ["⌘ / Ctrl L", "Jump to line in current editor"],
    ["⌘ / Ctrl V", "Paste image into composer (image-capable models)"],
  ];
</script>

<div class="overlay">
  <div class="help" role="dialog" aria-modal="true" aria-labelledby="help-title" tabindex="-1" use:modal={{ onClose }}>
    <header><h2 id="help-title">Help & appearance</h2><button class="icon-btn" type="button" aria-label="Close help" onclick={onClose}><Icon name="close" size={13} /></button></header>
    <label>Theme <select class="field" bind:value={theme}><option value="dark">Forge Dark</option><option value="light">Forge Light</option></select></label>
    <h3>Keyboard shortcuts</h3>
    <dl>{#each shortcuts as [keys, action]}<dt><kbd>{keys}</kbd></dt><dd>{action}</dd>{/each}</dl>
    <h3>Workspace workflow</h3>
    <p>Use the sidebar for sessions, file operations and content search. Use the inspector for approvals, thinking, plans, tasks, queue and staged changes.</p>
    <p>The editor has no buttons. Save, find and jump using shortcuts or the command palette. Reload and restore are palette actions; discarding changes still requires confirmation.</p>
    <p>Shells run your own commands without the agent sandbox. Switching sessions preserves running shells; closing a terminal tab ends its shell.</p>
    <p>Image paste supports PNG, JPEG, GIF and WebP, up to four files of 5 MiB each. Images are saved under workspace <code>.forge/attachments</code>.</p>
    <div class="actions"><button class="btn" type="button" onclick={onProviders}>Providers…</button><button class="btn" type="button" onclick={onGithub}>GitHub…</button></div>
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; display: grid; place-items: center; background: rgba(5, 7, 9, 0.6); z-index: 30; }
  .help { width: min(42rem, calc(100vw - 48px)); max-height: calc(100vh - 64px); overflow: auto; padding: 18px; border: 1px solid var(--border-strong); border-radius: var(--radius); background: var(--surface); box-shadow: var(--shadow); }
  header { display: flex; justify-content: space-between; align-items: center; }
  h2 { margin: 0; font-size: 15px; }
  h3 { margin: 20px 0 8px; font-size: 13px; }
  label { display: flex; align-items: center; gap: 12px; margin-top: 16px; font-size: 12px; }
  select { width: 160px; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 8px 16px; font-size: 12px; }
  dd { margin: 0; }
  dt { color: var(--accent); }
  p, dd { color: var(--secondary); line-height: 1.55; font-size: 12px; }
  .actions { display: flex; gap: 6px; }
</style>
