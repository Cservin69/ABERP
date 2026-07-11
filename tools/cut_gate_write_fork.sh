#!/usr/bin/env bash
#
# cut_gate_write_fork.sh — ADR-0099 H3 zero-residual AUDIT-LEDGER WRITE-FORK gate.
#
# The corrected fork model (editions CHECK 10M). The seq-369/416/428/515 Defense
# forks were NOT a narrow "opener + rogue sync_mirror" class — the TRUE fork
# primitive is ANY independent live-DB opener (Connection::open / Ledger::open /
# append_reopen / DuckDbBillingStore::open) that then APPENDS to the audit ledger,
# inside the `aberp serve` process, OUTSIDE the ONE shared aberp_db::Handle. Two
# such openers off the same head both self-assign the next seq and fork the
# ledger. tools/adr0099_write_fork_scan.awk detects that primitive per-fn
# (comment/string/cfg(test)-aware).
#
# ADR-0099 Addendum 3 (this session): the scanner also detects the SPLIT fork —
# an opener whose owned Connection is MOVED to an append in another fn (the
# invoice store-shape `DuckDbBillingStore::open(_).into_connection()`, and its
# transitive helper-chain variants). Those forks (issue_invoice / issue_storno /
# issue_modification / submit_invoice / poll_ack / mark_invoice_paid) were
# UNCOUNTED under the old per-fn model; teeth proven by cut_gate_write_fork_probes.sh.
#
# The prod H3 gate is ZERO such forks outside tools/adr0099_write_fork_allowlist.txt
# (the append_reopen primitive, separate-process CLI one-shots fenced by the F-E
# flock, and pre-serve boot openers). NO frozen-residual ledger, NO deferrals —
# prod diverges from the editions shrinking-residual (locked plan §"Prod divergence").
#
# MODE:
#   default              — INFORMATIONAL: print the remaining in-serve write-forks
#                          (the H3 migration worklist) + count, exit 0. Used while
#                          the opener migration is in progress so the branch stays
#                          green and the remainder is visible in CI.
#   ENFORCE_WRITE_FORK=1  — ENFORCING: exit non-zero if ANY non-allow-listed fork
#                          remains. Flip this ON in ci.yml + cut-gate.yml the moment
#                          the migration reaches zero (that is the H3 acceptance cut).
#
# Seam-excluded: crates/aberp-db (the shared Handle) and */tests/*.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SCAN="tools/adr0099_write_fork_scan.awk"
ALLOW="tools/adr0099_write_fork_allowlist.txt"
OPENSCAN="tools/adr0098_opener_scan.awk"   # ALL runtime openers (cfg(test)-aware)
for req in "$SCAN" "$ALLOW" "$OPENSCAN"; do
  [[ -f "$req" ]] || { echo "✗ FAIL: required gate asset missing: $req"; exit 1; }
done

enforce="${ENFORCE_WRITE_FORK:-0}"
echo "ADR-0099 H3 write-fork gate — root: $ROOT  (mode: $([[ "$enforce" == "1" ]] && echo ENFORCING || echo informational))"

scope_files() { find apps/aberp/src modules crates -name '*.rs' | grep -vE '/tests/|/aberp-db/' | sort; }

# ── CHECK M — no half-migrated subsystem (the all-or-nothing rule, ALWAYS ENFORCED) ──
# With the runtime checkpoint DISABLED in H3, the shared Handle holds a persistent
# WAL-resident connection. A per-subsystem file that ROUTES access through the
# Handle (`.db.write()` / `.db.read()`) MUST NOT also retain a SEPARATE runtime
# live-DB opener: the Handle's writes sit in the WAL while the separate open
# reads / close-folds the main file, so a fresh reader sees a TORN ledger — a live
# correctness bug (the `entry_already_delivered` duplicate-delivery class), not a
# style nit. A half-migrated subsystem is strictly WORSE than an unmigrated one.
# serve.rs is EXEMPT: it is the router, migrated handler-by-handler; each migrated
# handler runs its whole flow on ONE guard (self-contained, no interleave), which
# the route tests exercise directly.
echo "[CHECK M] no half-migrated subsystem — a Handle-using file keeps NO separate runtime opener (ENFORCED)"
mixed=0
while IFS= read -r f; do
  [[ "$f" == "apps/aberp/src/serve.rs" ]] && continue
  grep -qE '\.db\.(write|read)[[:space:]]*\(' "$f" || continue   # routes through the Handle?
  op="$(awk -f "$OPENSCAN" "$f" 2>/dev/null)"                    # residual runtime openers?
  [[ -z "$op" ]] && continue
  echo "  ✗ HALF-MIGRATED: $f uses the shared Handle AND retains a separate runtime opener —"
  echo "$op" | sed 's/^/        /'
  mixed=$((mixed+1))
done < <(scope_files)
if [[ "$mixed" -ne 0 ]]; then
  echo
  echo "WRITE-FORK GATE: ✗ FAILED — $mixed half-migrated subsystem(s). Route ALL of each"
  echo "subsystem's DB access (READS included: db.read()) through the shared Handle."
  exit 1
fi
echo "  ✓ no half-migrated subsystem — every Handle-using file (bar the serve.rs router) is fully single-instance"
echo

# Build the allow-listed set of "<file>:<fname>".
allow_set="$(grep -vE '^\s*#' "$ALLOW" | sed '/^\s*$/d' | sort -u)"
is_allowed() { grep -qxF "$1" <<< "$allow_set"; }

remaining=0
worklist="$(mktemp)"
while IFS= read -r f; do
  while IFS= read -r rec; do
    [[ -z "$rec" ]] && continue
    # rec = "<line>:<fname>:opener@Lx+append@Ly"
    fname="$(cut -d: -f2 <<< "$rec")"
    key="$f:$fname"
    if is_allowed "$key"; then continue; fi
    printf '%s:%s\n' "$f" "$rec" >> "$worklist"
    remaining=$((remaining + 1))
  done < <(awk -f "$SCAN" "$f" 2>/dev/null)
done < <(scope_files)

if [[ "$remaining" -eq 0 ]]; then
  echo "✓ ZERO non-allow-listed in-serve audit write-forks — every runtime audit writer is on the shared Handle."
  rm -f "$worklist"
  exit 0
fi

echo "  $remaining non-allow-listed in-serve write-fork(s) remain (the H3 migration worklist):"
sort "$worklist" | sed 's/^/    /'
rm -f "$worklist"
echo
if [[ "$enforce" == "1" ]]; then
  echo "WRITE-FORK GATE: ✗ FAILED — $remaining fork(s) must route through the shared aberp_db::Handle (db.write() + append_in_tx)."
  exit 1
fi
echo "WRITE-FORK GATE: (informational) — $remaining fork(s) still to migrate; gate flips to ENFORCING (ENFORCE_WRITE_FORK=1) at zero."
exit 0
