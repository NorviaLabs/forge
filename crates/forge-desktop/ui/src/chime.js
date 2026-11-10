// The attention chime (doc 2, D12): two short notes synthesized with Web
// Audio, so there is no audio file to bundle and the CSP needs no media-src.
//
// A webview may refuse to start audio before the first user gesture. The
// context is created and resumed on the first pointer or key press; a chime
// before that may be silent, while the dock badge still updates.

const SETTING = "forge.desktop.attentionSound";

let context = null;

export function soundEnabled(storage = globalThis.localStorage) {
  try {
    return storage?.getItem(SETTING) !== "off";
  } catch {
    return true;
  }
}

export function setSoundEnabled(on, storage = globalThis.localStorage) {
  try {
    storage?.setItem(SETTING, on ? "on" : "off");
  } catch {}
}

function audio() {
  const Context = globalThis.AudioContext ?? globalThis.webkitAudioContext;
  if (!Context) return null;
  context ??= new Context();
  if (context.state === "suspended") context.resume().catch(() => {});
  return context;
}

// Call once at startup: unlocks audio on the first gesture.
export function unlockOnFirstGesture(target = globalThis.window) {
  const unlock = () => {
    audio();
    target.removeEventListener("pointerdown", unlock, true);
    target.removeEventListener("keydown", unlock, true);
  };
  target.addEventListener("pointerdown", unlock, true);
  target.addEventListener("keydown", unlock, true);
}

export function chime() {
  const ctx = audio();
  if (!ctx) return;
  const start = ctx.currentTime;
  // A rising fifth, short and soft: noticeable without being an alarm.
  for (const [offset, frequency] of [[0, 660], [0.12, 990]]) {
    const tone = ctx.createOscillator();
    const gain = ctx.createGain();
    tone.type = "sine";
    tone.frequency.value = frequency;
    gain.gain.setValueAtTime(0.0001, start + offset);
    gain.gain.exponentialRampToValueAtTime(0.18, start + offset + 0.015);
    gain.gain.exponentialRampToValueAtTime(0.0001, start + offset + 0.22);
    tone.connect(gain).connect(ctx.destination);
    tone.start(start + offset);
    tone.stop(start + offset + 0.25);
  }
}
