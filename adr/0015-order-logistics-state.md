# ADR-0015 — Order + logistics state machine

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0006, ADR-0008, ADR-0011, ADR-0012, ADR-0013

## Scope

Orders (sales and purchase) have explicit lifecycles: drafted, confirmed,
allocated, picked, packed, shipped, delivered, completed — with side-paths
for cancellation, return, and amendment. Logistics covers the physical
movement from packing to delivery: package, shipment, carrier, tracking.

Decisions to be made:

- **State machine encoding**: typed Rust enum + transition functions, not
  free-text statuses.
- **Allowed transitions** explicit per state; illegal transitions refused at
  the type level where possible.
- **Backorder handling** when inventory is short.
- **Carrier integration** abstraction — adapter per carrier, common port.
- **Tracking number provenance** — external IDs stored as fields, never as
  primary keys (ADR-0005).
- **Customer notification**: which transitions notify; minimal PII in
  notifications.

## Open questions

- Whether sales and purchase orders share a model or have separate ones.
- B2B vs B2C posture for the CNC company's customer mix.
- Returns / RMA workflows — separate ADR or part of this one.

## Not in scope

- Carrier-specific business logic (lives in adapters).
- Customer-facing tracking page (cloud surface, later).
