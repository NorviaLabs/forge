<script>
  // Walking skeleton (K2): transcript, composer, Stop, approve-once/deny.
  // All state comes from Rust over one channel; this page only shows it.
  import { onMount } from "svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import Markdown from "./Markdown.svelte";

  let sessionId = $state("");
  let turnState = $state("idle");
  let messages = $state([]);
  let approval = $state(null);
  let streaming = $state("");
  let lastError = $state("");
  let prompt = $state("");

  function applySession(event) {
    turnState = event.turn_state;
    messages = event.messages;
    approval = event.pending_approval;
    // The transcript now holds what was streamed so far.
    streaming = "";
  }

  onMount(async () => {
    const channel = new Channel();
    channel.onmessage = (event) => {
      if (event.type === "stream" && event.event.type === "text_delta") {
        streaming += event.event.text;
      } else if (event.type === "session") {
        applySession(event);
      } else if (event.type === "error") {
        lastError = event.message;
      }
    };
    const hello = await invoke("subscribe", { channel });
    sessionId = hello.session_id;
    applySession(hello.initial);
    if (import.meta.env.MODE === "self-test" && hello.self_test) {
      const { runSelfTest } = await import("./selftest.js");
      runSelfTest(hello.self_test);
    }
  });

  async function call(command, args) {
    try {
      await invoke(command, args);
    } catch (error) {
      lastError = String(error);
    }
  }

  function send() {
    const text = prompt;
    prompt = "";
    call("submit_prompt", { text });
  }
</script>

<main>
  <header>
    <h1>Forge</h1>
    <p class="meta">Session {sessionId || "…"} · {turnState}</p>
  </header>
  {#if lastError}<p class="error" role="alert">{lastError}</p>{/if}

  <section class="transcript" aria-label="Transcript">
    {#each messages as message, index (index)}
      <article class="message {message.role}" data-role={message.role}>
        {#if message.role === "tool"}
          <pre>{message.text}</pre>
        {:else}
          <Markdown text={message.text} />
        {/if}
      </article>
    {/each}
    {#if streaming}
      <article class="message assistant streaming" data-role="assistant-streaming">
        <Markdown text={streaming} />
      </article>
    {/if}
  </section>

  {#if approval}
    <section class="approval" aria-label="Approval required">
      <h2>Approve {approval.tool}?</h2>
      <p>{approval.reason}</p>
      <pre>{approval.arguments}</pre>
      {#if approval.privileged}
        <p class="note">
          This runs outside the sandbox or opens network access. Approving it needs a
          system confirmation that arrives in a later release: approve it in the
          terminal, or deny it here.
        </p>
      {/if}
      <div class="actions">
        <button
          type="button"
          disabled={approval.privileged}
          onclick={() => call("approve_once", { callId: approval.call_id })}
        >
          Approve once
        </button>
        <button type="button" onclick={() => call("deny", { callId: approval.call_id })}>Deny</button>
      </div>
    </section>
  {/if}

  <form onsubmit={(event) => { event.preventDefault(); send(); }}>
    <textarea bind:value={prompt} rows="3" aria-label="Prompt"></textarea>
    <div class="actions">
      <button type="submit" disabled={!prompt.trim()}>Send</button>
      <button type="button" onclick={() => call("stop_turn")}>Stop</button>
    </div>
  </form>
</main>

<style>
  main { font-family: system-ui, sans-serif; margin: 1.5rem; max-width: 60rem; }
  .meta { color: #666; }
  .error { color: #b00020; }
  .note { color: #8a5a00; }
  .message { margin: 0.75rem 0; padding: 0.5rem 0.75rem; border-radius: 6px; }
  .message.user { background: #eef3ff; }
  .message.assistant { background: #f6f6f6; }
  .message.tool pre, .approval pre { white-space: pre-wrap; overflow-x: auto; }
  .approval { border: 2px solid #c77d00; border-radius: 6px; padding: 0.75rem; margin: 1rem 0; }
  textarea { width: 100%; box-sizing: border-box; }
  .actions { display: flex; gap: 0.5rem; margin-top: 0.5rem; }
</style>
