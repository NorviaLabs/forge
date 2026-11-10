<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let { workspace = "", error = "", busy = false, needsFolder = false, needsConfirmation = false, noGit = false, projects = [], onOpenProject, onTrust, onChoose, onQuit } = $props();
</script>

<div class="trust" role="dialog" aria-modal="true" aria-labelledby="trust-title" tabindex="-1" use:modal={{ busy }}>
  <div class="card">
    {#if needsFolder}
    <h1 id="trust-title">Open a folder</h1>
    <p>
      {projects.length ? "Open one of your projects, or choose another folder." : "Choose the project folder to work in."}
      It can be a Git repository or any other folder. Nothing is opened or written until you choose.
    </p>
    {#if projects.length}
      <ul class="projects" aria-label="Your projects">
        {#each projects as project (project.id)}
          <li>
            <button type="button" class="project" disabled={busy} title={project.root} onclick={() => onOpenProject?.(project)}>
              <span class="name">{project.name}</span>
              <span class="root mono">{project.root}</span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="actions">
      <button type="button" class="btn primary" disabled={busy} onclick={onChoose}>
        {busy ? "Opening…" : projects.length ? "Choose another folder…" : "Choose a folder…"}
      </button>
      <button type="button" class="btn" onclick={onQuit}>Quit</button>
    </div>
    {:else}
    <span class="badge"><Icon name="alert" size={14} /> {needsConfirmation ? "New project" : "Untrusted workspace"}</span>
    <h1 id="trust-title">{needsConfirmation ? "Open this folder as a project?" : "Trust this folder?"}</h1>
    <p class="path mono">{workspace}</p>
    {#if needsConfirmation}
    <p>
      This folder isn't a project yet. Adding it lets agents read and write everything inside it and
      run commands through approvals, and lists it in your projects. Nothing is written into the
      folder until you add it.
    </p>
    {:else}
    <p>
      Forge has never run here. Trusting it lets the agent read and write files in this folder and
      run commands through approvals. Nothing is written until you trust it. Commands requiring
      approval still ask; trusting a folder does not enable approve-all mode.
    </p>
    {/if}
    <p class="dim small">
      Trusting here is the same as trusting in the terminal: it is remembered for this folder and
      its subfolders.
    </p>
    {#if noGit}<p class="warn">No Git: agent edits can't be rolled back.</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="actions">
      <button type="button" class="btn primary" disabled={busy} onclick={onTrust}>
        {busy ? "Opening…" : needsConfirmation ? "Add as a project and open it" : "Trust this folder and open it"}
      </button>
      <button type="button" class="btn" onclick={onQuit}>Quit</button>
    </div>
    {/if}
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

  .warn {
    color: var(--warning);
  }

  .projects {
    margin: 0 0 4px;
    padding: 0;
    list-style: none;
  }

  .project {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    width: 100%;
    padding: 7px 9px;
    border: 1px solid var(--border);
    border-radius: 7px;
    margin-bottom: 6px;
    background: transparent;
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .project:hover:not(:disabled) {
    border-color: var(--border-strong);
  }

  .project .name {
    font-size: 13px;
    font-weight: 600;
  }

  .project .root {
    overflow: hidden;
    max-width: 100%;
    color: var(--muted);
    font-size: 11px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .actions {
    display: flex;
    gap: 6px;
    margin-top: 12px;
  }
</style>
