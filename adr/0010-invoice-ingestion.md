# ADR-0010 — Billingo + NAV invoice ingestion (read path)

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0001 … 0008, ADR-0009

## Scope

Pull invoices and related entities (customers, contacts) from Billingo
and from NAV into the local DuckDB store. Two purposes, with very
different design weight:

1. **Billingo as migration source (primary).** Ervin's framing: Billingo
   is *not a permanent integration*. It is a one-time bulk-import path
   so existing clients and historical invoices do not have to be
   re-entered manually, plus a client-acquisition convenience for
   prospects already on Billingo who want to switch to ABERP. After
   migration, ABERP's own native setup is authoritative; Billingo is
   no longer in the loop for that tenant. Design accordingly — do
   **not** over-engineer ongoing two-way sync.
2. **NAV ingestion (secondary).** Pull historical and ongoing submitted
   invoices from NAV's Online Számla read API for audit, dispute, and
   reconciliation. NAV remains authoritative for the submission state
   of invoices it has acknowledged.

The DuckDB store for ingested invoices is a **cache and projection**,
not the source of truth: NAV is authoritative for its acknowledged
invoices, Billingo is authoritative for what Billingo holds *during
migration only*, ABERP becomes authoritative for invoices it issues
itself (ADR-0009).

Decisions to be made:

- The polling vs webhook model for each source.
- How we detect and surface **divergence** between ABERP-issued and
  NAV-acknowledged states (the loud-failure principle).
- How we ingest historical invoices and bind them to internal entities
  (customers, products) without inventing identity.
- Whether ingested invoices have a different ULID prefix or share `inv_`
  (likely share, with a `source` field).
- Replay safety: ingestion is idempotent on (source, source_id).

## Open questions

- **Billingo:** does the migration import customers only, invoices only,
  or both? Recommendation: both, in two passes (customers first so
  invoice references resolve).
- **Billingo:** what is the "done with Billingo" signal — explicit
  operator action, or automatic after first ABERP-issued invoice?
- **NAV read path:** continuous polling or operator-triggered fetch
  during early phase? Recommendation: scheduled + manual trigger.
- How long to retain ingested raw blobs vs parsed projections.
- Privacy/PII implications of pulling customer data we did not originate.

## Not in scope

- Issuing invoices (ADR-0009).
- Payment reconciliation (separate ADR, not yet filed).
