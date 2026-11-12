// What the settings show of a project's stored sandbox grants: one group per
// kind, in the order a person would look for them, empty groups left out.

const GROUPS = [
  { kind: "host", field: "hosts", label: "Can reach" },
  { kind: "write", field: "writes", label: "Can push or change" },
  { kind: "write_dir", field: "write_dirs", label: "Can write outside the project" },
  { kind: "trusted_program", field: "trusted_programs", label: "Runs outside the sandbox" },
];

export function grantGroups(project) {
  return GROUPS.map(({ kind, field, label }) => ({ kind, label, items: project?.[field] ?? [] }))
    .filter((group) => group.items.length > 0);
}

export function projectName(path) {
  const parts = String(path ?? "").split("/").filter(Boolean);
  return parts.at(-1) ?? String(path ?? "");
}

// The list after `value` of `kind` was taken back from `project`; a project
// with nothing left drops out.
export function withoutGrant(projects, project, kind, value) {
  const group = GROUPS.find((entry) => entry.kind === kind);
  if (!group) return projects;
  return projects
    .map((entry) => entry.project === project
      ? { ...entry, [group.field]: entry[group.field].filter((item) => item !== value) }
      : entry)
    .filter((entry) => grantGroups(entry).length > 0);
}
