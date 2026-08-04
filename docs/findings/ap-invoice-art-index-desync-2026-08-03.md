# `ap_invoice` ART index desync — root cause + fix (prod incident 2026-08-03)

Companion to `~/Documents/Claude/Projects/ABERP-incident-20260803/INCIDENT_REPORT.md`
(triage + recovery). This file records what the **root-cause pass** measured and
what was changed. Every claim below was measured on a copy of the incident DB
(`aberp.duckdb`, sha256 `f59c1883…`); nothing here is inferred.

## 1. What the fault actually is

Marking an AP invoice paid killed the live instance:

```
FATAL Error: Invalid Input Error: Failed to delete all rows from index.
             Only deleted 0 out of 1 rows.
  duckdb::RowGroupCollection::RemoveFromIndexes → IndexDataRemover::Flush
  → CommitState::CommitDelete → UndoBuffer::RevertCommit
  → INTERNAL Error: Failed to append to PRIMARY_ap_invoice_0: duplicate key
```

**"0 out of 1" means the ART has NO entry for that row.** The fault is *missing*
index entries — not, as first read, a stale key→row_id mapping. The `PRIMARY
KEY` re-append in the trace is DuckDB's revert path reacting to the first
failure, not the first failure itself.

Bisected by dropping indexes on copies and re-running the fatal `DELETE`:

| dropped | result |
|---|---|
| nothing | FATAL |
| `ap_invoice_tenant_status_idx` only | FATAL |
| `ap_invoice_tenant_issue_idx` only | FATAL |
| **both secondaries** | **DELETE succeeds** |

So the poison is in the **two non-constraint `CREATE INDEX` ARTs**. The
constraint-backed ARTs (`PRIMARY KEY`, `UNIQUE`) are intact — a duplicate-PK
`INSERT` is still correctly rejected.

Affected rows: rowids 106–116 (10 rows, gap at 111), i.e. every `ap_invoice`
row appended after the last row that was checkpointed with a complete index.
Rowids 101–105 are absent entirely.

## 2. Where the rows went — the tear, not the heal

`audit_ledger` seq 8085 (2026-07-21T07:09:19Z) records
`db.auto_recovered {trigger: mirror_ahead_heal, source_snapshot_seq: 8059,
replayed_entries: 25}`. The DB head was 8059 (2026-07-20T06:48:59Z) while the
mirror was at 8084 — **26 durably-mirrored audit entries were missing from the
DB**, including the five `incoming_invoice_ingested` rows of 2026-07-20T06:49:30.
Those five AP invoices are precisely the missing rowids 101–105; the daemon
re-ingested them on 2026-07-21 at rowids 106–110 with fresh ULIDs.

That is the known, still-live **read-fork close-tear**: a foreign
`Connection::open` on the shared instance carries DuckDB's default pragmas, so
its *close* checkpoints and truncates the Handle's WAL. The heal is the
*recovery from* that event, not its cause — `heal_from_mirror_ahead`
(`crates/audit-ledger/src/mirror.rs`) replays into `audit_ledger` and touches no
other table, so it cannot by itself have poisoned `ap_invoice`'s indexes. The
correlation in the incident report is real but points at the tear.

The precise DuckDB internal that let the table's row groups persist without
their secondary-ART entries is upstream (`duckdb/duckdb#23046` family, still
open in 1.5.3 — the same class that made this tree drop every ART off
`audit_ledger`). Not chased further: it is not fixable here, and it does not
change the fix.

## 3. There is no non-destructive detector

Measured on the poisoned file, all three rolled-back probe shapes report
**CLEAN**:

| probe | result on a poisoned DB |
|---|---|
| `BEGIN; UPDATE … SET local_status=local_status; ROLLBACK` | OK (no detection) |
| `BEGIN; DELETE FROM ap_invoice WHERE id=…; ROLLBACK` | OK (no detection) |
| `BEGIN; DELETE FROM ap_invoice; ROLLBACK` | OK (no detection) |
| the same statements **committed** | FATAL, instance invalidated |

The fault fires in `CommitState::CommitDelete`, i.e. inside `Commit`; a rollback
never reaches it. A committed probe does detect it — by destroying the instance
it was meant to protect (in the CLI it surfaces as an uncaught C++ exception
that aborts the process).

Index-scan-vs-seq-scan comparison was also ruled out: at tenant scale DuckDB
never chooses an ART scan (`EXPLAIN` shows `SEQ_SCAN` for a 106-row table), so
the comparison is vacuous.

**Conclusion: this class cannot be detected non-destructively on DuckDB 1.5.3.**

## 4. The fix — repair unconditionally

`DROP INDEX` + re-`CREATE INDEX` **cures the real prod file**: the exact
statement that invalidated the instance
(`UPDATE ap_invoice SET local_status='Paid' WHERE id='apinv_01KYSF5RYMSYJZ6Q56QCVR8P5P'`)
then succeeds, zero rows lost. All 25 non-constraint indexes rebuilt on the
25 MB prod DB: **~40 ms**.

So `aberp_db::index_integrity::rebuild_secondary_indexes` rebuilds every index
`duckdb_indexes()` reports (the explicit `CREATE INDEX` set — constraint ARTs
are not listed and are not touched), using DuckDB's own round-tripped `sql` so
there is no hand-maintained registry to drift. `serve::run` calls it at boot,
after the mirror reconcile/heal and after every `ensure_schema`, on the existing
boot-phase connection — **no new DB opener, ADR-0098 Handle census unchanged** —
and before the shared Handle opens. A failure REFUSES boot.

Two DuckDB constraints the implementation obeys:

* statements run in **autocommit, one at a time** — `DROP INDEX` + `CREATE INDEX`
  inside an explicit `BEGIN`/`COMMIT` crashes DuckDB 1.5.x outright
  (*"Pure virtual function called!"*, measured);
* a half-done rebuild leaves at most a *missing* index — a plan regression,
  never a correctness one.

⚠ **R3 correction.** An earlier revision of this section claimed "the next
boot's `CREATE INDEX IF NOT EXISTS` restores it". **That is false for two of the
25 indexes.** Only tables whose `ensure_schema` is called inside `serve::run`
get that guarantee. `partners::ensure_schema` is **not** in the boot path — it
is called only on the demo DB (`serve.rs:632`) and from a route helper
(`serve.rs:10296`) — so a durable `DROP` of `partners_tenant_deleted_idx` /
`partners_tenant_display_idx` leaves them **absent until a partners route runs**.
Still a plan regression, not a correctness one, but not self-healing at boot.

### R2 — the repair is now audited

The rebuild emits a durable `db.indexes_rebuilt` audit row
(`{"indexes_rebuilt":N,"elapsed_ms":N}`) via
`rebuild_secondary_indexes_audited`, then re-syncs the mirror. Because there is
no detector (§3) and the repair is unconditional, this row is the **only**
durable signal that it ran: recurrence of the close-tear and boot-cost drift are
observable nowhere else. Deliberately a NEW `EventKind` and not
`db.auto_recovered` — that kind means "something went wrong", and firing it on
every boot would mask real recoveries. The append is best-effort (the repair has
already landed) and logs at `error` on failure.

### R1 — how long the poisoned window actually is

The poison is **persistent per table**: once an ART is missing entries, it stays
missing across restarts until something rebuilds it. This gate rebuilds at every
boot, so the exposure window is "from whenever the tear happens until the next
boot" — not "one request".

⚠ That window is **not** closed for the rest of a serve session, and the
generator is still live downstream of the gate: `apps/aberp/src/serve.rs:2389`
is an in-serve `duckdb::Connection::open` (the cad-blob key-provision audit),
which runs AFTER the repair and after the shared Handle opens. Its close carries
DuckDB's default pragmas and can therefore fold/truncate the Handle's WAL —
exactly the close-tear class that produced this incident. The repair makes the
consequence self-healing on the next boot; it does not remove the generator.

## 5. Snapshot validation — what changed, and what it does not claim

`validate_export` now records `secondary_index_count` from the re-imported
snapshot, and `take_snapshot` compares it against the live source; a mismatch
marks the snapshot invalid. Measured: source 25 == re-imported 25 on the
incident DB, so the gate cannot false-positive on a healthy export.

⚠ **The comparison must require BOTH counts to be real.** `-1` is the
"catalog unreadable" sentinel, not a count. The first revision of this arm
compared it like one, so an unreadable SOURCE catalog (`-1`) against a healthy
re-imported `25` scored as a mismatch → `valid: false` → **`restore_into`
refuses that snapshot → unrestorable**. Nothing else covered the source side:
every `validate_export` arm only ever sees the re-imported copy. The verdict now
lives in `index_inventory_verdict`, degrades an unreadable catalog to "not
compared", and is pinned both ways (a real shortfall invalidates; a sentinel on
either side never does). Both pins are mutation-verified: forcing the verdict to
`None`, or dropping the sentinel guard, reds the suite.

**Deliberate deviation, flagged.** The brief asked for the poisoned *source* to
make its snapshot `valid: false`. That is not implemented, for two reasons:

1. It is not detectable (§3), and `IMPORT DATABASE` rebuilds every index from
   parquet — which is exactly why `snap-37` said `valid: true` while its source
   was poisoned, and why restoring it CURED prod.
2. `restore_into` refuses to restore from a snapshot marked invalid. Marking
   snapshots of a poisoned source invalid would have blocked the only recovery
   that worked.

The poisoned-source case is instead closed structurally: serve boot repairs
unconditionally, so a snapshot taken by a booted process can never come from a
source poisoned *before* that boot. In-session poisoning after boot remains
theoretically reachable and is not covered — the next boot repairs it.

## 6. Pins

* `apps/aberp/tests/index_desync_incident_20260803.rs` — the distilled real
  incident DB (zstd; `*.duckdb` is gitignored, and a poisoned ART cannot be
  reconstructed from SQL). `fixture_is_still_poisoned` goes red if the fixture
  loses its teeth; `rebuild_secondary_indexes_cures_the_poisoned_database`
  proves the fix on the same bytes.
* `crates/aberp-db/src/index_integrity.rs` unit tests — constraint ARTs are
  never listed or dropped; the rebuild is idempotent.
* `crates/aberp-snapshot/tests/snapshot_tests.rs::snapshot_carries_and_records_the_secondary_index_inventory`.

## 6b. Known residuals (adversarial review, 2026-08-04)

| id | residual | status |
|---|---|---|
| **R5** | A standalone `CREATE UNIQUE INDEX` is ART-backed and just as poisonable, but `NOT is_unique` filters it out of the repair set — it would be silently unrepairable forever. | **Ratcheted.** `rebuild_secondary_indexes` counts standalone unique indexes BEFORE any drop and refuses loudly if there is one. Zero today; the first one added trips boot. Table-level `UNIQUE(...)` constraints are *not* standalone unique indexes and do not trip it (pinned both ways). |
| **R6** | `DROP INDEX "<name>"` is unqualified, so an index in a non-`main` schema would fail to drop. | **Known, fail-safe.** Unreachable today — every ABERP object lives in `main`. A non-`main` index would make the DROP error out and REFUSE boot, i.e. it fails loud rather than silently skipping the repair. Widen to schema-qualified if a second schema is ever introduced. |
| **R7** | The repair is bound to `serve` boot only. A CLI one-shot that writes the DB does not run it. | **Known, mitigated.** The whole-DB writer flock (ADR-0099 F-E) means a one-shot and `serve` cannot both hold the DB, and `aberp-ui` launches `serve` — so the normal desktop path always boots through the gate. A long-lived one-shot writer session remains uncovered until its next `serve` boot. |
| **R8** | `clippy --features production --all-targets -D warnings` is red on `apps/aberp/tests/serve_tenant_feature_guard.rs:16`. | **No action — not a cut blocker.** Pre-existing on `main` (that file is byte-identical to `origin/main`), and CI's clippy step runs default features with no production arm, so it is not on the cut path. |

## 7. NOT closed by this change

* The read-fork close-tear itself (28 forks, 12 live in-serve) is untouched —
  this fix makes its *index* consequence self-healing at boot, not its lost-write
  consequence.
* Recovering the currently-poisoned prod file still needs an operator action:
  either the snapshot rebuild in the incident report, or simply booting a build
  that carries this fix (which repairs on the way up). The latter is the cheaper
  path and should be confirmed against the real file before the cut.
