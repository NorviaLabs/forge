import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Built into ui/dist (release) or ui/dist-self-test (`--mode self-test`),
// which tauri.conf.json / tauri.self-test.conf.json embed. The self-test
// probes are imported only when the mode is "self-test", so the release
// bundle does not contain them. Relative asset paths: the page is served
// from tauri://localhost, never a dev server in builds.
export default defineConfig(({ mode }) => ({
  base: "./",
  plugins: [svelte()],
  build: {
    outDir: mode === "self-test" ? "dist-self-test" : "dist",
    emptyOutDir: true,
    target: "safari16",
  },
}));
