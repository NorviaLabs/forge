// Arrow keys recall prompts only at textarea boundaries, not while editing
// inside multiline text. Escape restores the draft saved before browsing.
export function historyKeyAction(event, { entries, value, start, end, index, draft }) {
  if (event.isComposing || event.shiftKey || event.ctrlKey || event.metaKey || event.altKey) return null;
  if (event.key === "Escape" && index !== null) return { value: draft, index: null, draft: "" };
  if (!entries.length || start !== end) return null;
  const up = event.key === "ArrowUp";
  const down = event.key === "ArrowDown";
  if ((!up && !down) || (up && value.slice(0, start).includes("\n"))
      || (down && value.slice(end).includes("\n")) || (down && index === null)) return null;
  const next = index === null ? entries.length - 1
    : (index + (up ? -1 : 1) + entries.length) % entries.length;
  return { value: entries[next], index: next, draft: index === null ? value : draft };
}
