// Custom agents in Settings: how the list groups them, which tools each set
// of capabilities allows, what the editor checks before saving, and the
// notice a session shows for agent files that did not load.
//
// The rules mirror forge-config's `agents` module; the backend validates
// every save again, so these only make the editor helpful, not safe.

export const CAPABILITIES = [
  { id: "read-only", label: "Read only", hint: "Reads files and searches. Runs alongside other agents." },
  { id: "write", label: "Write", hint: "Edits files and uses git, in its own worktree." },
  { id: "exec", label: "Exec", hint: "Runs shell commands in the sandbox, in its own worktree." },
  { id: "network", label: "Network", hint: "Fetches web pages; its commands may reach allowed hosts." },
];

// Tools never given to an agent: agents cannot start agents.
export const NEVER_DELEGATED = ["spawn_agent", "send_message", "wait_agent", "background_run"];
const ALWAYS_META = ["ask_user_question", "update_plan"];
const UNCONFINED_RETRY = "request_unconfined_retry";

export const NAME_MAX = 64;
const NAME_PATTERN = /^[a-z0-9]+(-[a-z0-9]+)*$/;

// Whether `mode` (a list of capability ids) lets an agent use a tool of
// `toolClass` (read, write, exec, network or meta).
export function toolAllowed(name, toolClass, mode) {
  if (NEVER_DELEGATED.includes(name)) return false;
  const has = (capability) => mode.includes(capability);
  if (name === UNCONFINED_RETRY) return has("exec") && has("network");
  switch (toolClass) {
    case "read": return true;
    case "write": return has("write");
    case "exec": return has("exec");
    case "network": return has("network");
    case "meta": return ALWAYS_META.includes(name);
    default: return false;
  }
}

// What a disabled tool needs, in words, for its checkbox.
export function toolNeeds(name, toolClass) {
  if (name === UNCONFINED_RETRY) return "needs exec and network";
  if (["write", "exec", "network"].includes(toolClass)) return `needs ${toolClass}`;
  return "not available to agents";
}

// Tick or untick a capability. `read-only` stands alone: choosing it clears
// the others, and choosing any other clears it.
export function toggleCapability(mode, capability) {
  if (mode.includes(capability)) return mode.filter((entry) => entry !== capability);
  if (capability === "read-only") return ["read-only"];
  return CAPABILITIES.map((entry) => entry.id)
    .filter((id) => id !== "read-only" && (id === capability || mode.includes(id)));
}

// Built-in tools the new mode no longer allows, so the editor can drop them
// and say so.
export function toolsOutsideMode(tools, mode, catalog) {
  return tools.filter((name) => {
    const tool = catalog.find((entry) => entry.name === name);
    return tool && !toolAllowed(tool.name, tool.class, mode);
  });
}

export function emptyForm(scope = "project") {
  return { scope, name: "", description: "", mode: ["read-only"], tools: [], otherTools: "", instructions: "" };
}

// An agent as the editor edits it. Tools the built-in catalog does not know
// (MCP tools, typos) go into the free-text field so nothing is lost.
export function formFromAgent(agent, catalog) {
  const known = new Set(catalog.map((tool) => tool.name));
  return {
    scope: agent.scope,
    name: agent.name,
    description: agent.description,
    mode: [...agent.mode],
    tools: agent.tools.filter((name) => known.has(name)),
    otherTools: agent.tools.filter((name) => !known.has(name)).join(", "),
    instructions: agent.instructions,
  };
}

export function parseOtherTools(text) {
  return String(text ?? "").split(/[\s,]+/).map((name) => name.trim()).filter(Boolean);
}

// What `save_agent` receives.
export function inputFromForm(form) {
  const tools = [...form.tools];
  for (const name of parseOtherTools(form.otherTools)) if (!tools.includes(name)) tools.push(name);
  return {
    name: form.name.trim(),
    description: form.description.trim(),
    mode: [...form.mode],
    tools,
    instructions: form.instructions.trim(),
  };
}

// Problems to fix before saving, by field; empty when the form can be saved.
// `taken` lists the names already used in the target scope by other agents.
export function validateForm(form, taken = []) {
  const errors = {};
  const name = form.name.trim();
  if (!name) errors.name = "Give the agent a name.";
  else if (name.length > NAME_MAX) errors.name = `Use at most ${NAME_MAX} characters.`;
  else if (!NAME_PATTERN.test(name)) errors.name = "Use lowercase letters, digits and single hyphens, like code-reviewer.";
  else if (taken.includes(name)) errors.name = `An agent named ${name} already exists here.`;
  if (!form.description.trim()) errors.description = "Say when the model should use this agent.";
  if (form.mode.length === 0) errors.mode = "Choose at least one capability.";
  else if (form.mode.includes("read-only") && form.mode.length > 1) errors.mode = "Read only cannot be combined with other capabilities.";
  if (inputFromForm(form).tools.length === 0) errors.tools = "Allow at least one tool.";
  return errors;
}

// The fields a save can fail on, in form order.
export const FIELD_ORDER = ["name", "description", "mode", "tools"];

// The first field with a problem and its message, or null. A failed save
// moves focus to that field and repeats the message beside the button.
export function firstProblem(errors) {
  const field = FIELD_ORDER.find((name) => errors?.[name]);
  return field ? { field, message: errors[field] } : null;
}

export function sameForm(left, right) {
  return JSON.stringify(inputFromForm(left)) === JSON.stringify(inputFromForm(right)) && left.scope === right.scope;
}

// The list's groups: the project on screen, then the user's own agents.
// Overridden user agents stay listed, marked, so they can still be edited.
export function agentGroups(view) {
  if (!view) return [];
  const project = view.agents.filter((agent) => agent.scope === "project");
  const user = [
    ...view.agents.filter((agent) => agent.scope === "user"),
    ...view.overridden.map((agent) => ({ ...agent, overridden: true })),
  ].sort((left, right) => left.name.localeCompare(right.name));
  return [
    { scope: "project", label: `Project · ${view.project_name}`, dir: view.project_dir, agents: project },
    { scope: "user", label: "User", dir: view.user_dir, agents: user },
  ];
}

// Names other agents already use in `scope`, leaving out the one being
// edited.
export function takenNames(view, scope, editing = null) {
  if (!view) return [];
  return [...view.agents, ...view.overridden]
    .filter((agent) => agent.scope === scope)
    .filter((agent) => !(editing && agent.scope === editing.scope && agent.file_name === editing.file_name))
    .map((agent) => agent.name);
}

export function capabilityLabel(mode) {
  return mode.map((id) => CAPABILITIES.find((entry) => entry.id === id)?.label ?? id).join(", ");
}

// The session notice for agent warnings, or null when there are none or the
// operator dismissed exactly these. A new warning brings it back.
export function agentNotice(warnings, dismissedKey) {
  if (!warnings?.length) return null;
  const key = warnings.join("\n");
  if (key === dismissedKey) return null;
  const skipped = warnings.filter((warning) => warning.startsWith("Skipped ")).length;
  const title = skipped
    ? `${skipped} custom agent file${skipped === 1 ? " was" : "s were"} skipped`
    : "Some custom agents need attention";
  return { key, title, lines: warnings };
}
