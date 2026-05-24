# ADR-0004 — Frontend: Tauri + Svelte local, cloud reserved

- **Status:** Accepted (cornerstone — pre-decided)
- **Date:** 2026-05-19
- **Deciders:** Ervin

## Context

ABERP's first user experience is a local desktop application on the operator's
workstation. Later it must also be reachable through a cloud UI (e.g., when
the operator is away or a second user joins). The same backend code must
serve both.

## Decision

- **Local UI:** Tauri shell wrapping a Svelte single-page application.
- **Cloud UI:** Separate TypeScript codebase, framework TBD when we build it,
  consuming the same backend command/query API as the local UI.
- The UI **never accesses the storage layer directly**. All UI ↔ backend
  traffic goes over a defined wire protocol (gRPC or HTTPS+JSON; decision in
  a later ADR before commit #1). This is true even when the UI and backend
  are in the same Tauri process — we route through the wire format to keep
  the cloud topology identical to the local one in shape.
- **Authentication on local is real.** The Tauri shell holds a session token
  obtained from the backend on launch. The token has the same shape and
  lifecycle as a cloud token. We do not have a "trust the local process"
  shortcut.

## Consequences

- A small per-call serialization cost locally. Accepted in exchange for shape
  parity with cloud.
- Cloud UI is reachable to design today (API contract is the contract; the UI
  is just one client). We do not need to defer cloud-readiness work into the
  build phase.
- Hot-reload during development uses the same wire protocol; we don't have a
  dev-only shortcut that bypasses it.
- The Svelte side stays thin — display, input validation, optimistic UI for
  user comfort. Business rules live in Rust modules.

## Adversarial review

- *"Why pay serialization cost in-process?"* — Because the failure mode of
  having a fast path in local and a slow path on cloud is "we'll fix it later"
  followed by years of subtle bugs in the cloud path. The cost is in
  microseconds for typed structs.
- *"Why Svelte and not React?"* — Smaller bundle, simpler reactivity, less
  framework churn. A future revisit is allowed if the Svelte ecosystem stops
  serving us. The choice is reversible; only the *separation* between UI and
  backend is the locked decision.
- *"What stops a malicious Svelte build from reading the user's filesystem
  through Tauri?"* — Tauri's allow-list. We expose only the specific commands
  ABERP needs. No `fs::all` permissions. Detail in ADR-0007.
- *"What if the cloud UI needs a feature the local UI does not?"* — Then the
  backend gains a new command. UIs differ in what they expose, not in what
  the backend knows how to do.

## Alternatives considered

- **Electron + React** — heavier runtime, history of supply-chain incidents.
- **Native (egui, Slint, etc.)** — appealing for a fully-Rust stack, but
  the ERP UI surface is large and forms-heavy; web tech wins on velocity.
- **PWA only, no desktop shell** — loses OS integration (label-printer drivers,
  filesystem for CAD/CAM artifacts, OS keychain). Refused.

## Open questions

- Exact wire protocol — gRPC vs HTTPS+JSON. Decided before commit #1.
- Component library on the Svelte side — TBD; we will not pull one in until
  there is real UI to test against it.
- Cloud UI framework — decided when cloud build begins, not before.
