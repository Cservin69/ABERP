# ADR-0082 — Periodic, validated, logical DuckDB snapshot system (`aberp-snapshot`).

- **Status:** Accepted
- **Date:** 2026-06-15
- **Deciders:** Ervin (via S426 brief — top backlog item, auto-mode).
- **Supersedes:** none. **Extends** S393's manual file-copy panic button (`apps/aberp/src/snapshot.rs`): the file-copy CLI is retired in favour of this system (see §Decision → CLI).
- **Related:** ADR-0008 (audit ledger, the hash chain this validates), S341/S410 / `[[no-sql-specific]]` (no SQL-level invariants — the corruption class this whole ADR exists to survive), `[[trust-code-not-operator]]`, `[[hulye-biztos]]`, the 2026-06-11 ART-corruption incident (§Context).

## Context

On **2026-06-11** a DuckDB **ART (adaptive-radix-tree) index corruption** in the live prod database cost **~5 hours of hand-surgery** to recover. This is the same on-disk corruption family that S332/S341/S375/S393/S410 have been chasing: a secondary-index / checkpoint structure inside the *live data file* becomes internally inconsistent (`duckdb/duckdb#23046` and relatives). It recurs, and every recurrence is a manual, error-prone, unbounded-downtime event.

`[[trust-code-not-operator]]`: a safety property that depends on the operator remembering to take a backup before a risky operation **is not a safety property** — it is a hope. The 5-hour incident happened *because* there was no fresh, known-good, automatically-produced rollback point. The recovery property must live in code that runs on a timer, not in operator discipline.

**Why a file copy is the wrong mechanism.** S393 shipped a manual panic button that copies the live `*.duckdb` file (+ WAL), checkpoint-folds the copy, and validates it. That is useful for a *pre-upgrade quiescent* copy, but it has a fatal flaw for *corruption recovery*: **ART corruption is internal to the live file**. A byte-for-byte copy of a file whose index tree is already corrupt produces a corrupt snapshot. `verify_external_invariants` on the copy *may* catch it — but only after the corruption has already happened, and a torn-but-structurally-valid index can pass that pragma while being logically wrong. We must not build the recurring-incident defence on a mechanism that copies the very thing that breaks.

**The logical-export insight.** DuckDB's `EXPORT DATABASE 'dir' (FORMAT PARQUET)` walks the *logical* contents — every table's rows — and writes `schema.sql`, `load.sql`, and one Parquet file per table. The output is **independent of the source's physical index/checkpoint structure**: it is rebuilt from a table scan, not copied from the ART. A snapshot taken this way is corruption-free *by construction* even if the live ART is degrading, and `IMPORT DATABASE` rebuilds a pristine file with fresh indexes. This is the mechanism the recurring incident actually needs.

## Decision

**Build `crates/aberp-snapshot`: a periodic, retention-managed, self-validating logical-snapshot subsystem using `EXPORT DATABASE` / `IMPORT DATABASE`, wired into `aberp serve` as a background daemon, with a restore/list CLI, an operator "Snapshots" tab, and four new audit events.** The file-copy CLI from S393 is replaced.

### Snapshot mechanism (corruption-resilient by construction)

Each snapshot:
1. Opens the tenant DB (in-process when `serve` is running — DuckDB shares one instance across connections in a process, so no cross-process lock conflict; its own connection when run from the stopped-server CLI).
2. Runs `EXPORT DATABASE '<store>/snap-<seq>-<ts>.partial' (FORMAT PARQUET)` — a logical table-scan export, **not** a file copy.
3. **Validates immediately** (§Validation). A snapshot that fails validation is marked `valid=false` and the **newest valid** snapshot is always preserved.
4. On success, atomically renames `*.partial` → `snap-<seq>-<ts>/` and writes `meta.json` tagging the snapshot with a **monotonic seq**, **UTC timestamp**, **SHA-256 of the source DB file**, byte size, and the validation verdict.

The store is `~/Documents/ABERP-snapshots/<tenant>/` — **outside the repo** (never committed) and **outside `~/.aberp/`** (so a tenant reset or a restore never deletes the rollback copies). The seq is derived by scanning existing snapshot directory names; there is **no separate manifest file to drift** from the on-disk reality — the filesystem *is* the index (`[[hulye-biztos]]`: one source of truth, nothing to reconcile).

### Validation (built into every snapshot)

After export, validation does a full **`IMPORT DATABASE` into a throwaway temp DuckDB file** and runs a smoke set:
- `SELECT count(*) FROM invoice` — invoices survive the round-trip,
- `SELECT count(*) FROM audit_ledger` — audit rows survive,
- **`Ledger::verify_chain()`** — the tamper-evident hash chain (ADR-0008) re-verifies end-to-end against the tenant genesis.

If any check fails, the snapshot is `valid=false`, `EventKind::SnapshotValidationFailed` is emitted, and retention is forbidden from pruning the last valid snapshot. Validation is **in code, not operator inspection** (`[[trust-code-not-operator]]`).

### Retention (pure, unit-tested math)

`plan_retention(records, policy, now)` is a pure function (the heavily-tested core). Default policy:
- keep the **last N = 24** snapshots (4 days at the 4-hour default cadence),
- keep one **daily** (newest of each UTC day) for **30 days**,
- keep one **weekly** (newest of each ISO week) for **1 year**.
A snapshot is retained if it is in *any* keep-set; the **newest valid** snapshot is *always* retained regardless of windows. Everything else is pruned, emitting `EventKind::SnapshotPruned`. All four numbers are configurable.

### Restore — safety lives in the binary (`[[trust-code-not-operator]]`)

`aberp snapshot restore <seq-or-timestamp> --to <path> --confirm` rebuilds a DB from a snapshot via `IMPORT DATABASE`. It **refuses to start** the restore unless **both**:
- `--confirm` is passed, **and**
- the `--to` target is **not** under `~/.aberp/` (the live tenant homes, including `~/.aberp/prod/aberp.duckdb`).

The refusal is a pure, tested guard (`ensure_restore_allowed`) executed before any IMPORT — a fat-fingered restore can never clobber the live prod DB. Recovering prod is then a deliberate two-step: restore to a side path, stop serve, swap the file in. This is intentional friction on the one irreversible operation.

### CLI (`aberp snapshot {now,list,restore}`)

S393's flat `aberp snapshot` (file-copy take) and `aberp restore-snapshot` are **replaced** by a subcommand group:
- `aberp snapshot now` — take one managed logical snapshot immediately,
- `aberp snapshot list` — seq / timestamp / size / validation status / age,
- `aberp snapshot restore <seq|ts> --to <path> --confirm` — guarded restore.

The reusable S393 primitives (read-only integrity verify, lock-conflict detection) are carried over; the file-copy `take`/`restore` functions and their tests are deleted (`[[no-sql-specific]]` / CLAUDE.md #13 — one snapshot system, not two).

### Daemon (must not block boot)

`serve` spawns a supervised `tokio` interval loop (default 4h) under the existing shutdown coordinator. If the snapshot dir is missing it is created on demand; if creation fails the daemon **logs, emits, and continues** — a snapshot problem never blocks prod startup. Each cycle: take → validate → retention-prune, each step emitting its audit event.

### Audit events

Four new `EventKind` variants under the `snapshot.*` namespace, JSON payloads (matching the ledger's `serde_json::to_vec` convention, **not** CBOR): `SnapshotCreated`, `SnapshotValidationFailed`, `SnapshotRestored`, `SnapshotPruned`. Because they flow through `audit_ledger`, they surface automatically in the S424 audit-events screen as well as the new Snapshots tab.

### No SQL-level invariants (`[[no-sql-specific]]`)

Nothing in this system adds a SQL `CHECK`, trigger, or secondary index — those are the corruption surface S341/S410 spent sessions removing. Seq monotonicity, retention, and restore-target safety are all enforced in Rust.

## Consequences

**Good.** The 5-hour recurring incident gets a code-enforced, validated, ≤4-hour-old rollback point that is corruption-free by construction. Restore is a one-liner that *cannot* hit prod by accident. Operators get a list + manual-snapshot UI; every operation is in the tamper-evident audit log.

**Costs / trade-offs.** Logical export is slower and larger than a file copy (full table scan + Parquet encode) — acceptable at tenant scale, on a 4-hour cadence. Validation doubles the work (export + a full import) — deliberate: an unvalidated snapshot is worse than none. The snapshot store grows under retention but is bounded by the policy.

**Replaced.** S393's file-copy `aberp snapshot` / `aberp restore-snapshot` commands are gone; any operator muscle-memory or script using them must move to `aberp snapshot now` / `aberp snapshot restore`. This is flagged in the cut notes.

**Not in scope.** Cloud/S3 offsite sync, encryption-at-rest, storefront DR, incremental/differential snapshots. The store is local-disk only; offsite replication is a follow-up.
