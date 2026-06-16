# ADR-0084 — Approved Vendor List (AVL) firing sites: CRUD UI, screening, PO-gate.

- **Status:** Accepted
- **Date:** 2026-06-16
- **Deciders:** Ervin (via S431 brief — first defense firing-site session, auto-mode).
- **Implements:** the firing sites the defense foundation (ADR-0070–0080) shipped without. Discharges S425 backlog item #7: "AVL CRUD UI missing; `supplier.export_screened` exists but never fires."
- **Related:** ADR-0078 (`supplier.*` AVL family — this is its first firing site), ADR-0068 (PO work, still Proposed — the AVL gate refuses at point-of-use ahead of it), ADR-0081 (`aberp-verify` NAV-leakage coverage gate — the 5 new EventKinds re-review it), `[[trust-code-not-operator]]`, `[[hulye-biztos]]`, `[[no-sql-specific]]`.

## Context

The aerospace/defense pivot (S344–S371) shipped the AVL *foundation*: the `aberp_compliance::avl` enums, four nullable AVL columns on `partners`, and the two `supplier.*` EventKinds (`dpas_priority_set`, `export_screened`) — all kind-only, **no firing site**. An operator had no way to add a vendor, screen one, or have a suspended vendor's PO refused. The compliance surface existed on paper but did nothing.

Three facts from the codebase shaped the design (verified, not assumed):

1. **The S345 `ApprovedSupplierEntry` / `QualLevel` scaffold was never persisted or wired** — a leaf struct with a bid/deliver vocab that does not match the brief's five-state approval lifecycle. Reusing it would have meant blending two vocabularies (CLAUDE.md rule 7).
2. **`supplier.export_screened` has a never-fired doc-comment payload** (`partner_id` + `clear/hit/inconclusive`). The brief pins a different, AVL-screening-action vocab for the firing.
3. **No PO surface exists** (ADR-0068 is Proposed). The refuse-PO gate therefore has to be a reusable point-of-use function exercised through a thin endpoint today, callable by a future PO-create path.

## Decision

**Ship a new persisted `avl_vendors` master-data table + CRUD/screen/status/PO-gate endpoints + a dark-theme SPA page, firing five new `supplier.*` EventKinds plus the existing `supplier.export_screened`.** Lives in one new backend module (`apps/aberp/src/avl_vendors.rs`), three new compliance enums (`aberp_compliance::avl`), and a self-contained SPA page.

### New types (in `aberp_compliance::avl`, round-trip-proven like the existing enums)

- `ApprovedStatus` — Pending / Approved / Conditional / Suspended / Revoked. `Revoked` is the terminal archive state (archive-not-delete); `Suspended`/`Revoked` are the two `blocks_po()` states. The transition invariant is in code (`can_transition_to`): every non-revoked source may move anywhere; `Revoked` is terminal except the no-op, so `Revoked → Approved` is refused unless an explicit manual override (`force`) is passed.
- `ApprovalCategory` — General / ITAR / EAR99 / Aerospace / Defense / Nuclear, multi-select, stored comma-joined.
- `AvlScreeningResult` — Pass / Conditional / Fail / SkippedNoIntegration (the mock-screening default). **Deliberately distinct** from `ExportScreeningStatus` and `export_control::ScreeningResult` (rule 7 — the brief's screen-action vocab is its own).

### New EventKinds (5; count 130 → 135), all in the `supplier.*` prefix family

`AvlVendorAdded`, `AvlVendorStatusChanged`, `AvlVendorRevoked`, `AvlScreeningOverdue`, `PoBlockedByVendorStatus` — `supplier.avl_vendor_added` / `…status_changed` / `…revoked` / `…screening_overdue` / `…po_blocked_by_vendor_status`. Keeping all five in `supplier.*` keeps the whole AVL surface globbable as one prefix and avoids opening a near-empty `po.*` family for a PO surface that has not shipped. App-layer JSON payloads only, never NAV XML — added to both NAV-leakage exhaustive arms + both `const _ == 135` pins + the runtime no-NAV family tests (ADR-0081).

### `supplier.export_screened` divergence (surfaced, not blended)

The brief's "Screen vendor" action fires the existing kind with `{vendor_id, partner_id, categories_screened, screening_result (pass/conditional/fail/skipped_no_integration), reviewer_login, decision_time_utc}` — NOT the S361 doc-comment shape. The payload is untyped at the ledger, so the kind is reused; the canonical fired shape lives in `SupplierExportScreenedPayload`. The screening itself is a no-op mock (no OFAC/SDN/Export-Denied integration yet), but the audit wiring is real.

### The PO gate ([[trust-code-not-operator]])

`po_eligibility(conn, tenant, partner_id)` returns `NoEntry` / `Eligible` / `Blocked`. A `Suspended`/`Revoked` vendor blocks: the `/api/avl-po-check` endpoint returns 409 with an operator-facing bilingual message and fires `PoBlockedByVendorStatus`. The function is `pub` so a future PO-create path calls it before writing. An unlisted partner is `NoEntry` (this gate blocks the two refused statuses; it does not mandate AVL membership).

### Boot-time re-screening reminder

`fire_overdue_screening_reminders` scans every non-revoked vendor whose `approved_until_utc` has lapsed and fires `AvlScreeningOverdue` once per overdue vendor at serve boot — non-fatal ([[hulye-biztos]]; a reminder scan never blocks boot).

## Consequences

- **Surgical:** the unwired S345 `ApprovedSupplierEntry`/`QualLevel` scaffold and the four S361 partner AVL columns are left untouched — `avl_vendors` is its own table.
- **Testable today:** the refuse-PO gate is exercised via `/api/avl-po-check` and the library function even though ADR-0068's PO surface is Proposed. Full e2e: vendor-add → screen → approved-until-expires → boot reminder → suspend → PO refusal.
- **No SQL CHECK/index** ([[no-sql-specific]]) — invariants in code; small master-data table scanned in full.
- The `supplier.export_screened` doc comment in `event_kind.rs` now reflects the actually-fired S431 payload.
