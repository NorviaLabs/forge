<script>
  import Icon from "./Icon.svelte";
  import { filterSlashCommands, slashKeyAction } from "../slash.js";

  let {
    turnState = "idle",
    stopping = false,
    sessionId = "",
    imageInput = false,
    attachment = null,
    commands = [],
    onRemoveAttachment = () => {},
    onSend,
    onStop,
    prompt = $bindable(""),
  } = $props();

  let box = $state();
  let imageDrafts = $state({});
  let sending = $state(false);
  let imageError = $state("");
  let slashIndex = $state(0);
  let slashHidden = $state(false);
  const images = $derived(imageDrafts[sessionId] ?? []);

  const running = $derived(turnState === "running" || turnState === "waiting");
  const slashMatches = $derived(filterSlashCommands(commands, prompt));
  const slashOpen = $derived(!slashHidden && slashMatches.length > 0);

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

  function onkeydown(event) {
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
    if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
      event.preventDefault();
      submit();
    }
  }
</script>

<form class="composer" aria-label="Compose" onsubmit={(event) => { event.preventDefault(); submit(); }}>
  {#if slashOpen}
    <ul class="slash" id="composer-commands" role="listbox" aria-label="Commands">
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
    oninput={resize}
    onkeydown={onkeydown}
    onpaste={paste}
    rows="2"
    aria-label="Prompt"
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
    <span class="hint"><kbd>⌘</kbd><kbd>↵</kbd> to send{imageInput ? " · Paste images here" : ""}</span>
    <span class="spacer"></span>
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

<style>
  .attachments { display: flex; flex-wrap: wrap; gap: 5px; }
  .error { margin: 0; color: var(--error); font-size: 12px; }
  .composer {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0 24px 16px;
    padding: 9px 10px 8px;
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    background: var(--surface);
    box-shadow: var(--shadow);
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
    outline: 2px solid var(--accent);
    outline-offset: 1px;
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
    font-size: 13.5px;
    line-height: 1.5;
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
