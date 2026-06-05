# PROD_v2.9.0 — WO auto-complete on last-op QA pass

**Cutover date:** TBD (S243 / PR-237; release branch push happens on
the operator's prod machine via `./run/release.sh PROD_v2.9.0`).
**Predecessor:** `PROD_v2.8.5` (S242 / PR-236, Outgoing-list Issued
column + ExtNav picker dark theme — Stage 3 Phase γ polish).
**Scope:** minor — new workflow-rail behavior (auto-cascade);
backend-only.

## Headline

**A Work Order now auto-flips to `Completed` the moment its FINAL
routing operation receives a passing QA decision.** The operator no
longer has to remember to click the WO `Complete` button — the code
does the thinking per [[trust-code-not-operator]].

Before PR-237 (S233 / PR-229 deferred this per ADR-0063 §"Open
questions" #9): a 3-op WO where the operator passes op#1, op#2, op#3
would sit forever in `InProgress`. The QA gate was satisfied, but the
state transition was operator-driven. The Dispatch board's "Eligible"
filter (S234 / ADR-0064) keys on `WorkOrderState::Completed`, so the
WO never appeared as dispatch-ready until someone manually clicked.

After PR-237: the same sequence flips the WO to `Completed`
automatically. The Dispatch board picks it up at the next refresh.
One state-machine pass, no dropped throughput.

## What changed

1. **`crates/aberp-work-orders/src/repository.rs`** — new
   `try_auto_complete_wo(tx, ctx, wo_id, idem_seed)` helper. Reads the
   WO state inside the supplied tx; returns `Ok(None)` when the WO is
   already terminal, never released, or the QA gate
   (`all_live_inspections_passed_for_wo`) is not yet satisfied. When
   eligible it calls the existing `transition_work_order(Complete)`
   handler — single source of truth, no SQL duplication. The Complete
   side effects (one `WoCompletion` `stock_movement` for the finished
   good + one `WorkOrderStateChanged` audit entry) ride the same
   commit as the QA decide that triggered them.

2. **`apps/aberp/src/serve.rs`** — `decide_qa_inspection_request` now
   conditionally invokes `try_auto_complete_wo` after a successful
   QA decide that lands in `QaState::Passed`. Surfaced via the new
   `DecideQaInspectionResponse.wo_auto_completed: Option<String>`
   field — `Some(wo_id)` when this Pass auto-completed the WO,
   `None` for partial passes / Fail / Rework / Dispose.

3. **`crates/aberp-work-orders/src/lib.rs`** — export
   `try_auto_complete_wo`.

4. **New test file:**
   `crates/aberp-work-orders/tests/wo_auto_complete.rs` — eight
   integration tests pin the brief invariants:
   - pass every op → WO auto-flips to Completed
   - fail any op → WO stays InProgress
   - out-of-order QA pass: only fires on the LAST pass regardless of
     which op was decided last
   - idempotency: a repeat hook on an already-Completed WO is a
     no-op (the `InProgress` pre-check short-circuits)
   - no regress: Pass→Fail on a previously-passed inspection AFTER
     auto-complete does NOT walk the WO back to InProgress
   - cross-actor supersede on the FINAL inspection (adapter Fail →
     operator Rework → re-Complete → fresh Pass) still triggers
     auto-complete on the re-pass
   - the auto-complete path emits exactly one `WoCompletion`
     stock_movement + one WorkOrderStateChanged → completed audit
   - the routing-op state cascade still terminates with all ops
     Completed once auto-complete has fired

## Why option B-Route, not B-Inline

The cleanest hook would have been inside `aberp_qa::decide_qa` —
same tx, no orchestration cost at the route layer. But aberp-qa
**must not** depend on aberp-work-orders (the cycle is called out
explicitly in
`crates/aberp-qa/Cargo.toml`: aberp-work-orders depends on aberp-qa
for the auto-create-on-op-completion path). The alternative —
duplicating `transition_work_order(Complete)`'s WoCompletion + audit
SQL inside aberp-qa — would have built a second WO-complete pathway
that needs to stay in sync with the first. Bad smell per CLAUDE.md
rule 13 (delete before optimize).

Putting the hook in the route handler (`decide_qa_inspection_request`)
threads the same tx into BOTH `aberp_qa::decide_qa` AND
`aberp_work_orders::try_auto_complete_wo`, so the QA Passed row, the
WO Completed flip, the WoCompletion stock_movement and the
WorkOrderStateChanged audit all ride one atomic commit. The helper
itself lives in aberp-work-orders (which already depends on aberp-qa
to read `all_live_inspections_passed_for_wo`).

## What did NOT change

- **No schema changes.** No new audit `EventKind`. No new column.
  `WorkOrderStateChanged` already carries the `to_state: completed`
  payload via `transition_work_order`; reading the audit ledger to
  reconstruct who triggered the auto-complete is a payload-side
  question (the `actor` field carries the operator who decided the
  QA Pass that triggered it).
- **No SPA changes.** The new
  `DecideQaInspectionResponse.wo_auto_completed` field is forward-
  compatible — TypeScript clients ignore unknown response fields. A
  follow-up SPA PR can surface a "WO auto-completed" toast off this
  field if operator feedback wants it; today the SPA refreshes the WO
  detail after a QA decide and reads the canonical state from
  `read_work_order`.
- **No regression downgrade path.** Per the brief explicitly: if a
  previously-passed last op is reopened (operator decides Fail on a
  Passed inspection after auto-complete already fired), the WO does
  NOT walk back from Completed to InProgress. That's an explicit
  operator concern (re-issue / Cancel + re-Release if needed), not
  an auto-cascade.

## Rollback

Trivial — PROD_v2.8.5 is the prior release branch and is untouched.
A regression in the auto-complete path is surfaced by:

- WO sits at `InProgress` after all QA passes → bug in the gate or
  the hook condition; check the audit ledger for the missing
  `WorkOrderStateChanged → completed` entry and the
  `mes.qa_inspection_decided → passed` chain.
- WO over-eagerly flips to `Completed` when an op is still pending →
  bug in `all_live_inspections_passed_for_wo`; verify the routing-op
  count equals the live-Passed-inspection count for the WO in
  question.

Operator can switch to PROD_v2.8.5 via
`./run/upgrade_prod.sh PROD_v2.8.5` per the standard runbook —
no schema migration to unwind.
