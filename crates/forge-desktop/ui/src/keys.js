// Shortcut labels for the platform the app is running on: "⌘K" on macOS,
// "Ctrl+K" elsewhere. Plain module so the wording is unit-testable.

export function isMac(platform = globalThis.navigator?.platform ?? "") {
  return /Mac|iPhone|iPad/.test(platform);
}

// The label for the primary modifier plus `key`, with Shift when asked.
export function chord(key, { shift = false, mac = isMac() } = {}) {
  if (mac) return `${shift ? "⇧" : ""}⌘${key}`;
  return `Ctrl+${shift ? "Shift+" : ""}${key}`;
}

// Whether a keydown is that chord: the primary modifier is ⌘ on macOS and
// Ctrl elsewhere, and no other modifier may be held.
export function isChord(event, key, { shift = false, mac = isMac() } = {}) {
  const primary = mac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
  return primary && !event.altKey && event.shiftKey === shift && event.key.toLowerCase() === key.toLowerCase();
}
