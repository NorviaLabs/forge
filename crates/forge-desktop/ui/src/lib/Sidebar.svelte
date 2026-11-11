<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";
  import { attentionLabel, projectStatus } from "../view.js";

  let {
    sessions = [],
    activeId = "",
    // Every registered project (the bridge's `ProjectView`), in registry
    // order, and the one on screen.
    projects = [],
    projectId = "",
    projectBusy = false,
    onSwitchProject,
    onAddProject,
    onCloseProject,
    onRemoveProject,
    onElsewhere,
    creating = false,
    open = false,
    attention = [],
    onSelect,
    onRequest,
    onCreate,
    onAttach,
    onArchive,
    onRename,
    onPin,
    onStop,
    onClose,
    onResume,
    onHelp,
    onCloseRail,
  } = $props();

  let newLabel = $state("");
  let composing = $state(false);
  let search = $state("");

  const needle = $derived(search.trim().toLowerCase());
  const matches = (session) => !needle || session.label.toLowerCase().includes(needle);
  const live = $derived(sessions.filter((session) => session.lifecycle === "active"));
  const closed = $derived(sessions.filter((session) => session.lifecycle !== "active"));
  const shownLive = $derived(live.filter(matches));
  const shownClosed = $derived(closed.filter(matches));
  // Roster order, never re-sorted: acting on a decision must not move rows.
  const waiting = $derived(sessions.filter((session) => attention.includes(session.session_id)));

  // At compact width the open rail is a modal overlay: it traps focus, closes
  // on Escape, and returns focus to the header control that opened it.
  function overlay(node) {
    const action = modal(node, { onClose: () => onCloseRail?.() });
    return () => action.destroy();
  }

  // Exposed to the command palette.
  export function startNewSession() {
    composing = true;
    newLabel = "";
  }

  function submitNew(event) {
    event.preventDefault();
    const label = newLabel.trim();
    if (!label || creating) return;
    onCreate(label);
    newLabel = "";
    composing = false;
  }
</script>

{#if open}
  <button type="button" class="rail-backdrop" aria-label="Close sessions" tabindex="-1" onclick={() => onCloseRail?.()}></button>
{/if}
<aside class="sidebar" class:open aria-label="Projects and sessions" {@attach open ? overlay : null}>
  <header class="workspace">
    <span class="mark" aria-hidden="true">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><path d="M3 6h13.5a4.5 4.5 0 0 1-4 4.5V13H15a2 2 0 0 1 2 2v2h2v2H5v-2h2v-2a2 2 0 0 1 2-2h1.5v-2.5A4.5 4.5 0 0 1 3 6Z"/><path d="M19.5 3.5 21 5.5l-2 .8.4-2.8Z"/></svg>
    </span>
    <strong class="brand">Forge</strong>
    {#if open}
      <button class="icon-btn" type="button" aria-label="Close sessions" onclick={() => onCloseRail?.()}>
        <Icon name="close" size={13} />
      </button>
    {/if}
  </header>

  <section class="panel projects" aria-label="Projects">
    <div class="section-head">
      <span class="label">Projects</span>
      <button class="icon-btn" type="button" title="Add a project" aria-label="Add a project" onclick={() => onAddProject?.()}>
        <Icon name="plus" size={14} />
      </button>
    </div>
    <ul class="project-list">
      {#each projects as project (project.id)}
        {@const status = projectStatus(project)}
        {@const current = project.id === projectId}
        <li class="project {status}" class:current>
          <div class="project-line">
            <button
              type="button"
              class="project-main"
              title={project.root}
              aria-current={current ? "true" : undefined}
              disabled={projectBusy && !current}
              onclick={() => onSwitchProject?.(project)}
            >
              <span class="project-dot" aria-hidden="true"></span>
              <span class="project-name">{project.name}</span>
              {#if status === "waiting"}
                <span class="project-badge" aria-label="{project.attention.length} waiting on you">{project.attention.length}</span>
              {:else if status === "running"}
                <span class="project-meta" aria-label="{project.running} of {project.limit} agents running">{project.running}/{project.limit}</span>
              {:else if status === "closed"}
                <span class="project-meta">closed</span>
              {/if}
            </button>
            {#if !current}
              <span class="row-actions">
                {#if project.open}
                  <button type="button" class="icon-btn" title="Close project (stops its agents)" aria-label={`Close ${project.name}`} onclick={() => onCloseProject?.(project)}>
                    <Icon name="close" size={12} />
                  </button>
                {:else}
                  <button type="button" class="icon-btn" title="Remove from Forge (the folder and its sessions stay)" aria-label={`Remove ${project.name}`} onclick={() => onRemoveProject?.(project)}>
                    <Icon name="trash" size={12} />
                  </button>
                {/if}
              </span>
            {/if}
          </div>
          {#if !current && project.attention.length}
            <ul class="requests" aria-label="Waiting in {project.name}">
              {#each project.attention as item (item.session_id + item.call_id)}
                <li>
                  <button
                    type="button"
                    class="request"
                    title="{project.name}: {item.session_label || 'Session'}"
                    onclick={() => onElsewhere?.({ ...item, project_id: project.id, project_name: project.name })}
                  >
                    <span class="request-kind">{attentionLabel(item.kind)}</span>
                    <span class="request-session">{item.session_label || "Session"}</span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </li>
      {/each}
    </ul>
  </section>

  {#if waiting.length > 0}
    <section class="panel needs-section" aria-label="Needs you">
      <div class="section-head">
        <span class="label">Needs you</span>
        <span class="count">{waiting.length}</span>
      </div>
      <ul class="session-list">
        {#each waiting as session (session.session_id)}
          <li class="needs-you">
            <button type="button" class="session" onclick={() => onRequest(session.session_id)}>
              <span class="state-dot {session.turn_state}"></span>
              <span class="session-name">{session.label}</span>
              <span class="session-state">{session.turn_state}</span>
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <section class="panel sessions">
    <div class="section-head">
      <span class="label">Sessions</span>
      <span class="head-actions">
        <button
          class="icon-btn"
          type="button"
          title="Attach an existing worktree"
          aria-label="Attach an existing worktree"
          onclick={onAttach}
        >
          <Icon name="layers" size={14} />
        </button>
      </span>
    </div>

    <div class="filter">
      <Icon name="search" size={12} />
      <input bind:value={search} placeholder="Search sessions" aria-label="Search sessions" spellcheck="false" />
    </div>

    {#if composing}
      <form class="new" onsubmit={submitNew}>
        <input
          class="field"
          bind:value={newLabel}
          placeholder="Session name"
          aria-label="New session name"
        />
        <button class="btn primary" type="submit" disabled={creating || !newLabel.trim()}>
          {creating ? "Creating…" : "Create"}
        </button>
      </form>
    {/if}

    <ul class="session-list">
      {#if creating}
        <li class="creating">
          <span class="mini spin"><Icon name="refresh" size={12} /></span>
          <span>Creating session…</span>
        </li>
      {/if}
      {#each shownLive as session (session.session_id)}
        <li
          class:active={session.session_id === activeId}
          class:needs-you={attention.includes(session.session_id)}
        >
          <button
            type="button"
            class="session"
            onclick={() => onSelect(session.session_id)}
            data-session-id={session.session_id}
          >
            <span class="state-dot {session.turn_state}"></span>
            {#if session.slot}<span class="slot" title="Pinned">{session.slot}</span>{/if}
            <span class="session-name">{session.label}</span>
            <span class="session-state">
              {attention.includes(session.session_id) ? "needs you" : session.turn_state}
            </span>
          </button>
          <span class="row-actions">
            {#if ["running", "waiting"].includes(session.turn_state)}
              <button
                type="button"
                class="icon-btn"
                title="Stop this session's turn"
                aria-label={`Stop ${session.label}`}
                onclick={() => onStop(session)}
              >
                <Icon name="stop" size={12} />
              </button>
            {/if}
            <button
              type="button"
              class="icon-btn"
              title={session.slot ? "Unpin session" : "Pin session"}
              aria-label={session.slot ? `Unpin ${session.label}` : `Pin ${session.label}`}
              onclick={() => onPin(session)}
            >
              <Icon name="pin" size={12} />
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Rename session"
              aria-label={`Rename ${session.label}`}
              onclick={() => onRename(session)}
            >
              <Icon name="pencil" size={12} />
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Stop managing this session (it stays resumable)"
              aria-label={`Close ${session.label}`}
              onclick={() => onClose(session)}
            >
              <Icon name="close" size={12} />
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Archive session"
              aria-label={`Archive ${session.label}`}
              onclick={() => onArchive(session)}
            >
              <Icon name="trash" size={12} />
            </button>
          </span>
        </li>
      {/each}
      {#if live.length === 0 && !creating}
        <li class="empty">No sessions yet.</li>
      {:else if needle && shownLive.length === 0 && shownClosed.length === 0}
        <li class="empty">No sessions match “{search.trim()}”.</li>
      {/if}
    </ul>

    {#if shownClosed.length > 0}
      <details class="closed" open={!!needle}>
        <summary>{shownClosed.length} closed session{shownClosed.length === 1 ? "" : "s"}</summary>
        <ul class="session-list">
          {#each shownClosed as session (session.session_id)}
            <li>
              <button
                type="button"
                class="session closed-row"
                title="Resume this session"
                onclick={() => onResume(session)}
              >
                <Icon name="play" size={11} />
                <span class="session-name">{session.label}</span>
                <span class="session-state">{session.lifecycle}</span>
              </button>
              {#if session.lifecycle === "closed"}
                <button
                  type="button"
                  class="icon-btn"
                  title="Archive session"
                  aria-label={`Archive ${session.label}`}
                  onclick={() => onArchive(session)}
                >
                  <Icon name="trash" size={12} />
                </button>
              {:else if session.ownership === "managed" && ["archived", "retained"].includes(session.lifecycle)}
                <button
                  type="button"
                  class="icon-btn"
                  title="Archive and remove this session's worktree"
                  aria-label={`Remove the worktree for ${session.label}`}
                  onclick={() => onArchive(session)}
                >
                  <Icon name="trash" size={12} />
                </button>
              {/if}
            </li>
          {/each}
        </ul>
      </details>
    {/if}
  </section>

  <footer class="rail-foot">
    <button class="btn" type="button" onclick={() => (composing = !composing)}>
      <Icon name="plus" size={13} />
      New session
    </button>
    <span class="spacer"></span>
    <button class="btn" type="button" onclick={onHelp}>Help</button>
  </footer>
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg-deep);
    border-right: 1px solid var(--border);
  }

  .workspace {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 12px 12px 10px;
    border-bottom: 1px solid var(--border);
  }

  .mark {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 8px;
    background: var(--accent);
    color: var(--accent-text);
    font-weight: 700;
    box-shadow: 0 1px 0 rgba(255, 255, 255, 0.25) inset, 0 2px 6px -2px rgba(0, 0, 0, 0.4);
  }

  .panel {
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 10px 8px 6px;
  }

  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 4px 6px;
  }

  .head-actions {
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .label {
    color: var(--muted);
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.02em;
  }

  .mini {
    display: inline-flex;
    color: var(--muted);
  }

  .new {
    display: flex;
    gap: 6px;
    padding: 0 4px 8px;
  }

  .new .field {
    flex: 1;
    min-width: 0;
  }

  .session-list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: auto;
    min-height: 0;
  }

  .session-list li {
    position: relative;
    display: flex;
    align-items: center;
    border-radius: var(--radius-sm);
  }

  .session-list li {
    margin-bottom: 1px;
  }

  .session-list li.active {
    background: var(--selection);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 22%, transparent);
  }

  .session {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    height: 34px;
    padding: 0 10px;
    border: 0;
    background: none;
    text-align: left;
    transition: background 120ms var(--ease);
  }

  .session:hover {
    background: var(--hover);
    border-radius: var(--radius-sm);
  }

  .session-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .session-state {
    color: var(--muted);
    font-size: 10px;
    letter-spacing: 0.03em;
  }

  .row-actions {
    display: flex;
    align-items: center;
    gap: 1px;
    margin-right: 3px;
    visibility: hidden;
    opacity: 0;
    pointer-events: none;
  }

  .session-list li.active:hover .row-actions,
  .session-list li.active:focus-within .row-actions {
    visibility: visible;
    opacity: 1;
  }

  .row-actions button {
    pointer-events: auto;
  }

  /* Only the selected row exposes overlaid actions: a running row's extra
     Stop button would otherwise steal the click intended to select it. */
  .session-list .row-actions {
    position: absolute;
    top: 50%;
    right: 3px;
    translate: 0 -50%;
    display: flex;
    align-items: center;
    gap: 1px;
    padding-left: 14px;
    background: linear-gradient(90deg, transparent, var(--hover) 14px);
    opacity: 0;
  }

  .session-list li.active .row-actions {
    background: linear-gradient(90deg, transparent, var(--selection) 14px);
  }

  .session-list li.active:hover .row-actions,
  .session-list li.active:focus-within .row-actions {
    opacity: 1;
  }

  li.needs-you .session-state {
    color: var(--warning);
    font-weight: 600;
  }

  .closed {
    margin: 4px 4px 0;
  }

  .closed summary {
    padding: 3px 4px;
    color: var(--muted);
    font-size: 11px;
    cursor: pointer;
  }

  .closed-row {
    gap: 6px;
    color: var(--muted);
    font-size: 12px;
  }

  .slot {
    display: inline-grid;
    place-items: center;
    min-width: 14px;
    height: 14px;
    border-radius: 4px;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: 9.5px;
    font-weight: 700;
  }

  .creating {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 32px;
    padding: 0 8px;
    color: var(--muted);
    font-size: 12px;
  }

  .filter {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 4px 6px;
    padding: 0 8px;
    height: 26px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--muted);
  }

  .filter input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    font-size: 12px;
  }

  .empty {
    padding: 6px 8px;
    color: var(--muted);
    font-size: 12px;
  }


  .brand {
    font-size: 13.5px;
    font-weight: 650;
    letter-spacing: -0.01em;
  }

  /* Projects: always visible, so a background project's requests and running
     agents are seen without opening anything. Capped so sessions keep room. */
  .projects {
    flex: none;
    max-height: 40%;
    border-bottom: 1px solid var(--border);
  }

  .projects .section-head {
    padding-bottom: 4px;
  }

  .project-list,
  .requests {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .project-list {
    overflow: auto;
    min-height: 0;
  }

  .project {
    border-radius: var(--radius-sm);
  }

  .project.current {
    background: var(--selection);
  }

  .project-line {
    position: relative;
    display: flex;
    align-items: center;
  }

  .project-main {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    height: 28px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--secondary);
    text-align: left;
  }

  .project-main:hover:not(:disabled) {
    background: var(--hover);
    color: var(--text);
  }

  .project-main:disabled {
    cursor: progress;
  }

  .project.current .project-main {
    color: var(--text);
    font-weight: 600;
  }

  .project-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Filled while open, a ring once closed; color follows what it needs. */
  .project-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
  }

  .project.running .project-dot {
    background: var(--accent);
  }

  .project.waiting .project-dot {
    background: var(--warning);
  }

  .project.closed .project-dot {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--muted);
  }

  .project.closed .project-main {
    color: var(--muted);
  }

  .project-meta {
    color: var(--muted);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
  }

  .project-badge {
    min-width: 17px;
    height: 17px;
    padding: 0 5px;
    border-radius: 9px;
    background: var(--warning);
    color: var(--accent-text);
    font-size: 10.5px;
    font-weight: 700;
    line-height: 17px;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }

  .project-line .row-actions {
    position: absolute;
    top: 50%;
    right: 3px;
    translate: 0 -50%;
    padding-left: 14px;
    background: linear-gradient(90deg, transparent, var(--bg-deep) 14px);
  }

  .project-line:hover .row-actions,
  .project-line:focus-within .row-actions {
    visibility: visible;
    opacity: 1;
  }

  /* A background project's waiting requests sit under its name: each one
     opens that project at the session's decision. */
  .requests {
    padding: 0 0 4px 23px;
  }

  .request {
    display: flex;
    align-items: baseline;
    gap: 6px;
    width: 100%;
    min-width: 0;
    height: 24px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    font-size: 11.5px;
    text-align: left;
  }

  .request:hover {
    background: var(--hover);
  }

  .request-kind {
    flex: none;
    color: var(--warning);
    font-weight: 600;
  }

  .request-session {
    overflow: hidden;
    color: var(--secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .needs-section {
    border-bottom: 1px solid var(--border);
  }

  .needs-section .session-state,
  .count {
    color: var(--warning);
  }

  .count {
    font-size: 11px;
    font-weight: 600;
  }

  .sessions {
    flex: 1;
  }

  .rail-foot {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: auto;
    padding: 8px 10px;
    border-top: 1px solid var(--border);
  }

  .rail-foot .btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }

  .spacer {
    flex: 1;
  }

  .rail-backdrop {
    display: none;
  }

  /* Compact: the rail leaves the grid and opens as an overlay. */
  @media (max-width: 760px) {
    .sidebar {
      display: none;
    }

    .sidebar.open {
      display: flex;
      position: fixed;
      inset: 0 auto 0 0;
      width: min(310px, 90vw);
      z-index: 16;
      box-shadow: var(--shadow);
    }

    .rail-backdrop {
      display: block;
      position: fixed;
      inset: 0;
      z-index: 15;
      border: 0;
      background: rgba(5, 7, 9, 0.5);
    }
  }
</style>
