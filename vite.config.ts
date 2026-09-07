import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri dev server settings: fixed port, no clearing of Rust errors from the terminal.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
