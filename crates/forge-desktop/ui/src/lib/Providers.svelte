<script>
  import { onMount } from "svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
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

  async function refresh() {
    providers = await invoke("list_providers");
    if (!profileId) profileId = providers[0]?.id ?? "";
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
      } else if (action === "importLogin") {
        message = await invoke("import_provider_login", { profileId });
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
    <label>Provider
      <select class="field" bind:value={profileId} disabled={busy} onchange={() => { disconnecting = false; removing = false; customOpen = false; customEditing = false; message = ""; error = ""; }}>
        {#each providers as provider (provider.id)}
          <option value={provider.id}>{provider.title}{provider.connected ? " — connected" : provider.reuse ? " — sign-in available" : ""}</option>
        {/each}
      </select>
    </label>
    {#if selected}
      <p>{selected.auth}. API keys are read from your environment or stored credentials. If missing, macOS opens a native hidden-input prompt; keys never enter this page.</p>
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

<style>
  .overlay { position: fixed; inset: 0; display: grid; place-items: center; background: rgba(5, 7, 9, 0.6); z-index: 35; }
  .providers { width: min(34rem, calc(100vw - 48px)); max-height: calc(100vh - 64px); overflow: auto; padding: 18px; border: 1px solid var(--border-strong); border-radius: var(--radius); background: var(--surface); box-shadow: var(--shadow); }
  header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 14px; }
  h2 { margin: 0; font-size: 15px; }
  h3 { font-size: 13px; margin: 0 0 6px; }
  label { display: flex; flex-direction: column; gap: 5px; color: var(--secondary); font-size: 12px; }
  p { color: var(--secondary); font-size: 12px; line-height: 1.55; overflow-wrap: anywhere; }
  .actions, form { display: flex; align-items: end; gap: 6px; }
  form { margin-top: 20px; padding-top: 16px; border-top: 1px solid var(--border); }
  form label { flex: 1; }
  form.custom { flex-direction: column; align-items: stretch; gap: 8px; }
  form.custom label { width: 100%; }
  .confirm { margin-top: 16px; padding-top: 12px; border-top: 1px solid var(--border); }
  .add-custom { margin-top: 16px; }
  .login { margin: 12px 0; }
  .error { color: var(--error); }
</style>
