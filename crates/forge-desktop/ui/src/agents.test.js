import { test } from "node:test";
import assert from "node:assert/strict";
import {
  agentGroups, agentNotice, capabilityLabel, emptyForm, firstProblem, formFromAgent, inputFromForm, sameForm,
  takenNames, toggleCapability, toolAllowed, toolNeeds, toolsOutsideMode, validateForm,
} from "./agents.js";

const catalog = [
  { name: "read_file", class: "read" },
  { name: "grep", class: "read" },
  { name: "edit", class: "write" },
  { name: "bash", class: "exec" },
  { name: "web_fetch", class: "network" },
  { name: "update_plan", class: "meta" },
  { name: "request_unconfined_retry", class: "meta" },
];

const agent = (name, scope, extra = {}) => ({
  name, scope, description: `${name} does things`, mode: ["read-only"], tools: ["read_file"],
  instructions: "Be careful.", file_name: `${name}.md`, path: `/x/${name}.md`, overrides_user: false, ...extra,
});

test("capabilities gate tool classes the way the backend does", () => {
  assert.ok(toolAllowed("read_file", "read", ["read-only"]));
  assert.ok(toolAllowed("update_plan", "meta", ["read-only"]));
  assert.ok(!toolAllowed("edit", "write", ["read-only"]));
  assert.ok(toolAllowed("edit", "write", ["write"]));
  assert.ok(!toolAllowed("bash", "exec", ["write"]));
  assert.ok(toolAllowed("bash", "exec", ["exec"]));
  assert.ok(!toolAllowed("web_fetch", "network", ["write", "exec"]));
  assert.ok(toolAllowed("web_fetch", "network", ["network"]));
  assert.ok(!toolAllowed("request_unconfined_retry", "meta", ["exec"]));
  assert.ok(toolAllowed("request_unconfined_retry", "meta", ["exec", "network"]));
  assert.ok(!toolAllowed("some_meta", "meta", ["write", "exec", "network"]));
  for (const name of ["spawn_agent", "send_message", "wait_agent", "background_run"]) {
    assert.ok(!toolAllowed(name, "exec", ["write", "exec", "network"]), name);
  }
  assert.equal(toolNeeds("edit", "write"), "needs write");
  assert.equal(toolNeeds("request_unconfined_retry", "meta"), "needs exec and network");
});

test("read only stands alone; other capabilities combine in a fixed order", () => {
  assert.deepEqual(toggleCapability(["read-only"], "exec"), ["exec"]);
  assert.deepEqual(toggleCapability(["exec"], "write"), ["write", "exec"]);
  assert.deepEqual(toggleCapability(["write", "exec"], "network"), ["write", "exec", "network"]);
  assert.deepEqual(toggleCapability(["write", "exec"], "read-only"), ["read-only"]);
  assert.deepEqual(toggleCapability(["write", "exec"], "write"), ["exec"]);
  assert.deepEqual(toggleCapability(["read-only"], "read-only"), []);
  assert.equal(capabilityLabel(["write", "exec"]), "Write, Exec");
});

test("narrowing the capabilities names the tools it takes away", () => {
  assert.deepEqual(toolsOutsideMode(["read_file", "edit", "bash", "mcp:x:y"], ["write"], catalog), ["bash"]);
  assert.deepEqual(toolsOutsideMode(["read_file"], ["read-only"], catalog), []);
});

test("an agent round-trips through the form, keeping unknown tools as text", () => {
  const source = agent("reviewer", "user", { tools: ["grep", "mcp:github:search", "edit"], mode: ["write"] });
  const form = formFromAgent(source, catalog);
  assert.deepEqual(form.tools, ["grep", "edit"]);
  assert.equal(form.otherTools, "mcp:github:search");
  const input = inputFromForm(form);
  assert.deepEqual(input.tools, ["grep", "edit", "mcp:github:search"]);
  assert.equal(input.instructions, "Be careful.");
  assert.ok(sameForm(form, formFromAgent(source, catalog)));
  assert.ok(!sameForm(form, { ...form, description: "changed" }));
  assert.ok(!sameForm(form, { ...form, scope: "project" }));
  assert.deepEqual(inputFromForm({ ...form, otherTools: " a, b  grep\nc " }).tools, ["grep", "edit", "a", "b", "c"]);
});

test("the editor explains every problem before saving", () => {
  const valid = { ...emptyForm("user"), name: "code-reviewer", description: "Reviews.", tools: ["grep"] };
  assert.deepEqual(validateForm(valid), {});
  const errors = (change, taken) => Object.keys(validateForm({ ...valid, ...change }, taken));
  assert.deepEqual(errors({ name: "" }), ["name"]);
  assert.deepEqual(errors({ name: "Code Reviewer" }), ["name"]);
  assert.deepEqual(errors({ name: "a--b" }), ["name"]);
  assert.deepEqual(errors({ name: "a".repeat(65) }), ["name"]);
  assert.deepEqual(errors({}, ["code-reviewer"]), ["name"]);
  assert.deepEqual(errors({ description: "  " }), ["description"]);
  assert.deepEqual(errors({ mode: [] }), ["mode"]);
  assert.deepEqual(errors({ mode: ["read-only", "write"] }), ["mode"]);
  assert.deepEqual(errors({ tools: [] }), ["tools"]);
  assert.deepEqual(errors({ tools: [], otherTools: "mcp:x:y" }), [], "a typed tool counts");
});

test("the list groups by scope and keeps overridden user agents, marked", () => {
  const view = {
    project_name: "demo", project_dir: "/demo/.forge/agents", user_dir: "/u/agents",
    agents: [agent("shared", "project", { overrides_user: true }), agent("zeta", "user"), agent("alpha", "user")],
    overridden: [agent("shared", "user")],
    issues: [],
  };
  const [project, user] = agentGroups(view);
  assert.equal(project.label, "Project · demo");
  assert.deepEqual(project.agents.map((entry) => entry.name), ["shared"]);
  assert.ok(project.agents[0].overrides_user);
  assert.equal(user.label, "User");
  assert.deepEqual(user.agents.map((entry) => [entry.name, Boolean(entry.overridden)]), [["alpha", false], ["shared", true], ["zeta", false]]);
  assert.deepEqual(agentGroups(null), []);

  assert.deepEqual(takenNames(view, "user").sort(), ["alpha", "shared", "zeta"]);
  assert.deepEqual(takenNames(view, "user", { scope: "user", file_name: "alpha.md" }).sort(), ["shared", "zeta"]);
  assert.deepEqual(takenNames(view, "project"), ["shared"]);
});

test("the session notice counts skipped files and stays dismissed until warnings change", () => {
  assert.equal(agentNotice([], ""), null);
  assert.equal(agentNotice(undefined, ""), null);
  const warnings = ["Skipped project agent file /p/a.md: bad", "agent `x`: tool `y` is not available and was ignored"];
  const notice = agentNotice(warnings, "");
  assert.equal(notice.title, "1 custom agent file was skipped");
  assert.deepEqual(notice.lines, warnings);
  assert.equal(agentNotice(warnings, notice.key), null, "dismissed");
  assert.ok(agentNotice([...warnings, "Skipped user agent file /u/b.md: bad"], notice.key), "new warnings return");
  assert.equal(agentNotice(["Skipped a", "Skipped b"], "").title, "2 custom agent files were skipped");
  assert.equal(agentNotice(["agent `x`: tool `y` is not available"], "").title, "Some custom agents need attention");
});

test("a failed save points at the first problem in form order", () => {
  assert.equal(firstProblem({}), null);
  assert.equal(firstProblem(undefined), null);
  const valid = { ...emptyForm("user"), name: "code-reviewer", description: "Reviews.", tools: ["grep"] };
  const taken = validateForm(valid, ["code-reviewer"]);
  assert.deepEqual(firstProblem(taken), { field: "name", message: "An agent named code-reviewer already exists here." });
  const several = validateForm({ ...valid, description: "", tools: [] });
  assert.equal(firstProblem(several).field, "description", "description comes before tools");
  assert.equal(firstProblem({ tools: "x", mode: "y" }).field, "mode");
});
