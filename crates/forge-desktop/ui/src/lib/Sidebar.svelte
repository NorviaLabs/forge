<script>
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let {
    sessions = [],
    activeId = "",
    project = "",
    projectCount = 0,
    // Requests waiting in other projects, oldest first.
    elsewhere = [],
    onProjects,
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
<aside class="sidebar" class:open aria-label="Sessions" {@attach open ? overlay : null}>
  <header class="workspace">
    <span class="mark">F</span>
    <button type="button" class="names" title="Switch project" aria-label="Project {project}: switch project" onclick={() => onProjects?.()}>
      <strong>Forge</strong>
      <span class="where" title={project}>
        {project || "Choose a project"}
        <Icon name="chevron" size={11} />
      </span>
      {#if projectCount > 1}<span class="sr-only">{projectCount} projects</span>{/if}
    </button>
    {#if open}
      <button class="icon-btn" type="button" aria-label="Close sessions" onclick={() => onCloseRail?.()}>
        <Icon name="close" size={13} />
      </button>
    {/if}
  </header>

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

  {#if elsewhere.length > 0}
    <section class="panel needs-section" aria-label="Waiting in other projects">
      <div class="section-head">
        <span class="label">Other projects</span>
        <span class="count">{elsewhere.length}</span>
      </div>
      <ul class="session-list">
        {#each elsewhere as item (item.project_id + item.session_id + item.call_id)}
          <li class="needs-you">
            <button type="button" class="session" onclick={() => onElsewhere?.(item)} title="{item.project_name}: {item.session_label}">
              <span class="state-dot waiting"></span>
              <span class="session-name">{item.session_label || "Session"}</span>
              <span class="session-state">{item.project_name}</span>
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
              {#if session.ownership === "managed" && ["archived", "retained"].includes(session.lifecycle)}
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
    border-radius: 7px;
    background: var(--accent);
    color: #0e1115;
    font-weight: 700;
  }

  .names {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    min-width: 0;
    padding: 2px 4px;
    margin: -2px -4px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: inherit;
    font: inherit;
    line-height: 1.25;
    text-align: left;
    cursor: pointer;
  }

  .names:hover {
    background: var(--surface-raised, rgba(255, 255, 255, 0.06));
  }

  .names .where {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    max-width: 100%;
  }

  .where {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: 11px;
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
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
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

  .session-list li.active {
    background: var(--selection);
  }

  .session {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    height: 32px;
    padding: 0 8px;
    border: 0;
    background: none;
    text-align: left;
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

  .names strong {
    font-size: 13px;
    letter-spacing: 0.01em;
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
