import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri dev server settings: fixed port, no clearing of Rust errors from the terminal.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // Two entry points. Without the explicit input map Vite emits only index.html and the
  // bundled build ships no panel.html, so `WebviewUrl::App("panel.html")` 404s in a bundle
  // while working fine in dev off the dev server.
  build: {
    rollupOptions: {
      input: { main: "index.html", panel: "panel.html" },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
