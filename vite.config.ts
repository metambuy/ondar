import { defineConfig } from "vite";
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
});
