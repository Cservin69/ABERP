// Svelte 5 mount entry — `mount` replaces `new App({ target })` from
// Svelte 4. The runtime drops the older constructor surface; we use
// the new shape from day one rather than relying on a legacy shim.
//
// `import.meta.env.DEV` lets us catch the "no #app element" case
// during dev without a mystery silent-mount failure in prod.

import { mount } from "svelte";

import "./lib/tokens.css";
import App from "./App.svelte";

const target = document.getElementById("app");
if (!target) {
  // Loud per CLAUDE.md rule 12 — a missing mount node would otherwise
  // produce a blank window with no console clue.
  throw new Error("ABERP UI: #app element missing from index.html");
}

mount(App, { target });
