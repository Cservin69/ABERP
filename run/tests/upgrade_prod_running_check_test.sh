#!/usr/bin/env bash
#
# upgrade_prod_running_check_test.sh — S400
#
# Regression test for the upgrade_prod.sh "prod is still running" preflight.
#
# Bug (S399, Class-A false positive): the check used `pgrep -x aberp-ui` /
# `pgrep -x aberp`, which match the bare process NAME. A dev/test build from
# a DIFFERENT checkout (Ervin's ~/Documents/.../ABERP) — or this checkout's
# own target/debug — therefore tripped the refusal even though no prod binary
# was running, blocking a legitimate upgrade.
#
# Fix: scope detection to THIS checkout's RELEASE binary by command line via
# running_prod_pids() in upgrade_prod.sh.
#
# This test sources the REAL function (no copy — so it can't silently drift
# from the shipped predicate) and asserts, with live mock processes:
#   1. a dev-checkout process (target/debug) does NOT trip the check
#   2. a prod-checkout RELEASE process (target/release) DOES trip it
#   3. the report is exact — aberp-ui's pid is not also listed under "aberp:"
#
# Exit 0 = all pass; non-zero = failure (CI/operator gate).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
UPGRADE_PROD="${SCRIPT_DIR}/../upgrade_prod.sh"

if [[ ! -f "$UPGRADE_PROD" ]]; then
  echo "[fail] upgrade_prod.sh not found at $UPGRADE_PROD" >&2
  exit 1
fi

# Source the pure helpers only (no upgrade flow).
# shellcheck source=/dev/null
ABERP_UPGRADE_PROD_LIB_ONLY=1 source "$UPGRADE_PROD"
set +e  # upgrade_prod.sh sets -e; relax it so assertions read cleanly.

if ! declare -F running_prod_pids >/dev/null 2>&1; then
  echo "[fail] running_prod_pids() not defined after sourcing — test seam broken" >&2
  exit 1
fi

fails=0
pass() { echo "[ ok ] $1"; }
fail() { echo "[FAIL] $1" >&2; fails=$((fails + 1)); }

# ---------- build mock checkouts --------------------------------------------
TMP="$(mktemp -d "${TMPDIR:-/tmp}/s400-upgrade-prod.XXXXXX")"
cleanup() {
  [[ -n "${PID_PROD_UI:-}" ]] && kill "$PID_PROD_UI" 2>/dev/null
  [[ -n "${PID_PROD_CLI:-}" ]] && kill "$PID_PROD_CLI" 2>/dev/null
  [[ -n "${PID_DEV_UI:-}" ]] && kill "$PID_DEV_UI" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

PROD_ROOT="${TMP}/prodco"
DEV_ROOT="${TMP}/devco"
mkdir -p "${PROD_ROOT}/target/release" "${DEV_ROOT}/target/debug"

# Mock "binaries": tiny COMPILED executables named aberp-ui / aberp — not
# shell scripts, and not copies of a system binary. Fidelity matters two ways:
#   - A compiled binary's `comm` is "aberp-ui", so the OLD `pgrep -x aberp-ui`
#     predicate WOULD have false-positived on these — i.e. this test can
#     actually FAIL against the bug it guards (a shell-script mock has
#     comm=bash and would hide the regression).
#   - Copying a *system* binary (cp /bin/sleep) is SIGKILL'd on macOS (invalid
#     code signature); a freshly linked binary is linker-adhoc-signed and runs.
# Executing by absolute path sets argv[0] to that path, exactly how cargo execs
# the built binary, so `pgrep -f "<path>"` sees the same command line as prod.
CC="$(command -v cc || command -v gcc || command -v clang || true)"
if [[ -z "$CC" ]]; then
  echo "[skip] no C compiler (cc/gcc/clang) on PATH — cannot build comm-faithful" >&2
  echo "[skip] mock binaries; skipping. Install Xcode CLT or gcc to run this test." >&2
  exit 0
fi
SRC="${TMP}/mock.c"
printf '#include <unistd.h>\nint main(void){for(;;)sleep(60);return 0;}\n' >"$SRC"
make_mock() {
  "$CC" -o "$1" "$SRC" || { echo "[fail] could not compile mock $1" >&2; exit 1; }
}
make_mock "${PROD_ROOT}/target/release/aberp-ui"
make_mock "${PROD_ROOT}/target/release/aberp"
make_mock "${DEV_ROOT}/target/debug/aberp-ui"

# ---------- scenario 1: ONLY a dev-checkout process is alive -----------------
# (mocks are `sleep` copies → pass a duration; long enough to outlive the test)
"${DEV_ROOT}/target/debug/aberp-ui" 60 &
PID_DEV_UI=$!
sleep 0.5

out="$(running_prod_pids "$PROD_ROOT")"
if [[ -z "$out" ]]; then
  pass "dev-checkout process does NOT trip the prod running-check (false positive fixed)"
else
  fail "dev process tripped the check — got: ${out}"
fi

# Sanity: the dev checkout's OWN check would still see it (proves we didn't
# just break detection entirely).
out_dev="$(running_prod_pids "$DEV_ROOT")"
# DEV_ROOT has no target/release, so this must ALSO be empty — confirms the
# check is release-scoped, never debug.
if [[ -z "$out_dev" ]]; then
  pass "release-scoped: a target/debug process never trips the check, even in its own checkout"
else
  fail "debug process tripped the release-scoped check — got: ${out_dev}"
fi

# ---------- scenario 2: a real prod RELEASE process is alive -----------------
# aberp gets a trailing arg so its command line is "<path> 60" — exercising
# the `( |$)` space-anchor (the `aberp` pattern must match path-then-space,
# yet still NOT swallow "aberp-ui"'s command line).
"${PROD_ROOT}/target/release/aberp-ui" 60 &
PID_PROD_UI=$!
"${PROD_ROOT}/target/release/aberp" 60 &
PID_PROD_CLI=$!
sleep 0.5

out="$(running_prod_pids "$PROD_ROOT")"
if [[ -n "$out" ]]; then
  pass "prod RELEASE process DOES trip the check (true positive preserved)"
else
  fail "prod release process did NOT trip the check — stale zombie would slip through"
fi

# Both real pids must appear.
if echo "$out" | grep -q "$PID_PROD_UI" && echo "$out" | grep -q "$PID_PROD_CLI"; then
  pass "both aberp-ui and aberp release pids are reported"
else
  fail "missing a pid in report. ui=${PID_PROD_UI} cli=${PID_PROD_CLI} report: ${out}"
fi

# Exactness: aberp-ui's pid must NOT appear on the "aberp:" line (the `( |$)`
# anchor must stop the `aberp` pattern from swallowing `aberp-ui`).
aberp_line="$(echo "$out" | grep -E '^\s*aberp:')"
if echo "$aberp_line" | grep -qw "$PID_PROD_UI"; then
  fail "aberp-ui pid ${PID_PROD_UI} leaked onto the 'aberp:' line — anchor broken: ${aberp_line}"
else
  pass "exact report: aberp-ui pid not double-counted under 'aberp:'"
fi

# The dev process must STILL be excluded even with prod processes present.
if echo "$out" | grep -qw "$PID_DEV_UI"; then
  fail "dev pid ${PID_DEV_UI} leaked into the prod report"
else
  pass "dev pid stays excluded while prod processes run"
fi

# ---------- result ----------------------------------------------------------
echo
if [[ $fails -eq 0 ]]; then
  echo "[pass] all upgrade_prod running-check assertions passed"
  exit 0
fi
echo "[fail] ${fails} assertion(s) failed" >&2
exit 1
