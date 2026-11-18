<script>
  // App settings: how Forge looks and sounds, the model providers it
  // connects to, and the custom agents it can start. Help holds only help.
  import { onMount } from "svelte";
  import Icon from "./Icon.svelte";
  import RemoteControl from "./RemoteControl.svelte";
  import { modal } from "../modal.js";
  import { invoke } from "../ipc.js";
  import { grantGroups, projectName, withoutGrant } from "../grants.js";
  import { mergeSettings, settingsModel } from "../computer.js";

  let { onClose, onProviders, onAgents, themes = [], theme = $bindable("forge-dark"), sound = $bindable(true), remoteStatus = null, section = null } = $props();

  // What sandboxed commands were allowed "always", per project.
  let grants = $state(null);
  let grantsError = $state("");

  // Web check, browser control and desktop control, from the user's own
  // config.
  let computer = $state(null);
  let computerError = $state("");
  let choosingProgram = $state(false);
  let programPath = $state("");
  const computerModel = $derived(computer ? settingsModel(computer) : null);

  onMount(async () => {
    // Opened from the footer's remote item: start at that section.
    if (section === "remote") document.getElementById("settings-remote")?.scrollIntoView({ block: "start" });
    try { grants = await invoke("sandbox_grants"); }
    catch (cause) { grantsError = `Could not read sandbox permissions: ${cause}`; }
    try { computer = await invoke("computer_use_settings"); }
    catch (cause) { computerError = `Could not read computer use settings: ${cause}`; }
  });

  // Save one change; on a refusal the switches go back to what is saved.
  async function saveComputer(change) {
    if (!computer) return;
    computerError = "";
    const next = mergeSettings(computer.settings, change);
    try {
      await invoke("save_computer_use_settings", { settings: next });
      computer = await invoke("computer_use_settings");
      choosingProgram = false;
    } catch (cause) {
      computerError = String(cause);
      computer = { ...computer };
    }
  }

  async function revoke(project, kind, value) {
    grantsError = "";
    try {
      await invoke("revoke_sandbox_grant", { project, kind, value });
      grants = withoutGrant(grants, project, kind, value);
    } catch (cause) {
      grantsError = `Could not remove ${value}: ${cause}`;
    }
  }
</script>

<div class="overlay">
  <div class="settings" role="dialog" aria-modal="true" aria-labelledby="settings-title" tabindex="-1" use:modal={{ onClose }}>
    <header>
      <h2 id="settings-title">Settings</h2>
      <button class="icon-btn" type="button" aria-label="Close settings" onclick={onClose}><Icon name="close" size={13} /></button>
    </header>
    <div class="body">
      <h3>Appearance</h3>
      <label>
        <span>Theme</span>
        <select class="field" bind:value={theme}>
          <option value="system">System</option>
          {#each themes as entry}<option value={entry.id}>{entry.name}</option>{/each}
        </select>
      </label>

      <h3>Computer use</h3>
      {#if computerError}<p class="note error" role="alert">{computerError}</p>{/if}
      {#if computerModel}
        <!-- The program serves web check and browser control alike. -->
        <div class="row">
          <span class="what">
            <span id="browser-label">Browser</span>
            {#if computerModel.browserProblem}
              <span class="note warn">{computerModel.browserProblem}</span>
            {:else}
              <span class="note path">{computerModel.browserPath}</span>
            {/if}
          </span>
          <span class="controls">
            {#if computerModel.browserOptions.length}
              <select
                class="field"
                aria-labelledby="browser-label"
                disabled={computerModel.browserRowDisabled}
                value={computerModel.selectedBrowser}
                onchange={(event) => saveComputer({ browser: { executable: event.currentTarget.value } })}
              >
                {#each computerModel.browserOptions as option (option.value)}
                  <option value={option.value}>{option.label}</option>
                {/each}
              </select>
            {/if}
            <button class="btn" type="button" disabled={computerModel.browserRowDisabled} onclick={() => { choosingProgram = true; programPath = computerModel.selectedBrowser; }}>Choose a program…</button>
          </span>
        </div>
        {#if choosingProgram}
          <form class="row sub" onsubmit={(event) => { event.preventDefault(); saveComputer({ browser: { executable: programPath } }); }}>
            <input class="field" bind:value={programPath} placeholder="/usr/bin/chromium" aria-label="Browser program, full path" />
            <button class="btn" type="submit">Use</button>
            <button class="btn" type="button" onclick={() => (choosingProgram = false)}>Cancel</button>
          </form>
        {/if}
        <label class:disabled={computerModel.webCheckDisabled}>
          <span class="what">
            <span>Web check</span>
            <span class="note">{computerModel.webCheckNote}</span>
          </span>
          <input type="checkbox" disabled={computerModel.webCheckDisabled} checked={computerModel.webCheckEnabled} onchange={(event) => saveComputer({ web_check: { enabled: event.currentTarget.checked } })} />
        </label>
        <label class:disabled={computerModel.browserControlDisabled}>
          <span class="what">
            <span>Browser control</span>
            <span class="note">{computerModel.browserNote}</span>
          </span>
          <input type="checkbox" disabled={computerModel.browserControlDisabled} checked={computerModel.browserEnabled} onchange={(event) => saveComputer({ browser: { enabled: event.currentTarget.checked } })} />
        </label>
        <label class="sub" class:disabled={computerModel.browserSubrowsDisabled}>
          <span>Show the browser window</span>
          <input type="checkbox" disabled={computerModel.browserSubrowsDisabled} checked={computerModel.showWindow} onchange={(event) => saveComputer({ browser: { headless: !event.currentTarget.checked } })} />
        </label>
        <label class:disabled={computerModel.desktopDisabled}>
          <span class="what">
            <span>Desktop control</span>
            <span class="note">{computerModel.desktopNote}</span>
          </span>
          <input type="checkbox" disabled={computerModel.desktopDisabled} checked={computerModel.desktopEnabled} onchange={(event) => saveComputer({ desktop: { enabled: event.currentTarget.checked } })} />
        </label>
      {/if}

      <h3>Notifications</h3>
      <label>
        <span>Play a sound when a session needs you</span>
        <input type="checkbox" bind:checked={sound} />
      </label>

      <h3>Connections</h3>
      <div class="row">
        <span class="what">
          <span>Model providers</span>
          <span class="note">Sign in, add a key, or add a custom provider.</span>
        </span>
        <button class="link" type="button" onclick={onProviders}>Manage</button>
      </div>

      <RemoteControl status={remoteStatus} />

      <h3>Agents</h3>
      <div class="row">
        <span class="what">
          <span>Custom agents</span>
          <span class="note">Specialists the model can start by name, in this project or in all of yours.</span>
        </span>
        <button class="link" type="button" aria-label="Manage custom agents" onclick={onAgents}>Manage</button>
      </div>

      <h3>Sandbox permissions</h3>
      <p class="note">What you allowed "always" for each project. Removing one takes effect at once, in open sessions too.</p>
      {#if grantsError}<p class="note error" role="alert">{grantsError}</p>{/if}
      {#if grants && grants.length === 0}
        <p class="note">Nothing beyond the defaults.</p>
      {/if}
      {#each grants ?? [] as project (project.project)}
        <div class="project" title={project.project}>{projectName(project.project)}</div>
        {#each grantGroups(project) as group (group.kind)}
          <div class="group">{group.label}</div>
          {#each group.items as item (item)}
            <div class="row grant">
              <span class="value">{item}</span>
              <button class="link" type="button" aria-label={`Remove ${item} from ${projectName(project.project)}`} onclick={() => revoke(project.project, group.kind, item)}>Remove</button>
            </div>
          {/each}
        {/each}
      {/each}
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    display: grid;
    /* Top-anchored: removing a permission shrinks the dialog, and a centred
       one would slide the next Remove button under the pointer. */
    place-items: start center;
    padding-top: 32px;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    z-index: 30;
  }

  .settings {
    display: flex;
    flex-direction: column;
    width: min(520px, calc(100vw - 48px));
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
    padding: 0 16px 16px;
    overflow: auto;
  }

  h3 {
    margin: 16px 0 4px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
  }

  /* One setting per row: what it is at the left, its control at the right. */
  label,
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: var(--bar);
    padding: 4px 0;
    font-size: var(--text-md);
  }

  .what {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .note {
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 18px;
  }

  p.note {
    margin: 0 0 4px;
  }

  .error {
    color: var(--error);
  }

  .project {
    margin-top: 10px;
    font-size: var(--text-md);
    font-weight: 600;
  }

  .group {
    margin-top: 4px;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .row.grant {
    min-height: 0;
    padding: 2px 0 2px 12px;
  }

  .value {
    min-width: 0;
    overflow: hidden;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  select {
    width: 200px;
    height: var(--control);
    flex: none;
    padding: 0 8px;
    font-size: var(--text-sm);
  }

  .sub {
    padding-left: 24px;
  }

  .sub.disabled,
  label.disabled {
    opacity: 0.6;
  }

  .controls {
    display: flex;
    gap: 8px;
    flex: none;
  }

  .controls select {
    width: 150px;
  }

  form.sub {
    justify-content: flex-start;
    gap: 8px;
  }

  form.sub .field {
    flex: 1;
    min-width: 0;
  }

  .note.warn {
    color: var(--warning);
  }

  .note.path {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }

  input[type="checkbox"] {
    width: 16px;
    height: 16px;
    flex: none;
    margin: 0;
    accent-color: var(--accent);
  }
</style>
