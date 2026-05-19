# ADR-0005 — Universal ID scheme: prefixed ULIDs

- **Status:** Accepted
- **Date:** 2026-05-19
- **Deciders:** Ervin

## Context

Identifiers run through every module, every event, every audit entry, every
API call. Getting the ID scheme wrong is expensive to fix because every
existing record must be rewritten, every external integration re-keyed, and
every log re-indexed.

The scheme must satisfy:

1. **Globally unique without coordination** (no central sequence service).
2. **Time-sortable**, so cursor pagination, recency queries, and audit reads
   are cheap.
3. **No information leak** — a competitor or a customer counting invoices
   should not be able to estimate our volume.
4. **Compact enough to log without dread**, URL-safe, double-click-selectable.
5. **Recognizable per entity type** at a glance, so a misrouted ID is caught
   by reading.
6. **Independent of tenancy and hosting** — IDs must not embed the tenant or
   the storage backend.

External identifiers (NAV invoice number, Billingo invoice ID, customer VAT
number) are separate. They are stored as fields, never as primary keys.

## Decision

All entity identifiers in ABERP are **ULIDs** (Universally Unique
Lexicographically Sortable Identifiers): 128 bits, 26 characters in Crockford
base32, time-prefixed. Reference: https://github.com/ulid/spec.

At every API and storage boundary, IDs are presented as **prefixed strings**
of the form:

```
<entity_prefix>_<ulid>
```

For example:

```
inv_01J9QFK0AYC5G3RNX2K8YVZ3M0     ← invoice
prd_01J9QFK7T6Q2BS0FX1P3W4G7DJ     ← product
loc_01J9QFKA9NX7VYHCDR0Z2T6E4M     ← stocking location
ord_01J9QFKDC2KEBVPJ8YH9XR4M2A     ← order
mvt_01J9QFKG58W1NCT7AKZQRPYJ40     ← inventory movement
shp_01J9QFKK7TDFRP9XW3JG0BNCY1     ← shipment
lbl_01J9QFKND3MABYJ6X8RW2Q7C0Z     ← printed label / vignette
cad_01J9QFKR0YZFGTHWP5M9N6X1D8     ← CAD/CAM artifact
```

A canonical prefix registry lives at `docs/id-prefixes.md` (created with this
ADR). Adding a prefix requires a PR that updates the registry.

**The storage key is the bare ULID**, not the prefixed string. Prefixes are
applied at serialization boundaries (event payloads, API responses, UI). This
keeps storage tight and lets us correct a prefix without rewriting rows.

### Sequence numbers (human-facing, regulated)

ULIDs are the internal identity. Human-facing **sequence numbers** required by
law (invoice numbers per Hungarian rule) are a separate concept:

- Stored as a separate field on the entity, not as the primary key.
- Allocated by a per-tenant, per-series, per-fiscal-year sequence allocator
  that guarantees contiguity (no gaps, no reuse). Detail in ADR-0009.
- The allocator is transactional with the entity creation; a sequence number
  is only "burned" if the entity exists.

### External identifiers

NAV invoice number, Billingo ID, customer VAT number, EAN/GTIN, manufacturer
part number — all stored as **typed fields** with their own validation. None
is ever a primary key. None is mutable once written, except by an explicit
amendment workflow that is audit-logged.

### Typed IDs in Rust

Every entity gets a newtype:

```rust
pub struct InvoiceId(Ulid);
pub struct ProductId(Ulid);
```

Crossing types requires explicit conversion. No `Uuid`-as-everywhere, no
`String`-as-id. This catches "passed an invoice ID where a product ID was
expected" at compile time.

## Consequences

- One ID scheme, everywhere, forever (modulo a superseding ADR).
- Logs become readable: `inv_01J9QF…` tells you what it is without context.
- Cursor pagination is `WHERE id > ?` — natural, fast, stable under inserts.
- We accept the 26-char length cost over a 22-char UUID-base64. Readability
  and time-sort beat brevity at this scale.
- ULID generation needs a monotonic counter to break ties within the same
  millisecond. The `ulid` crate handles this; we wrap it in an injectable
  `IdProvider` port for testability.

## Adversarial review

- *"ULIDs leak creation time."* — Yes, by design — that is the time-sortable
  property. We accept that an attacker who sees an invoice ID learns the
  invoice was created at time T. They do **not** learn how many invoices were
  created (the random part is uncorrelated). Where time leakage is itself a
  concern (rare in ERP), we will issue an opaque alias.
- *"Why not UUIDv7?"* — Same time-sortable property, similar properties.
  ULIDs win on log readability (Crockford base32 vs hex). The decision is
  reversible if the ecosystem coalesces on UUIDv7.
- *"Prefixes can be spoofed."* — Prefixes are metadata, not security. Type
  enforcement happens in code (newtypes) and at the storage layer (foreign
  keys). Prefixes exist for humans reading logs.
- *"What stops two processes from generating colliding ULIDs?"* — The random
  bits make collision negligible (2^80 within the same millisecond per process).
  The monotonic counter inside a single process eliminates same-ms collisions
  there. We do not rely on prefixes for uniqueness.
- *"Sequence numbers and ULIDs together = two sources of truth for identity."*
  — No. ULID is identity. Sequence number is a *legal label*. The audit ledger
  records the binding.

## Alternatives considered

- **Auto-increment integers** — rejected: volume leak, requires coordination,
  collide across tenants and across deployments, hostile to event sourcing.
- **UUIDv4** — rejected: not time-sortable, index fragmentation.
- **UUIDv7** — close second; chosen ULID for log readability and an existing
  Rust crate we trust. Reversible.
- **Snowflake-style 64-bit IDs** — requires a coordinator or careful sharding.
  Refused: complexity not justified.

## Open questions

- Whether to expose the prefixed form to external integrators (NAV will see
  what NAV demands; Billingo their own; our API consumers see prefixed form).
- Whether the random part of a ULID is sufficient when an attacker has high
  privilege to enumerate (e.g., an exported audit log). Defer to ADR-0007
  threat model.
