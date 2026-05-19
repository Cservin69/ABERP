# ABERP

Modular multi-tenant ERP. Rust backend, Tauri + Svelte local UI, cloud UI later.
First production surface: NAV-compliant invoicing for a single tenant.
First real-world user: a CNC manufacturing company (inventory, logistics, CAD/CAM).

This repository currently holds **design only** — no code yet. The order of operations
is deliberate: foundation, then ADRs, then build. We do not start coding until the
spine is approved and adversarially reviewed.

## Layout

```
ABERP/
  README.md           ← you are here
  FOUNDATION.md       ← the architectural spine — every ADR must be consistent with it
  adr/
    README.md         ← ADR index, numbering, status lifecycle, review cadence
    0001-*.md         ← spine ADRs (foundational, hard to reverse)
    ...
    0009-*.md         ← module ADRs (stubs first, then filled in)
```

## Reading order

1. `FOUNDATION.md` — the spine. Read this first.
2. `adr/README.md` — how ADRs work in this project.
3. The numbered ADRs — read in order; later ADRs assume earlier ones.

## Working principles (non-negotiable)

These come from the project's working agreement and apply to every change:

- **Think before coding.** State assumptions; don't guess.
- **Simplicity first.** Minimum code, no speculative abstractions.
- **Surgical changes.** Touch only what the task requires.
- **Goal-driven.** Define success criteria up front, loop until verified.
- **Use the model for judgment, not for routing or deterministic transforms.**
- **Surface conflicts, don't average them.** Two patterns? Pick one explicitly.
- **Read before you write.** No duplicate functions next to identical ones.
- **Tests verify intent, not just behavior.** A test that can't fail when business logic changes is wrong.
- **Match codebase conventions.** Don't fork patterns silently.
- **Fail loud.** "Completed successfully" with 14% silently skipped is the worst class of bug.

## Status

Pre-build. Foundation drafting in progress. Code commit #1 has not landed.
