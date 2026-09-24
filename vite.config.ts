// `vitest/config` re-exports Vite's `defineConfig` with the `test` key typed — one config for
// the build and the TypeScript tests (M3b 1b: vitest under jsdom; `pnpm test`).
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Tauri dev server settings: fixed port, no clearing of Rust errors from the terminal.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // One entry point, and it is not index.html. Without the explicit input map Vite looks for
  // index.html, emits nothing for panel.html, and `WebviewUrl::App("panel.html")` 404s in a
  // bundle while working in dev off the dev server (measured at M2a, when there were two).
  build: {
    rollupOptions: {
      input: { panel: "panel.html" },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  // The renderer's tests render components into jsdom with `../api` mocked; nothing reaches
  // Tauri. Reported as their own count beside the Rust one (CLAUDE.md), never added to it.
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.tsx"],
  },
});
