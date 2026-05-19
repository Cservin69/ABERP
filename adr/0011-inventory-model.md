# ADR-0011 — Inventory model

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0005, ADR-0006

## Scope

The core inventory module: products, variants, lots, serial numbers, stocking
locations, movements. Designed for the CNC company's first real-world use:
raw stock, work-in-progress, finished goods, consumables (tooling, fluids),
plus traceability for serialized parts.

Decisions to be made:

- **Stock representation**: balance-at-rest vs. event-sourced movements with
  a derived balance. Strong leaning toward event-sourced (every movement is
  a `mvt_` entry; current stock is a projection). Reason: matches the audit
  ledger discipline, makes reconciliation trivial.
- **Unit of measure** handling: base unit per product, conversions explicit,
  no implicit rounding.
- **Reservation model**: how an order reserves stock without removing it;
  expiry of reservations.
- **Lot vs serial** boundaries: when a product is lot-tracked vs serial-tracked
  vs untracked; mixing within a SKU.
- **Negative stock policy**: refused by default; configurable per location
  with explicit capability.

## Open questions

- BOM (bill of materials) — own ADR or part of inventory?
- Work-order / production model — interacts with CAM (ADR-0014); separate ADR.
- Cycle count and stocktake flows.

## Not in scope

- Purchasing / supplier management (separate ADR).
- Demand planning / forecasting (not for v1).
