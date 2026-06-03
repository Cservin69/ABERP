# ADR-0062 — Work Orders (`work_orders`) + 1-level BOM (`boms`) + linear Routing (`routings`): operator-button + adapter-trigger uniform handlers

- **Status:** Accepted
- **Date:** 2026-06-03
- **Deciders:** Ervin (via S230 Stage 3 Phase γ brief)
- **Supersedes:** none — but consumes the `CanonicalEvent::WorkOrderStateChanged` vocab declared in ADR-0060 §1 and closes the "work-order / production model" open question that ADR-0011 named-deferred.
- **Related:** ADR-0060 (Stage 3 Phase α framework — this ADR is one of Phase γ's four workflow rails), ADR-0061 (Inventory — release/complete write `stock_movements`), ADR-0063 (QA — auto-creates inspections on routing-op complete), ADR-0064 (Dispatch — consumes WOs whose every op is Passed), the memory pins [[no-sql-specific]], [[trust-code-not-operator]], [[aberp-stage3-manufacturing]].

## Context

ADR-0060 declared `CanonicalEvent::WorkOrderStateChanged` with a
closed lifecycle vocab (`Created → Released → InProgress → Completed
| Cancelled | OnHold`) but did NOT name the entity those events
mutate. This ADR creates that entity (`work_orders`), pairs it with
the artifacts a Stage 3 shop floor needs (1-level BOM, linear
routing), and pins the handler shape that ADR-0061 / 0063 / 0064 will
plug into.

The brief's sequencing logic is load-bearing: the workflow software
runs WITHOUT hardware adapters because every state mutation has a
manual SPA button as its first-class trigger. Adapters (the
S229-onward barcode / Renishaw / OPC-UA strands) eventually arrive
and call the **same** handler with `actor: Adapter(name)` instead of
`actor: SpaOperator(user)`. No new handlers, no parallel code paths.

The constraints carrying over from the wider posture:

- **No DB-engine specifics** — [[no-sql-specific]] + ADR-0019.
- **Audit-ledger F12 ritual** fires once per new state-mutation
  EventKind (three in this ADR: see §4).
- **Tenant boundary** per ADR-0002 on all new tables.
- **Single-process assumption** carries from ADR-0060 §"Offline-first per cell — deferred."
- **Operator pushback against the brief's framing.** The brief named
  "BOM nesting OUT OF SCOPE" — accepted; Áben's first WOs are
  flat-BOM (raw bar → CNC → finished part). Nested sub-assemblies
  will arrive when a real customer order needs them, and the BOM
  schema extends additively (a `parent_bom_id NULLABLE` column would
  do it).

## Decision

### 1. Three tables, one entity-of-meaning (`work_orders`), two supporting reference tables (`boms`, `routings`)

`work_orders` is the regulated entity. State transitions emit audit
events. Operators issue, release, start, complete, hold, cancel
through SPA buttons; future adapters trigger the same transitions.

`boms` is **a property of a product**, not of a WO. A product may
have one BOM (its component list); when a WO produces that product,
the WO snapshots the BOM rows at Release time and emits one
`StockMovementRecorded` per BOM line.

`routings` is **a property of a WO**, not of a product. Each WO gets
its own routing copy at Create time (or templated from a
product-default-routing in v2). Operations within a routing are
linear (no branching) and have est_time / est_cost as metadata.

```text
work_orders (
  wo_id              ULID PRIMARY KEY,   -- prefix `wo_`
  tenant_id          ULID NOT NULL,
  product_id         ULID NOT NULL,      -- the finished good being produced
  qty_target         DECIMAL(18,6) NOT NULL,  -- how many finished units
  state              VARCHAR NOT NULL,   -- closed-vocab WorkOrderState
  created_at         TIMESTAMP NOT NULL,
  released_at        TIMESTAMP,
  started_at         TIMESTAMP,
  completed_at       TIMESTAMP,
  cancelled_at       TIMESTAMP,
  hold_reason        VARCHAR,            -- only set while state = OnHold
  notes              VARCHAR
);

boms (
  bom_line_id        ULID PRIMARY KEY,   -- prefix `bml_`
  tenant_id          ULID NOT NULL,
  product_id         ULID NOT NULL,      -- parent: the finished good
  component_id       ULID NOT NULL,      -- child: a stock-tracked product
  qty_per_unit       DECIMAL(18,6) NOT NULL,
  created_at         TIMESTAMP NOT NULL,
  retired_at         TIMESTAMP          -- soft-retire (see §6)
);

routings (
  routing_op_id      ULID PRIMARY KEY,   -- prefix `rop_`
  tenant_id          ULID NOT NULL,
  wo_id              ULID NOT NULL,      -- routing belongs to one WO
  sequence           INTEGER NOT NULL,   -- 1, 2, 3 ... linear order
  op_name            VARCHAR NOT NULL,   -- operator-typed, free-text v1
  est_time_min       INTEGER,            -- optional, operator-typed
  est_cost_huf       DECIMAL(18,2),      -- optional, operator-typed
  state              VARCHAR NOT NULL,   -- closed-vocab RoutingOpState
  started_at         TIMESTAMP,
  completed_at       TIMESTAMP
);
```

### 2. The closed-vocab state vocabularies

`WorkOrderState` — pinned in ADR-0060 §1, re-declared here as the
canonical vocab:

```text
Created → Released → InProgress → Completed
                            ↘ Cancelled
                            ↘ OnHold  → InProgress  (resume)
                                      → Cancelled
```

| Variant | Storage string | Allowed transitions FROM |
|---|---|---|
| `Created` | `created` | (initial) |
| `Released` | `released` | Created |
| `InProgress` | `in_progress` | Released, OnHold |
| `Completed` | `completed` | InProgress |
| `Cancelled` | `cancelled` | Created, Released, InProgress, OnHold |
| `OnHold` | `on_hold` | Released, InProgress |

The transition table is the application-layer invariant. No DB
CHECK enforces it; the handler refuses + 400s on illegal transitions
per [[trust-code-not-operator]].

`RoutingOpState` — narrower, per-operation:

| Variant | Storage string | Allowed transitions FROM |
|---|---|---|
| `Pending` | `pending` | (initial — set when WO is created) |
| `Active` | `active` | Pending (when prior op Completed AND WO is InProgress) |
| `Completed` | `completed` | Active |
| `Skipped` | `skipped` | Pending (operator override when an op proves unneeded) |

A routing op is `Active` only when (a) its sequence is the lowest
pending sequence AND (b) the parent WO is `InProgress`. There is no
"start" / "stop" per op in v1 — completion of one op auto-activates
the next.

### 3. The handler shape (mock-friendly per Phase γ Cross-cutting #1)

Every state mutation has exactly one Rust function. The signature:

```rust
pub fn transition_work_order(
    tx: &Transaction,
    wo_id: WoId,
    to_state: WorkOrderState,
    actor: ActorKind,
    reason: Option<String>,
    idempotency_key: IdempotencyKey,
) -> Result<WorkOrderTransitionOutcome, TransitionError>;
```

The SPA route `POST /api/work-orders/:id/release` resolves
`actor = SpaOperator(user)` and calls it. A future
`CanonicalEvent::WorkOrderStateChanged` arriving from a barcode-scan
adapter resolves to `actor = Adapter(name)` and calls the SAME
function with the SAME signature. The handler does not branch on
actor for the state-transition logic — actor is captured into the
audit entry only.

**Why this matters:** the brief explicitly named the duality as
"manual buttons remain authoritative." Once Phase ε (CNC adapter) and
Phase ζ (Renishaw) land, the operator's button-press and the
adapter's auto-trigger reach the same code. Refusing to ship two
parallel paths is the discipline that prevents Stage 3 from sprawling
into "the workflow has two ways to do everything, which one is the
source of truth?"

### 4. Audit-ledger integration — three new EventKinds

The brief's Phase γ Cross-cutting #3 budget pinned "each new entity
gets EventKind variants." Three land in this ADR:

| EventKind variant | Storage string | Payload carries |
|---|---|---|
| `WorkOrderCreated` | `mes.work_order_created` | `wo_id`, `product_id`, `qty_target`, `routing_op_ids`, `actor`, `idempotency_key` |
| `WorkOrderStateChanged` | `mes.work_order_state_changed` | `wo_id`, `from_state`, `to_state`, `reason`, `actor`, `source_event_id` (Option ULID), `idempotency_key` |
| `RoutingOpStateChanged` | `mes.routing_op_state_changed` | `routing_op_id`, `wo_id`, `from_state`, `to_state`, `actor`, `idempotency_key` |

**Why three not one** — the create-vs-transition split mirrors the
Stage 1 `InvoiceDraftCreated` vs `InvoiceState*` pattern: create
emits the full snapshot once, transitions are deltas. The routing-op
state changes are a separate kind so future operations-dashboard
projections can glob `mes.routing_op_*` without sweeping WO-level
events.

**`source_event_id` is load-bearing** — when an adapter event (e.g.
`mes.adapter_event` carrying `WorkOrderStateChanged`) triggers a real
transition, both audit entries get written: the raw adapter event
AND the entity-level state-change. The entity event's
`source_event_id` cross-references the adapter event's ULID, so an
operator looking at the timeline can trace "the state change at
12:34 was triggered by adapter X's scan at 12:33." For SPA-button
transitions, `source_event_id = None`.

F12 four-edit ritual fires three times for this PR (one per new
variant). Prefix pins from ADR-0060 §4 are reused; new
`s230_work_order_kinds_use_mes_prefix` test.

### 5. Inventory coupling (ADR-0061 consumer)

**On `Released`:** the handler snapshots the product's BOM and
emits one `stock_movements` row per component:

```text
for each (bml in boms where product_id = wo.product_id AND retired_at IS NULL):
    inventory::record_movement(
        product_id     = bml.component_id,
        qty_delta      = -(bml.qty_per_unit * wo.qty_target),
        reason         = MovementReason::BomConsumption,
        ref_kind       = MovementRefKind::WorkOrder,
        ref_id         = wo.wo_id,
        operator       = transition.actor.attribution_string(),
        idempotency_key = derive_from(wo.wo_id, "release", bml.bom_line_id),
    )
```

All N movements + the WO state-transition audit entry land in **the
same transaction** as the WO state update. Half-applied release is
impossible: either every component consumes AND the state flips AND
the audit lands, or nothing changes.

**On `Completed`:** one positive movement for the finished good:

```text
inventory::record_movement(
    product_id     = wo.product_id,
    qty_delta      = wo.qty_target,
    reason         = MovementReason::WoCompletion,
    ref_kind       = MovementRefKind::WorkOrder,
    ref_id         = wo.wo_id,
    operator       = transition.actor.attribution_string(),
    idempotency_key = derive_from(wo.wo_id, "complete"),
)
```

**On `Cancelled` from `InProgress`:** no automatic reverse-consumption.
The operator must explicitly post `Adjustment` movements if they want
to return raw stock to inventory. Reason: an in-progress WO's
components may already be partially worked (cut bar, scrap chips) —
auto-reverse would be wrong. The cancel audit entry's `notes` field
should record "manual stock recovery required" as a hint.

**On `Cancelled` from `Created` or `Released`:** no movements have
been emitted yet (Created) OR reversal is mechanical (Released-but-
not-started). The handler refuses to auto-reverse from Released too —
same reason: components may have been physically picked from the
crib even though "InProgress" hasn't been clicked. Operator
discipline carries the recovery.

### 6. BOM retirement, not deletion

`boms` rows are NEVER DELETEd. When a product's BOM changes, the old
rows are soft-retired (`retired_at` set) and new rows are inserted.
A WO Released against the product snapshots the active (retired_at
IS NULL) rows at Release time. Historical WOs can be re-traced
against the BOM as-of their `released_at` by querying for
`(retired_at IS NULL OR retired_at > released_at) AND created_at <= released_at`.

No audit EventKind for BOM mutations in v1 — BOM is reference data,
not regulated state. Adding one is an additive future enhancement
when an operator survey asks "who changed the BOM for product X on
date Y."

### 7. SPA surface

- **New "Work Orders" main-nav tab** alongside Invoices (Outgoing /
  Incoming / Quotes). The tab hosts a `WorkOrderList.svelte` with
  state-facet chips (`Created`, `Released`, `InProgress`, `OnHold`,
  `Completed`, `Cancelled`) and a sort-by-created_at default.
- **WorkOrderDetail** page with:
  - Header: product, qty_target, state, audit timeline.
  - **Action buttons** rendered conditionally on state per the
    transition table: Release (from Created), Start (from Released),
    Complete (from InProgress), Hold (from Released or InProgress),
    Resume (from OnHold), Cancel (always available unless already
    terminal).
  - Routing operations table: sequence, op_name, est_time, est_cost,
    state, started_at, completed_at, per-op Complete / Skip buttons.
  - BOM section: snapshot of consumed components (read-only after
    Release).
- **Create-WO form** with product picker, qty_target, and an
  operator-typed routing-op list (minimum 1 row). The routing rows
  inherit from a product-default-routing in v2; v1 the operator types
  them per WO.
- **No Gantt / shop-floor board** in v1. The list view + state-facet
  chips is the operational surface. Operations dashboard
  (ADR-0060 Open Questions §"Operations dashboard projection") is
  the future home for visual scheduling.

### 8. Cross-cutting decisions (Stage 3 Phase γ shared)

These five repeat across ADR-0061 / 0062 / 0063 / 0064:

1. **Mock-friendly Adapter consumers.** §3 handler. SPA-button OR
   future-adapter — same function, `actor` enum captured.
2. **No DB-engine specifics** per [[no-sql-specific]]. State
   transition table lives in Rust; no CHECK on `work_orders.state`.
3. **Audit-ledger F12 ritual.** Three new variants this ADR.
4. **Tenant boundary** on `work_orders`, `boms`, `routings`.
5. **Out-of-scope-by-design list** at §"Out of scope" below.

## Consequences

- **Inventory feeds work-order release transactionally.** §5 spells
  this out; the work-order route handler opens a tx, INSERTs N
  stock_movements + N audit entries + 1 WO state-change + 1 WO
  audit entry, commits.
- **Routing operations auto-cascade.** When the operator clicks
  "Complete" on the active op, the next pending op flips to Active
  in the same tx. When the last op flips to Completed, the WO state
  flips to Completed (with the operator's confirm — see Open
  Questions §"Auto-complete on last op").
- **Adapter integration is mechanical.** A future barcode-scanner
  adapter that emits `CanonicalEvent::ScanReceived { payload: "wo_<id>:complete" }` resolves the WO, calls `transition_work_order(...,
  to_state: Completed, actor: Adapter("scanner-A"), ...)`. The handler
  is unchanged.
- **Stage 1 invoicing remains untouched.** Work orders do NOT
  auto-spawn invoices; that's ADR-0064 (Dispatch). A WO that's
  Completed sits awaiting QA pass (ADR-0063); only Dispatch crosses
  into Stage 1.
- **BOM snapshot at Release** means changing a BOM after Release
  does NOT retroactively change the consumed components. The release
  is a frozen historical event. Operators reviewing yesterday's
  release see yesterday's BOM.
- **The cancel-mid-WIP recovery story is operator-driven, not
  automatic.** §5 names this. The right shape for v1; an auto-reverse
  would be a footgun.
- **Audit traffic grows.** A typical WO is Create + Release + Start +
  (N op completes) + Complete = ~5–10 entries plus the inventory
  side (N component movements + 1 finished-good movement). 5 WOs/day
  × ~15 entries each = ~75 audit entries/day from work orders alone.
  Sizing data point.

## Adversarial review

- *"Why isn't routing a property of the product (template), with the
  WO pointing at a specific revision? Re-typing the routing per WO is
  hülye-biztos-unfriendly."* The v1 framing is yes-it-is-painful but
  it's once-per-WO and Áben does 5 WOs/day. The product-default-routing
  feature is the obvious v2 path — when an operator survey says
  "I'm retyping the same 4 ops every day for SKU X," the template
  lands. Refusing to pre-design is the [[think-then-act]] posture.
- *"BOM nesting will bite in 6 months when the first sub-assembly
  appears."* Likely true. The extension path is well-shaped
  (`boms.parent_bom_id NULLABLE`). The v1 design refuses to predict
  when that day arrives. If the first nested BOM lands before
  ADR-0064 ships, this ADR gets a follow-on; otherwise the extension
  happens incrementally.
- *"What happens to an in-progress WO when the operator restarts
  ABERP / the device dies / power cycles?"* State persists in the
  DB; audit entries persist. The WO resumes in whatever state it was
  last in. There is no in-memory state to lose — same posture as
  every other Stage 1 module.
- *"Auto-cascading routing ops (next op flips to Active when prior
  Completes) takes the operator out of the loop. What if op 4 of 6
  needs a manager approval before op 5 begins?"* Out of scope v1 —
  no approval gates between ops. If a real workflow requires it,
  the operator's escape valve is "Hold" between ops, which freezes
  the cascade until "Resume" clicks. The next-op-auto-Active
  behavior is the default; manager-approval gates are a v2 feature.
- *"Concurrent transitions: two operators click 'Complete' on the
  same WO simultaneously. The second wins; the first sees a stale
  state."* The handler reads the current state at the start of the
  transaction; if it doesn't match the expected `from` state, it
  refuses with `TransitionError::StateRaced`. Operator sees a "the
  state changed under you, refresh" error — standard optimistic-
  concurrency posture. No locks needed.
- *"Cancelled-from-InProgress doesn't auto-reverse stock — operators
  WILL forget to post the Adjustment movement and inventory will
  drift."* Yes — and the mitigation is the negative-stock chip on
  the inventory side AND the WO timeline noting "manual stock
  recovery required" AND the future operations dashboard's
  cancelled-WO surface. Auto-reverse is worse: the system would
  silently reverse cuts the operator may have already shipped to
  scrap. Operator-driven recovery is the right shape for v1.
- *"BOM rows have no `notes` / no `loss_factor` / no `scrap_allowance`.
  CNC shops routinely cut more raw bar than the part needs to
  account for chip loss."* Yes — and the extension is one column.
  Out of scope v1; flagged as Open Question.
- *"`source_event_id: Option<ULID>` invites lying — a buggy handler
  could write a state-change entry without populating the source even
  though an adapter triggered it. The audit story breaks silently."*
  The handler signature MUST include `source_event_id` as a required
  parameter (not derived); the SPA route fills it with None
  explicitly. A future code review test asserts every callsite either
  passes Some(_) or explicit None. Cheap; mechanical; prevents drift.

## Alternatives considered

- **Single `work_orders.state` enum with no separate routing-op
  state.** Refused — Routing operations have their own lifecycle
  (Pending → Active → Completed); collapsing them into the WO state
  would require carrying op_index everywhere. Two enums, two tables,
  two audit kinds is the right cut.
- **BOM as a JSON column on `products`.** Refused — querying
  "which products use component X" becomes a JSON-extract scan; the
  side-store pattern (input.json on invoices) is fine for opaque
  blobs but BOM is queried both directions (parent → children AND
  child → parents).
- **Routing as a JSON column on `work_orders`.** Refused for the
  same reason; per-op state transitions need rows, not blob
  patches.
- **Auto-complete WO when last op Completes.** Tempting; refused.
  The Stage 3 vision has QA pass between "last op done" and "WO
  done." ADR-0063 creates the QA inspection on each op completion;
  the WO transitions to Completed only when QA passes all required
  ops. Auto-flip would skip that gate.
- **Use a single `EventKind::WorkOrderLifecycle` with a payload
  discriminator.** Refused — three kinds is the F12 budget the brief
  set, and the per-kind glob (`mes.work_order_*`) is what the future
  operations dashboard projection wants.
- **Routing ops have started_at AND ended_at — give them a Start
  button per op.** Refused for v1 — operators clicking 6 starts and
  6 completes per WO is friction; the simpler "next op
  auto-activates" cascade is enough. Per-op start can be added when
  per-op time tracking proves necessary.

## Open questions

1. **Product-default routing template.** Trigger: operator survey
   says "I'm retyping the same routing per WO." Likely a
   `product_routings` table with the same shape as `routings` minus
   `wo_id` plus a `default_for_product_id`. Create-WO copies them in.
2. **BOM nesting (sub-assemblies).** Trigger: first WO whose product
   has a child product that ALSO has a BOM. Extension: nullable
   `parent_bom_id` on `boms` + recursive snapshot at Release.
3. **Per-BOM-line loss factor / scrap allowance.** Trigger: first
   real-world WO where operator says "I had to consume 1.1× the
   theoretical per chip loss." Additive column on `boms`.
4. **Per-op operator time tracking** (start button per op + actual
   time captured). Trigger: first real cost-accounting requirement.
5. **Approval gates between ops.** Trigger: first regulatory or
   customer requirement.
6. **Auto-complete on last op pass.** Trigger: ADR-0063 ships AND
   operator survey says the manual "Complete WO" click after every
   op passes QA is friction.
7. **WO-level cost rollup.** Sum routing-op est_cost + component
   stock cost. Trigger: first WO-margin / job-costing requirement.
8. **Concurrent-WO scheduling / dispatch board.** Trigger: 2+
   in-progress WOs that compete for the same machine. Likely the
   start of an operations-dashboard ADR (per ADR-0060 Open
   Questions).
9. **Adapter event idempotency at the WO-handler level.** A barcode
   scanner that fires twice for the same scan must not transition
   the WO twice. The handler's `idempotency_key` parameter is the
   gate; the adapter must supply a stable key derived from the scan
   (e.g. `scan_<ts>_<station>_<payload>`).

## Out of scope (deliberately)

- **BOM nesting.** v1 is flat.
- **Routing branching / alternates.** v1 is linear.
- **Machine assignment per op.** Waits for adapter coverage.
- **WO-level cost computation.** v1 routing has est_cost as
  metadata; no rollup.
- **Time-and-attendance integration.** v1 has no per-op timer.
- **Customer/order linkage** — WOs are not yet tied to sales orders
  in v1. A future sales-order module will add `wo.order_id`. Today
  Áben's WOs are dispatched from operator-typed quotes (ADR-0057
  quote intake pipes to invoice draft; this is the production
  side, parallel).
- **Multi-tenant routing-template library.** v1 routings are
  per-WO, per-tenant.
- **DB-level CHECK constraints** on `work_orders.state` or
  `routings.state`. App-layer transition table is the gate.
- **Audit kind for BOM mutations.** Reference-data; deferred.

## Invariants pinned (load-bearing for ADR-0063 / 0064)

1. **Every state transition emits exactly one
   `WorkOrderStateChanged` (or `RoutingOpStateChanged`) audit
   entry.** Pinned by `transition_emits_one_audit_entry`.
2. **Illegal transitions refused at the handler layer with 400.**
   Pinned by `transition_refuses_illegal_state_pair` per the §2 table.
3. **On `Released`, every active BOM row of the product emits one
   `BomConsumption` `stock_movements` row.** Pinned by
   `release_emits_one_movement_per_active_bom_row`.
4. **On `Completed`, exactly one `WoCompletion` `stock_movements`
   row is emitted for the product.** Pinned by
   `complete_emits_one_finished_good_movement`.
5. **Routing-op `Active` auto-advances to next sequence on prior
   `Completed`, inside the same transaction.** Pinned by
   `complete_op_activates_next_op_in_same_tx`.
6. **`actor: SpaOperator | Adapter | System` is captured into every
   audit entry; the handler does NOT branch on actor for transition
   logic.** Pinned by code-review + `handler_does_not_match_on_actor_for_transition` (a structural test).
7. **`source_event_id` is a required parameter; SPA routes pass
   `None`, adapter event handlers pass `Some(_)`.** Pinned by
   `route_handlers_pass_explicit_none_for_source_event_id`.
8. **BOM rows are NEVER DELETEd; `retired_at` is the soft-retire
   surface.** Pinned by `boms_table_never_deleted_through_application_code`.
