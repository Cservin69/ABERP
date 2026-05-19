# ADR-0010 — Billingo + NAV invoice ingestion (read path)

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0001 … 0008, ADR-0009

## Scope

Pull historical and ongoing in/outgoing invoices from Billingo and NAV into
the local DuckDB store. The DuckDB store is a **cache and projection**, not
the source of truth: NAV is authoritative for submitted invoices, Billingo
is authoritative for what Billingo holds, ABERP becomes authoritative for
invoices it issues itself (ADR-0009).

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

- Should ingestion run continuously or only on operator request, during the
  early phase? Recommendation: scheduled + manual trigger.
- How long to retain ingested raw blobs vs parsed projections.
- Privacy/PII implications of pulling customer data we did not originate.

## Not in scope

- Issuing invoices (ADR-0009).
- Payment reconciliation (separate ADR, not yet filed).
