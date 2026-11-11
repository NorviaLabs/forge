<script>
  import { onMount, tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let {
    sessionId = "",
    workspace = "",
    branch = "",
    model = "",
    contextUsage = 0,
    queued = 0,
    turnState = "idle",
    approveAll = false,
    thinking = false,
    tokens = null,
    counts = { tools: 0, skills: 0, allows: 0 },
    destination = null,
    onClose,
    onPickModel,
    onDetails,
    onToggleApproveAll,
    onToggleThinking,
    onClearApprovals,
    onContinue,
    onCompact,
    onStopSession,
    onFork,
    onCloseSession,
    onArchive,
    onRename,
    onPin,
  } = $props();

  const percent = $derived(Math.round(Math.min(Math.max(contextUsage, 0), 1) * 100));
  const level = $derived(percent >= 90 ? "high" : percent >= 70 ? "warm" : "");
  let panel = $state();

  // Opening at a destination puts that section at the top of the panel.
  onMount(async () => {
    await tick();
    if (!destination || destination === "actions") return;
    const section = panel?.querySelector(`#menu-${destination}`);
    if (section) panel.scrollTop = section.offsetTop - panel.offsetTop;
  });
</script>

<div class="inspector" role="dialog" aria-modal="true" aria-labelledby="menu-title" tabindex="-1" bind:this={panel} use:modal={{ onClose }}>
  <header class="menu-head">
    <h2 id="menu-title" class="menu-title">Session</h2>
    <button type="button" class="icon-btn" aria-label="Close the Session menu" onclick={onClose}>
      <Icon name="close" size={13} />
    </button>
  </header>
  <section id="menu-actions">
    <dl>
      <dt>State</dt>
      <dd class="state">
        <span class="state-dot {turnState}"></span>
        {turnState}
      </dd>
      <dt>Model</dt>
      <dd class="model">
        <button type="button" class="model-button" title={model ? `${model} — choose a model` : "Choose a model"} onclick={onPickModel}>
          <span>{model || "Choose a model"}</span>
          <span class="change-hint">Change</span>
        </button>
      </dd>
      <dt>Workspace</dt>
      <dd class="mono" title={workspace}>{workspace || "—"}</dd>
      {#if branch}
        <dt>Branch</dt>
        <dd class="mono branch"><Icon name="branch" size={12} />{branch}</dd>
      {/if}
      {#if queued > 0}
        <dt>Queued</dt>
        <dd>{queued} prompt{queued === 1 ? "" : "s"}</dd>
      {/if}
      <dt>Trust</dt>
      <dd>
        <button
          type="button"
          class="toggle"
          class:on={approveAll}
          title="Approve every review prompt this session; the sandbox still asks"
          onclick={onToggleApproveAll}
        >
          {approveAll ? "Every command" : "Ask me"}
        </button>
      </dd>
      <dt>Thinking</dt>
      <dd>
        <button type="button" class="toggle" class:on={thinking} onclick={onToggleThinking}>
          {thinking ? "On" : "Off"}
        </button>
      </dd>
      <dt>Session</dt>
      <dd class="mono dim" title={sessionId}>{sessionId.slice(0, 8)}</dd>
    </dl>
    <div class="row">
      {#if turnState === "running" || turnState === "waiting"}
        <button type="button" class="btn" onclick={onStopSession}>Stop</button>
      {:else if ["cancelled", "interrupted", "failed"].includes(turnState)}
        <button type="button" class="btn" onclick={onContinue}>Continue</button>
      {/if}
      <button type="button" class="btn" onclick={onCompact}>Compact</button>
      <button type="button" class="btn" onclick={onFork}>Fork</button>
      <button type="button" class="btn" onclick={onRename}>Rename</button>
      <button type="button" class="btn" onclick={onPin}>Pin</button>
      <button type="button" class="btn" onclick={onCloseSession}>Close session</button>
    </div>
    <div class="row apart">
      <button type="button" class="btn danger" onclick={onArchive}>Archive…</button>
      {#if counts.allows > 0}<button type="button" class="btn danger" onclick={onClearApprovals}>Clear approvals</button>{/if}
    </div>
  </section>


  <section id="menu-context">
    <h2>Context &amp; usage</h2>
    <div class="meter" aria-label="Context window used">
      <div class="fill {level}" style="width: {percent}%"></div>
    </div>
    <p class="meter-label">{percent}% of the context window</p>
    {#if tokens}
      <dl class="tight">
        <dt>Tokens</dt>
        <dd>{tokens.prompt.toLocaleString()} in · {tokens.completion.toLocaleString()} out</dd>
        {#if tokens.cache_hits > 0 || tokens.cache_writes > 0}
          <dt>Cache</dt>
          <dd>{tokens.cache_hits.toLocaleString()} read · {tokens.cache_writes.toLocaleString()} written</dd>
        {/if}
        {#if tokens.context_capacity > 0}
          <dt>Window</dt>
          <dd>{tokens.context_tokens.toLocaleString()} / {tokens.context_capacity.toLocaleString()}</dd>
        {/if}
        <dt>Tools</dt>
        <dd>{counts.tools} · {counts.skills} skill{counts.skills === 1 ? "" : "s"}</dd>
        {#if counts.allows > 0}
          <dt>Allowed</dt>
          <dd>{counts.allows} pattern{counts.allows === 1 ? "" : "s"} this session</dd>
        {/if}
      </dl>
    {/if}
    <div class="row">
      <button class="link" type="button" onclick={onDetails}>Token and cost details</button>
    </div>
  </section>
</div>

<style>
  /* The dialog scrolls as a whole under a pinned header: opening at a
     destination sets this element's scrollTop. */
  .inspector {
    position: relative;
    display: flex;
    flex-direction: column;
    width: min(36rem, calc(100vw - 48px));
    max-height: 82vh;
    padding: 0 0 16px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .menu-head {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }

  h2.menu-title {
    margin: 0;
    color: var(--text);
    font-size: var(--text-lg);
    font-weight: 600;
  }

  section {
    display: flex;
    flex: none;
    flex-direction: column;
    padding: 0 16px;
  }

  #menu-actions {
    padding-top: 16px;
  }

  /* Section labels. */
  h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 16px 0 8px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
    letter-spacing: 0;
  }

  .count {
    font-family: var(--font-mono);
  }

  dl {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    align-items: center;
    gap: 4px 16px;
    margin: 0;
    font-size: var(--text-sm);
  }

  dt {
    color: var(--muted);
  }

  dd {
    min-height: 24px;
    margin: 0;
    overflow: hidden;
    color: var(--secondary);
    line-height: 24px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  dd.state,
  dd.branch {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  dd.model {
    padding: 0;
    overflow: visible;
  }

  .model-button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: calc(100% + 8px);
    min-width: 0;
    height: 24px;
    padding: 0 8px;
    margin-left: -8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--secondary);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    text-align: left;
    transition: background var(--fast) var(--ease), color var(--fast) var(--ease);
  }

  .model-button:hover {
    background: var(--hover);
    color: var(--text);
  }

  .model-button:focus-visible {
    outline-offset: -2px;
  }

  .model-button span:first-child {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .change-hint {
    margin-left: auto;
    color: var(--muted);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
  }

  dd.dim,
  .dim {
    color: var(--muted);
  }

  .toggle {
    height: 24px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 1;
    transition: background var(--fast) var(--ease), border-color var(--fast) var(--ease), color var(--fast) var(--ease);
  }

  .toggle:hover {
    background: var(--hover);
    color: var(--text);
  }

  .toggle.on {
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--text);
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  #menu-actions > .row {
    margin-top: 16px;
  }

  /* Destructive actions sit apart from the routine ones. */
  #menu-actions > .row.apart {
    padding-top: 16px;
    border-top: 1px solid var(--border);
  }

  #menu-context > .row {
    margin-top: 12px;
  }

  .small {
    margin: 0;
    font-size: var(--text-sm);
  }

  .meter {
    height: 4px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }

  .fill {
    height: 100%;
    border-radius: inherit;
    background: var(--muted);
  }

  .fill.high {
    background: var(--warning);
  }

  .meter-label {
    margin: 8px 0 0;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  dl.tight {
    margin-top: 8px;
  }

  dl.tight dd {
    font-family: var(--font-mono);
  }

  @media (prefers-reduced-motion: reduce) {
    .model-button,
    .toggle {
      transition: none;
    }
  }
</style>
