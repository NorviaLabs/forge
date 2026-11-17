<script>
  // Settings › Remote control: turn the service on or off, choose where it
  // listens, pair one device with a one-time code, revoke it. The backend
  // owns every decision; this only collects input and shows its status.
  import { onDestroy, onMount, tick } from "svelte";
  import { invoke } from "../ipc.js";
  import {
    addressError,
    canEditConnection,
    confirmationText,
    countdown,
    deviceNote,
    portError,
    requestAnnouncement,
    settingsInput,
    statusLine,
  } from "../remote.js";

  let { status = null } = $props();

  // The fields, as typed. Filled from the backend once, then only by the
  // user while remote control is off.
  let form = $state({ enabled: false, reach: "loopback", address: "", port: "47390" });
  let loaded = $state(false);
  let busy = $state(false);
  let error = $state("");
  let pairing = $state(null);
  let pairedBefore = null;
  let confirmRevoke = $state(false);
  let copied = $state(false);
  let now = $state(Date.now());
  let ticker = null;

  let current = $derived(status);
  // A device that redeemed the code and waits for Allow or Refuse.
  let request = $derived(current?.pairing_request ?? null);
  let refused = $state(false);
  let deciding = $state(false);
  let announcement = $state("");
  let requestPanel = $state(null);
  let refuseButton = $state(null);
  let line = $derived(statusLine(current));
  let editable = $derived(canEditConnection(current));
  let device = $derived(current?.device ?? null);

  function fill(from) {
    if (!from) return;
    form.enabled = from.enabled;
    form.reach = from.reach;
    form.address = from.address ?? from.suggested_address ?? "";
    form.port = String(from.port);
    loaded = true;
  }

  onMount(async () => {
    try {
      const initial = await invoke("remote_status");
      if (!status) status = initial;
      fill(initial);
    } catch (cause) {
      error = `Remote control is unavailable: ${cause}`;
    }
  });

  onDestroy(() => {
    if (ticker) clearInterval(ticker);
    if (requestTicker) clearInterval(requestTicker);
  });

  // A request arriving replaces the code panel, comes into view, takes focus
  // on Refuse (the safe answer) and is announced. Its countdown ticks while
  // it waits.
  let announcedFor = null;
  let requestTicker = null;
  $effect(() => {
    const confirmation = request?.confirmation ?? null;
    if (!confirmation) {
      announcedFor = null;
      if (requestTicker) clearInterval(requestTicker);
      requestTicker = null;
      return;
    }
    if (confirmation === announcedFor) return;
    announcedFor = confirmation;
    refused = false;
    stopPairing();
    now = Date.now();
    if (!requestTicker) requestTicker = setInterval(() => (now = Date.now()), 1000);
    announcement = requestAnnouncement(request);
    tick().then(() => {
      requestPanel?.scrollIntoView({ block: "nearest" });
      // Show the ring even after a mouse click: focus moved on its own.
      refuseButton?.focus({ focusVisible: true });
    });
  });

  async function decide(allow) {
    if (!request || deciding) return;
    error = "";
    deciding = true;
    try {
      status = await invoke("remote_pair_decide", { confirmation: request.confirmation, allow });
      refused = !allow;
    } catch (cause) {
      error = `Could not answer the pairing request: ${cause}`;
    } finally {
      deciding = false;
    }
  }

  // A pairing ends when the code is used (a new device appears), expires,
  // is burnt by wrong guesses, or the service stops. Making a code clears
  // the burnt message, so only its arrival ends the panel.
  let lastPairingError = null;
  $effect(() => {
    const burnt = current?.pairing_error ?? null;
    const arrived = burnt && burnt !== lastPairingError;
    lastPairingError = burnt;
    if (!pairing) return;
    const id = current?.device?.id ?? null;
    if (id && id !== pairedBefore) stopPairing();
    else if (!current?.listening || arrived) stopPairing();
  });

  async function setEnabled(enabled) {
    error = "";
    if (enabled) {
      const why = portError(form.port) ?? addressError(form.address, form.reach);
      if (why) {
        error = why;
        form.enabled = false;
        return;
      }
    }
    busy = true;
    try {
      const next = await invoke("remote_configure", { settings: settingsInput({ ...form, enabled }) });
      status = next;
      fill(next);
    } catch (cause) {
      error = String(cause);
      form.enabled = !enabled;
    } finally {
      busy = false;
    }
  }

  async function startPairing() {
    error = "";
    confirmRevoke = false;
    refused = false;
    try {
      pairedBefore = device?.id ?? null;
      pairing = await invoke("remote_pair_start");
      copied = false;
      now = Date.now();
      ticker = setInterval(() => {
        now = Date.now();
        if (pairing && Date.parse(pairing.expires_at) <= now) stopPairing();
      }, 1000);
    } catch (cause) {
      error = `Could not make a pairing code: ${cause}`;
    }
  }

  function stopPairing() {
    pairing = null;
    if (ticker) clearInterval(ticker);
    ticker = null;
  }

  async function cancelPairing() {
    stopPairing();
    try { await invoke("remote_pair_cancel"); } catch { /* the code expires anyway */ }
  }

  async function copyLink() {
    try {
      await navigator.clipboard.writeText(pairing.link);
      copied = true;
    } catch {
      error = "Copying failed; select the link and copy it.";
    }
  }

  async function revoke() {
    error = "";
    try {
      await invoke("remote_revoke");
      confirmRevoke = false;
    } catch (cause) {
      error = `Could not revoke the device: ${cause}`;
    }
  }

  const dotClass = { running: "running", failed: "failed", idle: "queued" };
</script>

<h3 id="settings-remote">Remote control</h3>
<label class="row">
  <span class="what">
    <span>Let a paired device follow and direct sessions</span>
    <span class="note">It can watch sessions in open projects, send prompts, stop a turn, and allow or deny requests.</span>
  </span>
  <input type="checkbox" checked={form.enabled} disabled={!loaded || busy} onchange={(event) => setEnabled(event.currentTarget.checked)} />
</label>

{#if form.enabled || error}
  <label class="row">
    <span>Reachable from</span>
    <select class="field" bind:value={form.reach} disabled={!editable || busy} title={editable ? "" : "Turn remote control off to change this"}>
      <option value="loopback">This computer only</option>
      <option value="lan">Local network</option>
    </select>
  </label>
  {#if form.reach === "lan"}
    <div class="row">
      <span class="what">
        <span>Address</span>
        <span class="note">Anyone on this network can reach the port. Only a paired device gets in.</span>
      </span>
      <span class="fields">
        <input class="field mono address" aria-label="Address" bind:value={form.address} disabled={!editable || busy} spellcheck="false" />
        <input class="field mono port" aria-label="Port" inputmode="numeric" bind:value={form.port} disabled={!editable || busy} />
      </span>
    </div>
  {:else}
    <label class="row">
      <span>Port</span>
      <input class="field mono port" inputmode="numeric" bind:value={form.port} disabled={!editable || busy} />
    </label>
  {/if}
{/if}

{#if line}
  <p class="status" class:error-text={line.tone === "failed"} role={line.tone === "failed" ? "alert" : undefined}>
    <span class="state-dot {dotClass[line.tone]}" aria-hidden="true"></span>
    <span class="word">{line.word}</span>
    {#if line.text}<span class:mono={line.tone === "running"}>{line.text}</span>{/if}
  </p>
{/if}
{#if error}<p class="note error-text" role="alert">{error}</p>{/if}
{#if current?.pairing_error && !pairing}<p class="note error-text" role="alert">{current.pairing_error}</p>{/if}

<p class="sr-only" aria-live="polite">{announcement}</p>

{#if request}
  <div class="request" bind:this={requestPanel}>
    <div class="request-head">
      <span class="state-dot waiting" aria-hidden="true"></span>
      <span class="request-title">Allow {request.device_name} to pair?</span>
      <span class="note time">{countdown(request.expires_at, now)} left</span>
    </div>
    <p class="note lead gap-sm">Pair only if the device shows this number:</p>
    <span class="code mono" aria-label={`Confirmation number ${request.confirmation.split("").join(" ")}`}>{confirmationText(request.confirmation)}</span>
    <p class="note gap-sm">
      If the numbers differ, or you did not just enter the code, refuse.{#if device} Allowing replaces {device.name}.{/if}
    </p>
    <div class="decide">
      <button class="btn primary" type="button" disabled={deciding} onclick={() => decide(true)}>Allow</button>
      <button class="btn" type="button" disabled={deciding} bind:this={refuseButton} onclick={() => decide(false)}>Refuse</button>
    </div>
  </div>
{:else if refused && !pairing}
  <div class="row">
    <span class="what">
      <span>Pairing refused</span>
      <span class="note">Make a new code to pair a device.</span>
    </span>
    {#if current?.listening}<button class="btn" type="button" onclick={startPairing}>Pair a device</button>{/if}
  </div>
{:else if pairing}
  <div class="pairing" aria-live="polite">
    <p class="note lead">Enter this code on the device. It works once.</p>
    <div class="code-line">
      <span class="code mono">{pairing.code}</span>
      <span class="note">expires in {countdown(pairing.expires_at, now)}</span>
    </div>
    {#if pairing.replaces}
      <p class="note warn">Pairing replaces {pairing.replaces}. It keeps working until the new device pairs.</p>
    {/if}
    <p class="note gap">Or paste this pairing link into the device:</p>
    <div class="link-line">
      <input class="well mono" readonly value={pairing.link} aria-label="Pairing link" onfocus={(event) => event.currentTarget.select()} />
      <button class="btn small" type="button" onclick={copyLink}>{copied ? "Copied" : "Copy"}</button>
    </div>
    <p class="note gap-sm">Certificate <span class="mono">SHA-256 {pairing.fingerprint}</span>. The device checks it, so a look-alike server on the network cannot stand in.</p>
    <div class="actions"><button class="btn" type="button" onclick={cancelPairing}>Cancel</button></div>
  </div>
{:else if current?.listening && !device && !refused}
  <div class="row">
    <span class="what">
      <span>No device is paired</span>
      <span class="note">Pairing gives one device access until you revoke it.</span>
    </span>
    <button class="btn" type="button" onclick={startPairing}>Pair a device</button>
  </div>
{/if}

{#if device && !pairing && !request}
  <div class="row device">
    <span class="what">
      <span class="name">{device.name}</span>
      <span class="note">{deviceNote(device, now)}</span>
    </span>
    <span class="device-actions">
      <span class="presence" class:on={current?.connected}>
        <span class="state-dot {current?.connected ? 'running' : 'queued'}" aria-hidden="true"></span>{current?.connected ? "connected" : "not connected"}
      </span>
      {#if !confirmRevoke}<button class="btn danger" type="button" onclick={() => (confirmRevoke = true)}>Revoke</button>{/if}
    </span>
  </div>
  {#if confirmRevoke}
    <div class="confirm">
      <p>Revoke {device.name}? It is disconnected now and has to pair again to get back in.</p>
      <span class="buttons">
        <button class="btn danger" type="button" onclick={revoke}>Revoke</button>
        <!-- svelte-ignore a11y_autofocus -->
        <button class="btn" type="button" autofocus onclick={() => (confirmRevoke = false)}>Keep it</button>
      </span>
    </div>
  {:else if current?.listening}
    <div class="row short">
      <span class="note">Pairing another device replaces this one.</span>
      <button class="link small-link" type="button" onclick={startPairing}>Pair a different device</button>
    </div>
  {/if}
{/if}

<style>
  h3 {
    margin: 16px 0 4px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: var(--bar);
    padding: 4px 0;
    font-size: var(--text-md);
  }

  .row.short {
    min-height: var(--control-lg);
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

  p {
    margin: 0;
  }

  .mono {
    font-family: var(--font-mono);
  }

  select {
    width: 200px;
    height: var(--control);
    flex: none;
    padding: 0 8px;
    font-size: var(--text-sm);
  }

  input[type="checkbox"] {
    width: 16px;
    height: 16px;
    flex: none;
    margin: 0;
    accent-color: var(--accent);
  }

  /* Locked while remote control is on: say so without hiding the values. */
  .field:disabled,
  select:disabled {
    border-color: var(--border);
    background: var(--surface);
    color: var(--muted);
    cursor: not-allowed;
  }

  .fields {
    display: flex;
    flex: none;
    gap: 8px;
  }

  .field.address {
    width: 132px;
    font-size: var(--text-sm);
  }

  .field.port {
    width: 68px;
    flex: none;
    font-size: var(--text-sm);
  }

  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 4px 0 8px;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .status .word {
    color: var(--text);
  }

  .error-text,
  .error-text .word {
    color: var(--error);
  }

  /* A decision: the approval card's amber-tinted rule, on the ground. */
  .request {
    margin: 4px 0;
    padding: 12px;
    border: 1px solid color-mix(in srgb, var(--warning) 45%, var(--border));
    border-radius: var(--radius-sm);
    background: var(--bg);
  }

  .request-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .request-title {
    min-width: 0;
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  /* The countdown never wraps beside a long name. */
  .time {
    flex: none;
    margin-left: auto;
    white-space: nowrap;
  }

  .decide {
    display: flex;
    gap: 8px;
    margin-top: 12px;
  }

  .warn {
    margin-top: 8px;
    color: var(--warning);
    font-weight: 500;
  }

  /* Inside the dialog's card the next level is a well on the ground. */
  .pairing {
    margin: 4px 0;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
  }

  .lead {
    margin-bottom: 4px;
    color: var(--secondary);
  }

  .code-line {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }

  .code {
    font-size: var(--text-lg);
    font-weight: 600;
    line-height: 24px;
  }

  .gap {
    margin: 12px 0 4px;
  }

  .gap-sm {
    margin-top: 8px;
  }

  .link-line {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .well {
    flex: 1;
    min-width: 0;
    height: 24px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--text);
    font-size: var(--text-sm);
    text-overflow: ellipsis;
  }

  .btn.small {
    height: 24px;
    padding: 0 8px;
  }

  .btn.danger {
    color: var(--error);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 12px;
  }

  .device {
    border-top: 1px solid var(--border);
  }

  .name {
    font-weight: 600;
  }

  .device-actions {
    display: flex;
    flex: none;
    align-items: center;
    gap: 12px;
  }

  .presence {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .presence.on {
    color: var(--accent);
  }

  .confirm {
    padding: 12px 0 4px;
    border-top: 1px solid var(--border);
  }

  .confirm p {
    margin-bottom: 8px;
  }

  .buttons {
    display: flex;
    gap: 8px;
  }

  .small-link {
    font-size: var(--text-sm);
  }
</style>
