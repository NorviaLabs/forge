<script>
  import Icon from "./Icon.svelte";

  let { approval, onDecide, savedDraft = null, onDraft = null } = $props();

  let note = $state("");
  let noting = $state(false);
  let confirm = $state(null);
  let noteError = $state("");
  let lastCallId = "";

  function saveDraft() {
    onDraft?.({
      note,
      noting,
      confirmDecision: confirm?.decision ?? null,
    });
  }

  // Background snapshots replace the approval object but keep its call id.
  // Reset only for a genuinely different request, not for a fresh copy.
  // A parked draft survives session switches.
  $effect(() => {
    const callId = approval.call_id;
    if (callId === lastCallId) return;
    lastCallId = callId;
    note = savedDraft?.note ?? "";
    noting = savedDraft?.noting ?? false;
    confirm = savedDraft?.confirmDecision
      ? approval.options.find((option) => option.decision === savedDraft.confirmDecision) ?? null
      : null;
    noteError = "";
  });

  const privileged = $derived(approval.options.some((option) => option.privileged));

  function choose(option) {
    if (option.decision === "deny_with_note") {
      noting = true;
      saveDraft();
      return;
    }
    if (option.privileged) {
      confirm = option;
      saveDraft();
      return;
    }
    send(option, false);
  }

  function send(option, confirmed) {
    onDraft?.(null);
    onDecide(
      approval.call_id,
      option.decision,
      option.decision === "deny_with_note" ? note : null,
      confirmed,
    );
    confirm = null;
  }

  function sendNote() {
    if (!note.trim()) {
      noteError = "Say why, or choose Don't run.";
      return;
    }
    send(
      { decision: "deny_with_note", privileged: false },
      false,
    );
  }
</script>

<section class="approval" aria-label="Approval required" data-call-id={approval.call_id}>
  <header>
    <span class="badge"><Icon name="alert" size={13} /> Needs your approval</span>
    <strong>{approval.tool}</strong>
  </header>
  <p class="reason">{approval.reason}</p>
  <pre>{approval.arguments}</pre>

  {#if confirm}
    <div class="confirm" role="alertdialog" aria-label="Confirm this grant">
      <p>
        {confirm.decision === "allow_host_for_session" || confirm.decision === "allow_host_always"
          ? "This opens network access the sandbox refused."
          : confirm.decision === "approve_all"
            ? "Every review prompt in this session will be approved automatically. The sandbox stays on."
            : "This command runs outside the filesystem sandbox."}
      </p>
      <p class="detail">{confirm.help}</p>
      <div class="actions">
        <button type="button" class="btn danger" onclick={() => send(confirm, true)}>
          {confirm.label}
        </button>
        <button type="button" class="btn" onclick={() => { confirm = null; saveDraft(); }}>Cancel</button>
      </div>
    </div>
  {:else if noting}
    <label class="note-field">
      <span>Why not?</span>
      <input
        class="field"
        bind:value={note}
        oninput={saveDraft}
        placeholder="The agent reads this"
        aria-label="Reason for refusing"
      />
    </label>
    {#if noteError}<p class="error">{noteError}</p>{/if}
    <div class="actions">
      <button type="button" class="btn danger" onclick={sendNote}>Don't run, send note</button>
      <button type="button" class="btn" onclick={() => { noting = false; saveDraft(); }}>Cancel</button>
    </div>
  {:else}
    <div class="actions">
      {#each approval.options as option (option.decision)}
        <button
          type="button"
          class="btn"
          class:primary={option.decision !== "deny" && option.decision !== "deny_with_note"}
          title={option.help + (option.detail ? ` — ${option.detail}` : "")}
          onclick={() => choose(option)}
        >
          {option.label}
        </button>
      {/each}
    </div>
    {#if privileged}
      <p class="footnote">
        Options marked in this card that run outside the sandbox or open a host ask again
        before they are sent.
      </p>
    {/if}
  {/if}
</section>

<style>
  .approval {
    margin: 0 24px 10px;
    padding: 11px 12px;
    border: 1px solid color-mix(in srgb, var(--warning) 55%, var(--border));
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--warning) 6%, var(--surface));
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
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

  header strong {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text);
  }

  .reason {
    margin: 0 0 8px;
    color: var(--secondary);
    font-size: 12.5px;
  }

  pre {
    margin: 0 0 10px;
    padding: 9px 11px;
    max-height: 160px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-deep);
    color: var(--secondary);
    font-family: var(--font-mono);
    font-size: 11.5px;
    line-height: 1.5;
    white-space: pre-wrap;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .confirm {
    padding: 9px 10px;
    border: 1px solid color-mix(in srgb, var(--error) 50%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--error) 8%, var(--surface));
  }

  .confirm p {
    margin: 0 0 6px;
    color: var(--text);
    font-size: 12.5px;
  }

  .confirm .detail {
    color: var(--secondary);
    font-size: 11.5px;
  }

  .note-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 8px;
    font-size: 12px;
    color: var(--secondary);
  }

  .error {
    margin: 0 0 8px;
    color: var(--error);
    font-size: 12px;
  }

  .footnote {
    margin: 8px 0 0;
    color: var(--muted);
    font-size: 11px;
  }
</style>
