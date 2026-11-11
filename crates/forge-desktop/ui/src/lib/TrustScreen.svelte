<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let { workspace = "", error = "", busy = false, needsFolder = false, needsConfirmation = false, noGit = false, projects = [], onOpenProject, onTrust, onChoose, onQuit } = $props();
</script>

<div class="trust" role="dialog" aria-modal="true" aria-labelledby="trust-title" tabindex="-1" use:modal={{ busy }}>
  <div class="card">
    <p class="wordmark">Forge</p>
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
      <button type="button" class="btn" class:primary={!projects.length} disabled={busy} onclick={onChoose}>
        {busy ? "Opening…" : projects.length ? "Choose another folder…" : "Choose a folder…"}
      </button>
      <button type="button" class="btn ghost" onclick={onQuit}>Quit</button>
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
      <button type="button" class="btn ghost" onclick={onQuit}>Quit</button>
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
    overflow: auto;
    background: var(--bg);
    z-index: 40;
  }

  /* A plain centred column: no surface, border or shadow. */
  .card {
    width: min(480px, 100%);
  }

  .wordmark {
    margin: 0 0 24px;
    color: var(--muted);
    font-size: var(--text-lg);
    font-weight: 600;
    line-height: 24px;
  }

  .badge {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
    color: var(--warning);
    font-size: var(--text-sm);
    font-weight: 500;
  }

  h1 {
    margin: 0 0 16px;
    font-size: var(--text-lg);
    font-weight: 600;
    line-height: 28px;
  }

  p {
    margin: 0 0 12px;
    color: var(--secondary);
    font-size: var(--text-md);
    line-height: 20px;
  }

  .path {
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-deep);
    color: var(--text);
    font-size: var(--text-sm);
    line-height: 20px;
    overflow-wrap: anywhere;
  }

  .dim {
    color: var(--muted);
  }

  .small {
    font-size: var(--text-sm);
    line-height: 18px;
  }

  .error {
    color: var(--error);
    overflow-wrap: anywhere;
  }

  .warn {
    color: var(--warning);
  }

  .projects {
    margin: 0 -8px 12px;
    padding: 0;
    list-style: none;
  }

  .project {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: baseline;
    gap: 12px;
    width: 100%;
    height: var(--control-lg);
    padding: 0 8px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .project:hover:not(:disabled) {
    background: var(--hover);
  }

  .project:focus-visible {
    outline-offset: -2px;
  }

  .project .name {
    font-size: var(--text-md);
    font-weight: 500;
    white-space: nowrap;
  }

  .project .root {
    overflow: hidden;
    color: var(--muted);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 24px;
  }

  @media (prefers-reduced-motion: reduce) {
    .project {
      transition: none;
    }
  }
</style>
