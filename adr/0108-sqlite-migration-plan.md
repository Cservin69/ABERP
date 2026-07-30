# ADR-0108 — SQLite (WAL) migration: the executable DEV plan

- **Status:** **Accepted — GO-ready for execution.** This document authorises no
  engine code, no schema change, and no data migration by itself; it is the
  artefact a later execution session works from, one gated step at a time, and
  that session is now cleared to begin at Step 1.
  **Adversarially reviewed 2026-07-30 → verdict NO-GO pending four blockers
  (B1–B4) and seven must-fixes (F1–F7). All eleven are resolved in this
  revision; §14 is the self-audit, item by item, with the file:line evidence.**
  §13 is retained verbatim as the historical review record — it is the reasoning
  behind the changes, not an open list. Where §13 and the body disagree, the body
  is current.
- **Date:** 2026-07-30
- **Deciders:** Ervin
- **Depends on:** ADR-0107 / PR #47 (engine evaluation — recommends Option B),
  `docs/findings/sqlite-security-adversarial-20260730.md` / PR #49 (security gate
  — **ACCEPTABLE-AND-MITIGABLE**, conditional on M1–M12).
- **Related:** ADR-0019 (storage strategy, port traits, no FK / no CHECK / no
  engine-minted identity — the seam this plan stands on), ADR-0099 (H3/H4
  durability lane), ADR-0098 (opener census), ADR-0030 + ADR-0008 (audit mirror /
  hash chain), ADR-0009 (NAV issuing, 8-year retention), ADR-0037 (currency +
  MNB rate), ADR-0101 (per-line VAT rate-kind), ADR-0061/0062 (inventory / work
  orders), the memory pins `[[no-sql-specific]]`, `[[feedback_dev_db_disposable]]`,
  `[[feedback_customer_journey_e2e_gate]]`, `[[trust-code-not-operator]]`.
- **Execution scope:** the **DEV tenant only** (`test`,
  `apps/aberp-ui/aberp.duckdb`). Production is untouched by every step in §7.
  §11 states what a prod cutover would additionally require; it is **not
  authorised here**.

---

## 0. The four hard constraints this plan is built around

These are Ervin's, non-negotiable, and every step in §7 is shaped by them.

| # | Constraint | How the plan honours it |
|---|---|---|
| **C-I** | **Rollback-only.** Reversible at *every* step. DuckDB stays the source of truth until an explicit cutover that is not in this plan. | §6. The SQLite build **never opens `aberp.duckdb`**. It writes a different file (`aberp.sqlite`) built from a *read* of the DuckDB file. The DuckDB file is byte-unmodified for the whole exercise, so rollback is "stop the SQLite binary, rebuild default, start" — plus a pre-taken snapshot as belt-and-braces. |
| **C-II** | **DEV-only.** Prod untouched. | Every step is gated on `ABERP_DB` pointing inside `apps/aberp-ui/`. Step 1 lands a **refusal** (§7.1) that makes a SQLite-feature binary abort at boot if the resolved DB path is under `~/.aberp/`. Nothing in §7 reads, writes, or stats `~/.aberp/**`. |
| **C-III** | **Introduced behind a selector.** | §2.2 — a **compile-time cargo feature**, default OFF, plus the already-existing `ABERP_DB` path env var. Decision **D1**, with the alternative I rejected stated in full for the adversarial. |
| **C-IV** | **Single-command verified rollback.** | §6.2 — `run/rollback_to_duckdb.sh`, landed in **Step 1**, before any engine code. It restores, rebuilds, boots, and **verifies** (`verify_chain` genesis→head + row counts against the pre-migration manifest). It is tested by being *used* at the end of every step (§7 exit rule). |

**And the disposability lever.** The DEV DB is disposable
(`[[feedback_dev_db_disposable]]`). This plan uses that deliberately: the SQLite
side is **built by re-inserting rows read from DuckDB through the existing Rust
domain types**, not by any binary/file-level conversion. Where a family's data is
not worth carrying, §7 says so explicitly rather than writing a converter. See
§6.3 for the per-family carry/rebuild/drop decision — it is enumerated, not left
to the execution session.

---

## 1. Ground truth — what I measured, and where the two source documents are wrong

Every number below was re-derived at `b7d5c61` in this worktree. ADR-0107's counts
and PR #49's counts were **not** trusted; three of PR #49's own findings were
corrections to ADR-0107, and I found four more corrections below. **The
execution session should treat §1 as the census, not ADR-0107 §1.4 or PR #49's
baseline table.**

### 1.1 Corrections

| # | Claim in the source docs | Measured | Consequence |
|---|---|---|---|
| **G-1** | PR #49: "110 sites — **105** `ADD COLUMN IF NOT EXISTS` + **5** `ALTER COLUMN`" | `ADD COLUMN IF NOT EXISTS` = **110** total (**105** in `.rs` src, **5** in `.rs` tests). `ALTER COLUMN` = **0 executable** — all 5 hits are doc comments (`partners.rs:661`, `invoice_draft.rs:23`, `duckdb_store.rs:59,186,333`). | The "110" is right by coincidence and wrong by composition. There is no `ALTER COLUMN` work. |
| **G-2** | Both docs measure `--include='*.rs'` only. | **7 `.sql` migration files exist** and are `include_str!`-embedded and `execute_batch`-ed at boot: `aberp-inventory/migrations/V001`, `aberp-dispatch/V001`, `aberp-qa/V001`+`V002`, `aberp-work-orders/V001`+`V002`+`V003`. They carry **8 more executable `ADD COLUMN IF NOT EXISTS`** and **6 more `DECIMAL` columns including a money column** (`routings.est_cost_huf DECIMAL(18,2)`). | **This is the largest miss in both documents.** The true src DDL-rewrite count is **113**, not 105. The money census in §3 is incomplete without them. |
| **G-3** | Neither doc mentions `information_schema`. | **4 live src query sites** (`print_invoice.rs:926,986`, `quoting_materials.rs:1376`, `duckdb_store.rs:427`) + 1 test (`migration_pr73_old_schema.rs:98`). SQLite has **no `information_schema`**. | A separate, unnamed rewrite class. `duckdb_store.rs:427` is the S157 one-shot guard — if it silently returns "not integer" on SQLite the S157 ladder never runs. Fail-open shape. → **Step 4**. |
| **G-4** | Neither doc mentions `DROP COLUMN IF EXISTS`. | 2 executable sites (`duckdb_store.rs:357`, `quoting_materials.rs:132`) + 1 `RENAME COLUMN` (`duckdb_store.rs:358`). SQLite supports `DROP COLUMN` (≥3.35) and `RENAME COLUMN` (≥3.25) but **not `IF EXISTS` on `DROP COLUMN`**. | Small, but it is a hard parse error at boot, not a soft one. → **Step 4**. |
| **G-5** | ADR-0107 §3 B-cost-1: "money is already minor-unit integers". PR #49 F-6b corrects this for the quoting path. | Correct to correct it — and it is **worse than F-6b states**. Beyond `total_price_eur`, the `.sql` files add `qc_inspections`/`qc_inspection_plans` (6 `DOUBLE` measurement columns), and `invoice.huf_equivalent_total` is `DECIMAL(18,0)` on disk while its Rust type is already `i64`. | §3's table is the authoritative money census. |
| **G-6** | ADR-0107 §2 / §5: "**14** frozen in-serve read-forks". | `tools/adr0099_read_fork_structural_baseline.txt` holds **33** entries; its own header says **13** are live in-serve; a grep of the live/in-serve annotations returns **9**. The three numbers in the repo disagree. | Not load-bearing for this plan (the migration closes the *durability* half regardless) but it is a stale count in a frozen baseline, which is exactly the class of thing PR #43 was written to stop. → deferral ledger. |
| **G-7** | ADR-0107 §2 retirement table includes `db_writer_lock`. PR #49 F-7b says keep it. | **Keep it, and it needs no change at all.** `db_writer_lock::lock_path_for` keys the lock on `<parent-dir>/.aberp-db-writer.<tenant>.lock` — the **directory + tenant**, *not* the DB filename. So a DuckDB `serve` and a SQLite `serve` on tenant `test` in `apps/aberp-ui/` **already mutually exclude**. | A free, unplanned safety property for the reversible window: the two engines cannot both be live. Pin it with a test (§8, T-6). |

### 1.2 The census the execution session works from

| Probe | Count | Where |
|---|---:|---|
| `ADD COLUMN IF NOT EXISTS` — executable, src | **113** | 105 in 12 `.rs` files, 8 in 3 `.sql` files |
| — of which in `modules/billing/src/adapters/duckdb_store.rs` | 25 | the invoice family |
| `ADD COLUMN` built dynamically from a `const` list | 1 | `audit-ledger/src/storage/mod.rs:411` |
| `ADD COLUMN IF NOT EXISTS` in tests | 5 | `migration_pr73_old_schema.rs` ×3, `notes_migration.rs` ×2 |
| **Total DDL-add rewrite sites** | **114 src / 119 incl. tests** | |
| `ALTER COLUMN` | **0** | |
| `DROP COLUMN IF EXISTS` / `RENAME COLUMN` | 2 / 1 | |
| `information_schema` queries | **4 src + 1 test** | |
| `execute_batch` call sites (non-test) | 105 | the DDL delivery mechanism |
| `params!` call sites | 449 | all bound |
| `duckdb::Connection` in a signature | 120 | |
| `Connection::open(` | 227 | incl. tests |
| ADR-0098 frozen openers | 81 across 20 files | `adr0098_prod_opener_fingerprints.txt` |
| ADR-0099 frozen read-forks | 33 | `adr0099_read_fork_structural_baseline.txt` |
| `state.db.write()` / `.read()` call sites | 84 | the Handle seam's blast radius |
| `ON CONFLICT` — executable | **5** | The raw grep returns 21; **16 are doc comments and 1 is a test assertion string** (`quote_pricing_jobs.rs:3112`). This is `ALTER COLUMN`'s exact error (G-1), reproduced. The 5 real sites are `material_inventory.rs:555`, `supplier_prices.rs:470`, `quote_pricing_jobs.rs:415`+`:476`, `restore_from_nav_outgoing.rs:326`. All 5 conflict targets are already the declared `PRIMARY KEY` → **zero index work**. See §4.3. |
| `IS NOT DISTINCT FROM` | 8 | needs SQLite ≥ 3.39 |
| `LIKE` | 2 | unescaped metacharacters (M11) |
| `ATTACH` / `load_extension` / `CREATE TRIGGER` / `CREATE VIEW` / `WITH RECURSIVE` / `OVER (` | **0** | PR #49 confirmed |
| **SQL-side arithmetic on a money/quantity column** | **7** | §3.4 — the class neither source document names. The 7th is `aberp-inventory/src/repository.rs:549`, a `-` (subtraction) inside an `ORDER BY`. |
| **SQL-side `<` comparison on an R2 (TEXT-decimal) column** | **2** | `repository.rs:548` (`low_stock_products`) and `repository.rs:585` (`count_low_stock_products`, whose own doc comment says "same predicate"). The only Q2 lexicographic-ordering breaks in the tree; the second was found re-running the sweep for this revision and is **not** in §13's F1. §3.4. |
| **Read-only DuckDB opens** (`access_mode` / `read_only` / `READ_ONLY`) | **0** | Across `apps/`, `crates/`, `modules/`, non-test. Step 4's read-only open is capability **to build**, not to assume. §6.3, B4. |
| `.read()` / `.write()` split of the 84 Handle sites | **50 / 34** | §7 Step 3's two-axis audit (F7) is over the 50 `read()` sites; the 34 `write()` sites are the nesting context for axis (b). |
| `duckdb::Error::DuckDBFailure` | 3 | the only `duckdb::` path with **no** same-named rusqlite twin |
| DEV DB / mirror on disk | 20.4 MB / 1.3 MB, mode **0644** | confirms PR #49 F-5a |

---

## 2. Architecture — where SQLite slots in

### 2.1 The three layers, and which one moves

```
  domain / module code      Money(i64) · Decimal · VatRateKind · ULIDs      ← DOES NOT MOVE
  ───────────────────────────────────────────────────────────────────────
  storage adapters          duckdb_store.rs · repository.rs · storage/     ← types + DDL move
  ───────────────────────────────────────────────────────────────────────
  aberp_db::Handle          write() / read() / mirror lockstep             ← internals move,
                            db_writer_lock (unchanged, §1.1 G-7)              API does not
```

ADR-0019 already bought the top layer: no foreign keys, no engine-minted identity
(`CREATE SEQUENCE`/`nextval` deleted in S410), no CHECK constraints, portable ANSI
SQL, ULID identity minted in Rust. **Nothing in §7 may move an invariant into the
DDL** — the point of `[[no-sql-specific]]` is that the invariants stayed in Rust so
this migration is a type swap and not a semantics swap. The one apparent exception
— `STRICT` (M1) — is not an invariant moving into SQL; it is a *type* declaration
that makes a storage-class violation loud, which is the same job the DuckDB
`DECIMAL` declaration was doing (§3.1).

### 2.2 Decision D1 — the selector is **compile-time**, not runtime

**Chosen:** a workspace cargo feature `sqlite-engine`, **default OFF**, plus the
already-existing `ABERP_DB` path env var (`apps/aberp-ui/src/lib.rs:766`) which
selects `./aberp.duckdb` or `./aberp.sqlite`. Exactly one engine is linked into any
given binary.

**Rejected: a runtime selector with both engines linked.** It sounds more
reversible and is not. It requires the 449 `params!` sites, the 120
`duckdb::Connection` signatures, and the 84 `Handle` call sites to dispatch through
a trait object or an enum, because `duckdb::Connection` and `rusqlite::Connection`
are unrelated concrete types with unrelated `Row`, `Statement`, `Transaction`, and
`Error` types. That is a multi-thousand-line abstraction built speculatively
(CLAUDE.md rules 2 and 12) whose only consumer is a transition we intend to end —
and while it exists, every family is simultaneously reachable on two engines, which
is precisely the half-migrated shape rule 14 forbids. It would also link
`libduckdb` and `libsqlite3` into one bundle, on a workspace where the DuckDB
amalgamation has already twice overflowed CI's disk (`Cargo.toml:402`).

**The compile-time feature gives identical reversibility** because the reversibility
does not come from the selector — it comes from the fact that the two engines use
**two different files** and the SQLite build never opens the DuckDB one (§6.1).

> ⚠ **For the adversarial.** Ervin's wording was "config/feature selector"; I read
> that as satisfied by a cargo feature and chose the conservative engineering
> option. If he meant a runtime toggle he can flip without a rebuild, D1 is the
> decision to revisit — and the honest cost of that reading is roughly the
> abstraction layer described above. Flagged, not averaged.

### 2.3 Decision D2 — the type seam is a **type alias**, not a trait

`duckdb-rs` is a fork of `rusqlite`. The surfaces the tree uses are name-identical:
`Connection`, `Transaction`, `Statement`, `Row`, `Result`, `params!`, `prepare`,
`query_row`, `query_map`, `execute`, `execute_batch`, `transaction`, `try_clone`,
`ToSql`, `types::Type::Text`, `Error::QueryReturnedNoRows`,
`Error::FromSqlConversionFailure`.

So `aberp-db` grows a re-export module and the rest of the tree imports from it:

```rust
// crates/aberp-db/src/engine.rs — the ONLY place either engine crate is named.
#[cfg(not(feature = "sqlite-engine"))]
pub use duckdb::{params, types, Connection, Error, Params, Row, Statement, ToSql, Transaction, Result};
#[cfg(feature = "sqlite-engine")]
pub use rusqlite::{params, types, Connection, Error, Params, Row, Statement, ToSql, Transaction, Result};
```

The 449 `params!` sites and 120 signatures become a mechanical
`use duckdb::X` → `use aberp_db::engine::X` rewrite. **Measured divergences** —
the complete list, so the execution session is not surprised:

| Divergence | Sites | Handling |
|---|---:|---|
| `duckdb::Error::DuckDBFailure` → `rusqlite::Error::SqliteFailure` | **3** | The only variant with no twin. Wrap behind an `aberp_db::engine::is_engine_failure(&Error) -> bool` helper at all 3 sites. |
| `aberp_db::DbError::Duck(#[from] duckdb::Error)` | 1 | Becomes `#[from] engine::Error`. Variant renamed `Engine`. |
| `duckdb::Connection::open_in_memory()` | 2 + many tests | Same name in rusqlite. No change. |
| `Appender` API | **0** | Not used. |
| `savepoint` | **0** | Not used. |
| `MappedRows` | 1 | Same name. |

A trait abstraction over these would be a wrapper around a surface that is already
identical — rule 12's "should this exist at all" says no.

### 2.4 What is preserved, unchanged

- **`Handle::write()` / `Handle::read()` keep their exact signatures.** `write()`
  still returns a guard deref-ing to `&mut Connection`; `read()` still returns an
  owned connection. On SQLite `read()` becomes a genuine second connection rather
  than a `try_clone` — semantically *stronger* (it sees every prior commit) and
  API-identical. 84 call sites are untouched.
- **Single-writer.** The writer `Mutex` stays. It stops being a correctness
  requirement and becomes a throughput choice, and it is still what makes the
  `BEGIN IMMEDIATE` discipline (M5) cheap to reason about in-process.
- **`db_writer_lock` (F-E).** Unchanged, and per §1.1 G-7 it already spans both
  engines. **Not retired.** Its doc comment is re-scoped from "corruption guard" to
  "app-invariant guard" (M6) in Step 1.
- **The fsync'd mirror and the hash chain.** ADR-0030/0008 tamper-evidence is
  independently valuable and is the *rebuild source* for this migration (§6.3). Its
  **durability** role (preserve-and-refuse, torn-tail classifier, boot heal) is not
  retired in this plan either — it is the safety net during the crossing. Retiring
  it is a post-cutover decision, out of scope.
- **`aberp-snapshot`.** Untouched. `VACUUM INTO` replaces `EXPORT` only after
  cutover. During the reversible window the DuckDB snapshot machinery is what backs
  the rollback (§6.2).

### 2.5 Coexistence during the reversible window

```
apps/aberp-ui/
  aberp.duckdb                    ← source of truth. NEVER opened by a sqlite-engine build.
  aberp.duckdb.wal                ← DuckDB's OWN write-ahead log. Present whenever the DB
                                     was not cleanly closed; absent right now (clean close).
                                     Part of the DB's content, not a temp file.
  aberp.duckdb.audit.log          ← the mirror. READ by the migrator; never written by it.
  aberp.duckdb.audit.log.*.bak    ← the ADR-0030 preservation files (`.ahead-*`, `.healed-*`,
                                     `.devstale-*`). 9 `.bak` + 1 `.devstale-*` present on the
                                     DEV tenant today. The manifest must enumerate them.
  aberp.sqlite                    ← created by the migrator. Deleted by rollback.
  aberp.sqlite-wal  / -shm        ← WAL siblings. Deleted by rollback.
  aberp.sqlite.audit.log          ← the SQLite build's own mirror (mirror_path_for appends
                                     the suffix to the db path, so the two never collide).
  .aberp-db-writer.test.lock      ← shared by BOTH builds (dir+tenant keyed) → mutual exclusion.
  .aberp-premigration-<ts>/       ← the Step-2 snapshot + manifest. Rollback's restore source.
```

> **The `.wal` sidecar is the one artefact in this plan that can make DEV
> unrestorable, and §6.2 step 4 as originally written would have caused it.**
> "Restore `aberp.duckdb` from the snapshot dir" pairs a restored main file with
> whatever `aberp.duckdb.wal` happens to be on disk — a WAL from a *different*
> generation of the same file. DuckDB replays it on the next open. That is not a
> failed rollback, it is a corrupted one, and there is no second snapshot to go
> back to. **The snapshot captures `aberp.duckdb` and `aberp.duckdb.wal` as an
> atomic pair, and the restore writes both or neither** — never the main file
> alone, and never the main file with the WAL merely deleted (a WAL holding
> committed-but-unfolded transactions *is* part of the DB's content). §6.2 step 4
> and the Step-1 manifest both carry this; §6.2 step 8 adds the second snapshot
> behind it.
>
> **Second arm — the `.gitignore` gap, measured rather than assumed.** The
> repository is **public** and holds partner bank accounts, tax numbers and every
> invoice. Coverage today comes from four independent globs, not one `*.duckdb*`:
> `*.duckdb`, `*.duckdb.wal`, `*.duckdb-wal` (`.gitignore:50–52`), `*.log`
> (`:99`) and `*.bak` (`:111`). Verified with `git check-ignore` against every
> artefact class §7 will produce:
>
> | Artefact | Ignored today? | Matched by |
> |---|---|---|
> | `aberp.duckdb`, `aberp.duckdb.wal` | yes | `.gitignore:50,51` |
> | `aberp.duckdb.audit.log`, `aberp.sqlite.audit.log` | yes | `*.log` (`:99`) |
> | `aberp.duckdb.audit.log.*.bak` | yes | `*.bak` (`:111`) |
> | **`aberp.sqlite`** | **NO** | — |
> | **`aberp.sqlite-wal`, `aberp.sqlite-shm`** | **NO** | — |
> | **`.aberp-premigration-<ts>/`, `.aberp-rolledback-<ts>/`** | **NO** | — |
>
> So the gap is narrower than "nothing for `*.sqlite*`" and it is real: the SQLite
> main file, its two WAL siblings, and **both snapshot directories** — which hold
> a byte copy of the entire DuckDB DB and its mirror — are untracked *and
> unignored*. The snapshot directories are the more dangerous half. Three lines
> (`*.sqlite*`, `.aberp-premigration-*/`, `.aberp-rolledback-*/`), **Step 1,
> before the migrator or the snapshot script exists**, pinned by T-17.

**The engine is chosen at build time; the file is chosen by `ABERP_DB`; the two are
cross-checked at boot.** Step 1 lands a boot refusal: a `sqlite-engine` binary
whose resolved path does not end in `.sqlite` aborts, and a default binary whose
path does aborts. Fail loud, not fail open (rule 11). This is the mechanism that
makes "the DuckDB file is never opened by the SQLite build" a *checked* property
rather than a hope.

---

## 3. Phase 0.A — money, rate, quantity and hash representation

**This is the crown jewel and it is settled here.** PR #49's F-6a is the one
security *regression* the migration introduces, F-6b establishes the "money is
already integer" premise is false, and F-6c adds a BLOB/TEXT storage-class hazard
on the hash chain. All three are closed by the rules below.

### 3.1 The three representation rules

**R1 — Money is `INTEGER` minor units.** A monetary amount is an `i64` count of the
currency's minor unit (HUF: whole forints — HUF has no subunit in ABERP's model,
per `Huf(i64)`; EUR: cents, per `Eur(i64)`). Declared type `INTEGER` in a `STRICT`
table. The Rust type at the seam is `i64`. **No money column is ever `REAL`, ever
`TEXT`, and never `NUMERIC`/`DECIMAL`** (which `STRICT` forbids outright).

**R2 — Exact non-integer values (quantities, rates, tolerances) are `TEXT` holding
the canonical `rust_decimal::Decimal` string.** Declared type `TEXT` in a `STRICT`
table. The Rust type at the seam is `rust_decimal::Decimal`; the bind is
`d.to_string()`, the read is `Decimal::from_str(&s)`. This is **already exactly
what the code does today** (`duckdb_store.rs:777` "Decimal-as-string bind";
`:1287` `CAST(quantity AS VARCHAR)` → `Decimal::from_str`), which is why R2 is
the smallest-diff option and not merely the safest.

Scaled integers (µ-units at scale 6) were considered and rejected. They would
change `LineItem.quantity: Decimal`, `WorkOrder.qty_target: Decimal`,
`BomLine.qty_per_unit: Decimal`, `StockMovement.qty_delta: Decimal`, and
`RateMetadata.rate: Decimal` at every construction and every formatter, for a
column set that is never joined or arithmetic-ed in SQL after §3.4 lands. And
scaled integers introduce their own SQLite hazard: **`a*b` on INTEGERs that
overflows i64 silently converts to REAL** — the exact failure class we are
migrating to eliminate. Rejected on rules 2 and 12.

**R3 — Hashes and canonical payloads are `BLOB`, bound as `&[u8]`/`Vec<u8>`,
never as `&str`.** Declared type `BLOB` in a `STRICT` table. In SQLite, `BLOB` and
`TEXT` are distinct storage classes that **never compare equal**; a single `&str`
bind where `&[u8]` belongs makes a chain-link lookup return "not found" — the
symptom that already cost PR #40. `STRICT` + a `typeof()` assertion over every row
(M1's pin) is what closes it.

**And the rule that makes R1/R2 hold at runtime: no arithmetic on a money,
rate, or quantity column in SQL.** Not `SUM`, not `*`, not `+`, not `AVG`. Fold in
Rust over `Decimal` / `i64`. §3.4 enumerates the six sites; §8 T-8 is the cut-gate
grep that keeps it true.

### 3.2 The complete column census

Every money / rate / quantity / hash / measurement column in the tree, with its
declared type today, its Rust type, its SQLite target, and whether the migration
converts data. **Sources: `.rs` DDL and the 7 `.sql` migration files (§1.1 G-2).**

#### A — money, already `i64`: `BIGINT` → `INTEGER`. Representation unchanged.

| Table.column | Today | Rust | SQLite (STRICT) | Convert? |
|---|---|---|---|---|
| `invoice_line.unit_price` | `BIGINT` | `Money`→`i64` | `INTEGER NOT NULL` | no |
| `products.unit_price_minor` | `BIGINT` | `i64` | `INTEGER NOT NULL` | no |
| `ap_invoice.total_net_minor` / `total_vat_minor` / `total_gross_minor` | `BIGINT` | `i64` | `INTEGER NOT NULL` | no |
| `restored_invoice.total_net_minor` / `total_vat_minor` / `total_gross_minor` | `BIGINT` | `i64` | `INTEGER NOT NULL` | no |
| `purchase_order_lines.unit_price_minor` / `line_total_minor` | `BIGINT` | `i64` | `INTEGER NOT NULL` | no |
| `purchase_orders.subtotal_minor` / `vat_minor` / `total_minor` | `BIGINT` | `i64` | `INTEGER NOT NULL` | no |

> **Table names in this census are measured, not recalled.** An execution session
> greps §3.2 for a table name; a name that does not exist returns zero hits and
> reads as "already done", which is the silent-skip shape rule 11 exists to stop.
> Three names in the first draft did not exist in the tree and are corrected here
> and below: `purchase_order_lines` / `purchase_orders` (both plural, not
> singular — `apps/aberp/src/purchasing.rs`); `po_number_state` (§3.2 F, **not**
> `purchase_order_sequence`, which matches nothing anywhere in the tree);
> `quote_price_snapshots` (§3.2 D, **not** `supplier_prices` — `CREATE TABLE …
> supplier_prices` / `FROM supplier_prices` / `INTO supplier_prices` all return
> zero; `supplier_prices` is a *module* name, and the column it owns is declared
> at `apps/aberp/src/supplier_prices.rs:428`). **Every table name in §3.2 has been
> re-grepped against the tree.**

#### B — money declared `DECIMAL` but already `i64` in Rust: **converts to `INTEGER`**

| Table.column | Today | Rust | SQLite (STRICT) | Convert? |
|---|---|---|---|---|
| `invoice.huf_equivalent_total` | `DECIMAL(18,0)` | `RateMetadata.huf_equivalent_total: i64` | **`INTEGER`** | **YES** — bind changes from `i64::to_string()` (`duckdb_store.rs:~776`) to `i64` directly; read changes from string-parse to `r.get::<_, i64>()`. This is the single highest-value line in §3: it is the HUF figure that feeds the NAV filing and the printed invoice, and it is the one PR #49 F-6a names as silently becoming `0.14…`-class float. |
| `routings.est_cost_huf` | `DECIMAL(18,2)` | `Option<Decimal>` | **`TEXT`** (R2) | yes — see note |

> **`est_cost_huf` note (found in `V001__work_orders.sql:101`, named in neither
> source document).** It is money, in HUF, at scale 2 — but HUF has no subunit,
> and its Rust type is `Option<Decimal>`, not `Money`. Making it `INTEGER` minor
> units means deciding what HUF's minor unit is *for an estimate*, which is a
> product question. **Conservative call: R2 (`TEXT` decimal), not R1.** It is an
> operator estimate on a routing op; it never reaches NAV, the PDF, or the ledger
> totals. R2 keeps it exact and changes no Rust type. → flagged for the
> adversarial as the one money column that does not follow R1.

#### C — exact non-integer, `DECIMAL` → `TEXT` (R2). No Rust type change; bind/read already string.

| Table.column | Today | Rust | SQLite (STRICT) | Convert? |
|---|---|---|---|---|
| `invoice.exchange_rate` | `DECIMAL(18,6)` | `RateMetadata.rate: Decimal` | `TEXT` | representation-compatible; the migrator carries the canonical string verbatim |
| `invoice_line.quantity` | `DECIMAL(18,6)` | `LineItem.quantity: Decimal` | `TEXT NOT NULL` | ditto |
| `invoice_line.quantity_dec` | `DECIMAL(18,6)` | — (transient) | **must not exist post-migration** | The S157 widen ladder's scratch column (`MIGRATE_S157_SQL`, `duckdb_store.rs:355–358`: add `quantity_dec` → `UPDATE` copy → `DROP COLUMN IF EXISTS quantity` → `RENAME quantity_dec TO quantity`). Step 4 creates the invoice schema fresh with `quantity TEXT`, so the ladder must be **proved unreachable on SQLite**, not merely ported: SQLite refuses `DROP COLUMN` on an indexed/PK/UNIQUE column, so a ladder that *does* fire is a hard boot abort. Its trigger is `quantity_column_is_integer` (`duckdb_store.rs:423`), which reads `information_schema.columns.data_type` and `.ok()`s the error to `None` → `false`. On SQLite that query is a hard parse error, so the `.ok()` silently returns "not integer" and the ladder never fires — which happens to be the *correct* outcome for a freshly-created SQLite schema and the *wrong* mechanism for reaching it. §4.3 replaces the probe with `pragma_table_info` and makes the table-absent case an `Err`. |
| `work_orders.qty_target` | `DECIMAL(18,6)` | `Decimal` | `TEXT NOT NULL` | ditto |
| `boms.qty_per_unit` | `DECIMAL(18,6)` | `Decimal` | `TEXT NOT NULL` | ditto |
| `stock_movements.qty_delta` | `DECIMAL(18,6)` | `Decimal` | `TEXT NOT NULL` | ditto |
| `products.stock_qty` / `min_stock` | `DECIMAL(18,6)` | `Decimal` | `TEXT` | ditto |

> **Trailing-zero semantics change and it is deliberate.** DuckDB's
> `DECIMAL(18,6)` read-back renders `1.5` as `"1.500000"`; the formatters
> (`invoice-pdf/src/format.rs:183`, `nav_xml.rs:1611`) `.normalize()` those away.
> Under R2 the stored text is whatever `Decimal::to_string()` emitted at write, so
> a fresh row reads back `"1.5"` and a *migrated* row reads back `"1.500000"`.
> Both normalize to the same emitted bytes — **the normalize call is what makes
> this safe and it must not be removed.** §8 T-4 pins byte-identity of the NAV XML
> and the PDF across the migration for the whole DEV invoice set; that test is the
> reason this is a stated fact and not an assumption.

#### D — **money on a float today** (PR #49 F-6b; a pre-existing defect, widened by G-2)

`STRICT` will happily bless `REAL`. Carrying these across unchanged means the
migration faithfully preserves a float-money bug and M1 signs it off.

| Table.column | Today | Rust | Decision |
|---|---|---|---|
| `quote_pricing_jobs.total_price_eur` | `DOUBLE` | `Option<f64>` / `f64` | **`TEXT` (R2), Rust type → `Decimal`** |
| `quote_intake_log.total_price_eur` | `DOUBLE` | `f64` | **`TEXT` (R2), Rust type → `Decimal`** |
| `quote_price_snapshots.cost_per_kg_eur` (the table is `quote_price_snapshots`; the column is declared at `supplier_prices.rs:428`) | `DOUBLE` | `f64` | **`TEXT` (R2)** |
| `quoting_materials.cost_per_kg_eur` | `DOUBLE` | `f64` | **`TEXT` (R2)** |
| `quoting_parameters.cad_cam_rate_eur_per_hour`, `machining_rate_eur_per_minute` | `DOUBLE` | `f64` | **`TEXT` (R2)** |

**Scope call, stated plainly.** These are five money columns on the **quoting**
path. They do not reach NAV, the invoice PDF, or the audit chain — the quoting
path hands a price to the *operator*, who then issues an invoice through the
billing module where money is already `i64`. Fixing them is a real change to
`quote_pricing_pipeline` arithmetic and its calibration tests. **This plan
schedules them in Step 8 (the quoting family), not Step 5 (the invoice family),
and it does not let them ride as `REAL`.** If Step 8 proves larger than budgeted,
the correct fallback is to *stop* — leave the quoting family on DuckDB and keep
the reversible window open — not to migrate it as `REAL`. That is the rule-11 call
and it is written here so a later session cannot quietly take the easy branch.

#### E — non-money floats: `DOUBLE` → `REAL`, unchanged. Enumerated so the money census is provably complete.

`margin_profiles.gross_margin_pct`, `min_margin_pct`; `quote_pricing_jobs.margin_override_pct`,
`margin_floor_pct`; `quoting_machines.max_envelope_{x,y,z}_mm`, `daily_hours_avail`,
`buffer_pct`; `quoting_materials.density_g_cm3`, `machining_difficulty`,
`machinability_index`, `carbide_life_multiplier`, `quote_multiplier`;
`quoting_parameters.scrap_factor`, `profit_margin_base`, `overhead_factor`,
`min_margin`, `exotic_material_tax`, `setup_base_min`, `cad_cam_base_hours`,
`multiplier`, `base_time_minutes`, `setup_penalty_minutes`,
`inspection_minutes_per_feature`; `quote_calibration.estimated_minutes`,
`actual_minutes`; `material_inventory.on_hand_qty`, `reserved_qty`,
`committed_qty`, `consumed_qty`, `qty`; `work_orders.actual_machining_minutes`;
`qc_inspection_plans.nominal_value`, `upper_tol`, `lower_tol`;
`qc_inspections.nominal_value`, `upper_tol`, `lower_tol`, `actual_value`,
`deviation`.

> ⚠ **Two entries in E deserve the adversarial's attention.**
> (a) `material_inventory.*_qty` are `DOUBLE` while `stock_movements.qty_delta` is
> `DECIMAL` — **two representations of the same physical quantity in one product**
> (rule 7). Neither source document notices. This plan does **not** fix it (out of
> scope, rule 3) but records it in the deferral ledger, because migrating both
> as-is preserves the divergence under `STRICT`, which makes it look sanctioned.
> (b) `qc_inspections.deviation` is a *derived* float on a dimensional-inspection
> record used for a pass/fail verdict. It is not money. Keeping it `REAL` is the
> conservative no-change call, flagged.

#### F — integers and identity: `BIGINT`/`INTEGER` → `INTEGER`, unchanged

`invoice.sequence_number`, `invoice_sequence_state.next_number`,
`invoice_sequence_reservation.number`, `po_number_state.next_number`,
`invoice_line.vat_rate_basis_points`, `invoice.fiscal_year`,
`partners.issued_invoice_count`, `email_relay_queue.byte_size`,
`audit_ledger.seq`, `audit_ledger.time_mono`, `bom_revisions.rev_number`,
`bom_revisions.line_count`, `routings.sequence`, `routings.est_time_min`.

**`vat_rate_basis_points INTEGER` is why VAT never touches a float *in storage*** —
27% is `2700`, not `0.27`. The **storage** property is preserved verbatim, and
F-6a's storage-side float-coercion class cannot reach the VAT rate. F-6a reaches
`exchange_rate` and `huf_equivalent_total`, which is what §3.2 B and C close.

> ⚠ **The absolute form of this claim was false and is now narrowed to what
> measurement supports.** `apps/aberp/src/nav_xml.rs:1788` renders the value
> actually written to the NAV wire as
> `format!("{:.2}", vat_rate_basis_points as f64 / 10000.0)`. A full `f64` sweep
> of the emission reach set — `apps/aberp/src/nav_xml.rs`, `modules/billing/src`,
> `crates/invoice-pdf/src` — returns **exactly two hits, and one is a doc
> comment**: `nav_xml.rs:1788` (the live site) and `nav_xml.rs:2657` (a `///`
> line describing it). `modules/billing/src/domain/invoice.rs:21`'s hit is a doc
> comment stating the opposite policy. So the reach set contains **one** `f64`,
> and it is on the VAT *rate*, never on an amount.
>
> Three consequences, all settled here rather than left to the execution session:
>
> 1. **Not a filing defect.** `vat_rate_basis_points` is a `u16`
>    (`nav_xml.rs:1783`); for the finite set of legal Hungarian ÁFA rates
>    (0 / 500 / 1800 / 2700 bp) the `bp as f64 / 10000.0` → `{:.2}` round is
>    value-exact. Nothing filed to NAV is or was wrong. What was wrong was the
>    plan's invariant.
> 2. **It is a rule-7 fork.** The write path is `f64`; the inverse read path
>    `parse_vat_percentage_to_basis_points` (`nav_xml.rs:2658`) is exact
>    `Decimal::from_str_exact` × 10000 (`:2661–2663`). Two representations of one
>    value, one hop apart, in the same file — and the doc comment at `:2657`
>    names the `f64` it is the inverse of, so the fork is documented and
>    unnoticed rather than hidden.
> 3. **T-5(d) is redefined, not weakened** — see §3.3 and §8.

#### G — hash chain and payloads: `BLOB` → `BLOB` (R3)

`audit_ledger.prev_hash`, `binary_hash`, `entry_hash`, `payload`. Bound as
`Vec<u8>`. **M1's `typeof()` sweep must assert `'blob'` on all four for every
migrated row** — a `'text'` anywhere means the chain will not link.

#### H — declared types with no `STRICT` equivalent: mechanical renames

| Today | STRICT target | Notes |
|---|---|---|
| `VARCHAR` | `TEXT` | the bulk of the schema |
| `BIGINT` | `INTEGER` | SQLite INTEGER is 64-bit |
| `DOUBLE` | `REAL` | category E only |
| `BOOLEAN` (5 sites) | `INTEGER` | `rusqlite` binds `bool` ↔ `INTEGER` 0/1 natively |
| `DATE` (7 sites: `invoice.exchange_rate_date`, `payment_deadline`, `delivery_date`; `quote_intake_log.valid_until` ×2) | **`TEXT`** | already ISO-8601 `YYYY-MM-DD` on the wire, already read via `CAST(... AS VARCHAR)`. The `CAST` becomes a no-op and stays (harmless, and removing it is churn). |
| `DECIMAL(p,s)` | `TEXT` or `INTEGER` per §3.2 | `STRICT` **rejects** `DECIMAL` as a declared type — which is the point (PR #49 §6: it forces the decision rather than allowing deferral). |

### 3.3 How NAV XML and the invoice PDF consume it — no float, end to end

The trace, per value, from column to emitted byte:

| Value | Column | Read as | Domain type | Emitted by |
|---|---|---|---|---|
| line net / gross | `invoice_line.unit_price` INTEGER | `i64` | `Money::Huf/Eur` | `nav_xml`: integer→string; `invoice-pdf`: `format.rs` integer formatter |
| quantity | `invoice_line.quantity` TEXT | `String` | `Decimal::from_str` | `nav_xml`: `.normalize().to_string()`; PDF: same |
| VAT rate | `vat_rate_basis_points` INTEGER | `u16` | basis points | `nav_xml:1788`: **`bp as f64 / 10000.0`, formatted `{:.2}` — an `f64`.** Value-exact for 0/5/18/27. The one exception in this table; Step 5 converts it to `Decimal` and the allowlist empties. |
| exchange rate | `invoice.exchange_rate` TEXT | `String` | `Decimal::from_str` | printed invoice only (ADR-0037 §1.a) |
| HUF equivalent | `invoice.huf_equivalent_total` **INTEGER** | **`i64`** | `RateMetadata.huf_equivalent_total` | NAV wire + PDF |
| ledger hashes | `*_hash` BLOB | `Vec<u8>` | `EntryHash` | `verify_chain` |

**The claim, narrowed to what measurement supports.** The first draft of this
section read *"There is no point in this trace where an `f64` exists."* That is
false — the VAT-rate row above is an `f64` today. Two claims replace it. Both are
true against the tree at `b7d5c61`, and both are enforceable:

> **N-1 (the value claim).** No monetary *amount* and no *quantity* — no net, no
> gross, no VAT amount, no HUF equivalent, no line quantity — passes through an
> `f64` at any point between its column and its emitted byte. **Zero exceptions,
> today and after the migration.**
>
> **N-2 (the display claim, and its single exception).** A *percentage rendered
> for display* from an INTEGER basis-point count is permitted to use `f64`
> **only** where the rendering is value-exact over the closed set of values the
> column can hold. Exactly one site qualifies and it is named:
> `nav_xml.rs:1788`, over the four legal Hungarian ÁFA rates. **The allowlist has
> one entry and it may only shrink.**

The distinction is the whole point and it is not a hedge: N-1 is about a *value*
that must round-trip exactly at arbitrary precision, N-2 is about a *rendering* of
a value whose exact form is the integer that stays in storage. `vat_rate_basis_points`
never stops being `2700`; only its printed shape is derived. That is why F-6a's
storage-side float-coercion class cannot reach the VAT rate (§3.2 F), and why
narrowing here costs nothing: the rate the ÁFA arithmetic actually uses is the
integer, not the rendering.

**T-5(d) enforces N-1 with zero allowlist; T-5(e) enforces N-2 as a
one-entry ratchet** (§8). Neither is the "grep for `f64` and hope" gate the first
draft specified, which would have gone red on day one and whose cheapest repair
would have been to delete it — PR #43's name-vs-shape lesson in its other
direction.

**Conservative call, marked.** Step 5 converts `write_vat_rate_choice` to
`Decimal` (≈3 lines: `Decimal::from(bp) / Decimal::from(10000)`, `{:.2}` via
`round_dp(2)`), which makes N-2's allowlist **empty** and closes the rule-7 fork
against the exact `Decimal::from_str_exact` parse at `:2658`. This is the
conservative branch because the alternative — a permanent one-site exemption —
survives only by keeping a gate weaker than the property it names, and a
permanent allowlist entry is how "temporary" becomes "sanctioned" (§3.2 E's
`material_inventory` divergence is the same mechanism, one product-cycle later).
The value written to NAV is byte-identical either way; T-4's byte-identity pin
over the whole DEV invoice set is what proves that rather than asserts it.

The round-half-even HUF
conversion (`huf_equivalent_round_half_even`, ADR-0037 §1.c / C11) already runs on
`rust_decimal::Decimal` and lands on `i64`; §3.2 B removes the last string↔decimal
round-trip that stood between that `i64` and the column. The property test T-5
(§8) is what makes this claim falsifiable rather than asserted.

### 3.4 The **seven** SQL-side arithmetic sites that must move to Rust — and the one comparison

| Site | Statement | Why it breaks | Fix |
|---|---|---|---|
| `apps/aberp/src/reports.rs:800` | `CAST(SUM(CAST(il.quantity AS DECIMAL(38,6)) * il.unit_price) AS VARCHAR)` | **The sharp one.** Under R2 `quantity` is `TEXT`; SQLite coerces `TEXT * INTEGER` to `REAL` and the report silently becomes float money. | Select `quantity, unit_price` per row; fold in Rust with `Money::checked_mul_decimal` (already exists, `money.rs:54`) + the existing `decimal_str_to_i64` round-half-even (`reports.rs:1011`). |
| `apps/aberp/src/reports.rs:861` | `CAST(COALESCE(SUM(i.huf_equivalent_total), 0) AS VARCHAR)` | Under §3.2 B the column is `INTEGER`; `SUM` over INTEGER is exact but **raises on i64 overflow** and the `CAST … AS VARCHAR` round-trip is now pointless. | `SELECT huf_equivalent_total` and `checked_add` in Rust; loud on overflow. |
| `aberp-inventory/src/repository.rs:222` | `CAST(COALESCE(SUM(qty_delta),0) AS VARCHAR)` (cache rebuild, in-tx) | `qty_delta` becomes `TEXT` → `SUM` coerces to `REAL`. **This is the stock-cache invariant** `stock_qty = SUM(qty_delta)`. | Select the column, fold `Decimal` in Rust. |
| `aberp-inventory/src/repository.rs:629` | same, batch rebuild | same | same |
| `aberp-inventory/src/bin/rebuild_stock_cache.rs:29` | same, CLI one-shot | same | same |
| **`aberp-inventory/src/repository.rs:548–549`** (`low_stock_products`) — **site 7, and the tree's only Q2 break** | `WHERE COALESCE(stock_qty,0) < COALESCE(min_stock,0)` (`:548`)<br>`ORDER BY (COALESCE(stock_qty,0) - COALESCE(min_stock,0)) ASC, name ASC` (`:549`) | **Two distinct breaks in one statement.** (a) *The comparison* (`:548`). Both columns are R2/`TEXT` after migration, so `COALESCE(col, 0)` yields `TEXT` when the column is present and `INTEGER 0` when it is `NULL`. `TEXT < TEXT` is **lexicographic**: stock `'9'` vs min `'10'` compares `'9' > '1…'` → **FALSE → the low-stock product is silently not flagged.** And where one side is `NULL`→`INTEGER 0`, SQLite's storage-class ordering places INTEGER before TEXT *unconditionally*, so `0 < '<any text>'` is always TRUE. (b) *The ordering* (`:549`). `TEXT - TEXT` forces REAL coercion → **float arithmetic on a quantity**, exactly R1/R2's target class. | Select `stock_qty`, `min_stock` as `TEXT`; do **both** the `<` filter and the deficit ordering in Rust over `Decimal` — the function already parses both columns into `Decimal` from the `CAST(… AS VARCHAR)` projections at `:542–543`, so the fold has no new dependency and no new query. Lands with inventory in **Step 7**. |
| **`aberp-inventory/src/repository.rs:585`** (the low-stock *count*) | `WHERE COALESCE(stock_qty,0) < COALESCE(min_stock,0)` | The **same** lexicographic comparison, in a second statement 36 lines below the first, reached by a different caller. Found by the same per-column sweep; it would have been missed by a fix scoped to `low_stock_products` alone. | Same fold, or delete the query and count the folded rows. **Step 7, same commit.** |
| `reports.rs` `MAX(...)` / `COUNT(*)` sites | — | unaffected (no money arithmetic) | none |

**This work is not optional and not deferrable to a cleanup phase.** Three of the
sites are the inventory cache-rebuild path, which is a *write* — a float there
writes a wrong `stock_qty` back into the products cache. They land in the same
step as their family (Steps 5 and 7).

> **The fold move must also kill the fail-open beside it — and that fail-open is
> live in production today.**
> `apps/aberp/src/reports.rs:872` reads the EUR→HUF aggregate back through
> `decimal_str_to_i64(&s).unwrap_or(0)`. If the aggregate string does not parse,
> the ÁFA report silently prints **0 HUF** instead of failing. This is **not**
> introduced by the migration: it is running on DuckDB right now, on the ÁFA
> report, in the plan's own worst class (rule 11). What the migration adds is a
> *new way to trigger it* — a `REAL`-rendered `SUM` under R2 produces a string
> shape `decimal_str_to_i64` may reject — which makes it the mechanism by which a
> missed §3.4 fold would read as a working report.
>
> **Disposition, stated rather than folded in silently.** It is a **pre-existing
> defect**, and it is recorded as one in §9. It is fixed **in-migration, in the
> same Step-5 commit as the `reports.rs:861` fold**, not as a separate cut. The
> reason is specific and not convenience: the fix is *deleting* the
> `unwrap_or(0)` and letting the new Rust fold's `Result` propagate — the fold
> and the fail-open are the same three lines, and splitting them would mean
> writing the fold, leaving the swallow in place for one PR, and relying on the
> next PR to remove it. Splitting a fail-open away from the code that makes it
> reachable is how it survives. **The Step-5 PR body must name it as a
> pre-existing production defect being closed, so it is not miscounted as
> migration collateral.** Two sibling `unwrap_or(0)`s exist in the same file
> (`:827` on the net-total path, `:1279`); both are swept in the same commit and
> the sweep is stated in the PR body — a fix scoped to `:872` alone would leave
> the identical shape two functions away.

> **This closes Q2, in the plan, not in a future step.** A per-column sweep of
> every `ORDER BY`, `MIN`/`MAX`, `<`/`>`/`<=`/`>=`, and `BETWEEN` over all ten
> R2 (TEXT-decimal) columns — `exchange_rate`, `quantity`, `qty_target`,
> `qty_per_unit`, `qty_delta`, `stock_qty`, `min_stock`, `est_cost_huf`,
> `total_price_eur`, `cost_per_kg_eur` — returns **exactly two hits in the whole
> tree: `repository.rs:548` and `repository.rs:585`,** both the same predicate on
> the same two columns. Every other comparison on these values
> (`repository.rs:449`, `:502`; `work-orders/repository.rs:232`, `:699`) is
> already in Rust over `Decimal`.
>
> Q2's mitigation is therefore **done here**, not deferred to "check every
> `ORDER BY` before Step 5". Two lessons are worth keeping, because both are
> about the shape of the deferral rather than its content: the original wording
> said *`ORDER BY`* only and would have missed the `WHERE` — the half that
> actually returns wrong rows; and the first sweep found one site where there are
> two, because it stopped at the function §13 named instead of at the predicate.
> **A sweep is per-column or it is nothing.**

---

## 4. Phase 0.B — the 114 DDL rewrite sites

### 4.1 The pattern, exactly

Every `ALTER TABLE t ADD COLUMN IF NOT EXISTS c TYPE;` becomes a call to one shared
helper in `aberp-db`:

```rust
/// The ONLY way a column is added on SQLite. Identifiers come from `&'static str`
/// arguments — never from a value, never from a format! of runtime data.
pub fn ensure_columns(
    conn: &Connection,
    table: &'static str,
    cols: &'static [(&'static str, &'static str)],   // (name, declared_type)
) -> Result<(), DbError>
```

Its contract, in order:

1. Read the existing column set once: `SELECT name FROM pragma_table_info(?)` with
   `table` **bound as a value**, not interpolated. (`pragma_table_info` is
   table-valued and takes the name as a parameter — so the table identifier is
   bound too. Only the `ALTER` in step 3 interpolates, and only from `&'static str`.)
2. If the table itself does not exist → **return `Err`**. Not `Ok(())`. A missing
   table at `ensure_schema` time is a broken boot, and the declarative
   `IF NOT EXISTS` form could never express "silently skip".
3. For each `(name, ty)` not present: `ALTER TABLE {table} ADD COLUMN {name} {ty};`
   — the format string's three holes are all `&'static str` from the `const` table.
4. **Re-read `pragma_table_info` and assert every requested column is now present.
   If any is absent → `Err`.** This is M8's fail-loud post-condition and it is the
   whole reason the helper exists: PR #49 F-1c identifies this rewrite as
   reproducing D2a's exact fail-open shape (a column silently not added → a later
   read `.unwrap_or_default()`s → a guard passes vacuously → an exempt ÁFA base
   re-files to NAV at 0%). Step 4 without step 4's post-condition is the defect.
5. On any error, the message names table, column, and declared type. Rule 11.

**The identifier rule, stated as a checkable invariant:** *no `ensure_columns`
call site may pass anything but a `const`.* The `cols` parameter is
`&'static [(&'static str, &'static str)]`, so the type system enforces it — a
runtime `String` will not compile. That is stronger than a grep and it is why the
signature is shaped this way rather than taking `&[(String, String)]`.

### 4.2 The site inventory

| File | Sites | Family | Step |
|---|---:|---|---|
| `modules/billing/src/adapters/duckdb_store.rs` | 25 | invoice | 5 |
| `crates/aberp-quote-intake/src/log_table.rs` | 17 | quoting | 8 |
| `apps/aberp/src/quote_intake_query.rs` | 15 | quoting | 8 |
| `apps/aberp/src/partners.rs` | 12 | partners | 7 |
| `apps/aberp/src/quote_pricing_jobs.rs` | 10 | quoting | 8 |
| `apps/aberp/src/quoting_tunables.rs` | 7 | quoting | 8 |
| `apps/aberp/src/quoting_materials.rs` | 6 (+1 `DROP COLUMN IF EXISTS`) | quoting | 8 |
| `apps/aberp/src/material_inventory.rs` | 5 | inventory | 7 |
| `apps/aberp/src/restore_from_nav_outgoing.rs` | 4 | invoice | 5 |
| `apps/aberp/src/invoice_draft.rs` | 2 | invoice | 5 |
| `apps/aberp/src/serve.rs` | 1 | boot | 5 |
| `apps/aberp/src/email_relay_queue.rs` | 1 | email | 7 |
| `crates/aberp-inventory/migrations/V001__inventory.sql` | **4** | inventory | 7 |
| `crates/aberp-work-orders/migrations/V002__calibration_link.sql` | **2** | work orders | 7 |
| `crates/aberp-work-orders/migrations/V003__bom_revisions.sql` | **2** | work orders | 7 |
| `crates/audit-ledger/src/storage/mod.rs:411` | 1 (already dynamic, const-driven) | ledger | 5 |
| **src total** | **114** | | |
| tests (`migration_pr73_old_schema.rs` ×3, `notes_migration.rs` ×2) | 5 | | with their family |

**The `.sql` files need a delivery decision** (they are `include_str!` +
`execute_batch`, so they cannot call a Rust helper). Conservative call: **split
each `.sql` file into a `CREATE`-only part that stays SQL, and move its `ALTER …
ADD COLUMN` lines into an `ensure_columns` call in the crate's `ensure_schema`.**
8 lines move. The alternative — a mini-parser that rewrites the `.sql` at load
time — is a parser we would own forever (rule 12). Flagged for the adversarial.

### 4.3 The other DDL-shaped rewrites (§1.1 G-3, G-4)

| Item | Sites | Rewrite |
|---|---:|---|
| `information_schema.columns` → `pragma_table_info` | `duckdb_store.rs:427` (S157 guard), `quoting_materials.rs:1376` | Use `ensure_columns`' own probe. **`duckdb_store.rs:427` must fail loud on "table absent", not return `false`** — a silent `false` means the S157 quantity widen never runs and quantities truncate. |
| `information_schema.tables` → `sqlite_master` | `print_invoice.rs:926,986` | `SELECT count(*) FROM sqlite_master WHERE type='table' AND name='invoice'` |
| `DROP COLUMN IF EXISTS` | `duckdb_store.rs:357`, `quoting_materials.rs:132` | Guard on `pragma_table_info` then bare `DROP COLUMN` |
| `RENAME COLUMN` | `duckdb_store.rs:358` | Supported ≥3.25. No change. |
| `ON CONFLICT` — **5**, not 21 | **5** | **The audit is done, and it is empty work.** The 21 was a raw grep over comments — G-1's exact error, reproduced two rows below the correction that names it: **16 are doc comments and 1 is a test assertion string** (`quote_pricing_jobs.rs:3112`). All 5 executable sites are the same shape — `INSERT INTO t (…) VALUES (…) ON CONFLICT (<cols>) DO NOTHING` — and in **every one** `<cols>` is *exactly* the table's already-declared `PRIMARY KEY`, verified statement-by-statement against the DDL: |
| | | • `material_inventory.rs:555` → `inventory_balances`, PK `(tenant_id, material_grade)` at `material_inventory.rs:235` |
| | | • `supplier_prices.rs:470` → `quote_price_snapshots`, PK `(tenant_id, price_set_hash, grade)` at `supplier_prices.rs:429` |
| | | • `quote_pricing_jobs.rs:415` and `:476` → `quote_pricing_jobs`, PK `quote_id` at `quote_pricing_jobs.rs:248` |
| | | • `restore_from_nav_outgoing.rs:326` → `restore_lock`, PK `tenant_id` at `restore_from_nav_outgoing.rs:270` |
| | | SQLite resolves an upsert conflict target against a `PRIMARY KEY`'s implicit unique index exactly as DuckDB does. **Zero `UNIQUE` indexes to add. Zero rewrites. No `SELECT`-then-write. No new constraint — so the `[[no-sql-specific]]` / §2.1 tension Q5 was built around does not exist.** Some of the five branch on the affected-row count as an idempotency signal; SQLite's `changes()` returns 0 for a skipped upsert row, same as DuckDB — **pin it, don't re-derive it.** Step 3's obligation is **5 confirmation tests and no rewrite work**. |
| `IS NOT DISTINCT FROM` | 8 | Supported ≥3.39; M12 pins the floor at 3.51.3 anyway. No rewrite. |
| `CREATE INDEX IF NOT EXISTS` | many | Supported. No change. |
| `PRIMARY KEY` on `STRICT` tables | many | Supported; note `INTEGER PRIMARY KEY` aliases rowid — none of ABERP's PKs are integer (all ULID `TEXT`), so no behaviour change. |

---

## 5. The twelve mitigations as exit conditions

Every one is a **Phase-0 exit condition**, every one is pinned by a
**mutation-verified** test (the pin must be shown to go red when the mitigation is
removed — ADR-0107 §4.1's rule, applied to security as PR #49 requires), and every
one lands in the step named below. **M1, M5 and M6 may not be deferred past Step 5**
(the invoice/ledger family) — PR #49's gate answer is explicit on this.

| # | Mitigation | Lands in | Pin (test id in §8) |
|---|---|---|---|
| **M1** | `STRICT` on every table; declared types restricted to `INTEGER`/`TEXT`/`BLOB`/`REAL`; **no `DECIMAL`, no `NUMERIC`, no `REAL` on any money path** (§3) | Step 3 (helper + first family), enforced from Step 5 | T-1 float-reject, T-2 `typeof()` sweep |
| **M2** | `SQLITE_OMIT_LOAD_EXTENSION` in the bundled build; `rusqlite`'s `load_extension` feature never enabled; `sqlite3_db_config(ENABLE_LOAD_EXTENSION, 0)` at open | Step 2 | T-3a + cut-gate grep over every `Cargo.toml` |
| **M3** | `sqlite3_limit(SQLITE_LIMIT_ATTACHED, 0)` at open | Step 2 | T-3b + cut-gate grep for the `ATTACH` token |
| **M4** | `SQLITE_DBCONFIG_DEFENSIVE=1`, `ENABLE_TRIGGER=0`, `ENABLE_VIEW=0`, `PRAGMA trusted_schema=OFF` | Step 2 | T-3c (`CREATE TRIGGER`/`CREATE VIEW` rejected on the live handle) |
| **M5** | **`BEGIN IMMEDIATE` for every read-modify-write** — audit-chain append, invoice-number allocator, every upsert, the stock-cache rebuild | Step 3 (default in the `Handle`), audited per family | T-6 two-connection interleave must not produce two links off one `prev_hash` |
| **M6** | **Keep the F-E writer flock.** Re-scope its doc comment to "app-invariant guard". Per §1.1 G-7 it already spans both engines unchanged. | Step 1 (doc) + Step 2 (test) | T-7 existing `db_writer_lock_e2e` re-pointed; plus a cross-engine refusal test |
| **M7** | `journal_mode=WAL`, `synchronous=FULL`, `fullfsync=1`, explicit finite `busy_timeout`, **`shared_cache` explicitly OFF** | Step 2 | T-3d reads each pragma back and asserts the value; mutation-verified |
| **M8** | Const-driven DDL with a **fail-loud post-condition** (§4.1) | Step 3 | T-9 seeds a pre-migration schema, asserts every expected column exists after `ensure_schema`, and asserts an `Err` when one cannot be added |
| **M9** | `0600` on the DB **and its `-wal` / `-shm` siblings**; `0700` on the tenant dir | Step 2 | T-10 stats all three after a fresh open. (Also true for DuckDB today — DEV DB measured `0644`. See deferral ledger.) |
| **M10** | `rusqlite` with **bundled** `libsqlite3-sys`, floor ≥ **3.51.3**; add `libsqlite3-sys` to the existing `cargo-deny`/`cargo-audit` gate; **no ignore entry** | Step 2 | `cargo deny check` (exists in CI) + T-11 `sqlite3_libversion_number() >= 3051003` |
| **M11** | Escape `%`/`_`/`\` in the 2 `LIKE` patterns + `ESCAPE '\'`; replace SQL `LOWER()` with Rust `to_lowercase()` on **both** sides — SQLite's `LOWER()` is ASCII-only and `partners.rs:1001–1005` uses it as the **duplicate-partner guard** | Step 7 (partners family) | T-12 `Árvíztűrő` / `ÁRVÍZTŰRŐ` dedup still matches; a `%` needle does not over-match |
| **M12** | Bundled SQLite ≥ 3.39 for `IS NOT DISTINCT FROM` (8 sites); the **5** executable `ON CONFLICT` sites each confirmed to resolve against the table's declared `PRIMARY KEY` (§4.3 — the audit is complete; the obligation is confirmation, not rewrite) | Step 3 | T-11 (version) + 5 confirmation tests, one per site, each asserting the upsert is a no-op on a duplicate and that `changes()` reports 0 |

**Plus the standing prohibition PR #49 §8 adds:** never use `rusqlite`'s
`create_scalar_function` / `create_aggregate_function` / `create_window_function` /
`commit_hook` / `rollback_hook` / `update_hook` / `VTab` APIs (RUSTSEC-2021-0128,
CVE-2020-35866). They are attractive during exactly this migration — a custom
collation to replace `LOWER()`, an `update_hook` to feed the audit ledger — and M11
routes case-folding through Rust precisely so the temptation has an answer. Add the
symbol list to the cut-gate grep in Step 2.

**And the ratchet PR #49 §2 records:** ADR-0107 §4 rec. 6's future
"DuckDB reads the SQLite file via `sqlite_scanner`" must never be implemented by
enabling extensions inside `serve`. Out of scope here; recorded so Phase 4 does not
unwind M2.

---

## 6. Reversibility — the mechanism

### 6.1 Why rollback is cheap: the DuckDB file is never written

The whole reversibility argument reduces to one enforced property:

> **A `sqlite-engine` build never opens `aberp.duckdb`.** It opens `aberp.sqlite`.
> The migrator (Step 4) opens the DuckDB file **read-only, in a separate one-shot
> process**, and the SQLite file it produces is a fresh file.

So the DuckDB file at the end of Step 9 is byte-identical to the DuckDB file at the
start of Step 2. Rollback does not *restore* it in the normal case — it was never
touched. The snapshot exists for the abnormal case (a mis-run migrator, a wrong
`ABERP_DB`, an operator mistake) and because "we have a snapshot" is cheaper than
"we reasoned that we don't need one".

The property is **enforced, not assumed**, by the Step-1 boot refusal (§2.5): a
`sqlite-engine` binary with a resolved path not ending `.sqlite` aborts before
opening anything. Mutation-verify that refusal (T-13) — a refusal no test can red
is not a refusal.

### 6.2 `run/rollback_to_duckdb.sh` — landed in Step 1, before any engine code

Single command. Idempotent. Verifies. Refuses on anything unexpected.

```
run/rollback_to_duckdb.sh [--from <snapshot-dir>]
```

1. **Refuse** if `ABERP_DB` resolves outside `apps/aberp-ui/` or anywhere under
   `~/.aberp/` (C-II). Refuse if a writer holds `.aberp-db-writer.test.lock`
   (something is still running) — do not force it.
2. Stop the DEV app; wait for the lock to clear.
3. Move `aberp.sqlite`, `-wal`, `-shm`, `aberp.sqlite.audit.log` into
   `.aberp-rolledback-<ts>/` (**move, never delete** — rule 11; a deleted artefact
   cannot be post-mortemed).
4. **Take a second snapshot first.** Before restoring anything, copy the *current*
   on-disk state (`aberp.duckdb`, `aberp.duckdb.wal`, the mirror, every
   `.audit.log.*.bak`) into `.aberp-rolledback-<ts>/pre-restore/`. This is the
   snapshot behind the snapshot: a restore that goes wrong is otherwise
   unrecoverable, because step 5 overwrites the only other copy. It costs one
   `cp -c` of a 20 MB file.
5. If `--from` is given, or if `aberp.duckdb`'s digest does not match the
   pre-migration manifest, restore from the snapshot dir **as an atomic set —
   `aberp.duckdb` *and* `aberp.duckdb.wal` *and* `aberp.duckdb.audit.log` *and*
   every `aberp.duckdb.audit.log.*.bak` preservation file, all or none.** Restore
   into a staging dir, verify every digest against the manifest, then move the set
   into place; a partial move is a failed restore, not a corrupted one.
   **Never the main file alone.** Restoring `aberp.duckdb` beside a
   foreign-generation `aberp.duckdb.wal` does not fail the rollback — DuckDB
   replays the WAL on the next open and *corrupts* it. That is the one failure in
   this plan with nothing behind it, which is why step 4 exists.
   **And never the main file with the WAL merely deleted:** a WAL holding
   committed-but-unfolded transactions is part of the DB's content, so deleting it
   silently discards committed data. If the manifest recorded no `.wal` (the DB
   was cleanly closed at snapshot time — which is the state on disk today) and one
   is present now, it is **moved into `.aberp-rolledback-<ts>/`, never deleted** —
   same rule-11 reasoning as step 3.
6. `cargo build` **without** `--features sqlite-engine` (the default).
7. **Verify, and this is the part that makes it "verified rollback":**
   - `aberp verify-chain` genesis→head on the restored DuckDB DB — must be `OK`;
   - per-table row counts equal the pre-migration manifest;
   - the head `seq` equals the manifest's;
   - the mirror's last `entry_hash` equals the DB head's;
   - **the count of `audit_ledger` rows with a non-NULL `event_sig`, and the
     `audit_ledger_anchors` row count, equal the manifest's** — the same two
     numbers §6.3's reconciliation gate turns on, checked in the other direction
     so a rollback cannot restore a tamper-evidence-stripped DB and report PASS.
8. Print a one-line PASS/FAIL. **Non-zero exit on any mismatch.** Never "restored
   successfully" with a count off.

**It is tested by being used.** §7's exit rule: *every step ends by running
`rollback_to_duckdb.sh` and confirming a green DEV boot on DuckDB, then
re-applying the step.* A rollback path exercised once at the end is a rollback path
that has never been exercised.

### 6.3 What data crosses, and how — per family

The DEV DB is disposable, so this plan **rebuilds rather than converts** wherever
rebuilding is cleaner, and says which is which. No family uses file-level or
binary conversion; every row that crosses does so **through the existing Rust
domain types**, which is what makes §3's representation change a typed
transformation rather than a cast.

| Family | Method | Why |
|---|---|---|
| **`audit_ledger`** | **Row-by-row carry from the DuckDB table. The table is the source of truth; the mirror is a cross-check arm, never the source.** | See the inversion argument below. |
| **`audit_ledger_anchors`** | **Row-by-row carry.** | The S441 / ADR-0087 qualified-timestamp anchors (`crates/audit-ledger/src/session/anchors.rs:32`). Not in the mirror, and named in neither the first draft's carry table nor its §3.2 census — **carried unnamed is carried not at all**. Column typing per the measured DDL (`anchors.rs:33–43`), *not* by analogy to `audit_ledger`: `timestamp_token_bytes` is the only `BLOB` (R3); `chain_head_hash_at_anchor` is a **hex `VARCHAR` → `TEXT` (R2-shaped)**, and typing it `BLOB` would break `anchor_preimage`, which consumes it as `&str` (`chain/verify.rs:161`). |

#### The ledger inversion — why the table is the source and the mirror is the check

The first draft made the fsync'd mirror the *source* for `audit_ledger`. That is
wrong on two independent grounds, and the second one is worse than the first.

**Ground 1 — mirror replay is lossy, and every gate in this plan is blind to the
loss.** Measured: `MirrorEntry` (`crates/audit-ledger/src/mirror.rs:112–129`) has
**no `session_id`, no `session_pubkey`, no `event_sig` field**, and
`MirrorEntry::to_entry()` (`:211–214`) sets all three to `None` — the code says so
in its own comment: *"the ADR-0030 mirror is a hash-chain DIVERGENCE detector and
does not carry the session-signing columns."* Replaying the mirror therefore
**strips the S441 / ADR-0087 per-entry signature layer from the entire migrated
history**, and drops `audit_ledger_anchors` entirely because the mirror never held
it. `compute_entry_hash` deliberately excludes the session fields, so `verify_chain`
passes, all three head-`entry_hash` equalities pass, `PRAGMA integrity_check`
passes, and the `typeof()` sweep passes. **Green gate, gutted tamper-evidence** —
D2a's fail-open shape, inside the one family this plan exists to protect.

**And `verify_chain_signed` does not save it.** This is the part worth reading
twice, because adding that call was the obvious fix and it does not work. Its
anti-strip check (`chain/verify.rs:138–144`) fires only on the `(Some(sid), _, _)`
arm — an entry whose `session_id` survives but whose signature does not. Mirror
replay nulls `session_id` **too**, so every entry lands on
`(None, _, _) => { /* legacy / unsigned entry — allowed */ }` (`:146`) and the
strip is waved through as legacy data. Worse, with anchors dropped the anchor loop
never executes, so `anchors_pending == 0` and the returned verdict has
**`fully_anchored: true`** (`:188`) on a ledger with zero anchors and zero
signatures. *The strongest-sounding field in the verdict struct reads its most
reassuring value on the most thoroughly gutted input.*

**Ground 2 — the durability argument for mirror-as-source misreads the incident it
rests on.** The first draft justified mirror replay with "the mirror survived
2026-07-19 when the DB table did not." The incident record says the opposite.
`docs/runbooks/audit-mirror-defork-20260719.md` (commit `a8a6da3`): *the mirror
forked from the DB at seq 8056 (mirror 8060 > DB 8058)* — the **mirror** was
ahead and divergent, **the DB was authoritative** ("8058 rows, contiguous, zero
duplicate seqs"), and the repair **discarded the five divergent mirror entries and
rebuilt the mirror from the DB.** The single incident cited as proof that the
mirror is the more trustworthy artefact was resolved by treating the DB table as
the source of truth and the mirror as the thing that had drifted. Inverting the
carry is not merely safer; it is the direction the incident already established.

**What the mirror is still for.** Its durability role is real and unchanged: it is
fsync'd, it is the ADR-0030 divergence detector, and it is what makes a torn DB
tail *detectable*. That makes it an excellent **check** and a poor **source**, which
is exactly what its own doc comment claims for it. So:

1. **Carry the `audit_ledger` table row-by-row**, including `session_id`,
   `session_pubkey` and `event_sig`. All three are declared `VARCHAR`
   (`storage/mod.rs:1031–1033`) and bound as `&str` (`:738–740`), so they cross as
   **`TEXT`**, not `BLOB` — unlike `prev_hash` / `binary_hash` / `payload` /
   `entry_hash`, which are `BLOB` at `:1021,1025,1028,1030` and follow R3. T-2's
   `typeof()` sweep covers all seven explicitly; the three session columns matter
   most precisely because they are the ones with **no hash-chain check behind
   them**.
2. **Replay the mirror into a scratch in-memory ledger and diff it against the
   carried table** at the `entry_hash` level — the canonical agreement key per
   ADR-0030 §4, and the level `MirrorEntry`'s own doc comment
   (`mirror.rs:100–103`) says the two consumption paths are symmetric at.
3. **Classify the divergence rather than failing flat.** A flat
   "SQLite head == DuckDB head == mirror tail" equality would hard-stop on the
   2026-07-19 shape with no route forward. Three arms instead:
   - *mirror == table* → proceed.
   - *mirror **ahead** of table* (the 2026-07-19 shape) → **stop, do not migrate,
     do not force-fix**; route to the existing heal path
     (`AppendError::MirrorAheadOfDb` at `error.rs:125`, `heal_from_mirror_ahead`
     at `mirror.rs:873`, `heal_replay_mirror_tail` at `storage/mod.rs:785`), let
     it settle, then re-run the migrator from the top. **A migration is not a
     repair tool.**
   - *table **ahead** of mirror* → **hard stop, no heal.** This direction means the
     fsync'd mirror missed a committed append, which is a durability failure in
     the artefact the whole scheme leans on. It must not be papered over by a
     migration.

**The check that hard-stops instead of passing green.** One line of SQL per side,
run against **independently re-opened** connections (B4), and it is the only check
in the set that catches Ground 1:

```sql
-- signature coverage; must be EQUAL on both sides, and on the DuckDB side non-zero
SELECT COUNT(*) FROM audit_ledger WHERE event_sig IS NOT NULL;
-- anchor coverage; must be EQUAL on both sides
SELECT COUNT(*) FROM audit_ledger_anchors;
```

Under mirror-as-source these read `N` and `0`: **hard stop, loud, before anything
is promoted.** Under the corrected carry they read `N` and `N`. Two rules make it
stick:

- **`ChainVerdict.fully_anchored` is not admissible evidence.** It is `true` when
  `anchors_pending == 0`, which includes "there are no anchors at all"
  (`verify.rs:188`). The gate asserts the **counts**, and asserts
  `signatures_verified` and `anchors_anchored` are **non-zero on the DuckDB side
  before requiring equality** — an equality between two zeros is not a check.
- **Both counts are pinned in the Step-1 pre-migration manifest** and re-asserted
  by `rollback_to_duckdb.sh` (§6.2 step 7), so neither a forward migration nor a
  rollback can quietly produce a tamper-evidence-stripped ledger and report PASS.

Pinned by **T-18**, mutation-verified the only way that means anything: run the
migrator in mirror-replay mode against a copy of the DEV DB and **assert the gate
goes red**. A gate against B1 that has never been shown to catch B1 is not a gate.

#### The remaining families

| Family | Method | Why |
|---|---|---|
| **invoice / invoice_line / sequence tables** | **Row-by-row carry** through `duckdb_store`'s own read path → the new SQLite writer. | These are the legally-binding records (ADR-0009, 8-year). They must cross with byte-identical NAV/PDF output (T-4). Rebuilding them from the ledger is possible but would re-derive a regulatory record from a derived source — wrong direction. |
| **partners / products / purchasing** | Row-by-row carry. | Operator-entered master data; cheap; needed for the customer-journey e2e. |
| **inventory (`stock_movements`, cache cols)** | Carry `stock_movements` (append-only ledger); **rebuild** the `products.stock_qty` cache from `SUM(qty_delta)` **in Rust** via the existing `rebuild-stock-cache` path. | The cache is derived by definition, and rebuilding it exercises §3.4's Rust-side fold on real data. |
| **work orders / BOM / QA / QC / dispatch** | Row-by-row carry. | Small; the customer-journey e2e traverses them. |
| **quoting (`quote_pricing_jobs`, `quote_intake_log`, `quoting_*`, `supplier_prices`)** | **Drop and re-seed from the tunables defaults; do not carry job history.** | Step 8 changes five columns from `f64` to `Decimal` (§3.2 D). Carrying `f64` job history means writing a lossy `f64 → Decimal` converter for data that is DEV scratch. `[[feedback_dev_db_disposable]]` is exactly the licence to not build that. **The tunables/materials/machines rows ARE carried** (they are operator-configured, not scratch) — through the `Decimal` types, with a loud refusal on any value that does not round-trip. |
| **`quote_pricing_jobs` CAD artefacts** | Not touched. | Filesystem, AES-GCM, keychain-keyed. The DB holds a path; the path is carried verbatim. |
| **email outbox / relay queue** | Carry. | Small; the e2e sends an invoice email. |

**The reconciliation gate** (Step 4's exit, re-run at every family step):

- per-table row count SQLite == DuckDB, for every carried table;
- per-money-column **exact sum** SQLite == DuckDB, computed **in Rust on both
  sides** (never with SQL `SUM` — §3.4);
- `Ledger::verify_chain` genesis→head **OK on the SQLite side**;
- `verify_chain_signed` OK on the SQLite side — **and its `signatures_verified` and
  `anchors_anchored` counts asserted equal to the DuckDB side's, with the DuckDB
  side's required non-zero first. The verdict's `fully_anchored` flag is not
  admissible on its own** (`verify.rs:188`: it is `true` when there are no anchors
  at all);
- `audit_ledger_anchors` row count SQLite == DuckDB; **count of `audit_ledger` rows
  with a non-NULL `event_sig` SQLite == DuckDB** — the pair of checks that catch a
  silently-unsigned carry, and the only ones in this list that would have caught B1;
- SQLite head `entry_hash` == DuckDB head `entry_hash` == mirror tail `entry_hash`,
  **under the three-arm divergence classification above — not a flat equality**;
- `PRAGMA integrity_check` == `ok`;
- **`SELECT typeof(col)` over every row of every column in §3.2 A–G matches the
  declared class** — the M1 pin, applied to migrated data and not only to fresh
  writes.

Any mismatch → the step fails, `rollback_to_duckdb.sh` runs, nothing is force-fixed.

> **The migrator holds the writer lock, and the gate re-reads DuckDB
> independently.** The first draft had neither, and the two holes compound: a
> stale extraction verified against itself passes green.
>
> 1. **Rule 13 applies to the migrator.** Step 4 opens the DuckDB file in "a
>    separate one-shot process" — that is, as a *fresh opener*, which is precisely
>    the shape CLAUDE.md rule 13 says reads **stale** against Handle-WAL-resident
>    data. §6.2 gave the rollback script a `db_writer_lock` check; the migrator was
>    given none. If a DEV `serve` is live, the migrator silently migrates a stale,
>    short snapshot — and the audit ledger is the family where "short" means
>    "missing the most recent invoices".
>    → **The migrator acquires the tenant's `db_writer_lock` for its whole run and
>    refuses if it is held — not waits, not forces.** The API already exists and
>    is the one the rest of the tree uses:
>    `db_writer_lock::acquire_or_refuse(db_path, tenant, who)`
>    (`apps/aberp/src/db_writer_lock.rs:111`), keyed on dir+tenant via
>    `lock_path_for` (`:73`), which is why it excludes a live `serve` on either
>    engine (§1.1 G-7). There is precedent for exactly this: the 2026-07-19 repair
>    tool refuses while the whole-DB writer flock is held (`a8a6da3`).
>    It must **additionally refuse if `aberp.duckdb.wal` is present and non-empty**
>    (B3): a read-only DuckDB open cannot replay a WAL, so an unfolded WAL is data
>    the migrator cannot see and would not miss loudly. Holding the lock does not
>    make the WAL check redundant — the lock stops a *concurrent* writer; the WAL
>    check stops a *previously crashed* one.
> 2. **The verification must not reuse the extraction.** "Row count SQLite ==
>    DuckDB" is worthless if the DuckDB figure is the migrator's own in-memory
>    extraction count: it then compares the migrator against itself and passes
>    vacuously on any read-side loss — including the `event_sig` and anchor counts
>    that B1 turns on. **The gate runs as a separate invocation that re-opens
>    DuckDB and re-queries through the ordinary read path, after the migrator
>    process has exited and released the lock.** Stated as a shape, not a habit:
>    *no number the gate compares may have been produced by the migrator.*
> 3. **No read-only open exists in the tree today.** A sweep for
>    `access_mode` / `read_only` / `READ_ONLY` across `apps/`, `crates/`,
>    `modules/` returns **zero** non-test hits. Step 4's "opens DuckDB read-only"
>    is a capability to be *built* (`duckdb::Config::access_mode`), not one to be
>    assumed — and it is the single mechanism behind C-I's "the DuckDB file is
>    byte-unmodified". It gets its own pin (T-19): open read-only, attempt a write,
>    assert the error; and assert the file's digest is unchanged across the
>    migrator's full run against a copy.

---

## 7. Sequencing — nine independently-committable gated steps

**The per-step contract.** Each step: (a) is one PR; (b) closes with the CLAUDE.md
rule-4 gates — `cargo fmt` + build + test + `clippy -D warnings` + the cut gates;
(c) lands on a gate-green base; (d) ends with `rollback_to_duckdb.sh` run and a
green DuckDB DEV boot, then re-applied; (e) obeys rule 14 — **a family's writers
and readers cross together, never mid-family**.

**Steps 1–4 change no family's storage.** They build the machinery, the refusals,
and the migrator. If Phase 0 comes back saying the cost is larger than ADR-0107 §4
assumed, **the decision point is at the end of Step 4** and little has been spent —
that is ADR-0107's own "stop here having spent little" exit.

---

**Step 1 — Reversibility scaffolding. No engine code.**
- *Changes:* `run/rollback_to_duckdb.sh` (§6.2); the pre-migration snapshot script
  producing `.aberp-premigration-<ts>/` with digests + per-table row counts + head
  `seq`/`entry_hash` manifest; the `ABERP_DB`↔engine boot cross-check refusal
  (§2.5), inert while no `sqlite-engine` feature exists; M6's doc re-scope on
  `db_writer_lock`; the ADR-0107 §2 retirement-table amendment recording that
  `db_writer_lock` is **not** retired (PR #49 F-7b).
- *Verified by:* T-13 (refusal mutation-verified); the rollback script run against
  a DuckDB-only tree and asserted PASS; snapshot round-trip on a copy.
- **Also lands here (B3):** the three `.gitignore` entries (`*.sqlite*`,
  `.aberp-premigration-*/`, `.aberp-rolledback-*/` — §2.5's measured gap), and the
  manifest's enumeration of `aberp.duckdb.wal` and every `.audit.log.*.bak`.
  Both are prerequisites for Step 4 or the snapshot script producing **anything**
  on disk in a public repo, so they cannot wait a step. Pinned by T-17.
- **Also lands here (B1's half of the manifest):** the pre-migration manifest
  records the **non-NULL `event_sig` count** and the **`audit_ledger_anchors` row
  count** alongside the per-table row counts and the head `seq`/`entry_hash`.
  These are the two numbers §6.3's gate and §6.2's rollback verification both
  turn on; recording them before any engine code exists is what makes them a
  baseline rather than a self-report.
- **T-13's sequencing, resolved (F6).** As first written, the refusal was "inert
  while no `sqlite-engine` feature exists", and the arm that actually carries C-I
  — *a `sqlite-engine` binary refuses a non-`.sqlite` path* — is unbuildable until
  Step 3. The property the entire reversibility argument rests on would be
  **unpinned across Steps 1 and 2 — including Step 2, the step that links
  `rusqlite` into the tree.** Resolution: implement the decision as a **pure
  function** taking the engine as an *argument* rather than reading `cfg!`:

  ```rust
  pub enum Engine { DuckDb, Sqlite }
  /// Pure. No cfg!, no env, no fs. Both arms testable with no feature enabled.
  pub fn engine_path_agrees(engine: Engine, path: &Path) -> Result<(), EngineMismatch>
  ```

  Both arms are then unit-testable and mutation-verifiable **in Step 1 with no
  feature at all**, and the `~/.aberp/` refusal (C-II) rides the same function.
  Step 3 adds only the three-line `cfg!`-driven caller and re-runs T-13 end-to-end
  against a real binary. The ordering rule this generalises: **a refusal whose
  test cannot be written yet is not landed yet** — restructure it until it can be,
  or move it to the step where it can.
- *Rollback:* `git revert`. Nothing on disk changed.

**Step 2 — `rusqlite` dependency + the open-time posture. Nothing uses it yet.**
- *Changes:* `rusqlite` with bundled `libsqlite3-sys` ≥ 3.51.3 (M10);
  `SQLITE_OMIT_LOAD_EXTENSION` in the build (M2); a single
  `aberp_db::engine::open_hardened(path)` applying **M2, M3, M4, M7, M9** and
  nothing else; `cargo-deny` coverage (M10); cut-gate greps for `ATTACH`,
  `load_extension`, and the six forbidden `rusqlite` hook/vtab symbols.
- *Verified by:* T-3a–d, T-10, T-11, T-7 — **each mutation-verified** (remove the
  pragma, watch the test go red).
- *Rollback:* revert. The dependency is unreferenced by any family.

**Step 3 — The seam and the shared helpers. Still no family migrated.**
- *Changes:* `aberp_db::engine` type aliases behind the `sqlite-engine` feature
  (§2.3), incl. the 3 `DuckDBFailure` wrappers; `ensure_columns` with the
  fail-loud post-condition (§4.1, **M8**); `BEGIN IMMEDIATE` as the `Handle`'s
  transaction default (**M5**); a confirmation test per `ON CONFLICT` site
  (**M12**, §4.3 — 5 sites, the audit is done, no rewrites and no new `UNIQUE`
  index); **the exhaustive `read()` audit below.**
- *Verified by:* T-9 (M8 fail-loud, both arms), T-6 (M5 interleave), T-20 + T-21
  (the `read()` semantics pins), the 5-site `ON CONFLICT` confirmation table and
  the **complete** `read()` audit table, both in the PR body.
- *Rollback:* revert. Default build unaffected (feature off).

> **The `read()` audit (Q10) and the `busy_timeout` number (Q11) — one question,
> and the one this tree can least afford to defer.** Five of July's incidents came
> from the read-fork class; this is the same class, arriving through a semantics
> change rather than a stray `Connection::open`.
>
> `read()` becoming a real second connection is safe only under a claim §2.4
> asserts and never pins: that WAL gives a reader a fresh snapshot per statement.
> That holds **in autocommit** and is **false inside an explicit transaction**,
> where the reader freezes its snapshot at `BEGIN` and will not see a commit that
> lands after it. And a `read()` taken *while a `write()` guard is live* now
> contends for a real file lock instead of sharing one in-process instance, so
> M7's finite `busy_timeout` converts DuckDB's immediate mutex self-deadlock into
> a **timed hang, then `SQLITE_BUSY`** — rule 13's known failure mode with its
> loudness removed. The number *is* the observability of the worst case, which is
> why it is chosen here and not in Step 2 in isolation.
>
> **Scope — exhaustive, and the denominator is stated so completeness is
> checkable.** The 84 `Handle` sites split **50 `read()` / 34 `write()`**
> (§1.2). The audit classifies **all 50 `read()` sites**, with the 34 `write()`
> sites as the reachability context for axis (b). Two axes:
>
> - **(a) does it read inside an open transaction?** → the frozen-snapshot class.
> - **(b) is it reached while a `write()` guard is live?** → the lock-contention
>   class. Reachability is **closed under calls**, not judged line-locally — the
>   same reaching-set discipline ADR-0106's door gate uses, because a `read()`
>   three frames below a `write()` guard is the case a local read misses.
>
> Any site that is **both** is a defect `try_clone` was masking, and it is fixed
> in Step 3, not carried into its family's step. The output is a 50-row table in
> the PR body — every site classified, none marked "probably fine". **An audit
> with an unstated denominator is a sample.**
>
> **Three pins, all mutation-verified:**
> - **T-20** — commit on connection A → read on a **pre-existing** connection B in
>   autocommit → assert B sees it. This is §2.4's snapshot claim, pinned instead
>   of asserted. Its in-transaction twin asserts the *opposite*: B inside `BEGIN`
>   must **not** see the commit — so the test encodes the real semantics, not the
>   convenient half.
> - **T-21** — a nested `read()`-inside-`write()` **aborts loudly** rather than
>   waiting out `busy_timeout`. The loud abort is the point: rule 13's
>   self-deadlock is a *feature* of DuckDB's behaviour here, and a finite timeout
>   would silently downgrade it to a slow request.
> - **T-3d** (M7) asserts the chosen `busy_timeout` value is actually set.
>
> **The number, chosen conservatively and marked.** `busy_timeout = 5000 ms`,
> **on the explicit condition that T-21 lands first** — with the nested case
> aborting loudly, the timeout is only ever a backpressure knob for genuine
> cross-process contention, never the thing that hides a deadlock. 5 s is long
> enough that a checkpoint or a slow invoice write does not produce a spurious
> `SQLITE_BUSY` on the NAV path, and short enough that a UI request fails visibly
> rather than appearing to hang. Step 2 measures the real p99 write-hold under the
> customer-journey e2e and may revise the number **downward** with the measurement
> in the PR body; raising it requires re-arguing T-21. Recorded as a decision with
> a condition attached rather than left as "needs a number".
>
> **This audit gates Step 5. It does not run alongside it.**

**Step 4 — The migrator + the reconciliation gate. Read-only against DuckDB.**
- **Preconditions the migrator enforces before it opens anything** (B4). All are
  refusals; **none of them waits**:
  1. it **acquires** the tenant's `db_writer_lock` via
     `acquire_or_refuse(db_path, tenant, "migrate-to-sqlite")`
     (`apps/aberp/src/db_writer_lock.rs:111`) and holds it for its whole run —
     rule 13: a fresh opener reads Handle-WAL-resident DuckDB **stale**;
  2. `aberp.duckdb.wal` is absent or empty — a read-only open cannot replay it,
     so an unfolded WAL is data the migrator cannot see (B3);
  3. `ABERP_DB` resolves inside `apps/aberp-ui/` and nowhere under `~/.aberp/`
     (C-II), via the same pure `engine_path_agrees` from Step 1;
  4. the pre-migration snapshot exists and **verifies** — including the `.wal`
     pair and every `.audit.log.*.bak` (B3), and the two tamper-evidence counts
     (B1).

  The read-only open itself is **new capability** — a sweep for
  `access_mode`/`read_only`/`READ_ONLY` over `apps/`, `crates/`, `modules/`
  returns **zero** non-test hits — so it is built here (`duckdb::Config::access_mode`)
  and pinned by **T-19**, not assumed.
- *Changes:* `aberp migrate-to-sqlite` one-shot: opens DuckDB **read-only**, opens
  a fresh `aberp.sqlite`, **carries `audit_ledger` and `audit_ledger_anchors`
  row-by-row from the DuckDB tables — including `session_id` / `session_pubkey` /
  `event_sig` — and uses the mirror only as the three-arm cross-check (§6.3). It
  does not replay the mirror as a source.** Then: carries the remaining families
  per §6.3, applies §3's representation rules, and **refuses on any mismatch**;
  plus the `information_schema` → `pragma_table_info` / `sqlite_master` rewrites
  (§4.3 G-3) and the `DROP COLUMN IF EXISTS` guards (G-4).
- *The reconciliation gate is a separate invocation* (B4), run **after** the
  migrator has exited and released the lock. It re-opens DuckDB and re-queries
  through the ordinary read path: **no number it compares was produced by the
  migrator.**
- *Verified by:* run against a **copy** of the DEV DB in the scratchpad, then
  T-18's mirror-replay mutation (run the migrator in the rejected mirror-as-source
  mode and **assert the gate goes red** on the `event_sig` and anchor counts).
  Exit criterion — **the real DEV tenant DB migrates and the reconciliation gate
  passes green, including the `typeof()` sweep over all seven ledger columns, the
  two tamper-evidence counts, and `verify_chain` genesis→head.**
- *Rollback:* delete the produced `.sqlite`; revert. **This is ADR-0107 §4.1's
  "stop here having spent little" gate. If it fails, stop and re-open the engine
  decision.**

**Step 5 — The fused transactional core: `audit_ledger` + `modules/billing` +
invoice-sequence allocation.** *(the whole point of the exercise)*
- *Changes:* the 25 + 4 + 2 + 1 + 1 DDL sites (§4.2) via `ensure_columns`; `STRICT`
  DDL; §3.2 B's `huf_equivalent_total` `DECIMAL→INTEGER` bind/read change; §3.2 C's
  `exchange_rate` + `quantity` `DECIMAL→TEXT`; §3.4's `reports.rs:800,861` folds
  moved to Rust **and the three `unwrap_or(0)` fail-opens (`:827`, `:872`, `:1279`)
  deleted in the same commit** (F5 — a pre-existing production defect, named as
  such in the PR body, not counted as migration collateral); `write_vat_rate_choice`
  (`nav_xml.rs:1788`) converted from `f64` to `Decimal`, emptying N-2's allowlist
  and closing the rule-7 fork against `:2658` (B2); R3's BLOB binds audited across
  ~30 hash sites; S444's durable ledger-derived invoice-number floor carried across
  **unchanged** (belt and braces stay).
- *Verified by:* T-1, T-2, T-4 (**NAV XML + PDF byte-identity across the whole DEV
  invoice set** — this is also what proves the `write_vat_rate_choice` conversion
  changes no emitted byte), T-5(a–e) (money property tests + both `f64` gates),
  T-6, T-14 (crash / number-durability), and the full reconciliation gate.
- *Rollback:* `rollback_to_duckdb.sh`. **This is the step where the rollback drill
  is not a formality — run it, boot DuckDB green, re-apply, and say so in the PR.**

**Step 6 — Adversarial checkpoint.** Not a code step. Rule 4 reserves full
adversarial review for the invoice→NAV/ÁFA path; Step 5 *is* that path. No further
family crosses until this closes.

**Step 7 — The remaining non-quoting families,** one at a time, rule-14 fused:
partners (+ **M11**, T-12) → products/inventory (incl. §3.4's three cache-rebuild
folds **and both low-stock predicate folds, `repository.rs:548–549` and `:585`,
in one commit** — F1/Q2) → work orders/BOM → QA/QC → dispatch → purchasing →
email/relay.
- *Verified by:* per-family reconciliation + the family's existing round-trip tests
  + T-15 (customer-journey e2e) re-run after each.
- *Rollback:* per family; each is its own PR.

**Step 8 — The quoting family, including the five `f64` money columns (§3.2 D).**
- *Changes:* 17 + 15 + 10 + 7 + 6 = 55 DDL sites; `total_price_eur` ×2,
  `cost_per_kg_eur` ×2, and the two rate tunables `f64 → Decimal` at the Rust type;
  quoting job history dropped and re-seeded per §6.3.
- *Verified by:* the pricing-pipeline calibration tests re-pinned on `Decimal`;
  reconciliation on the carried tunables/materials/machines rows.
- *Rollback:* per §6.2. **If this step overruns, stop and leave quoting on DuckDB —
  do not migrate it as `REAL` (§3.2 D).** The reversible window stays open; that is
  what it is for.

**Step 9 — DEV soak + the report.** Run the DEV tenant on SQLite for a defined
period with the DuckDB build one command away. Produce the measured comparison
ADR-0107 §3.1 left as "unmeasured": bundle size delta, boot time, invoice-issue
latency, and — the one that decides whether the exercise worked — **the tear
count**, against the eight recorded tears in twenty days that opened ADR-0107 §1.1.
- *Exit:* a written recommendation on whether to open the prod-cutover ADR (§11).
  **This plan does not authorise that cutover.**

---

## 8. Test gates the execution and testing sessions must run

Ordered by what they defend. Every pin is **mutation-verified** — the test must be
shown to go red when the thing it pins is removed. A pin that cannot go red is not
a pin (ADR-0107 §4.1, extended to security by PR #49).

| id | Test | Defends |
|---|---|---|
| **T-1** | `INSERT` an `f64` into **every** column in §3.2 A, B, C, F, G → assert `SQLITE_CONSTRAINT_DATATYPE` | M1 / F-6a |
| **T-2** | `SELECT typeof(col)` over **every row** of every §3.2 column after migration → `'integer'` / `'text'` / `'blob'` / `'real'` as declared. **Explicitly enumerates all seven `audit_ledger` columns** — `prev_hash` / `binary_hash` / `payload` / `entry_hash` → `'blob'` (R3), and `session_id` / `session_pubkey` / `event_sig` → `'text'` (declared `VARCHAR`, `storage/mod.rs:1031–1033`; bound `&str`, `:738–740`). The three session columns are the ones with **no hash-chain check behind them** (`compute_entry_hash` excludes them), so `typeof` is their only structural guard. Plus `audit_ledger_anchors`: `timestamp_token_bytes` → `'blob'`, `chain_head_hash_at_anchor` → **`'text'`** (a hex `VARCHAR` consumed as `&str` by `anchor_preimage`, `verify.rs:161` — typing it `'blob'` breaks anchor verification). | M1 / F-6c / B1 |
| **T-3a–d** | `load_extension` errors; `ATTACH` errors; `CREATE TRIGGER`/`CREATE VIEW` rejected; each of `journal_mode`/`synchronous`/`fullfsync`/`busy_timeout`/`shared_cache` read back and asserted | M2/M3/M4/M7 |
| **T-4** | **Byte-identity**: for every invoice in the DEV DB, the NAV `InvoiceData` XML and the rendered PDF bytes are **identical** DuckDB vs SQLite | §3.3, the regulatory record |
| **T-5** | **Money property tests**: (a) `Decimal` round-trips through `TEXT` for 10⁵ generated values at scale 0–6 incl. trailing-zero forms; (b) `huf_equivalent_round_half_even` on `Decimal` → `i64` matches DuckDB's result for the whole DEV rate set; (c) `unit_price × quantity` folded in Rust equals the pre-migration DuckDB `DECIMAL(38,6)` aggregate for every invoice | §3.1, §3.3, §3.4 |
| **T-5(d)** | **N-1, zero allowlist.** No `f64` is constructed on any *monetary-amount or quantity* path between column and emitted byte, over `modules/billing/src` + `apps/aberp/src/nav_xml.rs` + `crates/invoice-pdf/src`. Enforced as a cut-gate grep for `as f64` / `: f64` / `f64::` / `0.0` **excluding** the VAT-rate site covered by T-5(e). **This is green today** — the full sweep of that reach set returns one live `f64` and it is the rate, not an amount — so the gate lands as a ratchet, not as a red. Mutation-verify by introducing an `f64` on the `huf_equivalent_total` read and watching it red. | §3.3 N-1 |
| **T-5(e)** | **N-2, a one-entry shrinking allowlist.** The set of `f64` constructions in the billing→`nav_xml`→`invoice-pdf` reach set is **exactly** `{nav_xml.rs:1788}` — a *percentage rendering* of an INTEGER basis-point count, value-exact over the four legal HU rates. Any second entry reds the gate. **Step 5 converts that site to `Decimal`, after which the allowlist is empty and T-5(e) asserts zero.** The allowlist may only shrink; growing it requires amending §3.3. | §3.3 N-2, B2 |
| **T-6** | Two connections interleave read-head → append; must **not** produce two links off one `prev_hash`. Run with and without `BEGIN IMMEDIATE` | M5 / F-7a |
| **T-7** | `db_writer_lock_e2e` re-pointed at SQLite; **plus** a cross-engine test: a DuckDB `serve` holding the lock refuses a SQLite `serve` on the same tenant+dir | M6 / F-7b / §1.1 G-7 |
| **T-8** | Cut-gate grep over any §3.2 A–D column name appearing in any SQL string, for **arithmetic**: `SUM(` `AVG(` `*` `+` **`-` `/`** — and for **comparison**: **`<` `>` `<=` `>=` `BETWEEN` `MIN(` `MAX(` `ORDER BY`** (F2). The first draft's pattern had `SUM(`/`*`/`+`/`AVG(` only: it omitted subtraction and division, and had **no comparison arm at all**, so it was structurally incapable of seeing `repository.rs:548–549` — the one statement §3.4 and Q2 both turn on. A gate that cannot red on the plan's own worst example is PR #43's name-vs-shape lesson, unlearned. **Mutation-verify against both known sites specifically:** restore the original `low_stock_products` query (`:548` `<` and `:549` `-`), watch T-8 red; restore `count_low_stock_products` (`:585` `<`), watch T-8 red **again** — one site's mutation passing is not evidence the other is covered. Also verified to red on `COALESCE(col, 0)`-wrapped operands, since that is the form both real sites take. | §3.4, Q2, F1, F2 |
| **T-9** | `ensure_columns`: seeds a pre-migration schema and asserts every expected column exists after `ensure_schema`; **and** asserts `Err` when a column cannot be added, and `Err` when the table is absent | M8 / F-1c / D2a's shape |
| **T-10** | mode of `aberp.sqlite`, `-wal`, `-shm` == `0600`; tenant dir `0700` | M9 / F-5a |
| **T-11** | `sqlite3_libversion_number() >= 3051003` | M10 / M12 |
| **T-12** | `Árvíztűrő` vs `ÁRVÍZTŰRŐ` still matches the partner dedup guard; a `%` needle does not over-match | M11 / F-1b |
| **T-13** | The `ABERP_DB`↔engine boot refusal, both directions; and a refusal when the resolved path is under `~/.aberp/` | C-I, C-II |
| **T-14** | **Crash / number-durability**: `SIGKILL` the writer mid-invoice-issue ×N; on restart assert (a) `verify_chain` OK, (b) no invoice number is ever re-issued, (c) the mirror tail and DB head agree. This is the S444 regression, re-armed on the new engine. | ADR-0107 §4 rec. 2 |
| **T-15** | **Customer-journey e2e** (`[[feedback_customer_journey_e2e_gate]]`): quote → order → work order → dispatch → invoice → NAV submit → PDF → email, end to end on SQLite, asserting the invoice number, the ÁFA breakdown, and the PDF bytes. Re-run after **every** family step in Step 7. | the whole product |
| **T-16** | `PRAGMA integrity_check` == `ok` after every step | corruption |
| **T-17** | **`.gitignore` coverage, asserted not assumed** (B3). `git check-ignore` returns 0 for each of: `aberp.sqlite`, `aberp.sqlite-wal`, `aberp.sqlite-shm`, `aberp.sqlite.audit.log`, `.aberp-premigration-<ts>/x`, `.aberp-rolledback-<ts>/x`, `aberp.duckdb`, `aberp.duckdb.wal`, `aberp.duckdb.audit.log`, `aberp.duckdb.audit.log.healed-1.bak`. A shell test in `run/tests/`, wired into CI — the repo is public and every one of these holds partner bank details. Mutation-verify by deleting the `*.sqlite*` line. | B3, Step 1 |
| **T-18** | **The tamper-evidence gate catches its own blocker** (B1). Run the migrator in the **rejected** mirror-as-source mode against a copy of the DEV DB; assert the reconciliation gate goes **red** on the non-NULL `event_sig` count and on the `audit_ledger_anchors` count. Also assert that `verify_chain`, `verify_chain_signed`, `PRAGMA integrity_check` and the head-hash equality all still pass on that same gutted output — **the point of the test is that four green checks and one red one is the true picture**, and that removing the two count checks makes the whole gate green on a signature-stripped ledger. | B1, §6.3 |
| **T-19** | **The read-only DuckDB open** (B4, new capability — zero such opens exist today). Open `aberp.duckdb` read-only, attempt a write, assert the error; and assert the file's SHA-256 is byte-unchanged across a full migrator run against a copy. This is the single mechanism behind C-I. | B4, C-I, Step 4 |
| **T-20** | **The WAL snapshot claim, pinned in both directions** (F7/Q10). Commit on connection A → read on a **pre-existing** connection B **in autocommit** → B sees it. Twin: B inside an explicit `BEGIN` → B does **not** see it. §2.4 asserted the first and never mentioned the second. | F7, Q10, §2.4 |
| **T-21** | **Nested `read()`-inside-`write()` aborts loudly** rather than waiting out `busy_timeout` (F7/Q11). Rule 13's self-deadlock is the desired behaviour; a finite timeout must not downgrade it to a slow request. T-21 landing is the stated precondition for the 5000 ms `busy_timeout`. | F7, Q11, rule 13 |

**On the existing gates.** ADR-0107 §4.1 Phase 2 says census entries and fork-gate
baselines are *deleted* as each family crosses. **This plan does not delete them.**
During a reversible window the DuckDB build is still buildable and still bootable,
and a gate deleted is a gate that cannot protect the thing you roll back to. They
are frozen as-is and their retirement is a post-cutover decision. (This is a
deliberate divergence from ADR-0107 §4.1 — surfaced, not averaged.)

---

## 9. Deferral ledger (CLAUDE.md rule 3)

Found while grounding this plan; **not fixed here**; each has the step that closes
it or an explicit "out of scope".

| Item | Closed by |
|---|---|
| **`reports.rs:872` `decimal_str_to_i64(&s).unwrap_or(0)` — a LIVE ÁFA-report fail-open running in production today, on DuckDB, independent of this migration.** A parse failure prints **0 HUF** instead of failing. Siblings at `:827` and `:1279`. | **Fixed in-migration**, Step 5, in the same commit as the `reports.rs:861` fold — the fold's `Result` replaces the swallow, and the two are the same three lines (§3.4). **Not a separate cut**, and the Step-5 PR body must name it as a pre-existing production defect so it is not miscounted as migration collateral. If Step 5 is deferred or the engine decision reopens at Step 4, **this reverts to owing its own PR** and does not lapse. |
| DEV DB measured mode **0644**; no code chmods the tenant DB — **true today, engine-independent** | M9 / Step 2, or a standalone 5-line PR now (PR #49 already flagged this) |
| `nav_xml.rs:1788` write path is `f64` while `:2658` read path is exact `Decimal` — a **rule-7 fork on the NAV VAT rate**, pre-existing and engine-independent; value-exact for all four legal HU rates, so not a filing defect | Closed by the Step-5 `Decimal` conversion (B2, §3.3), which also empties T-5(e)'s allowlist |
| `MirrorEntry` cannot round-trip a signed entry — the mirror is a divergence detector, not a backup, and its own comment says so (`mirror.rs:211–214`) | **Out of scope.** Recorded because B1 was the first time that design limit had a consumer that assumed otherwise. If the mirror is ever to be a recovery source, that is its own ADR — and it would have to add three columns and a signature-preserving encoder first. |
| `verify_chain_signed`'s anti-strip check is keyed on `session_id` surviving (`verify.rs:138–146`), so it cannot see a strip that nulls `session_id` too; and `ChainVerdict.fully_anchored` reads `true` on a ledger with zero anchors (`:188`) | **Out of scope for this plan, and worked around inside it** — §6.3's gate asserts counts rather than the verdict flags. Flagged because a *future* consumer will reach for `fully_anchored` and get the reassuring answer. Its own PR. |
| `material_inventory.*_qty` is `DOUBLE` while `stock_movements.qty_delta` is `DECIMAL` — **two representations of one physical quantity** (rule 7) | **Out of scope.** Recorded because migrating both as-is under `STRICT` makes the divergence look sanctioned. Needs its own decision. |
| `qc_inspections.deviation` is a derived `REAL` driving a pass/fail verdict | Out of scope; flagged in §3.2 E |
| ADR-0107 / the frozen baseline / its header disagree on the in-serve read-fork count (**14 / 13 / 9**) | Out of scope for the migration; a stale frozen baseline is the exact class PR #43 existed to prevent → its own PR |
| `aberp-mes::ledger_writer::write_one` appends through a fresh in-serve connection while the write-fork gate reports ZERO | ADR-0107 §5 says close it **by hand now** — a forked *append* forks the ledger under **any** engine. Independent of this plan; should land before Step 5. |
| The S392 NAV pre-flight is dead (0 `check_performed` in 225 mirror entries) | Orthogonal, engine-independent, and ADR-0107 §5 calls it the most under-weighted open item. Not this plan. |
| ADR-0107 §1.3 finding F1 (is a forked read stale, or was D2a row loss?) is unsettled | Needs a measurement; **the migration makes it moot** but does not answer it |
| ADR-0107 §2 lists `db_writer_lock` as retirable; ADR-0107 §3 B-cost-1 says money is already integer; ADR-0107 §4.1 Phase 0 does not scope the DDL rewrites | Amended in Step 1's PR body per PR #49's own deferral ledger, plus §1.1 G-2's `.sql` correction which PR #49 also missed |

---

## 10. The eleven open questions — **all closed**

These were the choices flagged for attack. All eleven were ruled on 2026-07-30
(§13.2 carries the reasoning); the dispositions below are current, and none of
them is an open item an execution session must resolve first. Four changed as a
result — Q2, Q5, Q7 and Q10.

| # | Question | Disposition | Where it lives now |
|---|---|---|---|
| **Q1** | Compile-time cargo feature vs runtime engine selector (§2.2 D1) | **Closed — compile-time.** Reversibility comes from *two files*, not from the selector, which the B3 fix makes more true rather than less. A runtime toggle costs a trait layer over 449 + 120 + 84 sites and keeps every family simultaneously reachable on two engines — the half-migrated shape rule 14 forbids. If Ervin meant a runtime toggle, that is his to reopen; the case is made, not averaged. | §2.2 |
| **Q2** | `TEXT`-decimal vs scaled-integer for quantities/rates (§3.1 R2) | **Closed — `TEXT`, and the lexicographic risk is swept rather than deferred.** The per-column sweep over all ten R2 columns is **done, in §3.4**: exactly two hits, `repository.rs:548` and `:585`, both folded into Rust in Step 7. The original deferral said *`ORDER BY`* and would have missed the `WHERE` — the half that returns wrong rows. | §3.4, T-8, Step 7 |
| **Q3** | `routings.est_cost_huf` → `TEXT` (R2) rather than `INTEGER` (R1) (§3.2 B) | **Closed — R2, the one documented R1 exception.** `Option<Decimal>` in Rust, never on the NAV wire, the PDF, or ledger totals. R1 would force a "what is HUF's minor unit for an *estimate*" product decision to serve consistency alone. | §3.2 B |
| **Q4** | The five quoting `f64` money columns (§3.2 D) — Step 8, converted to `Decimal` | **Closed — convert, and the strictness stands.** §3.2 D's pre-commitment ("if Step 8 overruns, *stop* — do not migrate as `REAL`") is the rule-11 guard that stops a later session taking the easy branch. Not softened. | §3.2 D, Step 8 |
| **Q5** | The `ON CONFLICT` sites: add `UNIQUE` indexes, or rewrite as `SELECT`-then-write? (§4.3) | **Dissolved.** There are **5**, not 21 (16 doc comments + 1 test string), and all 5 conflict targets are **already the declared `PRIMARY KEY`**, verified statement-by-statement. Zero indexes, zero rewrites — so the `[[no-sql-specific]]` tension the question was built around never existed. Step 3's obligation is 5 confirmation tests. | §1.2, §4.3, M12 |
| **Q6** | `.sql` migration files: split the `ALTER` lines out into `ensure_columns` (§4.2) | **Closed — split; 8 lines move.** Beats owning a load-time rewriter forever (rule 12). "A family's schema lives in two places" is real but small, and `CREATE`-stays-SQL / `ALTER`-moves-to-Rust is a legible line. | §4.2 |
| **Q7** | Does the ledger cross by mirror replay or table copy? (§6.3) | **Inverted — table copy is the source, the mirror is a three-arm cross-check.** The circularity this question asked about was not the defect; the **lossiness** was. And the durability argument for mirror-as-source misread its own incident: on 2026-07-19 the *mirror* was ahead and divergent and the **DB was authoritative**. See B1. | §6.3, T-18 |
| **Q8** | Drop quoting job history rather than write an `f64 → Decimal` converter (§6.3) | **Closed — drop.** Correct for a disposable DEV DB; **wrong for prod**, and §11.1 carries that forward explicitly rather than inheriting it silently. | §6.3, §11.1 |
| **Q9** | Keep the census / fork gates frozen instead of deleting per family (§8) | **Closed — keep, and the divergence from ADR-0107 §4.1 is the better call.** A gate deleted is a gate that cannot protect the state you roll back to. Under a rollback-only constraint that is a requirement, not a preference, and rule 12's objection to dead machinery does not reach machinery guarding a live rollback target. | §8 |
| **Q10** | Is `read()` returning a real second connection a behaviour change anywhere? (§2.4) | **Was the plan's weakest claim; now audited, not assumed.** "Strictly sees more" is false inside an explicit transaction. Step 3 classifies **all 50 `read()` sites** on two axes, gates Step 5, and pins the WAL snapshot claim in **both** directions (T-20) plus the nesting abort (T-21). This is the class behind five of July's incidents. | Step 3, T-20/T-21 |
| **Q11** | `busy_timeout` value (M7 said "explicit and finite", no number) | **Closed — 5000 ms, conditional on T-21 landing first,** revisable downward on Step 2's measurement. Decided together with Q10 because the number *is* the observability of Q10's worst case: without T-21's loud abort, a finite timeout turns rule 13's self-deadlock into a silent hang. | Step 3, M7, T-3d |

---

## 11. What a production cutover would additionally require — **not authorised here**

Recorded so Step 9's recommendation has a shape, and so nothing in §7 is mistaken
for prod work.

1. **Prod is not disposable.** Every "drop and re-seed" in §6.3 becomes "carry", so
   the `f64 → Decimal` converter this plan avoids (Q8) must be written, with a
   documented rounding rule and a refusal on any value that does not round-trip.
2. **A one-shot, offline, verified conversion** with the operator's machine
   quiesced, the `upgrade_prod.sh` path taught about the new filename, and a prod
   rollback drill rehearsed on a copy of the prod DB **before** the real run.
3. **The 8-year statutory retention window** means the SQLite file becomes the
   record of account. `PRAGMA integrity_check` + `verify_chain` + a snapshot must
   be part of the cutover transcript, not a follow-up.
4. **The prod tripwire is `debug_assertions`-only** and stays inert. ADR-0107 §5
   says do not invest in it under Option B; that holds, but it means prod has no
   in-process guard during the crossing — the F-E flock (M6) is the only one.
5. **`--features production` selects the live NAV endpoint at compile time**
   (`[[reference_nav_endpoint_is_compile_time]]`), so a prod SQLite build is
   `--features production,sqlite-engine` — a **new feature combination that has
   never been built**. It must be gated and smoke-tested on its own, and note that
   `--features production` already shows a dead-code warning dev builds cannot.
6. **Retiring the compensation machinery** (ADR-0107 §2's ~8 000 lines) is a
   separate, post-cutover ADR. Nothing in §7 retires anything, and `db_writer_lock`
   is **not** in the retirable set (§1.1 G-7, PR #49 F-7b).

---

## 12. Consequences

**If this plan is executed as written:**

- The DEV tenant runs on SQLite behind a feature flag, with a single verified
  command back to DuckDB at every point, and DuckDB byte-untouched throughout.
- Money, rate, quantity and hash representation are settled **before** any family
  crosses, with every such column in the tree named and typed (§3.2), and the claim
  "no float touches a monetary value" stated in the two forms measurement supports
  (§3.3 N-1 / N-2) and made falsifiable by T-5(d) and T-5(e).
- **The audit ledger crosses with its tamper-evidence intact and provably so.**
  `audit_ledger` and `audit_ledger_anchors` carry row-by-row from the DuckDB
  tables; the mirror keeps its evidentiary role as a three-arm cross-check; and
  two count equalities — non-NULL `event_sig`, anchor rows — are the checks that
  hard-stop rather than pass green, pinned by T-18 against the rejected design
  itself.
- The 114 DDL sites cross through one const-driven, fail-loud helper, closing
  PR #49 F-1c's reintroduction of D2a's shape.
- All twelve mitigations land as gated exit conditions with mutation-verified pins,
  three of them (M1, M5, M6) before the invoice path moves.
- Step 4 is a genuine cheap abort point: if reconciliation fails, the engine
  decision reopens having spent four scaffolding PRs.

**What this plan deliberately does not do:** migrate prod; retire any compensation
machinery; delete any gate or census baseline; fix the pre-existing defects in §9;
or authorise a cutover. Each is someone else's ADR.

**The risk this plan carries and cannot remove:** Steps 5 and 8 change how money is
stored on the paths that reach the Hungarian tax authority. Every mechanism in §6
and §8 exists to make that change reversible and observable, but reversible is not
the same as harmless, and the DEV-only scope (C-II) is what keeps the blast radius
at a disposable database.

---

## 13. Adversarial review — 2026-07-30 (historical record; **all items closed**)

> **Status of this section.** It is retained **verbatim as written on 2026-07-30**
> so the reasoning behind the changes survives, and because a review that is
> edited to match the fixes cannot be checked against them. **Its verdict is
> superseded**: B1–B4 and F1–F7 are resolved in §1–§8 and audited item-by-item in
> **§14**. Where §13 and the body disagree, the body is current. Read §13 for
> *why*; read §14 for *what closed it*.

> ## VERDICT (2026-07-30, now superseded by §14): **NO-GO to begin execution**, pending B1–B4 and F1–F7.
>
> The plan's **structure** is sound and its **direction** survives attack: the
> step ordering is right, Step 4 genuinely is a cheap abort point, the
> compile-time selector is the correct engineering call, keeping the frozen gates
> is correct, and the `db_writer_lock` mutual-exclusion property (G-7) verifies
> exactly as claimed — `lock_path_for` (`db_writer_lock.rs:73`) keys on
> `<parent-dir>/.aberp-db-writer.<tenant>.lock`, and `mirror_path_for`
> (`mirror.rs:94`) appends its suffix to the db path, so the two engines' mirrors
> cannot collide either. Both load-bearing claims measured true.
>
> It is NO-GO on two of the three grounds Ervin named as disqualifying.
>
> **The money model has a hole (B2):** §3.3's "there is no point in this trace
> where an `f64` exists" and §3.2 F's "VAT never touches a float" are both false —
> `nav_xml.rs:1788` renders the `<vatPercentage>` written to NAV via
> `vat_rate_basis_points as f64 / 10000.0`. Benign in value for all four legal
> Hungarian rates; **not** benign as a plan invariant, because T-5(d) is specified
> as a gate enforcing precisely the claim that is false, so it goes red on day one
> and the execution session's cheapest path is to weaken it.
>
> **The reversibility guarantee has a hole (B3):** §2.5's file map and §6.2 step 4
> never name `aberp.duckdb.wal`. Restoring the main file from the snapshot while a
> foreign-generation WAL sits beside it does not fail the rollback — it corrupts
> it, with no second snapshot behind it. This is the one step in the plan where a
> failure leaves DEV unrestorable, and it is a two-line fix.
>
> **And the crown-jewel family loses its tamper-evidence silently (B1),** which is
> not one of Ervin's three grounds but is worse than any of them: mirror replay
> strips `session_id` / `session_pubkey` / `event_sig` from every migrated entry
> **by design** (`mirror.rs:206–215`, in the code's own comment), drops
> `audit_ledger_anchors` entirely, and **every check in §6.3 passes green anyway**
> because `compute_entry_hash` excludes those fields. Fail-open at the exact
> point the plan was written to defend.
>
> Every one of the four is closable in the plan text, and three of them already
> are, above. **None of them reopens the engine decision.** Fix them and this is a
> GO — the plan is closer to ready than the verdict word suggests.

### 13.1 Must-fix before execution begins

| # | Must-fix | Lands |
|---|---|---|
| **B1** | Invert §6.3's ledger carry: **table row-by-row is the source, the mirror is a three-arm cross-check**. Add `audit_ledger_anchors` to the carry set. Add `verify_chain_signed`, anchor-count equality, and **non-NULL `event_sig` count equality** to the reconciliation gate. Extend T-2's `typeof()` sweep over the three session columns — they are the only hash-adjacent columns with no chain check behind them. | §6.3, Step 4, T-2 |
| **B2** | Resolve the `nav_xml.rs:1788` `f64`: convert `write_vat_rate_choice` to `Decimal` (≈3 lines, **recommended** — it also closes the rule-7 fork against the exact `Decimal` parse at `:2658`), **or** scope T-5(d) to an explicit allowlist naming the site. Stated in the Step-5 PR body either way. Silently weakening T-5(d) is the forbidden branch. | §3.2 F, §3.3, Step 5, T-5(d) |
| **B3** | Snapshot and restore `aberp.duckdb` + `aberp.duckdb.wal` + the mirror + every `.audit.log.*.bak` **as an atomic set, all or none**. Add `*.sqlite*`, `.aberp-premigration-*`, `.aberp-rolledback-*` to `.gitignore` **in Step 1** — the repo is public and the artefacts hold partner bank details. | §2.5, §6.2, Step 1 |
| **B4** | The migrator acquires `db_writer_lock` and **refuses** if held (rule 13: a fresh opener reads a Handle-WAL-resident DB stale); refuses on a non-empty `aberp.duckdb.wal`; and the reconciliation gate **re-reads DuckDB independently after the migrator exits** rather than comparing against the migrator's own extraction counts. Build + pin the read-only open (zero such opens exist in the tree today). | §6.3, Step 4 |
| **F1** | `aberp-inventory/src/repository.rs:549` — fold **both** the `<` comparison and the deficit `ORDER BY` into Rust. It is §3.4's 7th arithmetic site and the tree's **only** Q2 lexicographic break. | §3.4, Step 7 |
| **F2** | T-8's grep gains `-`, `/`, and the comparison operators; mutation-verify it against `repository.rs:549` specifically. | T-8 |
| **F3** | Correct the `ON CONFLICT` census 21 → **5**; record that all 5 targets are already the declared `PRIMARY KEY`, so Step 3's obligation is 5 confirmation tests, **not** an audit-and-rewrite. | §1.2, §4.3, Step 3 |
| **F4** | Fix the three non-existent table names in §3.2 and add `invoice_line.quantity_dec`. | §3.2 |
| **F5** | `reports.rs:871`'s `decimal_str_to_i64(...).unwrap_or(0)` dies in the same commit as the fold it hides. | §3.4, Step 5 |
| **F6** | Make the engine↔path refusal a **pure function taking the engine as an argument**, so T-13 is mutation-verifiable in Step 1 instead of unpinned until Step 3. | Step 1, T-13 |
| **F7** | Q10's `read()` audit is **exhaustive and gates Step 5**, classified on two axes (in-transaction reads; reads reached under a live `write()` guard), with the WAL snapshot claim pinned rather than asserted. Q11's `busy_timeout` number is chosen in the same breath — it is the observability of Q10's worst case, not a separate nit. | Step 3 |

### 13.2 Ruling on Q1–Q11

| # | Ruling | Reasoning |
|---|---|---|
| **Q1** | **Resolved-in-plan.** | Compile-time is right, and §2.2's rejection of the runtime selector is the strongest passage in the document. The reversibility genuinely comes from two files, not from the selector — which the B3 fix makes *more* true, not less. If Ervin meant a runtime toggle, that is his call to reopen; the engineering case is made honestly and not averaged. |
| **Q2** | **Must-fix (F1, F2) — and then closed, here, not deferred.** | The plan deferred this to "check every `ORDER BY` before Step 5" and the sweep is cheap enough to have done: across all ten R2 columns the tree yields **exactly one** hit. But the deferred wording would have missed it anyway — it says `ORDER BY`, and the half that returns wrong rows is the `WHERE`. `'9' < '10'` is FALSE lexicographically; a `NULL`→`INTEGER 0` on either side compares against TEXT by storage class unconditionally. |
| **Q3** | **Acceptable-open.** | `routings.est_cost_huf` → R2 is defensible and correctly flagged as the one R1 exception: `Option<Decimal>` in Rust, never on the NAV wire, PDF, or ledger totals, and R1 would force a "what is HUF's minor unit for an estimate" product decision to serve consistency alone. The plan's own discomfort with a rule that has an exception is the right instinct and the wrong trade here. |
| **Q4** | **Acceptable-open — and the strictness is correct.** | Converting the five quoting `f64` money columns rather than carrying them as `REAL` is right, and §3.2 D's pre-commitment ("if Step 8 overruns, *stop* — do not migrate as `REAL`") is exactly the rule-11 guard that stops a later session taking the easy branch. Do not soften it. One naming fix only (F4): the table is `quote_price_snapshots`. |
| **Q5** | **Resolved — the concern dissolves entirely.** | Not "audit 21 and expect it to grow": there are **5**, and all 5 conflict targets are already the declared `PRIMARY KEY` (`inventory_balances`, `quote_price_snapshots`, `quote_pricing_jobs` ×2, `restore_lock`). SQLite resolves against a PK's implicit unique index exactly as DuckDB does. Zero indexes added, zero rewrites, and the `[[no-sql-specific]]` tension the plan invented does not exist. Notably this was G-1's error — comment lines counted as executable — reproduced two rows below the correction. |
| **Q6** | **Acceptable-open.** | Splitting 8 `ALTER` lines out of the `.sql` files beats owning a load-time rewriter forever (rule 12). "A family's schema now lives in two places" is real but small, and the `CREATE`-stays-SQL / `ALTER`-moves-to-Rust split is a legible line. |
| **Q7** | **NO-GO as written (B1). The plan asked the adversarial to check the comparison for circularity; the circularity is not there — the *lossiness* is.** | The three-way head-hash comparison is **not** circular: `verify_chain` recomputes `compute_entry_hash` (`chain/verify.rs:50`), so a BLOB/TEXT class error would surface. The defect is elsewhere and larger. `MirrorEntry` carries no signing columns and `to_entry()` nulls all three; mirror-as-source therefore strips S441/ADR-0087 from the whole migrated ledger, drops `audit_ledger_anchors`, and passes every gate — because `compute_entry_hash` deliberately excludes those fields. Separately, the flat head-equality would **hard-stop on mirror-ahead-of-DB**, i.e. on the 2026-07-19 scenario cited as the *justification* for mirror replay. Both fixed above. |
| **Q8** | **Acceptable-open.** | Dropping quoting job history is correct for a disposable DEV DB, and §11.1 already carries the "wrong for prod" flag forward explicitly. |
| **Q9** | **Resolved-in-plan — and the divergence from ADR-0107 §4.1 is the better call.** | A gate deleted is a gate that cannot protect the state you roll back to. Under a rollback-only constraint that is not a preference, it is a requirement. Rule 12's objection to dead machinery does not apply to machinery guarding a live rollback target. |
| **Q10** | **Must-fix (F7).** | The plan's "assumed safe — it strictly sees *more*" is the one place a load-bearing engine-semantics claim is asserted rather than pinned, over 84 sites, in the class that caused five of July's incidents, deferred to a step that runs beside the family it guards. It is also **coupled to Q11** in a way the plan does not notice: a finite `busy_timeout` is what converts a nested `read()`-inside-`write()` from an immediate self-deadlock into a silent hang. |
| **Q11** | **Acceptable-open, folded into F7.** | Leaving the number to a Step-2 measurement is fine. Choosing it *without* Q10's nesting audit in hand is not — the audit tells you whether a timeout is a backpressure knob or a hang. Decide them together. |

### 13.3 Deferral ledger additions

| Item | Disposition |
|---|---|
| `nav_xml.rs:1788` write path is `f64` while `:2658` read path is exact `Decimal` — **rule-7 fork on the NAV VAT rate**, pre-existing, engine-independent | Closed by B2 option (a). If option (b) is chosen instead, this stays open and gets its own PR. |
| `reports.rs:871` `unwrap_or(0)` fail-open on the ÁFA report — **pre-existing today, on DuckDB** | Closed by F5 in Step 5. Worth noting it is a live rule-11 defect right now, independent of any engine. |
| `MirrorEntry` cannot round-trip a signed entry — the mirror is a divergence detector, not a backup, and ADR-0030's own comment says so | **Out of scope**, recorded because B1 is the first time that design limit has had a consumer that assumed otherwise. If the mirror is ever to be a recovery source, that is its own ADR. |
| `information_schema` executable-site count = **4** (`print_invoice.rs:926`, `:986`, `duckdb_store.rs:427`, `quoting_materials.rs:1376`) | Verified — §1.1 G-3 is correct as written. `duckdb_store.rs:427`'s `.ok()` → `false` fail-open is real and correctly routed to Step 4. |
| DDL census: 105 `.rs` + 8 `.sql` executable + 1 dynamic = **114** | Verified exactly. §1.2 and §4.2 are correct. |

---

## 14. Self-audit — the eleven items, closed

Every entry was re-measured against the tree at `b7d5c61` for this revision;
nothing below is carried over on §13's word. Line numbers are as-measured.

### 14.1 Blockers

| # | Resolution | Evidence it is closed |
|---|---|---|
| **B1** | **The carry is inverted.** §6.3 now makes the `audit_ledger` **table** the source of truth and the mirror a three-arm cross-check; `audit_ledger_anchors` is a named carry table with its column types taken from the DDL rather than by analogy. The gate gains two count equalities that hard-stop. | Loss confirmed: `MirrorEntry` has no signing fields (`crates/audit-ledger/src/mirror.rs:112–129`); `to_entry()` nulls all three, in its own comment (`:206–214`). Blindness confirmed: `compute_entry_hash` excludes them, so `verify_chain` (`chain/verify.rs:25`) passes. **`verify_chain_signed` also passes** — its anti-strip arm is `(Some(sid), _, _)` (`verify.rs:138–144`) and mirror replay nulls `session_id` too, so every entry lands on `(None, _, _) => legacy — allowed` (`:146`); with anchors dropped the verdict returns **`fully_anchored: true`** (`:188`). Fix: §6.3 "The ledger inversion", the two `COUNT(*)` checks, and the rule that `fully_anchored` is inadmissible. Anchor typing corrected against `session/anchors.rs:33–43` — `chain_head_hash_at_anchor` is a hex `VARCHAR`, **not** `BLOB`, because `anchor_preimage` consumes it as `&str` (`verify.rs:161`). |
| **B1 (2026-07-19 confirmation)** | The check **does** cover the cited scenario — and the scenario refutes the argument it was cited for. | `docs/runbooks/audit-mirror-defork-20260719.md` @ `a8a6da3`: *"mirror forked from the DB at seq 8056 (mirror 8060 > DB 8058)… The DB is authoritative… 8058 rows, contiguous, zero duplicate seqs"*; the repair **discarded the divergent mirror tail and rebuilt the mirror from the DB.** So: (i) the shape is *mirror ahead of table* → §6.3 arm 2 fires, **stops**, routes to `heal_from_mirror_ahead` (`mirror.rs:873`) / `AppendError::MirrorAheadOfDb` (`error.rs:125`), and re-runs — where the first draft's flat equality would have stopped with no route, and mirror-as-source would have **adopted the five known-bad entries as the migrated record of account**; (ii) the incident's own resolution treated the DB table as authoritative, which is the inversion B1 asks for. The plan's justification for mirror-as-source was a misreading of its own evidence. |
| **B2** | **The claim is narrowed to two enforceable forms and T-5(d) is redefined, not weakened.** §3.3 N-1: no `f64` on any money-*amount* or quantity path, **zero allowlist**, green today. §3.3 N-2: a percentage *display* derived from INTEGER basis points may use `f64` only where value-exact — **allowlist of exactly one site, may only shrink**. Step 5 converts it to `Decimal` and the allowlist empties. | Site confirmed: `apps/aberp/src/nav_xml.rs:1788` = `format!("{:.2}", vat_rate_basis_points as f64 / 10000.0)`, on a `u16` (`:1783`). Full `f64` sweep of `nav_xml.rs` + `modules/billing/src` + `crates/invoice-pdf/src` returns **one live hit** — `:1788` — plus two doc comments (`nav_xml.rs:2657`, `billing/domain/invoice.rs:21`). So N-1 is true today and T-5(d) lands green rather than red on day one; N-2's allowlist is exactly `{nav_xml.rs:1788}`. Rule-7 fork confirmed against the exact inverse at `nav_xml.rs:2658–2663`. Conservative branch marked in §3.3: convert, because a permanent exemption is the branch that survives by weakening. |
| **B3** | **WAL made a first-class artefact of the snapshot/restore contract, and the `.gitignore` gap measured rather than assumed.** §6.2 now: snapshot-before-restore (step 4), atomic all-or-none restore of `aberp.duckdb` + `.wal` + mirror + every `.bak` via a verified staging dir (step 5), never main-alone, never WAL-deleted. §2.5's map and Step 1's manifest both name `.wal`. | `git check-ignore` run over every artefact class (§2.5's table): coverage comes from **four** globs — `*.duckdb` `*.duckdb.wal` `*.duckdb-wal` (`.gitignore:50–52`), `*.log` (`:99`), `*.bak` (`:111`) — not the `*.duckdb*` the first draft claimed. Confirmed **unignored**: `aberp.sqlite`, `aberp.sqlite-wal`, `aberp.sqlite-shm`, **`.aberp-premigration-*/`, `.aberp-rolledback-*/`**. The snapshot dirs are the dangerous half — a byte copy of the whole DB in a public repo — and were missing from §13's list. Three lines land in **Step 1**, pinned by **T-17**. (Note `aberp.sqlite.audit.log` was already covered by `*.log`.) |
| **B4** | **The migrator takes the writer lock and the gate re-reads independently.** §6.3 and Step 4: `acquire_or_refuse(db_path, tenant, "migrate-to-sqlite")`, held for the whole run, **refuses — never waits**; additionally refuses on a non-empty `aberp.duckdb.wal`; and the reconciliation gate is a **separate invocation after the migrator exits**, so no number it compares was produced by the migrator. | API exists and is the tree's own: `apps/aberp/src/db_writer_lock.rs:111` (`acquire_or_refuse`), `:83` (`try_acquire`), keyed dir+tenant at `lock_path_for` (`:73`) — which is why it spans both engines (§1.1 G-7). Precedent: the 2026-07-19 repair tool "refuses while the whole-DB writer flock is held" (`a8a6da3`). Read-only open confirmed **absent**: `access_mode`/`read_only`/`READ_ONLY` over `apps/`, `crates/`, `modules/` returns **zero** non-test hits, so Step 4 states it as capability to build and pins it with **T-19**. |

### 14.2 Must-fixes

| # | Resolution | Evidence it is closed |
|---|---|---|
| **F1** | Both halves of `low_stock_products` fold into Rust in Step 7 — **and a second site was found.** | `crates/aberp-inventory/src/repository.rs:548` (`AND COALESCE(stock_qty, 0) < COALESCE(min_stock, 0)`) and `:549` (`ORDER BY (COALESCE(stock_qty,0) - COALESCE(min_stock,0)) ASC`). **New this revision:** `:585` in `count_low_stock_products` (`:580`) carries the *identical* predicate — its own doc comment says "same predicate" — and is **not** in §13's F1, which named the function rather than the predicate. Both fold in one Step-7 commit; the function already parses both columns to `Decimal` from the `CAST(… AS VARCHAR)` projections at `:542–543`, so no new query and no new dependency. §1.2's comparison count corrected 1 → 2. |
| **F2** | T-8 gains `-`, `/`, and a full comparison arm (`<` `>` `<=` `>=` `BETWEEN` `MIN(` `MAX(` `ORDER BY`), and must be mutation-verified against **both** sites. | §8 T-8, rewritten. The mutation requirement is explicit that reddening on `:548–549` is **not** evidence of coverage for `:585` — the two are separate mutations — and that the gate must red on `COALESCE(col, 0)`-wrapped operands, the form both real sites take. |
| **F3** | Census corrected 21 → **5**; all 5 targets confirmed to be the declared `PRIMARY KEY`; rewrite work **removed**; M12 re-scoped to confirmation tests. | Executable sites: `material_inventory.rs:555`, `supplier_prices.rs:470`, `quote_pricing_jobs.rs:415` + `:476`, `restore_from_nav_outgoing.rs:326`. Non-executable: 16 doc comments + `quote_pricing_jobs.rs:3112` (a test assertion string). PK match verified statement-by-statement: `material_inventory.rs:235`, `supplier_prices.rs:429`, `quote_pricing_jobs.rs:248`, `restore_from_nav_outgoing.rs:270`. §1.2, §4.3 and M12 all updated; **zero index additions, zero rewrites**, and Q5 marked *dissolved* in §10. |
| **F4** | Three non-existent table names removed; `invoice_line.quantity_dec` added; every remaining name re-grepped. | `purchase_order_sequence` → **`po_number_state`** (`apps/aberp/src/purchasing.rs`); `purchase_order`/`purchase_order_line` → **plural**; `supplier_prices` → **`quote_price_snapshots`** — `CREATE TABLE … supplier_prices` / `FROM supplier_prices` / `INTO supplier_prices` all return **zero**; it is a module name and the column is declared at `supplier_prices.rs:428`. `quantity_dec` added to §3.2 C with the S157 ladder (`MIGRATE_S157_SQL`, `duckdb_store.rs:355–358`) and its trigger `quantity_column_is_integer` (`:423`, `.ok()` → `false`). |
| **F5** | **Recorded explicitly as a live pre-existing production defect**, with the in-migration-vs-separate-cut question answered rather than folded in silently. | `apps/aberp/src/reports.rs:872` = `Ok(decimal_str_to_i64(&s).unwrap_or(0))`, on the ÁFA report, running on DuckDB **today**. Siblings found at `:827` and `:1279` (also new this revision). Disposition, stated in **both** §3.4 and §9: **fixed in-migration**, Step 5, same commit as the `:861` fold — because the fix *is* the fold (its `Result` replaces the swallow) and splitting a fail-open from the code that makes it reachable is how it survives. Two guards attached: the Step-5 PR body must name it as pre-existing so it is not miscounted as migration collateral, and §9 states that if Step 5 is deferred or the engine decision reopens at Step 4, it **reverts to owing its own PR** rather than lapsing. |
| **F6** | The engine↔path refusal becomes a **pure function taking the engine as an argument**, so both arms are mutation-verifiable in Step 1 with no feature enabled. | Step 1, with the signature written out: `engine_path_agrees(engine: Engine, path: &Path) -> Result<(), EngineMismatch>` — no `cfg!`, no env, no fs. Step 3 adds only the three-line `cfg!` caller and re-runs T-13 end-to-end. The generalised rule is stated: *a refusal whose test cannot be written yet is not landed yet.* This closes the gap where C-I's load-bearing property would have been unpinned across Step 2 — the step that links `rusqlite`. |
| **F7** | Q10's audit is **exhaustive, gates Step 5, has a stated denominator**, and Q11's number is chosen with it. | Step 3, rewritten. Denominator measured and published: the 84 Handle sites are **50 `read()` / 34 `write()`** (§1.2), so the audit covers **all 50** with the 34 as axis-(b) reachability context, closed under calls (ADR-0106's discipline, not line-local). Output is a 50-row table in the PR body — *"an audit with an unstated denominator is a sample."* Three pins: **T-20** pins the WAL snapshot claim in **both** directions (autocommit sees the commit; in-transaction must **not**), **T-21** pins the loud abort on nested `read()`-inside-`write()`, **T-3d** asserts the pragma value. **Q11 closed: `busy_timeout = 5000 ms`, conditional on T-21 landing first**, revisable downward on Step 2's measurement, raising it requires re-arguing T-21. |

### 14.3 What was preserved

Unchanged, because it survived attack: the nine-step ordering; **Step 4 as the
cheap abort point**; the compile-time selector (§2.2 D1) and its rejection of the
runtime alternative; **frozen, not deleted, census and fork gates** (§8 — the
deliberate divergence from ADR-0107 §4.1); and `db_writer_lock`'s dir+tenant
mutual exclusion (§1.1 G-7), which B4 now leans on directly.

### 14.4 Residuals — stated, not hidden

Nothing in B1–B4 or F1–F7 is left open. Three things are **deliberately** out of
scope and are recorded in §9 rather than closed here: the
`material_inventory.*_qty` / `stock_movements.qty_delta` representation
divergence (rule 7, needs its own decision); `MirrorEntry`'s inability to
round-trip a signed entry (a design limit of ADR-0030, not a defect — B1 was
simply its first consumer that assumed otherwise); and `verify_chain_signed`'s
`session_id`-keyed anti-strip check together with `ChainVerdict.fully_anchored`
reading `true` on an empty anchor set — worked around inside this plan by
asserting counts rather than verdict flags, but a trap for the next consumer.

Three findings in this revision are **new** — not in §13 — and each is the same
shape: a sweep that stopped at the named site rather than the named property.
`repository.rs:585` (F1), the two sibling `unwrap_or(0)`s at `reports.rs:827`
and `:1279` (F5), and the two snapshot directories missing from B3's
`.gitignore` list. They are recorded here rather than folded in quietly because
the pattern is the finding: **scope a sweep to the predicate, not to the
function the last reviewer happened to name.**

### 14.5 Verdict

**ADR-0108 is GO-ready for execution.** All four blockers and all seven must-fixes
are closed against measured evidence; the execution session begins at Step 1.
This document still authorises **no prod work** (C-II, §11) and the decision to
continue past **Step 4** remains a live checkpoint, not a formality.
