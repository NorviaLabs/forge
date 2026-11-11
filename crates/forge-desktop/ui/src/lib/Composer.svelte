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

  const contextPercent = $derived(Math.round(Math.min(Math.max(contextUsage, 0), 1) * 100));
  const contextLevel = $derived(contextPercent >= 90 ? "high" : contextPercent >= 70 ? "warm" : "");

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
  {#if attachment}
    <div class="attachments" aria-label="File attachment">
      <button class="btn" type="button" title="Remove file context" onclick={onRemoveAttachment}>
        Context: {attachment.path}:{attachment.line + 1} <Icon name="close" size={11} />
      </button>
    </div>
  {/if}
  {#if images.length}
    <div class="attachments" aria-label="Image attachments">
      {#each images as image, index (index)}
        <button class="btn" type="button" title="Remove image" onclick={() => (imageDrafts[sessionId] = images.filter((_, i) => i !== index))}>
          {image.name || `Image ${index + 1}`} · {Math.ceil(image.size / 1024)} KiB <Icon name="close" size={11} />
        </button>
      {/each}
    </div>
  {/if}
  {#if imageError}<p class="error" role="alert">{imageError}</p>{/if}
  <footer>
    <button
      class="icon-btn"
      type="button"
      aria-expanded={drawerOpen}
      aria-label={drawerOpen ? "Hide terminal" : "Show terminal"}
      title={drawerOpen ? "Hide terminal (⌃`)" : "Show terminal (⌃`)"}
      onclick={onToggleTerminal}
    >
      <Icon name="terminal" size={14} />
    </button>
    <span class="hint"><kbd>↵</kbd> send · <kbd>⇧</kbd><kbd>↵</kbd> newline{imageInput ? " · Paste images" : ""}</span>
    <span class="spacer"></span>
    <button class="context" type="button" title="Context & usage" onclick={onContext}>
      <span class="meter" aria-hidden="true"><span class="fill {contextLevel}" style="width: {contextPercent}%"></span></span>
      Context {contextPercent}%
    </button>
    {#if running}
      <button class="btn" type="button" disabled={stopping} onclick={onStop}>
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
    <span>Terminal</span>
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
  .attachments { display: flex; flex-wrap: wrap; gap: 5px; }
  .error { margin: 0; color: var(--error); font-size: 12px; }
  .composer {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 52rem;
    margin: 0 auto 18px;
    width: calc(100% - 48px);
    padding: 11px 12px 9px;
    border: 1px solid var(--border-strong);
    border-radius: 14px;
    background: var(--surface);
    box-shadow: var(--shadow);
    transition: border-color 140ms var(--ease), box-shadow 140ms var(--ease);
  }

  .slash {
    position: absolute;
    right: 0;
    bottom: calc(100% + 6px);
    left: 0;
    z-index: 20;
    max-height: 15rem;
    margin: 0;
    padding: 4px;
    overflow: auto;
    list-style: none;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--raised);
    box-shadow: var(--shadow);
  }

  .slash button {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    padding: 6px 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    text-align: left;
  }

  .slash button:hover,
  .slash button.active {
    background: var(--hover);
  }

  .slash-name {
    flex: none;
    color: var(--accent);
    font-family: var(--font-mono);
    font-size: 12px;
  }

  .slash-help {
    overflow: hidden;
    color: var(--muted);
    font-size: 11.5px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .composer:focus-within {
    border-color: var(--accent);
    box-shadow: var(--ring), var(--shadow);
  }

  textarea {
    width: 100%;
    min-height: 38px;
    max-height: 220px;
    padding: 2px 4px;
    border: 0;
    background: none;
    outline: none;
    resize: none;
    font-size: 14px;
    line-height: 1.55;
  }

  textarea::placeholder {
    color: var(--muted);
  }

  footer {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .spacer {
    flex: 1;
  }

  .context {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 6px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--muted);
    font-size: 11px;
  }

  .context:hover {
    background: var(--hover);
    color: var(--text);
  }

  .meter {
    width: 36px;
    height: 4px;
    overflow: hidden;
    border-radius: 2px;
    background: var(--border);
  }

  .fill {
    display: block;
    height: 100%;
    background: var(--accent);
  }

  .fill.warm { background: var(--warning); }
  .fill.high { background: var(--error); }

  .drawer-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px 4px 14px;
    border-top: 1px solid var(--border);
    color: var(--muted);
    font-size: 11.5px;
  }

  .hint {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: var(--muted);
    font-size: 11px;
  }

  kbd {
    display: inline-grid;
    place-items: center;
    min-width: 17px;
    height: 17px;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-deep);
    color: var(--secondary);
    font-family: var(--font-ui);
    font-size: 10px;
  }
</style>
