#!/usr/bin/env bash
#
# upgrade_prod_version_re_test.sh — 2026-07-21
#
# Negative gate for upgrade_prod.sh's VERSION_RE.
#
# Context: 7b849f7 widened the regex to `^PROD_(Defense_|Portable_)?v...` so
# this upgrader could drive the two edition lines. Both lines have since left
# this repo — Defense's refs were pruned 2026-07-11, Portable's launcher pair
# deleted 2026-07-21 — so the regex was narrowed back to `^PROD_v...`.
#
# That narrowing is a SAFETY guard, not a cleanup: while the infix arms were
# live, `upgrade_prod.sh PROD_Portable_v0.1.2` would have hard-reset an
# operator's PROD checkout onto a dev-profile edition ref.
#
# A guard that has never been observed rejecting anything is not yet a guard.
# So this test asserts the REFUSAL, not merely that valid versions survive:
# an edition version must exit non-zero AND say why.
#
# Why a subprocess and not the ABERP_UPGRADE_PROD_LIB_ONLY source-seam: that
# seam returns at line ~92, before VERSION_RE is defined at ~112, so the
# constant is not reachable through it. Driving the real script end-to-end
# also pins the thing that actually matters — the operator-visible exit code
# and message — rather than a regex in isolation.
#
# The version check (upgrade_prod.sh:168) deliberately runs BEFORE the
# dev-workspace sentinel (:197), so these assertions hold from any checkout
# path, dev tree included. No network, no git mutation: every case here is
# rejected before the script reaches its first git operation.
#
# Exit 0 = all pass; non-zero = failure (operator/CI gate).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
UPGRADE_PROD="${SCRIPT_DIR}/../upgrade_prod.sh"

if [[ ! -f "$UPGRADE_PROD" ]]; then
  echo "[fail] upgrade_prod.sh not found at $UPGRADE_PROD" >&2
  exit 1
fi

fails=0
pass() { echo "[ ok ] $1"; }
fail() { echo "[FAIL] $1" >&2; fails=$((fails + 1)); }

# The distinctive substring of the version-format refusal (upgrade_prod.sh:169).
readonly REJECT_MARKER="does not match"

# run_version <version> -> sets RC and OUT
run_version() {
  OUT="$(bash "$UPGRADE_PROD" "$1" 2>&1)"
  RC=$?
}

# ---------- 1. the edition versions must be REFUSED --------------------------
# These are the two real refs that motivated the narrowing: PROD_Portable_v0.1.2
# is a live branch+tag on THIS origin (so it would have resolved), and
# PROD_Defense_v0.2.1 was the abandoned Defense tip before the prune.
for v in PROD_Portable_v0.1.2 PROD_Defense_v0.2.1; do
  run_version "$v"
  if [[ $RC -eq 0 ]]; then
    fail "$v was ACCEPTED (exit 0) — the guard is not rejecting edition versions"
  elif [[ "$OUT" != *"$REJECT_MARKER"* ]]; then
    fail "$v exited $RC but NOT on the version-format check — refused for the wrong reason:
$(echo "$OUT" | head -3)"
  else
    pass "$v refused (exit $RC) on the version-format check"
  fi
done

# ---------- 2. the refusal must say where editions DO upgrade from -----------
# A bare "bad version" message would leave an operator stuck; the whole point
# of this change is to point them at the right repo.
run_version PROD_Portable_v0.1.2
if [[ "$OUT" != *"ABERP-Editions.git"* ]]; then
  fail "the refusal does not name ABERP-Editions.git — an operator is left without a next step"
else
  pass "refusal names ABERP-Editions.git as the editions' upgrade path"
fi
if [[ "$OUT" != *"NEM innen"* ]]; then
  fail "the refusal has no Hungarian arm — every other operator-facing die() in this script is EN+HU"
else
  pass "refusal carries the Hungarian arm"
fi

# ---------- 3. positive control — a well-formed PROD version is NOT refused --
# Without this, every assertion above would still pass if VERSION_RE rejected
# absolutely everything. PROD_v99.99.99 is deliberately well-formed but cannot
# exist on any origin, so the run dies at a LATER stage — which is exactly the
# proof we want: it got past the version check.
run_version PROD_v99.99.99
if [[ "$OUT" == *"$REJECT_MARKER"* ]]; then
  fail "PROD_v99.99.99 was rejected by VERSION_RE — the regex is over-tight, valid PROD versions are blocked"
else
  pass "PROD_v99.99.99 passes the version check (fails later, as it must — no such ref)"
fi

# ---------- 4. the pre-existing shape rules still hold -----------------------
# ADR-0056: 2-segment OR 3-segment only; 4+ segments and suffixes rejected.
for v in PROD_v2 PROD_v1.2.3.4 PROD_v1.2-rc1 PROD_2.32.1 prod_v1.2; do
  run_version "$v"
  if [[ "$OUT" != *"$REJECT_MARKER"* ]]; then
    fail "malformed version '$v' was NOT rejected by the version check"
  else
    pass "malformed version '$v' refused"
  fi
done

# ---------- report -----------------------------------------------------------
echo
if [[ $fails -eq 0 ]]; then
  echo "[ ok ] upgrade_prod.sh VERSION_RE: all assertions pass"
  exit 0
fi
echo "[FAIL] upgrade_prod.sh VERSION_RE: $fails assertion(s) failed" >&2
exit 1
