<script>
  import Icon from "./Icon.svelte";

  let { question, onAnswer, savedDrafts = null, onDrafts = null } = $props();

  // One draft per question id; rebuilt only when a different call arrives.
  // Snapshots replace this object on every background update, so comparing
  // object identity would erase answers mid-edit. A parked draft survives
  // session switches.
  let drafts = $state([]);
  let error = $state("");
  let lastCallId = "";

  $effect(() => {
    const callId = question.call_id;
    if (callId === lastCallId) return;
    lastCallId = callId;
    drafts = question.questions.map((item) => {
      const saved = savedDrafts?.find((draft) => draft.id === item.id);
      return {
        id: item.id,
        selected: saved?.selected ? [...saved.selected] : [],
        custom: saved?.custom ?? "",
        callId,
      };
    });
    error = "";
  });

  function draftFor(id) {
    return drafts.find((draft) => draft.id === id);
  }

  function saveDrafts() {
    onDrafts?.(drafts.map(({ id, selected, custom }) => ({ id, selected: [...selected], custom })));
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
    saveDrafts();
  }

  function setCustom(id, value) {
    const draft = draftFor(id);
    if (!draft) return;
    draft.custom = value;
    drafts = drafts;
    saveDrafts();
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
    onDrafts?.(null);
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
    <span class="state-dot waiting" aria-hidden="true"></span>
    <span class="badge">Needs your answer</span>
    <span class="spacer"></span>
    {#if question.questions.length > 1}
      <span class="count">{question.questions.length} questions</span>
    {/if}
  </header>

  {#each question.questions as item (item.id)}
    <div class="item">
      <h3 id={`question-${question.call_id}-${item.id}`}>{item.header || item.question}</h3>
      {#if item.header && item.header !== item.question}
        <p class="text">{item.question}</p>
      {/if}
      {#if item.options.length > 0}
        <div class="options" role="group" aria-labelledby={`question-${question.call_id}-${item.id}`}>
          {#each item.options as option (option.label)}
            <button
              type="button"
              class="option"
              class:chosen={draftFor(item.id)?.selected.includes(option.value)}
              aria-pressed={draftFor(item.id)?.selected.includes(option.value)}
              onclick={() => choose(item, option.value)}
            >
              <span class="mark" class:multi={item.multi_select} aria-hidden="true">
                {#if item.multi_select && draftFor(item.id)?.selected.includes(option.value)}<Icon name="check" size={10} />{/if}
              </span>
              <span class="option-label">{option.label}</span>
              {#if option.description}<span class="option-detail">{option.description}</span>{/if}
            </button>
          {/each}
        </div>
      {/if}
      <input
        class="field custom"
        placeholder={item.options.length > 0 ? "Or answer in your own words" : "Your answer"}
        aria-labelledby={`question-${question.call_id}-${item.id}`}
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
    <button type="button" class="btn ghost" onclick={() => { onDrafts?.(null); onAnswer(question.call_id, [], true); }}>
      Skip
    </button>
  </div>
</section>

<style>
  /* Aligned to the conversation's content column, like the composer below.
     A long set of questions scrolls inside the card instead of pushing the
     conversation out of the column. */
  .question {
    display: flex;
    flex: none;
    flex-direction: column;
    gap: 12px;
    width: calc(100% - 68px);
    max-width: 720px;
    max-height: 60%;
    margin: 0 auto 8px;
    padding: 16px;
    overflow-y: auto;
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

  .spacer {
    flex: 1;
  }

  .count {
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .item {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .item + .item {
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }

  h3 {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 500;
    line-height: 20px;
  }

  .text {
    margin: 0;
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 20px;
  }

  .options {
    display: flex;
    flex-direction: column;
  }

  .option {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0 8px;
    min-height: var(--control-lg);
    padding: 4px 8px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--text);
    font-size: var(--text-md);
    line-height: 20px;
    text-align: left;
    transition: background var(--fast) var(--ease), border-color var(--fast) var(--ease);
  }

  .option:hover {
    background: var(--hover);
  }

  .option.chosen {
    border-color: var(--border);
    background: var(--raised);
  }

  .option:focus-visible {
    outline-offset: -2px;
  }

  /* The choice mark: a ring for one answer, a box for several. The pressed
     state is also announced through aria-pressed. */
  .mark {
    display: inline-grid;
    flex: none;
    place-items: center;
    width: 16px;
    height: 16px;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--bg);
    color: var(--accent-text);
    transition: background var(--fast) var(--ease), border-color var(--fast) var(--ease);
  }

  .mark.multi {
    border-radius: 4px;
  }

  .mark::after {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
  }

  .option.chosen .mark {
    border-color: var(--accent);
  }

  .option.chosen .mark:not(.multi)::after {
    content: "";
  }

  .option.chosen .mark.multi {
    background: var(--accent);
  }

  .option-label {
    flex: none;
    max-width: 100%;
  }

  .option-detail {
    flex: 1;
    min-width: 0;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .custom {
    width: 100%;
  }

  .error {
    margin: 0;
    color: var(--error);
    font-size: var(--text-sm);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  @media (prefers-reduced-motion: reduce) {
    .option,
    .mark {
      transition: none;
    }
  }
</style>
