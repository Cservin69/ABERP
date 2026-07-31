#!/usr/bin/env bash
#
# rollback_to_duckdb.sh — ADR-0108 §6.2. Constraint C-IV: single-command,
# VERIFIED rollback from the SQLite migration back to DuckDB.
#
# It lands in Step 1, BEFORE any engine code exists, and every later step ends
# by running it. A rollback path exercised once at the end is a rollback path
# that has never been exercised.
#
# ---------------------------------------------------------------------------
# The two things that make this more than `rm aberp.sqlite`
# ---------------------------------------------------------------------------
#
# 1. `aberp.duckdb.wal` is PART OF THE DATABASE'S CONTENT (ADR-0108 B3, §2.5),
#    not a temp file. Restoring `aberp.duckdb` beside a foreign-generation
#    `.wal` does not FAIL the rollback — DuckDB replays it on the next open and
#    CORRUPTS it, and there is no second snapshot to go back to. So:
#      * main file and WAL are restored as an ATOMIC SET, or neither;
#      * NEVER the main file alone;
#      * NEVER the main file with the WAL merely deleted (a WAL holding
#        committed-but-unfolded transactions is committed data).
#    A `.wal` present now that the manifest did not record is MOVED aside,
#    never deleted — a deleted artefact cannot be post-mortemed (rule 11).
#
# 2. Before restoring anything it takes a SECOND snapshot of the current
#    on-disk state (`pre-restore/`). Step 5 below overwrites the only other
#    copy; without this, a restore that goes wrong is unrecoverable. It costs
#    one copy of a ~20 MB file.
#
# ---------------------------------------------------------------------------
# What it refuses (never forces, never waits)
# ---------------------------------------------------------------------------
#
#   * a DB under `~/.aberp/` — the migration is DEV-only (C-II);
#   * a live writer holding the tenant's whole-DB lock. ADR-0108 §6.2 step 2
#     reads "stop the DEV app; wait for the lock to clear"; this script does
#     NOT kill processes and does NOT wait. It refuses and names what to stop.
#     Killing an operator's running app from a rollback script is a bigger
#     hazard than the inconvenience of one manual step. DEVIATION, deliberate.
#   * any digest that does not match the manifest, at any point.
#
# Verification (step 7) delegates to `aberp verify-against-manifest`, which
# re-derives every number from the restored DB — including the two
# tamper-evidence counts (non-NULL `event_sig`, `audit_ledger_anchors`) that
# ADR-0108 B1 turns on, so a rollback cannot restore a signature-stripped
# ledger and report PASS.
#
# Usage:
#   run/rollback_to_duckdb.sh --db <path> [--tenant <t>] [--from <snapshot-dir>]
#                             [--no-build]
#
# Exit 0 = rolled back AND verified. Non-zero on anything unexpected.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DB=""
TENANT="test"
FROM=""
DO_BUILD=1

die() { printf '✗ %s\n' "$*" >&2; exit 1; }
note() { printf '  %s\n' "$*"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --db)      DB="${2:-}"; shift 2 ;;
    --tenant)  TENANT="${2:-}"; shift 2 ;;
    --from)    FROM="${2:-}"; shift 2 ;;
    --no-build) DO_BUILD=0; shift ;;
    -h|--help) sed -n '1,55p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[[ -n "$DB" ]] || die "--db is required (the DEV DuckDB path being rolled back to)"

# Absolutise so the guards below cannot be defeated by a relative path.
DB_DIR="$(cd "$(dirname "$DB")" 2>/dev/null && pwd)" || die "no such directory: $(dirname "$DB")"
DB_NAME="$(basename "$DB")"
DB="$DB_DIR/$DB_NAME"

echo "ADR-0108 rollback_to_duckdb — db: $DB  tenant: $TENANT"

# --- 1a. C-II: DEV only. ----------------------------------------------------
PROD_ROOT="$HOME/.aberp"
case "$DB/" in
  "$PROD_ROOT"/*) die "DEV-only violation (C-II): $DB is under $PROD_ROOT. This script never touches production." ;;
esac

# --- 1b. Refuse while a writer holds the tenant lock. ------------------------
# The lock is a flock on `<dir>/.aberp-db-writer.<tenant>.lock`. macOS ships no
# `flock(1)`, so the probe is "does any process hold this file open?" — which
# is exactly what a live holder looks like, and a crashed holder correctly
# reads as free (the file survives, the handle does not).
SANITIZED_TENANT="$(printf '%s' "$TENANT" | tr -c 'A-Za-z0-9._-' '_')"
LOCK="$DB_DIR/.aberp-db-writer.${SANITIZED_TENANT}.lock"
if [[ -e "$LOCK" ]]; then
  command -v lsof >/dev/null 2>&1 || die "lsof is unavailable, so the writer-lock check cannot run — refusing rather than guessing"
  if lsof -- "$LOCK" >/dev/null 2>&1; then
    die "a writer holds $LOCK — stop \`aberp serve\` (and any DB-mutating CLI) and re-run. This script never kills a process and never waits."
  fi
fi
note "✓ no live writer on tenant $TENANT"

TS="$(date -u +%Y%m%dT%H%M%SZ)"
ROLLBACK_DIR="$DB_DIR/.aberp-rolledback-$TS"
mkdir -p "$ROLLBACK_DIR/pre-restore" || die "cannot create $ROLLBACK_DIR"

# --- 3. Move the SQLite side aside. MOVE, never delete. ---------------------
SQLITE_BASE="${DB%.duckdb}.sqlite"
moved=0
for f in "$SQLITE_BASE" "$SQLITE_BASE-wal" "$SQLITE_BASE-shm" "$SQLITE_BASE.audit.log"; do
  if [[ -e "$f" ]]; then
    mv "$f" "$ROLLBACK_DIR/" || die "cannot move $f aside"
    note "moved aside: $(basename "$f")"
    moved=$((moved + 1))
  fi
done
note "✓ $moved SQLite artefact(s) preserved in $ROLLBACK_DIR"

# --- 4. The snapshot behind the snapshot. ------------------------------------
# Copy the CURRENT on-disk DuckDB state before step 5 overwrites it.
for f in "$DB" "$DB.wal" "$DB.audit.log" "$DB".audit.log.*.bak; do
  [[ -e "$f" ]] && { cp -p "$f" "$ROLLBACK_DIR/pre-restore/" || die "cannot pre-restore-copy $f"; }
done
note "✓ pre-restore copy taken: $ROLLBACK_DIR/pre-restore"

# --- 6 (moved earlier). Rebuild the default (DuckDB) binary. ----------------
# §6.2 orders the rebuild after the restore. It runs BEFORE it here for one
# reason: steps 5 and 7 are both delegated to that binary, so building later
# would mean discovering a broken build with the restore half-decided.
if [[ "$DO_BUILD" -eq 1 ]]; then
  ( cd "$ROOT" && cargo build -p aberp ) || die "cargo build (default features — no sqlite-engine) failed"
  note "✓ rebuilt the default DuckDB binary"
else
  note "· build skipped (--no-build)"
fi
BIN="${ABERP_ROLLBACK_BIN:-$ROOT/target/debug/aberp}"
[[ -x "$BIN" ]] || die "no aberp binary at $BIN — the restore and the verification both need it"

# --- 5. Restore, as an atomic set, only if asked or if the digest drifted. ---
# The digest comparison, the manifest parse, and the WAL-pairing decision all
# live in `aberp rollback-restore` (Rust). Shell orchestrates; it does not make
# the decision where being wrong corrupts the database (CLAUDE.md rule 5).
if [[ -n "$FROM" ]]; then
  SNAP="$FROM"
  note "! --from given — restoring the atomic set from $SNAP"
  "$BIN" rollback-restore --db "$DB" --from "$SNAP" --preserve-dir "$ROLLBACK_DIR" || die "restore failed"
else
  # No --from: use the newest pre-migration snapshot. The NORMAL case is that
  # the DuckDB file was never touched (C-I), so nothing is restored — and the
  # verification below still runs, because "we reasoned that we don't need to
  # restore" is not the same as "we checked".
  SNAP="$(ls -d "$DB_DIR"/.aberp-premigration-* 2>/dev/null | grep -v '\.partial$' | sort | tail -1)"
  [[ -n "$SNAP" ]] || die "no .aberp-premigration-* snapshot beside $DB and no --from given"
  if "$BIN" db-matches-manifest --db "$DB" --from "$SNAP" >/dev/null; then
    note "✓ $DB_NAME is byte-identical to the pre-migration snapshot (C-I held) — nothing to restore"
  else
    note "! $DB_NAME differs from the pre-migration snapshot — restoring the atomic set"
    "$BIN" rollback-restore --db "$DB" --from "$SNAP" --preserve-dir "$ROLLBACK_DIR" || die "restore failed"
  fi
fi
MANIFEST="$SNAP/manifest.json"

# --- 7. VERIFY. This is what makes it a *verified* rollback. -----------------
"$BIN" verify-against-manifest --db "$DB" --tenant "$TENANT" --manifest "$MANIFEST" \
  || die "verification FAILED against $MANIFEST — the rollback is NOT complete"

# --- 8. One line, and a non-zero exit on anything above. --------------------
echo "PASS rollback_to_duckdb — $DB verified against $MANIFEST; SQLite artefacts preserved in $ROLLBACK_DIR"
