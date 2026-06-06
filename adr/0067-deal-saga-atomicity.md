# ADR-0067 — DEAL saga: single-transaction atomic cascade from accepted quote to Work Order, with one deliberate pause-seam for the vendor-PO gate

- **Status:** Proposed
- **Date:** 2026-06-06
- **Deciders:** Ervin (via S265 auto-quoting ground-zero brief)
- **Supersedes:** none.
- **Related:** ADR-0066 (quote architecture), ADR-0068 (vendor-PO authorization — the pause-seam), ADR-0069 (material reservation — the reserve step), ADR-0062 (work orders — the saga's output), ADR-0064 (dispatch — the same staging-not-burning posture, and the downstream consumer of the WO), ADR-0015 (sales/PO state machine — **stub**; the reason the conversion target is a WO not an SO), ADR-0008 (audit ledger), the design doc [`docs/design/auto-quoting-ground-zero.md`](../docs/design/auto-quoting-ground-zero.md), and [[trust-code-not-operator]], [[hulye-biztos]].

## Context

When the operator agrees to a customer-accepted quote, a cascade must fire: earmark the materials, procure what's short, and kick off manufacturing. The brief calls this the **DEAL**. Three things make it non-trivial:

1. **Partial application is the enemy.** A DEAL that reserved materials but failed to create the Work Order — or created the WO but failed to reserve — leaves the operator with a corrupt half-state and no clear recovery. The whole cascade must be all-or-nothing (trust-code-not-operator).

2. **One step legitimately needs a human.** If procuring a short material means a vendor PO above the auto-spend ceiling, the saga cannot just proceed (it would auto-spend beyond authority) nor just fail (it would discard the reservations already taken for the *available* materials). It must **pause** for an operator decision.

3. **The conversion target does not exist as the brief named it.** The brief's terminal state `converted_to_so` assumes a Sales Order entity. ADR-0015 is a stub; there is no SO module. The cascade's real output in v1 is a **Work Order** (ADR-0062).

## Decision

**The DEAL is one DB transaction that converts an `accepted` quote into a `Work Order`, with exactly one pause-seam: the vendor-PO threshold gate.**

### 1. The DEAL token (hülye-biztos)

The operator confirms by typing a **single typed token** (e.g. `DEAL`), not by clicking through a multi-field confirmation dialog. One deliberate token is harder to fat-finger past than a checkbox lawyer wall and leaves a clean audit intent. The token submit is the saga trigger.

### 2. Preconditions (refuse loud, before BEGIN)

- `quote.state == accepted` — a DEAL on an `indicative` or already-`converted` quote is refused.
- an HMAC acceptance is on record for this `quote_id`.
- the frozen `calculated_breakdown_json` is present and its hash matches the `quote.auto_estimated`/override chain.

Any precondition failure is a loud refusal with the specific reason; nothing is written.

### 3. The cascade (one transaction)

```
BEGIN TX
  1. emit deal.started (audit; payload = quote_id, frozen-breakdown hash, operator)
  2. quote.state := binding
  3. for each BOM material line in the frozen breakdown:
       atp := stock_qty(prd) − SUM(open reservations on prd)            (ADR-0069)
       if atp >= need:
          insert stock_reservation(prd, need, ref=quote_id)             → mes.stock_reserved
       else:
          reserve atp (if > 0); short := need − atp
          po_eur := cost_of(short)
          if po_eur <= max_auto_po_eur AND day_running + po_eur <= daily_cap:   (ADR-0068)
              record vendor PO, fire supplier email                     → po.vendor_po_fired
          else:
              GOTO pause-seam (§4)
  4. create Work Order from BOM × qty (ADR-0062); set quote.converted_wo_id
  5. quote.state := converted
  6. emit deal.completed (audit; payload carries wo_id + reservation_ids + po_ids)
COMMIT
```

On **any** error before COMMIT → `ROLLBACK`; then a best-effort separate-tx `deal.rolled_back` audit entry naming the failing step. Post-rollback the quote is back at `accepted`, with **zero** reservations, **zero** PO records, **zero** WO. The operator fixes the named cause (e.g. missing master data) and retries the token.

### 4. The pause-seam — the one non-atomic point

When a vendor PO breaches the threshold (§3 step 3-else-else), the saga does **not** roll back and does **not** proceed. It **pauses**:

- the reservations already taken for *available* materials are **held** under the paused quote (not rolled back — re-acquiring them is wasteful and races other deals);
- `quote.state` carries a `deal_paused` sub-flag;
- `po.auto_threshold_exceeded` is emitted with the PO detail;
- the SPA surfaces an operator gate: *"PO of €X for `material` exceeds your €Y auto-limit — Approve or Decline."*

**Operator Approve** → resume the saga from step 3 for the remaining lines (the PO is now authorized), then steps 4–6 in a fresh transaction. **Operator Decline** → roll back the held reservations, `deal.rolled_back`, quote returns to `accepted`. The paused state is durable across a crash (it is `quotes` state + the audit entry, not in-memory), so a restart mid-pause resumes the gate.

This is the **only** deliberate seam in an otherwise atomic saga. It exists because the alternative — rolling back a 40-line BOM because one exotic line needs human spend authority — is operator-hostile.

### 5. Conversion target = Work Order (not Sales Order)

`quote.converted_wo_id` points at the created WO. There is **no** Sales Order in v1 (ADR-0015 stub). The state is named `converted` (not `converted_to_so`) so the name survives a future ADR-0015 unstub: when a real SO entity lands, it slots *above* the WO and `converted` still describes "this quote produced a downstream manufacturing artifact."

The DEAL does **not** create an invoice. Per ADR-0064, the invoice draft is born at **Dispatch** (WO shipped), operator-issued to NAV. The regulated surface stays operator-gated; the DEAL only starts manufacturing.

### 6. Audit chain

`deal.started` (open) → N × `mes.stock_reserved` + 0..N × `po.vendor_po_fired`/`po.auto_threshold_exceeded` → `deal.completed` (close, carrying every child id) **or** `deal.rolled_back`. An auditor walks quote → reservations → POs → WO from the close entry's payload. F12 ritual fires three times in this ADR (`deal.started`/`deal.completed`/`deal.rolled_back`); reservation and PO kinds are owned by ADR-0069 / ADR-0068.

## Consequences

- **No corrupt half-states.** Either a DEAL produces a WO with all materials reserved/procured, or it produces nothing and the quote is back at `accepted`.
- **Human spend authority is preserved without sacrificing atomicity elsewhere.** The pause-seam is surgical: it isolates the one step that needs judgment, holds the deterministic work already done, and resumes or unwinds on the operator's single decision.
- **The DEAL is a pure orchestration unit.** S273 ships it with the reservation and PO steps as typed *seams* (no-op stubs) — the atomic shell, the state machine, the WO creation, and the audit chain are reviewable before reservation's inventory-coupling (S274) or the PO module (post-S275) land.
- **The conversion is to manufacturing, not billing.** Consistent with ADR-0064: dispatching and invoicing are separate operator decisions downstream.
- **Locks held across the transaction.** Reserving materials reads-then-writes ATP; concurrent DEALs on overlapping materials serialize (DuckDB single-process; the ATP read re-sums reservations at write time per ADR-0069, so the second DEAL sees the first's reservation).

## Adversarial review

- *"The pause-seam means a DEAL can sit half-done indefinitely if the operator never decides."* True, and acceptable: the held reservations are visible (they decrement ATP, surfaced on the materials list), the paused quote is visible (state chip), and the `po.auto_threshold_exceeded` entry is in the timeline. A future SLA-nag can surface "paused > N days." The held reservations are the cost of not re-acquiring; an operator who abandons the gate can Decline to release them.
- *"Emitting `deal.started` inside the tx means a rollback erases the 'we tried' record — same forensic loss ADR-0064 §adversarial flagged."* Same posture, same answer: the audit ledger is successful-state-only by design; `deal.rolled_back` (best-effort separate tx) plus WARN-level `tracing` capture the attempt. v1 does not need NAV-grade attempt-before-call here.
- *"WO creation failing at step 4 wastes the reservations taken at step 3."* No — they are in the same transaction; a step-4 failure rolls back step 3's reservations too. The only held-across-failure case is the explicit pause-seam, which is a deliberate human gate, not a failure.
- *"Two operators (future multi-operator tenant) could both DEAL quotes that need the same scarce material."* The ATP re-sum at reservation write time (ADR-0069) makes the second DEAL see the first's reservation and route to the PO path or pause. Single-process serialization holds today; a future multi-process deployment needs the `SELECT ... FOR UPDATE` posture ADR-0061 §adversarial already names.
- *"Resuming a paused saga 'from step 3 for remaining lines' in a fresh transaction breaks the all-or-nothing claim."* The resume is itself one transaction (remaining reservations + WO + state flip + audit); the pre-pause reservations are its inputs, already durable. Atomicity holds per-transaction; the pause is the documented seam between two transactions, gated by an explicit human decision. This is the honest shape of a saga with a human-in-the-loop step.

## Alternatives considered

- **Eventual-consistency saga (steps as separate committed stages with compensations).** Rejected — compensation logic (un-reserve, un-PO, un-WO) is exactly the corrupt-half-state surface trust-code-not-operator avoids. One transaction with one explicit human seam is simpler and safer at this scale.
- **Roll back the whole DEAL when any PO breaches threshold.** Rejected — operator-hostile for large BOMs with one exotic line. The pause-seam holds the deterministic work.
- **Auto-approve POs up to the ceiling with no daily cap.** Rejected — a runaway (many sub-ceiling POs in one batch) could spend far beyond intent. The daily cap (ADR-0068) is the backstop.
- **Create a Sales Order as the conversion target.** Rejected — no SO module (ADR-0015 stub). Inventing one here is out of scope; the WO is the real artifact.
- **Auto-issue the invoice on DEAL.** Rejected — same posture as ADR-0064: billing is a separate operator decision, fired at Dispatch, operator-gated to NAV.
- **Multi-step confirmation dialog instead of a single DEAL token.** Rejected per hülye-biztos — a typed token is one deliberate act; a checkbox wall trains click-through.

## Open questions

1. **Paused-DEAL SLA / nag.** Trigger: first operator complaint about forgotten paused gates, or the operations-dashboard ADR.
2. **Multi-operator DEAL concurrency.** Trigger: first multi-operator tenant; resolves with the same `FOR UPDATE` posture ADR-0061 names.
3. **Sales-order conversion target.** Trigger: ADR-0015 unstubs. The `converted` state and `converted_wo_id` extend with a `converted_so_id` then.
4. **Machine-slot reservation in the saga.** Trigger: a Scheduling/Capacity board ADR ships (design doc §7). Until then no slot step exists; `MachineSlotReserved` is specced-not-emitted.

## Invariants pinned

1. **The DEAL cascade is one transaction; any pre-COMMIT failure rolls back reservations + PO records + WO + the `deal.started` entry.** Pinned by `deal_rolls_back_all_on_wo_failure`.
2. **A DEAL refuses unless `quote.state == accepted` with HMAC acceptance and a present frozen breakdown.** Pinned by `deal_refuses_non_accepted` and `deal_refuses_missing_acceptance`.
3. **The vendor-PO threshold breach pauses (does not roll back), holding prior reservations.** Pinned by `deal_pauses_on_po_over_threshold_holds_reservations`.
4. **Operator Decline on a paused DEAL releases held reservations and returns the quote to `accepted`.** Pinned by `paused_deal_decline_releases_reservations`.
5. **`quote.converted_wo_id` points at the WO created in the same successful transaction; no invoice is created by the DEAL.** Pinned by `deal_creates_wo_not_invoice`.
6. **`deal.completed` payload carries every reservation_id, po_id, and the wo_id.** Pinned by `deal_completed_payload_links_children`.
