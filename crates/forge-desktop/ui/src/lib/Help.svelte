<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";
  let { onClose, onSettings } = $props();
  import { chord } from "../keys.js";
  const shortcuts = [
    [chord("K"), "Command palette: files, sessions and actions"],
    ["Enter / ⌘ ↵", "Send prompt (Shift+Enter for a new line)"],
    ["↑ / ↓", "Cycle this session's prompt history at the first/last line"],
    ["Esc", "Close the open overlay; leave Files or Terminal when no editor, field or terminal has focus; restore your draft when browsing prompt history"],
    [chord("E"), "Open Files, or return to the session"],
    [chord("A", { shift: true }), "Take the first allow option on the pending approval"],
    [chord("D", { shift: true }), "Deny the pending approval"],
    ["/ then Tab", "Complete a command or installed skill"],
    [chord("S"), "Save current file"],
    [chord("W"), "Close the current document in Files"],
    ["⌘ M", "Choose model and reasoning effort"],
    ["⌘ J / Ctrl `", "Show or hide the terminal drawer; leave the Terminal screen"],
    [chord("F"), "Find in current editor"],
    [chord("G"), "Next match (Shift for previous)"],
    [chord("L"), "Jump to line in current editor"],
    [chord("V"), "Paste image into composer (image-capable models)"],
  ];
</script>

<div class="overlay">
  <div class="help" role="dialog" aria-modal="true" aria-labelledby="help-title" tabindex="-1" use:modal={{ onClose }}>
    <header><h2 id="help-title">Help</h2><button class="icon-btn" type="button" aria-label="Close help" onclick={onClose}><Icon name="close" size={13} /></button></header>
    <div class="body">
    <h3>Keyboard shortcuts</h3>
    <dl>{#each shortcuts as [keys, action]}<dt><kbd>{keys}</kbd></dt><dd>{action}</dd>{/each}</dl>
    <h3>Workspace workflow</h3>
    <p>The rail lists your projects at the top: a dot shows whether each is closed (ring), idle, working or waiting on you, and requests waiting in other projects appear under their name. Below are the sessions of the project on screen; those waiting on you appear under Needs you. The top bar opens Files, the terminal, a new session and the Session panel, which holds the plan, agents and tasks, the queue, context and usage, the model, and session actions.</p>
    <p>Files is where you browse, edit, stage and commit: scope the list to Changed, Staged or Last turn, stage a row or Stage all, then Commit. ‹ Session or Esc returns to the conversation with your place kept.</p>
    <p>The editor has no buttons. Save, find and jump using shortcuts or the command palette. Reload and restore are palette actions; discarding changes still requires confirmation.</p>
    <p>Shells run your own commands without the agent sandbox. Each session has one shell: Terminal in the top bar shows it as a drawer under the conversation, Focus terminal fills the window, and hiding it or switching sessions keeps it running. Stop terminal or <code>exit</code> ends it; the next open starts a fresh shell.</p>
    <p>Image paste supports PNG, JPEG, GIF and WebP, up to four files of 5 MiB each. Images are saved under workspace <code>.forge/attachments</code>.</p>
    </div>
    <footer class="actions"><span class="foot-note">Theme, sound and providers are in Settings.</span><button class="link" type="button" onclick={onSettings}>Open Settings</button></footer>
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

  .help {
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
    padding: 8px 16px 16px;
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

  dl {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: baseline;
    gap: 8px 16px;
    margin: 0;
    font-size: var(--text-sm);
  }

  dt {
    white-space: nowrap;
  }

  dd {
    margin: 0;
  }

  kbd {
    display: inline-block;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    color: var(--secondary);
    font-size: var(--text-xs);
    line-height: 18px;
  }

  p,
  dd {
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 18px;
  }

  p {
    margin: 0 0 8px;
  }

  code {
    font-size: var(--text-sm);
  }

  .actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-top: 1px solid var(--border);
  }

  .foot-note {
    margin-right: auto;
    color: var(--muted);
    font-size: var(--text-sm);
  }
</style>
