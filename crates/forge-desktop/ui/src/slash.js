// Slash-command filtering for the composer. Only the command token is
// completed; arguments stay in the operator's hands.
export function filterSlashCommands(commands, value) {
  if (!value.startsWith("/") || value.includes(" ") || value.includes("\n")) return [];
  const needle = value.toLowerCase();
  return commands
    .filter((command) => command.name.startsWith(needle))
    .slice(0, 8);
}

// Decide what a key does while the slash menu is open. Returns null when the
// key should proceed normally (notably Enter on an exact command, which
// submits and runs it).
export function slashKeyAction(event, { open, matches, index, value }) {
  if (!open || !matches.length) return null;
  if (event.key === "ArrowDown") return { type: "move", index: (index + 1) % matches.length };
  if (event.key === "ArrowUp") return { type: "move", index: (index - 1 + matches.length) % matches.length };
  if (event.key === "Tab") return { type: "complete", command: matches[index] };
  if (event.key === "Escape") return { type: "hide" };
  if (event.key === "Enter" && !event.metaKey && !event.ctrlKey) {
    const match = matches[index];
    if (match && value.trim() !== match.name) return { type: "complete", command: match };
  }
  return null;
}
