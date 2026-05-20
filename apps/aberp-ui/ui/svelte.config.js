import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

// Svelte 5 with runes. We do NOT pin `runes: true` globally —
// Svelte 5 auto-detects on a per-component basis when a component
// uses `$state` / `$derived` / `$effect`. The legacy mode is
// available for any component that wants it without ceremony.
export default {
  preprocess: vitePreprocess(),
};
