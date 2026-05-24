// Ambient module declarations for non-TS imports the SPA pulls in.
// Vite handles `.css` imports as side-effect modules at build time; the
// TS compiler needs a hint so `import "./lib/tokens.css"` typechecks.

declare module "*.css";
