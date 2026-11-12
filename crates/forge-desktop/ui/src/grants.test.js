import { test } from "node:test";
import assert from "node:assert/strict";
import { grantGroups, projectName, withoutGrant } from "./grants.js";

const project = {
  project: "/Users/u/code/app",
  hosts: ["api.example.com"],
  writes: ["github.com/o/app"],
  write_dirs: [],
  trusted_programs: ["docker"],
};

test("a project's grants show by kind, empty kinds left out", () => {
  assert.deepEqual(grantGroups(project).map((group) => [group.kind, group.items]), [
    ["host", ["api.example.com"]],
    ["write", ["github.com/o/app"]],
    ["trusted_program", ["docker"]],
  ]);
  assert.equal(projectName(project.project), "app");
});

test("taking a grant back removes it, and a project with nothing left", () => {
  const other = { ...project, project: "/Users/u/code/other" };
  let projects = withoutGrant([project, other], project.project, "host", "api.example.com");
  assert.deepEqual(projects[0].hosts, []);
  assert.deepEqual(projects[1].hosts, ["api.example.com"]);
  projects = withoutGrant(projects, project.project, "write", "github.com/o/app");
  projects = withoutGrant(projects, project.project, "trusted_program", "docker");
  assert.deepEqual(projects.map((entry) => entry.project), ["/Users/u/code/other"]);
});
