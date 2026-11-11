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
    plan = null,
    tokens = null,
    background = [],
    queue = [],
    interrupted = [],
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
    onCancelQueued,
    onEditQueued,
    onRestoreInterrupted,
    onCancelBackground,
    onOpenChild,
    onInsertTask,
    onResolveChild,
    onFork,
    onCloseSession,
    onArchive,
    onRename,
    onPin,
  } = $props();

  const percent = $derived(Math.round(Math.min(Math.max(contextUsage, 0), 1) * 100));
  const level = $derived(percent >= 90 ? "high" : percent >= 70 ? "warm" : "");
  let confirmChild = $state("");
  let panel = $state();

  // State is a dot plus a word: the dot class and the word for each status.
  const PLAN_DOTS = { in_progress: "running", completed: "completed" };
  const PLAN_WORDS = { in_progress: "in progress", completed: "done", pending: "pending" };
  const TASK_DOTS = {
    queued: "queued",
    running: "running",
    waiting_for_approval: "waiting",
    succeeded: "completed",
    failed: "failed",
    cancelled: "cancelled",
  };

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


  {#if plan && plan.items.length > 0}
    <section id="menu-plan">
      <h2>Plan</h2>
      {#if plan.explanation}<p class="small dim">{plan.explanation}</p>{/if}
      <ul class="plan">
        {#each plan.items as item, index (index)}
          <li class={item.status}>
            <span class="mark state-dot {PLAN_DOTS[item.status] ?? "queued"}" aria-hidden="true"></span>
            <span class="step">{item.step}</span>
            <span class="plan-status">{PLAN_WORDS[item.status] ?? String(item.status).replaceAll("_", " ")}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if background.length > 0}
    <section id="menu-agents">
      <h2>Agents &amp; tasks <span class="count">{background.length}</span></h2>
      <ul class="tasks">
        {#each background as task (task.id)}
          <li class={task.status} class:needs={task.awaiting_approval && task.approval}>
            <div class="task-head">
              <span class="task-label">{task.label}</span>
              <span class="task-id mono">#{task.id}</span>
              <span class="task-status"><span class="state-dot {TASK_DOTS[task.status] ?? ""}" aria-hidden="true"></span>{task.status.replaceAll("_", " ")}</span>
            </div>
            {#if task.detail}<p class="task-detail mono">{task.detail}</p>{/if}
            {#if task.approval}<pre class="task-detail mono">{`${task.approval.tool}\n${task.approval.arguments}`}</pre>{/if}
            <div class="row">
              {#if task.awaiting_approval && task.approval}
                {#if confirmChild === `${task.id}:${task.approval.call_id}`}
                  <p class="small">This opens a sandbox-denied host or runs outside the filesystem sandbox.</p>
                  <button type="button" class="btn danger" onclick={() => { onResolveChild(task.id, true, task.approval.call_id, task.approval.request_id, true); confirmChild = ""; }}>Confirm allow</button>
                  <button type="button" class="btn" onclick={() => (confirmChild = "")}>Cancel</button>
                {:else}
                  <button type="button" class="btn primary" onclick={() => task.approval.privileged ? (confirmChild = `${task.id}:${task.approval.call_id}`) : onResolveChild(task.id, true, task.approval.call_id, task.approval.request_id, false)}>Allow</button>
                {/if}
                <button type="button" class="btn" onclick={() => onResolveChild(task.id, false, task.approval.call_id, task.approval.request_id, false)}>
                  Don't run
                </button>
              {:else if !["succeeded", "failed", "cancelled"].includes(task.status)}
                <button type="button" class="btn" onclick={() => onCancelBackground(task)}>
                  Stop
                </button>
              {/if}
              {#if task.child_session_id}
                <button type="button" class="btn" onclick={() => onOpenChild(task)}>
                  Read
                </button>
              {/if}
              {#if ["succeeded", "failed", "cancelled"].includes(task.status) && (task.latest_message || task.detail)}
                <button type="button" class="btn" onclick={() => onInsertTask(task)}>
                  Insert result
                </button>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if queue.length > 0 || interrupted.length > 0}
    <section id="menu-queue">
      <h2>Queue <span class="count">{queue.length + interrupted.length}</span></h2>
      {#if queue.length > 0}
      <ul class="queue">
        {#each queue as item, index (`${item.source}:${item.id}`)}
          <li>
            <span class="queue-n mono" aria-hidden="true">{index + 1}</span>
            <span class="queued-text">{item.text}</span>
            <button
              type="button"
              class="icon-btn"
              title="Edit this prompt in the composer"
              aria-label="Edit queued prompt"
              onclick={() => onEditQueued(item, index + 1)}
            >
              <Icon name="pencil" size={13} />
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Remove from the queue"
              aria-label="Remove queued prompt"
              onclick={() => onCancelQueued(sessionId, item.id, index + 1, item.source)}
            >
              <Icon name="close" size={13} />
            </button>
          </li>
        {/each}
      </ul>
      {/if}
      {#if interrupted.length > 0}
      <h3>Interrupted</h3>
      <ul class="queue">
        {#each interrupted as item (item.id)}
          <li>
            <span class="queued-text">{item.text}</span>
            <button
              type="button"
              class="icon-btn"
              title="Put this prompt back in the composer"
              aria-label="Restore interrupted prompt"
              onclick={() => onRestoreInterrupted(item.text)}
            >
              <Icon name="pencil" size={13} />
            </button>
          </li>
        {/each}
      </ul>
      {/if}
    </section>
  {/if}

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
      <button class="btn" type="button" onclick={onDetails}>Status details…</button>
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
  h2,
  h3 {
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

  p.small.dim {
    margin-bottom: 8px;
  }

  .row .small {
    flex-basis: 100%;
    color: var(--secondary);
  }

  .plan,
  .tasks,
  .queue {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .plan li {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: start;
    gap: 8px;
    min-height: 28px;
    padding: 4px 0;
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 20px;
  }

  /* Centres the dot on the first line of the step. */
  .plan .mark {
    margin-top: 6px;
  }

  .plan .step {
    overflow-wrap: anywhere;
  }

  .plan-status {
    color: var(--muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  .plan li.completed {
    color: var(--muted);
  }

  .plan li.in_progress {
    color: var(--text);
  }

  .tasks {
    gap: 8px;
  }

  .tasks li {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }

  .tasks li.needs {
    border-color: color-mix(in srgb, var(--warning) 45%, var(--border));
  }

  .task-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 20px;
  }

  .task-label {
    min-width: 0;
    overflow: hidden;
    font-size: var(--text-sm);
    font-weight: 500;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-id {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .task-status {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 8px;
    margin-left: auto;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .task-detail {
    max-height: 120px;
    margin: 0;
    overflow: auto;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 16px;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }

  .queue li {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    min-height: 28px;
  }

  .queue-n {
    flex: none;
    min-width: 16px;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 28px;
  }

  .queued-text {
    display: -webkit-box;
    flex: 1;
    min-width: 0;
    padding: 4px 0;
    overflow: hidden;
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 20px;
    overflow-wrap: anywhere;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
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
