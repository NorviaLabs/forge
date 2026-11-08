// Slash-command filtering for the composer. Only the command token is
// completed; arguments stay in the operator's hands.
export function withSkills(commands, skills) {
  return [
    ...commands,
    ...skills.map(([name, help]) => ({ name: `/${name}`, help }))
      .filter((skill) => !commands.some((command) => command.name === skill.name)),
  ];
}

export function filterSlashCommands(commands, value) {
  if (!value.startsWith("/") || value.includes(" ") || value.includes("\n")) return [];
  const needle = value.toLowerCase();
  return commands
    .filter((command) => command.name.toLowerCase().startsWith(needle))
    .slice(0, 8);
}

// Decide what a key does while the slash menu is open. Returns null when the
// key should proceed normally (notably Enter on an exact command, which
// submits and runs it).
export function slashKeyAction(event, { open, matches, index, value }) {
  if (event.isComposing || event.shiftKey || event.altKey) return null;
  if (!open || !matches.length) return null;
  if (event.key === "ArrowDown") return { type: "move", index: (index + 1) % matches.length };
  if (event.key === "ArrowUp") return { type: "move", index: (index - 1 + matches.length) % matches.length };
  if (event.key === "Tab") return { type: "complete", command: matches[index] };
  if (event.key === "Escape") return { type: "hide" };
  if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
    const match = matches[index];
    return match && value.trim() !== match.name
      ? { type: "complete", command: match }
      : null;
  }
  if (event.key === "Enter" && !event.metaKey && !event.ctrlKey) {
    const match = matches[index];
    if (!match) return null;
    // An exact command submits here; a prefix completes first.
    return value.trim() === match.name
      ? { type: "submit" }
      : { type: "complete", command: match };
  }
  return null;
}
