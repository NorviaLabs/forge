<script>
  // What the session is doing besides the conversation: its plan, the
  // background jobs and child agents it started, and the prompts waiting
  // their turn. Always one click from the conversation, never in a dialog.
  import Icon from "./Icon.svelte";

  let {
    sessionId = "",
    plan = null,
    background = [],
    queue = [],
    interrupted = [],
    onCancelQueued,
    onEditQueued,
    onRestoreInterrupted,
    onCancelBackground,
    onOpenChild,
    onInsertTask,
    onResolveChild,
  } = $props();

  let confirmChild = $state("");
  const empty = $derived(!(plan?.items?.length) && background.length === 0 && queue.length === 0 && interrupted.length === 0);

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

</script>

<div class="activity">
  {#if empty}
    <div class="empty">
      <strong>Nothing in the background</strong>
      <p>The plan the agent publishes, the shell jobs and child agents it starts, and prompts you queue while a turn runs all appear here.</p>
    </div>
  {/if}
  {#if plan && plan.items.length > 0}
    <section id="activity-plan">
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
    <section id="activity-agents">
      <h2>Background tasks and agents <span class="count">{background.length}</span></h2>
      <ul class="tasks">
        {#each background as task (task.id)}
          <li class={task.status} class:needs={task.awaiting_approval && task.approval}>
            <div class="task-head">
              <span class="task-label">{task.label}</span>
              <span class="task-id mono">#{task.id}</span>
              <span class="task-status"><span class="state-dot {TASK_DOTS[task.status] ?? ""}" aria-hidden="true"></span>{task.status.replaceAll("_", " ")}</span>
            </div>
            {#if task.agent || task.model}
              <p class="task-meta small">
                {#if task.agent}Agent <span class="mono">{task.agent}</span>{/if}{#if task.agent && task.model} · {/if}{#if task.model}<span class="mono">{task.model}</span>{/if}{#if task.effort} · {task.effort} effort{/if}
              </p>
            {/if}
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
                <button type="button" class="link" onclick={() => onOpenChild(task)}>Read the transcript</button>
              {/if}
              {#if ["succeeded", "failed", "cancelled"].includes(task.status) && (task.latest_message || task.detail)}
                <button type="button" class="link" onclick={() => onInsertTask(task)}>Insert the result</button>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if queue.length > 0 || interrupted.length > 0}
    <section id="activity-queue">
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

</div>

<style>
  .activity {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-height: 0;
    padding: 0 16px 16px;
    overflow: auto;
  }

  section {
    padding-top: 16px;
  }

  h2,
  h3 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 8px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
  }

  h3 {
    margin-top: 12px;
  }

  .count {
    color: var(--muted);
  }

  .small {
    margin: 0;
    font-size: var(--text-sm);
  }

  p.small.dim {
    margin-bottom: 8px;
    color: var(--muted);
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .row {
    align-items: center;
  }

  .row .link {
    font-size: var(--text-sm);
  }

  .row .btn {
    height: 24px;
    padding: 0 8px;
  }

  .row .small {
    flex-basis: 100%;
    color: var(--secondary);
  }

  .empty {
    display: flex;
    flex: 1;
    flex-direction: column;
    justify-content: center;
    gap: 4px;
    padding: 24px 8px;
  }

  .empty strong {
    font-weight: 600;
  }

  .empty p {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 18px;
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

  /* The custom agent a child runs as, and the model it actually runs on. */
  .task-meta {
    margin: 0 0 4px;
    color: var(--secondary);
    font-size: var(--text-xs);
    line-height: 16px;
    overflow-wrap: anywhere;
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

</style>
