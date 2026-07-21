#!/usr/bin/env bash
#
# cut_gate_read_fork_probes.sh — proves CHECK N (the audit read-fork gate) has TEETH.
#
# MIGRATION-INVARIANT BY DESIGN. Every probe operates on a SYNTHETIC scratch file
# the probe itself creates/mutates/deletes inside a throwaway COPY of the tree —
# NEVER a real source file. So the probes stay valid through the entire read-fork
# migration (24 → 0), including full zero-residual.
#
# Detection probes assert on the SCANNER (adr0099_read_fork_scan.awk) directly:
# RED = the scanner emits a readfork record, GREEN = it stays silent. The gate
# probes exercise the allow-list + the fail-closed property (a de-gated scanner
# passes real read-forks). Exit 0 = every probe behaved.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCAN="tools/adr0099_read_fork_scan.awk"
GATE="tools/cut_gate_read_fork.sh"
ALLOW="tools/adr0099_read_fork_allowlist.txt"
SCRATCH="apps/aberp/src/zz_readfork_scratch.rs"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/readfork-probes.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0; bad=0

emits() { # $1 = rust source text -> echoes the scanner's readfork records
  printf '%s' "$1" | awk -f "$ROOT/$SCAN" 2>/dev/null
}
expect_emit() { # $1 label  $2 source
  if [[ -n "$(emits "$2")" ]]; then printf '  ✓ detects: %s\n' "$1"; pass=$((pass+1))
  else printf '  ✗ MISSED: %s — scanner emitted nothing\n' "$1"; bad=$((bad+1)); fi
}
expect_silent() { # $1 label  $2 source
  if [[ -z "$(emits "$2")" ]]; then printf '  ✓ correctly ignores: %s\n' "$1"; pass=$((pass+1))
  else printf '  ✗ FALSE POSITIVE: %s — scanner emitted: %s\n' "$1" "$(emits "$2")"; bad=$((bad+1)); fi
}
fresh() { local d; d="$(mktemp -d "$WORK/copy.XXXXXX")"; tar -C "$ROOT" --exclude=.git --exclude=target -cf - . | tar -C "$d" -xf -; printf '%s' "$d"; }

echo "negative probes for the ADR-0099 read-fork gate CHECK N (synthetic, migration-invariant)"
echo "root: $ROOT"; echo

# ── detection correctness (scanner) ──────────────────────────────────────────
# NOTE: snippets are MULTI-LINE like real code — the scanner emits a fn's record
# when its body-closing `}` is seen, so the opener/read must land on earlier lines.
expect_emit "P1 pure reader: Ledger::open + .entries() (no append)" \
'fn _p(p: &std::path::Path) {
    let l = aberp_audit_ledger::Ledger::open(p, t, h).unwrap();
    let _ = l.entries();
}'

expect_emit "P1b sync_mirror-only reader: Ledger::open + .sync_mirror()" \
'fn _p(p: &std::path::Path) {
    let l = aberp_audit_ledger::Ledger::open(p, t, h).unwrap();
    let _ = l.sync_mirror(&m);
}'

expect_emit "P1d single-line reader: opener shares the fn'\''s closing-brace line (deferred-flush fix)" \
'fn one(p: &std::path::Path) { let l = aberp_audit_ledger::Ledger::open(p, t, h).unwrap(); let _ = l.entries(); }'

expect_emit "P1e raw-SQL reader: fresh Connection::open + FROM audit_ledger (table name in a SQL string)" \
'fn raw(p: &std::path::Path) {
    let c = duckdb::Connection::open(p).unwrap();
    let _ = c.query_row("SELECT COUNT(*) FROM audit_ledger", [], |r| r.get::<_, i64>(0));
}'

expect_emit "P1f read-via-helper: Ledger::open + pending_from_ledger(&l) (read hidden one indirection away — the count_pending blind spot)" \
'fn _p(p: &std::path::Path) {
    let l = aberp_audit_ledger::Ledger::open(p, t, h).unwrap();
    let _ = submission_queue::pending_from_ledger(&l).unwrap().len();
}'

expect_silent "P2b raw INSERT INTO audit_ledger is a WRITE, not a read-fork" \
'fn w(p: &std::path::Path) {
    let c = duckdb::Connection::open(p).unwrap();
    let _ = c.execute("INSERT INTO audit_ledger (seq) VALUES (1)", []);
}'

expect_silent "P2 appender EXCLUDED: Ledger::open + .entries() + append_in_tx (that is a WRITE-fork)" \
'fn _p(p: &std::path::Path) {
    let l = aberp_audit_ledger::Ledger::open(p, t, h).unwrap();
    let _ = l.entries();
    aberp_audit_ledger::append_in_tx(&tx, &m, k, v, a, None).unwrap();
}'

expect_silent "P3 from_connection seam: reads the SHARED instance, not a fresh open" \
'fn _p(g: &Guard) {
    let l = aberp_audit_ledger::Ledger::from_connection(g.try_clone().unwrap(), t, h);
    let _ = l.entries();
}'

expect_silent "P3b from_connection seam + read-via-helper: pending_from_ledger on the SHARED instance (the migrated count_pending shape)" \
'fn _p(g: &Guard) {
    let l = aberp_audit_ledger::Ledger::from_connection(g.try_clone().unwrap(), t, h);
    let _ = submission_queue::pending_from_ledger(&l).unwrap().len();
}'

expect_silent "P4 cfg(test) ignored: a read-fork inside #[cfg(test)]" \
'#[cfg(test)]
mod z {
    fn _p(p: &std::path::Path) {
        let l = aberp_audit_ledger::Ledger::open(p, t, h).unwrap();
        let _ = l.entries();
    }
}'

expect_silent "P5 business read (no audit): Connection::open reading a business table" \
'fn _p(p: &std::path::Path) {
    let c = duckdb::Connection::open(p).unwrap();
    let _ = c.prepare("SELECT id FROM vendors");
}'

# ── gate wiring: allow-list + fail-closed ────────────────────────────────────
count_worklist() { ( cd "$1" && bash "$GATE" ) 2>&1 | grep -oE '[0-9]+ in-serve audit read-fork' | grep -oE '^[0-9]+'; }

echo "[P6 allow-list + FLOCK-FENCED] a FENCED scratch reader added to the allow-list is dropped"
c="$(fresh)"
printf 'fn probe_reader(p: &std::path::Path) {\n    let _g = crate::db_writer_lock::acquire_or_refuse(p, "t", "probe").unwrap();\n    let l = aberp_audit_ledger::Ledger::open(p, t, h).unwrap();\n    let _ = l.entries();\n}\n' > "$c/$SCRATCH"
base="$(count_worklist "$c")"
printf '%s:probe_reader\n' "$SCRATCH" >> "$c/$ALLOW"
withallow="$(count_worklist "$c")"
if [[ "$base" -gt 0 && "$withallow" -eq $((base-1)) ]]; then
  printf '  ✓ a flock-fenced allow-list entry is honoured (%s → %s)\n' "$base" "$withallow"; pass=$((pass+1))
else printf '  ✗ BROKEN: fenced allow-list entry not dropped (base=%s withallow=%s)\n' "$base" "$withallow"; bad=$((bad+1)); fi

echo "[P7 allow-list but NOT flock-fenced → exemption VOID] the incident-shaped hole"
c="$(fresh)"
# same reader but with NO acquire_or_refuse — a CLI-looking file that opens audit
# fresh without the cross-process flock is exactly the hazard, not an exemption.
printf 'fn probe_reader(p: &std::path::Path) {\n    let l = aberp_audit_ledger::Ledger::open(p, t, h).unwrap();\n    let _ = l.entries();\n}\n' > "$c/$SCRATCH"
base="$(count_worklist "$c")"
printf '%s:probe_reader\n' "$SCRATCH" >> "$c/$ALLOW"
withallow="$(count_worklist "$c")"
if [[ "$withallow" -eq "$base" ]]; then
  printf '  ✓ an UNFENCED allow-list entry is REFUSED exemption — still counted (%s → %s). The flock earns the exemption, the filename does not.\n' "$base" "$withallow"; pass=$((pass+1))
else printf '  ✗ HOLE: an unfenced allow-list entry was exempted (%s → %s) — CLI-on-filename is back\n' "$base" "$withallow"; bad=$((bad+1)); fi

echo "[P8 exemption↔premise coupling] deleting the flock test makes CHECK N go RED"
c="$(fresh)"
# The allow-list is non-empty in the tree, so the gate REQUIRES the flock tests.
# Remove the proving test file → the exemption's premise is gone → gate must fail.
rm -f "$c/apps/aberp/tests/db_writer_lock_e2e.rs"
rc=0; ( cd "$c" && bash "$GATE" ) >"$c/.o8" 2>&1 || rc=$?
if [[ "$rc" -ne 0 ]] && grep -qF 'EXEMPTION PREMISE UNTESTED' "$c/.o8"; then
  printf '  ✓ coupling holds: removing the flock test voids the CLI exemption (exit=%s) — the exemption cannot outlive its premise.\n' "$rc"; pass=$((pass+1))
else printf '  ✗ DECOUPLED: flock test removed but CHECK N still passed (exit=%s) — exemption can rot alone\n' "$rc"; sed 's/^/        /' "$c/.o8"; bad=$((bad+1)); fi

# INVERTED 2026-07-21 (finding F4). This META probe used to sabotage the scanner
# to `END{}` and assert the gate went GREEN — treating "a neutered scanner passes
# everything" as the proof that the scanner is load-bearing. That IS the F4
# vulnerability (an awk-version change on a CI runner silently greens the gate),
# and CHECK N0 now closes it. The probe's INTENT is preserved and strengthened:
# a neutered scanner must still be DETECTED, but now it is detected as BROKEN
# rather than silently believed. Proving the scanner is load-bearing no longer
# requires the gate to be exploitable.
echo "[META] a neutered scanner must be CAUGHT, not silently believed"
c="$(fresh)"
printf 'END{}\n' > "$c/$SCAN"   # sabotage: scanner emits nothing for every file
rc=0; ( cd "$c" && ENFORCE_READ_FORK=1 bash "$GATE" ) >"$c/.meta" 2>&1 || rc=$?
if [[ "$rc" -ne 0 ]] && grep -qF "SCANNER BROKEN" "$c/.meta"; then
  printf '  ✓ backstop holds: a neutered scanner (emits nothing) is caught by CHECK N0 and fails the gate (rc=%s) — a zero-fork verdict can no longer be produced by a dead tool.\n' "$rc"
  pass=$((pass+1))
else printf '  ✗ META BROKEN: neutered scanner was not caught (rc=%s; expected non-zero + "SCANNER BROKEN")\n' "$rc"; sed 's/^/        /' "$c/.meta"; bad=$((bad+1)); fi

# …and the backstop is NOT a policy switch: it stays RED when de-gated.
c="$(fresh)"
printf 'END{}\n' > "$c/$SCAN"
rc=0; ( cd "$c" && ENFORCE_READ_FORK=0 bash "$GATE" ) >"$c/.meta2" 2>&1 || rc=$?
if [[ "$rc" -ne 0 ]] && grep -qF "SCANNER BROKEN" "$c/.meta2"; then
  printf '  ✓ liveness is not bypassable: ENFORCE_READ_FORK=0 does not suppress CHECK N0 (rc=%s)\n' "$rc"
  pass=$((pass+1))
else printf '  ✗ META BROKEN: ENFORCE_READ_FORK=0 suppressed the liveness backstop (rc=%s)\n' "$rc"; bad=$((bad+1)); fi

echo
echo "probes passed: $pass   broken/escaped: $bad"
if [[ "$bad" -ne 0 ]]; then echo "READ-FORK PROBES: ✗ FAILED"; exit 1; fi
echo "READ-FORK PROBES: ✓ ALL CHECKS HAVE TEETH (synthetic; invariant under the migration)"
