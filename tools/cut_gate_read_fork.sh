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
# MODE (H3 STEP 7 acceptance — the worklist hit ZERO, so ENFORCE is now the
# DEFAULT; the acceptance state is fork-zero-ENFORCED):
#   default             — ENFORCING: exit non-zero if ANY non-allow-listed in-serve
#                         read-fork remains. Also set explicitly in cut-gate.yml so
#                         the flip is visible at the CI surface.
#   ENFORCE_READ_FORK=0 — INFORMATIONAL: print the in-serve read-forks + count,
#                         exit 0. Retained only as a local diagnostic probe.
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

enforce="${ENFORCE_READ_FORK:-1}"
echo "ADR-0099 H3 read-fork gate (CHECK N) — root: $ROOT  (mode: $([[ "$enforce" == "1" ]] && echo ENFORCING || echo informational))"

scope_files() { find apps/aberp/src modules crates -name '*.rs' | grep -vE '/tests/|/aberp-db/' | sort; }
allow_set="$(grep -vE '^\s*#' "$ALLOW" | sed '/^\s*$/d' | sort -u)"
is_allowed() { grep -qxF "$1" <<< "$allow_set"; }

# ── EXEMPTION ↔ PREMISE COUPLING (hard invariant, ALWAYS enforced) ──
# The CLI allow-list is sound ONLY because the cross-process F-E flock refuses a
# second writer — proven by two PERMANENT process-level tests. If the allow-list
# exempts anything, those tests MUST exist; otherwise the premise that justifies
# every entry has silently rotted and the exemptions are void. Couple them so
# neither can be removed alone.
FLOCK_TEST_FILE="apps/aberp/tests/db_writer_lock_e2e.rs"
FLOCK_REFUSE_TEST="second_process_is_refused_the_whole_db_writer_lock"
FLOCK_SIGKILL_TEST="lock_is_released_when_the_holder_is_sigkilled"
if [[ -n "$allow_set" ]]; then
  miss=""
  grep -q "fn ${FLOCK_REFUSE_TEST}" "$FLOCK_TEST_FILE" 2>/dev/null || miss="$miss $FLOCK_REFUSE_TEST"
  grep -q "fn ${FLOCK_SIGKILL_TEST}" "$FLOCK_TEST_FILE" 2>/dev/null || miss="$miss $FLOCK_SIGKILL_TEST"
  if [[ -n "$miss" ]]; then
    echo "✗ EXEMPTION PREMISE UNTESTED — the CLI read-fork allow-list exempts $(wc -l <<< "$allow_set" | tr -d ' ') entry-lines"
    echo "  on the cross-process flock, but its proving test(s) are MISSING:$miss"
    echo "  (expected in $FLOCK_TEST_FILE). The premise that justifies EVERY allow-list entry is"
    echo "  gone → the exemption is VOID. Restore the flock test, or empty the allow-list."
    echo
    echo "READ-FORK GATE: ✗ FAILED (exemption/premise decoupled — a hard invariant, not informational)"
    exit 1
  fi
fi

# A CLI one-shot's fresh audit read is coherent ONLY because the cross-process
# whole-DB writer flock (F-E, db_writer_lock::acquire_or_refuse) makes it mutually
# exclusive with serve — aberp-db's single-writer is a process-LOCAL Mutex and
# cannot fence a second process. So an allow-list entry is honoured ONLY if the
# file actually acquires that flock; a "CLI" file that opens the DB WITHOUT the
# flock can run concurrently with serve, read a stale main-file head, and (if it
# then appends) fork the chain — the incident's primitive. The exemption must be
# EARNED by the flock, never granted on the filename.
is_flock_fenced() { grep -qE 'acquire_or_refuse|try_acquire' "$1"; }

remaining=0
worklist="$(mktemp)"; allowed_hits="$(mktemp)"; unfenced="$(mktemp)"
while IFS= read -r f; do
  while IFS= read -r rec; do
    [[ -z "$rec" ]] && continue
    fname="$(cut -d: -f2 <<< "$rec")"
    key="$f:$fname"
    if is_allowed "$key"; then
      if is_flock_fenced "$f"; then
        printf '%s:%s\n' "$f" "$rec" >> "$allowed_hits"; continue
      fi
      # Allow-listed but NOT flock-fenced → the exemption's justification is
      # absent → it is a live cross-process hazard, NOT an accepted one.
      printf '%s:%s\n' "$f" "$rec" >> "$unfenced"
      printf '%s:%s\n' "$f" "$rec" >> "$worklist"
      remaining=$((remaining + 1)); continue
    fi
    printf '%s:%s\n' "$f" "$rec" >> "$worklist"
    remaining=$((remaining + 1))
  done < <(awk -f "$SCAN" "$f" 2>/dev/null)
done < <(scope_files)

if [[ -s "$unfenced" ]]; then
  echo "  ✗ ALLOW-LISTED BUT NOT FLOCK-FENCED (exemption VOID — these read audit fresh with NO"
  echo "    cross-process mutual exclusion against serve; add db_writer_lock::acquire_or_refuse"
  echo "    or migrate to a Handle read):"
  sort "$unfenced" | sed 's/^/      /'
fi
rm -f "$unfenced"

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
