<script>
  import { chord, isChord } from "../keys.js";

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

  // Presentation only: the first grant is the primary button, refusals read
  // as refusals, and a sandbox or network request says so in words.
  const isDeny = (option) => option.decision === "deny" || option.decision === "deny_with_note";
  const primaryDecision = $derived(approval.options.find((option) => !isDeny(option))?.decision);
  const riskLabel = $derived(
    approval.kind === "sandbox_escalation"
      ? "Sandbox escalation"
      : approval.kind === "network_host"
        ? "Network access"
        : "",
  );

  // Two shortcuts reach the card from anywhere in the window, the composer
  // included. They take the same path as a click, so a privileged option
  // still stops at its confirmation step.
  const allowOption = $derived(approval.options.find((option) => option.decision === primaryDecision) ?? null);
  const denyOption = $derived(approval.options.find((option) => option.decision === "deny") ?? null);

  function onShortcut(event) {
    if (confirm || noting || event.defaultPrevented) return;
    const option = isChord(event, "a", { shift: true }) ? allowOption : isChord(event, "d", { shift: true }) ? denyOption : null;
    if (!option) return;
    event.preventDefault();
    choose(option);
  }

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

<svelte:window onkeydown={onShortcut} />

<section class="approval" aria-label="Approval required" data-call-id={approval.call_id}>
  <header>
    <span class="state-dot waiting" aria-hidden="true"></span>
    <span class="badge">Needs your approval</span>
    <span class="spacer"></span>
    <strong>{approval.tool}</strong>
  </header>
  <pre>{approval.arguments}</pre>
  <p class="reason">{#if riskLabel}<span class="risk">{riskLabel}:</span> {/if}{approval.reason}</p>

  {#if confirm}
    <div class="confirm" role="alertdialog" aria-label="Confirm this grant">
      <p class="risk">
        {confirm.decision === "allow_host_for_session" || confirm.decision === "allow_host_always"
          ? "This opens network access the sandbox refused."
          : confirm.decision === "approve_all"
            ? "Every review prompt in this session will be approved automatically. The sandbox stays on."
            : confirm.decision === "allow_path_for_session" || confirm.decision === "allow_path_always"
              ? "Sandboxed commands will be able to write this directory. The sandbox stays on."
            : confirm.decision === "trust_program"
              ? "This program will run outside the sandbox in this project from now on, without asking."
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
          class:primary={option.decision === primaryDecision}
          class:danger={isDeny(option)}
          title={option.help + (option.detail ? ` — ${option.detail}` : "")}
          aria-keyshortcuts={option === allowOption ? (chord("A", { shift: true }).startsWith("Ctrl") ? "Control+Shift+A" : "Meta+Shift+A") : option === denyOption ? (chord("D", { shift: true }).startsWith("Ctrl") ? "Control+Shift+D" : "Meta+Shift+D") : undefined}
          onclick={() => choose(option)}
        >
          {option.label}
        </button>
      {/each}
      <span class="keys">
        {#if allowOption}<kbd>{chord("A", { shift: true })}</kbd> {allowOption.label.toLowerCase()}{/if}{#if allowOption && denyOption} · {/if}{#if denyOption}<kbd>{chord("D", { shift: true })}</kbd> deny{/if}
      </span>
    </div>
    {#if privileged}
      <p class="footnote">
        An option that runs outside the sandbox or opens a host asks you to confirm before it
        is sent.
      </p>
    {/if}
  {/if}
</section>

<style>
  /* Aligned to the conversation's content column, like the composer below. */
  .approval {
    display: flex;
    flex: none;
    flex-direction: column;
    gap: 12px;
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
    min-width: 0;
  }

  .badge {
    color: var(--text);
    font-size: var(--text-md);
    font-weight: 600;
    white-space: nowrap;
  }

  .spacer {
    flex: 1;
  }

  header strong {
    min-width: 0;
    overflow: hidden;
    color: var(--muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    font-weight: 400;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  pre {
    max-height: 160px;
    margin: 0;
    padding: 8px 12px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--text);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: 20px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .reason,
  .risk,
  .detail,
  .error,
  .footnote {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 20px;
    overflow-wrap: anywhere;
  }

  .reason {
    color: var(--secondary);
  }

  .risk {
    color: var(--warning);
  }

  .reason .risk {
    margin-right: 4px;
    font-weight: 500;
  }

  /* The second confirmation is an inline row of the card, not a card of its own. */
  .confirm {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }

  .detail {
    color: var(--secondary);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  .note-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: var(--secondary);
    font-size: var(--text-sm);
  }

  .error {
    color: var(--error);
  }

  .footnote {
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 16px;
  }

  .keys {
    margin-left: auto;
    color: var(--muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  kbd {
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
</style>
