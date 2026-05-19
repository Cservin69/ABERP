# Architecture Decision Records (ADRs)

ADRs capture decisions that are hard or costly to reverse. They are the only
place where architectural decisions live. If a decision is not in an ADR, it has
not been made, regardless of what the code does.

## Numbering

Four-digit, monotonic, never reused. `0001`, `0002`, ... A deleted decision is
**superseded**, not removed; its file stays.

## Status lifecycle

```
Proposed → Accepted → (Deprecated | Superseded by NNNN)
```

- **Proposed** — drafted, not yet adversarially reviewed. Not safe to build against.
- **Accepted** — has passed at least one adversarial review.
- **Deprecated** — no longer applies; replacement not needed.
- **Superseded by NNNN** — replaced by another ADR. The old one stays for history; the new one references it.

A ticket in a tracker is not enough to change an ADR. An ADR is changed only by:

1. Editing it in-place if status is still `Proposed`.
2. Filing a superseding ADR if status is `Accepted` or later.

## Standard ADR template

```markdown
# ADR-NNNN — <title>

- **Status:** Proposed | Accepted | Deprecated | Superseded by NNNN
- **Date:** YYYY-MM-DD
- **Deciders:** <names>
- **Supersedes:** (optional) ADR-NNNN

## Context

What problem are we solving? What constraints apply? What did we already rule out and why?

## Decision

The decision, stated as a single declarative paragraph or short list. No hedging.

## Consequences

What gets easier. What gets harder. What we lock ourselves into.

## Adversarial review

What would a hostile auditor / red team / future maintainer say about this?
Each ADR must have at least three such concerns answered or explicitly accepted.

## Alternatives considered

Other options, and the specific reason they lost. "Simpler" is not a reason on its own.

## Open questions

Things not decided here that this ADR depends on, with the ADR number that will resolve them.
```

## Adversarial review cadence

- **Design phase** (now): every two weeks, all `Proposed` ADRs.
- **Build phase**: every release, plus any ADR touched since the last review.
- **Incident-triggered**: any production incident triggers a review of the ADRs covering the affected surface.

## Index

### Spine (foundational — change at your peril)

- [ADR-0001 — Backend language: Rust](0001-backend-language-rust.md)
- [ADR-0002 — Tenant isolation: database-per-tenant](0002-tenant-isolation-db-per-tenant.md)
- ~~[ADR-0003 — Storage abstraction with DuckDB as first backend](0003-storage-abstraction-duckdb-first.md)~~ — **superseded by ADR-0019**
- [ADR-0004 — Frontend: Tauri + Svelte local, cloud reserved](0004-frontend-tauri-svelte.md)
- [ADR-0005 — Universal ID scheme: prefixed ULIDs](0005-id-scheme-ulid.md)
- [ADR-0006 — Module boundaries and contracts](0006-module-boundaries.md)
- [ADR-0007 — Security baseline and threat model](0007-security-baseline.md)
- [ADR-0008 — Tamper-evident audit ledger](0008-audit-ledger.md)
- [ADR-0019 — Storage strategy: one trait, relational SoT, search-first projections, no foreign keys](0019-storage-strategy-no-fks.md) — *replaces 0003 and 0018*

### Module-level (stubs — to be filled in)

- [ADR-0009 — NAV invoice issuing](0009-nav-invoice-issuing.md) — *stub*
- [ADR-0010 — Billingo + NAV invoice ingestion (read path)](0010-invoice-ingestion.md) — *stub*
- [ADR-0011 — Inventory model](0011-inventory-model.md) — *stub*
- [ADR-0012 — QR / vignette labels and no-touch handling](0012-qr-labels-no-touch.md) — *stub*
- [ADR-0013 — Robotics handoff (label print + place)](0013-robotics-handoff.md) — *stub*
- [ADR-0014 — CAD/CAM artifact storage](0014-cad-cam-artifacts.md) — *stub*
- [ADR-0015 — Order + logistics state machine](0015-order-logistics-state.md) — *stub*
- [ADR-0016 — Cloud sync and remote UI](0016-cloud-sync.md) — *stub*

### Cross-cutting

- [ADR-0017 — ABERP design language](0017-design-language.md)
- ~~[ADR-0018 — Storage evolution toward search-first / document stores](0018-storage-evolution-search-first.md)~~ — **superseded by ADR-0019**

### Deferred (not yet filed — tracked so they don't fall through)

- ADR — Stack baseline (async runtime, error crate, logging crate, CLI crate). *Required before commit #1; called out in ADR-0001.*
- ADR — Wire protocol (gRPC vs HTTPS+JSON) for UI ↔ backend. *Required before commit #1; called out in ADR-0004.*
- ADR — Backup, encryption-at-rest key management, and offsite key escrow. *Called out in ADR-0007.*
- ADR — Data retention and GDPR erasure workflow. *Called out in ADR-0002.*
- ADR — LLM use policy (which paths use models, which providers, supply chain). *Called out in ADR-0007.*
- ADR — Specific font family selection (Hungarian diacritic coverage). *Called out in ADR-0017.*
- ADR — Print rendering path (browser print vs Rust-side PDF). *Called out in ADR-0017.*
