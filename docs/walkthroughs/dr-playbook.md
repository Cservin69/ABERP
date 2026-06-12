# ABERP Disaster-Recovery Playbook — "ABERP won't boot"

Operator-facing, step-by-step guide for when the ABERP production
instance fails to start. Written after **INV-0** (2026-06-11), the
5-hour hand-surgery DB recovery that should have been a 30-second
snapshot restore.

The pinned lesson from INV-0: **snapshot-restore is Step 1, not the
last resort.** Hand-surgery preserves the in-place audit chain, but it
costs hours and is error-prone. A snapshot restore is `cp` + restart.
Reach for hand-surgery only when no usable snapshot exists OR the data
written *after* the snapshot is too critical to lose.

---

## Decision tree (read this first)

```
1. Boot failure?
   → Section 2 (snapshot restore) ← TRY THIS FIRST
   → If snapshot unavailable or post-snapshot data is critical: Section 3 + Section 4
2. Snapshot restored cleanly?
   → Section 5 (no compliance gap if snapshot was valid)
3. Hand-surgery was required?
   → Section 5 (compliance gap memo required)
```

Paths used throughout:

| What | Path |
|------|------|
| Live prod DB | `~/.aberp/prod/aberp.duckdb` |
| Live audit log | `~/.aberp/prod/aberp.duckdb.audit.log` |
| Release binary | `/Users/aben/ABERP/target/release/aberp` |
| Prod launcher | `./run/run_prod.sh` |

---

## Section 1: Symptoms — recognize the failure mode

*What this section accomplishes: lets you name the failure from what's
on screen, so you jump straight to the right next step.*

| Symptom | What it usually means | Next step |
|---------|----------------------|-----------|
| 🔴 Red SPA screen, **"healthz unreachable"** | Backend crashed mid-operation; the UI shell is up but the `aberp serve` subprocess is gone | Section 2 (restore), then Section 3 if you need the cause |
| 🔴 **"Backend boot failed: spawn aberp serve subprocess: read handshake line from aberp serve stdout: aberp serve stdout closed before the handshake line appeared"** | The subprocess died before it printed its READY handshake — it crashed during early boot | Section 3 (run `aberp serve` directly to see the real error), but try Section 2 first |
| 🔴 **bus error** in stdout/stderr when running `aberp serve` directly | A memory-mapped file read hit a corrupt region on disk — classic DuckDB page corruption | Section 2 (restore). The on-disk DB is damaged; do **not** keep restarting into it |
| 🔴 **DuckDB ART assertion** — `Assertion failed: ... art_operator.hpp` | ART (adaptive-radix-tree) index corruption, the known DuckDB 1.5.3 upstream issue | Section 2 (restore). Section 4 only if the snapshot is also affected |
| 🔴 **EROFS / Permission denied** | Filesystem permission or read-only mount problem — **not** data corruption | Fix permissions/mount first (this playbook is for corruption; a perms issue is usually `chmod`/remount, no restore needed) |

⚠️ A **bus error** or **ART assertion** means the bytes on disk are
bad. Restarting into the same file will reproduce the crash every time.
Stop restarting and go to Section 2.

---

## Section 2: Step 1 — Restore from snapshot (PREFERRED) ✅

*What this section accomplishes: replaces the corrupt prod DB with the
most recent good snapshot and gets you back to READY in seconds — the
INV-0 lesson made concrete.*

This is **Step 1**, deliberately. Ervin keeps point-in-time snapshots;
restoring one is faster and safer than any hand-surgery. The only cost
is losing business operations written *between* the snapshot and the
failure — usually a small, known window.

### 2a. Find the snapshot directory

**Terminal.** Snapshots live outside `~/.aberp/`. Check the likely
locations:

```
ls -la ~/Documents/ABERP-snapshots/ 2>/dev/null
ls -la ~/Library/CloudStorage/ 2>/dev/null
ls -la ~/.aberp-snapshots/ 2>/dev/null
```

Expected: one of these lists `aberp.duckdb` snapshot files (often
timestamped, e.g. `aberp.duckdb.2026-06-10T14-00`). Set `SNAP_DIR` to
whichever directory has them, e.g.:

```
SNAP_DIR=~/Documents/ABERP-snapshots
```

### 2b. List recent snapshots and pick one

**Terminal.**

```
ls -la "$SNAP_DIR" | head -20
```

Expected output: a reverse-chronological-ish listing of snapshot files
with sizes and timestamps. **Pick the most recent snapshot taken
BEFORE the corruption event** — typically the one just before today's
date, or yesterday afternoon's if the failure happened this morning. A
snapshot a few hours stale is almost always the right call.

### 2c. Kill any running ABERP

**Terminal.**

```
killall -9 aberp aberp-ui 2>/dev/null
```

Expected: no output (or `No matching processes` — both are fine). This
guarantees nothing holds the DB file open while you replace it.

### 2d. Back up the corrupt DB — do **not** delete it

**Terminal.** The corrupt file is forensic evidence (it pins *when* the
DuckDB corruption appeared) and may still hold post-snapshot rows you
need to splice back later.

```
ts=$(date +%Y%m%d-%H%M%S)
mkdir -p ~/.aberp-backups
cp -a ~/.aberp/prod/aberp.duckdb ~/.aberp-backups/aberp.duckdb.before-restore-$ts
cp -a ~/.aberp/prod/aberp.duckdb.audit.log ~/.aberp-backups/aberp.duckdb.audit.log.before-restore-$ts 2>/dev/null || true
```

Expected: no output. Confirm the copies exist:

```
ls -la ~/.aberp-backups/ | grep "$ts"
```

Expected: two `before-restore-$ts` files listed.

### 2e. Restore from the snapshot

**Terminal.** Replace `<snapshot-file>` with the file you picked in 2b.

```
cp -a "$SNAP_DIR"/<snapshot-file> ~/.aberp/prod/aberp.duckdb
cp -a "$SNAP_DIR"/<snapshot-file>.audit.log ~/.aberp/prod/aberp.duckdb.audit.log 2>/dev/null || true
```

Expected: no output. (If your snapshots bundle the audit log under a
different name, copy it to `~/.aberp/prod/aberp.duckdb.audit.log`
explicitly.)

### 2f. Restart

**Terminal.**

```
./run/run_prod.sh
```

Expected: the red PRODUCTION BUILD banner, then the backend reaches
**READY**, then the SPA renders normally. Sign in and spot-check that
invoices and the audit ledger look intact at the snapshot's point in
time.

⚠️ Any business operations BETWEEN the snapshot and the failure are
lost — that's the deliberate trade-off versus hand-surgery. If that
window contains data you cannot lose, **stop** and go to Section 3 +
Section 4 instead of accepting the restore.

---

## Section 3: Step 2 — Diagnostic (if snapshot unavailable or insufficient)

*What this section accomplishes: surfaces the real crash by running the
backend directly, bypassing the launcher's handshake check that hides
the underlying error.*

**Terminal.** The launcher only reports "handshake line never appeared"
— useless for diagnosis. Run `aberp serve` directly to see the actual
crash:

```
killall aberp aberp-ui 2>/dev/null
/Users/aben/ABERP/target/release/aberp serve --tenant prod 2>&1 | tee /tmp/aberp-crash.log
```

Expected: the backend either boots (prints its startup log and waits)
or **crashes with a concrete error** — a `bus error`, a DuckDB
`Assertion failed: ... art_operator.hpp`, an `EROFS`, etc. — captured
to both your screen and `/tmp/aberp-crash.log`.

Then:

1. **Identify the failure mode** by matching the output against the
   table in Section 1, and follow that row's next step.
2. **Capture the boot step immediately preceding the crash** — the last
   log line before the error names the subsystem (e.g. audit-ledger
   open, invoice-sequence load, catalogue migration). That line tells
   you which table is corrupt and feeds the surgical recovery in
   Section 4.

Keep `/tmp/aberp-crash.log` — you'll attach it to the gap memo in
Section 5.

---

## Section 4: Step 3 — Surgical recovery (LAST RESORT)

*What this section accomplishes: points you at the hand-surgery path —
the 5-hour INV-0 procedure — only when no snapshot can save you.*

This is what Dispatch + Ervin spent **5 hours** on during **INV-0**
(2026-06-11): hand-surgery on the corrupt DuckDB using the `duckdb` CLI
`EXPORT`/`IMPORT` flow plus selective audit-ledger splicing to preserve
the hash chain and sequence continuity.

The full procedure is **situational** — it changes with each corruption
pattern — so it is **not** reproduced here. Read the INV-0 incident memo
as the case study:

> `~/Library/Application Support/Claude/local-agent-mode-sessions/.../memory/project_aberp_snapshot_system_needed.md`

High-level shape of the DuckDB CLI recovery (adapt per incident):

- `EXPORT DATABASE '/tmp/aberp-export'` against the damaged DB to dump
  everything still readable.
- Inspect `/tmp/aberp-export` — a **0-byte CSV** marks the corrupt
  table. That's the one you'll have to reconstruct or accept loss on.
- Boot a fresh empty DB, then selectively
  `COPY <table> FROM '/tmp/aberp-export/<table>.csv' (HEADER, DELIMITER ',')`
  for every table that exported cleanly.
- Pay special attention to **`invoice_sequence_state`** — invoice
  numbering must stay strictly continuous (NAV requirement); never let
  the sequence reset or skip.

⚠️ Hand-surgery that touches `audit_ledger` produces a compliance gap.
Go to Section 5 the moment you splice or rebuild any ledger row.

---

## Section 5: Compliance gap considerations

*What this section accomplishes: tells you when a recovery created an
auditable compliance gap and how to record it.*

The audit chain is ABERP's **Part-11 / defense-grade compliance
posture** (`[[aberp-defense-aerospace-pivot]]`). Customer-auditors will
inspect it. Two very different cases:

- 🟢 **Snapshot restore (Section 2):** the snapshot's hash chain was
  authoritative *at snapshot time*, so restoring it is **not** a
  compliance gap. The chain is intact, just rewound. No memo required —
  though note the lost-window in your ops log.
- 🔴 **Audit ledger rebuilt or spliced (Section 4):** if any ledger row
  was reconstructed from CSV or hand-spliced, the hash chain has a
  discontinuity an auditor will see. **This is a real compliance gap.**
  Document it:

  **Terminal / editor.** Create `docs/findings/dr-gap-<date>.md`
  (e.g. `docs/findings/dr-gap-2026-06-11.md`) recording:

  - the failure mode and timestamp (from `/tmp/aberp-crash.log`),
  - which tables/rows were rebuilt vs restored,
  - the audit-ledger range affected and how continuity was re-established,
  - the invoice-sequence state before and after,
  - the snapshot that *would* have avoided the gap, if one existed.

✅ Rule of thumb: **rebuilding the chain from a valid snapshot is fine;
creating it from nothing is the gap.** When in doubt, write the memo —
an over-documented recovery never hurt an audit.

---

## Appendix: why snapshot-first

INV-0 took 5 hours because the recovery tried to preserve the in-place
audit chain via hand-surgery instead of restoring a snapshot that was
already sitting on disk. The invoices were intact in yesterday's
snapshot; only pilot quotes would have been lost. Restoring would have
been `cp` + `./run/run_prod.sh` — seconds.

Until the upstream DuckDB 1.5.3 ART corruption is fixed, this failure
class will recur. **Snapshot restore is Step 1. Always offer it first.**
