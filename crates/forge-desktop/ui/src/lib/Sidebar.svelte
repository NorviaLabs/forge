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
    onSettings,
    onNew,
    onCloseRail,
  } = $props();

  let search = $state("");

  const needle = $derived(search.trim().toLowerCase());
  const matches = (session) => !needle || session.label.toLowerCase().includes(needle);
  const live = $derived(sessions.filter((session) => session.lifecycle === "active"));
  const closed = $derived(sessions.filter((session) => session.lifecycle !== "active"));
  const shownLive = $derived(live.filter(matches));
  const shownClosed = $derived(closed.filter(matches));
  // Roster order, never re-sorted: acting on a decision must not move rows.
  const waiting = $derived(sessions.filter((session) => attention.includes(session.session_id)));
  // The sessions belong under the on-screen project's heading; with no such
  // project registered they are still listed, after the headings.
  const grouped = $derived(projects.some((project) => project.id === projectId));

  // At compact width the open rail is a modal overlay: it traps focus, closes
  // on Escape, and returns focus to the header control that opened it.
  function overlay(node) {
    const action = modal(node, { onClose: () => onCloseRail?.() });
    return () => action.destroy();
  }

</script>

{#snippet sessionGroup()}
  <section class="panel sessions">
    <ul class="session-list">
      {#if creating}
        <li class="creating">
          <span class="mini spin"><Icon name="refresh" size={12} /></span>
          <span>Creating session…</span>
        </li>
      {/if}
      {#each shownLive as session (session.session_id)}
        {@const needsYou = attention.includes(session.session_id)}
        <li
          class="row"
          class:active={session.session_id === activeId}
          class:needs-you={needsYou}
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
            <span class="session-state {session.turn_state}">
              {needsYou ? "needs you" : session.turn_state}
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
        <li class="empty">
          <p>No sessions yet. Start one to give an agent a task.</p>
          <button class="btn primary" type="button" onclick={() => onNew?.()}>
            <Icon name="plus" size={13} />
            New session
          </button>
        </li>
      {:else if needle && shownLive.length === 0 && shownClosed.length === 0}
        <li class="empty">No sessions match “{search.trim()}”.</li>
      {/if}
    </ul>

    {#if shownClosed.length > 0}
      <details class="closed" open={!!needle}>
        <summary>{shownClosed.length} closed session{shownClosed.length === 1 ? "" : "s"}</summary>
        <ul class="session-list">
          {#each shownClosed as session (session.session_id)}
            <li class="row">
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
{/snippet}

{#if open}
  <button type="button" class="rail-backdrop" aria-label="Close sessions" tabindex="-1" onclick={() => onCloseRail?.()}></button>
{/if}
<aside class="sidebar" class:open aria-label="Projects and sessions" {@attach open ? overlay : null}>
  <div class="filter">
    {#if open}
      <button class="icon-btn" type="button" aria-label="Close sessions" onclick={() => onCloseRail?.()}>
        <Icon name="close" size={13} />
      </button>
    {/if}
    <input class="field" bind:value={search} placeholder="Search sessions" aria-label="Search sessions" spellcheck="false" />
  </div>

  <div class="rail-scroll">
    {#if waiting.length > 0}
      <section class="panel needs-section" aria-label="Needs you">
        <div class="section-head">
          <span class="label">Needs you</span>
          <span class="count">{waiting.length}</span>
        </div>
        <ul class="session-list">
          {#each waiting as session (session.session_id)}
            <li class="row needs-you">
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
                  <span class="project-badge" aria-label="{project.attention.length} waiting on you">{project.attention.length} waiting</span>
                {:else if status === "running"}
                  <span class="project-meta" aria-label="{project.running} of {project.limit} agents running">{project.running} of {project.limit} running</span>
                {:else if status === "closed"}
                  <span class="project-meta">closed</span>
                {:else if current}
                  <span class="project-meta">{live.length} {live.length === 1 ? "session" : "sessions"}</span>
                {/if}
              </button>
              {#if current}
                <span class="row-actions">
                  <button type="button" class="icon-btn" title="Close project…" aria-label={`Close ${project.name}`} onclick={() => onCloseProject?.(project)}>
                    <Icon name="close" size={12} />
                  </button>
                </span>
              {:else}
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
            {#if current}
              {@render sessionGroup()}
            {:else if project.attention.length}
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

    {#if !grouped}
      {@render sessionGroup()}
    {/if}
  </div>

  <footer class="rail-foot">
    <button class="btn ghost" type="button" onclick={onSettings}>Settings</button>
    <span class="spacer"></span>
    <button class="btn ghost" type="button" onclick={onHelp}>Help</button>
  </footer>
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    padding: 12px 12px 0;
    border-right: 1px solid var(--border);
    background: var(--surface);
  }

  /* Search, with the overlay's close control beside it at compact width. The
     close control stays first in the document so it takes the overlay's
     initial focus. */
  .filter {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
    margin-bottom: 12px;
  }

  .filter .field {
    flex: 1;
    min-width: 0;
  }

  .filter .icon-btn {
    order: 1;
    flex: none;
  }

  /* One scroll for every list in the rail; the footer stays put. */
  .rail-scroll {
    flex: 1;
    min-height: 0;
    margin: 0 -12px;
    padding: 0 12px 12px;
    overflow-y: auto;
  }

  .panel {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    height: var(--control);
    padding-left: 8px;
  }

  .label {
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .count {
    padding-right: 8px;
    color: var(--warning);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .needs-section {
    margin-bottom: 12px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }

  .project-list,
  .requests,
  /* Sessions sit one step in from their project, so the group is
     structural and not a matter of weight. */
  .session-list {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .project .session-list {
    padding-left: 12px;
  }

  /* Project group headings */
  .project + .project {
    margin-top: 8px;
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
    height: var(--control);
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--muted);
    font-size: var(--text-xs);
    text-align: left;
    transition: background var(--fast) var(--ease), color var(--fast) var(--ease);
  }

  .project-main:hover:not(:disabled) {
    background: var(--hover);
    color: var(--text);
  }

  .project-main:focus-visible {
    outline-offset: -2px;
  }

  .project-main:disabled {
    cursor: progress;
  }

  .project.current .project-main {
    color: var(--text);
    font-weight: 600;
  }

  .project-name {
    font-weight: 600;
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Filled while open, a ring once closed; the word beside it says the rest. */
  .project-dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }

  .project.running .project-dot {
    background: var(--accent);
  }

  .project.waiting .project-dot {
    background: var(--dot-waiting);
  }

  .project.closed .project-dot {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--muted);
  }

  .project-meta,
  .project-badge {
    flex: none;
    font-size: var(--text-xs);
  }

  .project-meta {
    color: var(--muted);
    font-weight: 400;
  }

  .project-badge {
    color: var(--warning);
    font-weight: 600;
  }

  /* A background project's waiting requests sit under its name: each one
     opens that project at the session's decision. */
  .requests {
    padding-left: 16px;
  }

  .request {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    min-width: 0;
    height: var(--control);
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    font-size: var(--text-xs);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .request:hover {
    background: var(--hover);
  }

  .request:focus-visible {
    outline-offset: -2px;
  }

  .request-kind {
    flex: none;
    color: var(--warning);
    font-weight: 600;
  }

  .request-session {
    min-width: 0;
    overflow: hidden;
    color: var(--secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Session rows */
  .session-list li.row {
    position: relative;
    display: flex;
    align-items: center;
    height: var(--control-lg);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    transition: background var(--fast) var(--ease), border-color var(--fast) var(--ease);
  }

  .session-list li.row:hover {
    background: var(--hover);
  }

  .session-list li.active {
    border-color: var(--border);
    background: var(--raised);
  }

  .session-list li.active:hover {
    background: var(--raised);
  }

  .session {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    height: 100%;
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    font-size: var(--text-md);
    text-align: left;
  }

  .session:focus-visible {
    outline-offset: -2px;
  }

  .session-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .session-list li.active .session-name {
    font-weight: 500;
  }

  .session-state {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .session-state.running {
    color: var(--accent);
  }

  .session-state.waiting,
  li.needs-you .session-state,
  .needs-section .session-state {
    color: var(--warning);
  }

  .session-state.failed {
    color: var(--error);
  }

  .slot {
    display: inline-grid;
    flex: none;
    place-items: center;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: var(--radius-sm);
    background: var(--accent-soft);
    color: var(--accent);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  /* Needs you: compact rows, always a warning dot beside the words. */
  .needs-section li.row {
    height: var(--control);
  }

  .needs-section .state-dot {
    background: var(--dot-waiting);
  }

  /* Row actions lie over the row's right edge, so showing them moves nothing.
     Only the selected session row exposes them: a running row's Stop button
     would otherwise take the click meant to select it. */
  .row-actions {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    display: flex;
    align-items: center;
    border-radius: var(--radius-sm);
    background: var(--surface);
    visibility: hidden;
  }

  .session-list li.active .row-actions {
    background: var(--raised);
  }

  .project-line:hover .row-actions,
  .project-line:focus-within .row-actions,
  .session-list li.active:hover .row-actions,
  .session-list li.active:focus-within .row-actions {
    visibility: visible;
  }

  /* Closed and resumable sessions */
  .closed {
    margin-top: 8px;
  }

  .closed summary {
    padding: 4px 8px;
    border-radius: var(--radius-sm);
    color: var(--muted);
    font-size: var(--text-xs);
    cursor: pointer;
    transition: color var(--fast) var(--ease);
  }

  .closed summary:hover {
    color: var(--text);
  }

  .closed summary:focus-visible {
    outline-offset: -2px;
  }

  .closed-row {
    color: var(--muted);
  }

  .mini {
    display: inline-flex;
    color: var(--muted);
  }

  .creating {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--control-lg);
    padding: 0 8px;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
    padding: 8px;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .empty p {
    margin: 0;
  }

  /* The name form opens directly above the control that asked for it. */
  .new {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
    margin: 0 -12px;
    padding: 12px;
    border-top: 1px solid var(--border);
  }

  .new .field {
    flex: 1;
    min-width: 0;
  }

  .rail-foot {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--bar);
    flex: none;
    margin: 0 -12px;
    padding: 0 12px;
    border-top: 1px solid var(--border);
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
      width: min(320px, 90vw);
      z-index: 16;
      box-shadow: var(--shadow);
    }

    .rail-backdrop {
      display: block;
      position: fixed;
      inset: 0;
      z-index: 15;
      border: 0;
      border-radius: 0;
      background: color-mix(in srgb, var(--bg-deep) 60%, transparent);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .project-main,
    .request,
    .session-list li.row,
    .closed summary {
      transition: none;
    }
  }
</style>
