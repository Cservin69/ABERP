# ADR-0064 — Dispatch board (`dispatches`) + invoice auto-spawn from finished WOs: closes the Stage 3 → Stage 1 loop

- **Status:** Accepted
- **Date:** 2026-06-03
- **Deciders:** Ervin (via S230 Stage 3 Phase γ brief)
- **Supersedes:** none in full — extends ADR-0015 (Order + logistics state machine — stub) by closing the dispatch-side of its scope. Sales/PO state machines remain deferred.
- **Related:** ADR-0060 (Stage 3 framework — adapter triggers eventually arrive), ADR-0061 (Inventory — Dispatch emits a `Dispatch` movement), ADR-0062 (Work Orders — eligibility upstream), ADR-0063 (QA — all-pass gate), ADR-0009 (NAV invoice issuing — the auto-spawned draft enters this pipeline), ADR-0057 (quote intake — same staging-vs-canonical-invoice posture this ADR mirrors), the memory pins [[no-sql-specific]], [[trust-code-not-operator]], [[hulye-biztos]], [[aberp-stage3-manufacturing]], [[seller-toml-write-invariant]].

## Context

The four-ADR Phase γ arc ends here. ADR-0061 gave us the inventory
ledger; ADR-0062 the work-order lifecycle; ADR-0063 the QA gate. A
Completed WO that has passed every routing-op QA is finished good
sitting in stock. **Dispatch is the act of shipping it to the
recipient and triggering the invoice.**

This is the architectural cross-over: Stage 3 (manufacturing) hands
off to Stage 1 (invoicing) at a single point. The Stage-1 invoice
draft pipeline (ADR-0009, ADR-0057 quote intake) is the receiving
side. The dispatch action is the single Stage-3→Stage-1 trigger.

The brief's scope discipline matters here:

- Dispatch tracks the SHIP event + the recipient + the carrier
  metadata. It does NOT integrate with carrier APIs in v1.
- The auto-spawned invoice is a **draft**, not a NAV submission.
  The operator reviews and clicks Issue per the existing Stage 1
  flow. No silent NAV emit.
- "Partial shipments" — one WO, multiple shipments — is out of
  scope v1. One dispatch per WO.

Constraints carrying over:

- **No DB-engine specifics** — [[no-sql-specific]].
- **Audit F12 ritual** fires twice in this ADR (two new EventKinds).
- **Tenant boundary.**
- **Inventory coupling** — Dispatch emits a `Dispatch` movement
  per ADR-0061.
- **Stage 1 invoice draft pipeline** is the **only** path from
  dispatch to a NAV submission. No bypass.

## Decision

### 1. One new table: `dispatches`

```text
dispatches (
  dsp_id             ULID PRIMARY KEY,    -- prefix `dsp_`
  tenant_id          ULID NOT NULL,
  wo_id              ULID NOT NULL,       -- exactly one dispatch per WO in v1
  partner_id         ULID NOT NULL,       -- recipient; resolves to a partner row
  state              VARCHAR NOT NULL,    -- closed-vocab DispatchState
  created_at         TIMESTAMP NOT NULL,
  shipped_at         TIMESTAMP,           -- NULL while Drafted; set on Shipped
  carrier_kind       VARCHAR,             -- closed-vocab CarrierKind; NULL while Drafted
  tracking_number    VARCHAR,             -- operator-typed or pasted
  spawned_invoice_id ULID,                -- pointer to the invoice draft (if any)
  notes              VARCHAR
);
```

`DispatchState`:

| Variant | Storage string | Allowed transitions FROM |
|---|---|---|
| `Drafted` | `drafted` | (initial — operator created a draft) |
| `Shipped` | `shipped` | Drafted |
| `Cancelled` | `cancelled` | Drafted (only — a Shipped dispatch is terminal) |

`CarrierKind` — closed-vocab, deliberately small for v1:

| Variant | Storage string |
|---|---|
| `SelfDelivery` | `self_delivery` |
| `CustomerPickup` | `customer_pickup` |
| `MagyarPosta` | `magyar_posta` |
| `Gls` | `gls` |
| `Dpd` | `dpd` |
| `Foxpost` | `foxpost` |
| `Other` | `other` |

The list reflects Hungarian-market carriers + the two "no carrier"
cases (self-delivery, customer pickup). New carriers go in by enum
extension; no free-text bypass.

### 2. Eligibility — Stage 3 → Stage 1 gate

A WO becomes **dispatch-eligible** when:

1. WO state is `Completed` (ADR-0062 invariant; itself gated on
   ADR-0063 all-pass).
2. No `dispatches` row exists for the WO (v1 = one dispatch per WO).

The eligibility query lives in `dispatch::eligible_work_orders`. The
"Create Dispatch" SPA affordance is rendered only when both clauses
hold.

**Why this and not "all routing-ops Completed-AND-Passed" directly**:
the WO state IS the canonical eligibility signal per ADR-0062
§"WO state coupling — Completed depends on QA." Re-querying the
routing/QA tables duplicates the logic. The WO state encodes the
gate.

### 3. The handler shape (mock-friendly per Phase γ Cross-cutting #1)

Two handlers:

```rust
pub fn create_dispatch(
    tx: &Transaction,
    wo_id: WoId,
    partner_id: PartnerId,
    actor: ActorKind,
    notes: Option<String>,
    idempotency_key: IdempotencyKey,
) -> Result<DispatchCreatedOutcome, DispatchCreateError>;

pub fn mark_shipped(
    tx: &Transaction,
    dsp_id: DspId,
    carrier_kind: CarrierKind,
    tracking_number: Option<String>,
    shipped_at: Timestamp,
    actor: ActorKind,
    idempotency_key: IdempotencyKey,
) -> Result<DispatchShippedOutcome, DispatchShipError>;
```

SPA buttons → `actor: SpaOperator(user)`. A future barcode
adapter (e.g. shipping-bay scanner that pairs a tracking label with
a dispatch ULID) → `actor: Adapter(name)`. Same handlers.

### 4. Inventory coupling — `mark_shipped` emits a `Dispatch` movement

```rust
inventory::record_movement(
    product_id     = wo_lookup(dispatch.wo_id).product_id,
    qty_delta      = -(wo_lookup(dispatch.wo_id).qty_target),
    reason         = MovementReason::Dispatch,
    ref_kind       = MovementRefKind::Dispatch,
    ref_id         = dispatch.dsp_id,
    operator       = actor.attribution_string(),
    idempotency_key = derive_from(dispatch.dsp_id, "ship"),
)
```

Same transaction as the dispatch state flip + the audit entry.
Half-applied is impossible.

**Why on `mark_shipped` and not on `create_dispatch`** — until the
shipment leaves the building, the stock is still on hand. A
Drafted-then-Cancelled dispatch never decrements stock.

### 5. Invoice auto-spawn — closes the Stage 3 → Stage 1 loop

When `mark_shipped` fires, the handler ALSO calls into the Stage 1
invoice-draft pipeline:

```rust
let draft_input = build_draft_input_from_dispatch(&dispatch, &wo);
let invoice_id = invoicing::create_draft(
    tx,
    draft_input,
    actor,
    idempotency_key = derive_from(dispatch.dsp_id, "spawn_invoice"),
)?;
update_dispatches_set_spawned_invoice_id(tx, dispatch.dsp_id, invoice_id);
```

Same transaction. The draft populates:

- `partner_id` = dispatch.partner_id (the recipient).
- Lines: derived from the BOM **finished good × qty** (one line per
  unique product produced by the WO — in v1, one product per WO,
  so one line). Quantity = `wo.qty_target`. Unit price defaults to
  the product's master-data price; operator can edit pre-Issue.
- Issue date / payment method / currency: pulled from
  `seller.toml` defaults (per [[seller-toml-write-invariant]]
  numbering section). Operator can override on the draft form.

The result is a Stage 1 invoice **draft** sitting in the existing
outgoing-invoices list, marked `Drafted`. The operator follows the
normal Issue → NAV submit → email flow. **There is no auto-submit to
NAV from this path.** The operator's Issue click is the only NAV
trigger.

**Why a draft and not a finalized invoice** — same posture as
ADR-0057 quote intake §"daemon NEVER writes to `invoice`": the
regulated surface (`invoice` table → NAV submission) is operator-
adopted, not background-spawned. Dispatching a WO physically and
issuing the invoice fiscally are two different decisions; v1 keeps
them separate.

**Idempotency** — if a downstream error rolls back the tx, no
draft is created and the dispatch stays Drafted. If
`mark_shipped` is retried (operator double-clicks, adapter fires
twice), the `idempotency_key` on `create_draft` is stable
(`derive_from(dispatch.dsp_id, "spawn_invoice")`); the second call
returns the existing draft id. The `spawned_invoice_id` column
sees one write; subsequent UPDATEs are no-ops.

**Failure handling** — if `create_draft` fails (e.g. partner_id
doesn't resolve, master-data missing), the entire `mark_shipped`
transaction rolls back. The dispatch stays Drafted; the stock
movement is not written; the audit entry is not written. Loud-fail
per [[trust-code-not-operator]] — operator sees the specific error
("Partner lookup failed: no entity matches partner_id") and fixes
the master-data before retrying.

### 6. Audit-ledger integration — two new EventKinds

| EventKind variant | Storage string | Payload carries |
|---|---|---|
| `DispatchCreated` | `mes.dispatch_created` | `dsp_id`, `wo_id`, `partner_id`, `actor`, `idempotency_key` |
| `DispatchShipped` | `mes.dispatch_shipped` | `dsp_id`, `wo_id`, `carrier_kind`, `tracking_number`, `shipped_at`, `spawned_invoice_id`, `actor`, `idempotency_key` |

F12 ritual fires twice. The `mes.` prefix family from ADR-0060 §4
is reused.

**No new EventKind for "auto-spawned invoice draft"** — the
spawn calls the existing `invoicing::create_draft` which emits the
existing `InvoiceDraftCreated` (Stage 1 audit). The dispatch side
records the spawn outcome on its OWN audit entry
(`spawned_invoice_id` field on `DispatchShipped`), so the audit
trail walks both ways: from dispatch to invoice and from invoice's
draft entry to the dispatch (via a future cross-link query).

A Cancelled dispatch does NOT get a dedicated EventKind in v1 —
cancellation in `Drafted` state is rare; the `DispatchCreated` →
no-`DispatchShipped` pattern is the audit signal. If cancellation
volume surfaces, a `DispatchCancelled` kind is an additive future.

### 7. SPA surface

- **New "Dispatch board" main-nav tab** (or under Work Orders;
  flagged as Open Question per the same nav-density review as
  ADR-0063 §"SPA placement").
- **Dispatch board list** with state-facet chips
  (`Drafted` / `Shipped` / `Cancelled`). Default sort:
  `created_at` descending.
- **Eligible-WO list** as a side panel: "Ready to dispatch — N
  work orders." Click → opens the Create-Dispatch form
  pre-filled with WO + suggested partner (if WO has a customer
  hint from a future sales-order link; v1 the operator picks).
- **Create-Dispatch form**: WO picker (filtered to eligible WOs),
  partner picker (filtered to existing partners), notes, optional
  pre-fill of carrier_kind and tracking_number (deferred to
  Mark-Shipped in the typical flow).
- **Mark-Shipped form** (per dispatch row): carrier_kind dropdown
  (closed-vocab), tracking_number text, shipped_at datetime
  (default = now), Confirm.
- **Cancel button** on Drafted rows; modal confirm.
- **Spawned-invoice link** on Shipped rows: "Invoice draft
  created: YYYY-NNNNNN — view." Clicks through to the Stage 1
  invoice detail page.
- **No carrier-API integration** in v1. No label print. No
  shipping cost calc. Operator types everything.

### 8. Cross-cutting decisions (Stage 3 Phase γ shared)

1. **Mock-friendly Adapter consumers.** §3 handler shape. SPA +
   future adapter — same code.
2. **No DB-engine specifics.** State machine in Rust; no CHECK
   on `dispatches.state`.
3. **Audit-ledger F12 ritual.** Two new variants this ADR. Total
   Phase γ audit-kind additions: 1 (ADR-0061) + 3 (0062) + 2 (0063)
   + 2 (0064) = 8 new `EventKind` variants under the `mes.*` prefix.
4. **Tenant boundary** on `dispatches`.
5. **Out-of-scope-by-design list** at §"Out of scope" below.

## Consequences

- **The Stage 3 → Stage 1 loop is closed.** A WO that's Created,
  Released, In-Progress, Completed (with QA all-pass), Dispatched
  Drafted, Shipped — produces an invoice draft sitting in the
  operator's outgoing-invoices queue. The operator clicks Issue,
  NAV submission fires, email goes out. Existing Stage 1 pipeline,
  no changes.
- **No NAV exposure from dispatch.** The Issue gate is the operator's
  decision; dispatch only stages the draft. Same posture as the
  quote-intake daemon's staging-then-adopt model (ADR-0057).
- **One dispatch per WO** simplifies v1 dramatically. Partial
  shipments are a v2 concern; until a real customer asks for split
  shipment of a multi-unit WO, v1 ships.
- **Carrier list is closed-vocab.** Operators who use a sixth
  Hungarian carrier (e.g. Sprinter) submit a PR to extend the enum.
  Same posture as currency, payment method, NAV unit-of-measure —
  closed-vocab everywhere per [[trust-code-not-operator]].
- **Inventory drops on ship**, not on dispatch-create. A drafted-then-
  cancelled dispatch leaves stock untouched.
- **The dispatch carries a `notes` column** for shipping-bay free
  text (e.g. "fragile, top loaded"). Not surfaced on the invoice;
  internal operations note.
- **Stage 1 changes: zero.** The Stage 1 invoice draft pipeline
  is unchanged. `create_draft` is the existing call; the dispatch
  side passes a DraftInput shaped like any other (operator-typed,
  quote-intake-spawned). The Stage 1 outgoing invoice list does not
  visually distinguish dispatch-spawned drafts from other drafts in
  v1 — they're invoice drafts.
- **Future operations dashboard** can join `dispatches ⋈ invoice` to
  surface "shipped but not yet issued" (drafts > 24h old) or
  "shipped + issued + paid" (the full revenue path). Surface
  questions deferred to the operations-dashboard ADR.

## Adversarial review

- *"Auto-spawning an invoice draft on Ship hides the
  decision-to-bill from the operator. They may not realize a draft
  was created until they next open the invoice list."* The Mark-
  Shipped success toast on the dispatch UI shows "Invoice draft
  YYYY-NNNNNN created — view." The audit timeline on the dispatch
  also shows the spawn. v2 could push a notification; v1's surface
  is the form's success state.
- *"What if the operator wants to ship but NOT bill yet (e.g.
  consignment stock to a customer warehouse, billed monthly)?"*
  v1 has no consignment posture. The operator's escape valve is
  to ship, accept the draft, then defer Issuing until the billing
  cycle. The draft sits indefinitely in Drafted state per the
  existing Stage 1 model. Consignment as a first-class concept is
  deferred to a future Stage-1-extension ADR.
- *"`spawned_invoice_id` on dispatches creates a cross-table
  relationship that violates ADR-0019 §'no foreign keys' to some
  degree."* ADR-0019's posture is no DB-level FK; this is an
  application-level pointer (ULID column, application-enforced
  resolution). Same as `invoice.partner_id`, `restored_invoice.tenant_id`, etc. The ULID is the universal pointer per ADR-0005.
- *"Mark-Shipped runs four side effects in one transaction:
  dispatch state, stock movement, invoice draft, audit entries.
  Any one failure rolls back ALL of them — including the audit
  entry for the failed attempt. The operator loses the failure
  forensics."* True for the rollback case. The mitigation:
  `tracing` logs capture the rollback reason at WARN level; the
  HTTP response body carries the structured error. The audit-
  ledger's posture is "successful state changes only" by design.
  An ADR-0032-style "attempt-before-call" pattern could capture
  the failed attempt as a separate audit entry; v1 defers it
  (no NAV-grade idempotency need at the dispatch layer).
- *"One dispatch per WO is wrong if a 100-unit WO ships in 4
  truckloads."* True for that hypothetical. Áben's current scale
  (single CNC operator, custom-machined parts in batches of 1–10)
  makes one-shipment-per-WO the right v1 cut. The v2 path: split
  `dispatches` so dispatch.wo_id becomes dispatch_lines with
  per-line qty; aggregate per dispatch. Filed as Open Question.
- *"`Cancelled` from `Drafted` is the only cancel path. What if
  the operator marks shipped, the truck breaks down, and the
  shipment never actually leaves?"* v1: open a new dispatch when
  the goods are re-shipped, and flag the old as "shipped"
  forensically wrong. v2: an `Undelivered` state that reverses the
  stock movement and the invoice draft. v1 defers — Áben's scale
  makes this rare.
- *"Spawned invoice draft references no audit-link to the
  dispatch on its side — the Stage 1 invoice timeline has no
  natural pointer to the dispatch."* True. The Stage 1 `InvoiceDraftCreated` payload could carry an optional
  `source_dispatch_id` field (additive); flagged as a Phase γ
  follow-on (out of scope this ADR since it touches Stage 1
  surface area).
- *"What about VAT and currency on the auto-spawned draft?"*
  Defaults from `seller.toml` numbering + currency posture per
  ADR-0037 (EUR-denominated outgoing). v1: HUF default; operator
  switches to EUR pre-Issue if the customer is non-domestic. Same
  surface as operator-typed drafts.

## Alternatives considered

- **Auto-issue (auto-NAV-submit) on Ship.** Refused — silent NAV
  submissions break the [[no-smoke-test-in-prod]] posture by spirit
  and remove the operator's decision-to-bill gate. ADR-0057's
  quote-intake-stages-not-burns is the precedent.
- **Single combined `mark_shipped_and_issue` handler.** Refused —
  Ship and Issue are two operator decisions; collapsing them is
  the same anti-pattern.
- **Dispatch table with N-to-1 WOs per dispatch (consolidated
  shipments).** Refused for v1 — Áben's scale doesn't need it.
  v2 path is via dispatch_lines.
- **Reuse ADR-0011's "Order + logistics state machine" stub
  (ADR-0015) as the dispatch ADR.** Refused — ADR-0015's scope is
  sales/PO state machines AND logistics; v1 ships dispatch only.
  ADR-0015 stays a stub for the sales-order side.
- **Carrier as free text instead of closed-vocab.** Refused per
  the project's closed-vocab discipline (currency, payment method,
  NAV unit-of-measure all closed-vocab).
- **Invoice draft creation via post-commit hook (eventual
  consistency) instead of in the same transaction.** Refused —
  the [[trust-code-not-operator]] posture favors all-or-nothing.
  Operators seeing "Shipped" should know the draft exists.
- **One `EventKind::DispatchLifecycle`** with payload
  discriminator. Refused per the per-kind glob argument
  (ADR-0062 §4).

## Open questions

1. **SPA placement** — Dispatch board as a top-level nav tab vs
   sub-tab under Work Orders. Same trigger as ADR-0063 §"SPA
   placement."
2. **Partial shipments / consolidated shipments** (N WOs per
   dispatch OR multiple shipments per WO). Trigger: first
   customer requests it.
3. **Stage 1 `InvoiceDraftCreated` payload extension** with
   `source_dispatch_id`. Trigger: operations dashboard needs the
   reverse link, OR an audit reviewer asks "which invoice came
   from which dispatch."
4. **Consignment-stock posture** (ship without billing). Trigger:
   first consignment customer.
5. **Carrier-API integration** (Magyar Posta tracking pull, GLS
   label print). Trigger: operator volume justifies it.
6. **Returns / undelivered handling.** Trigger: first
   undelivered shipment.
7. **Dispatch SLA / nag** ("shipped but not invoiced > N days").
   Trigger: operations dashboard ADR ships.
8. **Multi-line dispatches** (one WO produces multiple distinct
   finished goods — v2 sub-assembly extension). Trigger: BOM
   nesting (ADR-0062 Open Q §"BOM nesting") lands.
9. **Auto-suggested partner from a sales-order link.** Trigger:
   sales-order module ships (ADR-0015 unstubs).
10. **Bilingual carrier_kind labels** (Hungarian + English for the
    SPA dropdown). Trigger: SPA i18n review.

## Out of scope (deliberately)

- **Carrier API integration / label print / cost calc.** v1 manual.
- **Partial shipments.** One dispatch per WO.
- **Returns / RMA.** Future ADR.
- **Consignment stock.** Future ADR.
- **Multi-product per dispatch.** Tied to BOM nesting.
- **Auto-NAV submission.** Operator's Issue click only.
- **Consolidated invoicing** (N dispatches → 1 invoice). v1: one
  draft per ship.
- **Returns-side stock movements** (`MovementReason::Return`).
  Closed-vocab extension when returns surface.
- **Dispatch-level audit for Cancelled.** No dedicated kind in v1.
- **DB CHECK constraints** on `dispatches.state` or
  `dispatches.carrier_kind`. App-layer.

## Invariants pinned

1. **`mark_shipped` writes exactly one `Dispatch` `stock_movements`
   row in the same transaction as the state flip + the audit
   entry + the invoice-draft spawn.** Pinned by
   `mark_shipped_writes_movement_and_spawns_draft_in_same_tx`.
2. **`mark_shipped` is a no-op if the dispatch is already
   `Shipped` (idempotency).** Pinned by `mark_shipped_idempotent_on_already_shipped`.
3. **`create_dispatch` refuses if WO is not `Completed` OR if
   another `dispatches` row already exists for the WO.** Pinned by
   `create_dispatch_refuses_ineligible_wo` and
   `create_dispatch_refuses_duplicate_for_wo`.
4. **The auto-spawned invoice is a `Drafted` invoice (no NAV
   submission).** Pinned by `spawn_invoice_creates_drafted_not_issued`.
5. **The `spawned_invoice_id` on the dispatch row points to the
   invoice that the same transaction created.** Pinned by
   `spawned_invoice_id_matches_draft_created_in_same_tx`.
6. **A failed `create_draft` rolls back the entire `mark_shipped`
   transaction — no dispatch state flip, no stock movement, no
   audit entry.** Pinned by `mark_shipped_rolls_back_on_draft_failure`.
7. **`actor: SpaOperator | Adapter | System` captured into every
   audit entry; handler does not branch on actor for state
   transitions.** Pinned by code review + structural test.
8. **`CarrierKind` is closed-vocab; free text refused at the
   boundary.** Pinned by `carrier_kind_rejects_free_text_at_route_boundary`.
