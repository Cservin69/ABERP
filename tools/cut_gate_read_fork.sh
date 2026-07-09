#!/usr/bin/env bash
#
# cut_gate_read_fork.sh — ADR-0099 H3 audit-ledger READ-FORK gate (CHECK N).
#
# The DUAL of the write-fork gate (CHECK 10M). That gate scans for an independent
# opener that APPENDS; it is STRUCTURALLY BLIND to an independent opener that
# READS. Once ANY audit writer is on the shared aberp_db::Handle (waves 1-2e
# already are) the Handle's audit rows are WAL-resident (checkpoint disabled in
# H3), so a FRESH `Ledger::open` / `Connection::open` reader sees only the folded
# SUBSET on the main file — a silent torn read. Proved in wave-2e: machine_crud's
# fresh read-back saw 1 of 3 events; the Handle read saw all 3. A gate that cannot
# see a bug class does not protect against it — this closes that gap.
#
# MODE:
#   default             — INFORMATIONAL: print the in-serve read-forks (the CHECK N
#                         migration worklist) + count, exit 0.
#   ENFORCE_READ_FORK=1 — ENFORCING: exit non-zero if ANY non-allow-listed in-serve
#                         read-fork remains. Flip ON when the worklist hits zero.
#
# Scope: apps/aberp/src + modules + crates, minus */tests/* and /aberp-db/.
# Allow-listed (tools/adr0099_read_fork_allowlist.txt): SEPARATE-PROCESS CLI
# one-shots only (no live Handle; flock-fenced) — their fresh reads are coherent.
#
# STATIC LIMITATION (see ADR-0099 §CHECK N): the allow-list encodes a runtime-
# reachability assumption (a fn's process). Four DUAL-CONTEXT fns (issue_storno,
# issue_modification, poll_ack, submit_invoice) run in BOTH serve and CLI — the
# same fn is coherent in CLI but hazardous in-serve; they are (correctly) NOT
# allow-listed, so they land in the worklist. The RUNTIME TRIPWIRE
# (SERVE_HANDLE_LIVE, proposed) is the backstop static scoping cannot provide.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SCAN="tools/adr0099_read_fork_scan.awk"
ALLOW="tools/adr0099_read_fork_allowlist.txt"
for req in "$SCAN" "$ALLOW"; do
  [[ -f "$req" ]] || { echo "✗ FAIL: required gate asset missing: $req"; exit 1; }
done

enforce="${ENFORCE_READ_FORK:-0}"
echo "ADR-0099 H3 read-fork gate (CHECK N) — root: $ROOT  (mode: $([[ "$enforce" == "1" ]] && echo ENFORCING || echo informational))"

scope_files() { find apps/aberp/src modules crates -name '*.rs' | grep -vE '/tests/|/aberp-db/' | sort; }
allow_set="$(grep -vE '^\s*#' "$ALLOW" | sed '/^\s*$/d' | sort -u)"
is_allowed() { grep -qxF "$1" <<< "$allow_set"; }

remaining=0
worklist="$(mktemp)"; allowed_hits="$(mktemp)"
while IFS= read -r f; do
  while IFS= read -r rec; do
    [[ -z "$rec" ]] && continue
    fname="$(cut -d: -f2 <<< "$rec")"
    key="$f:$fname"
    if is_allowed "$key"; then printf '%s:%s\n' "$f" "$rec" >> "$allowed_hits"; continue; fi
    printf '%s:%s\n' "$f" "$rec" >> "$worklist"
    remaining=$((remaining + 1))
  done < <(awk -f "$SCAN" "$f" 2>/dev/null)
done < <(scope_files)

na="$(wc -l < "$allowed_hits" | tr -d ' ')"
echo "  ($na CLI one-shot read(s) allow-listed as coherent — separate process, flock-fenced.)"

if [[ "$remaining" -eq 0 ]]; then
  echo "✓ ZERO non-allow-listed in-serve audit read-forks — every in-serve audit reader is on the shared Handle."
  rm -f "$worklist" "$allowed_hits"
  exit 0
fi

echo "  $remaining in-serve audit read-fork(s) remain (the CHECK N worklist — read via the Handle):"
sort "$worklist" | sed 's/^/    /'
rm -f "$worklist" "$allowed_hits"
echo
if [[ "$enforce" == "1" ]]; then
  echo "READ-FORK GATE: ✗ FAILED — $remaining in-serve reader(s) must read the audit ledger through the shared Handle"
  echo "  (db.read()/db.write() → try_clone → Ledger::from_connection; NO fresh Ledger::open)."
  exit 1
fi
echo "READ-FORK GATE: (informational) — $remaining in-serve read-fork(s) to migrate; gate flips to ENFORCING (ENFORCE_READ_FORK=1) at zero."
exit 0
