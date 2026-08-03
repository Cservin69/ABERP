# ADR-0109 — Storage-engine-agnostic repository seam: the post-cutover blueprint

- **Status:** Proposed — **blueprint, not a decision request.** The sequencing is
  already decided (§0) and is not reopened here.
- **Date:** 2026-08-01
- **Deciders:** Ervin
- **Executes:** **ADR-0019 §1** (cornerstone, Accepted 2026-05-19) — which already
  decided this seam, in these words, and which is measurably unbuilt.
- **Executes at:** **step 5 of the decided sequence (§0)** — after the PROD
  cutover and after step 4's consequences pass. §2 of this document *is* the
  evidence base step 4 works from.
- **Related:** ADR-0003 (superseded by 0019; same Decision), ADR-0006 (module
  boundaries — the input to where the port lines go), ADR-0008 / ADR-0030 (audit
  ledger + fsync mirror), ADR-0059 / ADR-0100 (SaaS — Postgres-per-tenant, the
  named next engine), ADR-0107 (engine evaluation), ADR-0108 (the SQLite
  migration), CLAUDE.md rules 2, 3, 7, 11, 12, 13, 14, 15, `[[no-sql-specific]]`.

> ### ⛔ Nothing here is authorised now
>
> This document changes no runtime behaviour, adds no engine code, and touches no
> in-flight migration branch. **It must not be executed during steps 1–3.** A
> session that reads §3 and starts building `aberp-storage` before the PROD
> cutover has misread this ADR and is working against the decided order.
>
> Its two jobs today are: (a) capture the coupling evidence **while the migration
> is exposing it** — evidence is perishable, and this is the only moment it is
> being paid for in full; (b) hold the seam design so step 5 starts from a
> reviewed blueprint rather than a blank page.

> ### Cross-reference caveat
>
> ADR-0107 and ADR-0108 are **not on `main`** (`origin/main` = `3f062ac`). They
> live on `adr-db-engine-evaluation` and `adr0108/*` (PR #53, tip `349dc94`). Every
> `adr/0107-*` / `adr/0108-*` link below dangles until those land. Measurements are
> labelled with the ref they were taken on: **`main` @ `3f062ac`** (the pristine
> pre-migration tree) or **PR #53 @ `349dc94`** (the migration's current tip).
> Scope for both: `crates/ apps/ modules/`; "src" excludes `/tests/`.

---

## 0. The decided sequence — accepted context

Ervin decided the order on 2026-08-01. It is fixed, it is not re-argued here, and
this ADR is written to fit inside it:

| Step | | Owner |
|---:|---|---|
| **1** | Finish the SQLite migration on the safe track (ADR-0108 Steps 7–9) | in flight, PR #53 |
| **2** | DEV test | |
| **3** | **PROD cutover** | **Ervin's gate** |
| **4** | Deduce consequences | ← **§2 of this document is that ledger** |
| **5** | Redesign the abstraction | ← **§3–§5 of this document is that blueprint** |

**The one consequence of this order that must be stated rather than buried**
(rule 11): because the seam lands *after* the cutover, the remaining families
(quoting, inventory, work-orders, email, dispatch, qa) are hand-translated **once
more**, and the retrofit in §5 starts from zero rather than from a partial seam.
That is a real cost — §5 prices it at **10–15 sessions** instead of the 4–7 an
interleaved order would have cost.

It is also the right trade, and this ADR says so rather than sulking about it: the
cutover moves a legally-binding tax ledger with 8-year statutory retention
(ADR-0009) onto a new engine. Doing that on a plan that is already designed,
gated, mutation-pinned and reversible (ADR-0108 §5, §6) beats doing it on a plan
being redesigned underneath it. **Steps 1–3 are the risk; step 5 is a
behaviour-preserving refactor** (§5.3). Ordering the risky thing first, on a
finished plan, is correct.

---

## 1. The gap this blueprint closes

### 1.1 ADR-0019 §1 decided this seam and it was never built

Verbatim, from `adr/0019-storage-strategy-no-fks.md` (Accepted, cornerstone):

> Each module defines its own **storage port** as a Rust trait whose methods are in
> terms of *domain types*, not SQL. […] Each module ships: a **DuckDB adapter**
> […] an **in-memory adapter** (tests; same trait). Module code never imports
> DuckDB types. The string `duckdb` does not appear in the domain or app layers of
> any module. […] A shared `aberp-storage` crate provides: connection pool
> abstraction […] forward-only versioned migration runner, recorded in a
> `_aberp_migrations` table per tenant […] **a transaction handle type that modules
> use without naming the backend.**

Five deliverables. State on `main` @ `3f062ac`:

| ADR-0019 §1 deliverable | Built? | Evidence |
|---|---|---|
| Per-module storage **port trait** | **1 of ~21** | The only domain port tree-wide: `modules/billing/src/ports/storage.rs:177` `pub trait BillingStore` (8 methods). The only other `trait *Store` is `crates/aberp-secret-store/src/lib.rs:52` — secrets, not storage. |
| **In-memory adapter** per module | **1** | `modules/billing/src/adapters/in_memory_store.rs:39`. |
| *"`duckdb` never appears in the domain or app layers"* | **violated, 74 src files** | 74 non-test src files import `duckdb::` across 11 crates/apps, 49 in `apps/aberp` (121 files incl. tests). Including the app layer of the one module that *has* a port: `modules/billing/src/app/error.rs:14` — `Storage(#[from] duckdb::Error)`. |
| Shared **`aberp-storage`** crate | **absent** | no such crate in `crates/`. |
| Migration runner / `_aberp_migrations` | **absent** | tree-wide grep: **0**. |
| **Backend-agnostic transaction handle** | **absent** | 148 src fn signatures take `&(mut) Connection` (145) or `&(mut) Transaction` (3). |

The sharpest artefact in the tree is a pair two files apart, inside the exemplar
module:

```
modules/billing/src/ports/storage.rs:5   //! The SQL string `duckdb` does not appear in domain or app layers.
modules/billing/src/app/error.rs:14          Storage(#[from] duckdb::Error),
```

**Nothing in the tree can detect this.** There is no gate, no test, no census for
"engine type named outside an adapter" — while there *are* six cut gates and
~3 712 LOC of scanner machinery for the opener census (ADR-0107 §2). We built a
ratchet for the symptom and none for the cause. That asymmetry is the root
consequence, and every row of §2 is a leaf of it.

### 1.2 Why a seam is not enough: `StorageEngine` survived one day

ADR-0107 §3 (Option B, reason 5) cites, as evidence the migration is cheap:

> An engine-swap seam already exists in code: S410 step 4 introduced `StorageEngine`
> + `DuckDbEngine` + `const STORAGE_ENGINE` in the snapshot layer […]

**Measured: it does not exist, and has not since 2026-06-15.** Tree-wide search for
`StorageEngine` / `DuckDbEngine` / `STORAGE_ENGINE` / `fold_wal` /
`verify_integrity` returns **zero**. `git log -S` gives the whole life:

| Commit | Date | Effect |
|---|---|---|
| `ee56d2e` (S410) | 2026-06-14 | Adds `trait StorageEngine`, `impl StorageEngine for DuckDbEngine`, `const STORAGE_ENGINE`. |
| `a1edbb0` (S426) | 2026-06-15 | Deletes all of it. `CHECKPOINT` returns inline — `crates/aberp-snapshot/src/take.rs:324`, `crash_safe.rs:230`. |

**One day.** Six weeks later a decision document cited it as standing
infrastructure, and the memory index still records it as landed.

S426 was not wrong. **A port with one implementation is a wrapper, and the next
refactor is right to delete it** (rule 12 — "optimising a thing that shouldn't
exist"). It had one impl, no second consumer, and no test that could go red when
it was inlined.

The same decay is visible in the port that *did* survive: `impl BillingStore` spans
`modules/billing/src/adapters/duckdb_store.rs:923–1082` — **160 of 1 492 lines.**
~89% of the "adapter" is inherent methods and DDL reachable without touching the
port; callers name the concrete `DuckDbBillingStore` **46** times against **13**
references to the trait (`apps/` + `crates/`). `BillingStore` is a door in a wall
with no other walls.

**Consequence for §3, and it is the design constraint that outranks the trait
shapes:** a seam is not the deliverable. **The seam plus what keeps it alive** is
(§4.3). Any step-5 session that ships the traits and skips the ratchet has
rebuilt `StorageEngine` at 20× the size.

---

## 2. The consequences ledger — what this migration exposed

**This section is the deliverable for step 4.** Each row is a coupling class the
DuckDB→SQLite crossing surfaced, measured with a citation, with what it cost and
what the seam does about it. It is written now, while the migration is paying for
it, because this evidence is perishable: once the cutover lands, the *reason* each
of these was expensive stops being visible in the diff.

### 2.0 Headline

**757 SQL statements in 74 src files behind 148 engine-typed function
signatures.** That, not SQLite's difficulty, is what "days, not hours" measures.

| Coupling | `main` @ `3f062ac` |
|---|---:|
| SQL literals, src: `SELECT` / `INSERT` / `UPDATE` / `DELETE` | 272 / 114 / 177 / 11 = **574** |
| DDL literals, src: `CREATE TABLE` / `ALTER TABLE` / `CREATE INDEX` | 56 / 95 / 32 = **183** |
| `params![` sites | **389** src (**449** incl. tests — the ADR-0107 §1.4 figure) |
| `duckdb::Connection` mentions | **120** |
| src files importing `duckdb::` | **74** (121 incl. tests) |
| **src fn signatures taking `&(mut) Connection` / `Transaction`** | **145 / 3 = 148** |
| `ADD COLUMN IF NOT EXISTS` | **114** src (ADR-0108's measured figure, confirmed) |
| `.sql` migration files | **7** |

**Distribution is the finding, not the total.** `apps/aberp/src/serve.rs` is
**33 242 lines**, imports `duckdb::`, and holds only ~3 SQL literals against 42
`.db.write()/.read()` calls. serve.rs is not writing SQL — it is **acquiring engine
handles and passing them down**. The HTTP layer's job is transaction orchestration
expressed in the engine's vocabulary. That is the coupling in its purest form, and
it is invisible to any count of SQL strings.

> ⚠ For `Handle` call sites use `tools/adr0108_handle_census.sh` (238 total / 102
> `read()` / 136 `write()`), **never a grep** — a naive `.db.write()|.db.read()`
> grep returns 84 and is wrong by 2.8×.

### 2.1 DDL — 114 sites, and the fail-open hiding in each one

SQLite has no `IF NOT EXISTS` on `ADD COLUMN`, so all 114 sites become
read-the-columns-then-decide. ADR-0108's own `crates/aberp-db/src/schema.rs:5–17`
(PR #53) states the breakdown and the hazard precisely: **105 in 12 `.rs` files, 8
in 3 `.sql` files, 1 already const-driven** — and PR #49 F-1c identifies the
rewrite as reproducing **D2a's exact fail-open shape**: a column silently not added
→ a later read `.unwrap_or_default()`s → an ADR-0101 guard passes vacuously → an
exempt ÁFA base re-files to NAV at 0%.

Top sites (ADR-0108 §4.2): `modules/billing/src/adapters/duckdb_store.rs` **25**,
`crates/aberp-quote-intake/src/log_table.rs` 17, `apps/aberp/src/quote_intake_query.rs`
15, `apps/aberp/src/partners.rs` 12, `quote_pricing_jobs.rs` 10.

**Cost paid:** one helper (`ensure_columns`) plus 114 hand-threaded call sites plus
a delivery decision for the `.sql` files (ADR-0108 §4.2 Q6 — split `CREATE` from
`ALTER`, 8 lines move). **Under the seam:** ~15 adapter `ensure_schema` impls over
a per-port `const SCHEMA`. The `&'static str` identifier rule and the step-4
post-condition (`schema.rs:22–25`) are already correct and are adopted verbatim.

### 2.2 Money and decimals — `STRICT` does not protect an R2 column

ADR-0108 §3.1 sets three representation rules: **R1** money = `INTEGER` minor
units; **R2** exact non-integers = `TEXT` holding a canonical `rust_decimal`
string; **R3** hashes = `BLOB`.

The measured correction (S450, ADR-0108 §3.1) is the one to carry forward:

| declared | given a REAL | result |
|---|---|---|
| `INTEGER` (R1, money) | `1234.56` | `SQLITE_CONSTRAINT_DATATYPE` — **enforced** |
| `TEXT` (R2, quantity/rate) | `0.1 + 0.2` | **accepted, stored `'0.30000000000000004'`** |
| `BLOB` (R3, hash) | `'abc'` | `SQLITE_CONSTRAINT_DATATYPE` — **enforced** |

**`STRICT` enforces R1 and R3 and does nothing for R2**, and `typeof()` reads
`'text'` either way, so the T-2 sweep is blind to it too. R2's guards are therefore
exactly two, both outside the engine: the Rust-side `Decimal` bind, and
`tools/cut_gate_money_arith.sh` (T-8) keeping arithmetic out of SQL — a gate that
**did not exist until 2026-08-01** while three landed artefacts cited it
(ADR-0108 M-1).

28 `DECIMAL(p,s)` declarations exist on `main`; a `DECIMAL` declaration in SQLite
takes NUMERIC affinity and can become an `f64` — the PR #49 money regression.

**Cost paid:** a full column census (ADR-0108 §3.2, five classes A–F), a
representation ruling per column, a bespoke scanner over **672 SQL statements in
295 files**, and a 14-probe teeth suite. **Under the seam:** `bind_money` /
`read_money` / `bind_decimal` / `read_decimal` in `aberp-storage::codec`. A money
column cannot be bound as anything else because no other bind function accepts a
`Money`. T-8 keeps its job at ~1/20th the scope — the adapter directory.

### 2.3 Case-folding — `LOWER()` is a correctness guard, and it is ASCII-only

SQLite's `LOWER()` folds ASCII only. `apps/aberp/src/partners.rs:1001–1005` uses
five `LOWER()` comparisons as the **duplicate-partner guard**; on crossing it stops
folding `Á`/`Ű`/`Ő` and **admits** the duplicate — a false negative, the direction
that does not announce itself (ADR-0108 M11 / T-12, deliberately still open on
PR #53 because the queries have not yet crossed). `:1049` adds two unescaped `LIKE`
patterns.

🔴 **New finding, and it is outside M11's stated scope.**
`apps/aberp/src/products.rs:367` — `AND LOWER(name) = LOWER(?)`, the **product-name
dedup guard** — and `:402` (`LOWER(...) LIKE ?`) carry the identical shape. M11
names `partners.rs` only. The per-column sweep is complete: **8 `LOWER(` src sites
— 6 partners, 2 products, 0 elsewhere.** Exposure today is zero (no SQLite
connection runs these). It needs a T-12-shaped pin in the products family's
crossing, and it should be added to ADR-0108 §9 by whoever next touches it.

**The general lesson, which is the one worth keeping:** a case-fold that is a
*correctness guard* was written in SQL, where its semantics belong to the engine.
**Under the seam** the fold happens in Rust (`to_lowercase()`) before the bind, in
one port method, for both tables at once — which is exactly M11's prescription,
finally given somewhere to live.

### 2.4 The rest of the dialect surface

| Leak | Sites (`main`) | What it cost |
|---|---:|---|
| `information_schema.{tables,columns}` | **7 src** | No `information_schema` in SQLite. `apps/aberp/src/print_invoice.rs:922` carries the comment *"information_schema is the portable path here"* — written in good faith, **false for the engine we are migrating to, and unfalsifiable by anything in the tree**. ADR-0108 §4.3 also requires `duckdb_store.rs:427` to fail loud on "table absent" rather than return `false`, or the S157 quantity widen silently never runs. |
| `ON CONFLICT` | **21 raw / 5 executable** | ADR-0108 §4.3 resolved this to **empty work** — all 5 targets are the declared `PRIMARY KEY`, verified statement-by-statement. Note the shape of the error though: the raw grep said 21 because **16 were doc comments and 1 a test-assertion string**. A count over SQL-as-text is not a count of behaviour. Under the seam, upserts are *methods* — you count methods. |
| `IS NOT DISTINCT FROM` | 8 | Portable ≥3.39. Confirmed by reading SQLite release notes, not by anything in the tree. |
| `DROP COLUMN IF EXISTS` | 2 | `duckdb_store.rs:357`, `quoting_materials.rs:132` — guard on `pragma_table_info`, then bare `DROP`. |
| `DuckDBFailure` → `SqliteFailure` | **3** | `incoming_invoices.rs:720`, `quote_intake_query.rs:438`, `:499`. ADR-0108 §2.3: *the only error variant with no twin*, wrapped behind `is_engine_failure`. Under the seam it is one `match` in one adapter mapping to a domain `StoreError`. |

### 2.5 SQL-side arithmetic and comparison — the sharpest class

ADR-0108 §3.4 enumerates **seven arithmetic sites plus one comparison**, and two of
them are worth carrying into the ledger because they are *silent wrong answers*,
not errors:

- **`aberp-inventory/src/repository.rs:548`** — `WHERE COALESCE(stock_qty,0) <
  COALESCE(min_stock,0)`. Under R2 both columns are `TEXT`, so `TEXT < TEXT` is
  **lexicographic**: stock `'9'` vs min `'10'` compares `'9' > '1…'` → FALSE →
  **the low-stock product is silently not flagged.** `:549`'s deficit ordering
  additionally forces `TEXT - TEXT` → REAL. And `:585` is *the same predicate, 36
  lines below, reached by a different caller* — found only because the sweep was
  redone per-column instead of per-function.
- **`apps/aberp/src/reports.rs:800`** (M-2) — the ÁFA report and the NAV filing
  rounded differently: the filing truncates per line (`floor(net × bp / 10_000)`,
  `invoice.rs:92`), the report rounded half-even over an unrounded aggregate. Two
  27% lines of 50 Ft net: filed **26 Ft**, reported **27 Ft**, always in the same
  direction. Fixed 2026-08-01 by making both call the *same* functions
  (`line_net_total` / `line_vat_amount`) — *not* by writing equivalent arithmetic,
  because equivalent arithmetic would have tied on most invoices and diverged on
  exactly the `.5` remainders.

**The lesson for the seam is the second one.** These are not portability bugs. They
are what happens when a *domain calculation* is expressible in two places. **Under
the seam ports return rows and folds are domain functions** — so `repository.rs:548`
has no expression at all, because there is no SQL for a domain author to write.

### 2.6 Rule-7: two representations of one physical quantity

```
apps/aberp/src/material_inventory.rs:229-231   on_hand_qty / reserved_qty / committed_qty   DOUBLE
crates/aberp-inventory/migrations/V001__inventory.sql:53   qty_delta                        DECIMAL(18,6)
```

ADR-0108 §9 correctly holds this out of scope — *"migrating both as-is under
`STRICT` makes the divergence look sanctioned."* What the ledger adds is **how it
arose**: two authors, two files, choosing a column type at the point of use, with
no place where "how does ABERP represent a physical quantity" is written as code.
The money types exist and are excellent (`modules/billing/src/domain/money.rs:27`
`Huf(i64)`, `:74` `Eur(i64)`, `:180` `enum Money`) — and they stop at the billing
module's edge. Inventory quantities never got one.

A repository seam is where a representation decision becomes **unavoidable**,
because there is exactly one function that binds a quantity and one that reads it
back. ADR-0108 §3.1's R1/R2/R3 *is* that decision — written in a document and
enforced by a grep gate, rather than by a type.

### 2.7 What the nascent seam already covers — and what is missing

The migration has been forced to build the bottom of the seam. This is real and it
should be built on, not restarted. Measured on **PR #53 @ `349dc94`**:

| Artefact | What it covers | LOC |
|---|---|---:|
| `crates/aberp-db/src/engine.rs:42/48` | The **type-alias re-export** (ADR-0108 D2) — `pub use duckdb::{…}` / `pub use rusqlite::{…}` behind the `sqlite-engine` feature. The *only* place either engine crate is named. | 164 |
| `engine.rs:59` `is_engine_failure` | The one error variant with no twin (§2.4). | |
| `engine.rs:96` `begin_immediate` | `BEGIN IMMEDIATE` discipline (M5), engine-neutral. | |
| `crates/aberp-db/src/schema.rs:119` `ensure_columns` | The **one way a column is added**, with the fail-loud post-condition. | 386 |
| `crates/aberp-db/src/sqlite.rs` | The **only way a SQLite connection is opened**, with the PR #49 security posture pinned before anything can write through it. | |
| `crates/aberp-db/src/readonly.rs` | Read-only DuckDB open — **new capability**; a sweep for `access_mode`/`read_only` returned **zero** non-test hits beforehand. | |
| `crates/aberp-db/src/engine_path.rs` | Boot cross-check: a `sqlite-engine` build whose path is not `*.sqlite` **aborts**. Fail loud, not fail open. | |
| `Handle` (`lib.rs:297` `write()`, `:328` `read()`) | Single-writer, poison recovery, `lock_recovering()` on **both** arms (R-3, binding). | 705 |

**That is a genuine foundation and §3 builds directly on it.** What is missing is
everything above the connection:

| Missing | Consequence |
|---|---|
| **A domain-typed port layer** | The seam abstracts the *crate name*, not the *layer*. `Connection` is still passed to 148 signatures. |
| **A backend-agnostic transaction handle** | ADR-0019 §1's named deliverable. Without it, rule 15's cross-module atomicity cannot be expressed through ports at all (§3.2). |
| **Per-engine adapters holding all SQL** | SQL is still in 74 files, each of which chooses its own dialect. |
| **A conformance suite** | Every mitigation (M1–M12, T-1…T-21) is written for *one* crossing. Nothing is reusable for swap #2. |
| **A ratchet** | See §1.1. Nothing can detect the seam's absence or its decay. |

**Adoption, measured, and it is the number that matters:** of 81 src files that
name `duckdb::` on PR #53, **6** import `aberp_db::engine` — and **5 of those 6 are
the migrator binaries** (`migrate_billing.rs`, `migrate_partners.rs`,
`migrate_to_sqlite.rs`, `premigration.rs`) plus `serve.rs`. `ensure_columns` has
**10 call sites outside `aberp-db`, all in migrator binaries**, against **118**
remaining `ADD COLUMN IF NOT EXISTS`. **The nascent seam is adopted by the
migration's tooling and by essentially none of the product.** That is correct for
where the plan is — families cross from Step 5 onward — and it is exactly why §5's
retrofit is priced from zero.

### 2.8 The counter-metric

On `main`, **74** src files name `duckdb::`. On PR #53's tip, **81**. Src
`params![` went 389 → 396; `ADD COLUMN IF NOT EXISTS` 114 → 118.

**Halfway through a migration whose purpose is to leave DuckDB, the number of files
naming DuckDB went up.** This is not a defect in ADR-0108 — the additions are
migrator tooling that must speak both engines by construction, and they are deleted
at cutover. It is recorded because it is the cleanest possible statement of the
problem: **with no seam, even the act of leaving an engine has to be written in
that engine's vocabulary.**

It also has an operational consequence that corrects an earlier recommendation of
mine — see §5.4.

---

## 3. The seam design

### 3.1 Shape

Four layers. Only the bottom two may name an engine type.

```
  ①  domain / app        Money · Decimal · VatRateKind · ULIDs · DraftInvoice
                         — no SQL, no Connection, no params!.  UNCHANGED.
  ───────────────────────────────────────────────────────────────────────────
  ②  ports               trait InvoiceStore, PartnerStore, InventoryStore,
                         LedgerStore, … — domain verbs, domain types,
                         `&mut Tx<'_>` for composition.  NO SQL.
  ───────────────────────────────────────────────────────────────────────────
  ③  adapters            sqlite/…  postgres/…  in_memory/…  — ALL SQL lives here,
                         one file per port per engine.  The ONLY place `params!`,
                         `Connection`, DDL text, or a dialect quirk appears.
  ───────────────────────────────────────────────────────────────────────────
  ④  aberp-storage       Tx / ReadTx (opaque) · single-writer Handle · codec ·
     (absorbs aberp-db,   ensure_columns · engine.rs alias · commit ordering
      incl. §2.7's work)  (business rows → audit append → mirror fsync)
```

Rule 14 (all-or-nothing per subsystem) is the retrofit boundary, unchanged: **a
family's ports, its adapter, and all its call sites cross together, in one commit.**

Note the adapter row says `sqlite/` and `postgres/`, not `duckdb/`. By step 5 the
DuckDB adapter is gone — which means **the seam's second implementation is not a
second engine**. See §4.3; this is the whole reason the in-memory adapter is
load-bearing rather than a testing nicety.

### 3.2 The load-bearing decision: the transaction handle

This is the piece ADR-0019 §1 named, never built, and the reason the seam is not
trivial. **A naive port-per-module seam is incompatible with CLAUDE.md rule 15.**
Rule 15 requires business `INSERT`s and the audit `append_in_tx` to commit in *one*
transaction on *one* `WriteGuard` (`create_ncr` is the reference). If
`InvoiceStore` and `LedgerStore` are independent traits, they cannot compose into
one transaction without either passing the engine's `Transaction` through the port
— re-leaking exactly what the seam removes — or a unit-of-work type.

The unit-of-work type is the answer, and it must be opaque:

```rust
// aberp-storage — the ONLY module that can construct or unwrap a Tx.
pub struct Tx<'a> { /* private: the engine transaction */ }
pub struct ReadTx<'a> { /* private */ }

impl Storage {                       // absorbs today's aberp_db::Handle
    pub fn write<R>(&self, f: impl FnOnce(&mut Tx<'_>) -> Result<R>) -> Result<R>;
    pub fn read<R>(&self,  f: impl FnOnce(&ReadTx<'_>) -> Result<R>) -> Result<R>;
}

pub trait InvoiceStore {
    fn insert_issued(&self, tx: &mut Tx<'_>, inv: &IssuedInvoice) -> Result<(), StoreError>;
}
pub trait LedgerStore {
    fn append(&self, tx: &mut Tx<'_>, e: &AuditEntry) -> Result<Seq, StoreError>;
}
```

Three properties fall out:

1. **Rule 15 becomes the only expressible shape.** Both ports take the same
   `&mut Tx`, so composing them in one transaction is the *natural* call, and
   "business-commit-then-audit-append" — the torn written-but-unaudited row rule 15
   forbids — becomes awkward to write. Today the correct shape is a markdown rule.
2. **Rule 13 becomes a type.** `Tx` cannot be constructed outside `aberp-storage`.
   A caller wanting a transaction must go through `Storage::write`. There is no
   second way.
3. **Fork-zero narrows from a gate to a visibility boundary** (§3.4).

The closure form rather than a returned guard is deliberate: it makes the nested
`read()`-inside-`write()` self-deadlock (rule 13's last clause; ADR-0108 R-3) a
borrow-checker problem rather than a runtime one. **Honest caveat:** it does not
eliminate it — a closure can still call `Storage::read` on a captured `&Storage`.
The mutex behaviour R-3 pins as *binding* must survive inside `Storage` regardless.
The seam reduces this hazard; it does not retire it.

### 3.3 Where the cross-cutting concerns live — each written once

| Concern | Lives in | Replaces (from §2) |
|---|---|---|
| **Money** | `aberp-storage::codec` — `bind_money(Money)` / `read_money() -> i64`. Ports take `Money`/`Huf`/`Eur`. | §2.2 R1, enforced by a type instead of by `STRICT` + a census |
| **Exact decimals** | `codec::bind_decimal` / `read_decimal`. | §2.2 R2 — the rule `STRICT` **cannot** enforce. The bind function is the enforcement. |
| **Hashes** | `codec::bind_hash(&[u8])`. | §2.2 R3 / the PR #40 BLOB-vs-TEXT chain-link miss |
| **No money arithmetic in SQL** | Structural: ports return rows, folds are domain functions (`line_net_total` / `line_vat_amount`). | §2.5 — `repository.rs:548` has no expression; the M-2 class cannot recur because there is one implementation, not two agreeing |
| **DDL** | `aberp-storage::ensure_columns` (§2.7, promoted as-is) + a per-port `const SCHEMA`. | §2.1's 114 sites → ~15 adapter `ensure_schema` impls |
| **Upsert** | A port method (`upsert_balance` → `Inserted` / `Skipped`), never `ON CONFLICT` at a call site. | §2.4 — and `changes() == 0` becomes a documented return value, not an engine artefact |
| **Case-fold + `LIKE`** | A port method taking `Normalized` / `SearchNeedle` newtypes that fold and escape in their constructors. | §2.3 — partners *and* products, once. A raw `String` cannot reach a `LIKE` pattern. |
| **Ordering / comparison on decimals** | Rust folds over `Decimal`. | §2.5's lexicographic silent-un-flag |
| **Schema probing** | `aberp-storage` (`pragma_table_info` / `sqlite_master` / `information_schema` — adapter's choice). | §2.4's 7 `information_schema` sites and `print_invoice.rs:922`'s false portability claim |
| **Error classification** | `StoreError` — `UniqueViolation` / `NotFound` / `Busy` / `Backend`. Adapters map. | §2.4's 3 `DuckDBFailure` sites |

### 3.4 Where the Handle, the mirror, and fork-zero sit

- **Single-writer `Handle`** becomes `aberp-storage`'s private implementation of
  `Storage::write`. Its API is **narrowed, not widened**: it stops handing out
  `Connection` and hands out `&mut Tx`. Everything ADR-0108 §2.4 preserves — the
  writer mutex, `lock_recovering()` on the read arm (R-3), `db_writer_lock` /
  F-E cross-process fencing — is preserved unchanged and moves inside.
- **Audit ledger + fsync mirror.** The commit ordering (business rows → audit
  `append_in_tx` → mirror fsync) becomes `Tx::commit`'s single implementation
  instead of a convention spread across call sites. ADR-0030's tamper-evidence role
  is untouched. **Out of scope:** whether the mirror's *durability* role retires
  post-cutover — ADR-0107 §2 and ADR-0108 §2.4 both defer it, correctly.
- **Fork-zero.** Under the seam, `Connection` is nameable only inside
  `aberp-storage` and the adapter directory. A route handler *cannot* open a
  connection, because it cannot name the type.

  **Two honest limits, because this is the claim most worth attacking.**
  (a) An adapter still can, so the surface shrinks from **74 files to ~15 adapter
  files** — ~5×, in one greppable directory. A narrowing, not an elimination: keep
  the opener census and **re-scope** it to the adapter directory rather than
  deleting it (ADR-0108 Q9's reasoning holds).
  (b) **This does not fix R-5** — *a foreign connection's `close` silently destroys
  every later commit's durability*, live in production on 13 in-serve routes.
  ADR-0108 §9 rules it gets its own PR, before and independent of the migration.
  Nothing here changes that. A reader who takes "fork-zero becomes structural" as
  licence to defer R-5 has misread this section.

### 3.5 What the seam deliberately does **not** abstract (rule 12)

- **Not a query builder, not an ORM.** ADR-0019 §1 already forbids the ORM;
  adapters write SQL directly, parameterized.
- **Not a runtime engine selector.** ADR-0108 §2.2 D1 (compile-time cargo feature)
  stands and is not reopened. **The seam and D1 are orthogonal:** D1 governs which
  engine crate links; the seam governs which *layer* may name it.
- **Not a replacement for `engine.rs`'s type alias** (§2.7). That re-export is a
  good cheap trick and becomes the adapter's own import path. It abstracts the
  crate name; the seam abstracts the layer. They compose.
- **Not `aberp-snapshot`.** Engine-specific by nature. It gets a small port
  (`fold_wal` / `verify_integrity` / `snapshot_to`) **only when a second engine
  implements it** — i.e. `StorageEngine` (§1.2) done at the moment it has two
  consumers instead of one day too early.
- **Not the search/projection layer** (ADR-0019 §2).
- **No in-memory adapter for a family until that family's port exists.**

---

## 4. The conformance suite — what makes swap #2 cheap

### 4.1 The swap recipe, after the seam

A future engine swap — SQLite → Postgres for the SaaS lane (ADR-0059 / ADR-0100) —
becomes three things:

1. **Implement one adapter directory** — ~15 files, SQL only. No call site outside
   it changes; no domain type changes; no engine-typed signature changes, because
   there are none left.
2. **Pass the conformance suite** (§4.2). Every adapter must. This is the gate.
3. **Migrate the data**, with the reconciliation gate ADR-0108 §6.3 already
   specifies (per-table row counts, per-money-column sums, `verify_chain`
   genesis→head).

Steps 1 and 3 are unavoidable under any design. **Step 2 is what makes "safe" and
"fast" the same path.** Today the safety of a crossing is a bespoke, per-family,
hand-written argument — ADR-0108's twelve mitigations and T-1…T-21, each
mutation-verified, excellent work, and **written once, for one crossing**. With the
suite, the safety argument becomes a standing asset: paid for once, run unchanged
on every crossing after.

Stated without inflation: **the seam does not make crossing #1 cheaper.** It makes
every crossing after the first cheap, and it makes them cheap by making them *safe
by the same artefact*.

### 4.2 What the suite pins

Tier 1 = semantics, every adapter including in-memory. Tier 2 = durability,
persistent adapters only.

| # | Pin | Red when | From |
|---|---|---|---|
| **C1** | Money round-trip: `Huf`/`Eur` in → identical out, incl. `i64::MIN/MAX`, negative, zero | any float path, any lossy affinity | §2.2 R1 |
| **C2** | Money overflow is **loud** — a sum overflowing `i64` returns `Err` | silent wrap | §2.2 |
| **C3** | Exact decimal round-trip at scale 6, trailing zeros, negatives; `0.1+0.2` never appears | `TEXT`-affinity float stringification — **the hole `STRICT` does not close** | §2.2 R2 |
| **C4** | BLOB ≠ TEXT: a hash written `&[u8]` is found by `&[u8]`, **not** by the equivalent `&str` | the PR #40 chain-link-not-found shape | §2.2 R3 |
| **C5** | Upsert: existing PK → `Skipped`, mutates nothing; new PK → `Inserted` | `changes()` divergence | §2.4 |
| **C6** | Case-fold: `Árvíztűrő tükörfúrógép Kft.` ≡ `ÁRVÍZTŰRŐ TÜKÖRFÚRÓGÉP KFT.`; same for the products guard | **any ASCII-only fold** | §2.3 — would have caught M11 pre-crossing |
| **C7** | `LIKE` needle escaping: `100% Precision _ Machining` matches only itself | metacharacter over-match | §2.3 |
| **C8** | Ordering & comparison: `9 < 10` on a quantity column; `NULL` ordering explicit; no storage-class ordering leak | the `repository.rs:548/585` silent un-flag | §2.5 |
| **C9** | DDL idempotence + fail-loud: twice = no-op; missing table = `Err`; post-condition re-read asserts every column present | ADR-0108 M8 / PR #49 F-1c | §2.1 |
| **C10** | Empty result is explicit — `Ok(vec![])` distinguishable; **no port method has a `Default` fallback** | the D2a shape (`.unwrap_or_default()` → vacuous guard → 0% ÁFA re-file) | §2.1 |
| **C11** | Error classification: PK violation → `StoreError::UniqueViolation`, not `Backend` | the 3 `DuckDBFailure` sites' string-sniffing | §2.4 |
| **C12** | Rule-15 atomicity: business insert + audit append in one `Tx`; an `Err` from the audit arm leaves **zero** business rows | torn written-but-unaudited row | §3.2 |
| **C13** | Commit survives `SIGKILL`: `commit()` returned `Ok` ⇒ row present after kill + reopen | the July class (ADR-0107 §1.1) | Tier 2 |
| **C14** | A second connection **cannot un-durable a prior commit** — open a foreign connection, close it, assert prior commits survive *and* later commits land | **R-5** | Tier 2 |
| **C15** | Monotonic sequence floor: after kill+reopen the allocator never re-issues | S444 / PR #46 | Tier 2 |
| **C16** | Single-writer + nesting: writers serialize; `read` inside `write` does not deadlock | ADR-0108 R-3 / T-21 | Tier 2 |
| **C17** | Durability pragmas configured **and mutation-verified** | ADR-0107 §4.1: a pragma no test can red is not configured | Tier 2 |

Seventeen pins. **Every one already exists somewhere** — as an engine-specific
test, an ADR-0108 mitigation, a cut gate, or a defect write-up. The novelty is not
the assertions; it is that they become **adapter-parametric and therefore
reusable**. That is also the honest cost: writing the suite is largely a *port of
existing test intent*, which is why §5 scores it as the largest genuinely-new item
but a bounded one.

### 4.3 What keeps the seam alive

§1.2's lesson, answered structurally. After cutover the DuckDB adapter is deleted,
so the seam is back to **one engine adapter** — the exact condition that killed
`StorageEngine`. Three things prevent the repeat:

1. **The in-memory adapter** (ADR-0019 §1 already mandates it) is the *permanent*
   second implementation. It cannot be inlined away because the conformance suite
   runs against it and module tests depend on it. This is not a testing nicety; it
   is the seam's structural reason to exist between swaps.
2. **The conformance suite** is the executable definition of the port contract. A
   port method with no C-pin is a method nobody can safely reimplement.
3. **A `no-engine-types-outside-adapters` cut gate** — structural (PR #43's lesson:
   match the *shape*; `c065351`'s lesson: survive `rustfmt`). It asserts that
   `rusqlite::` / `Connection` / `params!` appear only under an adapter path. **This
   is the artefact that has been missing since 2026-05-19** — the reason nobody
   noticed ADR-0019 §1 was unbuilt is that no gate could say so. Its timing is
   §5.4.

---

## 5. Honest cost, and the risk

### 5.1 The estimate

Ranges, with the basis stated so each is attackable. Assumes step 5 starts from the
post-cutover tree: SQLite only, DuckDB adapter deleted, §2.7's foundation intact.

| Work item | Net-new LOC | Moved LOC | Kind |
|---|---:|---:|---|
| `aberp-storage`: `Tx`/`ReadTx`, `Storage`, codec, commit ordering — absorbing `aberp-db` (705 LOC lib; 1 585 crate total per ADR-0107 §2) plus §2.7's `engine.rs` (164) / `schema.rs` (386) / `sqlite.rs` / `engine_path.rs` | 400–700 | ~2 200 | **Judgment** — `Tx` (§3.2) is the hard part |
| ~15 port traits, ~180–220 methods (basis: 757 src SQL statements at ~3–4 per domain operation; sanity-checked against `BillingStore`'s 8, which under-cover their own family — §1.2) | 800–1 200 | 0 | **Judgment** — where the port lines go |
| Adapter impls (SQLite): 757 statements relocated + one wrapper fn each | 2 000–3 000 | ~4 000–6 000 | **Mechanical** — the SQL text already exists |
| Call-site rewrite: 148 engine-typed signatures + their callers | 300–600 net | — | **Mechanical**, but 74 files × rule 14 |
| **Conformance suite** (17 pins × 2–4 cases + adapter-parametric harness) | **1 500–2 200** | ~800 ported intent | **Judgment** — highest value |
| In-memory adapters (~15) | 1 200–1 800 | ~235 (billing's, as template) | Mostly **mechanical** |
| `no-engine-types-outside-adapters` gate + probes (basis: existing gates run 300–600 LOC each incl. probes) | 400–700 | 0 | **Mechanical**; one judgment call — the ratchet's baseline |
| **Total** | **≈ 6 600 – 10 200 net-new** | **≈ 7 200 – 9 200 moved** | |

**Effort: ~15 gated steps, one per family, sized like ADR-0108's — which have been
landing at roughly one per session. Call it 10–15 sessions.**

That figure assumes the decided order (§0): the retrofit starts from zero because
each remaining family will already have been hand-translated during steps 1–2. An
interleaved order would have cost 4–7; the difference is the accepted price of
finishing the cutover on a stable plan.

**Where I am most likely wrong: the port count.** ~15 comes from ADR-0108 §7's
family decomposition (invoice, partners, inventory, work-orders, quoting, ledger,
email, dispatch, qa, purchasing, …). If the real answer is 25 because families do
not decompose as cleanly as the migration's step boundaries suggest, the trait and
in-memory rows both scale ~1.6× and the total lands near **14 000**. That is the
number an adversarial should push on first.

### 5.2 Mechanical vs judgment

**Mechanical (≈70% of LOC, ≈30% of risk):** relocating SQL text into adapters; the
import rewrite; wrapping each statement in a function; the `params!` sites, which
**do not change at all** — they move.

**Judgment (≈30% of LOC, ≈70% of risk), four items:**

1. **`Tx` composition (§3.2).** Get it wrong and rule 15 becomes unexpressible, or
   `Tx` leaks the engine and the seam is theatre. Must be designed before any
   family moves.
2. **Port boundaries.** Too fine is an ORM by accretion; too coarse forces a
   god-port or an engine-typed escape hatch. ADR-0006 is the input, but the
   invoice↔ledger↔numbering fusion (ADR-0108's Step-5 "fused family") does not
   respect module lines and needs a ruling.
3. **What stays out (§3.5).** Every "while we're here" is 200 lines deleted next
   quarter (rule 2).
4. **The 17 pins' *contents*.** A pin that cannot go red is worse than no pin —
   ADR-0108's own M-1 is the case study: three landed artefacts cited T-8 while
   `tools/` held no implementation.

### 5.3 The risk of doing it

- **It is a large refactor over a legally-binding tax ledger with 8-year statutory
  retention (ADR-0009).** That does not reduce to zero.
- **The mitigating fact is structural and real:** the retrofit is
  **behaviour-preserving by construction** — no file moves, no schema change, no
  data migration, the same SQL text against the same engine. This is categorically
  different from the cutover, which changes storage. **That asymmetry is exactly
  why the decided order is right** (§0): the dangerous step happens first, on a
  finished plan; the seam happens second, when it cannot lose data.
- **The proof technique already exists and already worked:** ADR-0108's **T-4
  byte-identity** test (landed, *zero divergence* across mixed-rate, storno and
  modification invoices). A retrofit step that keeps T-4 green has not changed the
  filed artefact. Each family step should carry a byte-identity or differential
  gate of its own.
- **Blast radius is bounded by rule 14** — per family, per step, each landing on a
  gate-green base, each independently revertable, because no step changes storage.
- **The genuine hazard is a silent behaviour change during relocation** — a
  `.unwrap_or_default()` reintroduced, an error arm collapsed, a `NULL` handling
  difference. C10 and C11 exist for this. **This is where a step will actually go
  wrong**, not in the design.
- **A post-cutover-specific hazard:** step 5 runs on the *production* line, not on
  a reversible DEV branch. ADR-0108's rollback (§6.2) protects the engine change,
  not this refactor. Step 5's safety net is the gates and T-4-style differential
  pins, and it should be planned as such from the first step rather than
  discovered at step 8.

### 5.4 A correction to my own earlier recommendation

An earlier draft of this ADR recommended landing the
`no-engine-types-outside-adapters` gate **immediately and standalone**, on the
grounds that it is behaviour-neutral and pays off under every branch.

**§2.8 says that is wrong, and the measurement is the reason.** During steps 1–3
the count legitimately *rises* — 74 → 81 — because migrator tooling must name both
engines by construction. A ratchet that has to be raised twice while it is being
installed is not a ratchet; it is a nuisance that teaches the next session to
raise it again. Landing it now would also put a new red gate across an in-flight
PR, which §0 forbids.

**It belongs at step 4**, where the migrator binaries have been deleted, the count
has settled at its true post-cutover value, and the baseline it freezes is a number
worth defending. It is still the cheapest item in this ADR and still the one that
pays off even if the rest of the seam is never built — just not yet.

---

## 6. Execution — what step 4 hands to step 5

**Step 4's output** (the "deduce consequences" step) is: this §2, re-measured
against the post-cutover tree, plus whatever the cutover itself adds. Three things
specifically will have changed and must be re-measured rather than inherited:

1. **The counts.** §2.0's census is a `main`-@-`3f062ac` snapshot. Post-cutover,
   the migrator binaries are gone and every family has crossed; the true retrofit
   surface is smaller than 757/74/148 and must be re-taken, not extrapolated.
2. **Which §2 rows survived as *defects* rather than as costs.** M11/T-12 (§2.3),
   the products finding, the `DOUBLE` vs `DECIMAL` divergence (§2.6) and R-5 are
   open items with owners; step 4 confirms each is closed or carries it forward.
3. **The `no-engine-types-outside-adapters` baseline** (§5.4) — measured and frozen
   at step 4, ratcheting from there.

**Step 5's first commit is not a port.** It is `aberp-storage`'s `Tx` (§3.2),
because every port signature depends on it and getting it wrong invalidates all
~200 methods. Design it, pin C12 against it, and only then cross the first family.

**Suggested family order for step 5**, and the reason: start with a *leaf* family
with no cross-module transaction (dispatch or qa), to shake out the port shape
cheaply; take the invoice↔ledger↔numbering fusion **second**, while the C12
atomicity pin is fresh and before ~13 more ports have been shaped by families that
never exercised it. Do not save the hard one for last — the fused family is the one
that determines whether `Tx` is right, and discovering it is wrong at step 12 costs
everything built before it.

---

## 7. Consequences

**If this blueprint is executed at step 5:**

- ADR-0019 §1 stops being aspirational. Its "never imports DuckDB types" clause
  becomes a gate that can go red.
- Engine swap #2 (SQLite → Postgres, SaaS) costs one adapter directory plus a
  conformance run instead of a second full hand-translation.
- Rules 13 and 15 move from markdown into the type system (§3.2). Rule 14 stays —
  it is a *migration* rule, not an engine one.
- The fork-zero surface narrows 74 → ~15 files; the census is **re-scoped, not
  deleted**.
- The quantity-representation divergence (§2.6) gets a place to be decided.
- **Cost:** 6 600–10 200 net-new LOC, 10–15 sessions (§5.1).
- **Locked in:** SQL lives in adapters, forever. Any future "just one quick query
  here" in a route handler becomes a gate violation. That is the point, and it is
  also a permanent tax on small changes — named as a cost, not only a benefit.

**If it is never executed:** ADR-0019 §1 should be **amended to say what we
actually do** — dialect-portability discipline (`[[no-sql-specific]]`), not a
repository seam. Leaving an Accepted cornerstone ADR describing infrastructure that
does not exist is worse than either building it or retracting it, because the next
engine decision will be costed against the ADR rather than against the tree. That
is not hypothetical: it is exactly what happened to ADR-0107 §3's `StorageEngine`
citation (§1.2).

---

## 8. Adversarial review — three concerns answered in advance

**1. "Why write this now? The consequences do not exist yet — you are documenting a
migration that has not finished."**
Half conceded, and the half that is conceded is scoped: §2's *counts* are a
pre-cutover snapshot and §6 requires them re-measured. What is **not** premature is
the *classes*: the 114 DDL sites, the `STRICT`-does-not-protect-R2 correction, the
ASCII `LOWER()`, the lexicographic `TEXT` comparison, the M-2 rounding fork. Those
were discovered *by* the crossing and each cost real engineering to find. Writing
them down after the cutover means writing them from memory and a diff, six weeks
later — which is precisely how ADR-0107 came to cite a seam that had been deleted
(§1.2). **Evidence is perishable; this ADR is the container, and the timing is the
point.**

**2. "This is the speculative abstraction rules 2 and 12 forbid."**
It would be, if the second consumer were hypothetical. ADR-0059 and ADR-0100 both
name Postgres-per-tenant for the SaaS lane, and ADR-0019 §1 names it as the second
backend the trait must be shaped by. **But** the rule-12 objection lands squarely
on §1.2 and must be answered structurally, not rhetorically: after cutover the
DuckDB adapter is deleted and the seam is back to *one engine*. That is why the
in-memory adapter is load-bearing (§4.3) — without it, §5's 10 000 lines are a
wrapper waiting for its own `a1edbb0`.

**3. "Your own evidence proves the opposite of your thesis. This codebase does not
sustain seams. Why will this one survive?"**
The strongest attack, and the honest answer is: **on the current evidence, it would
not.** `StorageEngine` lasted a day; `BillingStore` survives but is routed around
(160 of 1 492 adapter lines inside the `impl`; 46 concrete references vs 13 to the
trait). Both decayed for one reason: **nothing could measure the decay.** So the
proposal is not the seam — it is the seam *plus* the in-memory adapter, the 17
pins, and the ratchet gate. If step 5 ships the traits and declines §4.3, the
honest prediction is that the seam is ~60% intact in six months and cited as
complete in the ADR after next. The gate is not a nice-to-have; **it is the only
part of this proposal with a track record in this repository.**

---

## 9. Deferral ledger (CLAUDE.md rule 3)

Found while grounding this ADR. **None fixed here** — docs-only.

| Item | Closed by |
|---|---|
| 🔴 **`apps/aberp/src/products.rs:367` (`AND LOWER(name) = LOWER(?)`, the product-name dedup guard) and `:402` carry the identical ASCII-fold / unescaped-`LIKE` hazard as `partners.rs:1001–1005` / `:1049`, and are OUTSIDE ADR-0108 M11's stated scope.** On crossing, SQLite's ASCII-only `LOWER()` makes the guard **admit** a diacritic duplicate. Exposure today zero. Sweep is complete: 8 `LOWER(` src sites — 6 partners, 2 products, 0 elsewhere. | **Step 1** — the products family's crossing, as its first commit, with a T-12-shaped pin (the sequencing M11/T-12 already got). Should be added to ADR-0108 §9 by whoever next touches it. **This is the one item in this ledger that belongs to step 1, not step 4.** |
| **ADR-0107 §3 (Option B, reason 5) cites `StorageEngine` / `DuckDbEngine` / `const STORAGE_ENGINE` as an existing engine-swap seam. Deleted at `a1edbb0` (2026-06-15), one day after `ee56d2e` added it, six weeks before ADR-0107 was written.** Also recorded as landed in the memory index. Does not change ADR-0107's recommendation; does overstate its "already paid for" argument. | A correction line in ADR-0107 §3 when it next lands (it is not on `main`), plus a memory correction. **Not fixed here** — this ADR does not touch in-flight branches (§0). |
| The `no-engine-types-outside-adapters` gate does not exist, so ADR-0019 §1's central clause has been unenforced since 2026-05-19. | **Step 4** — see §5.4 for why *not* now. |
| `apps/aberp/src/print_invoice.rs:922` asserts `information_schema` "is the portable path here". It is not portable to SQLite. | ADR-0108 §4.3 already schedules the rewrite (`sqlite_master`); the **comment** should go with it so the next reader does not re-derive the claim. Step 1. |
| `material_inventory.rs:229–231` `DOUBLE` vs `V001__inventory.sql:53` `DECIMAL(18,6)` — two representations of one physical quantity (rule 7). Already in ADR-0108 §9 as out of scope. | Unchanged — out of scope. Re-recorded because §2.6 uses it as evidence and a reader should not infer this ADR closes it. **Step 4** should confirm it survived the cutover unchanged. |
| **R-5 is live in production today** (a foreign connection's `close` destroys every later commit's durability, 13 in-serve routes). §3.4 narrows the *future* surface and **does not fix it**. | ADR-0108 §9's ruling stands: **its own PR, before anything else.** Recorded here so §3.4 is never read as licence to defer it. |
| §2.0's census is a `main` @ `3f062ac` snapshot and will be stale at step 5. | **Step 4 re-measures it** (§6). Do not extrapolate. |

---

## 10. Open questions

Each is resolved *inside* step 5 unless stated. None blocks steps 1–3.

| # | Question | Resolved by |
|---|---|---|
| **Q1** | How many ports? §5.1 assumes ~15 from ADR-0108 §7's family decomposition; 25 puts the estimate near 14 000 LOC. | Step 5's first family, decomposed for real. |
| **Q2** | Does the invoice↔ledger↔numbering fusion (ADR-0108's Step-5 "fused family") get one port or three composed through `Tx`? Three is cleaner and leans entirely on §3.2 being right. | The `Tx` design commit, before any family moves — and §6 puts the fused family **second** precisely so this is answered early. |
| **Q3** | Do the in-memory adapters implement the **full** port or a subset? A subset weakens the §4.3 ratchet; the full port is ~1 200–1 800 LOC of test-only code. | Step 5, first family. Recommend full; §5.1 already carries the cost. |
| **Q4** | Does `aberp-snapshot` get a port, and when? §3.5 says only when a second engine implements it. | The SaaS/Postgres lane, not step 5. |
| **Q5** | Does the SaaS/Postgres lane (ADR-0059 / ADR-0100) land close enough behind step 5 that the port shape should be validated against Postgres semantics *while* it is being written (`NUMERIC`, real `information_schema`, different upsert)? This does not change **whether** to build the seam — only how much Postgres-shaped scrutiny the port review gets. | Ervin / ADR-0100 phasing. Not blocking. |
| **Q6** | Does step 5 need its own reversibility mechanism, given it runs on the production line without ADR-0108 §6.2's rollback? §5.3 says gates + T-4-style differential pins; that should be confirmed rather than assumed. | Step 5's plan, before its first commit. |
