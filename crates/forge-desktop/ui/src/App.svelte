<script>
  // Walking skeleton (K1): proves the pipe end to end. The real transcript,
  // composer and approvals are K2. All state comes from Rust over one
  // channel; this page only shows it.
  import { onMount } from "svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";

  let sessionId = $state("");
  let turnState = $state("idle");
  let messages = $state(0);
  let streamed = $state("");
  let lastError = $state("");
  let prompt = $state("");

  onMount(async () => {
    const channel = new Channel();
    let selfTest = false;
    channel.onmessage = (event) => {
      if (event.type === "stream" && event.event.type === "text_delta") {
        streamed += event.event.text;
      } else if (event.type === "session") {
        turnState = event.turn_state;
        messages = event.messages;
      } else if (event.type === "error") {
        lastError = event.message;
      }
    };
    const hello = await invoke("subscribe", { channel });
    sessionId = hello.session_id;
    selfTest = hello.self_test;
    if (selfTest) {
      await invoke("submit_prompt", { text: "self-test ping" });
    }
  });

  async function send() {
    const text = prompt;
    prompt = "";
    try {
      await invoke("submit_prompt", { text });
    } catch (error) {
      lastError = String(error);
    }
  }

  async function stop() {
    try {
      await invoke("stop_turn");
    } catch (error) {
      lastError = String(error);
    }
  }
</script>

<main>
  <h1>Forge</h1>
  <p class="meta">Session {sessionId || "…"} · {turnState} · {messages} messages</p>
  {#if lastError}<p class="error">{lastError}</p>{/if}
  <pre class="stream">{streamed}</pre>
  <form onsubmit={(event) => { event.preventDefault(); send(); }}>
    <textarea bind:value={prompt} rows="3" aria-label="Prompt"></textarea>
    <div class="actions">
      <button type="submit" disabled={!prompt.trim()}>Send</button>
      <button type="button" onclick={stop}>Stop</button>
    </div>
  </form>
</main>

<style>
  main { font-family: system-ui, sans-serif; margin: 1.5rem; }
  .meta { color: #666; }
  .error { color: #b00020; }
  .stream { white-space: pre-wrap; min-height: 4rem; }
  textarea { width: 100%; box-sizing: border-box; }
  .actions { display: flex; gap: 0.5rem; margin-top: 0.5rem; }
</style>
