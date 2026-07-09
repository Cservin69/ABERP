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
for req in "$SCAN" "$ALLOW"; do
  [[ -f "$req" ]] || { echo "✗ FAIL: required gate asset missing: $req"; exit 1; }
done

enforce="${ENFORCE_WRITE_FORK:-0}"
echo "ADR-0099 H3 write-fork gate — root: $ROOT  (mode: $([[ "$enforce" == "1" ]] && echo ENFORCING || echo informational))"

scope_files() { find apps/aberp/src modules crates -name '*.rs' | grep -vE '/tests/|/aberp-db/' | sort; }

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
