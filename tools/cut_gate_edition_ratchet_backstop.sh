#!/usr/bin/env bash
#
# cut_gate_edition_ratchet_backstop.sh — SOURCED (not executed) by
# tools/cut_gate_edition_ratchet.sh.
#
# WHY THIS EXISTS (finding, 2026-07-22 — the same class as F4). The ratchet's
# four arms were `find … 2>/dev/null | grep`, zero-hits ⇒ green: the exact hole
# cut_gate_scanner_backstop.sh closed for the awk gates, in find shape. Two ways
# it went blind, neither of which the artifact probes could see (they only ever
# fed it a HEALTHY find against a synthetic tree):
#
#     find ERRORS (a scan root that isn't there, an unreadable dir, a predicate a
#         runner's find doesn't support) → stderr swallowed by 2>/dev/null, the
#         pipeline yields 0 lines → the arm prints ✓ → GATE PASSED, having
#         scanned an unknown fraction of the tree.
#     the matcher silently matches nothing (a regex/typo regression) → 0 lines →
#         the arm prints ✓ → GATE PASSED.
#
# cut_gate_scanner_backstop.sh is awk-shaped (`awk -f scanner file`) and does not
# drop in here; this is its find-shaped twin. Two halves:
#
#   rb_find    — replaces `find … 2>/dev/null`. find's stderr is CAPTURED to a
#                per-arm file, never swallowed. The live arms call rb_health on
#                that file afterwards: a find that wrote to stderr scanned an
#                unknown amount of the tree, so a zero-hit verdict built on it is
#                worthless and becomes SCANNER BROKEN.
#   rb_expect  — the known-POSITIVE control, run on EVERY invocation (CHECK E0).
#                Each arm's matcher is handed a SYNTHETIC tree carrying a planted
#                positive AND a planted look-alike negative, and the hit count is
#                asserted. A matcher that cannot see a planted run_portable.sh is
#                dead, not clean; one that matches the negative too is not a
#                matcher. The SAME arm functions feed the control and the live
#                scan, so the control cannot drift from what actually runs.
#
# Liveness is ALWAYS enforced and is NOT gated by ENFORCE_EDITION_RATCHET: a
# blind scanner is a broken tool, not a policy question.

_rb_dir=""; _rb_broken=0; _rb_ctl_bad=0
RB_DIR=""  # read by the sourcing gate — the CHECK E0 control tree lives under it

rb_init() {
  _rb_dir="$(mktemp -d "${TMPDIR:-/tmp}/edition-ratchet-bs.XXXXXX")" \
    || { echo "  ✗ SCANNER BROKEN: cannot mktemp a control dir"; _rb_broken=1; return 1; }
  # shellcheck disable=SC2064
  trap "rm -rf '$_rb_dir'" EXIT
  # shellcheck disable=SC2034  # consumed by the sourcing gate, not this file
  RB_DIR="$_rb_dir"
}

# rb_find <errfile> <find-args…> — run find, CAPTURING its stderr to <errfile>
# instead of /dev/null. Use it exactly where `find … 2>/dev/null` used to sit.
rb_find() { local err="$1"; shift; find "$@" 2>"$err"; }

# rb_health <arm-label> <errfile> — any bytes on find's stderr mean the scan did
# not complete over the whole intended tree; a "zero hits ⇒ clean" verdict built
# on it means nothing. Records the arm as broken (does not exit — the caller
# harvests all arms, then rb_live_ok decides).
rb_health() {
  local arm="$1" err="$2"
  [[ -s "$err" ]] || return 0
  printf '  ✗ SCANNER BROKEN [%s]: find wrote to stderr — a zero-hit result here means NOTHING:\n' "$arm"
  sed 's/^/      /' "$err"
  _rb_broken=1
}

# rb_expect <arm-label> <got> <want> <label> — known-positive/negative control
# assertion. A miss marks the controls bad (does not exit).
rb_expect() {
  local arm="$1" got="$2" want="$3" label="$4"
  if [[ "$got" == "$want" ]]; then
    printf '  ✓ %-46s %s hit(s)\n' "$label" "$got"
    return 0
  fi
  printf '  ✗ SCANNER BROKEN [%s]: %s — expected %s hit(s), got %s\n' "$arm" "$label" "$want" "$got"
  _rb_ctl_bad=1
  return 1
}

# rb_live_ok — non-zero if any LIVE arm's find errored during the real scan.
rb_live_ok() {
  [[ "$_rb_broken" -eq 0 ]] && return 0
  echo "  A find that errored scanned an unknown fraction of the tree — this gate cannot"
  echo "  claim the Portable/Defense surface is absent until the scan runs clean. (Fix the"
  echo "  scan; do not send its stderr to /dev/null.)"
  return 1
}

# rb_controls_ok — non-zero if any CHECK E0 known-positive control missed.
rb_controls_ok() {
  [[ "$_rb_ctl_bad" -eq 0 ]] && return 0
  echo "  A matcher that cannot see a planted positive is DEAD, not clean — this gate"
  echo "  enforces nothing until it is repaired. (Fix the matcher; do not skip the check.)"
  return 1
}
