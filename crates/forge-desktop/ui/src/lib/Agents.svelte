<script>
  // Custom agents: specialists the model starts by name. Lists the project's
  // and the user's agents, edits every field of one, and repairs or deletes a
  // file that did not load. Forge writes the markdown files; nobody has to.
  import { onMount, tick } from "svelte";
  import Icon from "./Icon.svelte";
  import { modal } from "../modal.js";
  import { invoke } from "../ipc.js";
  import {
    CAPABILITIES, agentGroups, capabilityLabel, emptyForm, firstProblem, formFromAgent, inputFromForm, sameForm,
    takenNames, toggleCapability, toolAllowed, toolNeeds, toolsOutsideMode, validateForm,
  } from "../agents.js";

  let { onClose } = $props();

  let view = $state(null);
  let catalog = $state([]);
  let loadError = $state("");
  let busy = $state(false);
  let error = $state("");
  let message = $state("");
  // The list, the editor for one agent, or a broken file's text.
  let screen = $state("list");
  // The file being edited; null while creating.
  let editing = $state(null);
  let form = $state(emptyForm());
  let original = $state(emptyForm());
  let submitted = $state(false);
  let removedNote = $state("");
  let source = $state({ scope: "project", ref: null, fileName: "", reason: "", text: "", original: "" });
  // `${scope}:${file_name}` awaiting a delete confirmation.
  let confirmDelete = $state("");
  // Where to go once unsaved changes are discarded: "close" or "list".
  let confirmDiscard = $state("");

  const groups = $derived(agentGroups(view));
  const errors = $derived(validateForm(form, takenNames(view, form.scope, editing)));
  const dirty = $derived(
    screen === "edit" ? !sameForm(form, original) : screen === "source" ? source.text !== source.original : false,
  );
  const title = $derived(
    screen === "edit" ? (editing ? `Edit ${original.name}` : "New agent") : screen === "source" ? `Fix ${source.fileName}` : "Agents",
  );
  const shown = (field) => submitted && errors[field];
  // Ticked tools the capabilities do not allow (a file written by hand can
  // list them). They stay untickable, and the agent is started without them.
  const ignored = $derived(screen === "edit" ? toolsOutsideMode(form.tools, form.mode, catalog) : []);

  // A confirmation appears where the operator is looking and takes focus on
  // its safe choice, even when the form is scrolled away from it.
  function reveal(node) {
    node.scrollIntoView({ block: "nearest" });
    node.querySelector("[data-safe]")?.focus();
  }

  // Where focus goes for each field a save can fail on: a failed save moves
  // focus to the first problem (scrolling it into view) and says so by the
  // button.
  const FIELD_SELECTORS = {
    name: "#agent-name",
    description: "#agent-description",
    mode: "#agent-mode input",
    tools: "#agent-tools input:not(:disabled), #agent-other-tools",
  };
  const problem = $derived(firstProblem(errors));
  let form_node = $state(null);

  // Escape, the back arrow and the close button: with the discard question
  // open, Escape answers it with "Keep editing".
  function dismiss() {
    if (confirmDiscard) confirmDiscard = "";
    else leave("close");
  }

  async function refresh() {
    view = await invoke("list_agents");
  }

  onMount(async () => {
    try {
      [view, catalog] = await Promise.all([invoke("list_agents"), invoke("list_agent_tools")]);
    } catch (cause) {
      loadError = `Could not read agents: ${cause}`;
    }
  });

  // Closing or going back never silently drops an unsaved edit.
  function leave(target) {
    if (busy) return;
    if (dirty) {
      confirmDiscard = target;
      return;
    }
    go(target);
  }

  function go(target) {
    confirmDiscard = "";
    if (target === "close") onClose();
    else {
      screen = "list";
      error = "";
      submitted = false;
      removedNote = "";
    }
  }

  function startNew() {
    const scope = view?.project_trusted ? "project" : "user";
    form = emptyForm(scope);
    original = emptyForm(scope);
    editing = null;
    open("edit");
  }

  function startEdit(agent) {
    form = formFromAgent(agent, catalog);
    original = formFromAgent(agent, catalog);
    editing = { scope: agent.scope, file_name: agent.file_name };
    open("edit");
  }

  async function startSource(issue) {
    if (busy) return;
    busy = true;
    error = "";
    try {
      const ref = { scope: issue.scope, file_name: issue.file_name };
      const text = await invoke("read_agent_source", { agent: ref });
      source = { scope: issue.scope, ref, fileName: issue.file_name, reason: issue.reason, text, original: text };
      open("source");
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  function open(next) {
    screen = next;
    submitted = false;
    error = "";
    message = "";
    removedNote = "";
    confirmDelete = "";
  }

  function setCapability(id) {
    const next = toggleCapability(form.mode, id);
    const removed = toolsOutsideMode(form.tools, next, catalog);
    form.mode = next;
    if (removed.length) {
      form.tools = form.tools.filter((name) => !removed.includes(name));
      removedNote = `Unticked ${removed.join(", ")}: the capabilities no longer allow ${removed.length === 1 ? "it" : "them"}.`;
    } else {
      removedNote = "";
    }
  }

  function setTool(name, on) {
    form.tools = on ? [...form.tools, name] : form.tools.filter((entry) => entry !== name);
  }

  async function save() {
    submitted = true;
    if (Object.keys(errors).length) {
      await tick();
      const selector = problem && FIELD_SELECTORS[problem.field];
      const field = selector && form_node?.querySelector(selector);
      field?.focus();
      field?.scrollIntoView?.({ block: "center" });
      return;
    }
    busy = true;
    error = "";
    try {
      const saved = await invoke("save_agent", { agent: inputFromForm(form), scope: form.scope, previous: editing });
      await refresh();
      screen = "list";
      message = `Saved ${saved.name} to ${saved.path}.`;
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  async function saveSource() {
    busy = true;
    error = "";
    try {
      const saved = await invoke("save_agent_source", { scope: source.scope, previous: source.ref, text: source.text });
      await refresh();
      screen = "list";
      message = `Saved ${saved.name}; it loads now.`;
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  async function remove(target, name) {
    busy = true;
    error = "";
    try {
      await invoke("delete_agent", { agent: { scope: target.scope, file_name: target.file_name } });
      confirmDelete = "";
      await refresh();
      message = `Deleted ${name}.`;
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  const scopeWord = (scope) => (scope === "project" ? "Project" : "User");
  const key = (item) => `${item.scope}:${item.file_name}`;
  const locked = (scope) => scope === "project" && view && !view.project_trusted;
</script>

<div class="overlay">
  <div
    class="agents"
    role="dialog"
    aria-modal="true"
    aria-labelledby="agents-title"
    tabindex="-1"
    use:modal={{ onClose: dismiss, busy }}
  >
    <header>
      {#if screen !== "list"}
        <button class="icon-btn" type="button" aria-label="Back to agents" disabled={busy} onclick={() => leave("list")}>
          <Icon name="arrow-left" size={13} />
        </button>
      {/if}
      <h2 id="agents-title">{title}</h2>
      <button class="icon-btn" type="button" aria-label="Close agents" disabled={busy} onclick={() => leave("close")}>
        <Icon name="close" size={13} />
      </button>
    </header>

    <div class="body" inert={Boolean(confirmDiscard)}>

      {#if screen === "list"}
        <p class="intro">
          Custom agents are specialists the model can start by name when a task matches their description.
          Forge saves each one as a markdown file.
        </p>
        {#if loadError}<p class="error" role="alert">{loadError}</p>{:else if !view}<p class="note" role="status">Loading agents…</p>{/if}
        <div class="actions">
          <button class="btn primary" type="button" disabled={!view || busy} onclick={startNew}>
            <Icon name="plus" size={13} /> New agent
          </button>
        </div>
        {#if message}<p class="note" role="status">{message}</p>{/if}
        {#if error}<p class="error" role="alert">{error}</p>{/if}

        {#each groups as group (group.scope)}
          <section aria-labelledby={`agents-group-${group.scope}`}>
            <h3 id={`agents-group-${group.scope}`}>{group.label} <span class="count">{group.agents.length}</span></h3>
            <p class="path mono" title={group.dir}>{group.dir}</p>
            {#if locked(group.scope)}
              <p class="note">This project is not trusted, so Forge does not write agents into it.</p>
            {/if}
            {#if group.agents.length === 0}
              <p class="note">No {group.scope} agents yet.</p>
            {:else}
              <ul class="agent-list">
                {#each group.agents as agent (key(agent))}
                  <li class:overridden={agent.overridden}>
                    <div class="agent-head">
                      <span class="agent-name mono">{agent.name}</span>
                      <span class="chip">{scopeWord(agent.scope)}</span>
                      {#if agent.overrides_user}<span class="chip">Overrides user agent</span>{/if}
                      {#if agent.overridden}<span class="chip">Overridden by project</span>{/if}
                      <span class="spacer"></span>
                      <button
                        class="link"
                        type="button"
                        disabled={busy || locked(agent.scope)}
                        aria-label={`Edit ${agent.name} (${scopeWord(agent.scope)})`}
                        onclick={() => startEdit(agent)}
                      >Edit</button>
                      <button
                        class="link"
                        type="button"
                        disabled={busy || locked(agent.scope)}
                        aria-label={`Delete ${agent.name} (${scopeWord(agent.scope)})`}
                        onclick={() => (confirmDelete = key(agent))}
                      >Delete…</button>
                    </div>
                    <p class="agent-description">{agent.description}</p>
                    <p class="agent-meta">
                      {capabilityLabel(agent.mode)} · {agent.tools.length} tool{agent.tools.length === 1 ? "" : "s"}
                    </p>
                    {#if confirmDelete === key(agent)}
                      <div class="confirm" use:reveal>
                        <p>Delete {agent.name}? Forge removes <span class="mono">{agent.path}</span>. Agents already running keep running.</p>
                        <div class="actions">
                          <button class="btn danger" type="button" disabled={busy} onclick={() => remove(agent, agent.name)}>Delete agent</button>
                          <button class="btn" type="button" data-safe disabled={busy} onclick={() => (confirmDelete = "")}>Cancel</button>
                        </div>
                      </div>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </section>
        {/each}

        {#if view?.issues.length}
          <section aria-labelledby="agents-issues">
            <h3 id="agents-issues">Couldn't load <span class="count">{view.issues.length}</span></h3>
            <p class="note">These files were skipped, and sessions run without them. Fix the text or delete the file.</p>
            <ul class="agent-list">
              {#each view.issues as issue (key(issue) + issue.path)}
                <li class="issue">
                  <div class="agent-head">
                    <Icon name="alert" size={13} />
                    <span class="agent-name mono">{issue.file_name || issue.path}</span>
                    <span class="chip">{scopeWord(issue.scope)}</span>
                    <span class="spacer"></span>
                    {#if issue.file_name}
                      <button
                        class="link"
                        type="button"
                        disabled={busy || locked(issue.scope)}
                        aria-label={`Edit ${issue.file_name} as text`}
                        onclick={() => startSource(issue)}
                      >Edit as text</button>
                      <button
                        class="link"
                        type="button"
                        disabled={busy || locked(issue.scope)}
                        aria-label={`Delete ${issue.file_name}`}
                        onclick={() => (confirmDelete = key(issue))}
                      >Delete…</button>
                    {/if}
                  </div>
                  <p class="reason">{issue.reason}</p>
                  <p class="path mono" title={issue.path}>{issue.path}</p>
                  {#if confirmDelete === key(issue)}
                    <div class="confirm" use:reveal>
                      <p>Delete <span class="mono">{issue.path}</span>?</p>
                      <div class="actions">
                        <button class="btn danger" type="button" disabled={busy} onclick={() => remove(issue, issue.file_name)}>Delete file</button>
                        <button class="btn" type="button" data-safe disabled={busy} onclick={() => (confirmDelete = "")}>Cancel</button>
                      </div>
                    </div>
                  {/if}
                </li>
              {/each}
            </ul>
          </section>
        {/if}
      {:else if screen === "edit"}
        <form class="editor" novalidate bind:this={form_node} onsubmit={(event) => { event.preventDefault(); save(); }}>
          <fieldset>
            <legend>Scope</legend>
            <label class="choice">
              <input type="radio" name="agent-scope" value="project" bind:group={form.scope} disabled={busy || !view?.project_trusted} />
              <span><strong>Project · {view?.project_name}</strong>
                <span class="hint">{view?.project_trusted ? "Saved in this project's .forge/agents." : "Trust this project to save agents in it."}</span></span>
            </label>
            <label class="choice">
              <input type="radio" name="agent-scope" value="user" bind:group={form.scope} disabled={busy} />
              <span><strong>User</strong> <span class="hint">Yours, in every project. A project agent with the same name overrides it.</span></span>
            </label>
          </fieldset>

          <div class="field-row">
            <label for="agent-name">Name</label>
            <input
              id="agent-name"
              class="field mono"
              bind:value={form.name}
              placeholder="code-reviewer"
              spellcheck="false"
              autocomplete="off"
              disabled={busy}
              aria-invalid={Boolean(shown("name"))}
              aria-describedby="agent-name-help"
            />
            <p id="agent-name-help" class:field-error={shown("name")} class="hint">
              {shown("name") || "Lowercase letters, digits and hyphens. The model starts the agent by this name."}
            </p>
          </div>

          <div class="field-row">
            <label for="agent-description">Description</label>
            <textarea
              id="agent-description"
              class="field area"
              rows="3"
              bind:value={form.description}
              disabled={busy}
              aria-invalid={Boolean(shown("description"))}
              aria-describedby="agent-description-help"
            ></textarea>
            <p id="agent-description-help" class:field-error={shown("description")} class="hint">
              {shown("description") || "The model sees this with the name and uses it to decide when to start the agent."}
            </p>
          </div>

          <fieldset id="agent-mode" aria-describedby="agent-mode-help">
            <legend>Capabilities</legend>
            {#each CAPABILITIES as capability (capability.id)}
              <label class="choice">
                <input
                  type="checkbox"
                  checked={form.mode.includes(capability.id)}
                  disabled={busy}
                  onchange={() => setCapability(capability.id)}
                />
                <span><strong>{capability.label}</strong> <span class="hint">{capability.hint}</span></span>
              </label>
            {/each}
            <p id="agent-mode-help" class:field-error={shown("mode")} class="hint">
              {shown("mode") || "Reading is always allowed. Read only cannot be combined with the others."}
            </p>
            {#if removedNote}<p class="note" role="status">{removedNote}</p>{/if}
          </fieldset>

          <fieldset id="agent-tools" aria-describedby="agent-tools-help">
            <legend>Tools</legend>
            <p id="agent-tools-help" class:field-error={shown("tools")} class="hint">
              {shown("tools") || "The agent gets only the tools ticked here."}
            </p>
            {#if ignored.length}
              <p class="warning" role="status">
                The capabilities do not allow {ignored.join(", ")}; the agent starts without {ignored.length === 1 ? "it" : "them"}. Untick {ignored.length === 1 ? "it" : "them"}, or add the capability.
              </p>
            {/if}
            <div class="tool-grid">
              {#each catalog as tool (tool.name)}
                {@const allowed = toolAllowed(tool.name, tool.class, form.mode)}
                {@const ticked = form.tools.includes(tool.name)}
                <label class="tool" class:unavailable={!allowed} title={tool.description}>
                  <input
                    type="checkbox"
                    checked={ticked}
                    disabled={busy || (!allowed && !ticked)}
                    onchange={(event) => setTool(tool.name, event.currentTarget.checked)}
                  />
                  <span class="mono">{tool.name}</span>
                  {#if !allowed}
                    <span class="hint" class:warning={ticked}>{toolNeeds(tool.name, tool.class)}{ticked ? " · ignored" : ""}</span>
                  {/if}
                </label>
              {/each}
            </div>
            <label for="agent-other-tools">Other tools</label>
            <input
              id="agent-other-tools"
              class="field mono"
              bind:value={form.otherTools}
              placeholder="mcp:server:tool"
              spellcheck="false"
              autocomplete="off"
              disabled={busy}
              aria-describedby="agent-other-tools-help"
            />
            <p id="agent-other-tools-help" class="hint">MCP tools by name, separated by commas. A tool the agent's capabilities do not allow is ignored when it starts.</p>
          </fieldset>

          <div class="field-row">
            <label for="agent-instructions">Instructions</label>
            <textarea
              id="agent-instructions"
              class="field area mono"
              rows="10"
              bind:value={form.instructions}
              disabled={busy}
              spellcheck="false"
              aria-describedby="agent-instructions-help"
            ></textarea>
            <p id="agent-instructions-help" class="hint">Markdown. Added to the agent's system prompt when it starts.</p>
          </div>

          {#if submitted && problem}<p class="error" role="alert">Can't save yet: {problem.message}</p>{/if}
          {#if error}<p class="error" role="alert">{error}</p>{/if}
          <div class="actions">
            <button class="btn primary" type="submit" disabled={busy}>{editing ? "Save changes" : "Create agent"}</button>
            <button class="btn" type="button" disabled={busy} onclick={() => leave("list")}>Cancel</button>
          </div>
        </form>
      {:else}
        <form class="editor" novalidate onsubmit={(event) => { event.preventDefault(); saveSource(); }}>
          <p>
            <span class="mono">{source.fileName}</span> did not load:
            <span class="reason">{source.reason}</span>
          </p>
          <div class="field-row">
            <label for="agent-source">File text</label>
            <textarea
              id="agent-source"
              class="field area mono"
              rows="18"
              bind:value={source.text}
              spellcheck="false"
              disabled={busy}
              aria-describedby="agent-source-help"
            ></textarea>
            <p id="agent-source-help" class="hint">
              Forge checks the text before saving; text that still would not load is not written. The file takes the agent's name.
            </p>
          </div>
          {#if error}<p class="error" role="alert">{error}</p>{/if}
          <div class="actions">
            <button class="btn primary" type="submit" disabled={busy || !dirty}>Save file</button>
            <button class="btn" type="button" disabled={busy} onclick={() => leave("list")}>Cancel</button>
          </div>
        </form>
      {/if}
    </div>
    {#if confirmDiscard}
      <!-- Over the form rather than at its top, so it shows wherever the form
           is scrolled. -->
      <div class="discard-layer">
        <div class="discard" role="alertdialog" aria-modal="true" aria-labelledby="agents-discard" use:reveal>
          <p id="agents-discard">Discard your unsaved changes?</p>
          <div class="actions">
            <button class="btn danger" type="button" onclick={() => go(confirmDiscard)}>Discard changes</button>
            <button class="btn" type="button" data-safe onclick={() => (confirmDiscard = "")}>Keep editing</button>
          </div>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: start center;
    padding-top: 32px;
    background: color-mix(in srgb, var(--bg-deep) 70%, transparent);
    z-index: 35;
  }

  .agents {
    position: relative;
    display: flex;
    flex-direction: column;
    width: min(720px, calc(100vw - 48px));
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
    gap: 8px;
    height: var(--bar);
    flex: none;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
  }

  h2 {
    flex: 1;
    min-width: 0;
    margin: 0;
    overflow: hidden;
    font-size: var(--text-lg);
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .body {
    flex: 1;
    min-height: 0;
    padding: 8px 16px 16px;
    overflow-x: hidden;
    overflow-y: auto;
  }

  h3 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 20px 0 2px;
    font-size: var(--text-md);
    font-weight: 600;
  }

  .count {
    color: var(--muted);
    font-weight: 400;
  }

  p {
    margin: 8px 0;
    color: var(--secondary);
    font-size: var(--text-sm);
    line-height: 18px;
    overflow-wrap: anywhere;
  }

  .intro {
    margin-top: 8px;
  }

  .note,
  .hint {
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .path {
    margin: 0 0 8px;
    overflow: hidden;
    color: var(--muted);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .error,
  .reason,
  .field-error {
    color: var(--error);
  }

  .warning,
  .tool .hint.warning {
    color: var(--warning);
  }

  /* The discard question covers the dialog below its header, whatever the
     form's scroll position. */
  .discard-layer {
    position: absolute;
    inset: var(--bar) 0 0 0;
    display: grid;
    place-items: center;
    padding: 16px;
    background: color-mix(in srgb, var(--bg-deep) 60%, transparent);
  }

  .discard {
    width: min(360px, 100%);
    padding: 16px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .discard p {
    margin: 0;
    color: var(--text);
    font-size: var(--text-md);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-top: 12px;
  }

  .agent-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .agent-list li {
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
  }

  .agent-list li.overridden .agent-name,
  .agent-list li.overridden .agent-description {
    color: var(--muted);
  }

  .agent-list li.issue {
    border-color: color-mix(in srgb, var(--error) 45%, var(--border));
  }

  .agent-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  .agent-head :global(svg) {
    color: var(--error);
    flex: none;
  }

  .agent-name {
    color: var(--strong);
    font-size: var(--text-md);
    overflow-wrap: anywhere;
  }

  .spacer {
    flex: 1;
  }

  .agent-description {
    margin: 4px 0 0;
    color: var(--text);
  }

  .agent-meta {
    margin: 2px 0 0;
    color: var(--muted);
  }

  .confirm {
    margin-top: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }

  .editor {
    display: flex;
    flex-direction: column;
    gap: 16px;
    margin-top: 8px;
  }

  /* Block layout, not flex: WebKit lays out a flex fieldset's legend
     inconsistently, which showed as stray gaps between sections. A floated
     legend renders like an ordinary heading. */
  fieldset {
    display: block;
    min-width: 0;
    margin: 0;
    padding: 0;
    border: 0;
  }

  fieldset > * + * {
    margin-top: 6px;
  }

  legend {
    float: left;
    width: 100%;
  }

  legend + * {
    clear: both;
  }

  legend,
  .field-row > label,
  fieldset > label[for] {
    margin-bottom: 4px;
    padding: 0;
    color: var(--text);
    font-size: var(--text-md);
    font-weight: 600;
  }

  fieldset > label[for] {
    display: block;
    margin-top: 12px;
    margin-bottom: 4px;
  }

  fieldset > label[for] + * {
    margin-top: 0;
  }

  .field-row {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .field-row .hint,
  fieldset .hint {
    margin: 0;
  }

  fieldset > p.hint,
  fieldset > p.note,
  fieldset > p.warning {
    margin-top: 4px;
  }

  .area {
    height: auto;
    min-height: 64px;
    padding: 6px 10px;
    line-height: 20px;
    resize: vertical;
  }

  .field[aria-invalid="true"] {
    border-color: var(--error);
  }

  .choice {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    font-size: var(--text-md);
  }

  .choice input {
    flex: none;
    margin: 3px 0 0;
    accent-color: var(--accent);
  }

  .choice strong {
    font-weight: 500;
  }

  .tool-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 4px 12px;
  }

  /* A long name and its hint wrap inside the column, never past it. */
  .tool {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px 8px;
    min-width: 0;
    min-height: 24px;
    font-size: var(--text-sm);
  }

  .tool .mono {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .tool input {
    flex: none;
    margin: 0;
    accent-color: var(--accent);
  }

  .tool.unavailable .mono {
    color: var(--muted);
  }

  .tool .hint {
    padding-left: 22px;
    flex-basis: 100%;
    font-size: var(--text-xs);
  }
</style>
