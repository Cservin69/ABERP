# ADR-0012 — QR / vignette labels and no-touch handling

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0005, ADR-0007, ADR-0008, ADR-0011

## Scope

Every physical item, lot, location, package, and shipment can be tagged with a
QR vignette. Scanning a vignette identifies the entity by ULID, authorizes a
state transition (move, pick, pack, ship, consume), and emits an audit-ledger
entry. The intent is **no-touch handling**: the operator (or robotics, ADR-0013)
moves things; the system records what moved by reading the QR.

Decisions to be made:

- **Label payload**: prefixed ULID alone vs. a signed payload with capability
  (to prevent forged scans). Likely signed for high-value flows, plain for
  display.
- **Label format**: QR with embedded URL (deep-link into the local backend)
  vs. raw payload.
- **Printer abstraction**: label printer port + driver adapters per vendor.
- **Vignette lifecycle**: minted → applied → in-use → retired. Audit entries
  for each.
- **Anti-counterfeit posture**: how a scanned label proves itself; how a
  reused or copied label is detected.

## Open questions

- Which label printer(s) the CNC company will use first — informs driver
  priority.
- Whether to use DataMatrix instead of QR for small parts (denser at small
  sizes).
- Outdoor / harsh-environment label durability.

## Not in scope

- Robotics handoff for label placement (ADR-0013).
- Customer-facing tracking pages (later cloud surface).
