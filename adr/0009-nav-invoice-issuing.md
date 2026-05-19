# ADR-0009 — NAV invoice issuing

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0001 … 0008

## Scope

The first production surface of ABERP: create an invoice in ABERP, submit it
to the NAV Online Számla system, store the response (verbatim + parsed),
finalize the invoice, and serve it back to the operator and the customer.

The decisions that must be made before code is written for this module:

- Which NAV API version we target, and how we track NAV schema changes.
- How the per-tenant, per-series, per-fiscal-year **sequence allocator** works
  (transactional, gap-free, replay-safe).
- How invoice amendments and storno are modelled — referenced by ULID, never
  by sequence number reuse.
- mTLS certificate handling: where the certificate lives, how it rotates,
  how the operator is notified before expiry.
- Idempotency on NAV submission: NAV's `transactionId` and our internal
  idempotency key, and how retries are bounded.
- What goes in the audit ledger on every state transition (draft → ready →
  submitted → ack-received → finalized → amended/storno).
- The certification / audit posture: which NAV-issued conformance documents
  we will obtain, and on what schedule.

## Open questions

- Does ABERP issue invoices directly from day one, or use Billingo as a
  transitional submission path? Default: direct from day one — the user
  has stated ABERP should be the source of truth.
- Currency handling — HUF-only at first, multi-currency from when?
- Self-billing flows (NAV supports them) — in scope?

## Adversarial review (placeholder)

To be expanded when the decisions are filled in. Concerns already on the
table:

- *"NAV API version drift will break submissions silently."* — Mitigation
  shape: schema-version-checked at startup; refusal to submit if mismatch.
- *"Sequence-number allocator must survive crash mid-allocation."* — Mitigation
  shape: allocator-as-saga in the audit ledger; the burned number is
  reconciled on restart.
- *"What about the NAV inspector visit scenario (offline submission)?"* —
  Mitigation shape: queue + replay, with a hard upper bound and operator
  notification.

## Not in scope

- Billingo migration / backfill — ADR-0010.
- Multi-tenant invoice numbering across separate fiscal regimes — far future.
