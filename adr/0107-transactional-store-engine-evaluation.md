# ADR-0107 — Database engine for the transactional system-of-record (invoice / audit ledger)

- **Status:** Proposed — **decision requested from Ervin**. No engine change, no
  code migration, no schema change is authorised by this document.
- **Date:** 2026-07-30
- **Deciders:** Ervin
- **Related:** ADR-0019 (storage strategy — one trait, relational SoT,
  engine-agnostic — the seam this whole evaluation stands on), ADR-0099
  (production durability-hardening lane, H1–H5), ADR-0098 (opener census, editions
  origin), ADR-0030 (audit-ledger mirror file), ADR-0082 (snapshot system),
  ADR-0008 (audit ledger / hash chain), ADR-0009 (NAV invoice issuing — 8-year
  retention), ADR-0059 + ADR-0100 (SaaS / cloud lane — already names
  Postgres-per-tenant), the memory pins `[[no-sql-specific]]`,
  `[[trust-code-not-operator]]`.
- **Supersedes:** nothing. **Constrains:** the deferred H4 durable-checkpoint step
  of ADR-0099 and the D1-lineage detector work (§5).

---

## Context

Ervin's question, in his words: *was last month's durability firefighting a
symptom of a poor DB choice — DuckDB being "lightweight but fragile"?*

The question is fair and overdue. July 2026 cost us seven named defects, five
merged PRs, one production boot refusal, three invoice numbers filed twice to NAV,
and roughly sixteen thousand lines of machinery that exists for no reason other
than to make the storage engine behave. That is a lot of engineering to spend on a
20 MB database that one person writes to from one desktop app.

This ADR answers the question with the month's actual evidence, and then puts
three options on the table with honest costs. It deliberately reaches a
recommendation — Ervin asked for a decision document, not a survey — but the
recommendation is a proposal, and §3 is written so that choosing against it is a
defensible engineering call, not a mistake.

**One framing note up front.** "Is DuckDB a poor choice?" and "is DuckDB buggy?"
are different questions with different answers. The evidence says DuckDB is *not*
notably buggy; it says DuckDB is being asked to provide a guarantee it does not
advertise, and that our compensations for that gap are where the month went. The
distinction matters because it changes which option is right.

---

## 1. Diagnosis — what actually failed, and why

### 1.1 The month's incident set

Every row below is a real, merged, referenced event from July 2026. "Class" is
assigned in §1.2.

| # | Date | Incident | Ref | Class |
|---|------|----------|-----|-------|
| 1 | 07-19 | **PROD refused to boot.** Audit mirror at seq 8060, DB at 8058, chains fork at 8056. Five committed audit entries existed in the fsync'd mirror and were **gone from the DB**. Repair branch `incident/audit-mirror-defork-20260719` @`d283d00`; prod was never written. | `[[project_aberp_mirror_defork_20260719]]` | **a + b** |
| 2 | 07-27 | **Live NAV-acked invoice read back "not found in audit ledger."** `print_invoice::render_to_bytes` (reached in-serve from the PDF route, auto-email compose, and manual resend) opened **three** independent DuckDB sessions. Invoice `TEST-ABERPNEW2026/0062`, gross 3 136 900 Ft, NAV-acked `SAVED`, then every follow-up read 404'd. Pre-existing since `a3458db`. | PR #40 @`baf5095` | **a + b** |
| 3 | 07-27 | **D2a — an exempt ÁFA base could be silently re-filed to NAV at plain 0%.** `serve::read_base_line_vat_kinds` forked the read, got an empty `Vec` via `.unwrap_or_default()`, and the ADR-0101 S2 guard (`.find(|k| !k.is_percent())`) **passed vacuously over the empty vector**. AAM / reverse-charge / intra-Community bases affected. Independently scored ship-blocking. | PR #41 @`d16f5f5` | **a + b + d** |
| 4 | 07-28 | **The email-attachment render forked the DB** — the same class as #2, on a path PR #40 missed. | PR #42 @`4acd42b` | **a + b** |
| 5 | 07-28 | **The read-fork gate was a name allowlist, not a rule.** Four defects (#2, #3, #4, E1) shipped straight through it. Rewritten to detect the *shape*. Surfaced **28 pre-existing forks, 12 live in-serve** — frozen as a ratchet, **not fixed**. | PR #43 @`a68635e` | **e** |
| 6 | 07-28 | **The new structural rule was defeated by `rustfmt`** — it matched line-local text, so three wrapped shapes read as clean. Five more forks surfaced (28 → 33), including live in-serve `handle_relay_send_email`. | `c065351` | **e** |
| 7 | 07-30 | **Invoice numbers 62 / 63 / 64 were each handed out 2–3×.** Every first use `SAVED` at NAV; every repeat `ABORTED` with `INVOICE_NUMBER_NOT_UNIQUE`. `invoice_sequence_state.next_number` was the allocator's only floor and it is not durable. **PROD was exposed** (the S392 pre-flight is compiled out of production builds — and is dead anyway: 0 `invoice.check_performed` entries in 225 mirror entries). | PR #46 @`c053cd0` | **a + b + d** |

**And the tear rate, which is the part that should worry us most.** The DEV
tenant's own directory is a forensic record. `apps/aberp-ui/` currently holds:

- 3 × `aberp.duckdb.audit.log.ahead-*.bak` (07-11) — mirror-ahead-of-DB
  preservations, i.e. three detected losses of committed DB state;
- 5 × `aberp.duckdb.audit.log.healed-*.bak` (07-20, 07-27, 07-28, 07-29, 07-30)
  — one per boot-heal replay, i.e. **five more tears in eleven days, one of them
  today**.

Eight recorded tears of committed data in twenty days, on a 20 MB database. Every
DEV session this month opened with a `db.auto_recovered` entry at the head of the
ledger. We had normalised that.

### 1.2 Classification

| Class | Meaning | Count (of the 7) |
|---|---|---|
| **a** | DuckDB is an OLAP/analytical embedded engine being used as an OLTP crash-durable ledger — the **enabling condition** | 5 |
| **b** | Our usage pattern — multiple in-serve openers, per-connection pragmas, checkpoint-off (H3), process kills — the **trigger** | 5 |
| **c** | A genuine DuckDB defect | **0** |
| **d** | Our own app-layer design defect, engine-independent | 2 (#3, #7) |
| **e** | A defect in the machinery we built to compensate | 2 (#5, #6) |

**Nothing in July was a DuckDB bug.** That is the honest headline, and it cuts
against the "fragile" framing. Every one of #1–#4 and #7 is our code opening a
second session on a file it already had open. Every one was fixed by routing
through the shared `Handle`. DuckDB did what it says on the tin.

The genuine DuckDB defects are real but older, and they are a minority:

- **S332 / 2026-06-10** — an upstream ART assertion crash on the highest-frequency
  audit write path (`FixedSizeBuffer::GetOffset → … → ART::Insert → WriteToWAL`),
  `kind=quote.email_outbox_fetched`, ~17k rows/day. Diagnosed; **no fix shipped**
  because the only ART indexes on `audit_ledger` are the load-bearing
  `UNIQUE (seq)` / `UNIQUE (id)` chain constraints, which have no droppable name.
  We shipped a regression harness and lived with it.
- **`duckdb#23046`** — torn checkpoint metadata when N actors checkpoint one file.
  This is the bug the entire H3 single-instance discipline exists to route around.
- **1.5.2 `FetchStringFromDict` read-path assertion** and the earlier
  **LoadCheckpoint family** (S375) — both forced version bumps (S393: 1.5.2 →
  1.5.3), and the bump's on-disk storage upgrade is **one-way**
  (`Cargo.toml:241`).

So: three or four genuine engine defects per quarter, all in the *storage /
checkpoint / index* layer — which is precisely the layer a transactional
system-of-record depends on absolutely, and precisely the layer of DuckDB that is
youngest.

### 1.3 The mechanism, stated precisely

This is worth writing down exactly, because the obvious reading is wrong and the
repo has been bitten by re-deriving it.

DuckDB shares **one `Database` instance per path per process**. A fresh
`Connection::open` on a path the process already has open therefore does not get
its own database — it gets a second *session* on the same instance. But
`wal_autocheckpoint` and `disable_checkpoint_on_shutdown` are **per-connection**
settings. `aberp-db` applies them to every connection it hands out; a session
opened outside that seam carries DuckDB's defaults. So the foreign session sits on
the shared instance with checkpointing switched **back on**, and **its close
checkpoints the shared instance's WAL** — the Handle's own settings cannot prevent
it.

Measured on the same file in the same process (`aberp-snapshot/src/take.rs:162`):
a `Handle` clone reports `wal_autocheckpoint` = **931.3 GiB**; a fresh
`Connection::open` reports **16.0 MiB**. Mutation-verified: re-introducing the
fresh open drains the WAL **3156 → 0 bytes**.

That fold, landing while `sync_mirror` had already durably appended the same rows,
is exactly what put the mirror at 8060 and the DB at 8058 on 2026-07-19.

**Why the compensations cannot fully close it.** ABERP is in a genuine bind, and
it is an engine-fitness bind, not a discipline one:

1. To avoid `duckdb#23046` (torn checkpoint metadata) we must stop uncontrolled
   checkpointing — hence `disable_checkpoint_on_shutdown` + a 1 TB
   `wal_autocheckpoint` on every Handle connection, and `checkpoint_enabled =
   false` in the H3 posture (`aberp-db/src/lib.rs:139`).
2. That leaves committed business rows **WAL-resident indefinitely** by design.
3. And the pragmas that protect the WAL are per-connection, so a single foreign
   session anywhere in a ~7-daemon `serve` process can undo them.

The mitigation for the engine's checkpoint bugs *creates* the WAL-loss exposure.
No amount of discipline resolves that; it can only reduce the number of places
where discipline must hold. **Every one of the 289 original openers was a place
where discipline had to hold.**

> **Finding F1 — an unresolved internal contradiction, surfaced not averaged
> (CLAUDE.md rule 7).** Two of our most careful documents disagree on whether a
> forked *read* is stale. PR #40's and D2a's write-ups both state that "a second
> instance reads only the last-checkpointed subset." `take.rs` states the
> opposite, explicitly to stop the re-derivation (`take.rs:177`): *"because the instance IS
> shared, the old fresh session did still read the live WAL — the export was not
> stale. The fold is the defect."* The `take.rs` account is the one carrying
> measurements; the PR #40/#41 accounts are inferential. The likely reconciliation
> is that D2a's empty `Vec` was genuine **row loss from an earlier fold**, not
> staleness. **This does not change any fix** — routing through the Handle is
> correct under either mechanism — and it does not change this ADR's
> recommendation. It is recorded because *the team does not have a settled model
> of its production engine's durability semantics after a month of studying it*,
> and that epistemic cost is itself evidence about engine fit. Settling it needs a
> measurement, which is out of scope for a doc-only change.

### 1.4 The decisive fact: we are not running an OLAP workload

DuckDB's entire reason to exist is vectorized columnar analytics. Measured across
`crates/`, `apps/`, `modules/`:

| Analytical SQL construct | Occurrences |
|---|---|
| Window functions (`OVER (`) | **0** |
| `PARTITION BY` | **0** |
| `WITH RECURSIVE` | **0** |
| `QUALIFY` | **0** |
| `USING SAMPLE` | **0** |
| `read_csv*` | **0** |
| `HUGEINT`, `list_value`, `struct_pack` | **0** |
| `GROUP BY` | 8 |
| `SUM(` | 14 |
| `COUNT(*)` | 80 |

Against that: **449** `params!` call sites and **120** `duckdb::Connection`
signatures — a single-row-write, single-row-read, point-lookup OLTP profile. The
only file with meaningful aggregation is `apps/aberp/src/reports.rs` (1 976
lines), and its heaviest construct is a cast-and-sum.

The dataset is **20 MB**. One operator. One node. One writer. Eight-year statutory
retention (ADR-0009:528).

We are paying DuckDB's transactional-durability weaknesses in full and collecting
**none** of its analytical strengths. That asymmetry — not fragility — is the real
answer to Ervin's question.

---

## 2. What we built to compensate, and what it costs

Each row is machinery that exists because the engine does not provide the
guarantee natively. The right-hand column is what a server-grade OLTP engine
(Postgres) or a battle-tested embedded OLTP engine (SQLite in WAL mode) gives for
free.

| Machinery | LOC | Purpose | Native equivalent |
|---|---:|---|---|
| `crates/aberp-db` (`Handle`, `WriteGuard`, `debounce`, poison recovery, concurrency e2e) | 1 585 | Force one process-wide instance; one writer; `read()` = `try_clone` | Connection pool + engine-level MVCC. Serializing writes stays as a *performance* choice, not a correctness one |
| `audit-ledger/src/mirror.rs` | 2 307 | fsync'd append-only mirror as **primary evidence**; preserve-and-refuse; torn-tail classifier; boot heal | Nothing. The mirror is a WAL for the WAL. Its tamper-evidence role is legitimate and would survive; its *durability* role would not |
| `audit-ledger/src/storage/mod.rs` | 1 259 | Ledger storage incl. the opener/trigger notes for `duckdb#23046` | Ordinary DAO |
| `audit-ledger/src/serve_tripwire.rs` | 147 | Panic in debug/test if any fresh `Ledger::open` / `Handle::open` happens in-serve | Nothing — a second connection is simply correct |
| `crates/aberp-snapshot` (crash-safe boot, `provision_atomic`, `atomic_install`, `EXPORT`-based logical snapshots, retention) | 2 204 | Corruption-proof snapshots because ART corruption is *internal* to the live file, so a byte copy copies it | `VACUUM INTO` / Online Backup API + `PRAGMA integrity_check`; or `pg_dump` / base backup |
| 6 cut gates + 6 probe suites + backstop (`tools/cut_gate_*.sh`, `*.awk`) | 3 712 | Freeze the opener census; detect write-forks and read-forks structurally | Not needed for durability. (The NAV-door and keychain gates are orthogonal and stay) |
| Frozen baselines (`adr0098_*`, `adr0099_*`) | 733 | 81 openers across 20 files; 33 read-forks, **14 live in-serve** | — |
| Coherence / tripwire / crash e2e tests (14 files) | 4 006 | Prove the Handle discipline holds on each migrated family | — |
| `adr/0099-prod-durability-hardening-lane.md` | 1 429 | The H1–H5 plan itself | — |
| **Total** | **≈ 17 400** | | |

Plus the costs that do not show up as lines of code:

- **Three of the fifteen CLAUDE.md rules** (13 "one Handle, all access", 14
  "all-or-nothing per subsystem", 15 "audit atomically") exist solely to keep
  humans and models from tripping this engine. Rule 14's "half-migrated is worse
  than unmigrated" is a startling thing to have to write down.
- **The migration is not finished, and its remainder is the risk.** The opener
  census has ratcheted 289/42 → **81/20**, which is real progress; but **14
  in-serve read-forks are frozen, not fixed**, including `write_one` in
  `aberp-mes::ledger_writer`, which *appends* through a fresh in-serve connection
  while the write-fork gate reports **ZERO**. `CHECK N1 ✓ 0 new` means "none
  added", never "fork-free".
- **The gates are themselves a defect surface** — incidents #5 and #6 were gate
  defects, and the July record includes four separate fail-open fixes
  (`d432939` char literals, `813a858` blind lexer / dead-scanner green,
  `d226827` `cfg(test)` lexer, `f8ff121` red-means-red).
- **The prod tripwire is inert.** `serve_tripwire` is `debug_assertions`-only.
  The strongest control we have does not run in production.
- **One-way storage upgrades** on a young format holding an eight-year statutory
  record.

Roughly **17 000 lines and a third of the project's standing rules** are a
compensation layer. That is the number Ervin should weigh.

---

## 3. Options

The workload the engine must serve, stated once so each option can be measured
against it: single node, single operator, one writer, 20 MB, point reads and
single-row writes, no analytics, **a legally-binding gap-free monotonic invoice
sequence filed to NAV**, an eight-year retention window, and a Tauri desktop app
the operator starts and kills at will.

### Option A — Stay on DuckDB, disciplined

Finish H3 (close the 14 in-serve forks), land H4 (the validated durable checkpoint
— `quiesce → EXPORT → atomic_install → reopen`), treat the fsync'd mirror as the
real source of truth, keep the census and gates ratcheting to zero, and add
whatever remains to keep the operator from killing the process mid-write.

**What breaks when discipline lapses** — and this is the whole case against A:

- **One** foreign `Connection::open` anywhere in the ~7-daemon `serve` process
  re-enables checkpointing on the shared instance and can fold the WAL. It need
  not be on an invoice path; #2 was a PDF renderer and #4 was an email attachment.
- The failure is **silent and intermittent**. The same DEV database emailed
  cleanly on 07-20 (seq 147 folded) and failed on 07-27 (seq 158 not). A passing
  manual test is not evidence the fork is gone.
- The failure is **loss, not corruption**, so it defeats in-DB constraints
  structurally. S444's sharpest lesson:
  `invoice_sequence_reservation` *does* carry `UNIQUE (series_id, fiscal_year,
  number)` and it could not help — **the tear deletes the very row that would
  conflict.** An in-database constraint cannot enforce an invariant against loss
  of the rows it constrains. Every durability guarantee must therefore be
  reconstructed in application code from the mirror, one invariant at a time, as
  S444 just did for one counter.
- **H5's auto-heal removed the stop that saved prod.** The boot reconciler replays
  the mirror tail into `audit_ledger` — and *only* into `audit_ledger`. Nothing
  rebuilds `invoice_sequence_state` or the `invoice` rows. So the DB knew in the
  ledger that 65 was reserved while the counter said 64. Heal-and-continue
  replaced the 2026-07-19 boot refusal without teaching the business tables to
  heal. Under A, every business table needs its own heal path.
- Discipline **has** lapsed, repeatedly, under maximum attention: four defects
  shipped through the gate in one month, and the gate's replacement was defeated
  by a code formatter within a day.

**Effort:** H4 is a substantial engineering step (a validated quiesce-and-fold
under a live server) plus 14 fork migrations plus per-table heal paths. Call it the
largest of the three options on a five-year horizon, because it never ends.
**Risk:** high and *unbounded* — the exposure is any future `Connection::open` by
any future contributor or model. **Machinery eliminated:** none; it grows.
**Fit for a NAV sequential ledger:** poor. The invariant is "never reuse a number",
the failure mode is "lose the record of the number", and the engine cannot be made
to guarantee the record survives a commit.

### Option B — SQLite in WAL mode

Embedded, zero-ops, single-file, same deployment story as DuckDB — and a
transactional engine whose durability is its entire design centre.

**What changes at the engine level:**

| Property | DuckDB (H3 posture) | SQLite (WAL, `synchronous=FULL`, `fullfsync=1`) |
|---|---|---|
| Commit durability | Deferred; committed rows sit in an un-checkpointed WAL indefinitely by design | fsync per commit; a returned `COMMIT` is on stable storage |
| Second connection, same process | Second *session* on a shared instance; its close can checkpoint/fold the WAL | Independent connection; sees every prior commit; close folds nothing |
| Second **process** | Separate instance; needs the F-E flock to fence | Native, correct, concurrent; WAL readers do not block the writer |
| Durability pragmas | **per-connection** — one foreign session defeats them | `synchronous` is per-connection but WAL framing/recovery is per-database; a foreign connection cannot un-durable a prior commit |
| Crash recovery | Checkpoint + WAL replay in a young storage layer; ART assertion class live | WAL replay on open; the canonical, most-tested crash-recovery path in the industry |
| Storage format stability | v1.x, one-way upgrades (`Cargo.toml:241`) | Format stable since 2004, committed to 2050; a Library-of-Congress recommended archival format |
| Verification pedigree | Standard OSS test suite | TH3 (100% MC/DC branch coverage), `dbsqlfuzz`, decades of adversarial crash testing |
| Analytics | Excellent — **and unused (§1.4)** | Adequate for 8 `GROUP BY` and 14 `SUM(` |

**The class of bug that consumed July becomes definitionally impossible.** "A
committed row vanished because another connection closed" has no expression in
SQLite's model. Incidents #1, #2, #4 could not occur. #3 and #7 lose their
enabling condition — their app-layer halves (the `.unwrap_or_default()` fail-open;
the counter floor living only in a business table) remain real bugs, and both are
already fixed.

**What the ADR-0019 app-layer-invariant design saves us.** This is the single
biggest reason B is cheap, and it is a design decision Ervin already made:

- Module code never imports DuckDB types; each module owns a **storage port trait
  in domain terms**, with a DuckDB adapter and an in-memory adapter. Swapping the
  engine means writing a second adapter behind traits that already exist.
- **No foreign keys, ever** (ADR-0019 §3). No cascade semantics, lock-ordering, or
  insert-ordering to port.
- **No engine-minted identity.** S410 deleted the last `CREATE SEQUENCE` /
  `nextval()` pair; IDs are app-minted prefixed ULIDs. Only 3 residual `nextval`
  mentions remain, none load-bearing.
- **No CHECK constraints.** S410 stripped 13 of them; every one was redundant with
  a Rust enum or validator, and each strip landed with a rejection test.
- **The SQL is portable ANSI.** Zero DuckDB-only constructs (§1.4). `VARCHAR`,
  `BLOB`, parameterized statements. `rusqlite` exposes the same `params!` macro
  name and a near-identical `Connection` / `query_row` / `execute_batch` surface,
  so most of the 449 `params!` sites and 120 `duckdb::Connection` signatures are a
  mechanical type-and-import swap.
- **An engine-swap seam already exists in code**: S410 step 4 introduced
  `StorageEngine` + `DuckDbEngine` + `const STORAGE_ENGINE` in the snapshot layer,
  moving `CHECKPOINT` behind `fold_wal` and `PRAGMA verify_external_invariants`
  behind `verify_integrity`.

In short: the invariants were deliberately kept out of the DDL for exactly this
day, and `[[no-sql-specific]]` is about to pay for itself.

**The honest costs of B:**

1. **`DECIMAL` is the one real semantic gap.** `DECIMAL(18,6)` (quantities) and
   `DECIMAL(18,0)` are declared in `modules/billing/src/adapters/duckdb_store.rs`
   and read back as strings whose trailing zeros the formatting layer already
   normalises (`invoice-pdf/src/format.rs`, `nav_xml.rs`). SQLite has no decimal
   type — a `DECIMAL` declaration gets NUMERIC affinity and can silently become an
   `f64`. **Money must never touch a float.** The fix is known and not exotic:
   money is already minor-unit integers (`read_invoice_total_gross_minor`), and
   quantities move to scaled integers (µ-units at scale 6) or `TEXT` with
   app-layer decimal arithmetic. But it is a real, careful, test-heavy change
   across the billing adapter, the PDF formatter, the NAV XML renderer, and the
   stock-movement routes. **This is the largest single line item in B and should
   be scoped before committing to it.**
2. **macOS fsync.** A plain `fsync` on macOS does not guarantee the write reached
   the platter. `PRAGMA fullfsync=1` (F_FULLFSYNC) is mandatory, not optional, and
   costs write latency we will not notice at this volume. It must be pinned by a
   test, not a comment.
3. **SQLite is also a single writer.** One write transaction at a time. The
   `Handle`'s write serialization therefore **stays** — but as a throughput
   choice, not as the thing standing between us and data loss. Roughly the
   `aberp-db` crate survives at a fraction of its current conceptual weight.
4. **WAL mode needs shared memory and a local filesystem.** `-shm`/`-wal`
   siblings; no network filesystems. Fine for a desktop app; would need revisiting
   for any exotic storage.
5. **Reporting.** `reports.rs` needs review for SQLite's narrower function set.
   Nothing there looks hard, but it is not zero.
6. **Two engines during the transition**, if we keep DuckDB for reporting. That is
   a real complexity cost and CLAUDE.md rule 7 says pick one — see §4 for how to
   avoid ending up with two permanently.

**What B retires outright:**

| Retired | LOC |
|---|---:|
| `aberp-snapshot` crash-safe/`EXPORT` machinery → `VACUUM INTO` + `integrity_check` (a fraction of the size, and simpler: single-file atomic, not a Parquet directory) | ~1 500 of 2 204 |
| Mirror's **durability** role (preserve-and-refuse, torn-tail classifier, boot heal, per-table heal paths). Its **tamper-evidence** role stays — it is the ADR-0008/0030 hash chain and it is independently valuable | ~1 200 of 2 307 |
| Read-fork + write-fork gates, probe suites, structural scanner, frozen baselines — a second connection stops being a defect | ~2 400 + 733 |
| `serve_tripwire` (the debug-only panic on a fresh open) | 147 |
| The 14 coherence e2e tests' *durability* assertions (the business assertions stay) | ~1 500 of 4 006 |
| **H4 entirely** — the validated durable checkpoint has no reason to exist | the whole deferred step |
| CLAUDE.md rules 13 and 14 collapse to ordinary connection hygiene; rule 15 (audit atomically) stays and gets *stronger* — it becomes a real ACID transaction | 2 of 15 rules |
| **Total retired** | **≈ 7 500–8 000** |

**Effort:** the billing/quantity decimal work (the real cost), a `rusqlite`
adapter per storage port, a forward migration that reads the DuckDB tenant DB and
writes the SQLite one **with a hash-chain re-verification gate on the other side**,
and a swap of the snapshot primitives. Weeks, not months, and — critically —
**incremental and reversible**, because the port traits let both adapters exist
simultaneously and be diffed against each other on real data.
**Risk:** moderate, front-loaded, and *bounded* — it is a migration with a
verifiable end state (`Ledger::verify_chain` genesis→head on the new engine, plus
row-count and total-sum reconciliation per table). Contrast A, whose risk is
unbounded in time.
**Fit for a NAV sequential ledger:** strong. Commit durability is exactly the
guarantee the invariant needs, and a 2050-committed archival format is exactly
what an eight-year statutory record wants.

### Option C — Postgres for the transactional core, DuckDB for analytics

Real server OLTP: MVCC, per-commit WAL durability with `synchronous_commit`, true
concurrent readers and writers, point-in-time recovery, mature `pg_dump` /
base-backup tooling, and constraints that are actually load-bearing because rows
do not vanish.

Technically this is the strongest option. The question is whether it is the right
option for **this** product, and the honest weighing turns on one thing: ABERP is
a **Tauri desktop ERP that one operator starts and kills**.

| Deployment shape | Assessment |
|---|---|
| **External service** (operator installs Postgres.app / Homebrew, or we require a running server) | Wrong for the product. It moves a database daemon, a data directory, a port, a role/auth setup, and a major-version upgrade path onto a machinist. `[[trust-code-not-operator]]` is a standing rule here. The 2026-07-19 recovery already needed a hand-written incident script; a Postgres data directory the operator can break, back up wrongly, or leave behind on a machine swap is a categorically worse recovery surface than one 20 MB file. |
| **Bundled/embedded child process** (`pg_embed`-style: ship the binaries, `initdb` on first run, supervise on a unix socket) | Feasible and honestly considered. But: +150–250 MB of platform binaries in a signed/notarized Tauri bundle; a supervised child process to start, health-check, and shut down cleanly on app quit — **process kills are one of our trigger classes, and this adds a second process to kill**; `initdb` and `pg_upgrade` on the operator's machine with no operator; and macOS codesigning of bundled executables, which memory already records as a source of instability (`[[feedback_cargo_codesign_destabilizes]]`). We would trade an engine problem for a lifecycle problem. |
| **Postgres for the cloud/SaaS lane only** | **Correct, and already decided.** ADR-0019 §1 names "Postgres-per-tenant for cloud later"; ADR-0059 recommends Postgres-per-tenant for the SaaS path. Nothing in this ADR should disturb that. |

**Effort:** everything B needs (the port adapters, the decimal work — Postgres has
real `NUMERIC`, so decimals get *easier* here than in B) **plus** the entire
bundling, provisioning, supervision, upgrade, and notarization story, plus a
second engine kept for the analytics we do not actually run (§1.4).
**Risk:** low on data integrity, meaningfully high on operations and packaging —
and the operational risk lands on a single non-technical operator with no DBA.
**Machinery eliminated:** the same ~8 000 lines as B, plus in-DB constraints
become genuinely usable again (rows do not vanish, so `UNIQUE (series_id,
fiscal_year, number)` would finally have teeth — a real and specific benefit for
the NAV invariant).
**Fit for a NAV sequential ledger:** excellent on integrity, poor on
single-operator desktop operability. **Keep it for the cloud lane, where its
weaknesses are irrelevant and its strengths are the point.**

### 3.1 Side by side

| | **A. DuckDB, disciplined** | **B. SQLite (WAL)** | **C. Postgres** |
|---|---|---|---|
| Commit durability | Deferred by design | ✅ Native, per-commit | ✅ Native, per-commit |
| July's failure class possible? | ✅ Yes, silently | ❌ No | ❌ No |
| In-DB constraints load-bearing? | ❌ No (loss defeats them) | ✅ Yes | ✅ Yes |
| Ops burden on operator | None | None | Significant (external) / moderate (bundled) |
| Bundle / build footprint | 0 | Reduced — the bundled libduckdb amalgamation is the workspace's heaviest native dep (it twice overflowed CI's disk and forced the `[profile.dev]` debuginfo trim, `Cargo.toml:402`); exact delta **unmeasured** | +150–250 MB of platform binaries |
| Migration effort | Ongoing, never ends | **Weeks, bounded** | Weeks + packaging programme |
| Durability machinery retired | 0 | ≈ 8 000 LOC | ≈ 8 000 LOC |
| Risk profile | Unbounded in time | Front-loaded, verifiable | Low data / high ops |
| Archival format for 8-year record | Young, one-way upgrades | ✅ Stable to 2050 | ✅ Stable, tooled |
| Analytics we actually use | Unused strength | Sufficient | Sufficient |
| Fit: NAV sequential ledger, single-node desktop | ❌ Poor | ✅ **Strong** | ⚠️ Right engine, wrong deployment |
| Fit: future multi-tenant SaaS | ❌ Poor | ⚠️ Adequate per-tenant | ✅ **Correct** |

---

## 4. Recommendation

**Adopt Option B: migrate the transactional system-of-record to SQLite in WAL
mode, and retire DuckDB from the transactional path entirely rather than keeping
it for reporting.**

The reasoning, shortest form first:

1. **We are paying for the wrong engine's weaknesses and using none of its
   strengths.** Zero window functions, zero recursive CTEs, 20 MB, one writer,
   point lookups. This is a textbook embedded-OLTP workload (§1.4). That fact
   alone decides it.
2. **The invariant we cannot afford to break is a durability invariant.** A
   gap-free monotonic invoice number filed to the Hungarian tax authority is a
   legal obligation, and S444 proved the failure mode is *loss of the record*,
   which no in-database constraint can defend against. The engine must guarantee
   that a committed row survives. SQLite's core promise is precisely that.
3. **Discipline has been tried at maximum effort and it leaked.** ~17 400 lines,
   three of fifteen standing rules, six gates, four fail-open gate fixes, and four
   defects still shipped through in one month — with the replacement gate defeated
   by `rustfmt` a day later. Option A's cost is not the work remaining; it is that
   the work never ends and the exposure is every future `Connection::open`.
4. **The migration is unusually cheap because Ervin already paid for it.**
   ADR-0019's port traits, no foreign keys, no engine-minted identity, no CHECK
   constraints, portable ANSI SQL, and S410's `StorageEngine` seam mean the
   invariants do not move. This is the specific, concrete return on
   `[[no-sql-specific]]`.
5. **Postgres is the right engine and the wrong deployment — for now.** It stays
   the plan for the cloud lane (ADR-0019 §1, ADR-0059), and the port seam makes
   SQLite → Postgres a second mechanical hop, not a rewrite. Choosing B does not
   spend the Postgres option; it defers it to where it belongs.
6. **Drop DuckDB rather than keep it for reporting** (CLAUDE.md rules 7 and 12).
   Eight `GROUP BY`s and fourteen `SUM(`s do not justify a second engine, a second
   set of connection rules, or a second corruption class. If a genuine analytical
   need appears later, DuckDB can read a SQLite file directly through its
   `sqlite_scanner` extension — analytics on demand, zero standing complexity.
   Keeping the transactional engine unambiguous is worth more than saving
   `reports.rs` a rewrite.

**Where I would push back on myself, and what would change the answer.** If the
`DECIMAL(18,6)` audit in Phase 0 comes back showing decimal quantities are woven
through the NAV XML and PDF paths more deeply than §3's Option-B cost line
assumes, B's cost could double, and Option A + a scoped H4 becomes arguable *as a
holding position* — but only as a holding position, because A's risk does not
decay. If ABERP's roadmap moves to multi-tenant SaaS within twelve months, skip
straight to C and treat this ADR's Phase 1–3 as the Postgres migration instead;
the seam and the phasing are identical, and doing SQLite first would be waste.

### 4.1 Phased path, if B is chosen

Every phase is closed by the CLAUDE.md rule-4 gates (fmt + build + test + clippy
`-D warnings` + the cut gates), lands on a gate-green base, and is independently
abandonable. Rule 14 applies unchanged and is the migration boundary: **migrate a
family's writers and readers together, never mid-family.** The **storage-port
trait is the seam**; both adapters coexist while the port is being crossed.

- **Phase 0 — scope and prove, no migration.** Audit every `DECIMAL` site and
  decide the target representation (scaled integer vs `TEXT` + app decimal).
  Prototype the `rusqlite` adapter behind **one** low-risk port (a leaf read-only
  family). Pin `journal_mode=WAL`, `synchronous=FULL`, `fullfsync=1`,
  `busy_timeout` in code with a **mutation-verified** test each — a durability
  pragma that no test can red is not configured. Write the forward migrator and
  the reconciliation gate (per-table row counts, per-money-column sums,
  `Ledger::verify_chain` genesis→head on the SQLite side). **Exit criterion: a
  real DEV tenant DB migrates and reconciles bit-for-bit on the ledger.** If it
  does not, stop here having spent little.
- **Phase 1 — the transactional core.** `audit-ledger` + `modules/billing` +
  invoice sequence allocation, together, as one fused family. This is where the
  entire value is: the ledger and the number allocator stop being tearable. Keep
  the fsync'd mirror as tamper-evidence, and keep S444's durable
  ledger-derived floor — belt and braces are cheap once the braces actually hold.
- **Phase 2 — the remaining families**, one at a time, in the rule-14 order the
  H3 migration already established. As each family crosses, its census entries and
  fork-gate baselines are **deleted**, not ratcheted — the gate's purpose retires
  with the family. The 14 frozen in-serve forks are closed by the migration rather
  than by hand.
- **Phase 3 — retire the machinery** (§2's "retired" column). Swap
  `aberp-snapshot`'s `EXPORT`/`atomic_install` for `VACUUM INTO` +
  `integrity_check`; retire the mirror's boot-heal/preserve-and-refuse arms while
  keeping the hash chain; delete the read/write-fork gates, `serve_tripwire`, and
  the H4 seam; simplify CLAUDE.md rules 13–14 to connection hygiene. **Do this
  deliberately and last** — the machinery is the safety net during the crossing,
  and rule 12 says delete it once, properly, rather than optimise it.
- **Phase 4 — reporting.** Port `reports.rs`. Remove the `duckdb` dependency from
  the workspace. If analytics ever justify it, reintroduce DuckDB read-only over
  the SQLite file via `sqlite_scanner`.

---

## 5. How this decision gates the deferred durability work (D1 / H4)

**This is the operative consequence of filing this ADR, and the reason it should
be decided before the next durability session opens.**

The D1 label has moved, so state it precisely. D1 was originally *"the read-fork
scanner name-lists read helpers; the general shape is what should be flagged — a
structural CHECK N rewrite"*, carried three times and **closed by PR #43**
(`a68635e`). What is actually open in that lineage today:

| Open item | Status | Gated by this ADR? |
|---|---|---|
| **H4** — the validated durable checkpoint (`quiesce → EXPORT → atomic_install → reopen`), seam stubbed at `aberp-db/src/lib.rs:564`, `checkpoint_enabled = false` | Deferred, unstarted | ✅ **Yes — do not build it under B or C.** H4 is DuckDB-checkpoint-specific by construction and has no counterpart on either target engine. It is the single largest piece of prospective throwaway work in the tree. |
| **The write-fork gate's name-list weakness** — the same hole PR #43 closed on the read side. `aberp-mes::ledger_writer::write_one` appends through a fresh in-serve connection while CHECK 10M reports **ZERO** | Open, untouched | ✅ **Yes for the detector, no for the defect.** Do not build a second structural scanner. **Do** close `write_one` by hand now — a forked *append* can fork the ledger under any engine, and it is a one-family fix. |
| **`Handle::open` absent from the ADR-0098 opener census** | Open | ✅ Yes. Under B the census retires per family; do not re-baseline it. |
| **The 14 frozen in-serve read-forks** | Frozen, not fixed | ⚠️ **Partly.** Their *durability* aspect is closed by the migration; do not hand-migrate them onto the Handle under B. But triage them once now for **app-layer fail-open** — D2a's second half (`.unwrap_or_default()` making a guard pass vacuously) is engine-independent and would survive any migration. |
| **The prod tripwire is `debug_assertions`-only** | Open | ✅ Yes. Do not invest in making it production-live; under B it has nothing to guard. |
| **S392 NAV pre-flight is dead** (0 `invoice.check_performed` in 225 entries; `Clear` and `Unavailable` both record nothing, so "0 entries" cannot distinguish a working probe from a dead one — and it is compiled out of production) | Open | ❌ **No — orthogonal, and it should be fixed regardless.** This is a NAV-transport defect, not a storage one. It is arguably the most under-weighted open item in the tree: it is the last line of defence against exactly the S444 class, and it does not work. |

**The instruction this ADR asks Ervin to confirm:** if the direction is B or C,
**freeze H4 and all DuckDB-specific detector work now.** Under A, H4 becomes the
top durability priority and should be scheduled immediately, because A's exposure
is live on the deployed line today. The one thing that would be wasteful under
every option is to keep doing both — building deeper DuckDB-specific machinery
while an engine decision is pending.

---

## 6. Consequences

**If B is accepted:**

- A bounded migration replaces an unbounded discipline burden; ~8 000 lines and
  two of fifteen standing rules come out.
- The class of defect that consumed July stops being expressible.
- In-database constraints become load-bearing again, so future invariants can be
  enforced where they belong instead of reconstructed in application code one at a
  time.
- The eight-year statutory record lands in a format with a public stability
  commitment past 2050.
- Short-term risk rises during the crossing: a migration touching the invoice and
  audit path is the highest-stakes change in the product. This is why Phase 0
  exits on a bit-for-bit ledger reconciliation and Phase 1 is fused, gated, and
  reversible.
- `[[no-sql-specific]]`, ADR-0019, and S410 are validated. Worth saying out loud:
  the cheap migration is a dividend on discipline paid years earlier.

**If A is accepted:** H4 must be scheduled immediately (the exposure is live), and
every business table carrying a durability invariant needs its own
mirror-derived heal path, as S444 built for one counter. §2's machinery is
permanent and will keep growing. This ADR should then be revisited on the next
tear.

**If C is accepted:** the packaging and supervision programme is the critical
path, not the data migration. Phases 0–2 are unchanged; Phase 3 additionally owns
`initdb`, supervision, clean shutdown on app quit, `pg_upgrade`, and notarized
bundling of platform binaries.

**Under every option:** F1 (§1.3) should be settled by measurement, `write_one`
should be closed by hand, the 14 frozen forks should be triaged for app-layer
fail-open, and the dead S392 pre-flight should be fixed. None of those depend on
the engine choice.

## 7. Alternatives considered and rejected

- **DuckDB for transactions + SQLite for the ledger only.** Two engines, two
  connection disciplines, and the invoice tables — which S444 proved are the
  tearable ones — stay on the weaker engine. The ledger is already the *durable*
  side via the mirror; the business tables are the gap. Rejected: it hardens the
  half that is already hardest.
- **Mirror-as-primary-source-of-truth, DB as cache.** Effectively already true for
  the audit ledger, and it is why the 2026-07-19 boot refusal worked. Extending it
  to every business table means writing a bespoke append-only durable log and
  replay path per table — reimplementing a transactional engine, badly, in
  application code. Rejected on rule 12: do not optimise a thing that should not
  exist.
- **Keep DuckDB and never kill the process.** Not enforceable on a desktop app a
  machinist operates, and #2/#4 show the fold does not need a kill — a normal
  connection close on a normal PDF render is sufficient.
- **Wait for DuckDB's storage layer to mature.** Plausible on a longer horizon and
  the project is moving fast, but it makes a legally-binding tax ledger's
  integrity contingent on someone else's roadmap, with one-way storage upgrades
  and no rollback. Rejected for this workload; DuckDB remains an excellent choice
  for the analytical workload we do not currently have.
