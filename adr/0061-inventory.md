# ADR-0061 — Inventory module v1: append-only `stock_movements` ledger, denormalized `stock_qty` cache on `products`, virtual low-stock view

- **Status:** Accepted
- **Date:** 2026-06-03
- **Deciders:** Ervin (via S230 Stage 3 Phase γ brief)
- **Supersedes:** [ADR-0011 — Inventory model](0011-inventory-model.md) (stub)
- **Related:** ADR-0008 (audit-ledger — but inventory is its OWN ledger, see §3 below), ADR-0019 (relational SoT — engine-agnostic), ADR-0060 (Stage 3 manufacturing-adapter framework — sibling ADR; this one consumes its `CanonicalEvent` vocab), the memory pins [[no-sql-specific]], [[trust-code-not-operator]], [[hulye-biztos]], [[aberp-stage3-manufacturing]].

## Context

ADR-0011 was filed in 2026-05-19 as a stub naming "stock representation:
balance-at-rest vs event-sourced movements" as an open question. This ADR
**closes that question** — event-sourced, with a denormalized
balance-at-rest cache on `products` for read-cheap rendering. It also
closes ADR-0011's open question on whether BOM is part of inventory: no
— BOM lives in ADR-0062 (Work Orders) because the consumer is
production, not stock.

The Stage 3 brief sequences four ADRs (this one → Work Orders → QA →
Dispatch) such that **the workflow software is built before any
hardware lands**. Inventory is the foundation everything else stands
on: Work Orders consume from stock on Release and produce into stock on
Complete; Dispatch ships finished goods. Without an inventory module
the rest of Stage 3 has nowhere to read from / write to.

The constraints carrying over from ADR-0060 + the project's wider
posture:

- **No DB-engine specifics** — per [[no-sql-specific]] and ADR-0019, all
  invariants live in application code. No `CHECK` constraints on
  derived quantities; no triggers; no stored procedures. Schema
  migrations are forward-only additive SQL.
- **Single-process assumption** — Phase α deferred offline-first per
  cell to a future ADR. Inventory in v1 lives in the central process;
  the offline-first ADR will eventually split adapter / ledger writer
  but does not split inventory state.
- **Audit-everything posture** — every state mutation generates a
  ledger entry. The `stock_movements` table is itself the
  inventory-side ledger; the system audit-ledger records the higher
  level "a movement was recorded by actor X for reason Y" envelope.
- **Hülye-biztos** — operators must not be able to corrupt stock by
  fat-fingering `stock_qty` on a product form. The form does not
  expose stock_qty for direct edit; the only mutation surface is "post
  a stock movement" with a reason.

## Decision

### 1. Two tables, one rule: ledger first, cache second

**The ledger** (`stock_movements`) is the source of truth. Append-only.
Every row is one movement; no UPDATEs, no DELETEs.

```text
stock_movements (
  movement_id        ULID PRIMARY KEY,        -- prefix `mvt_`
  tenant_id          ULID NOT NULL,            -- per ADR-0002
  product_id         ULID NOT NULL,            -- prefix `prd_`
  qty_delta          DECIMAL(18,6) NOT NULL,   -- signed; +production / −consumption / ±adjust
  reason             VARCHAR NOT NULL,         -- closed-vocab MovementReason
  ref_kind           VARCHAR,                  -- closed-vocab MovementRefKind, NULL allowed for manual_adjust
  ref_id             VARCHAR,                  -- ULID of the referenced entity, NULL allowed for manual_adjust
  at                 TIMESTAMP NOT NULL,       -- operator-visible time of movement
  operator           VARCHAR NOT NULL,         -- attribution string
  idempotency_key    VARCHAR NOT NULL,         -- F8 idempotency
  notes              VARCHAR                   -- free-text, optional
);
```

**The cache** (extension of existing `products`) is a denormalized
rollup, recomputed after every movement write. Operators read
`stock_qty` directly when listing products; nobody writes it directly.

```text
products  -- ALTER TABLE ADD COLUMN, additive only:
  stock_qty           DECIMAL(18,6) NOT NULL DEFAULT 0,
  min_stock           DECIMAL(18,6) NOT NULL DEFAULT 0,
  bin_location        VARCHAR,                  -- free-text v1; multi-cell deferred
  last_movement_at    TIMESTAMP                 -- denormalized from latest stock_movements row
```

### 2. The closed-vocab enums

`MovementReason` (why the movement happened, not where the entity came from):

| Variant | Storage string | Used by |
|---|---|---|
| `Receipt` | `receipt` | Manual GRN / future PO-receive |
| `BomConsumption` | `bom_consumption` | ADR-0062 Work Order Release |
| `WoCompletion` | `wo_completion` | ADR-0062 Work Order Complete |
| `Adjustment` | `adjustment` | Manual stock-take correction (loud + reason required) |
| `Dispatch` | `dispatch` | ADR-0064 Dispatch shipping (negative qty) |
| `Scrap` | `scrap` | ADR-0063 QA Fail-Dispose |

`MovementRefKind` (the entity that caused the movement; allows the
operator to trace back from stock to root cause):

| Variant | Storage string | When ref_id is non-NULL |
|---|---|---|
| `WorkOrder` | `work_order` | ADR-0062 emits with `wo_<ulid>` |
| `QaInspection` | `qa_inspection` | ADR-0063 emits on Dispose with `qa_<ulid>` |
| `Dispatch` | `dispatch` | ADR-0064 emits with `dsp_<ulid>` |
| `Invoice` | `invoice` | Reserved for future inbound-stock-from-AP-invoice |
| `Manual` | `manual` | NULL `ref_id`, operator supplies notes |

Both enums are Rust `#[derive(Serialize, Deserialize)]` with
`#[serde(rename_all = "snake_case")]`. The storage strings are
round-trip pinned (same posture as
[[nav-gotchas]] §"closed-vocab" pins).

### 3. Application-layer invariant: `stock_qty = SUM(qty_delta)`

The rule that holds the design together:

```text
After EVERY write to stock_movements:
  SET products.stock_qty       = SUM(qty_delta) WHERE product_id = $1
  SET products.last_movement_at = MAX(at)        WHERE product_id = $1
```

The write happens **in the same DB transaction** as the movement
INSERT. No CHECK constraint enforces it — the invariant lives in the
`inventory::record_movement` function, which is the **only** write
surface for either column. Per [[no-sql-specific]]: invariants belong
in Rust, not in the storage engine.

**Why a cache at all if the ledger is the truth?** Two reasons:

1. **Product-list render cost.** Operators view the products list with
   100s of rows; computing `SUM(qty_delta)` per row per render is
   wasteful. Once the cache is in place, the list query is a flat
   SELECT.
2. **Low-stock detection.** A virtual view `WHERE stock_qty <
   min_stock` resolves in microseconds against the cache; against the
   ledger it would scan the whole table.

**Why isn't this two sources of truth?** Because the cache is
**derived**, not **independently maintained**. A `tools/rebuild-stock-cache`
utility recomputes the cache from the ledger; it's a forward-only
rebuild, idempotent. If a future operator panic surfaces "stock_qty
disagrees with SUM(qty_delta)", the recovery is `cargo run -- rebuild-stock-cache` and the cache catches up. The ledger never needs
correcting.

### 4. Audit-ledger integration

One new `EventKind` variant: `StockMovementRecorded` (storage string
`mes.stock_movement_recorded`).

**Why the `mes.` prefix and not `system.`** — per ADR-0060 §4, Stage 3
events live under `mes.*` so the per-OUTGOING-invoice export bundle's
`system.*` consumers do not get accidentally swept by Stage 3 traffic.
Stock movements are Stage 3 entities (driven by work orders / dispatch
/ QA, all `mes.*`); the prefix family is consistent.

**Why a new EventKind separate from ADR-0060's `mes.adapter_event`** —
because `mes.adapter_event` is **raw telemetry** subject to broadcast
lossiness per ADR-0060 §"Consequences". A stock movement is a
**regulated quantity** — losing one means the cache drifts and
inventory is wrong. The two surfaces must not share an audit kind
even if both emit canonical-event-shaped payloads.

The payload:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StockMovementRecordedPayload {
    pub movement_id: String,        // `mvt_<ulid>`
    pub product_id: String,         // `prd_<ulid>`
    pub qty_delta: Decimal,
    pub reason: MovementReason,
    pub ref_kind: Option<MovementRefKind>,
    pub ref_id: Option<String>,
    pub operator: String,
    pub idempotency_key: String,
}
```

F12 four-edit ritual fires once for this PR: new variant + `as_str`
arm + `from_storage_str` arm + variants-array entry. The `mes.` prefix
pin from ADR-0060 §4 is reused.

### 5. The closed `MovementReason` × sign convention

A defensive invariant the route handler enforces before writing:

| Reason | Required sign | If wrong-sign at boundary |
|---|---|---|
| `Receipt` | positive | refuse + 400 |
| `BomConsumption` | negative | refuse + 400 |
| `WoCompletion` | positive | refuse + 400 |
| `Dispatch` | negative | refuse + 400 |
| `Scrap` | negative | refuse + 400 |
| `Adjustment` | any | accept (the operator typed the sign) |

The check happens at the route layer per [[trust-code-not-operator]]:
loud-fail at the boundary, never silently flip. The product never
goes negative unless `Adjustment` writes it negative — the
**negative-stock policy** ADR-0011 named is implemented as "refuse
when post-write `stock_qty < 0` AND reason ≠ `Adjustment`". Adjustment
is the only path that can drive stock negative, and only with explicit
operator intent.

### 6. SPA surface

- **Products list** gains a **low-stock chip** on rows where
  `stock_qty < min_stock`. Chip is a categorical signal per ADR-0017;
  no numeric badge.
- **Product detail** gains a **Stock movements** tab next to the
  existing detail surface. The tab shows the descending-by-date list
  of `stock_movements` for that product with columns: at, qty_delta,
  reason, ref (linked when ref_kind is non-NULL), operator, notes.
- **New "Post movement" affordance** on the product detail page. Form
  fields: qty_delta (signed), reason (closed-vocab dropdown), notes
  (optional), idempotency_key (auto-generated by the SPA).
  `ref_kind` / `ref_id` are NOT operator-supplied — they are set
  only by upstream callers (Work Order release, Dispatch, QA dispose).
  Operator-supplied movements always have `ref_kind = Manual`,
  `ref_id = NULL`.
- **No bulk import** in v1. Stock-take is a series of `Adjustment`
  movements typed by the operator.

### 7. Cross-cutting decisions (Stage 3 Phase γ shared)

These five decisions repeat across ADR-0061 / 0062 / 0063 / 0064 so
that future-Dispatch (and future-Ervin) know what they explicitly
chose to keep uniform across the workflow rails:

1. **Mock-friendly adapter consumers.** Every state mutation in this
   ADR has exactly one handler function. The SPA route calls it
   today; the future adapter consumer (e.g. a barcode scan that
   triggers a receipt) calls the same function tomorrow. No
   double-implementation. The handler signature accepts an `actor:
   ActorKind` enum (`SpaOperator(user) | Adapter(name) | System`) so
   the audit-ledger entry records WHO drove the mutation without the
   handler caring about the source.

2. **No DB-engine specifics** per [[no-sql-specific]]. CHECK
   constraints exist only on closed-vocab columns that already exist
   in the codebase pattern (currency, local_status from earlier ADRs).
   Derived quantities (`stock_qty`) carry NO CHECK — the application
   layer is the invariant author.

3. **Audit-ledger F12 ritual.** Each new entity in Phase γ adds
   `EventKind` variants for its state mutations. ADR-0061 adds one
   (`StockMovementRecorded`); ADR-0062 will add three (work order
   state transitions); ADR-0063 will add two; ADR-0064 will add two.
   Every mutation emits.

4. **Tenant boundary.** All new tables get `tenant_id ULID NOT NULL`
   per ADR-0002. No cross-tenant queries; the per-tenant DuckDB path
   is the boundary at file level too.

5. **Out-of-scope-by-design list** at the bottom of every ADR.
   See §"Out of scope" below for this one.

## Consequences

- **Ledger replaces stocktake records.** A `tools/stocktake-recalc`
  script can read the ledger as-of any timestamp and produce a
  historical snapshot. No separate "month-end inventory snapshot"
  table needed.
- **Recovery is mechanical.** If the cache drifts (operator
  edit-by-mistake, future schema migration bug), `rebuild-stock-cache`
  reproduces it from the ledger. The ledger is the load-bearing
  artifact.
- **Work Order release / completion is one INSERT-into-movements per
  BOM-line per WO + one cache rebuild per touched product.** ADR-0062
  exercises this; the upstream caller writes movements
  in-the-same-transaction-as the WO state change.
- **Stage 3 hardware integration is mechanical when it lands.** A
  future barcode scanner that emits `ScanReceived{ qr_payload: "prd_xyz" }` resolves the product, optionally cross-references a
  pending work order, and calls `inventory::record_movement` with
  `actor: Adapter(name)`. No new write surface; same handler.
- **Inventory itself does NOT need an adapter trait.** ADR-0060's
  adapter trait is for hardware sources. Inventory is application
  state; it gets WRITTEN-TO by adapter event handlers but doesn't
  expose an `Adapter`-typed interface itself. Same model as the
  existing invoice / AP / restored-invoice modules.
- **DECIMAL(18,6) for qty_delta** matches the [[decimal-quantity-s157]]
  precedent (invoice line quantity also Decimal(18,6)). Engine-agnostic
  per [[no-sql-specific]]; round-half-even when conversions surface.
- **Audit traffic grows.** Every BOM consumption is N audit entries
  (one per component line). A 5-component WO release is 5
  `StockMovementRecorded` entries plus 1 `WorkOrderStateChanged` (ADR-0062).
  For 10 work orders/day across 10-component BOMs that's ~110
  audit-ledger entries/day from inventory alone. Well inside the
  current ledger budget; flagged as a sizing data point for the
  future operations dashboard.

## Adversarial review

- *"Why isn't `stock_qty` just a view over `SUM(qty_delta)` — that
  removes the two-sources-of-truth concern entirely."* A view defers
  the SUM cost to read time. For a product-list render with 200
  products that's 200 SUMs over a ledger that grows monotonically. A
  cache column write-time + a flat SELECT at read-time is the right
  trade for the operator-visible product list. The recovery path
  (`rebuild-stock-cache`) collapses the worst-case "cache drifted"
  scenario to one CLI invocation. The ledger remains the truth; the
  cache is denorm.
- *"What if two concurrent writes hit `record_movement` for the same
  product? The cache could land at the SUM of one but not both."* The
  invariant is "write happens in the same transaction as the INSERT."
  DuckDB serializes; SQLite (the engine-agnostic fallback per
  ADR-0019) serializes; Postgres would need `SELECT ... FOR UPDATE`
  on the product row inside the tx. The cross-engine rule: rewrite
  `stock_qty` from `SELECT SUM(qty_delta) FROM stock_movements WHERE
  product_id = $1` rather than from `current_stock_qty + qty_delta`.
  Reading the SUM at write time costs an O(movements-for-product)
  scan but is concurrency-safe regardless of engine isolation level.
  Sizing: a 5-year horizon × 10 movements/product/year × 500
  products = 25K rows per worst-case product. SUM over 25K rows is
  microseconds. Acceptable.
- *"Negative-stock-on-Adjustment is a footgun — operators will use
  Adjustment to 'fix' problems they don't understand and silently
  drive stock negative."* The signal is the categorical chip on the
  product list. A product with `stock_qty < 0` gets a distinct
  "Stock negative" chip alongside the low-stock chip — operators
  see it on the list immediately. The escape valve (Adjustment) is
  the right shape; surfacing the consequence loudly is the mitigation.
- *"No reservation model — but ADR-0011 named reservations as a
  decision."* Deferred per the "delete the part" rule. Reservations
  are a forecasting feature; v1 has no concurrent-consumer pattern
  that needs them (one operator, one shop floor). When a sales-order
  module ships and an order can sit "allocated but not yet released
  to a WO," THAT module will surface the need. Flagged in §"Open
  questions."
- *"Single `bin_location: String` will fail when Áben's shop floor has
  3–4 CNCs each with their own raw-stock cribs."* Yes — v1 is
  hülye-biztos single-warehouse. The string holds operator-typed
  labels ("rack-A-3", "CNC1-tool-locker"); multi-cell semantics that
  treat bin_location as a structured location is a v2 question. The
  ledger doesn't carry a per-movement `from_bin` / `to_bin` — a future
  multi-cell ADR will need to extend `stock_movements` with those
  columns and re-derive per-bin balances. Filed as Open Question.
- *"Why no `unit_of_measure` on stock_movements? Inventory of liquid
  coolant in liters vs solid parts in pieces vs tooling in 'each' is
  the canonical inventory-v1 footgun."* Carried on the **product**,
  not the movement — same pattern as [[unit-of-measure-emit-s159]] on
  the invoice line side (line carries unit; line owner is the product).
  A movement of `qty_delta = 5.5` inherits the unit of its product.
  If a future v2 wants per-movement units (e.g. "received in pallets,
  consumed in pieces") the conversion table lives on the product, not
  the movement. Aligned with the [[no-sql-specific]] posture: keep the
  ledger row narrow, surface conversions in app code.

## Alternatives considered

- **Balance-at-rest only (no ledger), with audit entries reflecting
  the delta.** Refused — the audit ledger is a system surface; an
  inventory module that derives current stock from system audit
  ledger queries inverts the layering. The system audit ledger
  records "an inventory module recorded a movement" — it doesn't BE
  the inventory ledger. Symmetric to the way ADR-0008 doesn't BE
  the invoice state; the invoice table is the state.
- **Event-sourced with no cache** (read-time SUM). Refused — product
  list render cost. See §3.
- **Postgres-only with materialized view.** Refused per [[no-sql-specific]]
  + ADR-0019 — invariants in app, engines are interchangeable.
- **Use ADR-0060's `mes.adapter_event` for stock movements too.**
  Refused — broadcast lossiness on the adapter ledger-writer path
  (ADR-0060 §"Consequences" + §"Adversarial review #4") can drop
  events under load. Stock movements MUST NOT be lossy. The
  application calls audit-ledger's append API directly (the same
  surface every other Stage 1 audit producer uses) — no broadcast in
  the path.
- **Defer BOM-tracked consumption to ADR-0062 alone, skip
  `MovementReason::BomConsumption` here.** Refused — the reason
  enum is the closed vocab; pinning it complete now (with reasons
  that are emitted by FUTURE callers in ADR-0062 / 0063 / 0064)
  avoids one enum-extension churn per consumer. The four downstream
  reasons (`BomConsumption`, `WoCompletion`, `Dispatch`, `Scrap`) are
  declared here even though no caller emits them yet.
- **Tabular UI for "post movement"** instead of a single-row form.
  Refused — bulk-import is named-deferred. The single-row form is
  enough for stocktake (operator types row by row) at the Áben scale.
- **Negative-stock CHECK constraint** at the DB level (refuse INSERT
  if post-write `stock_qty < 0`). Refused per [[no-sql-specific]] —
  the check belongs in the `record_movement` route handler with the
  reason-and-sign matrix, not in the schema.

## Open questions

These do not block PR-226 filing. Each is named with a trigger.

1. **Multi-cell `bin_location` semantics.** Trigger: first cell
   controller spec lands AND the brief surfaces "per-cell stock
   visibility" as an operator requirement. Likely an additive
   `stock_movements.from_bin` + `to_bin` + a per-(product, bin)
   cache view, possibly via a separate `stock_by_bin` table.
2. **Reservation model.** Trigger: first sales-order or
   work-order-queue feature that needs to mark stock "allocated, not
   yet consumed." Today a WO release synchronously emits the
   `BomConsumption` movement; reservations would split that into
   `Reserve` (positive on a virtual "reserved" balance) + `Consume`
   (negative on actual stock).
3. **Lot / serial tracking.** Trigger: first regulatory or customer
   requirement that says "we need to trace which lot of raw stock
   went into this finished good." Likely a `lot_id` column on
   `stock_movements` plus a `lots` table. The ledger shape extends
   additively.
4. **Unit-of-measure conversions across movement types.** Trigger:
   first product purchased in one unit (pallets) and consumed in
   another (pieces). Conversion table on the product; movement rows
   stay single-unit per the product's base unit.
5. **Stocktake reconciliation report.** Trigger: first operator
   request for "show me the variance between physical count and
   ledger." Will likely be a one-shot CLI tool that produces a CSV
   from the ledger as-of a timestamp + an operator-supplied
   physical-count CSV, surfacing the deltas.
6. **Bulk import / receipt-from-invoice.** Trigger: AP invoice rows
   that name physical goods (vs services) and tenant operator wants
   inbound stock auto-recorded. Today the AP module records the
   invoice; a future bridge could emit `Receipt` movements
   referenced by `MovementRefKind::Invoice`. Reserved in the
   `MovementRefKind` enum.
7. **Min-stock per bin** vs the current per-product. Trigger: same
   as Open Question 1.
8. **Operator-tunable low-stock chip threshold** (e.g. amber when
   approaching min, red when below). Trigger: operator survey says
   the binary chip isn't loud enough.

## Out of scope (deliberately)

Future-Ervin should expect to file separate ADRs for these. They are
NOT bugs in v1; they are explicit choices to ship a working
inventory module before scope-creeping into a full WMS.

- **Multi-warehouse / multi-cell location modeling.** v1 ships a
  single free-text `bin_location` string; structured locations are
  v2.
- **FIFO / LIFO costing.** v1 has no cost layer at all. Per-line cost
  is a function on the WO/Dispatch side; stock has no cost basis in
  v1.
- **Lot / serial tracking.** ADR-0011 named these; ADR-0061 defers
  them.
- **Reservation / allocation model.** WO release is synchronous
  consume; no allocate-then-consume split.
- **Cycle counting / stocktake workflow.** Operator types Adjustment
  movements; no guided cycle-count tooling.
- **Demand planning / forecasting.** Not v1.
- **Purchasing / supplier management.** Separate future ADR.
- **DB-level CHECK / triggers** on derived quantities. Per [[no-sql-specific]].
- **Negative-stock prevention at the DB layer.** App-layer reason-sign
  matrix is the gate.

## Invariants pinned (load-bearing for ADR-0062 onward)

1. **`stock_movements` is append-only.** No UPDATE / DELETE through
   the application code. Pinned by a future
   `record_movement_writes_one_row_no_update` test.
2. **`stock_qty` is rebuilt from `SUM(qty_delta)` after every write,
   in the same transaction.** Pinned by
   `record_movement_updates_cache_inside_tx`.
3. **`MovementReason::BomConsumption | Scrap | Dispatch` are
   negative; `Receipt | WoCompletion` are positive; `Adjustment` is
   any.** Pinned by `record_movement_refuses_wrong_sign_per_reason`.
4. **`record_movement` is the only write surface for `stock_qty`
   and `last_movement_at`.** Pinned by code review + a
   `products_stock_qty_only_written_via_record_movement` audit (a
   grep-based test that asserts no other `UPDATE products SET
   stock_qty` lives in the tree).
5. **One audit-ledger `StockMovementRecorded` per movement.** Pinned
   by `record_movement_emits_one_audit_entry`.
6. **Operator-supplied movements (`actor = SpaOperator`) always set
   `ref_kind = Manual` and `ref_id = NULL`.** The SPA form never
   exposes ref_kind / ref_id to the operator. Pinned by
   `spa_movement_form_has_no_ref_kind_field`.
