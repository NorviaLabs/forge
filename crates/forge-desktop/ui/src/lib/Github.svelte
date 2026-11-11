<script>
  import { onMount } from "svelte";
  import { invoke } from "../ipc.js";
  import Markdown from "../Markdown.svelte";
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";
  let {
    onClose,
    sessionId = "",
    sessionIssue = null,
    sessionPr = "",
    onCreateSession = () => {},
    onChanged = () => {},
    embedded = false,
  } = $props();
  let issues = $state([]);
  let details = $state(null);
  let logs = $state(null);
  let busy = $state(false);
  let error = $state("");
  let filter = $state("");
  let pr = $state("");
  const visible = $derived(issues.filter((issue) => `${issue.number} ${issue.title}`.toLowerCase().includes(filter.toLowerCase())));

  async function load(command, args = {}) {
    if (busy) return;
    busy = true;
    error = "";
    try {
      const result = await invoke(command, { sessionId, ...args });
      if (command === "github_issues") issues = result;
      else if (command === "github_details") { details = result; logs = null; }
      else logs = result;
    } catch (cause) { error = String(cause); }
    finally { busy = false; }
  }

  async function act(command, issueNumber) {
    if (busy) return;
    busy = true;
    error = "";
    try {
      await invoke(command, { sessionId, issueNumber });
      onChanged();
    } catch (cause) { error = String(cause); }
    finally { busy = false; }
  }

  function startSession(issue) {
    onCreateSession(issue.number, issue.title);
  }

  function runReference(link) {
    const match = link?.match(/^https:\/\/github\.com\/[^/]+\/[^/]+\/actions\/runs\/(\d+)(?:\/job\/(\d+))?/);
    return match ? { runId: Number(match[1]), jobId: match[2] ? Number(match[2]) : null } : null;
  }

  // A check's state as a dot class and a word, from gh's bucket.
  function checkState(check) {
    if (check.bucket === "pass") return ["completed", "passed"];
    if (check.bucket === "fail") return ["failed", "failed"];
    if (check.bucket === "cancel") return ["cancelled", "cancelled"];
    if (check.bucket === "skipping") return ["", "skipped"];
    if (String(check.state).toUpperCase() === "IN_PROGRESS") return ["running", "running"];
    return ["queued", "pending"];
  }

  onMount(() => { load("github_issues"); });
</script>

{#snippet body()}
    <header>
      <h2 id="github-title">GitHub</h2>
      <button class="btn" type="button" disabled={busy} onclick={() => load("github_issues")}>Refresh issues</button>
      {#if !embedded}<button class="icon-btn" type="button" aria-label="Close GitHub" onclick={onClose}><Icon name="close" size={13} /></button>{/if}
    </header>
    <div class="top">
    <p class="hint">
      Issues, pull requests and CI from this workspace's remote. Requires <code>gh auth login</code>.
      {#if sessionIssue}
        This session is linked to issue #{sessionIssue}{#if sessionPr} · <span class="mono">{sessionPr}</span>{/if}.
      {:else}
        Start a session from an issue to enable push, PR and feedback actions.
      {/if}
    </p>
    <form onsubmit={(event) => { event.preventDefault(); load("github_details", { issueNumber: null, prNumber: Number(pr) }); }}>
      <input class="field" bind:value={filter} placeholder="Filter issues" aria-label="Filter GitHub issues" />
      <input class="field pr-number" bind:value={pr} placeholder="PR number" aria-label="Pull request number" inputmode="numeric" />
      <button class="btn" type="submit" disabled={busy || !Number.isSafeInteger(Number(pr)) || Number(pr) < 1}>Load PR</button>
    </form>
    {#if busy}<p role="status">Loading GitHub…</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    </div>
    <div class="columns">
      <nav aria-label="GitHub issues">
        {#each visible as issue (issue.number)}
          <button class="issue" type="button" disabled={busy} onclick={() => load("github_details", { issueNumber: issue.number, prNumber: null })}>
            <span class="mono">#{issue.number}</span>
            <span class="issue-title">{issue.title}</span>
            {#if issue.state}<span class="issue-state">{issue.state.toLowerCase()}</span>{/if}
          </button>
        {/each}
        {#if !busy && visible.length === 0}<p>No matching open issues.</p>{/if}
      </nav>
      <section class="detail" aria-label="GitHub details">
        {#if details?.issue}
          <h3 class="item-title"><span class="mono">#{details.issue.number}</span> {details.issue.title}</h3>
          <p>{details.issue.state}</p>
          <div class="actions">
            <button class="btn" type="button" disabled={busy} onclick={() => startSession(details.issue)}>
              Start session for this issue
            </button>
            {#if sessionIssue === details.issue.number}
              <button class="btn primary" type="button" disabled={busy} onclick={() => act("github_create_pr", details.issue.number)}>
                Push and create PR
              </button>
              <button class="btn" type="button" disabled={busy} onclick={() => act("github_refresh_pr", details.issue.number)}>
                Refresh PR
              </button>
              <button class="btn" type="button" disabled={busy} onclick={() => act("github_apply_feedback", details.issue.number)}>
                Apply PR feedback
              </button>
            {/if}
          </div>
          <Markdown text={details.issue.body} />
        {/if}
        {#if details?.pr}
          <h3 class="item-title">PR <span class="mono">#{details.pr.number}</span>: {details.pr.title}</h3>
          <p>{details.pr.state}{details.pr.draft ? " · Draft" : ""} · {details.pr.review || "No review decision"}</p>
          <p class="mono">{details.pr.head} → {details.pr.base}</p>
          <h3>CI checks</h3>
          {#each details.checks as check, index (index)}
            {@const [dot, word] = checkState(check)}
            <div class="check">
              <span class="check-state"><span class="state-dot {dot}" aria-hidden="true"></span>{word}</span>
              <span class="check-name">{check.name}</span>
              <span class="check-raw mono">{check.state}</span>
              {#if runReference(check.link)}
                <button class="btn" type="button" disabled={busy} onclick={() => load("github_logs", runReference(check.link))}>Logs</button>
              {/if}
            </div>
          {/each}
          {#if details.checks_error}<p class="error">{details.checks_error}</p>
          {:else if details.checks.length === 0}<p>No checks reported.</p>{/if}
        {:else if details}<p>No linked pull request found.</p>{/if}
        {#if logs}
          <h3>CI logs</h3>
          <pre>{logs.text}</pre>
          {#if logs.truncated}<p>Log truncated at 64 KiB.</p>{/if}
        {/if}
        {#if !details}<p>Select an issue or enter a pull request number.</p>{/if}
      </section>
    </div>
{/snippet}

<!-- A screen beside Session and Files when embedded; a dialog otherwise. -->
{#if embedded}
  <section class="github embedded" aria-labelledby="github-title">
    {@render body()}
  </section>
{:else}
  <div class="overlay">
    <div class="github" role="dialog" aria-modal="true" aria-labelledby="github-title" tabindex="-1" use:modal={{ onClose }}>
      {@render body()}
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    z-index: 30;
  }

  .github {
    display: flex;
    flex-direction: column;
    width: min(960px, calc(100vw - 48px));
    height: min(740px, calc(100vh - 64px));
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
  }

  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .top {
    flex: none;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
  }

  .top p {
    margin: 0 0 12px;
  }

  .top p:last-child {
    margin: 12px 0 0;
  }

  /* Section labels. */
  h3 {
    margin: 16px 0 8px;
    color: var(--muted);
    font-size: var(--text-xs);
    font-weight: 400;
    letter-spacing: 0;
  }

  h3.item-title {
    margin: 16px 0 4px;
    color: var(--text);
    font-size: var(--text-md);
    font-weight: 600;
    line-height: 20px;
    overflow-wrap: anywhere;
  }

  .item-title .mono {
    color: var(--muted);
    font-size: inherit;
    font-weight: 400;
  }

  p {
    margin: 8px 0;
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 18px;
    overflow-wrap: anywhere;
  }

  code {
    font-size: var(--text-sm);
  }

  form {
    display: flex;
    gap: 8px;
  }

  form .btn {
    height: var(--control-lg);
  }

  .pr-number {
    width: 112px;
    flex: none;
  }

  .columns {
    display: grid;
    flex: 1;
    grid-template-columns: minmax(200px, 1fr) minmax(0, 2fr);
    min-height: 0;
  }

  nav {
    min-width: 0;
    padding: 8px;
    overflow: auto;
    border-right: 1px solid var(--border);
  }

  nav p {
    margin: 4px 8px;
    color: var(--muted);
  }

  .issue {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
    width: 100%;
    height: var(--control-lg);
    padding: 0 8px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text);
    font-size: var(--text-sm);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }

  .issue:hover:not(:disabled) {
    background: var(--hover);
  }

  .issue:focus-visible {
    outline-offset: -2px;
  }

  .issue .mono {
    color: var(--muted);
  }

  .issue-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .issue-state {
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .detail {
    min-width: 0;
    padding: 0 16px 16px;
    overflow: auto;
  }

  .detail > p:first-child {
    margin-top: 16px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin: 8px 0 16px;
  }

  .check {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: var(--control-lg);
    border-bottom: 1px solid var(--border);
    font-size: var(--text-sm);
  }

  .check-state {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 8px;
    width: 80px;
    color: var(--secondary);
    font-size: var(--text-xs);
  }

  .check-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .check-raw {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }

  .check .btn {
    flex: none;
    height: 24px;
    padding: 0 8px;
  }

  /* The log well. */
  pre {
    max-height: 320px;
    margin: 0;
    padding: 12px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 18px;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }

  .error {
    color: var(--error);
  }

  @media (prefers-reduced-motion: reduce) {
    .issue {
      transition: none;
    }
  }

  .github.embedded {
    flex: 1;
    width: auto;
    height: auto;
    min-height: 0;
    border: 0;
    border-radius: 0;
    background: var(--bg);
    box-shadow: none;
  }
</style>
