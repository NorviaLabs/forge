<script>
  import Icon from "./Icon.svelte";

  let { question, onAnswer } = $props();

  // One draft per question id; rebuilt only when a different call arrives.
  // Snapshots replace this object on every background update, so comparing
  // object identity would erase answers mid-edit.
  let drafts = $state([]);
  let error = $state("");
  let lastCallId = "";

  $effect(() => {
    const callId = question.call_id;
    if (callId === lastCallId) return;
    lastCallId = callId;
    drafts = question.questions.map((item) => ({
      id: item.id,
      selected: [],
      custom: "",
      callId,
    }));
    error = "";
  });

  function draftFor(id) {
    return drafts.find((draft) => draft.id === id);
  }

  function choose(item, label) {
    const draft = draftFor(item.id);
    if (!draft) return;
    if (item.multi_select) {
      draft.selected = draft.selected.includes(label)
        ? draft.selected.filter((entry) => entry !== label)
        : [...draft.selected, label];
    } else {
      draft.selected = [label];
    }
    drafts = drafts;
    error = "";
  }

  function setCustom(id, value) {
    const draft = draftFor(id);
    if (!draft) return;
    draft.custom = value;
    drafts = drafts;
  }

  function send() {
    const freeTextOnly = question.questions.some((item) => item.options.length === 0);
    for (const item of question.questions) {
      const draft = draftFor(item.id);
      const answered = item.options.length === 0
        ? !!draft?.custom.trim()
        : draft?.selected.length > 0 || !!draft?.custom.trim();
      if (!answered) {
        error = freeTextOnly
          ? "Answer every question before sending."
          : `“${item.header}” needs an answer.`;
        return;
      }
    }
    onAnswer(
      question.call_id,
      drafts.map((draft) => ({
        id: draft.id,
        selected: draft.selected,
        custom: draft.custom.trim() || null,
      })),
      false,
    );
  }
</script>

<section class="question" aria-label="The agent is asking" data-call-id={question.call_id}>
  <header>
    <span class="badge"><Icon name="chat" size={13} /> Needs your answer</span>
    {#if question.questions.length > 1}
      <span class="count">{question.questions.length} questions</span>
    {/if}
  </header>

  {#each question.questions as item (item.id)}
    <div class="item">
      <h3>{item.header || item.question}</h3>
      {#if item.header && item.header !== item.question}
        <p class="text">{item.question}</p>
      {/if}
      {#if item.options.length > 0}
        <div class="options" role="group" aria-label={item.header}>
          {#each item.options as option (option.label)}
            <button
              type="button"
              class="option"
              class:chosen={draftFor(item.id)?.selected.includes(option.value)}
              aria-pressed={draftFor(item.id)?.selected.includes(option.value)}
              onclick={() => choose(item, option.value)}
            >
              <span class="option-label">{option.label}</span>
              {#if option.description}<span class="option-detail">{option.description}</span>{/if}
            </button>
          {/each}
        </div>
      {/if}
      <input
        class="field custom"
        placeholder={item.options.length > 0 ? "Or answer in your own words" : "Your answer"}
        aria-label={item.options.length > 0 ? "Custom answer" : "Answer"}
        bind:value={
          () => draftFor(item.id)?.custom ?? "",
          (value) => setCustom(item.id, value)
        }
      />
    </div>
  {/each}

  {#if error}<p class="error" role="alert">{error}</p>{/if}

  <div class="actions">
    <button type="button" class="btn primary" onclick={send}>Send answer</button>
    <button type="button" class="btn" onclick={() => onAnswer(question.call_id, [], true)}>
      Skip
    </button>
  </div>
</section>

<style>
  .question {
    margin: 0 24px 10px;
    padding: 11px 12px;
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border));
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--accent) 5%, var(--surface));
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }

  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--accent);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .count {
    color: var(--muted);
    font-size: 11px;
  }

  .item + .item {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }

  h3 {
    margin: 0 0 4px;
    font-size: 12.5px;
    font-weight: 600;
  }

  .text {
    margin: 0 0 8px;
    color: var(--secondary);
    font-size: 12.5px;
  }

  .options {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 6px;
  }

  .option {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 7px 9px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--secondary);
    text-align: left;
    font-size: 12.5px;
  }

  .option:hover {
    border-color: var(--border-strong);
    color: var(--text);
  }

  .option.chosen {
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--text);
  }

  .option-label {
    font-weight: 500;
  }

  .option-detail {
    color: var(--muted);
    font-size: 11.5px;
  }

  .custom {
    width: 100%;
  }

  .error {
    margin: 8px 0 0;
    color: var(--error);
    font-size: 12px;
  }

  .actions {
    display: flex;
    gap: 6px;
    margin-top: 10px;
  }
</style>
