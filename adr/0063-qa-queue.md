# ADR-0063 — QA queue v1: `qa_inspections` per routing-op completion, manual pass/fail authoritative, future Renishaw adapter is a co-equal trigger

- **Status:** Accepted
- **Date:** 2026-06-03
- **Deciders:** Ervin (via S230 Stage 3 Phase γ brief)
- **Supersedes:** none — new entity sized for Phase γ.
- **Related:** ADR-0060 (Stage 3 framework — consumes the `CanonicalEvent::QualityResultReceived` vocab), ADR-0062 (Work Orders — auto-creates inspections on routing-op Completed), ADR-0064 (Dispatch — gates eligibility on all-pass), ADR-0061 (Inventory — Dispose emits a `Scrap` movement), the memory pins [[no-sql-specific]], [[trust-code-not-operator]], [[hulye-biztos]], [[aberp-stage3-manufacturing]].

## Context

ADR-0060 §1 declared `CanonicalEvent::QualityResultReceived` for
Phase ζ (Renishaw on-machine probe). That event vocab will arrive
when the hardware lands. **Before** the hardware lands, ABERP needs
to support QA gates on every routing operation that a human inspector
signs off — a CNC shop with 3 in-progress WOs has 9–15 ops in flight
and needs a triage queue, not a "wait for the Renishaw arm to scan
it" pattern.

ADR-0062 §"On Completed" deliberately deferred the WO transition to
Completed until QA has cleared the relevant ops. This ADR is the
gate that closes that deferral: auto-create a `qa_inspections` row
on every routing-op completion, surface them in a QA queue, let
the operator (or a future adapter) decide Pass / Fail / Rework /
Dispose.

The brief is firm on a key point: **manual buttons remain
authoritative even after the Renishaw adapter lands.** An adapter
result auto-flips Pending → Passed/Failed, but the operator can
ALWAYS override. The right shape is: adapter writes the decision,
audit captures who wrote it, operator can write again later. No
"this row is adapter-owned, you can't touch it" lockout.

Constraints carrying over:

- **No DB-engine specifics** — [[no-sql-specific]].
- **Audit F12 ritual** fires twice in this ADR (two new EventKinds).
- **Tenant boundary** on `qa_inspections`.
- **Inventory coupling** — Dispose emits a `Scrap` movement.
- **Single-process** per ADR-0060.

## Decision

### 1. One table: `qa_inspections`, one row per routing-op completion

```text
qa_inspections (
  qa_id              ULID PRIMARY KEY,    -- prefix `qa_`
  tenant_id          ULID NOT NULL,
  wo_id              ULID NOT NULL,       -- which work order
  routing_op_id      ULID NOT NULL,       -- which op completed
  state              VARCHAR NOT NULL,    -- closed-vocab QaState
  decided_at         TIMESTAMP,           -- NULL while Pending
  decided_by         VARCHAR,             -- attribution; NULL while Pending
  reason             VARCHAR,             -- free text on Fail / Rework / Dispose
  measurement        VARCHAR,             -- optional, may carry an adapter blob
  source_event_id    VARCHAR,             -- ULID of the triggering event, if any
  created_at         TIMESTAMP NOT NULL,
  superseded_by      ULID                 -- NULL unless a later qa_id overrode
);
```

`QaState`:

| Variant | Storage string | Allowed transitions FROM |
|---|---|---|
| `Pending` | `pending` | (initial — auto-created when routing op flips to Completed) |
| `Passed` | `passed` | Pending |
| `Failed` | `failed` | Pending, Passed (operator override after-the-fact) |
| `Reworking` | `reworking` | Failed |
| `Disposed` | `disposed` | Failed, Reworking |

`Passed → Failed` is allowed (operator caught a defect after the
fact). `Failed → Reworking → Passed`: a rework that succeeds.
`Failed → Disposed`: the part is scrap; emit a `Scrap` movement.

### 2. Auto-creation on routing-op Completed

ADR-0062 §"§5 Inventory coupling" runs inside the
`transition_routing_op` handler. Same handler, additional step: when
a routing-op flips to `Completed`, INSERT one `qa_inspections` row
with `state = Pending` in the same transaction. The QA queue's
"Pending" view picks it up immediately.

**Every op gets a Pending inspection.** v1 does not let the operator
opt out — a "this op doesn't need QA" decision is an explicit
`Pass` click (or v2 a `Skip` state). The reason: a default-skip
posture invites silent gaps. Pending + 1-click Pass is fast enough.

### 3. The handler shape (mock-friendly per Phase γ Cross-cutting #1)

```rust
pub fn decide_qa(
    tx: &Transaction,
    qa_id: QaId,
    to_state: QaState,
    actor: ActorKind,
    reason: Option<String>,
    measurement: Option<String>,
    source_event_id: Option<EventId>,
    idempotency_key: IdempotencyKey,
) -> Result<QaDecisionOutcome, QaTransitionError>;
```

- SPA Pass / Fail / Rework / Dispose buttons → `actor =
  SpaOperator(user)`, `source_event_id = None`.
- Future Renishaw adapter event → adapter handler resolves the
  `qa_id` from the routing-op id carried in the canonical event,
  calls `decide_qa(..., actor: Adapter("renishaw-cell-A"),
  source_event_id: Some(adapter_event_ulid))`. Same handler.
- The handler does NOT branch on actor for the state-transition
  logic. The audit entry carries the actor for the operator's later
  trace.

### 4. Operator-override-after-adapter: `superseded_by`

When an operator clicks Fail on a row the adapter just flipped to
Passed (or vice versa), the existing row is NOT mutated. Instead:

- INSERT a NEW `qa_inspections` row with the new state.
- UPDATE the prior row's `superseded_by` to the new `qa_id`.

This preserves the adapter's decision in the audit trail while
making the operator's decision the live state. The "live state" of
the inspection is the row WHERE `superseded_by IS NULL` AND
`wo_id = ?` AND `routing_op_id = ?`.

**Why not mutate in place?** The brief said "manual buttons remain
authoritative (operator can override adapter)" — the simpler
interpretation is "operator UPDATEs the state." The supersede
posture is **stronger**: the adapter's reading is preserved
forensically. A future quality dispute ("the Renishaw said pass,
why was this rejected?") has a durable audit trail.

The supersede only fires when the new actor differs from the live
row's actor. Operator-to-operator state changes within the same
inspection (Pending → Failed → Reworking → Passed) all mutate the
single row. Operator-vs-adapter is the cross-actor boundary that
triggers supersede.

### 5. Audit-ledger integration — two new EventKinds

| EventKind variant | Storage string | Payload carries |
|---|---|---|
| `QaInspectionCreated` | `mes.qa_inspection_created` | `qa_id`, `wo_id`, `routing_op_id`, `actor`, `idempotency_key` |
| `QaInspectionDecided` | `mes.qa_inspection_decided` | `qa_id`, `from_state`, `to_state`, `reason`, `measurement`, `actor`, `source_event_id`, `superseded_qa_id`, `idempotency_key` |

F12 ritual fires twice for this PR. Prefix pins from ADR-0060 §4
reused; new `s230_qa_kinds_use_mes_prefix`.

### 6. Inventory coupling — `Disposed` emits Scrap

`decide_qa(..., to_state: Disposed, ...)` writes one
`stock_movements` row with:

- `qty_delta = -(qty_per_unit_at_this_op)` — there is no separate
  "per-op output qty" in v1; the disposal is sized at the WO's
  qty_target prorated by the op's position (in v1 we assume the
  whole WO qty is disposed when an op fails terminally; the operator
  can hand-adjust).
- `reason = MovementReason::Scrap` (per ADR-0061 §2).
- `ref_kind = MovementRefKind::QaInspection`, `ref_id = qa_id`.

Same transaction as the QA state change + the audit entry. Half
applied is impossible.

**`Rework` does NOT emit a movement.** The part has not been
disposed; it goes back to the floor. ADR-0062's routing handles
the rework by inserting a new routing-op (operator triggers a "redo
op" affordance in v2; v1 the operator manually adds a fresh op via
the WO detail form).

**`Failed` without subsequent Dispose** also emits no movement. The
WO sits at the failed op until the operator decides Rework or
Dispose; the inventory side is unaffected.

### 7. WO state coupling — Completed depends on QA

ADR-0062 §"On Completed" deferred the WO state flip to Completed
until "QA passes all required ops." This ADR pins what "required"
means in v1:

**ALL routing ops of the WO must have at least one
non-superseded `qa_inspections` row in state `Passed`** before the
WO state can flip to `Completed`. The handler check:

```rust
fn wo_completion_eligible(wo_id) -> bool {
    routing_ops_of(wo_id).all(|op|
        live_qa_inspection_for_op(op).map_or(false, |q| q.state == Passed)
    )
}
```

A Completed WO with all-pass also fires the
`MovementReason::WoCompletion` positive movement per ADR-0062 §5.

**`Disposed` blocks WO completion.** A disposed op leaves the WO
in a state with no path to Completed. The operator's recovery is
to Cancel the WO (per ADR-0062 §"State machine") and start a fresh
one if the part is to be re-made.

### 8. SPA surface

- **New "QA queue" main-nav tab** (or a sub-tab under Work Orders;
  TBD per the design language ADR-0017's nav density review — flag
  as Open Question §"SPA placement").
- **QA queue list** with state-facet chips (`Pending` is the
  default filter). Columns: WO, product, routing-op name, created_at,
  actor (if decided), state.
- **Pass / Fail / Rework / Dispose buttons** per row. Fail prompts
  for a reason; Dispose prompts for reason + confirms scrap with a
  modal ("This will write off N units to scrap. Continue?").
- **WorkOrderDetail integration** — the routing-ops table on the
  WO detail page surfaces the live QA state per op as a chip next
  to the op state.
- **No measurement-blob viewer** in v1. The `measurement` column
  carries adapter blobs (Renishaw report fragments) but v1 surfaces
  them as raw strings only; structured-rendering is a v2 question.

### 9. Cross-cutting decisions (Stage 3 Phase γ shared)

1. **Mock-friendly Adapter consumers.** §3 + §4 handler — same code
   for SPA + adapter, supersede preserves both decisions.
2. **No DB-engine specifics.** State machine in Rust; no CHECK on
   `qa_inspections.state`.
3. **Audit-ledger F12 ritual.** Two new variants this ADR.
4. **Tenant boundary** on `qa_inspections`.
5. **Out-of-scope-by-design list** at §"Out of scope" below.

## Consequences

- **Every routing op gets a deliberate Pass click.** Operator
  friction is real but the alternative (default-skip QA) loses the
  Stage 3 vision's quality posture.
- **Dispose is a regulated action.** Modal confirm + scrap movement
  + audit entry + WO-completion-blocked. The operator can't
  fat-finger their way to silent stock loss.
- **Adapter integration is mechanical.** The Renishaw adapter
  resolves the routing-op id from its scan, looks up the live
  qa_id, calls `decide_qa(..., actor: Adapter("renishaw-cell-A"),
  ...)`. Operator override later supersedes the adapter row but
  preserves the original measurement.
- **Forensic audit story is durable.** A future quality dispute
  walks the audit ledger for `mes.qa_inspection_decided` entries
  matching the wo_id; supersede chain reveals every actor change.
- **WO Completed is a derived gate, not a button.** ADR-0062's
  Complete button on the WO detail page is enabled only when
  `wo_completion_eligible` returns true. The operator sees the
  reason via a disabled-with-tooltip ("Waiting for QA pass on op 3:
  finish-mill").
- **Routing ops without separately-tracked output qty** means
  partial-failure (op 3 of 6 fails on 1 of 10 units) is not
  representable in v1. The operator must either dispose the whole
  qty (Dispose) or rework all 10 (Reworking). Flagged as Open
  Question §"Partial-qty inspections."
- **Audit traffic grows.** Each routing op = 1 Created + ≥1 Decided.
  A 6-op WO is ~12+ entries. 5 WOs/day × 12 = ~60 audit entries/day
  from QA. Sizing data point.

## Adversarial review

- *"Every routing op needs a Pending → Pass click — operators will
  click through reflexively and the QA gate becomes ceremony.
  Real quality posture would require a measurement field or photo
  before allowing Pass."* True in spirit, but v1 ships without that
  friction. The op-name itself is the contextual signal; the audit
  entry captures the actor. v2 can add "Pass requires a non-empty
  measurement" per op-name pattern. The operator-survey trigger:
  first quality incident traced back to a reflexive-click.
- *"Operator-override-after-adapter via supersede is wrong shape —
  the Renishaw is more reliable than the operator and v2 will
  trust the adapter. Why preserve operator override at all?"*
  The brief said "manual buttons remain authoritative" — Ervin's
  call. The cost of being wrong (operator overrides correct adapter
  pass-result) is one re-inspection; the cost of disabling override
  (adapter wrongly fails a part, operator has no recovery) is a
  scrapped good. Asymmetric; the brief's call is the safe one.
- *"`Failed → Passed` is not in the allowed-transition list — but
  `Failed → Reworking → Passed` is. What's the difference?"* The
  difference is the audit story. `Failed → Reworking → Passed`
  documents that rework happened; `Failed → Passed` directly would
  silently flip a failed inspection to pass with no rework record.
  The transition table refuses the direct path on purpose.
- *"Supersede chain: a Renishaw adapter and an operator ping-pong
  on the same op (adapter writes Pass, operator overrides Fail,
  adapter re-fires Pass, operator re-overrides Fail). The
  `qa_inspections` table grows linearly with the ping-pong."* True.
  The mitigation: operator UI shows the supersede count when > 1
  ("This inspection has been overridden 3 times — see audit timeline").
  Real ping-pong is an operations problem, not a data problem.
  v1 doesn't limit the chain length.
- *"`Disposed` triggers a Scrap movement sized at the WO's qty_target
  — but if the WO target was 10 and 9 passed earlier ops with the
  10th failing on the LAST op, the scrap qty is 1, not 10."* This
  is the partial-qty problem; flagged as Open Question. v1's
  per-WO-qty-disposal model is wrong for any multi-unit run with
  defects mid-run. The brief deferred per-op output qty; this is
  the cost. Operators with multi-unit runs work around it by
  splitting WOs (one per unit) — friction but workable.
- *"QA state machine has no `OnHold` (a 'waiting for the inspector
  to come back from lunch' state). Operators will use `Pending` to
  mean both 'not yet inspected' and 'inspector busy'."* True.
  Trigger for adding an OnHold: first operator complaint that the
  Pending queue mixes truly-new with in-flight inspections. Cheap
  to add later.
- *"WO completion blocked on all-pass means a WO can sit Completed-
  but-pending forever if the operator forgets to QA the last op.
  No nag mechanism in v1."* True. The operations dashboard
  (future ADR) is the right home for "WOs stuck in QA > N hours."
  v1's surface is the WO list filtered by state; operators see the
  backlog directly.

## Alternatives considered

- **One `EventKind::QaLifecycle` with a payload discriminator.**
  Refused per the ADR-0062 §4 logic: per-kind globs serve the
  future operations-dashboard projection.
- **Mutate the live row on operator override (no supersede).**
  Refused — loses the adapter's measurement forensically. §4
  argument.
- **Track the supersede chain via the audit ledger only, not via
  a column on `qa_inspections`.** Refused — finding the "live"
  inspection for a routing-op would require an audit-walk per query.
  The `superseded_by` column lets a flat SELECT resolve it. Same
  posture as the [[storno-workflow-adr0049]] `is_storno` wire
  field: denormalized for read-cheapness.
- **Default-skip QA (auto-Pass on routing-op Completed, no
  Pending row).** Refused — defeats the QA gate. Operator must
  consciously click Pass.
- **Per-op output qty + per-unit serial tracking** (so partial
  failures are representable). Refused for v1 — sized for v2 when
  serial tracking lands.
- **Reuse `mes.adapter_event` for adapter-driven QA decisions.**
  Refused per the broadcast-lossiness argument from ADR-0061 §4.
  Regulated entity state needs lossless audit.

## Open questions

1. **SPA placement** — QA queue as a top-level nav tab vs sub-tab
   under Work Orders. Triggers: design-language review per
   ADR-0017's nav density posture.
2. **Partial-qty inspections** — represent "9 of 10 passed, 1
   disposed" without splitting the WO. Trigger: first multi-unit
   defect.
3. **QA `OnHold` state** for inspector-busy. Trigger: operator
   complaint.
4. **Measurement blob structured viewer** for Renishaw adapter
   reports. Trigger: ADR-0060 Phase ζ ships AND operator survey
   says raw-string display is unreadable.
5. **`Pass-requires-measurement` enforcement** per op-name pattern.
   Trigger: first quality incident from a reflexive Pass click.
6. **QA-decision idempotency** for adapter events that fire twice
   for the same scan. The `idempotency_key` parameter is the gate;
   the Renishaw adapter must derive a stable key.
7. **Inspector role / permission model.** v1 has no roles —
   anyone can decide_qa. v2 may want a separate "inspector"
   capability per ADR-0007 §Capabilities.
8. **QA-required vs QA-optional per routing op.** v1: every op.
   v2: per-op-template flag in the product-default-routing.
9. **Auto-complete WO on all-pass** — when the last op flips to
   QA Passed, the WO auto-flips to Completed. Trigger: operator
   survey says the manual "Complete WO" click is friction. Tied
   to ADR-0062 §Open Questions §"Auto-complete on last op."

## Out of scope (deliberately)

- **Per-unit inspection** (serial-level QA). Whole-qty in v1.
- **Inspector roles / permissions.** Anyone decide_qa.
- **Measurement-required gate.** Free Pass click.
- **Structured-measurement viewer.** Raw string surface.
- **QA SLA / nag mechanism.** Future ops dashboard.
- **Rework cost tracking.** v2.
- **Statistical process control (SPC).** Stage 3 future.
- **DB CHECK constraints** on `qa_inspections.state`. App-layer.

## Invariants pinned (load-bearing for ADR-0064)

1. **Every routing-op Completed creates exactly one Pending
   `qa_inspections` row in the same transaction.** Pinned by
   `routing_op_completed_creates_qa_inspection_in_same_tx`.
2. **Every QA decision emits exactly one `QaInspectionDecided`
   audit entry.** Pinned by `decide_qa_emits_one_audit_entry`.
3. **Cross-actor decision INSERTs a new row + sets the prior
   row's `superseded_by`.** Pinned by
   `cross_actor_decision_creates_new_row_supersedes_prior`.
4. **Same-actor decision UPDATEs the existing row.** Pinned by
   `same_actor_decision_updates_in_place`.
5. **`Disposed` emits exactly one `Scrap` `stock_movements` row in
   the same transaction.** Pinned by
   `dispose_emits_one_scrap_movement_in_same_tx`.
6. **`wo_completion_eligible(wo_id)` returns true only when every
   routing-op has a live `qa_inspections` row in `Passed`.** Pinned
   by `wo_completion_eligible_requires_all_ops_passed`.
7. **Illegal QA transitions refused at handler with 400.** Pinned by
   `decide_qa_refuses_illegal_state_pair` per the §1 table.
8. **`actor: SpaOperator | Adapter | System` captured into every
   audit entry.** Pinned by code-review + structural test.
