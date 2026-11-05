import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Built into ui/dist, which tauri.conf.json embeds. Relative asset paths:
// the page is served from tauri://localhost, never a dev server in builds.
export default defineConfig({
  base: "./",
  plugins: [svelte()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "safari16",
  },
});
