<script>
  import Icon from "./Icon.svelte";

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
    changes = [],
    upstream = "",
    ahead = 0,
    behind = 0,
    gitBusy = false,
    mergeInProgress = false,
    conflicts = [],
    onOpenDiff,
    onGitOperation,
    onPickModel,
    onDetails,
    onStage,
    onUnstage,
    onCommit,
    onBranches,
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
    onRename,
    onPin,
  } = $props();

  const percent = $derived(Math.round(Math.min(Math.max(contextUsage, 0), 1) * 100));
  const level = $derived(percent >= 90 ? "high" : percent >= 70 ? "warm" : "");
  let confirmChild = $state("");
</script>

<aside class="inspector" aria-label="Session details">
  <section>
    <h2>Session</h2>
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
    </div>
    <div class="row">
      <button type="button" class="btn" onclick={onRename}>Rename</button>
      <button type="button" class="btn" onclick={onPin}>Pin</button>
      <button type="button" class="btn" onclick={onCloseSession}>Close</button>
      {#if counts.allows > 0}<button type="button" class="btn" onclick={onClearApprovals}>Clear approvals</button>{/if}
    </div>
  </section>

  <section>
    <h2>Context</h2>
    <button class="btn" type="button" onclick={onDetails}>Status details…</button>
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
  </section>

  {#if plan && plan.items.length > 0}
    <section>
      <h2>Plan</h2>
      {#if plan.explanation}<p class="small dim">{plan.explanation}</p>{/if}
      <ul class="plan">
        {#each plan.items as item, index (index)}
          <li class={item.status}>
            <span class="mark" aria-hidden="true">
              {item.status === "completed" ? "✓" : item.status === "in_progress" ? "▸" : "·"}
            </span>
            <span>{item.step}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if background.length > 0}
    <section>
      <h2>Background <span class="count">{background.length}</span></h2>
      <ul class="tasks">
        {#each background as task (task.id)}
          <li class={task.status}>
            <div class="task-head">
              <span class="task-label">{task.label}</span>
              <span class="task-status">{task.status.replaceAll("_", " ")}</span>
            </div>
            {#if task.detail}<p class="task-detail mono">{task.detail}</p>{/if}
            {#if task.approval}<pre class="task-detail mono">{task.approval.tool}\n{task.approval.arguments}</pre>{/if}
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
              {#if task.latest_message || task.detail}
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

  {#if queue.length > 0}
    <section>
      <h2>Queued <span class="count">{queue.length}</span></h2>
      <ul class="queue">
        {#each queue as item, index (`${item.source}:${item.id}`)}
          <li>
            <span class="queued-text">{item.text}</span>
            <button
              type="button"
              class="icon-btn"
              title="Edit this prompt in the composer"
              aria-label="Edit queued prompt"
              onclick={() => onEditQueued(item, index + 1)}
            >
              <Icon name="pencil" size={11} />
            </button>
            <button
              type="button"
              class="icon-btn"
              title="Remove from the queue"
              aria-label="Remove queued prompt"
              onclick={() => onCancelQueued(sessionId, item.id, index + 1, item.source)}
            >
              <Icon name="close" size={11} />
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if interrupted.length > 0}
    <section>
      <h2>Interrupted <span class="count">{interrupted.length}</span></h2>
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
              <Icon name="pencil" size={11} />
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <section class="changes">
    <h2>Changes <span class="count">{changes.length}</span></h2>
    {#if mergeInProgress}
      <p class="dim small">A merge is in progress with {conflicts.length} unresolved {conflicts.length === 1 ? "file" : "files"}.</p>
      <div class="row">
        <button type="button" class="btn" disabled={gitBusy} onclick={() => onGitOperation({ kind: "abort_merge" })}>
          Abort merge…
        </button>
      </div>
    {/if}
    {#if changes.length === 0}
      <p class="dim small">Clean working tree.</p>
    {:else}
      <ul>
        {#each changes as change (change.path)}
          <li>
            <button type="button" class="change" title={change.path} onclick={() => onOpenDiff(change.path)}>
              <span class="badge {change.status.toLowerCase()}">{change.status}</span>
              <span class="mono path">{change.path}</span>
            </button>
            {#if change.unstaged}
              <button
                type="button"
                class="icon-btn"
                title="Stage this file"
                aria-label={`Stage ${change.path}`}
                onclick={() => onStage(change.path)}
              >
                <Icon name="plus" size={11} />
              </button>
            {/if}
            {#if change.staged}
              <button
                type="button"
                class="icon-btn"
                title="Unstage this file"
                aria-label={`Unstage ${change.path}`}
                onclick={() => onUnstage(change.path)}
              >
                <Icon name="arrow-left" size={11} />
              </button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
    <div class="row">
      <button type="button" class="btn" onclick={onCommit}>Commit…</button>
      <button type="button" class="btn" onclick={onBranches}>Branches…</button>
    </div>
  </section>

  <section class="changes">
    <h2>Upstream</h2>
    <p class="dim small mono">
      {upstream || "No upstream configured"}
      {#if ahead} · ↑{ahead}{/if}
      {#if behind} · ↓{behind}{/if}
    </p>
    <div class="row">
      <button type="button" class="btn" disabled={gitBusy} onclick={() => onGitOperation({ kind: "fetch" })}>Fetch</button>
      <button type="button" class="btn" disabled={gitBusy} onclick={() => onGitOperation({ kind: "pull" })}>Pull…</button>
      <button type="button" class="btn" disabled={gitBusy || !upstream} onclick={() => onGitOperation({ kind: "push" })}>Push…</button>
    </div>
  </section>

  <section class="keys">
    <h2>Keys</h2>
    <p><kbd>⌘</kbd><kbd>K</kbd> commands</p>
    <p><kbd>⌘</kbd><kbd>M</kbd> model</p>
    <p><kbd>⌘</kbd><kbd>↵</kbd> send</p>
    <p><kbd>⌘</kbd><kbd>W</kbd> close tab</p>
    <p><kbd>⌃</kbd><kbd>`</kbd> terminal</p>
  </section>
</aside>

<style>
  .inspector {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
    padding: 14px 14px 18px;
    overflow: auto;
    border-left: 1px solid var(--border);
    background: var(--bg-deep);
  }

  section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  h2 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  dl {
    display: grid;
    grid-template-columns: 4.2rem minmax(0, 1fr);
    gap: 6px 8px;
    margin: 0;
    font-size: 12px;
  }

  dt {
    color: var(--muted);
  }

  dd {
    margin: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--secondary);
  }

  dd.state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  dd.model {
    padding: 0;
  }

  .model-button {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    min-width: 0;
    padding: 2px 5px;
    margin-left: -5px;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--secondary);
    text-align: left;
    font-family: var(--font-mono);
    font-size: 11.5px;
  }

  .model-button:hover {
    background: var(--hover);
    color: var(--text);
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
    font-size: 10px;
    opacity: 0;
  }

  .model-button:hover .change-hint {
    opacity: 1;
  }

  dd.branch {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }

  dd.dim,
  .dim {
    color: var(--muted);
  }

  .meter {
    height: 5px;
    border-radius: 999px;
    background: var(--surface);
    overflow: hidden;
  }

  .fill {
    height: 100%;
    border-radius: inherit;
    background: var(--success);
    transition: width 200ms ease;
  }

  .fill.warm {
    background: var(--warning);
  }

  .fill.high {
    background: var(--error);
  }

  .meter-label {
    margin: 0;
    color: var(--muted);
    font-size: 11px;
  }

  .count {
    color: var(--secondary);
    font-weight: 500;
  }

  .changes ul {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .change {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    min-width: 0;
    height: 24px;
    padding: 0 6px;
    border: 0;
    border-radius: 4px;
    background: none;
    text-align: left;
  }

  .change:hover {
    background: var(--hover);
  }

  .changes li {
    display: flex;
    align-items: center;
    gap: 1px;
  }

  .changes .change {
    flex: 1;
  }

  .badge {
    display: inline-grid;
    place-items: center;
    flex: none;
    width: 15px;
    height: 15px;
    border-radius: 4px;
    background: var(--surface);
    color: var(--warning);
    font-size: 9.5px;
    font-weight: 700;
  }

  .badge.d {
    color: var(--error);
  }

  .badge.\? {
    color: var(--success);
  }

  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11.5px;
  }

  .small {
    margin: 0;
    font-size: 12px;
  }

  dl.tight {
    margin-top: 2px;
    font-size: 11.5px;
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .row .btn {
    height: 24px;
    padding: 0 8px;
    font-size: 11.5px;
  }

  .toggle {
    padding: 1px 7px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    color: var(--secondary);
    font-size: 11px;
  }

  .toggle:hover {
    border-color: var(--border-strong);
    color: var(--text);
  }

  .toggle.on {
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--accent);
  }

  .plan,
  .tasks,
  .queue {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .plan li {
    display: flex;
    gap: 6px;
    color: var(--secondary);
    font-size: 11.5px;
    line-height: 1.4;
  }

  .plan .mark {
    width: 10px;
    color: var(--muted);
  }

  .plan li.completed {
    color: var(--muted);
    text-decoration: line-through;
  }

  .plan li.completed .mark {
    color: var(--success);
  }

  .plan li.in_progress {
    color: var(--text);
  }

  .plan li.in_progress .mark {
    color: var(--accent);
  }

  .task-head {
    display: flex;
    align-items: baseline;
    gap: 6px;
    justify-content: space-between;
  }

  .task-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11.5px;
  }

  .task-status {
    flex: none;
    color: var(--muted);
    font-size: 10px;
    letter-spacing: 0.03em;
  }

  .tasks li.waiting_for_approval .task-status {
    color: var(--warning);
  }

  .tasks li.failed .task-status {
    color: var(--error);
  }

  .task-detail {
    white-space: pre-wrap;
    margin: 2px 0 4px;
    overflow-wrap: anywhere;
    color: var(--muted);
    font-size: 10.5px;
  }

  .queue li {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .queued-text {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--secondary);
    font-size: 11.5px;
  }

  .keys p {
    display: flex;
    align-items: center;
    gap: 4px;
    margin: 0;
    color: var(--muted);
    font-size: 11.5px;
  }

  kbd {
    display: inline-grid;
    place-items: center;
    min-width: 17px;
    height: 17px;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface);
    color: var(--secondary);
    font-family: var(--font-ui);
    font-size: 10px;
  }
</style>
