<script>
  import { onMount } from "svelte";
  import { Channel } from "@tauri-apps/api/core";
  import { invoke } from "../ipc.js";
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";

  let { onClose, onChanged } = $props();
  let providers = $state([]);
  let profileId = $state("");
  let model = $state("");
  let busy = $state(false);
  let error = $state("");
  let message = $state("");
  let login = $state(null);
  let disconnecting = $state(false);
  let removing = $state(false);
  let customOpen = $state(false);
  let customEditing = $state(false);
  let custom = $state({ id: "", base_url: "", model: "", wire: "chat-completions" });
  const selected = $derived(providers.find((entry) => entry.id === profileId));

  // Choosing another provider drops any half-finished confirmation or form.
  function choose(id) {
    if (id === profileId) return;
    profileId = id;
    disconnecting = false;
    removing = false;
    customOpen = false;
    customEditing = false;
    message = "";
    error = "";
  }

  // Connection state as a dot class and a word.
  function connection(provider) {
    if (provider.connected) return ["completed", "connected"];
    if (provider.needs_auth) return ["failed", "sign-in rejected"];
    if (provider.reuse) return ["queued", "sign-in available"];
    return ["", "not connected"];
  }

  async function refresh() {
    providers = await invoke("list_providers");
    if (!profileId) profileId = providers[0]?.id ?? "";
  }

  // A provider just connected: wait once while Forge prepares its models.
  // A failure here is not a failed connection; it is retried in the
  // background.
  async function prepareModels() {
    const connected = message;
    login = null;
    message = "Preparing models…";
    try {
      await invoke("prepare_model_tiers", { profileId });
    } catch {
      // Kept up to date in the background.
    }
    message = connected;
  }

  async function run(action) {
    if (busy) return;
    busy = true;
    error = "";
    message = "";
    login = null;
    try {
      if (action === "connect") {
        const channel = new Channel();
        channel.onmessage = (view) => (login = view);
        message = await invoke("connect_provider", { profileId, channel });
        await prepareModels();
      } else if (action === "importLogin") {
        message = await invoke("import_provider_login", { profileId });
        await prepareModels();
      } else if (action === "disconnect") {
        message = await invoke("disconnect_provider", { profileId });
      } else if (action === "saveCustom") {
        message = await invoke("save_custom_provider", {
          id: custom.id.trim(),
          baseUrl: custom.base_url.trim(),
          model: custom.model.trim(),
          wire: custom.wire,
        });
        customOpen = false;
      } else if (action === "updateCustom") {
        message = await invoke("update_custom_provider", {
          id: profileId,
          baseUrl: custom.base_url.trim(),
          model: custom.model.trim(),
        });
        customOpen = false;
      } else if (action === "removeCustom") {
        message = await invoke("remove_custom_provider", { id: profileId });
        profileId = "";
        removing = false;
      } else {
        await invoke("add_model", { profileId, model: model.trim() });
        model = "";
        message = "Model added. Choose it in the model picker.";
      }
      await refresh();
      await onChanged();
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
      disconnecting = false;
      login = null;
    }
  }

  onMount(() => {
    refresh().catch((cause) => (error = String(cause)));
  });
</script>

<div class="overlay">
  <div class="providers" role="dialog" aria-modal="true" aria-labelledby="providers-title" tabindex="-1" use:modal={{ onClose }}>
    <header>
      <h2 id="providers-title">Providers</h2>
      <button class="icon-btn" type="button" aria-label="Close providers" onclick={onClose}><Icon name="close" size={13} /></button>
    </header>
    <div class="body">
    <ul class="list" aria-label="Provider">
      {#each providers as provider (provider.id)}
        {@const [dot, word] = connection(provider)}
        <li>
          <button
            type="button"
            class="provider"
            class:current={provider.id === profileId}
            aria-pressed={provider.id === profileId}
            disabled={busy}
            onclick={() => choose(provider.id)}
          >
            <span class="name">{provider.title}</span>
            <span class="source mono">{provider.auth}</span>
            <span class="connection"><span class="state-dot {dot}" aria-hidden="true"></span>{word}</span>
          </button>
        </li>
      {/each}
    </ul>
    {#if selected}
      <h3 class="section-label">{selected.title}</h3>
      <p>{selected.auth}. API keys are read from your environment or stored credentials. If missing, macOS opens a native hidden-input prompt; keys never enter this page.</p>
      {#if selected.needs_auth}<p class="error" role="alert">The saved sign-in was rejected or has expired. Connect to enter a new key or reuse another sign-in.</p>{/if}
      {#if login}
        <section class="login" aria-live="polite">
          <h3>Finish signing in in your browser</h3>
          <p class="mono">{login.url}</p>
          <p>Device code: <strong class="mono">{login.code}</strong></p>
          <p>Waiting for authorization. This can take several minutes.</p>
        </section>
      {:else if busy}
        <p role="status">Working… If a system key prompt opened, finish or cancel it there.</p>
      {/if}
      {#if busy}<p>Closing this panel leaves the connection attempt running. Cancel a native key prompt in that prompt; device login expires automatically.</p>{/if}
      {#if disconnecting}
        <p>Remove stored credentials for {selected.title}? Environment variables remain unchanged. Existing sessions may hold their current connection until you switch models.</p>
        <div class="actions">
          <button class="btn danger" type="button" disabled={busy} onclick={() => run("disconnect")}>Disconnect provider</button>
          <button class="btn" type="button" disabled={busy} onclick={() => (disconnecting = false)}>Cancel</button>
        </div>
      {:else}
        <div class="actions">
          {#if selected.reuse && !selected.connected}
            <button class="btn primary" type="button" disabled={busy} onclick={() => run("importLogin")}>
              Reuse {selected.reuse} sign-in{selected.reuse_account ? ` (${selected.reuse_account})` : ""}
            </button>
          {/if}
          <button class={selected.reuse && !selected.connected ? "btn" : "btn primary"} type="button" disabled={busy} onclick={() => run("connect")}>Connect</button>
          <button class="btn" type="button" disabled={busy} onclick={() => (disconnecting = true)}>Disconnect…</button>
          {#if selected.custom}
            <button
              class="btn"
              type="button"
              disabled={busy}
              onclick={() => {
                custom = { id: selected.id, base_url: selected.base_url, model: "", wire: "chat-completions" };
                customEditing = true;
                customOpen = true;
              }}
            >Edit…</button>
            <button class="btn danger" type="button" disabled={busy} onclick={() => (removing = true)}>Remove…</button>
          {/if}
        </div>
      {/if}
      <form onsubmit={(event) => { event.preventDefault(); run("add"); }}>
        <label>Model ID
          <input class="field mono" bind:value={model} placeholder="Provider model ID" disabled={busy} spellcheck="false" />
        </label>
        <button class="btn" type="submit" disabled={busy || !model.trim()}>Add model</button>
      </form>
    {/if}

    {#if removing && selected}
      <div class="confirm">
        <p>Remove custom provider {selected.title}? Its entry, stored key, cached models and saved selections are removed. Existing sessions may keep their current connection until they switch models.</p>
        <div class="actions">
          <button class="btn danger" type="button" disabled={busy} onclick={() => run("removeCustom")}>Remove provider</button>
          <button class="btn" type="button" disabled={busy} onclick={() => (removing = false)}>Cancel</button>
        </div>
      </div>
    {/if}

    {#if customOpen}
      <form class="custom" onsubmit={(event) => { event.preventDefault(); run(customEditing ? "updateCustom" : "saveCustom"); }}>
        <h3>{customEditing ? `Edit ${custom.id}` : "Add custom provider"}</h3>
        {#if !customEditing}
          <label>ID
            <input class="field mono" bind:value={custom.id} placeholder="labbox" disabled={busy} spellcheck="false" />
          </label>
        {/if}
        <label>Base URL
          <input class="field mono" bind:value={custom.base_url} placeholder="https://api.example.com/v1" disabled={busy} spellcheck="false" />
        </label>
        <label>First model ID
          <input class="field mono" bind:value={custom.model} placeholder="model-name" disabled={busy} spellcheck="false" />
        </label>
        {#if !customEditing}
          <label>Wire protocol
            <select class="field" bind:value={custom.wire} disabled={busy}>
              <option value="chat-completions">Chat completions</option>
              <option value="responses">Responses</option>
              <option value="anthropic-messages">Anthropic messages</option>
              <option value="claude-agent">Claude Agent</option>
            </select>
          </label>
        {/if}
        <div class="actions">
          <button class="btn primary" type="submit" disabled={busy || !custom.base_url.trim() || (!customEditing && !custom.id.trim())}>
            {customEditing ? "Save provider" : "Add provider"}
          </button>
          <button class="btn" type="button" disabled={busy} onclick={() => (customOpen = false)}>Cancel</button>
        </div>
      </form>
    {:else}
      <button class="btn add-custom" type="button" disabled={busy} onclick={() => { custom = { id: "", base_url: "", model: "", wire: "chat-completions" }; customEditing = false; customOpen = true; }}>
        Add custom provider…
      </button>
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if message}<p role="status">{message}</p>{/if}
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    z-index: 35;
  }

  .providers {
    display: flex;
    flex-direction: column;
    width: min(560px, calc(100vw - 48px));
    max-height: calc(100vh - 64px);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
  }

  h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .body {
    flex: 1;
    min-height: 0;
    padding: 16px;
    overflow: auto;
  }

  /* Provider rows, separated by rules. */
  .list {
    max-height: 232px;
    margin: 0;
    padding: 0;
    overflow: auto;
    border-top: 1px solid var(--border);
    list-style: none;
  }

  .list li {
    border-bottom: 1px solid var(--border);
  }

  .provider {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    width: 100%;
    height: var(--control-lg);
    padding: 0 8px;
    border: 0;
    border-radius: 0;
    background: none;
    color: var(--text);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .provider:hover:not(:disabled) {
    background: var(--hover);
  }

  .provider.current {
    background: var(--raised);
  }

  .provider:focus-visible {
    outline-offset: -2px;
  }

  .name {
    font-size: var(--text-md);
    font-weight: 500;
    white-space: nowrap;
  }

  .source {
    overflow: hidden;
    color: var(--muted);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .connection {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    color: var(--secondary);
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  .section-label {
    margin: 16px 0 8px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
    letter-spacing: 0;
  }

  h3 {
    margin: 0 0 8px;
    font-size: var(--text-md);
    font-weight: 600;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: var(--secondary);
    font-size: var(--text-sm);
  }

  p {
    margin: 8px 0;
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 18px;
    overflow-wrap: anywhere;
  }

  .actions,
  form {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 8px;
  }

  .actions {
    margin-top: 12px;
  }

  form {
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
  }

  form label {
    flex: 1;
  }

  /* Beside a field, a button takes the field's height. */
  form:not(.custom) .btn {
    height: var(--control-lg);
  }

  form.custom {
    flex-direction: column;
    flex-wrap: nowrap;
    align-items: stretch;
    gap: 12px;
  }

  form.custom label {
    width: 100%;
  }

  form.custom .actions {
    margin-top: 4px;
  }

  .confirm {
    margin-top: 16px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }

  .add-custom {
    margin-top: 16px;
  }

  .login {
    margin: 12px 0;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
  }

  .login p:last-child {
    margin-bottom: 0;
  }

  .error {
    color: var(--error);
  }

  @media (prefers-reduced-motion: reduce) {
    .provider {
      transition: none;
    }
  }
</style>
