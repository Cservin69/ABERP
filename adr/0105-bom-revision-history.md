# ADR-0105 — BOM revision history and the work-order revision pin

- **Status:** **Accepted** (implemented this session).
- **Date:** 2026-07-26
- **Deciders:** Ervin Áben (feature owner). Implementation-pass by Claude.
- **Related:** ADR-0062 (Work Orders v1 — the `boms` table this extends; §6 soft-retire, §5 Release consumption), ADR-0061 (stock movements / negative-stock policy), ADR-0081 (`EventKind` count pins), ADR-0090 (NCR/CAPA — the audited-business-row pattern), ADR-0099 (shared `Handle` / opener census), ADR-0104 + S429 (content-addressed reproducibility pins).

---

## 0. TL;DR

`boms` already **retains** superseded lines (`retired_at`, never `DELETE`d, ADR-0062 §6). What it does not give them is **identity**: no revision number, no author, no reason, and the only grouping key is `created_at` — which collides when two saves land in the same RFC3339 second. Nothing records **which BOM a work order was built against**; the Release handler reads whatever is active at that moment and leaves no trace of it. Retained-but-unidentifiable is not traceability.

This ADR adds:

1. **A revision store** (`bom_revisions`): one header row per authored revision — `rev_number` (monotonic per product), `created_at`, `author`, `reason`, `line_count`, `content_hash`. Each `boms` line carries its `bom_rev_id`.
2. **Attribution on the hash-chained ledger**: a new `mes.bom_revision_created` `EventKind` carrying the **full line snapshot**, appended in the SAME transaction as the business rows.
3. **The traceability pin** (`work_orders.bom_rev_id`): stamped once, at Release, from the revision the consumed lines belonged to. A later BOM edit cannot retro-change what a released batch was built to.
4. **A diff** (`diff_bom_revisions`): added / removed / re-quantified between any two revisions, component-keyed, computed server-side.

---

## 1. Current state (grep-verified before designing)

| Layer | Today | File:line |
|---|---|---|
| BOM storage | `boms (bom_line_id, tenant_id, product_id, component_id, qty_per_unit, created_at, retired_at)`, 1-level, flat | `crates/aberp-work-orders/migrations/V001__work_orders.sql:69` |
| Authoring | `replace_bom_for_product` — soft-retire prior active rows, INSERT the new set. **No audit kind**, no actor parameter, no reason | `crates/aberp-work-orders/src/repository.rs:139` |
| Active read | `list_active_bom_for_product` — `WHERE retired_at IS NULL` | `repository.rs:89` |
| Release consumption | reads the active set, emits one `BomConsumption` movement per line. **Does not record which set** | `repository.rs:608` |
| Route surface | `GET`/`POST /api/products/:id/bom`; the POST discarded `require_ready`'s operator login | `apps/aberp/src/serve.rs:4112` |
| SPA | `ProductDetail.svelte` "Receptúra" tab — read table + full-replace editor | `apps/aberp-ui/ui/src/routes/ProductDetail.svelte` |
| Precedent: audited business write | business INSERTs + `append_in_tx` in ONE tx (`create_ncr`, `record_movement`, `create_work_order`) | CLAUDE.md rule 15 |
| Precedent: reproducibility pin | FNV-1a over a canonical sorted set, stamped on the consuming row (`coefficient_set_hash`, `quote_price_snapshots`) | `crates/aberp-quote-engine/src/calibration.rs:121` |

**The gap:** revision *identity* and the WO→BOM *link*. Retention already existed and is untouched.

---

## 2. Design decisions (each flagged; the conservative option taken)

### 2.1 Immutability model → **copy-on-write full snapshots**, not a delta/event log ⚑

**Decision:** each revision is the complete set of lines, retained in `boms` and grouped by `bom_rev_id`.

**Why (conservative):** the write path is *already* copy-on-write — `replace_bom_for_product` retires the whole active set and inserts a whole new one, and the retained rows are already on disk. Adding a header row and a foreign key **labels what is physically there**; it changes no storage semantics and needs no backfill of behaviour.

**Flagged alternative — a delta/event log** (store `+bolt`, `bar: 2→3`, replay to reconstruct). Rejected: reconstructing business state by replaying events is a pattern this codebase uses **nowhere**. The audit ledger is an integrity chain, not a state-reconstruction source; every subsystem answers "what is it now / what was it then" with a SQL read. Introducing replay for BOMs alone would mean two ways to answer the same question, which CLAUDE.md rule 7 forbids. Storage cost is not a real constraint here: BOMs are bounded at 200 lines and edited rarely.

### 2.2 Where revisions live → **their own table**, with the ledger as the attestation ⚑

**Decision:** both, with distinct jobs. `bom_revisions` is the **queryable** store (the SPA history panel, the WO pin resolution). The `mes.bom_revision_created` ledger entry is the **tamper-evident attestation**.

**Why (conservative):** this is exactly the split every other audited subsystem uses (NCR, inventory, work orders). Riding the ledger *alone* was considered and rejected — answering "list the revisions of product X" would become a full-chain scan with JSON extraction, and the ledger is append-only by design, so it is a poor primary index. Using the table *alone* was also rejected: a table row can be rewritten by anyone with DB access, and once a WO pins a revision, that revision is part of the regulated traceability record.

**Consequence — this reverses ADR-0062 §6's call** that "BOM is reference data, not regulated state; no audit kind in v1". That call was right when nothing referenced a BOM version. It stops being right the moment a work order pins one. Stated here rather than left as a silent contradiction (rule 7).

**Payload carries the FULL line set**, not a pointer, so the chain can attest what the BOM *was* independently of the mutable tables. Bounded by the existing `MAX_BOM_LINES_PER_REQUEST` (200). Same posture as `WorkOrderCreatedPayload`.

### 2.3 Revision identity → **`bmr_<ULID>` + a monotonic per-product `rev_number`** ⚑

**Decision:** the stable key is `bom_rev_id = bmr_<ULID>`; the operator-facing name is `rev_number`, 1-based and monotonic per `(tenant_id, product_id)`, allocated by an in-tx `MAX(rev_number)+1` probe.

**Why (conservative):** identical to the `work_orders.wo_number` allocator posture (V001) — the in-tx probe is the authoritative gate, no DB-level `UNIQUE` per the `[[no-sql-specific]]` posture. Operators say "revision 3", not a ULID; machines need a stable opaque key. Both, as everywhere else in this codebase.

Attribution is `author` (the `ActorKind::as_operator_string()` form every other WO audit payload uses), `created_at`, and an optional free-text `reason`. **The POST route now requires the operator login** it previously discarded — a revision that cannot name its author is not a revision.

**Flagged:** `reason` is **optional**, not mandatory. Forcing a note makes operators type "." — a mandatory field that collects noise is worse than an honest empty one. Blank/whitespace normalises to `NULL` so "no reason given" is distinguishable from "reason was a space".

### 2.4 Pre-ADR-0105 rows → **not backfilled**; the Release path warns loud ⚑

**Decision:** existing `boms` rows keep `bom_rev_id IS NULL`. No migration invents revisions for them. A Release against such a BOM **succeeds, pins nothing, and returns a warning** telling the operator to re-save the BOM to start its history.

**Why (conservative):** those rows have no recorded author and no recorded reason. Minting a synthetic "revision 0" would **fabricate attribution the system never had** — precisely the class of lie the trust-code-not-operator and audit-integrity posture exists to prevent. A `NULL` pin is honest; a fabricated one is worse than none.

**Flagged alternative — refuse the Release outright** until the BOM is re-saved. Rejected as too aggressive: it would break every existing tenant's production flow on upgrade for a bookkeeping gap the operator can close with one save. The warning rides the existing `WorkOrderTransitionOutcome::warnings` channel the negative-stock policy already uses, so it surfaces on the SPA without a new surface.

A **mixed** active set (some lines revisioned, some not, or two distinct revisions live at once) is a torn write and **hard-refuses** — a wrong pin is worse than no pin (rule 11).

### 2.5 The traceability link → **`work_orders.bom_rev_id`, stamped at Release** ⚑ — **WIRED, not deferred**

**Decision:** the WO row carries the revision it was released against. `COALESCE(bom_rev_id, ?)` in the transition UPDATE, so it is stamped **once** and no later transition (Start / Hold / Resume / Complete / Cancel) can overwrite it.

**Why Release and not Create:** Release is where the BOM is actually consumed (ADR-0062 §5 emits the `BomConsumption` movements there). A WO created against revision 2 but released a week later after an ECO is genuinely built to revision 3 — pinning at create time would record a fiction.

**Why `NULL` is unambiguous:** `bom_rev_id IS NULL AND released_at IS NULL` = not yet built. `bom_rev_id IS NULL AND released_at IS NOT NULL` = released against a legacy unrevisioned BOM (and warned at the time).

**Flagged as out of scope:** pinning at the **shipped-part** level (`part_marks` / dispatch). Parts already trace to their work order, and the WO now traces to a BOM revision, so the chain is complete without a second pin; adding one would duplicate the fact in two places. Recorded in §5.

### 2.6 Diff → **component-keyed, server-side, refuses on ambiguity** ⚑

**Decision:** `diff_bom_revisions(from_lines, to_lines)` is a pure function returning `{added, removed, changed}`, keyed on `component_id`, each vector sorted deterministically. Exposed at `GET /api/products/:id/bom/diff/:from_rev/:to_rev`.

A component-keyed diff is only well defined if a component appears at most once per BOM — nothing enforced that before. So:

- **New author-time gate:** a duplicate `component_id` within one BOM is **refused loud**. Two lines for the same component are one line with the summed quantity; silently collapsing them would under-report every subsequent diff. This is a behaviour change on `replace_bom_for_product`, flagged here. It cannot break existing data (it gates writes only), and any *existing* duplicate-bearing revision still reads back intact.
- **Diff-time refusal:** if either side contains a duplicate (as a legacy revision might), the diff returns an error rather than silently keeping the last-seen line.

Server-side so the SPA cannot drift from what the traceability record says changed.

### 2.7 Identical consecutive saves → **still mint a revision** ⚑

**Decision:** saving a BOM whose content hash matches the current revision mints revision N+1 anyway.

**Why (conservative):** an operator save is a real event. Suppressing it would be hidden behaviour — the operator presses Save, something appears to happen, and no record exists. The `content_hash` makes the no-op visible (equal hashes across two revisions) and the diff between them is empty and self-describing. Simplicity over cleverness (rule 2).

`content_hash` is FNV-1a over the canonical `component_id=qty;` set **sorted by component id**, so line order does not affect it — identical construction to `CalibrationTable::set_hash` (S429) and the ADR-0104 price-set pin.

### 2.8 Schema shape → additive only; **no `superseded_at`** ⚑

`V003__bom_revisions.sql` is `CREATE TABLE IF NOT EXISTS` + two `ADD COLUMN IF NOT EXISTS`, same idempotent forward-only posture as V001/V002. No CHECK constraints (`[[no-sql-specific]]`).

`retired_at` on `boms` is **kept**, not replaced by a join against the revision headers: legacy rows have no revision, so a revision-join read would make them invisible — silent data loss on existing tenants. Both are written by the same statement pair inside one transaction, so they cannot diverge.

Conversely, `bom_revisions` deliberately has **no `superseded_at`** column: the current revision is `MAX(rev_number)`. A second record of the same fact is a second thing that can go stale (rule 12).

### 2.9 DB access → the **shared `Handle`**; the census does not move ⚑

Every new read and the write ride `state.db.read()` / `state.db.write()` (ADR-0099 H3 STEP 4c — `work_orders` is a Handle family). **No new `Connection::open` anywhere**, so the ADR-0099 opener census is unchanged by this work. The revision header INSERT, the line INSERTs, the prior-set retire, and the ledger append are all on the caller's single `WriteGuard` transaction (rules 13, 14, 15).

---

## 3. What this ADR does NOT do

- **No nested / multi-level BOMs.** ADR-0062's flat 1-level model is unchanged; revisioning is orthogonal to it.
- **No revision approval workflow** (draft → approved → released). Every save is immediately the active revision, as before. A quality-gated BOM release is a separate decision with its own state machine.
- **No revert-to-revision button.** An operator re-authors the lines; that mints a new revision recording who reverted and why, which is the honest record. A "revert" that silently re-activates old rows would break `rev_number` monotonicity.
- **No cross-product BOM diff.** Both sides of a diff are gated to the product in the path.

---

## 4. Invariants

1. **Retention** — a superseded revision's lines are never `UPDATE`d or `DELETE`d; only `retired_at` is stamped. Revision N reads back exactly as authored, forever.
2. **Attribution** — every revision has an author and a timestamp. A BOM write with no operator login is refused at the route.
3. **Atomicity** — the `bom_revisions` header, the `boms` lines, and the `mes.bom_revision_created` ledger entry commit together or not at all.
4. **Monotonicity** — `rev_number` is strictly increasing per `(tenant, product)`, allocated inside the write transaction.
5. **Pin immutability** — `work_orders.bom_rev_id` is written once, at Release, and never overwritten.
6. **Pin honesty** — the pin is either the revision actually consumed, or `NULL` with a warning. It is never a guess.
7. **Diff well-definedness** — a component appears at most once per authored BOM; an ambiguous diff refuses rather than under-reports.

---

## 5. Deferrals

| # | Item | What closes it |
|---|---|---|
| D1 | **Shipped-part-level pin.** Parts trace to a WO, and the WO now traces to a revision, so the chain is complete; a direct `part_marks.bom_rev_id` would duplicate the fact. | Only if a part is ever built outside a work order. |
| D2 | **Legacy BOM adoption is operator-driven.** Existing tenants keep unrevisioned BOMs until someone re-saves each product's recipe; releases warn until then. No bulk adoption tool. | A one-shot operator CLI, if the warning volume justifies it. |
| D3 | **`validate_qa_or_routing_op_id` is now also used for `bmr_` ids** in `apps/aberp-ui/src/commands.rs`; the name is narrower than the use. Reusing it beat a fourth byte-identical validator. | A rename sweep, out of scope for this PR (rule 3). |
| D4 | **No revision approval state.** See §3. | A separate ADR if the quality system requires BOM sign-off. |

---

## 6. Proof

`crates/aberp-work-orders/tests/bom_revision_history.rs` — 11 tests:

- three edits → three retained, numbered, attributable revisions; revision 1 still holds its **original** quantities after two supersessions; all five authored lines retained.
- content hash is order-insensitive and quantity-sensitive; re-saving an identical set still mints a revision.
- duplicate component refused at author time and at diff time.
- diff reports added / removed / changed correctly, is directional, and self-diffs to empty.
- **a released WO pins its revision; a later BOM edit does not move the pin; resolving the pin re-derives the exact as-built BOM; a WO released after the edit pins the new revision.**
- Start / Hold / Resume leave the pin untouched; an unreleased WO has none.
- a legacy unrevisioned BOM releases with a loud warning and no pin.
- one ledger entry per revision, carrying the full snapshot and naming its predecessor; a refused author leaves no entry.

Mutation-verified: reverting each of the pin stamp, the pin's `COALESCE` immutability, the duplicate gate, and the legacy warning individually reds its test.
