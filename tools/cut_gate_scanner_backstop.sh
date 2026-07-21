#!/usr/bin/env bash
#
# cut_gate_scanner_backstop.sh — SOURCED (not executed) by the awk-scanner cut
# gates: cut_gate_keychain_seam.sh, cut_gate_read_fork.sh, cut_gate_write_fork.sh.
#
# WHY THIS EXISTS (finding F4, 2026-07-21). All three of those gates were
# "zero hits ⇒ green" with `awk … 2>/dev/null`. Measured on this tree BEFORE
# this file existed:
#
#     scanner CRASHES (awk syntax error)  → 0 hits → exit=0 → GATE PASSED
#     scanner SILENT  (prints nothing)    → 0 hits → exit=0 → GATE PASSED
#
# An awk-version change on a CI runner, a bad merge, or a deleted rule therefore
# produces a silently GREEN gate that enforces nothing. The opener-census gate
# does not have this hole — its CHECK P2 diffs against a non-empty frozen
# fingerprint file, so a dead scanner goes RED. These three had no such backstop.
#
# The two halves of the fix:
#   bs_scan   — replaces `awk … 2>/dev/null` in the per-file loop. stderr is
#               CAPTURED (never swallowed) and a non-zero awk exit is recorded.
#               Loops run in subshells, so failures are recorded in a FILE and
#               harvested afterwards by bs_scan_ok.
#   bs_check  — hands the scanner a SYNTHETIC control and asserts the hit count.
#               Both directions: a known-POSITIVE must hit (a scanner that finds
#               nothing when handed a planted bypass is broken, not clean) and a
#               known-NEGATIVE must not (or "it hits everything" would pass too).
#               Each gate's positives include the two LEXER TRAPS that were live
#               fail-opens — a char literal holding a quote, and a raw string
#               holding a stray quote — so a lexer regression is caught here even
#               if no real code happens to sit behind a trap that day.
#
# Liveness is ALWAYS enforced and is NOT covered by the ENFORCE_* switches: a
# dead scanner is a broken tool, not a policy question.

_bs_errs=""; _bs_dir=""; _bs_ctl_bad=0

bs_init() {
  _bs_dir="$(mktemp -d "${TMPDIR:-/tmp}/gatebackstop.XXXXXX")"
  _bs_errs="$_bs_dir/scan.err"
  : > "$_bs_errs"
  # shellcheck disable=SC2064
  trap "rm -rf '$_bs_dir'" EXIT
}

# bs_scan <scanner> <file> — emit the scanner's records; NEVER swallow a failure.
bs_scan() {
  local out rc
  out="$(awk -f "$1" "$2" 2>>"$_bs_errs")"; rc=$?
  [[ "$rc" -ne 0 ]] && printf 'scanner %s exited %s on %s\n' "$1" "$rc" "$2" >> "$_bs_errs"
  [[ -n "$out" ]] && printf '%s\n' "$out"
  return 0
}

# bs_scan_ok — call AFTER the per-file loop. Non-zero if any scan crashed or
# wrote to stderr. A "clean" verdict built on a broken scanner is worthless.
bs_scan_ok() {
  [[ -s "$_bs_errs" ]] || return 0
  echo "  ✗ SCANNER FAILED DURING THE SCAN — a zero-hit result here means NOTHING:"
  sed 's/^/      /' "$_bs_errs"
  return 1
}

# bs_check <scanner> <expected-hits> <label>  — control source on stdin.
bs_check() {
  local scan="$1" want="$2" label="$3" f rc got
  f="$_bs_dir/control.rs"; cat > "$f"
  awk -f "$scan" "$f" > "$_bs_dir/ctl.out" 2> "$_bs_dir/ctl.err"; rc=$?
  got="$(wc -l < "$_bs_dir/ctl.out" | tr -d ' ')"
  if [[ "$rc" -eq 0 && ! -s "$_bs_dir/ctl.err" && "$got" -eq "$want" ]]; then
    printf '  ✓ %s (%s hit(s), as expected)\n' "$label" "$got"; return 0
  fi
  printf '  ✗ SCANNER BROKEN: %s — expected %s hit(s), got %s (awk exit=%s)\n' \
         "$label" "$want" "$got" "$rc"
  [[ -s "$_bs_dir/ctl.err" ]] && sed 's/^/      stderr: /' "$_bs_dir/ctl.err"
  _bs_ctl_bad=1; return 1
}

# bs_controls_ok — non-zero if any bs_check failed.
bs_controls_ok() {
  [[ "$_bs_ctl_bad" -eq 0 ]] && return 0
  echo "  A scanner that cannot see a planted positive is DEAD, not clean — this gate"
  echo "  enforces nothing until it is repaired. (Fix the scanner; do not skip the check.)"
  return 1
}
