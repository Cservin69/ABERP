# `[[no-sql-specific]]` Drift Audit — 2026-06-14

**Scope:** every `*.sql` migration, every embedded DDL string in `*.rs`, the
billing/audit storage adapters, and all `CREATE`/`ALTER`/`INSERT` SQL across
`apps/aberp/src`, `crates/aberp-*`, `modules/*`.
**Principle under test (`[[no-sql-specific]]`):** *invariants live in the app
layer, NOT as CHECK / triggers / DB-specific features; a future engine swap
(DuckDB → Postgres / SQLite / Elasticsearch) must be painless.*
**Method:** case-sensitive grep sweep + direct read of every DDL-bearing file.
**Posture:** READ-ONLY forensics. No code touched. Commit lives on
`forensics-no-sql-audit`, unpushed.
**Triggering context:** S409 dropped two secondary indexes on
`outbound_email_queue` to dodge a DuckDB-ART bug — that was painless precisely
*because* those indexes were perf hints, not the source of correctness. This
audit asks: is the rest of the codebase that clean?

---

## TL;DR

The codebase has a **clear generational split**:

- **Newer modules** (`aberp-inventory`, `aberp-dispatch`, `aberp-work-orders`,
  `aberp-qa`, `email_relay_queue`) are **exemplary** — they cite
  `[[no-sql-specific]]` by name in their migration headers, keep closed-vocab
  state in Rust enums, declare **no** CHECK / FK / DB-level UNIQUE, and use
  indexes purely as perf hints. These could move to any engine tomorrow.
- **Older core** (`modules/billing`, `audit-ledger`, `products`, `partners`,
  `incoming_invoices`/`ap_invoice`, `restore_from_nav_outgoing`,
  `quoting_tunables`) still encodes invariants in the schema: **CHECK
  constraints, a `CREATE SEQUENCE` + `nextval()` identity column, and
  dedup-via-UNIQUE-violation.**

**The worst offenders are entirely absent** — no `CREATE TRIGGER`, no
`GENERATED ALWAYS AS`, no `SERIAL`/`IDENTITY`/`AUTO_INCREMENT`, no `RETURNING`,
no `MERGE`/`UPSERT`, no `FOREIGN KEY`, no `ON DELETE CASCADE` anywhere in
production DDL (ADR-0019 already banned FKs; relations are app-level ULID
pointers). The drift that exists is shallow and mechanical to remove.

One strong mitigating asset: **`modules/billing` already ships a second,
non-DuckDB adapter** (`in_memory_store.rs`) behind the `BillingStore` port —
the engine *is* abstracted at the most invariant-critical surface.

---

## Negative confirmations (the clean part)

Verified ABSENT across all `*.rs` + `*.sql` (excluding `/target`, `node_modules`):

| Pattern | Result |
|---|---|
| `CREATE TRIGGER` | 0 hits |
| `GENERATED ALWAYS AS (…)` computed columns | 0 hits |
| `SERIAL` / `IDENTITY` / `AUTO_INCREMENT` | 0 hits |
| `INSERT … RETURNING` | 0 hits |
| `MERGE` / `UPSERT` / `INSERT OR REPLACE/IGNORE` | 0 hits |
| `FOREIGN KEY` / `REFERENCES` in production DDL | 0 hits (ADR-0019) |
| `ON DELETE/UPDATE CASCADE` | 0 hits |
| `DEFAULT NOW()` / `CURRENT_TIMESTAMP` / `uuid_generate` / `gen_random` | 0 hits (all timestamps are app-supplied RFC3339 `VARCHAR`) |

This is the single most important finding: the categories with the largest
blast radius on an engine swap (server-side defaults, cascading RI, triggers,
generated columns) **do not exist**. The audit is about a handful of CHECK
constraints and one sequence, not a structural rewrite.

---

## 🔴 DRIFT findings

### Category 1 — `CHECK` constraints (the dominant drift)

The principle names CHECK constraints explicitly. They appear in the older core
and are functionally redundant: every constrained value is already a Rust enum
or validated in the app before insert. They also actively cause DuckDB-specific
*pain today* — multiple migration comments note `ALTER COLUMN TYPE` is forbidden
on a CHECK-constrained column, forcing add/backfill/drop/rename ladders.

| file:line | snippet | tag | impact on engine swap | remediation |
|---|---|---|---|---|
| `modules/billing/src/adapters/duckdb_store.rs:51` | `reset_policy VARCHAR NOT NULL CHECK (reset_policy IN ('never','annual_on_fiscal_year'))` | 🔴 | ES has no CHECK; PG/SQLite need identical syntax | drop CHECK — `ResetPolicy` enum already gates it |
| `modules/billing/src/adapters/duckdb_store.rs:59` | `next_number BIGINT NOT NULL CHECK (next_number >= 1)` | 🔴 | range invariant lives in DB | drop — allocator already enforces `>= start_value` floor (S394) |
| `modules/billing/src/adapters/duckdb_store.rs:70` | `status VARCHAR NOT NULL CHECK (status IN ('reserved','used','voided'))` | 🔴 | enum-in-DB | drop — `ReservationStatus` enum is authoritative |
| `modules/billing/src/adapters/duckdb_store.rs:173` | `quantity DECIMAL(18,6) NOT NULL CHECK (quantity >= 0)` | 🔴 | comment at :171 confirms this CHECK *blocked* the S157 `ALTER COLUMN TYPE`, forcing a rename ladder | drop — line-item validation lives in `LineItem` construction |
| `crates/audit-ledger/src/storage/schema.rs:45` | `seq BIGINT NOT NULL CHECK (seq >= 1)` | 🔴 | the module doc (S341) brags it dropped UNIQUE for portability but **kept** this CHECK | drop — `append()` computes `seq` monotonically; hash chain detects gaps |
| `crates/audit-ledger/src/storage/schema.rs:48` | `time_mono BIGINT NOT NULL CHECK (time_mono >= 0)` | 🔴 | same | drop — monotonic clock is app-supplied |
| `apps/aberp/src/products.rs:240` | `unit_kind VARCHAR NOT NULL CHECK (unit_kind IN ('Nav','Own'))` | 🔴 | enum-in-DB | drop — closed vocab in app |
| `apps/aberp/src/products.rs:242` | `currency VARCHAR NOT NULL CHECK (currency IN ('HUF','EUR'))` | 🔴 | enum-in-DB | drop — `Currency` enum |
| `apps/aberp/src/partners.rs:529` | `kind VARCHAR NOT NULL CHECK (kind IN ('Customer','Supplier','Both'))` | 🔴 | enum-in-DB | drop — `PartnerKind` enum |
| `apps/aberp/src/incoming_invoices.rs:391` | `currency VARCHAR NOT NULL CHECK (currency IN ('HUF','EUR'))` | 🔴 | enum-in-DB | drop |
| `apps/aberp/src/incoming_invoices.rs:392` | `local_status VARCHAR NOT NULL CHECK (local_status IN ('Outstanding','Paid','Irrelevant'))` | 🔴 | enum-in-DB | drop — status enum |
| `apps/aberp/src/restore_from_nav_outgoing.rs:199` | `currency VARCHAR NOT NULL CHECK (currency IN ('HUF','EUR'))` | 🔴 | enum-in-DB | drop |
| `apps/aberp/src/restore_from_nav_outgoing.rs:3307` | same CHECK, embedded in a test seed | 🔴 | test DDL replicates the drift | drop alongside production |

**Inconsistency worth flagging:** the audit-ledger schema (`schema.rs:6-25`)
contains a long, proud justification for *dropping* `UNIQUE(seq)`/`UNIQUE(id)`
for portability + a DuckDB bug — yet leaves `CHECK (seq >= 1)` two lines down.
Same class of invariant, opposite decision. The principle was applied to UNIQUE
but not carried through to CHECK in the very file that documents it best.

### Category 2 — `CREATE SEQUENCE` + `DEFAULT nextval()` (engine-specific identity)

| file:line | snippet | tag | impact | remediation |
|---|---|---|---|---|
| `apps/aberp/src/quoting_tunables.rs:381` | `CREATE SEQUENCE IF NOT EXISTS quoting_complexity_rules_id_seq;` | 🔴 | DuckDB/Postgres-only object; SQLite & ES have no `CREATE SEQUENCE` | move ID minting to the app (ULID, like every other table) |
| `apps/aberp/src/quoting_tunables.rs:383` | `id BIGINT NOT NULL PRIMARY KEY DEFAULT nextval('quoting_complexity_rules_id_seq')` | 🔴 | server-side ID generation; relies on the sequence object | app supplies the PK; drop the `DEFAULT nextval` |
| `apps/aberp/src/quoting_tunables.rs:427` | `CREATE SEQUENCE IF NOT EXISTS quoting_stock_adjustments_id_seq;` | 🔴 | same | same |
| `apps/aberp/src/quoting_tunables.rs:429` | `id BIGINT NOT NULL PRIMARY KEY DEFAULT nextval('quoting_stock_adjustments_id_seq')` | 🔴 | same | same |

This is the **only** place in the codebase that delegates identity generation to
the engine. Everywhere else PKs are app-minted ULIDs (`inv_…`, `mvt_…`,
`dsp_…`, `wo_…`). These two tables are the odd ones out — and the principle's
"painless engine swap" fails hardest here, because `nextval` has no portable
equivalent.

### Category 3 — DB-as-source-of-uniqueness (UNIQUE the app *depends* on)

The principle allows declared UNIQUE/PK as portable *if the app also enforces
it*. The codebase has both kinds. The 🔴 case is `ap_invoice`, where dedup is
implemented by **catching the DuckDB UNIQUE-violation error** rather than a
pre-insert probe:

| file:line | snippet | tag | impact | remediation |
|---|---|---|---|---|
| `apps/aberp/src/incoming_invoices.rs:397` | `UNIQUE (tenant_id, supplier_tax_number, nav_invoice_number)` | 🔴 | dedup logic at `:577-595` keys off the INSERT raising a UNIQUE violation; on an engine that doesn't fire one (ES, or DuckDB cross-`Connection` — see the `:1722` "QUIRK" test which already documents UNIQUE not firing cross-connection) the dedup **silently breaks** = duplicate AP invoices | pre-insert `find_existing_id()` probe inside the tx as the authoritative gate; keep UNIQUE only as a backstop |
| `modules/billing/src/adapters/duckdb_store.rs:85` | `idempotency_key VARCHAR NOT NULL UNIQUE` | 🟡 | idempotency on re-submit; the allocator path reads-then-inserts in one tx so it's *belt-and-suspenders*, but verify the read-side gate exists for every caller | confirm app-layer idempotency check precedes insert at all call sites; demote UNIQUE to backstop |
| `modules/billing/src/adapters/duckdb_store.rs:50,69,75,160` | `code … UNIQUE`, `invoice_id … UNIQUE`, `UNIQUE (series_id, fiscal_year, number/sequence_number)` | 🟡 | the allocator holds a single-writer tx, so uniqueness is app-serialized; but the *declaration* still isn't portable to ES | keep PK, drop secondary UNIQUE declarations once app-gate confirmed (mirror the S341 audit-ledger move) |
| `apps/aberp/src/invoice_draft.rs:95` | `UNIQUE (tenant_id, source_dispatch_id)` | 🟡 | one-draft-per-dispatch; relies on DB | confirm app probe inside `create` tx |
| `apps/aberp/src/restore_from_nav_outgoing.rs:202` | `UNIQUE (tenant_id, source_nav_invoice_number)` | 🟡 | restore-once guard | the `ON CONFLICT DO NOTHING` at `:323` already pairs with it; make the app probe authoritative |

**Precedent already set:** `audit-ledger` (S341) and `email_relay_queue` (S409)
both deliberately removed DB-side uniqueness/index reliance and proved integrity
survives on the app layer. The billing UNIQUE declarations are the same pattern
that hasn't been migrated yet.

### Category 4 — DuckDB-specific storage/admin operations

| file:line | snippet | tag | impact | remediation |
|---|---|---|---|---|
| `apps/aberp/src/snapshot.rs:132` | `conn.execute_batch("CHECKPOINT;")` | 🔴 | `CHECKPOINT` is DuckDB-specific WAL-fold | the whole snapshot tool is engine-coupled by design; gate behind the storage port if a swap ever happens |
| `apps/aberp/src/snapshot.rs:275` | `conn.prepare("PRAGMA verify_external_invariants")` | 🔴 | DuckDB-only integrity PRAGMA (the code already handles "unavailable" gracefully at :279-285) | abstract into a `StorageHealth` port method |
| `apps/aberp/src/snapshot.rs:492` | `PRAGMA disable_checkpoint_on_shutdown` | 🔴 | DuckDB-only | same |

These are the **deepest** engine coupling, but they live in an explicitly
operational subsystem (the `aberp snapshot` / `restore-snapshot` panic-button,
S393). A snapshot/restore tool is inherently tied to the on-disk file format; it
is *acceptable* coupling provided it's quarantined behind the storage port and
never referenced from business logic. Today it reaches `duckdb::Connection`
directly — that's the part to fix.

---

## 🟡 QUESTIONABLE findings

| file:line | snippet | tag | note |
|---|---|---|---|
| `apps/aberp/src/quote_pricing_jobs.rs:374,435` | `ON CONFLICT (quote_id) DO NOTHING` | 🟡 | idempotency guard for the S376 phantom-retry loop; portable to PG/SQLite, **not** ES; relies on PK |
| `apps/aberp/src/material_inventory.rs:505` | `ON CONFLICT (tenant_id, material_grade) DO NOTHING` | 🟡 | concurrent-saga race guard; same portability caveat |
| `apps/aberp/src/restore_from_nav_outgoing.rs:323` | `ON CONFLICT (tenant_id) DO NOTHING` | 🟡 | lock-acquire guard; same |
| `quoting_tunables.rs:390,402,414-419` + `partners.rs:539-540` + `material_inventory.rs:228-231` + `quoting_materials.rs:60-64` | `NOT NULL DEFAULT 0 / 1.0 / 'Domestic'` literal defaults | 🟡 | ANSI-portable, but means the app does **not** always supply the value — an engine with different DEFAULT semantics shifts data. Migration comments already document the DuckDB `ALTER ADD COLUMN … DEFAULT` replay trap (`partners.rs:596-599`). Prefer app-supplied values. |
| `*` (billing, lines, exchange_rate) | `DECIMAL(18,6)` / `DECIMAL(18,0)` | 🟡 | portable type, but precision/rounding semantics vary by engine; pin in app-layer money types (already done via `Huf`/`Decimal`) |
| `crates/aberp-dispatch/tests/dispatch_round_trip.rs:49,51` · `qa_round_trip.rs:38,40` · `work_order_round_trip.rs:37,39` · `repository_round_trip.rs:38,40` · `serve_qa_decide_session_id.rs:50` | test-seed `products`/`partners` DDL with `CHECK (unit_kind IN …)` / `CHECK (currency IN …)` | 🟡 | **test/production DDL divergence.** Production `partners` (`partners.rs:529`) has `CHECK (kind IN …)`; the *test* `partners` seed (`dispatch_round_trip.rs:59`) drops it. Meanwhile test `products` seeds add CHECKs. Per CLAUDE.md rule 9/11 the round-trip tests can't catch a CHECK regression because they don't mirror production schema. Unify on the production DDL (ideally the post-remediation, CHECK-free one). |

---

## 🟢 INTENDED / compliant (the principle honored)

These are correct under `[[no-sql-specific]]` and need **no** change:

- **Every `CREATE INDEX`** is a documented perf hint, never an authority:
  `email_relay_queue` (S409 dropped two with zero correctness loss),
  `stock_movements_tenant_product_at_idx`, `dispatches_tenant_*`,
  `work_orders_tenant_*`, `qa_inspections_tenant_*`. Each migration comment
  states the uniqueness/ordering gate lives in the app
  (e.g. `work_orders/migrations/V001:52-57`, `dispatch/migrations/V001:44-47`).
- **`PRIMARY KEY` everywhere** — app-minted ULIDs (`inv_…`, `mvt_…`, `dsp_…`,
  `wo_…`); portable, and idempotency relying on PK is allowed by the principle.
- **No foreign keys** (ADR-0019) — relations are app-level ULID pointers;
  cascading is done in app sagas, not the engine.
- **Closed vocabularies in Rust enums**, not DB CHECK, in all newer modules:
  `DispatchState`/`CarrierKind` (dispatch), `WorkOrderState`/`RoutingOpState`
  (work-orders), `next_qa_state` (qa), `MovementReason`/`MovementRefKind`
  (inventory), `QueueState` (email_relay_queue). Each migration cites
  `[[no-sql-specific]]` explicitly.
- **`audit-ledger` UNIQUE-drop (S341)** and **`email_relay_queue` index-drop
  (S409)** — live proof the app-layer-invariant posture works: integrity is
  carried by the hash chain + `AUDIT_APPEND_LOCK` and by app-layer state
  transitions respectively.
- **`modules/billing/src/adapters/in_memory_store.rs`** — a real, non-DuckDB
  implementation of the `BillingStore` port that "mirrors the DuckDB adapter's
  behaviour exactly." The most invariant-critical surface is *already* engine-
  abstracted; the DuckDB drift is confined to one adapter's DDL.

---

## Verdict

- **DDL surface in compliance:** ~**70%** by table count. Of ~28 DDL-bearing
  tables, the structural-portability blockers (CHECK / `nextval` / dedup-via-
  UNIQUE) touch ~9 tables, all in the older core. The remaining ~19 (all newer
  modules + audit-ledger body + email queue) are clean.
- **🔴 DRIFT items:** **24** (13 CHECK constraints, 4 sequence/nextval lines,
  4 DB-as-source UNIQUE, 3 DuckDB admin PRAGMA/CHECKPOINT).
- **🟡 QUESTIONABLE items:** **~12** (3 `ON CONFLICT`, literal `DEFAULT`s across
  4 files, `DECIMAL` precision, test/prod DDL divergence across 5 test files).
- **Worst-class offenders (triggers, generated columns, server defaults,
  cascading RI, RETURNING, MERGE):** **0**. The drift is shallow and mechanical.

### Top 5 highest-risk drifts (ranked by data-integrity blast radius)

1. **`incoming_invoices.rs:397` UNIQUE + dedup-via-violation (`:577-595`)** —
   the *only* drift that can cause silent data corruption. On an engine that
   doesn't raise a UNIQUE violation (ES; or DuckDB cross-`Connection`, already a
   documented quirk at `:1722`), duplicate AP invoices slip through. Highest
   blast radius despite being one table.
2. **`quoting_tunables.rs:381-383, 427-429` `CREATE SEQUENCE` + `nextval`** —
   no portable equivalent in SQLite/ES; an engine swap *cannot* run this DDL at
   all. Hard blocker, not a soft one.
3. **`snapshot.rs:132/275/492` CHECKPOINT + PRAGMA** — deepest engine coupling;
   the entire snapshot/restore subsystem assumes the DuckDB file format and
   reaches `duckdb::Connection` directly from app code.
4. **`duckdb_store.rs` UNIQUE bundle (`:50,69,75,85,160`)** — billing is the
   regulated heart; needs the same audit-ledger-style migration to app-gated
   uniqueness before any swap. Medium blast radius, high care required.
5. **13 CHECK constraints (billing, audit, products, partners, ap_invoice,
   restore)** — lowest individual risk (all redundant with Rust enums) but the
   widest footprint, and they actively block DuckDB `ALTER COLUMN TYPE` today.

### Recommended remediation sequence (3–5 steps)

1. **Strip CHECK constraints** from the 6 older-core schemas (billing, audit,
   products, partners, incoming_invoices, restore). Pure deletion — the enums
   already enforce every constrained value. Side benefit: unblocks future
   `ALTER COLUMN TYPE` migrations. Lowest risk, do first. *(Verify each enum
   gate exists per CLAUDE.md rule 8 before deleting.)*
2. **Replace `quoting_tunables` sequences with app-minted IDs** — drop the two
   `CREATE SEQUENCE` + `DEFAULT nextval`, mint the PK in Rust like every other
   table (ULID or app counter). Removes the only un-portable DDL object.
3. **Convert `ap_invoice` dedup to an authoritative pre-insert probe** inside
   the ingest tx (the `find_existing_id` already exists as the *fallback* — promote
   it to the *primary* gate). Demote UNIQUE to a backstop. Closes the silent-dup
   hole and the cross-connection quirk.
4. **Quarantine the snapshot subsystem behind the storage port** — move
   CHECKPOINT/PRAGMA behind a `StorageMaintenance`/`StorageHealth` trait so
   business code never touches `duckdb::Connection`; a non-DuckDB engine
   supplies its own impl (or a no-op).
5. **Unify test DDL with production** (rule 9/11) — point the round-trip test
   seeds at the post-remediation CHECK-free schemas so tests exercise the real
   shape and can fail when business logic changes. Demote the remaining billing
   UNIQUE declarations to backstops once app-gates are confirmed (mirror S341).

---

*Generated by a read-only forensics pass at `6ac3b30` (PROD_v2.27.62).
No code, migrations, or schemas were modified. Every finding cites file:line
for one-click verification.*
