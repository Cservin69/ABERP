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
| `Handle::write()` / `.read()` call sites (non-test) | **238** | the Handle seam's blast radius. **CORRECTED 2026-07-31 (finding R-2): was 84.** Re-measure with `tools/adr0108_handle_census.sh`, not a grep — see the note under this table. |
| `ON CONFLICT` — executable | **5** | The raw grep returns 21; **16 are doc comments and 1 is a test assertion string** (`quote_pricing_jobs.rs:3112`). This is `ALTER COLUMN`'s exact error (G-1), reproduced. The 5 real sites are `material_inventory.rs:555`, `supplier_prices.rs:470`, `quote_pricing_jobs.rs:415`+`:476`, `restore_from_nav_outgoing.rs:326`. All 5 conflict targets are already the declared `PRIMARY KEY` → **zero index work**. See §4.3. |
| `IS NOT DISTINCT FROM` | 8 | needs SQLite ≥ 3.39 |
| `LIKE` | 2 | unescaped metacharacters (M11) |
| `ATTACH` / `load_extension` / `CREATE TRIGGER` / `CREATE VIEW` / `WITH RECURSIVE` / `OVER (` | **0** | PR #49 confirmed |
| **SQL-side arithmetic on a money/quantity column** | **7** | §3.4 — the class neither source document names. The 7th is `aberp-inventory/src/repository.rs:549`, a `-` (subtraction) inside an `ORDER BY`. |
| **SQL-side `<` comparison on an R2 (TEXT-decimal) column** | **2** | `repository.rs:548` (`low_stock_products`) and `repository.rs:585` (`count_low_stock_products`, whose own doc comment says "same predicate"). The only Q2 lexicographic-ordering breaks in the tree; the second was found re-running the sweep for this revision and is **not** in §13's F1. §3.4. |
| **Read-only DuckDB opens** (`access_mode` / `read_only` / `READ_ONLY`) | **0** | Across `apps/`, `crates/`, `modules/`, non-test. Step 4's read-only open is capability **to build**, not to assume. §6.3, B4. |
| `.read()` / `.write()` split of the 238 Handle sites | **102 / 136** | **CORRECTED 2026-07-31 (finding R-2): was 50 / 34.** §7 Step 3's two-axis audit (F7) is over the **102** `read()` sites; the **136** `write()` sites are the nesting context for axis (b). The script also reports the test surface separately (48 / 73 = 121 when the audit was taken); that number moves with the suite and is **not** the audit denominator. |
| `duckdb::Error::DuckDBFailure` | 3 | the only `duckdb::` path with **no** same-named rusqlite twin |
| DEV DB / mirror on disk | 20.4 MB / 1.3 MB, mode **0644** | confirms PR #49 F-5a |

> **The Handle census was wrong by a factor of ~2.8, and how it was wrong
> matters (finding R-2, read-fork audit 2026-07-31).** The original number came
> from
>
> ```bash
> grep -rn '\.db\.read()\|\.db\.write()' --include='*.rs' apps crates modules | wc -l   # 84
> ```
>
> which is reproducible and wrong in two ways this repo had already been bitten
> by:
>
> 1. **It is single-line.** `serve.rs` overwhelmingly formats these chains as
>    `state\n    .db\n    .read()`, so receiver and method land on different
>    lines and the pattern cannot match. This is exactly the defect PR #43 (D1a)
>    found in the read-fork *scanner* and fixed there by going structural — the
>    same defect survived here because the census was a one-line grep.
> 2. **It requires the literal `.db.` prefix**, so every `Handle` bound to a
>    local (`db.read()`, `handle.read()`, `h.read()`, `svc.deps.db.read()`,
>    `state_for_task.db.read()`) is invisible.
>
> The census is now a script — **`tools/adr0108_handle_census.sh`** — which
> rejoins rustfmt-wrapped chains, is receiver-agnostic, and excludes the
> non-`Handle` `.read()`/`.write()` receivers **by name** (`boot_state`,
> `self.inner`, `self.registry`, `self.smtp_password` — all `RwLock`s), so a new
> non-Handle receiver shows up as a count change instead of being swallowed by a
> clever regex. It is a measurement tool, not a gate: re-run it to check these
> numbers are still true. Step 3's audit denominator is **102**, and an audit
> over 50 of 102 would have been a 49 % sample presented as exhaustive.
>
> **Neither opener census is a superset of the other.** The ADR-0098 opener
> census (81 openers / 41 fn-sites) and the ADR-0099 read-fork baseline (33
> entries + an 11-entry allow-list) have partially disjoint coverage: **4**
> fn-sites are in the opener census and in neither read-fork list, and **7**
> baseline entries are absent from the opener census (`Handle::open_default` and
> demo/new-tenant paths its token set does not recognise). Treat neither as the
> complete set. Per finding R-5 the census that matters for the *durability*
> hazard is the **ADR-0098 opener census (81)**, because the injury is a foreign
> connection's `close`, which does not respect a read/write partition — see §9.

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
`duckdb::Connection` signatures, and the 238 `Handle` call sites to dispatch through
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
  API-identical. **238** call sites are untouched (R-2 — was stated as 84).
- **Single-writer.** The writer `Mutex` stays. It stops being a correctness
  requirement and becomes a throughput choice, and it is still what makes the
  `BEGIN IMMEDIATE` discipline (M5) cheap to reason about in-process.
  **R-3 (2026-07-31), binding:** `Handle::read()`'s SQLite arm **also** takes
  that mutex (`lock_recovering()`), exactly as the DuckDB arm does — so a nested
  `read()`-inside-`write()` resolves against the Rust mutex and never reaches
  SQLite's busy handler, poison-recovery keeps engine parity, and the
  re-entrancy tripwire stays load-bearing. Recorded on `Handle::read()`'s
  doc-comment and pinned by T-21. **R-1 (2026-07-31):** "single-writer" was
  *already* not quite true before this migration — three read paths ran DDL
  through `read()`, escaping the mutex. Fixed; pinned tree-wide by
  `apps/aberp/tests/adr0108_no_ddl_on_read_handle.rs`.
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

> ⚠ **CORRECTED 2026-07-31 by Step 5's measurement (S450). `STRICT` does NOT
> protect an R2 column, and the sentence below that implies it does is
> narrowed here.** `STRICT` applies the ordinary affinity conversion and
> refuses only what cannot convert **losslessly**. REAL → TEXT always
> converts, so a float written into `quantity` or `exchange_rate` is
> **accepted and stringified** — and `typeof()` still reads `'text'`, so the
> T-2 sweep is blind to it as well.
>
> | declared | given a REAL | result |
> |---|---|---|
> | `INTEGER` (R1, money) | `1234.56` | `SQLITE_CONSTRAINT_DATATYPE` |
> | `INTEGER` (R1, money) | `1234.0` | accepted, converted to `1234` |
> | `TEXT` (R2) | `0.1 + 0.2` | **accepted, stored as `'0.30000000000000004'`** |
> | `BLOB` (R3) | `'abc'` | `SQLITE_CONSTRAINT_DATATYPE` |
>
> So R1 and R3 *are* enforced by the declared type; **R2 is not enforced by
> anything in the engine.** Its guards are exactly two, and they are now known
> to be the only two: the Rust-side bind (a `Decimal`, never an `f64`) and
> **T-8's cut-gate keeping arithmetic out of SQL** — which is what makes T-8 a
> load-bearing mitigation rather than a tidiness rule. Pinned by
> `strict_does_not_protect_an_r2_text_column_from_a_float`
> (`crates/aberp-db/tests/adr0108_money_representation.rs`).
>
> ⚠ **The sentence above was ASPIRATIONAL when it was written, and is not any
> more. Corrected 2026-08-01 (S452).** Step 6 measured that **T-8 had never
> been built**: three landed artefacts cited it, `tools/` held no
> implementation, and no workflow ran one — so for the whole of Steps 1-6 the
> claim "exactly two guards" described a set with **one** member (M-1). T-8 now
> exists and runs:
>
> | | |
> |---|---|
> | gate | `tools/cut_gate_money_arith.sh` (checks T8-1 scope, T8-2 liveness, T8-3 findings-vs-register, T8-4 register-is-a-ratchet) |
> | scanner | `tools/adr0108_money_arith_scan.awk` — the §3.2 A-D census as a const list, taint-propagating through `COALESCE`/`CAST`/`ROUND` wrappers |
> | register | `tools/adr0108_money_arith_pending_folds.txt` — the four §3.4 sites not yet folded; it can only shrink |
> | teeth | `tools/cut_gate_money_arith_probes.sh` — 14 mutations, 12 red / 2 stay-green |
> | wiring | `.github/workflows/cut-gate.yml`, beside the posture gate and its probes |
>
> **What is guarded today and what is not.** The gate is live over the whole
> existing tree (672 SQL statements, 295 files, `.rs` **and** the 7 `.sql`
> migration files) and its register is exact. Its teeth over the **real** §3.4
> fold sites land with Step 7, because those folds are Step 7's commit — until
> then the four sites sit in the register, where a *new* one cannot join them
> and a *folded* one cannot linger. Exposure meanwhile stays zero: none of the
> four families has crossed the seam, so all four statements still execute on
> DuckDB, where `DECIMAL` is a real decimal type.

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
>
> > ✅ **LANDED AS R2 2026-08-01 (S456, Step 7 Part D), and the flag stands.**
> > `migrate_work_orders` declares the column `TEXT` in a `STRICT` table and
> > carries the DuckDB string **verbatim** through
> > `migrate_billing::canonical_decimal` — the same validate-and-carry the Step-5
> > invoice family uses for `exchange_rate` and `quantity`, so the exception
> > introduces no new mechanism, only a different rule for one column. The gate
> > asserts `typeof(routings.est_cost_huf) = 'text'` on every row and folds the
> > column in Rust over `Decimal`; a `'real'` there would be F-6a's float-money
> > class arriving through this exception, which is the specific thing worth
> > watching. **This remains the only money column in the tree that is not R1**,
> > and the adversarial review it was flagged for has not happened yet.

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
`actual_minutes`; ~~`material_inventory.on_hand_qty`, `reserved_qty`,
`committed_qty`, `consumed_qty`, `qty`~~ (**moved out of E — see the correction
below**); `work_orders.actual_machining_minutes`;
`qc_inspection_plans.nominal_value`, `upper_tol`, `lower_tol`;
`qc_inspections.nominal_value`, `upper_tol`, `lower_tol`, `actual_value`,
`deviation`.

> ⚠ **Two entries in E deserve the adversarial's attention.**
> (a) `material_inventory.*_qty` are `DOUBLE` while `stock_movements.qty_delta` is
> `DECIMAL` — **two representations of the same physical quantity in one product**
> (rule 7). Neither source document notices. This plan does **not** fix it (out of
> scope, rule 3) but records it in the deferral ledger, because migrating both
> as-is preserves the divergence under `STRICT`, which makes it look sanctioned.
>
> > ⚠ **RESOLVED AT THE STORAGE LAYER 2026-08-01 (S455, Step 7 Part C), and the
> > "out of scope" above is narrowed rather than kept.** The adversarial's own
> > sentence is the argument: carrying them as `REAL` would have shipped a
> > `STRICT` schema, blessed by a green reconciliation gate, declaring a
> > quantity to be a float in one half of the product and exact in the other.
> > **All five columns cross as R2 `TEXT`** — the same canonical
> > `rust_decimal::Decimal` string `stock_movements.qty_delta` already uses —
> > **with zero value change**, enforced per value rather than asserted:
> > `migrate_products::canonical_decimal_from_f64` renders each `f64` as the
> > shortest decimal that round-trips it, refuses anything above the canonical
> > quantity scale of 6 (`DECIMAL(18,6)`'s), and refuses again unless the
> > `Decimal` converts back to the identical `f64`. **A value that cannot cross
> > exactly fails the migration; it is never rounded into range** (rule 11).
> > The five columns are therefore **out of category E and into category C**.
> >
> > What is *not* resolved is the **app-layer** modelling: `material_inventory.rs`
> > still holds these as `f64` in Rust while `aberp-inventory` holds its side as
> > `Decimal`. That is a change to a saga's arithmetic and its callers, it has
> > nothing to do with the engine, and it gets its **own §9 row** with the exact
> > tables and columns rather than riding along here (rule 3).
> (b) `qc_inspections.deviation` is a *derived* float on a dimensional-inspection
> record used for a pass/fail verdict. It is not money. Keeping it `REAL` is the
> conservative no-change call, flagged.
>
> > ✅ **`work_orders.actual_machining_minutes` CROSSED AS `REAL`, UNCHANGED,
> > 2026-08-01 (S456, Step 7 Part D) — category E holds for it, and the reason it
> > did not follow S455's five is worth stating rather than leaving to
> > inference.** S455 moved five columns out of E because they were one half of a
> > **measured** rule-7 divergence: the same physical quantity was `DECIMAL` in
> > `stock_movements` and `DOUBLE` in `inventory_balances`, so carrying both
> > as-is would have blessed the fork. `actual_machining_minutes` has no exact
> > counterpart — `routings.est_time_min` is an `INTEGER` *estimate* at a
> > different granularity (per-op, not per-WO), and the calibration consumer's own
> > columns (`quote_calibration.estimated_minutes` / `actual_minutes`) are
> > `DOUBLE` as well. There is no fork to close.
> >
> > ✅ **THE EIGHT QC MEASUREMENT COLUMNS CROSSED AS `REAL`, UNCHANGED,
> > 2026-08-02 (S457, Step 7 Part E) — note (b) above is followed, not
> > re-litigated, and the flag stands.** `qc_inspection_plans.{nominal_value,
> > upper_tol, lower_tol}` and `qc_inspections.{nominal_value, upper_tol,
> > lower_tol, actual_value, deviation}` are `REAL` in a `STRICT` table and
> > bit-identical per row. They did not follow S455's five for the same reason
> > `actual_machining_minutes` did not: **no exact counterpart exists anywhere in
> > the tree**, so there is no rule-7 fork to close — a dimensional measurement
> > in `units` is stored in exactly one place in this product.
> >
> > **And `deviation` makes the argument concrete rather than analogous.** It is
> > *derived by subtraction* — `qc::verdict` computes `actual - nominal` in `f64`
> > (`crates/aberp-qa/src/qc/verdict.rs:103`) — so an ordinary pair like `25.03`
> > and `25.0` yields `0.030000000000000426`, scale 17. R2 refuses past scale 6,
> > so an R2 carry would have **hard-failed the whole migration on an ordinary
> > inspection row**, not on a pathological one. The fixture carries exactly that
> > value and the test asserts the scale.
> >
> > ⚠ **What the `REAL` call does NOT get for free, and Part E added it:**
> > `DOUBLE` → `REAL` is bit-exact for every *finite* `f64`, but **SQLite has no
> > `NaN`** — a bound `f64::NAN` is stored as `NULL` (measured by
> > `sqlite_stores_a_bound_nan_as_null`; the infinities *do* round-trip). All
> > eight columns are `NOT NULL`, so a `NaN` would have surfaced as an
> > unattributable `NOT NULL constraint failed`, and on a *nullable* measurement
> > column it would cross as a silent `NULL` that the gate's `IS NOT NULL`-scoped
> > `typeof` sweep cannot see either. `migrate_quality::finite_measurement`
> > refuses a non-finite measurement before the bind, naming table, column and
> > row. **Any future §3.2 E column on a nullable float must use it** — that is
> > the general lesson, and it is not specific to QC.
> >
> > **And the risk runs the other way for a measured duration.** R2 refuses
> > anything above the canonical quantity scale of 6, so an R2 carry would
> > **hard-fail the whole migration** on an ordinary row like `12.3456789` —
> > refusing a value that carries no exactness requirement, on a column that is
> > not money and never reaches a filing. `DOUBLE` → `REAL` is bit-exact and adds
> > nothing new. The gate proves it per row (the `f64` read back from SQLite must
> > equal the `f64` DuckDB held, a `NaN` therefore hard-stopping), and the
> > fixture's `wo-02` carries exactly the scale-7 duration the argument turns on.

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

> ✅ **N-2's "value-exact" is now MEASURED over the whole domain, not asserted
> over the intended one (Step 6, S451).** The clause "value-exact over the
> closed set of values the column can hold" was doing more work than it could
> carry: the column's Rust type is `u16`, so *what the column can hold* is
> 65 536 values, and "the four legal rates" is a property of the **doors**, not
> of the type. Swept:
>
> - The `f64` render and the exact `Decimal` render (`Decimal::from(bp) /
>   Decimal::from(10000)`, `round_dp(2)`, `{:.2}` — the exact shape §3.3's
>   conservative call prescribes) agree on **every** `u16` value **except**
>   those that are an exact tie at the second decimal place (`bp % 100 == 50`),
>   where the `f64`'s binary approximation decides the tie and the exact value
>   decides it by the rounding strategy.
> - **No legal Hungarian rate is in that class** (0 / 500 / 1800 / 2700 all
>   have `bp % 100 == 0`). N-2 is therefore sound, and for a stated reason
>   rather than a hopeful one.
> - The practical consequence for the session that lands the B2 conversion:
>   **it moves no filed byte for any rate the product can issue**, and the
>   sweep says so before the change rather than after it.
>
> Pinned by `the_permitted_f64_vat_rate_render_is_exact_except_on_second_decimal_ties`
> and `the_legal_hu_rates_round_trip_through_the_f64_render_and_the_exact_parse`
> (`apps/aberp/src/nav_xml.rs`, unit tests). The second closes the §9 rule-7
> fork row against `:2658`'s exact parse in the direction that matters: the
> forward render round-trips back to the original basis points on all four
> rates. The sweep asserts its divergent set is **non-empty**, so it cannot
> pass vacuously.

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
| `apps/aberp/src/reports.rs:800` | `CAST(SUM(CAST(il.quantity AS DECIMAL(38,6)) * il.unit_price) AS VARCHAR)` | **The sharp one.** Under R2 `quantity` is `TEXT`; SQLite coerces `TEXT * INTEGER` to `REAL` and the report silently becomes float money. | ~~Select `quantity, unit_price` per row; fold in Rust with `Money::checked_mul_decimal` (already exists, `money.rs:54`) + the existing `decimal_str_to_i64` round-half-even (`reports.rs:1011`).~~ **Corrected by M-2 and LANDED 2026-08-01 (S453).** That prescription mixes two rounding orders and is not value-neutral — see the M-2 row in §9. What landed: project `quantity` / `unit_price` / **`vat_rate_kind`** per line and fold in `fold_outgoing_lines` through **`aberp_billing::domain::invoice::{line_net_total, line_vat_amount}`** — the functions `nav_xml::write_summary` itself sums. Not "equivalent arithmetic": the same two functions, so report == filed is structural. The `vat_rate_kind` projection is new and load-bearing (the old report derived VAT from basis points alone and knew nothing of ADR-0103 Invariant V). |
| `apps/aberp/src/reports.rs:861` | `CAST(COALESCE(SUM(i.huf_equivalent_total), 0) AS VARCHAR)` | Under §3.2 B the column is `INTEGER`; `SUM` over INTEGER is exact but **raises on i64 overflow** and the `CAST … AS VARCHAR` round-trip is now pointless. | `SELECT huf_equivalent_total` and `checked_add` in Rust; loud on overflow. ✅ **LANDED 2026-08-01 (S453)**, in the same commit as the three `unwrap_or(0)` fail-opens' deletion, as §3.4 requires below. |
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

### 6.2.1 How to reach a genuinely green drill — the demonstration plan (Step 6, S451)

S-1 and S-2 make step 7's verification unreachable against the DEV tenant *as it
stands today*. Neither is fixed by weakening a check. The DEV DB is disposable
([[feedback_dev_db_disposable]]), and that is the lever: **give the drill a
tenant that has something to protect.** Four operations, in order, each with its
own exit condition, none of them a change to `premigration.rs`:

1. **Reconcile the mirror first (closes S-2).** Run the existing
   `docs/runbooks/audit-mirror-defork-20260719.md` heal against the DEV tenant.
   *Exit:* `head_seq == mirror_tail_seq`, `verify-chain` genesis→head `OK`.
   Its own focused session — a migration is not a repair tool, and neither is an
   adversarial review.
2. **Stand up a signed DEV tenant (closes S-1).** Not by editing the existing
   one: create a **fresh tenant** in `tenants.toml` with `dap_enabled = true`
   (a non-production build — a production build refuses to start on that flag,
   `serve.rs:2945`), boot it, and drive the customer journey through it. Boot
   alone opens a service session and takes an anchor; the heartbeat takes more.
   *Exit:* `signed_entry_count > 0` **and** `anchor_count > 0` measured through
   `premigration::run_snapshot`, i.e. through the same reader the gate uses.
3. **Run T-15 on that tenant** — quote → order → work order → dispatch →
   invoice → NAV submit (test endpoint) → PDF → email — so the ledger carries
   *business* entries interleaved with the signed session entries, and the
   invoice family carries real rows. *Exit:* the journey completes and the
   invoice number, the ÁFA breakdown and the PDF bytes assert.
4. **Then the drill, in place, on that tenant:** snapshot → migrate → reconcile
   → `rollback_to_duckdb.sh` → green DuckDB boot → re-apply. *Exit:* §6.2 step
   7 prints **PASS** with all six checks green, including the two B1 counts —
   which is now a real equality between two non-zero numbers rather than an
   unreachable one.

**Note what step 2 exposes and do not let it pass silently.** Because
`append_in_tx` hard-passes `None` for the session, only session-lifecycle
entries are ever signed; a signed DEV tenant will show `signed_entry_count` in
the single digits against hundreds of business entries. That is enough to make
B1 a real check, and it is **not** enough to call the chain "tamper-evident" in
any operator-facing sense. §11's production-cutover list must carry that
distinction explicitly, because on a production build `dap_enabled = true` is a
hard refusal today — so **a production tenant cannot satisfy B1's precondition
at all**, and the prod cutover therefore owes either the ADR-0099 fork fix that
unblocks DÁP or a stated, testable exemption. That is a larger finding than
S-1's DEV symptom and it is where S-1 actually points.

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
| **inventory (`stock_movements`, cache cols)** | ~~Carry `stock_movements` (append-only ledger); **rebuild** the `products.stock_qty` cache from `SUM(qty_delta)` **in Rust** via the existing `rebuild-stock-cache` path.~~ **CORRECTED 2026-08-01 (S455): carry everything, including the cache columns, verbatim.** | ~~The cache is derived by definition, and rebuilding it exercises §3.4's Rust-side fold on real data.~~ **The rebuild would disarm the gate.** The reconciliation arm compares the two sides row-for-row; a migrator that re-derived `stock_qty` would make a DuckDB cache that had legitimately drifted — the exact condition ADR-0061 §3's `rebuild-stock-cache` recovery path exists for — show as a per-row difference, and the only way back to green would be to teach the gate the transformation, i.e. **to verify the extraction against itself (B4)**. It would also make the migration a repair tool, which §6.3's own mirror argument rules out three paragraphs above: *a migration is not a repair tool.* A cache that is wrong on DuckDB must cross as wrong and be repaired by the path that owns that repair. |
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
> where the reader freezes its snapshot and will not see a commit that lands
> after it.
>
> > **CORRECTED 2026-07-31 by finding R-4 (read-fork audit): the snapshot is
> > NOT taken at `BEGIN`.** This paragraph originally said "freezes its snapshot
> > at `BEGIN`". Measured, that is false. `BEGIN` is `BEGIN DEFERRED`: it
> > acquires nothing and starts no read transaction. **The snapshot is taken at
> > the first read statement.**
> >
> > | Sequence | Result |
> > |---|---|
> > | `BEGIN` → writer commits → `SELECT` | **sees** the commit |
> > | `BEGIN` → `SELECT` → writer commits → `SELECT` | does **not** see it |
> > | … → `COMMIT` → `SELECT` | re-syncs |
> >
> > The exposure therefore begins at a transaction's **first `SELECT`**, not at
> > its `BEGIN`. Since axis (a) found **zero** `read()` sites that open a
> > transaction at all, the class is empty either way — but a T-20 written to the
> > original wording would have been written, failed, and then "fixed" into
> > whichever assertion happened to go green. That is how a false semantics claim
> > gets pinned. The corrected pin is `t20b_the_snapshot_is_taken_at_the_first_read_not_at_begin`.
>
> And a `read()` taken *while a `write()` guard is live* now
> contends for a real file lock instead of sharing one in-process instance, so
> M7's finite `busy_timeout` converts DuckDB's immediate mutex self-deadlock into
> a **timed hang, then `SQLITE_BUSY`** — rule 13's known failure mode with its
> loudness removed. The number *is* the observability of the worst case, which is
> why it is chosen here and not in Step 2 in isolation.
>
> > **CORRECTED 2026-07-31 by finding R-3 (read-fork audit). The paragraph above
> > has an unexamined step, and the Q10↔Q11 coupling it asserts does not exist.**
> > `Handle::read()` does not simply hand out a connection: it runs the debug
> > re-entrancy tripwire, takes the writer `Mutex` via `lock_recovering()`, calls
> > `ensure_open()`, and only then `try_clone`s. Under `sqlite-engine` only the
> > last step changes. Whether the SQLite arm keeps the mutex was a **free choice
> > nobody had made** — and T-21 was unwritable until it was made, because one arm
> > makes the described race impossible and the other leaves no abort to assert.
> >
> > **The choice, now made and binding: the SQLite arm KEEPS `lock_recovering()`**
> > (recorded on `Handle::read()`'s doc-comment, `crates/aberp-db/src/lib.rs`). A
> > nested `read()`-inside-`write()` therefore resolves against the **Rust** mutex
> > — panicking on the tripwire in debug, deadlocking on the mutex in release —
> > and **never reaches SQLite**. `busy_timeout` is never involved and cannot
> > downgrade rule 13's loud self-deadlock into a silent timed hang. The failure
> > mode is unchanged from today: not a regression.
> >
> > Rejected: opening a fresh SQLite connection without the mutex. It makes the
> > nested case *legal*, deletes the tripwire's premise, requires a loud abort to
> > be deliberately re-added, and trades a deterministic failure for a
> > timing-dependent one. Reopening it needs its own measurement, not a Step-5
> > implementation detail.
>
> **Scope — exhaustive, and the denominator is stated so completeness is
> checkable.** The **238** non-test `Handle` sites split **102 `read()` / 136
> `write()`** (§1.2 — *corrected 2026-07-31 by finding R-2; the ADR originally
> said 84 / 50 / 34, measured with a single-line grep that could not see
> rustfmt-wrapped chains or a `Handle` bound to a local*). The audit classifies
> **all 102 `read()` sites**, with the 136 `write()` sites as the reachability
> context for axis (b). Re-measure with `tools/adr0108_handle_census.sh`. Two
> axes:
>
> - **(a) does it read inside an open transaction?** → the frozen-snapshot class.
> - **(b) is it reached while a `write()` guard is live?** → the lock-contention
>   class. Reachability is **closed under calls**, not judged line-locally — the
>   same reaching-set discipline ADR-0106's door gate uses, because a `read()`
>   three frames below a `write()` guard is the case a local read misses.
>
> Any site that is **both** is a defect `try_clone` was masking, and it is fixed
> in Step 3, not carried into its family's step. The output is a **102**-site
> table — every site classified, none marked "probably fine". **An audit with an
> unstated denominator is a sample** — and a denominator measured with a grep
> that cannot see two thirds of the tree's formatting makes it one anyway.
> *(Delivered: `docs/findings/read-fork-audit-sqlite-20260731.md` §1 and §5.1 —
> 99 SAFE as-is, 3 SAFE-with-a-required-change (R-1, now fixed), 0 to reroute;
> both axes measured EMPTY.)*
>
> **Three pins, all mutation-verified:**
> - **T-20** — commit on connection A → read on a **pre-existing** connection B in
>   autocommit → assert B sees it. This is §2.4's snapshot claim, pinned instead
>   of asserted. Its in-transaction twin asserts the *opposite* — **and per R-4
>   the twin's trigger is B's FIRST READ STATEMENT, not its `BEGIN`** (`BEGIN` is
>   `BEGIN DEFERRED` and acquires nothing): B inside a transaction that has
>   **already issued a read** must **not** see the commit, while B that has only
>   issued `BEGIN` **does** see it — so the test encodes the real semantics, not
>   the convenient half. **LANDED** as `t20a` + `t20b` in the audit's harness
>   (`crates/aberp-db/tests/adr0108_read_fork.rs`, PR #52); `t20b` is one of only
>   two arms of six that actually discriminate WAL.
> - **T-21** *(rewritten 2026-07-31, R-3 — the original wording was unwritable;
>   see the correction above)* — a nested `read()`-inside-`write()` **never
>   reaches the storage engine**: it resolves against the Rust writer `Mutex`,
>   panicking on the re-entrancy tripwire in debug and deadlocking on the mutex
>   in release, so `busy_timeout` is never involved. Two arms, because a
>   behavioural pin alone cannot see the decision reversed — `assert_not_reentrant`
>   panics *before* the lock, so a SQLite arm that dropped `lock_recovering()`
>   while keeping the tripwire would stay green with the invariant gone. The
>   **structural** arm asserts `Handle::read()` still calls `lock_recovering()`
>   and that no engine-gated arm returns before it. Landed:
>   `crates/aberp-db/tests/adr0108_t21_nested_read_in_write.rs`, mutation-verified,
>   green under the default build and `--features sqlite-engine`.
> - **T-3d** (M7) asserts the chosen `busy_timeout` value is actually set.
>
> **The number, chosen conservatively and marked.** `busy_timeout = 5000 ms`,
> **on the explicit condition that T-21 lands first** — with the nested case
> resolving against the Rust mutex, the timeout is only ever a backpressure knob
> for genuine cross-process contention, never the thing that hides a deadlock.
> *(R-3: the condition is now satisfied — T-21 has landed in the R-3 shape. The
> audit also measured that on the READ surface `busy_timeout` is close to
> irrelevant: a WAL reader with a 0 ms timeout still succeeds against a live
> `IMMEDIATE` writer, and a snapshot conflict returns `SQLITE_BUSY_SNAPSHOT`
> immediately without invoking the busy handler at all. 5000 ms is a **write**-
> contention ceiling, not a read knob.)* 5 s is long
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

> **What Step 5 ACTUALLY landed, 2026-07-31 (S450) — read this before Step 7.**
>
> The bullet above is the full Step 5. What landed is its **migrator half**,
> and the split is deliberate: Ervin's constraint for this session was
> *"DuckDB remains the source of truth for this family until an explicit,
> gated cutover; SQLite is populated behind the compile-time selector."*
>
> **Landed.** The family's `STRICT` DDL built the way the DuckDB schema is
> built (base `CREATE` + six `ensure_columns` ladders, 16 columns, M8's
> post-condition exercised on the real family rather than on an
> already-complete table); the typed row-by-row carry applying §3.2 B's
> `huf_equivalent_total` `DECIMAL→INTEGER` and §3.2 C's `exchange_rate` /
> `quantity` `DECIMAL→TEXT`; the reconciliation gate's arm for the family
> (presence symmetry, row counts, **per-row** money equality keyed by PK,
> per-money-column exact sums folded in Rust, a 12-column `typeof()` sweep,
> bucket-by-bucket sequence agreement); T-1, T-5(a), R1/R3 round-trips, and
> T-14 on SQLite. S444's durable floor is carried unchanged and now pinned on
> the new engine.
>
> **NOT landed, and each one needs the Rust-side seam crossing that is the
> cutover:** the 25 + 4 + 2 + 1 `ensure_columns` *call-site* rewrites inside
> `duckdb_store.rs` / `restore_from_nav_outgoing.rs` / `invoice_draft.rs` /
> `serve.rs`; `write_vat_rate_choice`'s `f64 → Decimal` (B2) and therefore
> T-5(d)/(e); `reports.rs:800,861`'s folds **and the three `unwrap_or(0)`
> fail-opens at `:827`, `:872`, `:1279`**; T-4's NAV/PDF byte-identity, which
> is only meaningful once something reads the SQLite side. Per §9's own escape
> clause, **the `reports.rs` fail-opens revert to owing their own PR and do not
> lapse** — they are a live production defect on the ÁFA report, independent
> of this migration.
>
> **The rollback drill was run** against a byte-identical copy of the DEV
> tenant DB (20 MB, 240 ledger entries, 16 invoices) rather than in place —
> the conservative call, and it cost nothing, because the copy carries the real
> data. It surfaced **S-1** and **S-2** in §9, which are why the ADR's stated
> Step-5 exit criterion ("the real DEV tenant DB migrates and the gate passes
> green") is **not reachable today for two pre-existing reasons**, both caught
> loudly by gates that were built for exactly that. That is the single most
> important input to Step 6.

**Step 6 — Adversarial checkpoint.** Not a code step. Rule 4 reserves full
adversarial review for the invoice→NAV/ÁFA path; Step 5 *is* that path. No further
family crosses until this closes.

> **Step 6 RAN, 2026-08-01 (S451). Verdict: GO to Step 7, conditional on the
> four must-fixes below.** Test-hardening only; no production code changed.
>
> **What was attacked, and what held.**
>
> 1. **N-1 re-swept over the emission reach set** (`apps/aberp/src/nav_xml.rs`
>    + `modules/billing/src` + `crates/invoice-pdf/src`, recursive, `f64`/`f32`
>    /float-literal): **one** live `f64` and it is `nav_xml.rs:1788`, the VAT
>    *rate* render. `invoice-pdf`'s `f32`s are page geometry and RGB colour;
>    `modules/billing`'s only hit is a doc comment stating the opposite policy.
>    No monetary amount and no quantity touches a float between column and
>    emitted byte. The whole arithmetic chain is `i64` / `Decimal`:
>    `checked_mul_decimal` → `round_dp_with_strategy(0, MidpointNearestEven)`,
>    `vat_amount` → `i64` × `bp` / 10 000, `huf_equivalent_round_half_even` →
>    `Decimal` throughout, `format_native_amount` → integer string / `abs/100 .
>    abs%100`. **N-1 holds.**
> 2. **B2 measured rather than asserted** — see §3.3's ✅ block above.
> 3. **T-4 LANDED and green, with an in-suite mutation proof.** The gap Step 5
>    left ("only meaningful once something reads the SQLite side") is closed by
>    reading the SQLite side *in the test*: every seeded invoice is
>    reconstructed from **both** engines and driven through the production
>    `nav_xml` renderer — the fresh-issuance body, the storno body and the
>    modification body — and the three byte strings are compared. **Zero
>    divergence.** The fixture adds the shapes Step 5's own fixture did not
>    have: a **mixed-rate** invoice carrying all four legal ÁFA rates (four
>    `summaryByVatRate` buckets) and a uniform **non-`Percent`** (`AamExempt`)
>    invoice, where a kind that failed to cross changes the *element name*, not
>    a digit. `apps/aberp/tests/adr0108_step6_nav_byte_identity.rs`.
> 4. **The PDF half of T-4 is discharged by reduction, stated not assumed.**
>    `print_invoice.rs` builds every money-bearing `PdfLine` field from the
>    **on-disk NAV XML**, not from the database; the only DB-sourced PDF inputs
>    are the buyer-facing notes and the rate metadata, and both are compared in
>    the same test. Byte-identical XML + verbatim notes **is** the PDF claim.
>    If a later change sources an amount from the DB, the reduction breaks —
>    the test's module docs say so at the point of reliance.
> 5. **Q2, re-swept for the invoice family specifically.** Every SQL statement
>    in the tree mentioning `quantity` / `exchange_rate` / `huf_equivalent_total`
>    / `unit_price` / `vat_rate_basis_points`, filtered for `ORDER BY` / `MIN` /
>    `MAX` / `BETWEEN` / range predicates: **the only orderings are on `id` and
>    `(invoice_id, ordinal)`**, both identity columns, both inside the migrator.
>    **No ordering, range or extremum on an R2 column feeds a NAV total or a
>    PDF subtotal.** §3.4's two Q2 hits remain the tree's only ones and both are
>    inventory, i.e. Step 7.
> 6. **The R2 bind census is trivially closed *today* and that is the point.**
>    Because `modules/billing` has not crossed the seam, the tree's **only**
>    SQLite bind path into an R2 column is `migrate_billing.rs::carry_billing`,
>    and it binds a `String` produced by `canonical_decimal` (a validating
>    pass-through, never a re-render) with `unit_price` / `huf_equivalent_total`
>    as `i64` via `money_minor_units`, which **refuses** a non-integral value
>    rather than casting it. There is no `f64` bind site to find. The exposure
>    is entirely **prospective**: it arrives with the cutover, when
>    `duckdb_store.rs`'s writers become the SQLite writers — which is exactly
>    when T-8 has to already exist. See the must-fix.
>
> **Must-fix before Step 7 lands, all four recorded in §9.**
>
> - **M-1 — ✅ CLOSED 2026-08-01 (S452).** T-8 is built, mutation-verified and
>   wired into `cut-gate.yml`; §3.1's "exactly two guards" sentence is no longer
>   aspirational. See §8 T-8 for the four corrections the implementation forced,
>   and the §9 row. The original finding is preserved below.
> - **M-1 (blocking Step 7): T-8 does not exist.** §3.1's 2026-07-31 correction
>   states R2's guards are "exactly two, and they are now known to be the only
>   two" — the Rust `Decimal` bind and T-8's no-arithmetic-in-SQL cut-gate.
>   Measured: `tools/` carries `cut_gate_sqlite_posture.sh` and its probes and
>   nothing else; neither workflow runs a T-8 step; a tree-wide search for
>   `T-8` returns **three doc-comment citations and zero implementations**.
>   **R2 has one guard, not two**, and three landed artefacts assert otherwise.
> - **M-2: §3.4's prescribed fold is not value-neutral, and T-5(c) pins that it
>   is.** See §9.
> - **M-3: S-1's ruling** — B1 stands; the drill needs a signed tenant. §9.
> - **M-4: S-2's ruling** — DEV-only artefact, reconcile before any real-DEV
>   run. §9.
>
> **What Step 6 did NOT do, deliberately (rule 3).** It did not build T-8: the
> gate's mutation targets are `repository.rs:548/549/585`, which are Step 7's
> commit, and a gate landed here would either red on day one or ship with an
> allowlist for its own two known sites. It did not cross a family, run the
> real-DEV drill, or touch `~/.aberp/**`.
>
> ⚠ **The T-8 half of that paragraph rested on a stale reading of the tree, and
> S452 measured it false.** `repository.rs:548/549/585` were **already folded**
> into `low_stock_candidates` on this branch — the predicate and the deficit
> ordering run in Rust over `Decimal`, and only a `COALESCE(…)` *projection*
> survives in the SQL. So the third branch neither foreseen nor argued against
> was the one actually available: a gate that is **green on the tree today**,
> reds on those statements as **synthetic** mutations (P2/P3/P4), and parks the
> four remaining `SUM` sites in a **shrink-only register** rather than an
> allowlist. T-8 landed on that branch; §8 T-8 records it.

**Step 7 — The remaining non-quoting families,** one at a time, rule-14 fused:
partners (+ **M11**, T-12 — *corrected: M11/T-12 belong to the family's
Rust-side crossing, not to its migrator half; see §9*) → products/inventory (incl. §3.4's three cache-rebuild
folds **and both low-stock predicate folds, `repository.rs:548–549` and `:585`,
in one commit** — F1/Q2) → work orders/BOM → QA/QC → dispatch → purchasing →
email/relay.
- *Verified by:* per-family reconciliation + the family's existing round-trip tests
  + T-15 (customer-journey e2e) re-run after each.
- *Rollback:* per family; each is its own PR.

> **Step 7 Part A LANDED 2026-08-01 (S453)** — M-2's per-line ÁFA fold, before
> any family crossed. See the §9 M-2 row.
>
> **Step 7 Part B — the PARTNERS family crossed, 2026-08-01 (S454).**
> `apps/aberp/src/migrate_partners.rs` +
> `apps/aberp/tests/adr0108_step7_partners_family.rs`, plugged into
> `migrate_families` and `reconcile` beside the Step-5 billing arm.
>
> **Same split as Step 5, and Ervin's same constraint: the MIGRATOR HALF only.**
> DuckDB remains the source of truth; `partners.rs`'s 12 DDL sites and all of its
> queries are untouched and still run against DuckDB. What landed: the `STRICT`
> DDL (17-column base `CREATE` + the two indexes, then PR-97 / S361 / S428 as
> **three `ensure_columns` ladders**, 7 columns, so M8's post-condition is
> exercised rather than no-op'd), the typed row-by-row carry inside one
> `BEGIN IMMEDIATE`, and the gate's arm.
>
> **The family has no money in it at all.** One table, 24 columns, and the only
> non-string column is `issued_invoice_count` (§3.2 F — a count). §3's R1/R2
> rules have nothing to bite on, which is why it is the right family to go
> first: the machinery gets one more real exercise where a mistake cannot move a
> filed figure. The gate's arm is correspondingly **per-column over all 24**
> rather than money-only — on a table with no money the regulatory payload is
> `tax_number` / `eu_vat_number` (the NAV buyer identifiers) plus the five
> identity columns the dedup guard keys on, and "the important ones" would have
> been the wrong subset to pick.
>
> ⚠ **M11 and T-12 did NOT land, deliberately.** M11 is "escape the `LIKE`
> metacharacters and replace SQL `LOWER()` with Rust `to_lowercase()` on both
> sides", over `partners.rs:1001–1005` (the duplicate-partner guard) and `:1049`
> (the typeahead filter). Nothing in Part B makes those queries run against
> SQLite, so a T-12 written now could only assert that **DuckDB's** `LOWER()`
> still folds `Á` — true, and not the property M11 exists to protect. §7's own
> ordering rule decides it: *a refusal whose test cannot be written yet is not
> landed yet.* Both move to the family's Rust-side crossing; §9 carries the row.
> The DEV-shaped fixture already seeds the two values a real T-12 needs
> (`Árvíztűrő tükörfúrógép` and a stored literal `%` / `_`).
>
> ⚠ **Measured, and it narrowed the fixture rather than the code:**
> `partners.customer_vat_status` and `issued_invoice_count` **cannot be NULL** on
> any `partners` table built from the base `CREATE` — it declares both
> `NOT NULL DEFAULT`, and DuckDB refuses the INSERT. Their nullable history
> exists only on a genuinely pre-PR-97 table (where the PR-97 ladder adds them
> unconstrained because DuckDB rejects a constraint on `ADD COLUMN`), and even
> there the ladder's follow-on `UPDATE` backfills them at the next boot. **The
> SQLite columns are declared nullable anyway** — nullable is the shape that
> carries *both* histories, and re-deriving the backfill in the migrator is the
> "verify the extraction against itself" shape B4 forbids.
>
> **Six pins**, including the one that matters most: the per-row equality arm is
> **shown to go red** on a single changed column on a single row
> (`a_single_changed_column_on_a_single_row_reds_the_gate`, mutating
> `tax_number`) — the row count, the `Σ issued_invoice_count` fold and the
> `typeof` sweep are all blind to that mutation, so that arm is the only thing
> between a silently-corrupted partner record and a green gate.

> **Step 7 Part C — the PRODUCTS/INVENTORY family crossed, 2026-08-01 (S455),
> and §3.2 E's rule-7 divergence is resolved at the storage layer rather than
> carried.** `apps/aberp/src/migrate_products.rs` +
> `apps/aberp/tests/adr0108_step7_products_family.rs`, plugged into
> `migrate_families` and `reconcile` beside the Step-5 and Part-B arms.
>
> **Four tables, two owning modules, 46 columns**: `products` (14 — `products.rs`
> plus `V001__inventory.sql`'s four-column ladder), `stock_movements` (11),
> `inventory_balances` (12 — `material_inventory.rs`, plus S432's four-column
> ladder), `inventory_reservations` (9, plus S275's one). Because two
> independent `ensure_schema` calls build the four, **presence is checked and
> held symmetrically per TABLE, not per family** — a products-only database and
> a material-inventory-only one are both legitimate shapes.
>
> **THE RULE-7 RESOLUTION is the substance of this commit.** All five `DOUBLE`
> quantity columns cross as R2 `TEXT`, value-neutrally, refusing rather than
> rounding. See the correction block in §3.2 E and the §9 row. Pinned by a
> **deterministic property sweep of 4096 generated quantities plus 20
> adversarial ones**, asserting the disjunction that matters: each value either
> round-trips to the identical `f64` *or* is refused with a message that says
> why — a test that asserted only the happy arm would pass on a carry that
> silently rounded everything to six places. Both arms are required to fire.
>
> ⚠ **Same split as Steps 5 and 7B: the MIGRATOR HALF only.** DuckDB remains the
> source of truth; `products.rs`, `material_inventory.rs` and
> `aberp-inventory/src/repository.rs` are untouched and every one of their
> queries still runs against DuckDB.
>
> ⚠ **§3.4's three cache-rebuild folds did NOT land here, and this line
> ("incl. §3.4's three cache-rebuild folds") is corrected in §9** — on exactly
> the precedent Part B set for M11/T-12, and for the same reason. The T-8
> pending-fold register keeps its two entries; both still match a live
> statement, so T8-3/T8-4 are green because the sites are *there*, not because
> the gate cannot see them. **The migrator's own code holds to T-8
> unconditionally**: no `SUM`/`AVG`/`*`/`+`/`-` on a money or quantity column
> appears in it, and every fold in it is Rust over `Decimal` / `i64`.
>
> ⚠ **§6.3's inventory row is corrected, not executed**: the cache columns are
> carried verbatim rather than rebuilt, because rebuilding them would force the
> gate to verify the extraction against itself (B4). See §6.3.
>
> **The load-bearing pin is `a_qty_delta_rewrite_that_preserves_the_sum_still_reds_the_gate`**
> — two movements on one product are rewritten so that `Σ qty_delta` is
> unchanged. The row count, the `Σ` fold and the `typeof` sweep are all green on
> that mutation, so the per-row arm is the only thing between a rewritten
> movement history and a green gate.

> **Step 7 Part D — the WORK-ORDERS / BOM family crossed, 2026-08-01 (S456).**
> `apps/aberp/src/migrate_work_orders.rs` +
> `apps/aberp/tests/adr0108_step7_work_orders_family.rs`, plugged into
> `migrate_families` and `reconcile` beside the Step-5, Part-B and Part-C arms.
> (The brief called it "Part B"; B and C were already taken, so it is D — named
> rather than silently renumbered.)
>
> **Four tables, one owning crate, 43 columns**: `work_orders` (16 — V001's 13
> plus V002's two and V003's one), `boms` (8 — V001's 7 plus V003's one),
> `routings` (10, V001), `bom_revisions` (9, V003). A **single**
> `aberp_work_orders::ensure_schema` builds all four, so unlike Part C the family
> has one construction path — and **presence is still held per TABLE**, because
> `bom_revisions` and the two `bom_rev_id` columns arrive with V003 (ADR-0105)
> and a DuckDB file written before that migration legitimately has three of the
> four. Per-table is strictly the safer of the two available shapes: an asymmetry
> is still a hard stop, and only the genuinely legitimate partial shape passes.
> Pinned by `a_pre_adr0105_source_without_bom_revisions_still_crosses`.
>
> ⚠ **`routings.est_cost_huf` crossed as R2 `TEXT`, not R1 `INTEGER` — the one
> money column in the tree that does not follow R1, exactly as §3.2 B's note
> pre-decided.** It is flagged again here because it is the single exception and
> a later reader must not discover it by grep. Two things make the exception safe
> rather than merely stated: the value is carried **verbatim** through
> `migrate_billing::canonical_decimal`, which refuses anything `rust_decimal`
> cannot restore; and the gate folds it in Rust over `Decimal`, never as a SQL
> `SUM`. `typeof(routings.est_cost_huf) = 'text'` is asserted on every row —
> a `'real'` there would be F-6a's float-money class arriving through the
> exception.
>
> ⚠ **`work_orders.actual_machining_minutes` stayed `REAL`, and Part C's five
> `DOUBLE`s did NOT set a precedent for it.** Part C's five were one half of a
> *measured* rule-7 divergence with an exact counterpart; this one has none
> (`routings.est_time_min` is an `INTEGER` estimate at a different granularity,
> and `quote_calibration.{estimated,actual}_minutes` are `DOUBLE` too). And the
> risk runs the other way: R2 refuses above the canonical quantity scale of 6, so
> an R2 carry would **hard-fail the migration** on an ordinary measured duration
> like `12.3456789` — refusing a value that carries no exactness requirement, on
> a column that is not money. The fixture's `wo-02` holds exactly that value so
> the argument is exercised rather than asserted. §3.2 E carries the
> confirmation.
>
> ⚠ **Same split as Steps 5, 7B and 7C: the MIGRATOR HALF only.** DuckDB remains
> the source of truth; `crates/aberp-work-orders/src/repository.rs` is untouched
> and all of its queries still run against DuckDB. §9 carries the row.
>
> **No §3.4 fold is owed by this family and none is deferred** — §3.4's seven
> sites contain no work-orders statement, §5's mitigations name none, and §6.3's
> row says "row-by-row carry". **The T-8 pending-fold register is unchanged by
> this commit**: nothing joined it and nothing left it.
>
> **Nine pins.** The load-bearing one is
> `a_single_changed_column_on_a_single_row_reds_the_gate`, run three times over
> the three column classes: the money column, a `qty_per_unit` **swap that leaves
> `Σ` unchanged**, and an ordinary `wo_number` that no fold and no `typeof` sweep
> touches at all. The disjunction sweep is 4096 generated decimal renderings plus
> an 18-row adversarial table, asserting that each value either crosses
> **byte-identically** or is refused with a message naming the column and the row
> — and requiring **both arms to fire** (measured: 3923 carried, 191 refused).
> Two properties of the shared R2 validator were *measured rather than assumed*
> and both are recorded in the table itself: `Decimal::from_str` accepts exponent
> form, and it **rounds** past 28 significant digits rather than erroring. See
> the §9 row.

> **Step 7 Part E — the QA / QC family crossed, 2026-08-02 (S457).**
> `apps/aberp/src/migrate_quality.rs` +
> `apps/aberp/tests/adr0108_step7_quality_family.rs`, plugged into
> `migrate_families` and `reconcile` beside the Step-5, Part-B, Part-C and
> Part-D arms.
>
> **Same split, same constraint: the MIGRATOR HALF only.** `apps/aberp/src/quality.rs`
> and `crates/aberp-qa/src` are untouched; every one of their queries still runs
> against DuckDB. §9 carries the row.
>
> **Six tables, two owning modules, three construction paths, 84 columns** —
> `ncrs` / `ncr_transitions` / `capas` (built by `aberp::quality::ensure_schema`),
> `qa_inspections` (V001), `qc_inspection_plans` / `qc_inspections` (V002). The
> table set was **grepped, not recalled**, per §3.2's own rule.
>
> **This family has NO money and NO quantity — §3.2's A, B, C, D and G
> categories are all empty here**, and saying it plainly is worth more than a
> hedge. Its only numbers are `ncr_transitions.seq` (§3.2 F), eight dimensional
> measurements (§3.2 E) and two booleans (§3.2 H). R1 and R2 have nothing to
> bite on, so the gate's arm is **per column over all 84** rather than
> money-first: on an ISO-9001 record the payload is the record's completeness,
> and "the important columns" would have been the wrong subset to pick. A unit
> pin (`no_column_in_this_family_is_money_or_quantity`) reds if a later session
> adds a cost or a quantity to an NCR or an inspection without deciding its
> representation.
>
> **The eight floats stay `REAL`, which is §3.2 E's call followed, not a fresh
> one** — and Part D's distinguishing test is what confirms it rather than
> Part C's. Part C moved five `DOUBLE`s out of E because they were one half of a
> *measured* rule-7 divergence; these eight have no exact counterpart anywhere in
> the tree, so there is no fork to close. And the risk runs the other way more
> sharply here than it did for `actual_machining_minutes`, because `deviation` is
> **derived by subtraction**: `qc::verdict` computes `actual - nominal` in `f64`
> (`verdict.rs:103`), so an ordinary pair like `25.03` and `25.0` yields
> `0.030000000000000426`. R2 refuses anything past scale 6, so carrying these as
> R2 would have **hard-failed the migration on an ordinary inspection row**. The
> fixture's `qci-01` carries exactly that value and the test asserts its scale is
> past 6, so the argument is pinned rather than narrated.
>
> ⚠ **The first booleans any family in this migration has carried.** §3.2 H's
> `BOOLEAN` → `INTEGER` had never been exercised; `qc_inspection_plans.enabled`
> and `qc_inspections.calibration_stale_at_event` exercise it now. Read and bound
> as `bool` on both sides, `typeof` asserted `'integer'` — a `'text'` `"true"`
> would read back as `false` and silently disable every enabled inspection plan.
>
> ⚠ **This family adds a refusal Part D did not have, and the reason is measured
> rather than preferred: SQLite has no `NaN`.** A bound `f64::NAN` is stored as
> `NULL` — pinned by `sqlite_stores_a_bound_nan_as_null`, which also measures
> that the *infinities* do round-trip. All eight measurement columns are
> `NOT NULL`, so without a refusal a `NaN` would surface as a bare
> `NOT NULL constraint failed` naming neither row nor reason, and on any future
> nullable measurement column it would cross as a **silent `NULL`**.
> `migrate_quality::finite_measurement` therefore refuses a non-finite
> measurement before the bind, naming the table, the column and the row — the
> same shape `canonical_decimal` has for R2. The infinities are refused with it
> as a *product* call, stated as such: an infinite dimensional measurement drives
> a pass/fail verdict that can no longer mean anything.
>
> ⚠ **No M11-shaped hazard here, and that is measured too.** `LOWER(` and `LIKE`
> both return **zero** hits against all six tables across `apps/aberp/src` and
> `crates/aberp-qa/src`, so the ASCII-`LOWER()` fold partners must fix at its
> cutover has no site in this family. Recorded so a later session does not
> re-derive it — and so the absence is known rather than assumed.
>
> ⚠ **There is no `ensure_columns` ladder in this family**, measured the same
> way: `ALTER TABLE` returns zero hits against all six tables. M8's
> post-condition has nothing to exercise here — which is precisely *why* presence
> is held **per table**: this family's schema evolved by adding **tables**, in
> three separate migrations, so a database predating S443 legitimately has four
> of the six and one predating S439 has only `qa_inspections`. A per-family
> answer would read that as "present" and then hard-stop on a table the source
> never had.
>
> **Ten pins.** The load-bearing one is
> `a_single_changed_column_on_a_single_row_reds_the_gate`, run four times over
> the four column classes: a measurement nudged by **one ULP** (no fold touches
> these at all and `typeof` reads `'real'` either way), the boolean, an ordinary
> `ncrs.description`, and the **composite-keyed** `ncr_transitions.note` with
> `Σ seq` provably unchanged. The disjunction sweep is 4096 generated
> measurements plus a 14-row adversarial table, requiring **both arms to fire**
> (measured: 3850 carried, 260 refused) — and because a validator agreeing with
> itself is not the property, a second pin pushes **192 generated doubles through
> real DuckDB → SQLite storage** and compares all 960 values **bit for bit**.
>
> **No §3.4 fold is owed by this family and none is deferred.** §3.4's seven
> arithmetic sites contain no QA/QC statement and §6.3's row says "row-by-row
> carry". The T-8 pending-fold register is **unchanged by this commit**: nothing
> joined it and nothing left it.

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
| **T-5** | **Money property tests**: (a) `Decimal` round-trips through `TEXT` for 10⁵ generated values at scale 0–6 incl. trailing-zero forms; (b) `huf_equivalent_round_half_even` on `Decimal` → `i64` matches DuckDB's result for the whole DEV rate set; **(c) — RESTATED by M-2, and landed 2026-08-01 (S453).** ~~`unit_price × quantity` folded in Rust equals the pre-migration DuckDB `DECIMAL(38,6)` aggregate for every invoice~~ — that pinned the fold to the *old report*, which did not agree with the filing, so executing §3.4 literally would have RED it and invited a repair of whichever side was cheaper to change (the ADR-0107 §4.1 shape). It now pins the thing that matters: **for the same invoice, the ÁFA report's `(net, vat)` per VAT-rate bucket equals the `<vatRateNetAmount>` / `<vatRateVatAmount>` NAV was sent** — parsed out of the *emitted body*, never recomputed, so it is not the report compared against a second copy of its own arithmetic. Seven tests in `apps/aberp/tests/adr0108_step7_report_ties_to_filing.rs` over fresh, mixed-rate (4 buckets, fractional quantities), non-`Percent`, modification, storno and whole-window shapes, driven through the **public** `compute_financial_report`. Mutation-verified: restoring the pre-M-2 group-rounding reds **5 of the 7**. The 6th is the deliberate witness that the two arithmetics are different numbers (26 vs 27), so `report == filed` cannot be satisfied by two identically-wrong sides. | §3.1, §3.3, §3.4 |
| **T-5(d)** | **N-1, zero allowlist.** No `f64` is constructed on any *monetary-amount or quantity* path between column and emitted byte, over `modules/billing/src` + `apps/aberp/src/nav_xml.rs` + `crates/invoice-pdf/src`. Enforced as a cut-gate grep for `as f64` / `: f64` / `f64::` / `0.0` **excluding** the VAT-rate site covered by T-5(e). **This is green today** — the full sweep of that reach set returns one live `f64` and it is the rate, not an amount — so the gate lands as a ratchet, not as a red. Mutation-verify by introducing an `f64` on the `huf_equivalent_total` read and watching it red. | §3.3 N-1 |
| **T-5(e)** | **N-2, a one-entry shrinking allowlist.** The set of `f64` constructions in the billing→`nav_xml`→`invoice-pdf` reach set is **exactly** `{nav_xml.rs:1788}` — a *percentage rendering* of an INTEGER basis-point count, value-exact over the four legal HU rates. Any second entry reds the gate. **Step 5 converts that site to `Decimal`, after which the allowlist is empty and T-5(e) asserts zero.** The allowlist may only shrink; growing it requires amending §3.3. | §3.3 N-2, B2 |
| **T-6** | Two connections interleave read-head → append; must **not** produce two links off one `prev_hash`. Run with and without `BEGIN IMMEDIATE` | M5 / F-7a |
| **T-7** | `db_writer_lock_e2e` re-pointed at SQLite; **plus** a cross-engine test: a DuckDB `serve` holding the lock refuses a SQLite `serve` on the same tenant+dir | M6 / F-7b / §1.1 G-7 |
| **T-8** | Cut-gate grep over any §3.2 A–D column name appearing in any SQL string, for **arithmetic**: `SUM(` `AVG(` `*` `+` **`-` `/`** — and for **comparison**: **`<` `>` `<=` `>=` `BETWEEN` `MIN(` `MAX(` `ORDER BY`** (F2). The first draft's pattern had `SUM(`/`*`/`+`/`AVG(` only: it omitted subtraction and division, and had **no comparison arm at all**, so it was structurally incapable of seeing `repository.rs:548–549` — the one statement §3.4 and Q2 both turn on. A gate that cannot red on the plan's own worst example is PR #43's name-vs-shape lesson, unlearned. **Mutation-verify against both known sites specifically:** restore the original `low_stock_products` query (`:548` `<` and `:549` `-`), watch T-8 red; restore `count_low_stock_products` (`:585` `<`), watch T-8 red **again** — one site's mutation passing is not evidence the other is covered. Also verified to red on `COALESCE(col, 0)`-wrapped operands, since that is the form both real sites take.<br><br>✅ **BUILT 2026-08-01 (S452), closing M-1.** `tools/cut_gate_money_arith.sh` + `tools/adr0108_money_arith_scan.awk` + `tools/adr0108_money_arith_pending_folds.txt` + `tools/cut_gate_money_arith_probes.sh`, wired into `cut-gate.yml` beside the SQLite posture gate. Green over 672 SQL statements in 295 files, register exact at 7 records / 4 sites. Four corrections to the spec above, each measured rather than assumed:<br>(a) **It is not a grep.** `COALESCE(stock_qty,0) < COALESCE(min_stock,0)` puts a `)` — not a column — beside the operator, so no operator/column pattern can see it. The scanner marks census columns, then collapses parenthesised groups innermost-first, propagating the taint through wrappers, and tests operator adjacency at every collapse. `CAST(SUM(CAST(il.quantity AS DECIMAL(38,6)) * il.unit_price) AS VARCHAR)` reduces to `SUM(~m~ * ~m~)` and fires four records across two classes.<br>(b) **The comparison arm is R2-only, and the "A–D" above is corrected to that.** Comparing or ordering an R1 `INTEGER` money column in SQLite is exact and correct; reddening on `ORDER BY unit_price` would be crying wolf, and a gate that cries wolf gets switched off — the same call §3.3 makes for T-5(d)/(e). §3.4's own Q2 sweep is per-column over the **R2 ten**, and that is the hazard's real boundary. The arithmetic arm stays over all of A–D (`SUM` on R1 is exact but raises on i64 overflow).<br>(c) **The named mutation targets are already fixed, so the mutations are synthetic and the proof is stronger for it.** `repository.rs:548/549/585` were folded into `low_stock_candidates` on this branch before T-8 landed; the gate is green on them because they are *fixed*, not because it is blind. P2/P3/P4 plant those exact statements — the `<`, the `-` deficit `ORDER BY`, and the second function's twin — as **three separate mutations**, honouring F2's "one site's mutation passing is not evidence the other is covered."<br>(d) **The census is wider than §3.4's prose.** "All ten R2 columns" omits `quantity_dec` and both `quoting_parameters` rate columns, which §3.2 D does list. The const list is the union of §3.2's tables: 10 R1 + 13 R2 names. | §3.4, Q2, F1, F2 |
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
| **T-20** | **The WAL snapshot claim, pinned in both directions** (F7/Q10). Commit on connection A → read on a **pre-existing** connection B **in autocommit** → B sees it. Twin: B inside a transaction that has **already issued a read** → B does **not** see it. §2.4 asserted the first and never mentioned the second. *(**CORRECTED 2026-07-31, R-4:** this row said "B inside an explicit `BEGIN`". Measured false — `BEGIN` is `BEGIN DEFERRED`, acquires nothing, and starts no read transaction; the snapshot is taken at the **first read statement**. A T-20 written to the old wording would have failed and then been "fixed" into whichever assertion went green, pinning a false claim. The corrected pin is `t20b_the_snapshot_is_taken_at_the_first_read_not_at_begin`.)* | F7, Q10, §2.4, R-4 |
| **T-21** | **Nested `read()`-inside-`write()` NEVER REACHES THE ENGINE** (F7/Q11). *Rewritten 2026-07-31 by finding R-3 — the original "aborts loudly rather than waiting out `busy_timeout`" described a race that cannot occur, and was unwritable until the SQLite arm's mutex choice was made.* Per R-3 the SQLite arm **keeps `lock_recovering()`**, so the nested case resolves against the Rust `Mutex`: tripwire panic in debug, mutex deadlock in release, `busy_timeout` never involved. Two arms — behavioural (`#[should_panic]`, `debug_assertions`-gated) **and structural** (`Handle::read()` still calls `lock_recovering()`, with no engine-gated arm returning before it), because the tripwire panics *before* the lock and would mask the decision being reversed. **LANDED** — `crates/aberp-db/tests/adr0108_t21_nested_read_in_write.rs`. | F7, Q11, rule 13, R-3 |

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
| **S-1 — THE DEV TENANT HAS NO TAMPER-EVIDENCE COVERAGE AT ALL, so Step 5's stated exit criterion cannot be met and neither can §6.2's rollback verification.** Measured 2026-07-31 against a byte-identical copy of `apps/aberp-ui/aberp.duckdb` (240 `audit_ledger` rows): **`signed_entry_count = 0`, `anchor_count = 0`.** The S441 / ADR-0087 per-entry signature layer and the qualified-timestamp anchors are simply not populated on this tenant. B1's non-zero precondition therefore hard-stops the migrator *and* `rollback_to_duckdb.sh`, both correctly — an equality between two zeros is not a check. But the consequence is sharper than "the gate is strict": **§6.2 step 7's verification can never report PASS against the actual DEV database**, and §6.2's own argument is that "a rollback path exercised once at the end is a rollback path that has never been exercised". Every other check in the drill passed (all 40-odd per-table row counts, the WAL check, the artefact digests); only these two fail. | **Step 6's adversarial, as an input rather than a finding to fix in flight.** Two candidate dispositions and they are genuinely different: (a) the DEV ledger's signature/anchor layer is *supposed* to be populated and is not — a live defect in S441/ADR-0087's wiring, which would make this the most valuable thing Step 5 found; or (b) it is expected to be empty on a DEV tenant that has never opened a signed session, in which case B1's non-zero precondition needs a stated, testable exemption rather than an unreachable PASS. **Do not soften the precondition to make the drill green** — that is the exact fail-open B1 exists to stop. **RULED 2026-08-01 (Step 6): disposition (b), and the root cause is in the code, not inferred.** `serve.rs:2922` reads `tenant_dap_config`; `spawn_dap_audit_chain` runs **only** when the tenant's `dap_enabled` is true, and `tenant_registry.rs:255/280` default it to **`false`** — `serve.rs:2966` even fails *safe* to OFF when the registry cannot be read ("assuming OFF (unsigned chain)"). The DEV `test` tenant has never opened a signed session, so `signed_entry_count = 0` and `anchor_count = 0` are the **correct** state, not a wiring defect. Two sharpenings the ledger row did not have: (i) a **PRODUCTION build REFUSES TO START** with `dap_enabled = true` (`serve.rs:2945`, the ADR-0099 scanner-blind-fork guard), so signed entries are today **unreachable in prod by construction** — which means B1's precondition can never be satisfied on a prod-shaped tenant either, and §11's cutover list must say so; (ii) even with `dap_enabled = true`, **business appends are never signed** — `append_in_tx` (`storage/mod.rs:615`) hard-passes `None` for the session, so only session-lifecycle entries carry an `event_sig`. `signed_entry_count` is therefore a *small non-zero* number under DÁP, never "all entries", and any future gate that assumes otherwise is wrong. **B1's refusal is CORRECT and MUST NOT be softened**: it says "this database has no tamper-evidence to verify", which is true, and turning it into a green two-zero equality would be the fail-open. The drill is made green by giving it something to protect — see the demonstration plan in the §9 row below. |
| **S-2 — THE DEV TENANT'S AUDIT MIRROR IS AHEAD OF THE DB TABLE BY 5 ENTRIES, TODAY.** Measured in the same snapshot: `head_seq = 240`, `mirror_tail_seq = 245`. This is the **2026-07-19 shape** (`docs/runbooks/audit-mirror-defork-20260719.md`), recurring — five committed audit appends that the fsync'd mirror kept and the DB table lost. It is what R-5 looks like from the outside. `classify_mirror` classifies it correctly and routes to `heal_from_mirror_ahead` with "STOP: a migration is not a repair tool", so the migrator would refuse here too even with S-1 resolved. | **The existing heal path, run by the operator, before any real-DEV migration** — not by this migration and not by a session doing something else. Recorded here because it is a *second*, independent reason the Step-5 exit criterion is unreachable today, and because a session that fixed only S-1 would then hit this and might read it as a Step-5 regression. **RULED 2026-08-01 (Step 6): a DEV-only data artefact, safe to reconcile, and the migration must NOT learn to handle it.** The mirror is an append-only fsync'd sidecar; a DuckDB checkpoint folds and truncates the DB's WAL but cannot touch the mirror, so "mirror ahead by N" is the outside view of R-5's fork-close (see the R-5 row) and nothing about it is engine-specific or migration-specific. `classify_mirror` already routes it to `heal_from_mirror_ahead` with "STOP: a migration is not a repair tool", which is the right refusal: a migrator that reconciled a divergent chain would be *writing* history under the guise of copying it. **The reconcile is the existing heal path, run by the operator, in its own focused session** — `docs/runbooks/audit-mirror-defork-20260719.md`, repair FIRST and only then re-run the drill. Not run here: this session is analysis + test-hardening and does not touch `~/.aberp/**` or the live DEV tenant. |
| **M-1 — T-8 DOES NOT EXIST, AND §3.1 ASSERTS IT DOES. R2's guard count is ONE, not two.** Measured 2026-08-01 (Step 6): `tools/` contains `cut_gate_sqlite_posture.sh` + `cut_gate_sqlite_posture_probes.sh` and no arithmetic/comparison gate; neither `.github/workflows/ci.yml` nor `cut-gate.yml` runs a T-8 step; a tree-wide search for `T-8` / `t8_` returns **three doc-comment citations and zero implementations** (`crates/aberp-db/tests/adr0108_money_representation.rs:25` and `:109`, `apps/aberp/tests/adr0108_step5_billing_family.rs:344`). §3.1's corrected note says R2's guards "are exactly two, and they are now known to be the only two" — the Rust `Decimal` bind and T-8. **One of the two is not built**, so three landed artefacts document a mitigation that has never run. **Exposure today is ZERO** — `modules/billing` has not crossed the seam, so the only SQLite bind path into an R2 column is `carry_billing`, which binds validated `String`s and `i64`s. The exposure arrives with the family's Rust-side crossing. | **Step 7's FIRST commit, before any family crosses**, and it is a blocker rather than a nicety: §8 T-8's mutation targets (`repository.rs:548` `<`, `:549` `-`, `:585` `<`) are Step 7's own fold sites, so the gate and its mutation proof land together — which is why Step 6 did not build it here (rule 3; a gate landed early would either red on day one or ship with an allowlist for the two sites it exists to catch). Until it lands, **§3.1's "exactly two" sentence is wrong and is corrected to "one guard today; T-8 is owed"**.<br><br>✅ **CLOSED 2026-08-01 (S452). T-8 is built, and it landed BEFORE Step 7 rather than inside it.** `tools/cut_gate_money_arith.sh`, `tools/adr0108_money_arith_scan.awk`, `tools/adr0108_money_arith_pending_folds.txt`, `tools/cut_gate_money_arith_probes.sh`; wired into `cut-gate.yml` beside the SQLite posture gate and its probes. Green over **672 SQL statements in 295 files** (`.rs` non-test items **and** all 7 `.sql` migration files), with **14/14 probes behaving** — 12 red, 2 stay-green. **R2's guard count is now two, and §3.1 says so with a file path.**<br><br>**The scheduling argument above was built on a stale reading and is retracted.** It said the gate could not land early because its mutation targets are Step 7's fold sites, so it would "either red on day one or ship with an allowlist for the two sites it exists to catch". Measured: `repository.rs:548/549/585` are **already folded** on this branch — the `<` and the deficit ordering run in Rust over `Decimal` inside `low_stock_candidates`, and only a `COALESCE(…)` projection survives in SQL. So neither horn applied. The gate is green on the real tree, and P2/P3/P4 red on those three statements as **synthetic** mutations in throwaway copies — which is the stronger proof, because it does not depend on the tree still carrying the defect.<br><br>**What DOES stay owed to Step 7** is narrow and it is not the gate: the four `SUM` sites §3.4 schedules for a Rust fold (`reports.rs::query_outgoing_groups`, `::query_eur_huf_equivalent`, `repository.rs::record_movement`, `::rebuild_stock_cache_for_tenant`) sit in the **pending-fold register**, which is a **ratchet, not an allowlist**: a new site cannot join it (T8-3) and a folded site cannot linger in it (T8-4, mutation-verified by P8). The `reports.rs` pair additionally waits on **M-2**, since §3.4's fold changes the rounding. |
| **M-2 — §3.4's PRESCRIBED FOLD IS NOT VALUE-NEUTRAL, AND T-5(c) PINS THAT IT IS.** §3.4 says `reports.rs:800` becomes a Rust fold "with `Money::checked_mul_decimal` … + the existing `decimal_str_to_i64` round-half-even". Those are two *different* rounding orders. Today the SQL sums the **unrounded** `DECIMAL(38,6)` products per `(invoice, vat_rate)` group and rounds **once**, at `decimal_str_to_i64`; `checked_mul_decimal` rounds **per line** and then the fold sums whole minor units. On any invoice with a fractional quantity these differ. `reports.rs:1010–1014` already **documents** the choice ("per-line rounding would match NAV byte-perfect but the dashboard is a management view"), so it is a known, deliberate divergence between the ÁFA dashboard and the NAV filing — but §3.4 does not say the fold changes it, and **§8 T-5(c) asserts the folded result "equals the pre-migration DuckDB `DECIMAL(38,6)` aggregate for every invoice"**, which is the opposite claim. A session executing §3.4 literally will write the fold, watch T-5(c) red, and repair whichever of the two is cheaper to change — the shape ADR-0107 §4.1 exists to stop. | **Step 7 / the cutover session, as an explicit product decision, taken BEFORE the fold is written.** Either (a) keep the single-rounding management view — then the fold must sum `Decimal` products and round once, and §3.4's `checked_mul_decimal` wording is wrong; or (b) move the report to per-line rounding so it ties to the filing — then §3.4 is right, T-5(c) must be restated as "equals the pre-migration aggregate **for invoices with integral quantities**", and the change of published figures needs saying out loud. **Not decided here** — it is Ervin's call which number the ÁFA dashboard should show. Recorded because the plan currently contains both answers.<br><br>✅ **CLOSED 2026-08-01 (S453). Ervin ruled (b): ÁFA rounding is PER-LINE — the report shows what was filed.** Landed as Step 7's first commit, before any family crossed.<br><br>**The ruling alone was not sufficient, and this is the part worth reading.** "Round per line" would still have missed, because the two paths differed in **rounding mode** as well as granularity. The filing's per-line VAT is `floor(net × bp / 10_000)` — integer division, **truncating** (`LineItem::vat_amount`, `invoice.rs:92`); `reports.rs` rounded **half-even**. A fold that rounded per line but kept half-even would have tied on most invoices and diverged on exactly the `.5` remainders — the worst possible failure shape, because it looks correct in testing. So the fix is not equivalent arithmetic but the **same functions**: `line_net_total` / `line_vat_amount` were extracted as free functions in `modules/billing/src/domain/invoice.rs`, `LineItem::{net_total, vat_amount}` now delegate to them, and `reports.rs::fold_outgoing_lines` calls them directly. `nav_xml::write_summary` sums those methods, so `report == filed` is a property of there being one implementation, not of two implementations agreeing.<br><br>**Published figures move, said out loud as the row demanded.** Worked example, now the regression fixture: two 27% lines of 50 Ft net. Filed VAT `13 + 13 = 26 Ft`; the old report printed `round_half_even(100 × 2700 / 10_000) = 27 Ft`. The dashboard was one forint high on that invoice, always in the same direction, scaling with line count. It was documented at the time as "approximate within rounding tolerance of the per-line figures" — true, and beside the point: a management view a bookkeeper cannot reconcile against the bevallás is a reconciliation problem, not a simplification.<br><br>**Two things fell out that the ledger row did not anticipate.** (i) The report never read `vat_rate_kind` at all — it derived VAT from basis points alone, so an exempt line admitted with a non-zero rate by one of ADR-0106's gate-bypassing doors would have been *reported* with VAT the filing correctly carried as zero (ADR-0103 Invariant V). The fold inherits the kind check for free by calling `line_vat_amount`. (ii) `decimal_str_to_i64` and `round_half_even_div` both became dead and were deleted with their tests (rule 12), which is what took the third `unwrap_or(0)` at `:1279` with them.<br><br>§8 T-5(c) is restated accordingly; §3.4's site-1 row carries the correction; the two `reports.rs` records left the T-8 pending-fold register in the same commit, as T8-4's ratchet requires. |
| **R-5 — A FOREIGN CONNECTION'S `close` SILENTLY DESTROYS EVERY SUBSEQUENT COMMIT'S DURABILITY. Live in production today, on DuckDB, on 13 in-serve routes. Measured, deterministic (3/3), with a no-fork control and a mutation.** The `Handle` sets `disable_checkpoint_on_shutdown` + `wal_autocheckpoint='1TB'` (`lib.rs:648`) so its commits stay WAL-resident *by design*. A co-resident `Connection::open` carries neither pragma, so **its close checkpoints**: the WAL is folded into the main file and truncated to zero. From that moment the writer's WAL is gone and does not come back — 10 further commits returned `Ok` and were written **nowhere**; on process exit they are gone. **The read-fork class is not a stale read.** The forked read itself is coherent (DuckDB replays the WAL on open); the stale read is a *symptom of a prior fork's close*. **Consequences for this plan's framing:** (i) read/write is the wrong axis — the injury is the `close`, so the census that matters is the **ADR-0098 opener census (81)**, not the read-fork subset (33); (ii) the 13 GROUP-A in-serve entries are not stale-read nuisances — the **first** hit of any one of them ends that `serve` process's write durability for invoices, audit rows and the invoice-number floor; (iii) it settles ADR-0107 §1.3 F1. | **ITS OWN PR, BEFORE ANYTHING ELSE — explicitly NOT folded into this migration** (CLAUDE.md rule 3). This plan is DEV-only (§11 authorises no cutover), so **prod stays on DuckDB and this stays live for as long as it does; the migration is not its fix.** Two shapes, in preference order: **(1) containment — make every opener carry the two pragmas** at every `Connection::open` on a live tenant DB (measured to reduce the loss to zero; hours of work; the forks remain, so it is containment, not a fix); **(2) migrate the 13 GROUP-A routes to the `Handle`**, which is what the frozen baseline says should happen — larger, and it is product work on live invoice/NAV routes. The containment should land first regardless, because it stops the bleeding on a defect that is live today. Evidence: `docs/findings/read-fork-audit-sqlite-20260731.md` §3, `duckdb_a_foreign_close_silently_destroys_every_later_commit`. |
| The frozen baseline's GROUP-A rationale states the mechanism as a stale read of "the last-checkpointed **SUBSET**" (`tools/adr0099_read_fork_structural_baseline.txt`). **Measured false** — see R-5. | Same PR as R-5: the baseline's header text is where the next reader learns the mechanism, so leaving it wrong there is worse than leaving it unfixed. |
| The read-fork gate and the write-fork gate partition on read/write, but the hazard is the **close** and does not respect that partition. | Recorded here; a gate change belongs with R-5, **not** with this migration. |
| **`reports.rs:872` `decimal_str_to_i64(&s).unwrap_or(0)` — a LIVE ÁFA-report fail-open running in production today, on DuckDB, independent of this migration.** A parse failure prints **0 HUF** instead of failing. Siblings at `:827` and `:1279`. | **Fixed in-migration**, Step 5, in the same commit as the `reports.rs:861` fold — the fold's `Result` replaces the swallow, and the two are the same three lines (§3.4). **Not a separate cut**, and the Step-5 PR body must name it as a pre-existing production defect so it is not miscounted as migration collateral. If Step 5 is deferred or the engine decision reopens at Step 4, **this reverts to owing its own PR** and does not lapse.<br><br>✅ **CLOSED 2026-08-01 (S453), in Step 7's first commit rather than Step 5's** — Step 5 landed only its migrator half and the escape clause above kept the debt alive, which is exactly what it was written for. **All three are gone**, and none by adding an `expect`: `:827`'s enclosing `row_to_outgoing` no longer parses an aggregate at all (the fold reads the raw columns and a non-decimal `quantity` is a hard `FromSqlConversionFailure`); `:872`'s reader is replaced by a `checked_add` fold whose overflow propagates as `Err`; `:1279` was inside `round_half_even_div`, which the M-2 fold made dead and which was deleted with its test. **The ÁFA report can no longer print 0 HUF in place of a figure it failed to compute.** |
| **M11 / T-12 are NOT closed by Step 7 Part B's partners crossing, and §7's Step-7 line ("partners (+ M11, T-12)") is corrected to say so.** M11 is `partners.rs:1001–1005` (the duplicate-partner guard: five `LOWER()` comparisons) and `:1049` (the typeahead's two unescaped `LIKE` patterns). Part B landed the **migrator half** — DDL, carry, gate — and changed no query: all six sites still execute on a DuckDB `Connection`. A T-12 written against today's tree could therefore only assert that **DuckDB's** `LOWER()` folds `Á` and that DuckDB's `LIKE` over-matches on a `%` needle. Both are true, neither is the property M11 exists to protect, and pinning them would produce a green T-12 with the mitigation absent — the same "three landed artefacts document a mitigation that has never run" shape M-1 was. **Exposure today is ZERO**: no SQLite connection in the tree runs a `LOWER()` or a `LIKE` against `partners`. | **The partners family's Rust-side crossing (the cutover), as its first commit** — the same sequencing M-1 got, and for the same reason: the mitigation and its mutation-verifiable pin land together, in the commit that first makes the queries reachable on SQLite. §7's own rule is the authority: *a refusal whose test cannot be written yet is not landed yet.* Part B pre-positions what T-12 will need — the DEV-shaped fixture in `adr0108_step7_partners_family.rs` already seeds `Árvíztűrő tükörfúrógép Kft.` / its all-caps legal name **and** a row carrying a literal `%` and `_` (`100% Precision _ Machining`), so T-12 is a test to write, not a fixture to build. ⚠ Note for whoever writes it: SQLite's `LOWER()` is **ASCII-only**, so the guard silently stops matching `Á`/`Ű`/`Ő` the moment the query crosses — a *false negative on a duplicate-partner check*, i.e. it admits the duplicate rather than blocking a good row, which is the direction that does not announce itself. |
| DEV DB measured mode **0644**; no code chmods the tenant DB — **true today, engine-independent** | M9 / Step 2, or a standalone 5-line PR now (PR #49 already flagged this) |
| `nav_xml.rs:1788` write path is `f64` while `:2658` read path is exact `Decimal` — a **rule-7 fork on the NAV VAT rate**, pre-existing and engine-independent; value-exact for all four legal HU rates, so not a filing defect | Closed by the Step-5 `Decimal` conversion (B2, §3.3), which also empties T-5(e)'s allowlist |
| `MirrorEntry` cannot round-trip a signed entry — the mirror is a divergence detector, not a backup, and its own comment says so (`mirror.rs:211–214`) | **Out of scope.** Recorded because B1 was the first time that design limit had a consumer that assumed otherwise. If the mirror is ever to be a recovery source, that is its own ADR — and it would have to add three columns and a signature-preserving encoder first. |
| `verify_chain_signed`'s anti-strip check is keyed on `session_id` surviving (`verify.rs:138–146`), so it cannot see a strip that nulls `session_id` too; and `ChainVerdict.fully_anchored` reads `true` on a ledger with zero anchors (`:188`) | **Out of scope for this plan, and worked around inside it** — §6.3's gate asserts counts rather than the verdict flags. Flagged because a *future* consumer will reach for `fully_anchored` and get the reassuring answer. Its own PR. |
| `material_inventory.*_qty` is `DOUBLE` while `stock_movements.qty_delta` is `DECIMAL` — **two representations of one physical quantity** (rule 7) | ~~**Out of scope.** Recorded because migrating both as-is under `STRICT` makes the divergence look sanctioned. Needs its own decision.~~ ✅ **RESOLVED AT THE STORAGE LAYER 2026-08-01 (S455, Step 7 Part C).** Conservative resolution, taken rather than deferred because the deferral's own stated cost — a `STRICT` schema blessed by a green gate that says a quantity is a float here and exact there — is paid the moment the family crosses. **All five columns** (`inventory_balances.on_hand_qty` / `reserved_qty` / `committed_qty` / `consumed_qty`, `inventory_reservations.qty`) **cross as R2 `TEXT` with zero value change**, enforced per value in `migrate_products::canonical_decimal_from_f64`: shortest round-trip rendering → refuse above the canonical quantity scale of 6 → refuse unless the `Decimal` converts back to the identical `f64`. **Refuse, never round.** §3.2 E carries the correction block; the five columns move from category E to category C. |
| **The APP-LAYER half of the same rule-7 divergence is still open, and it is not the storage one.** `apps/aberp/src/material_inventory.rs` models these quantities as `f64` in Rust — `Balance.{on_hand_qty, reserved_qty, committed_qty, consumed_qty}` (`:302–305`) plus the derived `Balance.available_qty` (`:313`), the `f64` bind/read of `inventory_reservations.qty`, and `MaterialInventoryError::InsufficientMaterial`'s four `f64` fields (`:206–212`) — while `aberp-inventory` models the *same physical quantity* as `rust_decimal::Decimal` (`StockMovement.qty_delta`, `products.stock_qty`). After S455 the two agree in **storage** and still disagree in **Rust**, which is the narrower and more honest statement of the divergence. Exposure is bounded but real: the DEAL saga's sufficiency check (`requested + reserved + committed <= on_hand`) is float arithmetic, so it can admit or refuse a marginal reservation on a representation artefact. | **Its own PR, not this plan, and not Step 8.** It is a change to the saga's arithmetic, its 409 error payload, the SPA toast that renders those four numbers, and the calibration tests — none of it engine-related, all of it reachable today on DuckDB. Recorded with the exact symbols so the next session does not have to re-derive the surface. **Do not fold it into a migration commit**: mixed with a storage change it would be indistinguishable from migration collateral, which is precisely how the original divergence survived unnoticed in two source documents. |
| **§3.4's three cache-rebuild folds did NOT land with the products/inventory family, and §7's Step-7 line ("incl. §3.4's three cache-rebuild folds") is corrected to say so.** They are `SUM(qty_delta)` inside `aberp_inventory::repository::{record_movement, rebuild_stock_cache_for_tenant}` (the third §3.4 row, `bin/rebuild_stock_cache.rs`, carries no SQL of its own — it calls the second). Part C landed the **migrator half** — DDL, carry, gate — and changed no query: both statements still execute on a DuckDB `Connection`, where `DECIMAL` is a real decimal type and `SUM` does not coerce. **Exposure today is ZERO**, and the mutation that proves the fold matters — a `REAL`-coerced `SUM` writing a float `stock_qty` back into the products cache — is unreachable until the queries cross. | **The family's Rust-side crossing (the cutover), as its first commit** — the same sequencing M-1 and M11/T-12 got, and §7's own rule is the authority: *a refusal whose test cannot be written yet is not landed yet.* The T-8 **pending-fold register keeps both entries** and stays honest doing it: each still matches a live statement, so T8-3 (nothing new may join) and T8-4 (a folded site may not linger) are green because the sites are *there*, not because the gate is blind. ⚠ **Note for whoever writes it**: the fold is not "sum the same thing in Rust" — `record_movement`'s `SUM` runs **inside the movement-recording transaction** and its result is written straight back to `products.stock_qty`, so the fold must stay in that transaction and must not become a second round trip that could see a torn read. |
| **The work-orders / BOM family crossed as its MIGRATOR HALF only (Step 7 Part D), so `crates/aberp-work-orders/src/repository.rs` still runs every one of its queries against DuckDB.** Same shape as Steps 5, 7B and 7C and recorded for the same reason: the STRICT DDL, the typed carry and the gate's arm are landed and green, and none of that makes the product read or write the SQLite copy. **Exposure today is ZERO** — no SQLite connection in the tree touches `work_orders`, `boms`, `routings` or `bom_revisions` outside the migrator and its test. ⚠ The specific thing a later session must not misread: `routings.est_cost_huf` is now **R2 `TEXT` in the SQLite schema and still `DECIMAL(18,2)` in DuckDB**, and `repository.rs:1544/1625/1954` already read it as `CAST(est_cost_huf AS VARCHAR)` → `Decimal`, so the Rust-side crossing needs no type change there — but it does need the R2 bind discipline (`Decimal`, never `f64`) on the **write** side, because `STRICT` will not enforce it (§3.1's corrected note). | **The family's Rust-side crossing (the cutover), as its own commit** — the same sequencing M-1, M11/T-12 and §3.4's inventory folds got, and §7's own rule is the authority: *a refusal whose test cannot be written yet is not landed yet.* |
| **The migrator's two R2 validators differ in strictness, and the difference is now MEASURED rather than assumed.** `migrate_billing::canonical_decimal` (used by the invoice, work-orders and BOM families for a verbatim `DECIMAL` → `TEXT` carry) is built on `Decimal::from_str`, which **accepts exponent form** (`"1e6"`) and **rounds past 28 significant digits rather than erroring**. `migrate_products::canonical_decimal_from_f64` (used for the five rule-7 `DOUBLE`s) is built on `Decimal::from_str_exact`, which errors on both. Both behaviours are pinned in `adr0108_step7_work_orders_family::every_carried_decimal_either_round_trips_byte_identically_or_is_refused`'s adversarial table, with the measurement written beside each row. | **Nothing, today — and the condition under which that stops being true is the point of recording it.** Exposure is zero for every family that has crossed: the source is always a `DECIMAL(18,6)` or `DECIMAL(18,2)` column, which has 18 digits and cannot reach 28, and DuckDB's `CAST(… AS VARCHAR)` never emits exponent form. The lenient validator is also *correct* for a verbatim carry — the string is stored unchanged, so the SQLite read path applies the same `from_str` rounding the DuckDB reader already applied and both sides see one value. **It stops being zero for a family whose R2 source is not a `DECIMAL` column** — i.e. Step 8's §3.2 D columns, which are `DOUBLE` today. Step 8 must use the `from_str_exact` path (`canonical_decimal_from_f64`) for those, not `canonical_decimal`; picking the lenient one would silently admit a value the column cannot represent. |
| `qc_inspections.deviation` is a derived `REAL` driving a pass/fail verdict | ~~Out of scope; flagged in §3.2 E~~ **STORAGE DISPOSITION TAKEN 2026-08-02 (S457, Step 7 Part E): it stays `REAL`, and the "derived" part is what settles it rather than what worries it.** `qc::verdict` computes it as `actual - nominal` in `f64` (`verdict.rs:103`), so `25.03 - 25.0` is `0.030000000000000426` — scale 17. R2 refuses past scale 6, so the "exact" alternative would have **hard-failed the migration on an ordinary inspection row**. It crosses bit-identically and the gate proves it per row. **What remains open is the APP-layer question, and it is not a storage one:** a pass/fail verdict computed from `f64` subtraction against `f64` tolerances can flip on a representation artefact for a measurement sitting exactly on a band edge — the same shape as the `material_inventory` saga's sufficiency check in the row above, and it wants the same treatment. Its own PR: a change to `qc::verdict`'s arithmetic, its verdict tiers and its calibration tests, all reachable today on DuckDB and none of it engine-related. **Do not fold it into a migration commit** — mixed with a storage change it would be indistinguishable from migration collateral. |
| **The QA/QC family crossed as its MIGRATOR HALF only (Step 7 Part E), so `apps/aberp/src/quality.rs` and `crates/aberp-qa/src` still run every one of their queries against DuckDB.** Same shape as Steps 5, 7B, 7C and 7D and recorded for the same reason: the STRICT DDL, the typed carry and the gate's arm are landed and green, and none of that makes the product read or write the SQLite copy. **Exposure today is ZERO** — no SQLite connection in the tree touches `ncrs`, `ncr_transitions`, `capas`, `qa_inspections`, `qc_inspection_plans` or `qc_inspections` outside the migrator and its test. ⚠ Three things a later session must not misread. (i) **This family has no money and no quantity at all** — its only numbers are `seq`, eight §3.2 E measurements and two booleans — so the Rust-side crossing owes no R1/R2 bind discipline; what it *does* owe is the `bool` bind on the two §3.2 H columns, because a `"true"` bound as `&str` would read back as `false`. (ii) **The M11 hazard has no site here**, measured: `LOWER(` and `LIKE` return zero hits against all six tables, so unlike partners this family's cutover owes no ASCII-fold fix. (iii) **`finite_measurement` guards the migrator's read path only**; the product's own write path into the eight `REAL` columns is unguarded, so the Rust-side crossing must decide whether a non-finite measurement can be *written* at all — today it can, on DuckDB, and nothing rejects it. | **The family's Rust-side crossing (the cutover), as its own commit** — the same sequencing M-1, M11/T-12, §3.4's inventory folds and Part D got, and §7's own rule is the authority: *a refusal whose test cannot be written yet is not landed yet.* |
| ADR-0107 / the frozen baseline / its header disagree on the in-serve read-fork count (**14 / 13 / 9**) | Out of scope for the migration; a stale frozen baseline is the exact class PR #43 existed to prevent → its own PR |
| `aberp-mes::ledger_writer::write_one` appends through a fresh in-serve connection while the write-fork gate reports ZERO | ADR-0107 §5 says close it **by hand now** — a forked *append* forks the ledger under **any** engine. Independent of this plan; should land before Step 5. |
| The S392 NAV pre-flight is dead (0 `check_performed` in 225 mirror entries) | Orthogonal, engine-independent, and ADR-0107 §5 calls it the most under-weighted open item. Not this plan. |
| ~~ADR-0107 §1.3 finding F1 (is a forked read stale, or was D2a row loss?) is unsettled~~ | **CLOSED 2026-07-31 by the measurement in R-5 above.** Neither: the forked read is *coherent*, and the loss is the fork's **close** truncating the writer's WAL. The migration does not make it moot on prod — prod stays on DuckDB. Tracked as R-5, its own PR. |
| `apps/aberp/tests/serve_numbering_route.rs::put_then_get_round_trips_template` **flaked once** under the CI-shaped `cargo test -p aberp -p aberp-db --features sqlite-engine` arm (read back `start_value: 1247` instead of the `1` just written). Green in isolation and green on an immediate identical re-run; green in the default `--workspace` run. Cross-test interference on shared state, not an engine defect — the `sqlite-engine` feature changes no storage yet (Steps 1–4 are the reversible foundation). | ~~**Out of scope**, recorded 2026-07-31 so a future session does not misread it as a Step-5 regression.~~ **CLOSED 2026-08-01 (Step 6) — it recurred on the DEFAULT `--workspace` arm (which this row said was green), on the sibling test `put_preserves_identity_and_bank_sections`, and the cause is a two-line test-isolation defect exactly as predicted.** `unique_tmpdir()` (`serve_numbering_route.rs:29`) keyed the scratch directory on `(pid, nanos)` with **no per-call counter**, so two `#[test]` threads reading the clock in the same tick get the *same* directory and both write `seller.toml` into it. The observed failure is that signature verbatim: the identity/bank assertion read back a file containing only `[seller.numbering]` — the one the *other* test wrote. Fixed here with the `AtomicU64` per-call counter the ADR-0108 test files already use. Test-only, two lines; folded in rather than deferred because a non-deterministic suite makes **every** subsequent step's gate untrustworthy, and rule 4 makes the gates the per-step trust surface. Named in the Step-6 commit body so it is not miscounted as adversarial collateral. |
| ADR-0107 §2 lists `db_writer_lock` as retirable; ADR-0107 §3 B-cost-1 says money is already integer; ADR-0107 §4.1 Phase 0 does not scope the DDL rewrites | Amended in Step 1's PR body per PR #49's own deferral ledger, plus §1.1 G-2's `.sql` correction which PR #49 also missed |

---

## 10. The eleven open questions — **all closed**

These were the choices flagged for attack. All eleven were ruled on 2026-07-30
(§13.2 carries the reasoning); the dispositions below are current, and none of
them is an open item an execution session must resolve first. Four changed as a
result — Q2, Q5, Q7 and Q10.

| # | Question | Disposition | Where it lives now |
|---|---|---|---|
| **Q1** | Compile-time cargo feature vs runtime engine selector (§2.2 D1) | **Closed — compile-time.** Reversibility comes from *two files*, not from the selector, which the B3 fix makes more true rather than less. A runtime toggle costs a trait layer over 449 + 120 + 238 sites (R-2) and keeps every family simultaneously reachable on two engines — the half-migrated shape rule 14 forbids. If Ervin meant a runtime toggle, that is his to reopen; the case is made, not averaged. | §2.2 |
| **Q2** | `TEXT`-decimal vs scaled-integer for quantities/rates (§3.1 R2) | **Closed — `TEXT`, and the lexicographic risk is swept rather than deferred.** The per-column sweep over all ten R2 columns is **done, in §3.4**: exactly two hits, `repository.rs:548` and `:585`, both folded into Rust in Step 7. The original deferral said *`ORDER BY`* and would have missed the `WHERE` — the half that returns wrong rows. | §3.4, T-8, Step 7 |
| **Q3** | `routings.est_cost_huf` → `TEXT` (R2) rather than `INTEGER` (R1) (§3.2 B) | **Closed — R2, the one documented R1 exception.** `Option<Decimal>` in Rust, never on the NAV wire, the PDF, or ledger totals. R1 would force a "what is HUF's minor unit for an *estimate*" product decision to serve consistency alone. | §3.2 B |
| **Q4** | The five quoting `f64` money columns (§3.2 D) — Step 8, converted to `Decimal` | **Closed — convert, and the strictness stands.** §3.2 D's pre-commitment ("if Step 8 overruns, *stop* — do not migrate as `REAL`") is the rule-11 guard that stops a later session taking the easy branch. Not softened. | §3.2 D, Step 8 |
| **Q5** | The `ON CONFLICT` sites: add `UNIQUE` indexes, or rewrite as `SELECT`-then-write? (§4.3) | **Dissolved.** There are **5**, not 21 (16 doc comments + 1 test string), and all 5 conflict targets are **already the declared `PRIMARY KEY`**, verified statement-by-statement. Zero indexes, zero rewrites — so the `[[no-sql-specific]]` tension the question was built around never existed. Step 3's obligation is 5 confirmation tests. | §1.2, §4.3, M12 |
| **Q6** | `.sql` migration files: split the `ALTER` lines out into `ensure_columns` (§4.2) | **Closed — split; 8 lines move.** Beats owning a load-time rewriter forever (rule 12). "A family's schema lives in two places" is real but small, and `CREATE`-stays-SQL / `ALTER`-moves-to-Rust is a legible line. | §4.2 |
| **Q7** | Does the ledger cross by mirror replay or table copy? (§6.3) | **Inverted — table copy is the source, the mirror is a three-arm cross-check.** The circularity this question asked about was not the defect; the **lossiness** was. And the durability argument for mirror-as-source misread its own incident: on 2026-07-19 the *mirror* was ahead and divergent and the **DB was authoritative**. See B1. | §6.3, T-18 |
| **Q8** | Drop quoting job history rather than write an `f64 → Decimal` converter (§6.3) | **Closed — drop.** Correct for a disposable DEV DB; **wrong for prod**, and §11.1 carries that forward explicitly rather than inheriting it silently. | §6.3, §11.1 |
| **Q9** | Keep the census / fork gates frozen instead of deleting per family (§8) | **Closed — keep, and the divergence from ADR-0107 §4.1 is the better call.** A gate deleted is a gate that cannot protect the state you roll back to. Under a rollback-only constraint that is a requirement, not a preference, and rule 12's objection to dead machinery does not reach machinery guarding a live rollback target. | §8 |
| **Q10** | Is `read()` returning a real second connection a behaviour change anywhere? (§2.4) | **Was the plan's weakest claim; now audited, not assumed.** "Strictly sees more" is false inside an explicit transaction. Step 3 classifies **all 102 `read()` sites** (R-2 — the stated 50 was a 49 % sample) on two axes, gates Step 5, and pins the WAL snapshot claim in **both** directions (T-20, corrected by R-4) plus the nesting behaviour (T-21, rewritten by R-3). This is the class behind five of July's incidents. **Audit delivered 2026-07-31: both axes EMPTY, one required change (R-1), now fixed.** | Step 3, T-20/T-21 |
| **Q11** | `busy_timeout` value (M7 said "explicit and finite", no number) | **Closed — 5000 ms; the T-21 condition is now SATISFIED** (T-21 landed 2026-07-31 in the R-3 shape). Revisable downward on Step 2's measurement. *R-3 correction: the stated Q10↔Q11 coupling does not exist — because the SQLite arm keeps `lock_recovering()`, a nested `read()` never reaches SQLite, so no finite timeout could ever have hidden that deadlock. 5000 ms is a **write**-contention ceiling (two `BEGIN IMMEDIATE` writers; a checkpointer behind a long reader), measured to be near-irrelevant on the read surface.* | Step 3, M7, T-3d, R-3 |

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
| **Q10** | **Must-fix (F7).** | The plan's "assumed safe — it strictly sees *more*" is the one place a load-bearing engine-semantics claim is asserted rather than pinned, over 238 sites (R-2 — the review said 84), in the class that caused five of July's incidents, deferred to a step that runs beside the family it guards. It is also **coupled to Q11** in a way the plan does not notice: a finite `busy_timeout` is what converts a nested `read()`-inside-`write()` from an immediate self-deadlock into a silent hang. |
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
| **F7** | Q10's audit is **exhaustive, gates Step 5, has a stated denominator**, and Q11's number is chosen with it. | Step 3, rewritten. Denominator measured and published: the **238** non-test Handle sites are **102 `read()` / 136 `write()`** (§1.2), so the audit covers **all 102** with the 136 as axis-(b) reachability context, closed under calls (ADR-0106's discipline, not line-local). *(**R-2, 2026-07-31:** this row originally said 84 = 50 / 34 and "all 50". The denominator **was** stated — and measured with a single-line, `.db.`-prefixed grep that cannot see rustfmt-wrapped chains or a `Handle` bound to a local. Stating a denominator is not the same as measuring one; `tools/adr0108_handle_census.sh` is now the measurement.)* Three pins: **T-20** pins the WAL snapshot claim in **both** directions — *its in-transaction half was worded falsely; see R-4* — **T-21** pins the nested `read()`-inside-`write()` behaviour *(rewritten by R-3: it never reaches the engine)*, **T-3d** asserts the pragma value. **Q11 closed: `busy_timeout = 5000 ms`; the T-21 condition is satisfied**, revisable downward on Step 2's measurement, raising it requires re-arguing R-3. |

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
