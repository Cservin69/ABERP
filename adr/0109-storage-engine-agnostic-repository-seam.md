# ADR-0109 — A storage-engine-agnostic repository seam: finishing what ADR-0019 §1 decided

- **Status:** Proposed — **sequencing decision requested from Ervin (§6)**. No code
  is authorised by this document. It changes no runtime behaviour, touches no
  in-flight migration branch, and touches nothing under `~/.aberp/**`.
- **Date:** 2026-08-01
- **Deciders:** Ervin
- **Supersedes:** nothing.
- **Executes:** **ADR-0019 §1** (cornerstone, Accepted 2026-05-19) — which already
  decided this seam, in these words, and which is measurably unbuilt. This ADR is
  its execution plan, amended by what ADR-0108 measured.
- **Related:** ADR-0003 (superseded by 0019; its Decision section is the same
  decision), ADR-0006 (module boundaries — the input to where the port lines go),
  ADR-0008/0030 (audit ledger + fsync mirror), ADR-0059 / ADR-0100 (SaaS —
  Postgres-per-tenant, the *named second consumer* that makes this non-speculative),
  ADR-0107 (engine evaluation), ADR-0108 (the SQLite migration in flight),
  CLAUDE.md rules 2, 7, 12, 13, 14, 15, the memory pins `[[no-sql-specific]]`,
  `[[project_aberp_saas_migration_adr0100]]`.

> **Cross-reference caveat, stated up front.** ADR-0107 and ADR-0108 are **not on
> `main`** as of this writing (`origin/main` = `3f062ac`). They live on
> `adr-db-engine-evaluation` and `adr0108/*` / PR #53. Every `adr/0107-*` and
> `adr/0108-*` link below dangles until those land. Every *measurement* in §1 was
> taken on `main` @ `3f062ac` — the pristine pre-migration tree — and is labelled
> as such; branch-only artefacts are labelled with their branch.

---

## Context

Ervin's framing, 2026-08-01, in his words: *the DuckDB→SQLite migration taking days
is itself the evidence that the engine-swappability goal is unmet. "Safe" and
"fast" should be the same path, and the way to make them converge is a real
abstraction seam.*

That framing is correct, and the measurement below is worse than the framing
implies. The goal is not merely unmet — **it was decided, written down as a
cornerstone ADR, and then not built**, and nothing in the tree has ever been able
to tell us so. This ADR's first job is therefore not to propose an abstraction. It
is to establish, with counts and line numbers, that the abstraction ADR-0019
mandates does not exist; to explain the specific mechanism by which it decayed;
and only then to specify what completing it costs.

The distinction matters because it changes the recommendation. "We need a seam" is
a design argument that CLAUDE.md rules 2 and 12 are entitled to attack as
speculative. "ADR-0019 §1 is Accepted, its five concrete deliverables are 1-for-5
built, and the missing four are exactly the four the SQLite migration is currently
paying for by hand" is not a design argument. It is a defect report against the
architecture of record.

---

## 1. Ground truth — the measured gap

All counts: `main` @ `3f062ac`, scope `crates/ apps/ modules/`, `--include='*.rs'
--include='*.sql'`. "src" excludes `/tests/` paths.

### 1.1 What ADR-0019 §1 decided

Verbatim, from `adr/0019-storage-strategy-no-fks.md` (Accepted, cornerstone):

> Each module defines its own **storage port** as a Rust trait whose methods are in
> terms of *domain types*, not SQL. […] Each module ships: a **DuckDB adapter**
> […] an **in-memory adapter** (tests; same trait). Module code never imports
> DuckDB types. The string `duckdb` does not appear in the domain or app layers of
> any module. […] A shared `aberp-storage` crate provides: connection pool
> abstraction […] forward-only versioned migration runner, recorded in a
> `_aberp_migrations` table per tenant […] **a transaction handle type that modules
> use without naming the backend.**

Five deliverables. Their state today:

| ADR-0019 §1 deliverable | Built? | Evidence (`main` @ `3f062ac`) |
|---|---|---|
| Per-module storage **port trait** | **1 of ~21** | Exactly one domain port exists tree-wide: `modules/billing/src/ports/storage.rs:177` `pub trait BillingStore` (8 methods). The only other `trait *Store` is `crates/aberp-secret-store/src/lib.rs:52` — secrets, not storage. |
| **In-memory adapter** per module | **1** | `modules/billing/src/adapters/in_memory_store.rs:39` `impl BillingStore for InMemoryBillingStore`. |
| *"The string `duckdb` does not appear in the domain or app layers"* | **violated 74×** | 74 non-test src files import `duckdb::`, across 11 crates/apps — **49 of them in `apps/aberp` alone** (121 files including tests). Including, precisely, the app layer of the one module that has a port: `modules/billing/src/app/error.rs:14` — `Storage(#[from] duckdb::Error)`. |
| Shared **`aberp-storage` crate** | **does not exist** | `ls crates/` — no `aberp-storage`. |
| Versioned **migration runner** / `_aberp_migrations` | **does not exist** | Tree-wide grep for `_aberp_migrations`: **0**. |
| **Backend-agnostic transaction handle** | **does not exist** | 148 src fn signatures take `&(mut) Connection` / `&(mut) Transaction` directly (§1.3). |

The sharpest single line in the tree is the pair two files apart inside the
exemplar module:

```
modules/billing/src/ports/storage.rs:5   //! The SQL string `duckdb` does not appear in domain or app layers.
modules/billing/src/app/error.rs:14          Storage(#[from] duckdb::Error),
```

The doc comment asserting the invariant and the app-layer type violating it have
coexisted long enough that neither has been read against the other. **Nothing in
the tree can detect this.** There is no gate, no test, and no census for
"engine type named outside an adapter" — while there *are* six cut gates and 3 712
lines of scanner machinery for the opener census (ADR-0107 §2). We built a ratchet
for the symptom and none for the cause.

### 1.2 The seam that existed for one day

ADR-0107 §3 (Option B, reason 5) cites, as evidence the migration is cheap:

> An engine-swap seam already exists in code: S410 step 4 introduced `StorageEngine`
> + `DuckDbEngine` + `const STORAGE_ENGINE` in the snapshot layer, moving
> `CHECKPOINT` behind `fold_wal` and `PRAGMA verify_external_invariants` behind
> `verify_integrity`.

**Measured: it does not exist and has not existed since 2026-06-15.** A tree-wide
search for `StorageEngine` / `fold_wal` / `verify_integrity` / `STORAGE_ENGINE`
returns **zero**. `git log -S` gives the whole life:

| Commit | Date | Effect |
|---|---|---|
| `ee56d2e` (S410) | 2026-06-14 | Adds `trait StorageEngine`, `impl StorageEngine for DuckDbEngine`, `const STORAGE_ENGINE`. |
| `a1edbb0` (S426) | 2026-06-15 | Deletes all of it. `CHECKPOINT` returns inline — `crates/aberp-snapshot/src/take.rs:324`, `crates/aberp-snapshot/src/crash_safe.rs:230`. |

**The seam survived one day.** Six weeks later a decision document cited it as
standing infrastructure, and the memory index still records it as landed.

This is the mechanism, and it is the most important paragraph in this ADR: **a
port with one implementation is not a seam; it is a wrapper, and the next
refactor is correct to delete it** (rule 12 — "optimising a thing that shouldn't
exist"). S426 was not wrong. `StorageEngine` had one impl, no second consumer, and
no test that could go red when it was inlined. It had no reason to survive and it
did not.

Any seam this ADR proposes must therefore answer, before anything else: *what
keeps it alive?* §4.3 is that answer, and it is the reason the in-memory adapter
is not optional.

### 1.3 The coupling census — what a swap actually costs today

| Coupling | Count | Note |
|---|---:|---|
| `params![` call sites | **449** | matches ADR-0107 §1.4 |
| `duckdb::Connection` mentions | **120** | matches ADR-0107 §1.4 |
| src files importing `duckdb::` | **74** | 121 including tests |
| **src fn signatures taking `&(mut) Connection`** | **145** | the seam-crossing count |
| src fn signatures taking `&(mut) Transaction` | **3** | |
| `.transaction(` / `.prepare(` / `.execute(` / `.query_row(` / `.query_map(` / `.execute_batch(` (src) | 221 / 157 / 214 / 117 / 117 / 105 | |
| `state.db.write()` / `.read()` | 84 | (ADR-0108 R-2 measures 238 total `Handle` call sites, 102 `read()`, via `tools/adr0108_handle_census.sh` — **use that, never a grep**) |
| SQL statement literals, src: `SELECT`/`INSERT`/`UPDATE`/`DELETE` | 272 / 114 / 177 / 11 | **574** DML |
| SQL DDL literals, src: `CREATE TABLE`/`ALTER TABLE`/`CREATE INDEX` | 56 / 95 / 32 | **183** DDL |
| `ADD COLUMN IF NOT EXISTS` | **119** raw (ADR-0108 measures **114** src + tests separately) | |
| `.sql` migration files | **7** | dispatch ×1, qa ×2, work-orders ×3, inventory ×1 |

**757 SQL statements in 74 files behind 148 engine-typed function signatures.**
That is the number Ervin's "days, not hours" observation is measuring. It is not
that SQLite is hard; it is that there is no single place where SQL is written, so
every dialect difference is a search-and-replace across the product.

Distribution matters as much as the total. `apps/aberp/src/serve.rs` is **33 242
lines** and imports `duckdb::` — but it holds only ~3 SQL literals and 42
`.db.write()/.read()` calls. serve.rs is not writing SQL; it is **acquiring
engine handles and passing them down**. That is the coupling in its purest form:
the HTTP layer's job is transaction orchestration expressed in the engine's
vocabulary. A domain repository would take that job away from it entirely.

### 1.4 The dialect leaks the migration is currently hand-translating

Each of these is a *symptom of the same cause* — the app speaks the engine's
dialect directly, so every dialect difference is an app-wide edit:

| Leak | Sites (`main`) | What breaks on a swap |
|---|---|---|
| `ADD COLUMN IF NOT EXISTS` | 119 | SQLite has no `IF NOT EXISTS` on `ADD COLUMN`. ADR-0108 §4.1 answers with one `ensure_columns` helper — **the right answer, and it is a repository-layer function that currently has to be threaded to 119 call sites by hand.** |
| `information_schema.{tables,columns}` | 7 src | No `information_schema` in SQLite (`pragma_table_info` / `sqlite_master`). `apps/aberp/src/print_invoice.rs:922` carries the comment *"information_schema is the portable path here"* — written in good faith, false for the engine we are migrating to, and unfalsifiable by any test in the tree. |
| SQL `LOWER()` as a **correctness** guard | 8 src | `apps/aberp/src/partners.rs:1001–1005` is the duplicate-partner guard. SQLite's `LOWER()` is **ASCII-only**, so on crossing it stops folding `Á`/`Ű`/`Ő` and **admits** the duplicate (ADR-0108 M11/T-12, deliberately still open on PR #53). |
| …and the same shape, **outside M11's scope** | 2 | `apps/aberp/src/products.rs:367` `AND LOWER(name) = LOWER(?)` — the product-name dedup guard — and `:402` (a `LOWER(...) LIKE`). ADR-0108's M11 row names only `partners.rs`. **New finding; recorded in §8.** |
| `ON CONFLICT` | 21 raw / **5 executable** | ADR-0108 §4.3 resolved this to empty work (all 5 targets are declared PKs). Note the shape of the error: the raw grep was 21 because 16 were doc comments. A repository layer makes this measurable by construction — upserts would be *methods*, and you count methods. |
| `IS NOT DISTINCT FROM` | 8 | Fine ≥3.39; portability confirmed only by having read the SQLite release notes, not by anything in the tree. |
| `DECIMAL(p,s)` declarations | 28 | SQLite has no decimal type; a `DECIMAL` declaration takes NUMERIC affinity and can become `f64` — the PR #49 money regression, closed by ADR-0108 §3's R1/R2/R3 rules. |

### 1.5 The rule-7 divergences a seam would have made impossible

CLAUDE.md rule 7: *surface conflicts, don't average them.* Two representations of
one physical quantity, both live, in the same product:

```
apps/aberp/src/material_inventory.rs:229-231   on_hand_qty / reserved_qty / committed_qty   DOUBLE
crates/aberp-inventory/migrations/V001__inventory.sql:53   qty_delta                        DECIMAL(18,6)
```

ADR-0108 §9 records this as out of scope, correctly — *"migrating both as-is under
`STRICT` makes the divergence look sanctioned."* But note **how it arose**: two
authors, two files, two years apart, each choosing a column type at the point of
use, with no place where "how does ABERP represent a physical quantity" is written
down as code. The money types exist and are excellent
(`modules/billing/src/domain/money.rs:27` `Huf(i64)`, `:74` `Eur(i64)`, `:180`
`enum Money`) — and they stop at the billing module's edge. Inventory quantities
never got one.

**A repository seam is where a representation decision becomes unavoidable**,
because there is exactly one function that binds a quantity and exactly one that
reads it back. That is not a hypothetical benefit; it is the specific defect
class above, and ADR-0108 §3.1's R1/R2/R3 rules are that decision, written down —
but written down in a *document*, enforced by a *grep gate*
(`tools/cut_gate_money_arith.sh`, PR #53) rather than by a type.

---

## 2. Diagnosis, in one paragraph

ABERP's architecture of record (ADR-0003 → ADR-0019 §1) specified a
domain-repository seam. What was actually built is a *dialect-portability
discipline*: no foreign keys, no engine-minted identity, no CHECK constraints,
ANSI-only SQL, app-minted ULIDs, invariants in Rust (`[[no-sql-specific]]`, S410).
That discipline is real and it is why ADR-0107 could honestly call the migration
"weeks, not months" instead of "a rewrite" — **it removes semantic obstacles**.
What it does not do is remove *sites*. 757 statements in 74 files is 757 statements
in 74 files whether or not each one is portable. The discipline made each edit
easy and left the number of edits untouched. A seam attacks the number of edits.
That is precisely the gap between "safe" (which the discipline delivers) and
"fast" (which it does not), and it is why Ervin's two words currently name two
different paths.

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
  ③  adapters            duckdb/…  sqlite/…  in_memory/…  — ALL SQL lives here,
                         one file per port per engine.  The ONLY place `params!`,
                         `Connection`, DDL text, or a dialect quirk appears.
  ───────────────────────────────────────────────────────────────────────────
  ④  aberp-storage       Tx / ReadTx (opaque) · the single-writer Handle ·
     (new crate, absorbs   ensure_columns · migration runner · commit ordering
      aberp-db)            (business rows → audit append → mirror fsync)
```

Rule 14 (all-or-nothing per subsystem) is unchanged and is the retrofit boundary:
**a family's ports, its adapter, and all its call sites cross together, in one
commit.**

### 3.2 The load-bearing design decision: the transaction handle

This is the piece ADR-0019 §1 named ("a transaction handle type that modules use
without naming the backend"), never built, and the reason the whole seam is
non-trivial. **A naive port-per-module seam is incompatible with CLAUDE.md rule
15.** Rule 15 requires business `INSERT`s and the audit `append_in_tx` to commit in
*one* transaction on *one* `WriteGuard` (`create_ncr` is the reference). If
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

// Ports take the opaque handle. Domain code never sees inside it.
pub trait InvoiceStore {
    fn insert_issued(&self, tx: &mut Tx<'_>, inv: &IssuedInvoice) -> Result<(), StoreError>;
}
pub trait LedgerStore {
    fn append(&self, tx: &mut Tx<'_>, e: &AuditEntry) -> Result<Seq, StoreError>;
}
```

Three properties fall out, and they are the seam's whole value:

1. **Rule 15 becomes the only expressible shape.** Both ports take the same
   `&mut Tx`, so composing them in one transaction is the *natural* call and
   "business-commit-then-audit-append" (the torn-row shape rule 15 forbids) becomes
   awkward to write. Today the correct shape is a rule in a markdown file.
2. **Rule 13 ("one Handle, all access") becomes a type, not a rule.** `Tx` cannot
   be constructed outside `aberp-storage`. A caller that wants a transaction must
   go through `Storage::write`. There is no second way.
3. **Fork-zero narrows from a gate to a visibility boundary** — see §3.4.

The closure form (`write(|tx| …)`) rather than a returned guard is deliberate: it
makes the nested-`read()`-inside-`write()` self-deadlock (CLAUDE.md rule 13's last
clause, ADR-0108 R-3) a *borrow-checker* problem rather than a runtime one, since
`&mut Tx` is already exclusively borrowed. **Honest caveat:** it does not fully
eliminate it — a closure can still call `Storage::read` on a captured `&Storage`.
The mutex-based `lock_recovering()` behaviour ADR-0108 R-3 pins as *binding* must
survive inside `Storage` regardless. The seam reduces this hazard; it does not
retire it, and any claim otherwise should be attacked.

### 3.3 Where the cross-cutting concerns live — each written once

| Concern | Lives in | Written once because |
|---|---|---|
| **Money** | `aberp-storage::codec` — `bind_money(i64)` / `read_money() -> i64`. Ports take `Money`/`Huf`/`Eur`; adapters call the codec. | ADR-0108 §3.1 R1 becomes one function pair per engine instead of a gate over 672 statements. A money column cannot be bound as anything else because no other bind function accepts a `Money`. |
| **Exact decimals** (quantity, rate) | `aberp-storage::codec` — `bind_decimal` / `read_decimal`. | R2 today is guarded by *two* things (the Rust bind, and `tools/cut_gate_money_arith.sh` — ADR-0108 M-1, built 2026-08-01). Under the seam the second guard's job shrinks to "the adapter directory", because SQL exists nowhere else. |
| **No arithmetic on money/qty in SQL** | Structurally: ports return rows; folds are domain functions (`aberp_billing::domain::invoice::{line_net_total, line_vat_amount}` — the M-2 fix, landed 2026-08-01). | The T-8 scanner keeps its job but its scope collapses from 295 files to ~15 adapter files. |
| **DDL / `ensure_columns`** | `aberp-storage` (ADR-0108 §4.1's helper, promoted from `aberp-db`) + a per-port `const SCHEMA`. | 119 sites become ~15 adapter `ensure_schema` impls. The `&'static str` identifier rule (ADR-0108 §4.1) is preserved verbatim — it is already the right design. |
| **Upsert** | A port method (`upsert_balance`, not `ON CONFLICT`). | 5 executable sites; the adapter chooses `ON CONFLICT DO NOTHING`, `INSERT OR IGNORE`, or `MERGE`. The `changes() == 0` idempotency signal becomes a documented return value (`Inserted`/`Skipped`), not an engine artefact. |
| **Case-folded lookup** | A port method (`find_duplicate_partner(&Normalized)`), with the fold done in **Rust** (`to_lowercase()`) before the bind. | Kills the `LOWER()` ASCII trap at its root, for `partners.rs` *and* `products.rs`, once. This is ADR-0108 M11's prescription — the seam is where it has a home. |
| **`LIKE` escaping** | Same: a port method takes a `SearchNeedle` newtype that escapes `%`/`_`/`\` in its constructor. | A raw `String` cannot reach a `LIKE` pattern. |
| **Ordering / comparison on decimals** | Ports return rows; filtering and ordering are Rust folds. | ADR-0108 §3.4's site 7 (`repository.rs:548/585`, lexicographic `TEXT < TEXT` silently un-flagging a low-stock product) has no expression: there is no SQL for a domain author to write. |
| **Error classification** | `StoreError` — a domain enum (`UniqueViolation`, `NotFound`, `Busy`, `Backend`). Adapters map. | The 3 `duckdb::Error::DuckDBFailure` sites (`incoming_invoices.rs:720`, `quote_intake_query.rs:438`, `:499`) that ADR-0108 §2.3 flags as *the only variant with no twin* stop being a portability problem: they are one `match` in one adapter. |

### 3.4 Where the Handle, the mirror, and fork-zero sit

- **Single-writer `Handle`** becomes `aberp-storage`'s private implementation of
  `Storage::write`. Its API is *narrowed*, not widened: it stops handing out
  `Connection` and hands out `&mut Tx`. Everything ADR-0108 §2.4 preserves
  (the writer mutex, `lock_recovering()` on the read arm per R-3, `db_writer_lock`
  / F-E cross-process fencing) is preserved unchanged and moves inside.
- **Audit ledger + fsync mirror.** The commit ordering — business rows, audit
  `append_in_tx`, mirror fsync — becomes `Tx::commit`'s single implementation
  instead of a convention spread across call sites. ADR-0030's mirror keeps its
  tamper-evidence role untouched; the seam changes *where the ordering is
  written*, not what it is. **Explicitly out of scope:** whether the mirror's
  *durability* role retires post-cutover (ADR-0107 §2, ADR-0108 §2.4 — both
  correctly defer it).
- **Fork-zero.** Today: 6 cut gates, ~3 712 LOC of scanners, a frozen 81-opener
  census, 33 read-forks (14 live in-serve) frozen not fixed, and R-5 — *a foreign
  connection's `close` silently destroys every later commit's durability*, **live
  in production on 13 in-serve routes**. Under the seam, `Connection` is nameable
  only inside `aberp-storage` and the adapter directory. A route handler
  *cannot* open a connection, because it cannot name the type.

  **Two honest limits, because this is the claim most worth attacking.**
  (a) An adapter still can, so the surface shrinks from **74 files to ~15
  adapter files** — a ~5× reduction, in one greppable directory. That is a
  narrowing, not an elimination, and the opener census should be *kept and
  re-scoped to the adapter directory*, not deleted. (b) **This does not fix R-5.**
  R-5 is live on DuckDB in production today and ADR-0108 §9 rules it must get its
  own PR, before and independent of any migration. Nothing here changes that, and
  a reader who takes "fork-zero becomes structural" as licence to defer R-5 has
  misread this section.

### 3.5 What the seam deliberately does **not** abstract (rule 12)

Named explicitly, because an unbounded seam is the failure mode rule 2 exists to
stop:

- **Not a query builder, not an ORM.** ADR-0019 §1 already forbids the ORM;
  adapters write SQL directly, parameterized. Unchanged.
- **Not a runtime engine selector.** ADR-0108 §2.2 D1 rejected linking both
  engines and this ADR does not reopen it. Adapter selection stays compile-time
  (`sqlite-engine` feature). **The seam and D1 are orthogonal** — D1 is about which
  engine crate links; the seam is about which *layer* may name it.
- **Not a replacement for `aberp_db::engine`'s type alias** (ADR-0108 §2.3 D2,
  on PR #53). The re-export is a *good* cheap trick and stays as the adapter's
  own import path. It abstracts the crate name; the seam abstracts the layer.
  They compose.
- **Not `aberp-snapshot`.** Snapshot/restore is engine-specific by nature. It gets
  a small port (`fold_wal` / `verify_integrity` / `snapshot_to`) **only when a
  second engine implements it** — i.e. it is `StorageEngine` (§1.2) done at the
  moment it has two consumers instead of one day too early.
- **Not the search/projection layer** (ADR-0019 §2). Projections stay projections.
- **No in-memory adapter for a family until that family's port exists.** The
  in-memory adapters are the ratchet (§4.3), not a testing-convenience side quest.

---

## 4. How "safe == fast" falls out

### 4.1 The swap recipe, after the seam

A future engine swap (SQLite → Postgres for the SaaS lane, ADR-0059 / ADR-0100)
becomes exactly three things:

1. **Implement one adapter directory** — ~15 files, one per port, containing SQL
   only. No call site outside it changes. No domain type changes. No signature
   in the 148-site set changes, because there are no engine-typed signatures left.
2. **Pass the conformance suite** (§4.2). Every adapter must. This is the gate.
3. **Migrate the data**, with the reconciliation gate ADR-0108 §6.3 already
   specifies (per-table row counts, per-money-column sums, `verify_chain`
   genesis→head).

Steps 1 and 3 are unavoidable under any design. **Step 2 is what converts "safe"
and "fast" into the same path**: today the safety of a crossing is established by
a bespoke, per-family, hand-written argument (ADR-0108's twelve mitigations, T-1…T-21,
each mutation-verified — excellent work, and *written once for one crossing*).
With the suite, the safety argument is a **standing asset**: it was paid for on
crossing #1 and it runs unchanged on crossings #2 and #3.

That is the concrete mechanism, and it should be stated without inflation: the
seam does not make the *first* crossing cheaper. It makes every crossing after
the first cheap, and it makes them cheap by making them *safe by the same
artefact*.

### 4.2 What the conformance suite pins

Two tiers. Tier 1 is semantics — every adapter, including in-memory. Tier 2 is
durability — persistent adapters only.

**Tier 1 — semantics (every adapter):**

| # | Pin | Red when |
|---|---|---|
| C1 | **Money round-trip.** `Huf(i64)`/`Eur(i64)` in → identical out, incl. `i64::MIN/MAX`, negative, zero. | any float path; any lossy affinity |
| C2 | **Money overflow is loud.** A sum that overflows `i64` returns `Err`, never a wrapped or coerced value. | silent wrap (ADR-0108 §3.1's rejected scaled-integer hazard) |
| C3 | **Exact decimal round-trip.** `Decimal` at scale 6, trailing zeros, negatives, `0.1+0.2` never appears. | `TEXT`-affinity float stringification (the exact hole `STRICT` does **not** close — ADR-0108 §3.1 correction) |
| C4 | **BLOB ≠ TEXT.** A hash written as `&[u8]` is found by a `&[u8]` lookup and **not** by the equivalent `&str`. | the PR #40 chain-link-not-found shape |
| C5 | **Upsert.** `upsert` on an existing PK returns `Skipped` and mutates nothing; on a new PK returns `Inserted`. | `changes()` semantics divergence |
| C6 | **Case-fold.** `Árvíztűrő tükörfúrógép Kft.` and `ÁRVÍZTŰRŐ TÜKÖRFÚRÓGÉP KFT.` are the same partner; `products.rs`' name guard likewise. | **any ASCII-only fold** — this is the pin that would have caught ADR-0108 M11 before the crossing |
| C7 | **`LIKE` needle escaping.** A needle containing `%` and `_` matches only the literal (`100% Precision _ Machining`). | unescaped metacharacter over-match |
| C8 | **Ordering & comparison.** `9 < 10` on a quantity column ordering; `NULL` ordering explicit; no storage-class ordering leak. | the `repository.rs:548/585` lexicographic silent-un-flag |
| C9 | **DDL idempotence + fail-loud.** `ensure_schema` twice is a no-op; a missing table is `Err`, never `Ok(())`; the post-condition re-read asserts every requested column present. | ADR-0108 §4.1's M8 / F-1c fail-open |
| C10 | **Empty result is explicit.** A no-rows query returns `Ok(vec![])` distinguishably; **no port method has a `Default` fallback.** | the D2a shape — `.unwrap_or_default()` making an ADR-0101 guard pass vacuously |
| C11 | **Error classification.** A PK violation → `StoreError::UniqueViolation`, not `Backend`. | the 3 `DuckDBFailure` sites' string-sniffing |
| C12 | **Rule-15 atomicity.** A business insert + audit append in one `Tx`; an `Err` from the audit arm leaves **zero** business rows. | torn written-but-unaudited row |

**Tier 2 — durability (persistent adapters only):**

| # | Pin | Red when |
|---|---|---|
| C13 | **Commit survives `SIGKILL`.** `commit()` returned `Ok` ⇒ the row is present after kill + reopen. | the July class (ADR-0107 §1.1) |
| C14 | **A second connection cannot un-durable a prior commit.** Open a foreign connection on the same path, close it, assert every prior commit survives *and* every later commit lands. | **R-5** — this is the pin that would have caught it |
| C15 | **Monotonic sequence floor.** After kill+reopen, the allocator never re-issues an already-issued number. | S444 / PR #46 |
| C16 | **Single-writer + nesting.** Concurrent writers serialize; a `read` inside a `write` does not deadlock (ADR-0108 R-3 / T-21). | |
| C17 | **`fullfsync` / durability pragmas are configured *and* mutation-verified.** | ADR-0107 §4.1's rule: a pragma no test can red is not configured |

Seventeen pins. **Every one of them already exists somewhere** — as a
DuckDB-specific test, an ADR-0108 mitigation (M1…M12), a cut gate, or a defect
write-up. The suite's novelty is not the assertions; it is that they become
**adapter-parametric** and therefore reusable. That is worth stating plainly
because it is also the honest cost: writing the suite is largely a *port* of
existing test intent, which is why §5 scores it as the largest genuinely-new item
but not an unbounded one.

### 4.3 What keeps the seam alive

§1.2's lesson, answered directly. A seam with one implementation dies. The seam
therefore ships with **two permanent implementations and one ratchet**:

1. **The in-memory adapter** (ADR-0019 §1 already mandates it) is the *permanent*
   second implementation. It cannot be inlined away by a refactor because the
   conformance suite runs against it and module tests depend on it. It is the
   reason `StorageEngine` died and `BillingStore` did not.
2. **The conformance suite** is the executable definition of the port contract. A
   port method with no C-pin is a port method nobody can safely reimplement.
3. **A `no-engine-types-outside-adapters` cut gate** — structural (per PR #43's
   lesson: match the *shape*, and per `c065351`'s lesson: survive `rustfmt`). It
   asserts that `duckdb::` / `rusqlite::` / `Connection` / `params!` appear only
   under an adapter path. Today that gate would report **74 violations**; it lands
   as a ratchet at 74 and can only shrink. **This is the artefact that has been
   missing since 2026-05-19** — the reason nobody noticed ADR-0019 §1 was unbuilt
   is that no gate could say so.

---

## 5. Honest cost

### 5.1 The estimate

Ranges, not points. Basis stated for each so the estimate is attackable.

| Work item | Net-new LOC | Moved LOC | Judgment or mechanical |
|---|---:|---:|---|
| `aberp-storage` crate: `Tx`/`ReadTx`, `Storage`, codec, `ensure_columns`, commit ordering — absorbing today's `aberp-db` (705 LOC lib + the crate's 1 585 total per ADR-0107 §2) | 400–700 | ~1 600 | **Judgment** — the `Tx` design (§3.2) is the hard part |
| ~15 port traits, ~180–220 methods (basis: 757 src SQL statements at ~3–4 statements per domain operation; sanity-checked against `BillingStore`'s 8 methods, which under-cover their own family — §8.3) | 800–1 200 | 0 | **Judgment** — where the port lines go (ADR-0006 is the input) |
| Adapter impls (DuckDB): 757 SQL statements relocated + one wrapper fn each | 2 000–3 000 | ~4 000–6 000 | **Mechanical** — the SQL text is already written and already portable |
| Call-site rewrite: 148 engine-typed signatures + their callers | 300–600 (net delta) | — | **Mechanical**, but 74 files × rule 14 |
| **Conformance suite** (17 pins × 2–4 cases, adapter-parametric harness) | **1 500–2 200** | ~800 (ported intent) | **Judgment** — the highest-value item |
| In-memory adapters (~15) | 1 200–1 800 | ~235 (billing's, as the template) | Mostly **mechanical** |
| The `no-engine-types-outside-adapters` gate + probes (basis: existing gates run 300–600 LOC each incl. probe suites) | 400–700 | 0 | **Mechanical**, with one judgment call: the ratchet's initial baseline |
| **Total** | **≈ 6 600 – 10 200 net-new** | **≈ 6 600 – 8 600 moved** | |

**Effort.** ~15 gated steps, one per family, sized like ADR-0108's steps (which
have been landing at roughly one per session). **Standalone: 10–15 sessions.**
Folded into the remaining ADR-0108 crossings (§6): **4–7 additional sessions**,
because the SQL relocation happens during a crossing anyway — the marginal work is
writing the trait and the in-memory impl, not moving the SQL.

**Where I would be most wrong:** the port-count. I estimate ~15 ports from the
family decomposition ADR-0108 §7 already uses (invoice, partners, inventory,
work-orders, quoting, ledger, email, dispatch, qa, purchasing, …). If the real
answer is 25 ports because families do not decompose as cleanly as the migration's
step boundaries suggest, the trait and in-memory rows both scale ~1.6× and the
total lands near 14 000. That is the single number an adversarial should push on.

### 5.2 Mechanical vs judgment, stated sharply

**Mechanical (≈70% of the LOC, ≈30% of the risk):** relocating SQL text into
adapters; the `use duckdb::X` → adapter-local import rewrite; wrapping each
statement in a function; the `params!` sites, which **do not change at all** —
they move.

**Judgment (≈30% of the LOC, ≈70% of the risk), four items:**

1. **`Tx` composition (§3.2).** Get this wrong and rule 15 becomes unexpressible,
   or `Tx` leaks the engine and the seam is theatre. This is the one piece that
   must be designed before any family moves.
2. **Port boundaries.** A port too fine is an ORM by accretion; too coarse and
   cross-family reads force either a god-port or an engine-typed escape hatch.
   ADR-0006 module boundaries is the input, but the invoice↔ledger↔numbering fusion
   (ADR-0108's Step-5 "fused family") does not respect module lines and will need a
   ruling.
3. **What stays out (§3.5).** Every "while we're here" is 200 lines deleted next
   quarter (rule 2).
4. **The 17 C-pins' *contents*.** A pin that cannot go red is worse than no pin —
   ADR-0108's own M-1 is the case study: three landed artefacts cited T-8 while
   `tools/` held no implementation.

### 5.3 The risk of doing it

Stated as an auditor would:

- **This is a large refactor over a legally-binding tax ledger with 8-year
  statutory retention (ADR-0009).** That is the top-line risk and it does not
  reduce to zero.
- **Mitigating fact, and it is a real one:** the retrofit is **behaviour-preserving
  by construction**. No file moves, no schema changes, no data migration, the same
  SQL text executes against the same engine. This is categorically different from
  ADR-0108's crossing, which changes storage. The available proof technique is the
  one that already worked: ADR-0108's **T-4 byte-identity** test (landed, *zero
  divergence* across mixed-rate, storno, and modification invoices). A retrofit
  step that keeps T-4 green has not changed the filed artefact.
- **Blast radius is bounded by rule 14** — per family, per step, each landing on a
  gate-green base, each independently revertable, because no step changes storage.
- **The genuine hazard is a silent behaviour change during relocation** — a
  `.unwrap_or_default()` reintroduced, an error arm collapsed, a `NULL` handling
  difference. C10 and C11 exist for exactly this, and per-family byte-identity or
  differential tests should gate each step. **This is where a step will actually go
  wrong**, not in the design.
- **Concurrency with ADR-0108.** PR #53 is live and has landed money-path changes
  (the M-2 ÁFA rounding fix *moves published figures*). Any seam work must not
  collide. §6 is entirely about this.

---

## 6. Sequencing — the decision for Ervin

### Option A — finish ADR-0108's safe-track first, then seam-and-retrofit

Complete Steps 7–9 (inventory, work-orders, email; quoting; cutover prep) on the
current type-alias approach. Then build `aberp-storage`, the ports, the conformance
suite, and retrofit the completed SQLite path family by family.

**For:**
- **The seam's shape is an *output* of two working adapters, not an input.**
  ADR-0019 §1 says this in its own words: *"The trait shape is constrained by what
  **two** real backends need."* Under A, both adapters exist and the trait is
  **extracted** — mechanical and verifiable. Under B it is **designed** — judgment,
  against one working engine and one half-crossed one, which is the worst possible
  input.
- The retrofit becomes a **pure behaviour-preserving refactor** (§5.3) — a
  fundamentally lower-risk operation than a mid-flight pivot on a legal ledger.
- ADR-0108 is ~⅔ landed (Steps 1–4, 5's migrator half, 7A, 7B, T-8) with a working
  rollback (§6.2) and a live PR. Its remaining steps are known and scoped.
- Nothing landed is thrown away: the type alias (D2) becomes the adapter's import;
  `ensure_columns` becomes `aberp-storage`'s; the R1/R2/R3 rules become the codec;
  T-8's scanner keeps its job at 1/20th the scope.

**Against:**
- The remaining crossings (quoting is ~55 DDL sites; inventory + work-orders carry
  the §3.4 fold sites) get hand-translated **once more** — the very thing Ervin is
  objecting to.
- "Then" is where architectural work goes to die. §1.2 is the precedent: a seam
  scheduled after the urgent thing lasted one day.

### Option B — pivot now to seam-first

Pause ADR-0108 after the current step, build `aberp-storage` + ports + the
conformance suite against the DuckDB adapter, then let the seam carry the
remaining families across.

**For:**
- Purer. The remaining ~5 families cross once, through the seam.
- Ervin's driver taken at face value: stop hand-translating, build the thing that
  makes translation unnecessary.

**Against:**
- **Designs the trait against one-and-a-half engines** — the failure mode ADR-0019
  §1 explicitly warned about.
- **Doubles the blast radius per family.** Each remaining family would cross the
  engine seam *and* the repository seam in one commit, on the invoice/ÁFA/NAV
  path. Rule 14 is satisfied per family but the change per step is ~2×.
- **Mid-flight turn on a live PR with landed money-path changes.** PR #53 carries
  the M-2 rounding fix that moves published ÁFA figures. A pause here leaves the
  tree in a state where some families have crossed and some have not — ADR-0108's
  rollback (§6.2) is designed for that, but it is designed for a *pause*, not for a
  months-long parallel architecture programme.
- The seam does not make crossing #1 cheaper (§4.1). B pays the seam's full cost
  *and* the migration's remaining cost, in series, before either delivers.

### Recommendation: **A**, with one non-optional modification

**Take Option A.** The decisive argument is not schedule risk; it is that
**ADR-0019 §1's own rule says the trait must be shaped by two real backends, and
under B there is only one.** Building the seam now means guessing at the contract
and then discovering it — which is how we get a `StorageEngine` that lasts a day,
or a `BillingStore` that 46 concrete `DuckDbBillingStore` references route around.
Under A the contract is *read off* two working adapters. That is the difference
between extraction and invention, and extraction is the one that survives.

The modification, which is **part of the recommendation and not a third option**:

> **Each remaining ADR-0108 crossing puts its family's SQL behind a module-local
> port as part of the crossing commit** — trait + adapter + in-memory impl, no
> generic `aberp-storage` layer, no `Tx` type, no cross-family design. Just: this
> family's SQL now lives in one file behind one trait.

This is rule-14-shaped (one family, writers and readers together, one commit), and
rule-2-safe (each port has two consumers *immediately* — the DuckDB adapter that
backs rollback, and the SQLite one). It costs **~30–40% more per remaining step**
and it means the §5 retrofit starts from ~40% done instead of zero — which is what
turns "10–15 sessions" into "4–7".

**And the escape clause, because a modification that endangers the primary goal is
a bad modification (rule 3):** if it threatens Steps 7–9's gate-green cadence,
**drop it** and retrofit wholesale afterwards. The migration finishing is the
higher-priority objective. Say so in the step's PR body rather than letting it
lapse silently.

**What would change my recommendation to B.** If Ervin's roadmap moves the SaaS
lane (ADR-0059 / ADR-0100 — Postgres-per-tenant) inside ~6 months, then swap #2
arrives before A's retrofit would finish, the seam's payoff moves from swap #3 to
swap #2, and B's "pay it once" argument wins on arithmetic. **That is a roadmap
question I cannot answer from the tree, and it is the single input that flips
this.** ADR-0107 §4 makes the same conditional call in the same direction and
should be read alongside.

---

## 7. Consequences

**If accepted (A + the modification):**

- ADR-0019 §1 stops being aspirational. Its "never imports DuckDB types" clause
  becomes a gate (§4.3.3) that can go red, at a ratchet baseline of 74.
- Engine swap #2 (SQLite → Postgres, SaaS) costs one adapter directory plus a
  conformance run instead of a second full hand-translation.
- Rules 13 and 15 move from markdown into the type system (§3.2). Rule 14 stays —
  it is a *migration* rule, not an engine one.
- The fork-zero surface narrows 74 → ~15 files. The census and gates are
  **re-scoped, not deleted** (ADR-0108 Q9's reasoning holds: a gate deleted cannot
  protect the state you roll back to).
- The quantity-representation divergence (§1.5) gets a place to be decided.
- **Cost:** 6 600–10 200 net-new LOC and 4–7 extra sessions folded into ADR-0108's
  remaining steps, on top of ADR-0108's own remaining cost. Each remaining
  crossing step gets ~30–40% larger.
- **Locked in:** SQL lives in adapters, forever. Any future "just one quick query
  here" in a route handler is a gate violation. That is the point, and it is also
  a permanent tax on small changes — worth naming as a cost, not only a benefit.

**If rejected:** ADR-0019 §1 should be **amended to say what we actually do** —
dialect-portability discipline (`[[no-sql-specific]]`), not a repository seam.
Leaving an Accepted cornerstone ADR describing infrastructure that does not exist
is worse than either building it or retracting it, because the next engine
decision will be costed against the ADR rather than against the tree — which is
exactly what happened to ADR-0107 §3's `StorageEngine` citation.

---

## 8. Adversarial review — three concerns answered in advance

**1. "This is the speculative abstraction rules 2 and 12 forbid. You are proposing
a wrapper for a swap that may never happen."**
Partly conceded, and the concession is specific. It would be speculative if the
second consumer were hypothetical. It is not: ADR-0059 and ADR-0100 both name
Postgres-per-tenant for the SaaS lane, and ADR-0019 §1 names it as the second
backend the trait must be shaped by. Two named consumers, one of them already
in a phased plan. **But** the rule-12 objection lands squarely on §1.2's lesson,
and the answer must be structural rather than rhetorical: the seam ships with a
permanent second implementation (in-memory), an executable contract (the 17 pins),
and a ratchet gate — because a seam without those is exactly the thing S426 was
right to delete.

**2. "You are proposing a large refactor over a legally-binding tax ledger, on top
of an in-flight migration of the same ledger. The risk is not worth it."**
The risk is real and §5.3 does not minimise it. Three things bound it: the
retrofit changes **no storage** and is behaviour-preserving by construction; it is
per-family under rule 14 with each step independently revertable; and the proof
technique already exists and already returned zero divergence (T-4 byte-identity,
landed). The recommendation is also the *conservative* branch — A explicitly
refuses to turn the migration mid-flight, and the modification carries an escape
clause that subordinates the seam to the migration's cadence.

**3. "Your central evidence — that the seam decayed — proves the opposite of your
thesis. It shows this codebase does not sustain seams. Why will this one survive?"**
This is the strongest attack and it deserves the honest answer: **on the current
evidence, it would not.** `StorageEngine` lasted one day; `BillingStore` survives
but is routed around (46 concrete `DuckDbBillingStore` references vs 13 to the
trait, in `apps/` + `crates/`; and 922 of `duckdb_store.rs`'s 1 492 lines sit
*outside* the `impl BillingStore` block at `:923`). Both decayed for the same
reason: **nothing could measure the decay.**

To put the second one precisely, because it is the more instructive: the
`impl BillingStore` block spans `duckdb_store.rs:923–1082` — **160 lines of
1 492**. Roughly 89% of the "adapter" is inherent methods and DDL reachable
without touching the port at all. `BillingStore` is not a seam either; it is a
door in a wall with no other walls.

The seam is therefore not the proposal — the seam *plus its ratchet* is. If Ervin takes this ADR's design and
declines §4.3's gate, the honest prediction is that the seam is ~60% intact in six
months and cited as complete in the ADR after next. The gate is not a nice-to-have;
it is the only part of this proposal with a track record in this repository.

---

## 9. Deferral ledger (CLAUDE.md rule 3)

Found while grounding this ADR. **None fixed here** — this is a docs-only change.

| Item | Closed by |
|---|---|
| **ADR-0107 §3 (Option B, reason 5) cites `StorageEngine` / `DuckDbEngine` / `const STORAGE_ENGINE` as an existing engine-swap seam. It was deleted at `a1edbb0` (2026-06-15), one day after `ee56d2e` added it — six weeks before ADR-0107 was written.** It is also recorded as landed in the memory index (`project_aberp_s410_no_sql_specific_sweep`, step 4). The citation is one of six reasons for the B recommendation and its removal does not change that recommendation, but it overstates the "already paid for" argument. | A correction line in ADR-0107 §3 when it next lands (it is not on `main`), plus a memory correction. **Not fixed here** — ADR-0107 lives on `adr-db-engine-evaluation` and this ADR does not touch in-flight branches. |
| **`apps/aberp/src/products.rs:367` (`AND LOWER(name) = LOWER(?)`, the product-name dedup guard) and `:402` (`LOWER(...) LIKE ?`) carry the identical ASCII-fold / unescaped-`LIKE` hazard as `partners.rs:1001–1005` / `:1049`, and are OUTSIDE ADR-0108 M11's stated scope.** M11 names `partners.rs` only. On crossing, SQLite's ASCII-only `LOWER()` makes the products guard **admit** a duplicate that differs only in a Hungarian diacritic — the direction that does not announce itself, exactly as ADR-0108's M11 note warns for partners. **Exposure today is zero** (no SQLite connection runs these queries). | **The products family's crossing**, as its first commit, with a T-12-shaped pin — the same sequencing M11/T-12 got. Should be added to ADR-0108 §9 by whoever next touches it. A per-column sweep for `LOWER(` returns 8 src sites; **6 are partners, 2 are products, 0 elsewhere** — the sweep is complete, so this is the whole residue. |
| The `no-engine-types-outside-adapters` gate does not exist, so ADR-0019 §1's central clause has been unenforced since 2026-05-19. Baseline today would be **74**. | §4.3.3, with the seam. Could land *standalone and early* as a pure ratchet at 74 — it is ~400–700 LOC, changes no behaviour, and would immediately stop the count growing. **Cheapest single item in this ADR and the only one that pays off even if the seam is rejected.** |
| `apps/aberp/src/print_invoice.rs:922`'s comment asserts `information_schema` "is the portable path here". It is not portable to SQLite. | ADR-0108 §4.3 already schedules the rewrite (`sqlite_master`); the *comment* should go with it so the next reader does not re-derive the claim. |
| `material_inventory.rs:229–231` `DOUBLE` vs `V001__inventory.sql:53` `DECIMAL(18,6)` — two representations of one physical quantity (rule 7). Already in ADR-0108 §9 as out of scope. | Unchanged — out of scope. Re-recorded because §1.5 uses it as evidence and a reader should not infer this ADR closes it. |
| **R-5 is live in production today** (foreign `close` destroys later commits' durability, 13 in-serve routes). §3.4 narrows the *future* surface and **does not fix it**. | ADR-0108 §9's ruling stands: **its own PR, before anything else.** Recorded here so §3.4's "fork-zero becomes structural" is never read as licence to defer it. |

---

## 10. Open questions

| # | Question | Resolved by |
|---|---|---|
| **Q1** | **Does the SaaS lane (Postgres-per-tenant) land inside ~6 months?** This is the single input that flips the §6 recommendation from A to B. | Ervin / ADR-0100 phasing. **Blocking for the sequencing decision, not for the design.** |
| **Q2** | How many ports? §5.1 assumes ~15 from ADR-0108 §7's family decomposition; 25 puts the estimate near 14 000 LOC. | Measured in the first retrofit step, by decomposing one family for real. |
| **Q3** | Does the invoice↔ledger↔numbering fusion (ADR-0108's Step-5 "fused family") get one port or three composed through `Tx`? Three is cleaner and leans entirely on §3.2 being right. | The `Tx` design step, before any family moves. |
| **Q4** | Does `aberp-snapshot` get a port, and when? §3.5 says "only when a second engine implements it" — i.e. at ADR-0108's Phase 3, not before. | ADR-0108 cutover. |
| **Q5** | Do the in-memory adapters implement the *full* port or a subset? A subset weakens the §4.3 ratchet; the full port is ~1 200–1 800 LOC of test-only code. | First retrofit step; recommend full, and let §5.1's estimate carry it. |
| **Q6** | Should the §4.3.3 gate land **now**, standalone, ahead of any decision? It is behaviour-neutral, ~400–700 LOC, and pays off under every branch including rejection. **My recommendation is yes.** | Ervin, independently of §6. |
