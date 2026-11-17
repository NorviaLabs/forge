// Where a session runs, as the page words it. A remote target is
// `{ host, path, ask }` from the backend; a local session has none.

function trimmedPath(path) {
  const trimmed = (path ?? "").replace(/\/+$/, "");
  return trimmed === "" ? "/" : trimmed;
}

/** `devbox:/srv/app`, or `devbox:…/billing/api` for a deeper folder. */
export function compactTarget(target) {
  if (!target) return "";
  const path = trimmedPath(target.path);
  const parts = path.split("/").filter(Boolean);
  const shown = parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : path;
  return `${target.host}:${shown}`;
}

/** The head chip: `Runs on devbox:…/billing/api`, or just the host when narrow. */
export function targetChip(target, narrow) {
  if (!target) return "";
  return narrow ? target.host : `Runs on ${compactTarget(target)}`;
}

/** The full `host:path`, for tooltips and the popover. */
export function targetTitle(target) {
  if (!target) return "";
  return `${target.host}:${trimmedPath(target.path)}`;
}

/** Why a remote folder cannot be used, or `""`. */
export function folderError(folder, host) {
  const value = (folder ?? "").trim();
  if (!value) return `Enter the folder on ${host}.`;
  if (!value.startsWith("/")) return "Use an absolute path, starting with /.";
  return "";
}

/** The `target` argument of `create_session`, or `null` for this Mac. */
export function targetRequest({ runOn, folder, ask }) {
  if (!runOn || runOn === "local") return null;
  return { host: runOn, path: (folder ?? "").trim(), ask: Boolean(ask) };
}

/** The approval card's heading. */
export function approvalHeading(approval) {
  return approval?.target ? `Run this command on ${approval.target}?` : "Needs your approval";
}

/** Why Files, Changes, Git, GitHub and the terminal are off. */
export function offHere(target) {
  if (!target) return "";
  return `This session works in ${trimmedPath(target.path)} on ${target.host}. Files, Changes, Git, GitHub and the terminal show this Mac, so they are off.`;
}
