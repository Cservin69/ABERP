# ADR-0108 — SQLite (WAL) migration: the executable DEV plan

- **Status:** Proposed — **plan only**. This document authorises no engine code,
  no schema change, and no data migration. It is the artefact a later execution
  session works from, one gated step at a time.
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
| `ON CONFLICT` | 21 | needs an explicit conflict target on SQLite |
| `IS NOT DISTINCT FROM` | 8 | needs SQLite ≥ 3.39 |
| `LIKE` | 2 | unescaped metacharacters (M11) |
| `ATTACH` / `load_extension` / `CREATE TRIGGER` / `CREATE VIEW` / `WITH RECURSIVE` / `OVER (` | **0** | PR #49 confirmed |
| **SQL-side arithmetic on a money/quantity column** | **6** | §3.4 — the item neither doc names |
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
  aberp.duckdb.audit.log          ← the mirror. READ by the migrator; never written by it.
  aberp.sqlite                    ← created by the migrator. Deleted by rollback.
  aberp.sqlite-wal  / -shm        ← WAL siblings. Deleted by rollback.
  aberp.sqlite.audit.log          ← the SQLite build's own mirror (mirror_path_for appends
                                     the suffix to the db path, so the two never collide).
  .aberp-db-writer.test.lock      ← shared by BOTH builds (dir+tenant keyed) → mutual exclusion.
  .aberp-premigration-<ts>/       ← the Step-2 snapshot + manifest. Rollback's restore source.
```

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
| `purchase_order_line.unit_price_minor` / `line_total_minor` | `BIGINT` | `i64` | `INTEGER NOT NULL` | no |
| `purchase_order.subtotal_minor` / `vat_minor` / `total_minor` | `BIGINT` | `i64` | `INTEGER NOT NULL` | no |

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
| `supplier_prices.cost_per_kg_eur` | `DOUBLE` | `f64` | **`TEXT` (R2)** |
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
`invoice_sequence_reservation.number`, `purchase_order_sequence.next_number`,
`invoice_line.vat_rate_basis_points`, `invoice.fiscal_year`,
`partners.issued_invoice_count`, `email_relay_queue.byte_size`,
`audit_ledger.seq`, `audit_ledger.time_mono`, `bom_revisions.rev_number`,
`bom_revisions.line_count`, `routings.sequence`, `routings.est_time_min`.

**`vat_rate_basis_points INTEGER` is why VAT never touches a float** — 27% is
`2700`, not `0.27`. That property is preserved verbatim and is worth naming: the
ÁFA percentage on the NAV wire is rendered from an integer basis-point count, so
F-6a's float-coercion class cannot reach the VAT rate itself. It reaches
`exchange_rate` and `huf_equivalent_total`, which is what §3.2 B and C close.

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
| VAT rate | `vat_rate_basis_points` INTEGER | `i64` | basis points | `nav_xml`: `bp/100` as decimal string, integer arithmetic |
| exchange rate | `invoice.exchange_rate` TEXT | `String` | `Decimal::from_str` | printed invoice only (ADR-0037 §1.a) |
| HUF equivalent | `invoice.huf_equivalent_total` **INTEGER** | **`i64`** | `RateMetadata.huf_equivalent_total` | NAV wire + PDF |
| ledger hashes | `*_hash` BLOB | `Vec<u8>` | `EntryHash` | `verify_chain` |

**There is no point in this trace where an `f64` exists.** The round-half-even HUF
conversion (`huf_equivalent_round_half_even`, ADR-0037 §1.c / C11) already runs on
`rust_decimal::Decimal` and lands on `i64`; §3.2 B removes the last string↔decimal
round-trip that stood between that `i64` and the column. The property test T-5
(§8) is what makes this claim falsifiable rather than asserted.

### 3.4 The six SQL-side arithmetic sites that must move to Rust

| Site | Statement | Why it breaks | Fix |
|---|---|---|---|
| `apps/aberp/src/reports.rs:800` | `CAST(SUM(CAST(il.quantity AS DECIMAL(38,6)) * il.unit_price) AS VARCHAR)` | **The sharp one.** Under R2 `quantity` is `TEXT`; SQLite coerces `TEXT * INTEGER` to `REAL` and the report silently becomes float money. | Select `quantity, unit_price` per row; fold in Rust with `Money::checked_mul_decimal` (already exists, `money.rs:54`) + the existing `decimal_str_to_i64` round-half-even (`reports.rs:1011`). |
| `apps/aberp/src/reports.rs:861` | `CAST(COALESCE(SUM(i.huf_equivalent_total), 0) AS VARCHAR)` | Under §3.2 B the column is `INTEGER`; `SUM` over INTEGER is exact but **raises on i64 overflow** and the `CAST … AS VARCHAR` round-trip is now pointless. | `SELECT huf_equivalent_total` and `checked_add` in Rust; loud on overflow. |
| `aberp-inventory/src/repository.rs:222` | `CAST(COALESCE(SUM(qty_delta),0) AS VARCHAR)` (cache rebuild, in-tx) | `qty_delta` becomes `TEXT` → `SUM` coerces to `REAL`. **This is the stock-cache invariant** `stock_qty = SUM(qty_delta)`. | Select the column, fold `Decimal` in Rust. |
| `aberp-inventory/src/repository.rs:629` | same, batch rebuild | same | same |
| `aberp-inventory/src/bin/rebuild_stock_cache.rs:29` | same, CLI one-shot | same | same |
| `reports.rs` `MAX(...)` / `COUNT(*)` sites | — | unaffected (no money arithmetic) | none |

**This work is not optional and not deferrable to a cleanup phase.** Three of the
five sites are the inventory cache-rebuild path, which is a *write* — a float
there writes a wrong `stock_qty` back into the products cache. They land in the
same step as their family (Steps 5 and 7).

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
| `ON CONFLICT` | 21 | **Each needs a real `UNIQUE` index.** ADR-0019's "no DB-level UNIQUE" posture means several may not have one — SQLite's upsert requires an explicit conflict target that resolves to a unique index. **This is a genuine tension with `[[no-sql-specific]]` and §2.1's "no invariant moves into the DDL"**: a `UNIQUE` index added *only* to satisfy `ON CONFLICT` is a constraint in the schema. Conservative resolution: audit all 21 in Step 3; where a unique index already exists, use it; where none does, **rewrite the upsert as an explicit in-transaction `SELECT`-then-`INSERT`/`UPDATE` under `BEGIN IMMEDIATE`** rather than adding a constraint. Flagged — this is the item most likely to grow. |
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
| **M12** | Bundled SQLite ≥ 3.39 for `IS NOT DISTINCT FROM` (8 sites); **audit all 21 `ON CONFLICT` sites for a resolvable conflict target** (§4.3) | Step 3 | T-11 (version) + the Step-3 audit is itself a gate: every one of the 21 either names an existing unique index or is rewritten |

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
4. If `--from` is given, or if `aberp.duckdb`'s digest does not match the
   pre-migration manifest, restore `aberp.duckdb` + `aberp.duckdb.audit.log` from
   the snapshot dir.
5. `cargo build` **without** `--features sqlite-engine` (the default).
6. **Verify, and this is the part that makes it "verified rollback":**
   - `aberp verify-chain` genesis→head on the restored DuckDB DB — must be `OK`;
   - per-table row counts equal the pre-migration manifest;
   - the head `seq` equals the manifest's;
   - the mirror's last `entry_hash` equals the DB head's.
7. Print a one-line PASS/FAIL. **Non-zero exit on any mismatch.** Never "restored
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
| **`audit_ledger`** | **Replay from the fsync'd mirror** (`aberp.duckdb.audit.log`, 1.3 MB), not from the DuckDB table. | The mirror is ADR-0030's primary evidence and is the *more* durable of the two — the 2026-07-19 incident had five entries in the mirror and gone from the DB. Replaying the mirror and then running `verify_chain` genesis→head on the SQLite side is a **stronger** exit criterion than copying the table. Any DB-vs-mirror divergence surfaces here as a hard stop, which is exactly what we want to know before Step 5 completes. |
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
- SQLite head `entry_hash` == DuckDB head `entry_hash` == mirror tail `entry_hash`;
- `PRAGMA integrity_check` == `ok`;
- **`SELECT typeof(col)` over every row of every column in §3.2 A–G matches the
  declared class** — the M1 pin, applied to migrated data and not only to fresh
  writes.

Any mismatch → the step fails, `rollback_to_duckdb.sh` runs, nothing is force-fixed.

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
  transaction default (**M5**); the `ON CONFLICT` audit of all 21 sites (**M12**,
  §4.3) with each site's resolution recorded.
- *Verified by:* T-9 (M8 fail-loud, both arms), T-6 (M5 interleave), the 21-site
  audit table in the PR body.
- *Rollback:* revert. Default build unaffected (feature off).

**Step 4 — The migrator + the reconciliation gate. Read-only against DuckDB.**
- *Changes:* `aberp migrate-to-sqlite` one-shot: opens DuckDB **read-only**, opens
  a fresh `aberp.sqlite`, replays the ledger from the mirror, carries the families
  per §6.3, applies §3's representation rules, runs the reconciliation gate (§6.3),
  and **refuses on any mismatch**; the `information_schema` → `pragma_table_info` /
  `sqlite_master` rewrites (§4.3 G-3) and the `DROP COLUMN IF EXISTS` guards (G-4).
- *Verified by:* run against a **copy** of the DEV DB in the scratchpad. Exit
  criterion — **the real DEV tenant DB migrates and the reconciliation gate passes
  green, including the `typeof()` sweep and `verify_chain` genesis→head.**
- *Rollback:* delete the produced `.sqlite`; revert. **This is ADR-0107 §4.1's
  "stop here having spent little" gate. If it fails, stop and re-open the engine
  decision.**

**Step 5 — The fused transactional core: `audit_ledger` + `modules/billing` +
invoice-sequence allocation.** *(the whole point of the exercise)*
- *Changes:* the 25 + 4 + 2 + 1 + 1 DDL sites (§4.2) via `ensure_columns`; `STRICT`
  DDL; §3.2 B's `huf_equivalent_total` `DECIMAL→INTEGER` bind/read change; §3.2 C's
  `exchange_rate` + `quantity` `DECIMAL→TEXT`; §3.4's `reports.rs:800,861` folds
  moved to Rust; R3's BLOB binds audited across ~30 hash sites; S444's durable
  ledger-derived invoice-number floor carried across **unchanged** (belt and braces
  stay).
- *Verified by:* T-1, T-2, T-4 (**NAV XML + PDF byte-identity across the whole DEV
  invoice set**), T-5 (money property tests), T-6, T-14 (crash / number-durability),
  and the full reconciliation gate.
- *Rollback:* `rollback_to_duckdb.sh`. **This is the step where the rollback drill
  is not a formality — run it, boot DuckDB green, re-apply, and say so in the PR.**

**Step 6 — Adversarial checkpoint.** Not a code step. Rule 4 reserves full
adversarial review for the invoice→NAV/ÁFA path; Step 5 *is* that path. No further
family crosses until this closes.

**Step 7 — The remaining non-quoting families,** one at a time, rule-14 fused:
partners (+ **M11**, T-12) → products/inventory (incl. §3.4's three cache-rebuild
folds) → work orders/BOM → QA/QC → dispatch → purchasing → email/relay.
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
| **T-2** | `SELECT typeof(col)` over **every row** of every §3.2 column after migration → `'integer'` / `'text'` / `'blob'` / `'real'` as declared | M1 / F-6c |
| **T-3a–d** | `load_extension` errors; `ATTACH` errors; `CREATE TRIGGER`/`CREATE VIEW` rejected; each of `journal_mode`/`synchronous`/`fullfsync`/`busy_timeout`/`shared_cache` read back and asserted | M2/M3/M4/M7 |
| **T-4** | **Byte-identity**: for every invoice in the DEV DB, the NAV `InvoiceData` XML and the rendered PDF bytes are **identical** DuckDB vs SQLite | §3.3, the regulatory record |
| **T-5** | **Money property tests**: (a) `Decimal` round-trips through `TEXT` for 10⁵ generated values at scale 0–6 incl. trailing-zero forms; (b) `huf_equivalent_round_half_even` on `Decimal` → `i64` matches DuckDB's result for the whole DEV rate set; (c) `unit_price × quantity` folded in Rust equals the pre-migration DuckDB `DECIMAL(38,6)` aggregate for every invoice; (d) **no code path between column and emitted byte constructs an `f64`** — enforced as a `clippy`/grep gate over the billing + nav_xml + invoice-pdf crates | §3.1, §3.3, §3.4 |
| **T-6** | Two connections interleave read-head → append; must **not** produce two links off one `prev_hash`. Run with and without `BEGIN IMMEDIATE` | M5 / F-7a |
| **T-7** | `db_writer_lock_e2e` re-pointed at SQLite; **plus** a cross-engine test: a DuckDB `serve` holding the lock refuses a SQLite `serve` on the same tenant+dir | M6 / F-7b / §1.1 G-7 |
| **T-8** | Cut-gate grep: no `SUM(`/`*`/`+`/`AVG(` over any §3.2 A–D column name in any SQL string | §3.4 |
| **T-9** | `ensure_columns`: seeds a pre-migration schema and asserts every expected column exists after `ensure_schema`; **and** asserts `Err` when a column cannot be added, and `Err` when the table is absent | M8 / F-1c / D2a's shape |
| **T-10** | mode of `aberp.sqlite`, `-wal`, `-shm` == `0600`; tenant dir `0700` | M9 / F-5a |
| **T-11** | `sqlite3_libversion_number() >= 3051003` | M10 / M12 |
| **T-12** | `Árvíztűrő` vs `ÁRVÍZTŰRŐ` still matches the partner dedup guard; a `%` needle does not over-match | M11 / F-1b |
| **T-13** | The `ABERP_DB`↔engine boot refusal, both directions; and a refusal when the resolved path is under `~/.aberp/` | C-I, C-II |
| **T-14** | **Crash / number-durability**: `SIGKILL` the writer mid-invoice-issue ×N; on restart assert (a) `verify_chain` OK, (b) no invoice number is ever re-issued, (c) the mirror tail and DB head agree. This is the S444 regression, re-armed on the new engine. | ADR-0107 §4 rec. 2 |
| **T-15** | **Customer-journey e2e** (`[[feedback_customer_journey_e2e_gate]]`): quote → order → work order → dispatch → invoice → NAV submit → PDF → email, end to end on SQLite, asserting the invoice number, the ÁFA breakdown, and the PDF bytes. Re-run after **every** family step in Step 7. | the whole product |
| **T-16** | `PRAGMA integrity_check` == `ok` after every step | corruption |

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
| DEV DB measured mode **0644**; no code chmods the tenant DB — **true today, engine-independent** | M9 / Step 2, or a standalone 5-line PR now (PR #49 already flagged this) |
| `material_inventory.*_qty` is `DOUBLE` while `stock_movements.qty_delta` is `DECIMAL` — **two representations of one physical quantity** (rule 7) | **Out of scope.** Recorded because migrating both as-is under `STRICT` makes the divergence look sanctioned. Needs its own decision. |
| `qc_inspections.deviation` is a derived `REAL` driving a pass/fail verdict | Out of scope; flagged in §3.2 E |
| ADR-0107 / the frozen baseline / its header disagree on the in-serve read-fork count (**14 / 13 / 9**) | Out of scope for the migration; a stale frozen baseline is the exact class PR #43 existed to prevent → its own PR |
| `aberp-mes::ledger_writer::write_one` appends through a fresh in-serve connection while the write-fork gate reports ZERO | ADR-0107 §5 says close it **by hand now** — a forked *append* forks the ledger under **any** engine. Independent of this plan; should land before Step 5. |
| The S392 NAV pre-flight is dead (0 `check_performed` in 225 mirror entries) | Orthogonal, engine-independent, and ADR-0107 §5 calls it the most under-weighted open item. Not this plan. |
| ADR-0107 §1.3 finding F1 (is a forked read stale, or was D2a row loss?) is unsettled | Needs a measurement; **the migration makes it moot** but does not answer it |
| ADR-0107 §2 lists `db_writer_lock` as retirable; ADR-0107 §3 B-cost-1 says money is already integer; ADR-0107 §4.1 Phase 0 does not scope the DDL rewrites | Amended in Step 1's PR body per PR #49's own deferral ledger, plus §1.1 G-2's `.sql` correction which PR #49 also missed |

---

## 10. Open questions flagged for the adversarial

Where a choice was open I took the conservative branch and recorded it. These are
the ones most worth attacking.

| # | Question | My call | Why it might be wrong |
|---|---|---|---|
| **Q1** | Compile-time cargo feature vs runtime engine selector (§2.2 D1) | Compile-time | Ervin said "config/feature selector". If he meant runtime-togglable without a rebuild, D1 is wrong and the cost is a trait layer over 449 + 120 + 84 sites. |
| **Q2** | `TEXT`-decimal vs scaled-integer for quantities/rates (§3.1 R2) | `TEXT` | Smallest diff, matches today's bind exactly, no overflow class. But it forbids SQL-side aggregation forever (§3.4) and makes ordering lexicographic — **check every `ORDER BY` on a §3.2 C column before Step 5.** |
| **Q3** | `routings.est_cost_huf` → `TEXT` (R2) rather than `INTEGER` (R1) (§3.2 B) | R2 | It is the one money column not following R1. Defensible (operator estimate, never on the wire) but it is an inconsistency in a rule whose value is its exceptionlessness. |
| **Q4** | The five quoting `f64` money columns (§3.2 D) — Step 8, converted to `Decimal` | Convert, do not carry as `REAL` | It grows Step 8 materially. The alternative (carry as `REAL`, fix later) is the branch I explicitly forbade; if the adversarial thinks that's over-strict, this is the place to say so. |
| **Q5** | The 21 `ON CONFLICT` sites: add `UNIQUE` indexes, or rewrite as `SELECT`-then-write? (§4.3) | Rewrite; add no constraint | Adding indexes is far less code but moves an invariant into the DDL, against ADR-0019 / `[[no-sql-specific]]` / §2.1. **This is the item most likely to blow up Step 3's estimate.** |
| **Q6** | `.sql` migration files: split the `ALTER` lines out into `ensure_columns` (§4.2) | Split (8 lines move) | The alternative is a load-time rewriter we would own forever. But splitting means a family's schema now lives in two places. |
| **Q7** | Ledger crosses by **mirror replay**, not table copy (§6.3) | Mirror replay | Stronger exit criterion, and the mirror is the more durable artefact. But it makes Step 5 depend on the mirror being complete — if the mirror is itself short, the migration inherits the gap. The reconciliation gate compares against the DuckDB table precisely to catch that; the adversarial should check that comparison is not circular. |
| **Q8** | Drop quoting job history rather than write an `f64 → Decimal` converter (§6.3) | Drop | Correct for a disposable DEV DB; **wrong for prod**, and §11 must not inherit it silently. |
| **Q9** | Keep the census / fork gates frozen instead of deleting per family (§8) | Keep | Diverges from ADR-0107 §4.1 Phase 2. Costs nothing but leaves dead machinery standing longer than rule 12 likes. |
| **Q10** | Is `read()` returning a real second connection (rather than a `try_clone`) a behaviour change anywhere? (§2.4) | Assumed safe — it strictly sees *more* | 84 call sites. If any depends on reader/writer sharing an uncommitted view, this is where it breaks. **Not exhaustively audited; Step 3 must audit it.** |
| **Q11** | `busy_timeout` value (M7 says "explicit and finite", no number) | Not chosen here | Too short → spurious `SQLITE_BUSY` on the invoice path; too long → a deadlock reads as a hang. Needs a number, measured, in Step 2. |

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
  "no float touches a monetary value" made falsifiable by T-5.
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
