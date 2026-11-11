<script>
  import { tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { filterSlashCommands, slashKeyAction } from "../slash.js";
  import { historyKeyAction } from "../history.js";

  let {
    turnState = "idle",
    stopping = false,
    sessionId = "",
    imageInput = false,
    attachment = null,
    commands = [],
    history = [],
    onRemoveAttachment = () => {},
    onSend,
    onStop,
    drawerOpen = false,
    onToggleTerminal,
    onFocusTerminal,
    onStopTerminal,
    contextUsage = 0,
    onContext,
    prompt = $bindable(""),
  } = $props();


  let box = $state();
  let imageDrafts = $state({});
  let sending = $state(false);
  let imageError = $state("");
  let slashIndex = $state(0);
  let slashHidden = $state(false);
  let historyIndex = $state(null);
  let historyDraft = "";
  const images = $derived(imageDrafts[sessionId] ?? []);

  const running = $derived(turnState === "running" || turnState === "waiting");
  const slashMatches = $derived(filterSlashCommands(commands, prompt));
  const slashOpen = $derived(historyIndex === null && !slashHidden && slashMatches.length > 0);

  $effect(() => {
    void prompt;
    slashIndex = 0;
    slashHidden = false;
  });

  function complete(command) {
    prompt = `${command.name} `;
    slashHidden = true;
    box?.focus();
  }

  function resize() {
    if (!box) return;
    box.style.height = "auto";
    box.style.height = `${Math.min(box.scrollHeight, 220)}px`;
  }

  $effect(() => {
    void prompt;
    resize();
  });

  $effect(() => {
    void sessionId;
    imageError = "";
    historyIndex = null;
    historyDraft = "";
  });

  async function submit() {
    const text = prompt.trim();
    if ((!text && !images.length) || sending) return;
    const id = sessionId;
    const attached = images;
    sending = true;
    try {
      const ok = await onSend(text, attached, id);
      if (ok) {
        historyIndex = null;
        historyDraft = "";
        imageDrafts[id] = [];
        imageError = "";
        if (sessionId === id && prompt.trim() === text) prompt = "";
        resize();
      }
    } finally { sending = false; }
  }

  function paste(event) {
    const pasted = [...(event.clipboardData?.files ?? [])].filter((file) => file.type.startsWith("image/"));
    if (!pasted.length) return;
    event.preventDefault();
    imageError = "";
    if (!imageInput) { imageError = "Choose an image-capable model before attaching images."; return; }
    if (pasted.length + images.length > 4 || pasted.some((file) => file.size > 5 * 1024 * 1024)) {
      imageError = "Attach up to four images, at most 5 MiB each."; return;
    }
    imageDrafts[sessionId] = [...images, ...pasted];
  }

  async function onkeydown(event) {
    const action = slashKeyAction(event, {
      open: slashOpen,
      matches: slashMatches,
      index: slashIndex,
      value: prompt,
    });
    if (action) {
      event.preventDefault();
      if (action.type === "move") slashIndex = action.index;
      else if (action.type === "complete") complete(action.command);
      else if (action.type === "submit") submit();
      else slashHidden = true;
      return;
    }
    const recalled = historyKeyAction(event, {
      entries: history, value: prompt,
      start: box.selectionStart, end: box.selectionEnd,
      index: historyIndex, draft: historyDraft,
    });
    if (recalled) {
      event.preventDefault();
      prompt = recalled.value;
      historyIndex = recalled.index;
      historyDraft = recalled.draft;
      await tick();
      const caret = event.key === "ArrowUp" ? 0 : prompt.length;
      box?.setSelectionRange(caret, caret);
      return;
    }
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      submit();
    }
  }
</script>

<form class="composer" aria-label="Compose" onsubmit={(event) => { event.preventDefault(); submit(); }}>
  {#if slashOpen}
    <ul class="slash" id="composer-commands" role="listbox" aria-label="Commands and skills">
      {#each slashMatches as command, index (command.name)}
        <li role="presentation">
          <button
            id={`composer-command-${index}`}
            type="button"
            role="option"
            aria-selected={index === slashIndex}
            class:active={index === slashIndex}
            onmouseenter={() => (slashIndex = index)}
            onclick={() => complete(command)}
          >
            <span class="slash-name">{command.name}</span>
            <span class="slash-help">{command.help}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
  <textarea
    bind:this={box}
    bind:value={prompt}
    oninput={() => { historyIndex = null; historyDraft = ""; resize(); }}
    onkeydown={onkeydown}
    onpaste={paste}
    rows="2"
    aria-label="Prompt"
    title="↑/↓ cycle prompt history at the first/last line; Esc restores your draft"
    role="combobox"
    aria-autocomplete="list"
    aria-expanded={slashOpen}
    aria-controls="composer-commands"
    aria-activedescendant={slashOpen ? `composer-command-${slashIndex}` : undefined}
    placeholder="Ask Forge, or describe what to change…"
    spellcheck="false"
  ></textarea>
  {#if imageError}<p class="error" role="alert">{imageError}</p>{/if}
  <footer>
    {#if attachment}
      <div class="attachments" aria-label="File attachment">
        <button class="btn chip" type="button" title="Remove file context" onclick={onRemoveAttachment}>
          <span class="chip-text">Context: {attachment.path}:{attachment.line + 1}</span> <Icon name="close" size={11} />
        </button>
      </div>
    {/if}
    {#if images.length}
      <div class="attachments" aria-label="Image attachments">
        {#each images as image, index (index)}
          <button class="btn chip" type="button" title="Remove image" onclick={() => (imageDrafts[sessionId] = images.filter((_, i) => i !== index))}>
            <span class="chip-text">{image.name || `Image ${index + 1}`} · {Math.ceil(image.size / 1024)} KiB</span> <Icon name="close" size={11} />
          </button>
        {/each}
      </div>
    {/if}
    <span class="spacer"></span>
    {#if running}
      <button class="btn danger" type="button" disabled={stopping} onclick={onStop}>
        <Icon name="stop" size={13} />
        {stopping ? "Stopping…" : "Stop"}
      </button>
    {/if}
    <button class="btn primary" type="submit" disabled={sending || (!prompt.trim() && !images.length) || (images.length > 0 && !imageInput)}>
      <Icon name="send" size={13} />
      {sending ? "Sending…" : "Send"}
    </button>
  </footer>
</form>
{#if drawerOpen}
  <div class="drawer-head">
    <Icon name="terminal" size={13} />
    <span class="drawer-title">Terminal</span>
    <span class="spacer"></span>
    <button class="icon-btn" type="button" title="Focus terminal" aria-label="Focus terminal" onclick={onFocusTerminal}>
      <Icon name="expand" size={13} />
    </button>
    <button class="icon-btn" type="button" title="Hide terminal (⌃`)" aria-label="Hide terminal" onclick={onToggleTerminal}>
      <Icon name="close" size={13} />
    </button>
    <button class="icon-btn" type="button" title="Stop the shell" aria-label="Stop terminal" onclick={onStopTerminal}>
      <Icon name="stop" size={13} />
    </button>
  </div>
{/if}

<style>
  /* Aligned to the conversation's content column: at most 720px wide, with a
     24px gutter on each side when the centre column is narrower. */
  .composer {
    position: relative;
    display: flex;
    flex: none;
    flex-direction: column;
    gap: 8px;
    width: calc(100% - 68px);
    max-width: 720px;
    margin: 0 auto 16px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    transition: border-color var(--fast) var(--ease);
  }

  /* The card carries the textarea's focus indicator. */
  .composer:focus-within {
    border-color: var(--accent);
    box-shadow: var(--ring);
  }

  textarea {
    display: block;
    width: 100%;
    min-height: 40px;
    max-height: 220px;
    padding: 0;
    border: 0;
    border-radius: 0;
    background: transparent;
    outline: none;
    resize: none;
    font-size: var(--text-md);
    line-height: 20px;
  }

  textarea::placeholder {
    color: var(--muted);
  }

  .error {
    margin: 0;
    color: var(--error);
    font-size: var(--text-sm);
  }

  footer {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .spacer {
    flex: 1;
  }

  .attachments {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    min-width: 0;
  }

  .attachments .chip {
    max-width: 240px;
    height: 22px;
    padding: 0 8px;
    font-size: var(--text-xs);
    font-weight: 400;
  }

  .chip-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .slash {
    position: absolute;
    right: 0;
    bottom: calc(100% + 8px);
    left: 0;
    z-index: 20;
    max-height: 240px;
    margin: 0;
    padding: 4px;
    overflow: auto;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--raised);
    box-shadow: var(--shadow);
  }

  .slash button {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    height: var(--control);
    padding: 0 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .slash button:hover,
  .slash button.active {
    background: var(--hover);
  }

  .slash button:focus-visible {
    outline-offset: -2px;
  }

  .slash-name {
    flex: none;
    color: var(--text);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
  }

  .slash-help {
    min-width: 0;
    overflow: hidden;
    color: var(--muted);
    font-size: var(--text-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .drawer-head {
    display: flex;
    flex: none;
    align-items: center;
    gap: 8px;
    height: var(--bar);
    padding: 0 8px 0 16px;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .drawer-title {
    color: var(--text);
    font-weight: 600;
  }

  @media (prefers-reduced-motion: reduce) {
    .composer,
    .slash button {
      transition: none;
    }
  }
</style>
