// Ambient module declarations for non-TS imports the SPA pulls in.
// Vite handles `.css` imports as side-effect modules at build time; the
// TS compiler needs a hint so `import "./lib/tokens.css"` typechecks.

declare module "*.css";

// ADR-0110 D7 — Vite's `?raw` query hands back a module's source as a string.
// `durability-alert-banner.test.ts` uses it to pin the durability banner's
// markup contract (role="alert", the deliberately off-palette red, no dismiss
// control) without pulling a DOM test stack into this package. Declared here
// rather than reaching for `@types/node` + `readFileSync`, which would add a
// dependency to assert something Vite already provides.
declare module "*?raw" {
  const source: string;
  export default source;
}
