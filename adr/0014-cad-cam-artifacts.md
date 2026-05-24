# ADR-0014 — CAD/CAM artifact storage

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0005, ADR-0006, ADR-0007

## Scope

The CNC company produces and consumes CAD files (parts, assemblies) and CAM
files (toolpaths, post-processed G-code). These artifacts are bound to
products, work orders, and revisions. ABERP must store them, version them,
bind them to inventory and order state, and serve them to operators and
(eventually) machines.

Decisions to be made:

- **Storage model**: content-addressed blob store (BLAKE3 or SHA-256 hash as
  the address), with metadata in DuckDB pointing into it. Deduplicates
  identical files across revisions and across customers.
- **Revisioning**: every artifact change creates a new content-addressed
  object and a new revision row. Old revisions are never overwritten.
- **Approval workflow**: who can promote a revision from draft to released;
  audit-ledger entry per state.
- **Format awareness**: ABERP does not parse CAD/CAM internals at first; it
  treats them as opaque blobs with metadata. Format-specific features
  (preview render, dimension extraction) come later if needed.
- **Confidentiality**: customer CAD is often trade secret. At-rest encryption
  per ADR-0007 applies; export and sharing require explicit capability.

## Open questions

- Where blobs physically live — inside the per-tenant directory next to the
  DuckDB file vs. a separate blob directory with its own backup policy.
- Maximum artifact size — affects backup and sync design.
- Machine-side delivery (USB stick, direct network) — separate concern,
  later.

## Not in scope

- CAD viewer / editor functionality.
- CAM post-processor logic.
