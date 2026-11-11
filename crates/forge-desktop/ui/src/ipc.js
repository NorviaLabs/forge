// The page's one way to call the backend: Tauri's `invoke`, with a typed
// rejection turned into a `CommandFailure` (see failure.js).
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { commandFailure } from "./failure.js";

export async function invoke(name, args) {
  try {
    return await tauriInvoke(name, args);
  } catch (cause) {
    throw commandFailure(cause);
  }
}
