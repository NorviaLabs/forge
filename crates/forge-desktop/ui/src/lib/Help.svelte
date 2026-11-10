<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";
  let { onClose, onProviders, onGithub, themes = [], theme = $bindable("forge-dark"), sound = $bindable(true) } = $props();
  const shortcuts = [
    ["⌘ / Ctrl K", "Command palette: files, sessions and actions"],
    ["Enter / ⌘ ↵", "Send prompt (Shift+Enter for a new line)"],
    ["↑ / ↓", "Cycle this session's prompt history at the first/last line"],
    ["Esc", "Close the open overlay; leave Files or Terminal when no editor, field or terminal has focus; restore your draft when browsing prompt history"],
    ["⌘ / Ctrl E", "Open Files, or return to the session"],
    ["/ then Tab", "Complete a command or installed skill"],
    ["⌘ / Ctrl S", "Save current file"],
    ["⌘ / Ctrl W", "Close the current document in Files"],
    ["⌘ M", "Choose model and reasoning effort"],
    ["⌘ J / Ctrl `", "Show or hide the terminal drawer; leave the Terminal screen"],
    ["⌘ / Ctrl F", "Find in current editor"],
    ["⌘ / Ctrl G", "Next match (Shift for previous)"],
    ["⌘ / Ctrl L", "Jump to line in current editor"],
    ["⌘ / Ctrl V", "Paste image into composer (image-capable models)"],
  ];
</script>

<div class="overlay">
  <div class="help" role="dialog" aria-modal="true" aria-labelledby="help-title" tabindex="-1" use:modal={{ onClose }}>
    <header><h2 id="help-title">Help & appearance</h2><button class="icon-btn" type="button" aria-label="Close help" onclick={onClose}><Icon name="close" size={13} /></button></header>
    <label>Theme <select class="field" bind:value={theme}><option value="system">System</option>{#each themes as entry}<option value={entry.id}>{entry.name}</option>{/each}</select></label>
    <label class="toggle"><input type="checkbox" bind:checked={sound} /> Play a sound when a request waits on you</label>
    <h3>Keyboard shortcuts</h3>
    <dl>{#each shortcuts as [keys, action]}<dt><kbd>{keys}</kbd></dt><dd>{action}</dd>{/each}</dl>
    <h3>Workspace workflow</h3>
    <p>The rail lists your projects at the top: a dot shows whether each is closed (ring), idle, working or waiting on you, and requests waiting in other projects appear under their name. Below are the sessions of the project on screen; those waiting on you appear under Needs you. The session header opens Files, Terminal and the Session menu, which holds the plan, agents and tasks, the queue, context and usage, the model, and session actions.</p>
    <p>Files is where you browse, edit, stage and commit: scope the list to Changed, Staged or Last turn, stage a row or Stage all, then Commit. ‹ Session or Esc returns to the conversation with your place kept.</p>
    <p>The editor has no buttons. Save, find and jump using shortcuts or the command palette. Reload and restore are palette actions; discarding changes still requires confirmation.</p>
    <p>Shells run your own commands without the agent sandbox. Each session has one shell: the terminal button under the composer shows it as a drawer, Focus terminal fills the window, and hiding it or switching sessions keeps it running. Stop terminal or <code>exit</code> ends it; the next open starts a fresh shell.</p>
    <p>Image paste supports PNG, JPEG, GIF and WebP, up to four files of 5 MiB each. Images are saved under workspace <code>.forge/attachments</code>.</p>
    <div class="actions"><button class="btn" type="button" onclick={onProviders}>Providers…</button><button class="btn" type="button" disabled={!onGithub} title={onGithub ? undefined : "GitHub needs a Git repository"} onclick={onGithub}>GitHub…</button></div>
  </div>
</div>

<style>
  .toggle { display: flex; align-items: center; gap: 6px; margin-top: 8px; }
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
