import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri's dev server expects a fixed port + non-clearing TTY behaviour.
// We do NOT pull in @tauri-apps/api/vite — keeping the config small and
// independent of Tauri-version-specific Vite plugins per CLAUDE.md
// rule 2.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: "127.0.0.1",
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: true,
  },
});
