<script>
  // A running command is trying to reach a host nothing has allowed yet. The
  // gateway is holding its connection: allowing the host lets the command
  // carry on where it is, so this card appears while the turn is running and
  // nothing is run again.
  let { question, onAnswer } = $props();

  let busy = $state(false);

  async function answer(decision) {
    if (busy) return;
    busy = true;
    try {
      await onAnswer(question.host, decision);
    } finally {
      busy = false;
    }
  }
</script>

<section class="host" aria-label="A command is waiting to reach a host" data-host={question.host}>
  <header>
    <span class="state-dot waiting" aria-hidden="true"></span>
    <span class="badge">{question.write ? "Allow this change as you?" : "Allow network access?"}</span>
  </header>
  {#if question.write}
    <p class="text">
      A running command wants to change <code>{question.host}</code> using your account:
      <code>{question.write}</code>. It is paused until you decide.
    </p>
  {:else}
    <p class="text">
      A running command is trying to reach <code>{question.host}</code>{#if question.port !== 443}<span class="port"> on port {question.port}</span>{/if}.
      It is paused until you decide.
    </p>
  {/if}
  <div class="actions">
    <button type="button" class="btn primary" disabled={busy} onclick={() => answer("allow_once")}>Allow once</button>
    <button type="button" class="btn" disabled={busy} onclick={() => answer("allow_for_session")}>Allow this session</button>
    <button type="button" class="btn" disabled={busy} onclick={() => answer("allow_always")}>Always for this project</button>
    <span class="spacer"></span>
    <button type="button" class="btn ghost" disabled={busy} onclick={() => answer("deny")}>Don't allow</button>
  </div>
</section>

<style>
  .host {
    display: flex;
    flex: none;
    flex-direction: column;
    gap: 10px;
    width: calc(100% - 68px);
    max-width: 720px;
    margin: 0 auto 8px;
    padding: 16px;
    border: 1px solid var(--border);
    border-color: color-mix(in srgb, var(--warning) 45%, var(--border));
    border-radius: var(--radius);
    background: var(--surface);
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .badge {
    color: var(--text);
    font-size: var(--text-md);
    font-weight: 600;
  }

  .text {
    margin: 0;
    color: var(--text);
    font-size: var(--text-md);
    overflow-wrap: anywhere;
  }

  code {
    font-family: var(--font-mono);
  }

  .port {
    color: var(--muted);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  .spacer {
    flex: 1;
  }
</style>
