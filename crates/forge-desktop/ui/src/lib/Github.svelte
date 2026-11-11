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
      const result = await invoke(command, args);
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

  onMount(() => { load("github_issues"); });
</script>

<div class="overlay">
  <div class="github" role="dialog" aria-modal="true" aria-labelledby="github-title" tabindex="-1" use:modal={{ onClose }}>
    <header>
      <h2 id="github-title">GitHub</h2>
      <button class="btn" type="button" disabled={busy} onclick={() => load("github_issues")}>Refresh issues</button>
      <button class="icon-btn" type="button" aria-label="Close GitHub" onclick={onClose}><Icon name="close" size={13} /></button>
    </header>
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
    <div class="columns">
      <nav aria-label="GitHub issues">
        {#each visible as issue (issue.number)}
          <button class="issue" type="button" disabled={busy} onclick={() => load("github_details", { issueNumber: issue.number, prNumber: null })}>
            <span class="mono">#{issue.number}</span> {issue.title}
          </button>
        {/each}
        {#if !busy && visible.length === 0}<p>No matching open issues.</p>{/if}
      </nav>
      <section class="detail" aria-label="GitHub details">
        {#if details?.issue}
          <h3>#{details.issue.number} {details.issue.title}</h3>
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
          <h3>PR #{details.pr.number}: {details.pr.title}</h3>
          <p>{details.pr.state}{details.pr.draft ? " · Draft" : ""} · {details.pr.review || "No review decision"}</p>
          <p class="mono">{details.pr.head} → {details.pr.base}</p>
          <h3>CI checks</h3>
          {#each details.checks as check, index (index)}
            <div class="check">
              <span>{check.name} — {check.state} ({check.bucket})</span>
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
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; display: grid; place-items: center; background: rgba(5, 7, 9, 0.6); z-index: 30; }
  .github { display: flex; flex-direction: column; width: min(960px, calc(100vw - 48px)); height: min(740px, calc(100vh - 64px)); padding: 16px; border: 1px solid var(--border-strong); border-radius: var(--radius); background: var(--surface); box-shadow: var(--shadow); }
  header { display: flex; gap: 10px; align-items: center; }
  h2 { flex: 1; margin: 0; font-size: 15px; }
  h3 { margin: 16px 0 6px; font-size: 13px; }
  p { font-size: 12px; color: var(--secondary); line-height: 1.5; }
  form { display: flex; gap: 6px; margin-bottom: 12px; }
  .pr-number { width: 110px; }
  .columns { flex: 1; display: grid; grid-template-columns: minmax(180px, 1fr) minmax(0, 2fr); min-height: 0; }
  nav { overflow: auto; border-right: 1px solid var(--border); padding-right: 12px; }
  .detail { overflow: auto; padding: 0 14px; }
  .issue { display: block; width: 100%; border: 0; padding: 8px; border-radius: var(--radius-sm); background: transparent; color: var(--text); text-align: left; font-size: 12px; }
  .issue:hover { background: var(--hover); }
  .issue span { color: var(--accent); }
  .actions { display: flex; flex-wrap: wrap; gap: 6px; margin: 6px 0 10px; }
  .check { display: flex; align-items: center; gap: 8px; padding: 6px 0; font-size: 12px; }
  .check span { flex: 1; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; font: 11px/1.5 var(--font-mono); }
  .error { color: var(--error); }
</style>
