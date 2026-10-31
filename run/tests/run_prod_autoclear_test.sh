#!/usr/bin/env bash
#
# run_prod_autoclear_test.sh — PROD_v2.35.1
#
# Regression test for run_prod.sh's accidental-test-run self-clear.
#
# Bug: running ./run/run_desktop.sh (tenant=test) inside the PROD install drops
# two UNTRACKED, non-gitignored files in apps/aberp-ui/ (.aberp-db-writer.test.lock
# and aberp.duckdb.ckpt-ok) and run_prod.sh then refused with "Working tree has
# uncommitted changes — Frankenstein build", needing a human.
#
# Fix: run_prod.sh clears exactly those two (untracked, lock-not-held) files
# before the UNCHANGED Frankenstein check. This test runs the REAL run_prod.sh
# in a throwaway git repo (no copy of the logic) with HOME redirected and `npm`
# shadowed to fail, so the script stops right after the guard — nothing builds.
#
#   a. stray artifacts present        -> cleared, guard passes ("✓ HEAD matches")
#   b. real tracked modification      -> STILL refuses, artifacts' state irrelevant
#   b2. unexpected untracked file     -> STILL refuses (allowlist is exact)
#   c. writer lock HELD by a process  -> NOT removed, and the guard refuses
#   d. TRACKED file named like an artifact -> NOT removed
#
# Exit 0 = all pass; non-zero = failure.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
RUN_PROD="${SCRIPT_DIR}/../run_prod.sh"
[[ -f "$RUN_PROD" ]] || { echo "[fail] run_prod.sh not found at $RUN_PROD" >&2; exit 1; }

fails=0
pass() { echo "[ ok ] $1"; }
fail() { echo "[FAIL] $1" >&2; fails=$((fails + 1)); }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/run-prod-autoclear.XXXXXX")"
HOLDER_PID=""
cleanup() {
  [[ -n "$HOLDER_PID" ]] && kill "$HOLDER_PID" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

# A shadowed `npm` that fails: run_prod.sh reaches it only AFTER the guard, so
# reaching it proves the guard passed, and exit 4 stops the flow (no build).
mkdir -p "${TMP}/bin"
printf '#!/bin/sh\nexit 1\n' >"${TMP}/bin/npm"
chmod +x "${TMP}/bin/npm"

LOCK_REL="apps/aberp-ui/.aberp-db-writer.test.lock"
CKPT_REL="apps/aberp-ui/aberp.duckdb.ckpt-ok"

# make_repo <name> — a fresh repo whose HEAD equals a refs/remotes/origin/PROD_v*
# tip, with run_prod.sh (the REAL one) committed.
make_repo() {
  local r="${TMP}/$1"
  mkdir -p "${r}/run/lib" "${r}/apps/aberp-ui/ui" "${TMP}/home-$1"
  cp "$RUN_PROD" "${r}/run/run_prod.sh"
  # ADR-0111 addendum 2 — run_prod.sh sources the LaunchAgent helper.
  cp "${SCRIPT_DIR}/../lib/aberp_launch_agent.sh" "${r}/run/lib/aberp_launch_agent.sh"
  (
    cd "$r" || exit 1
    git init -q .
    git config user.email t@example.invalid; git config user.name t
    echo src > apps/aberp-ui/src.rs
    git add -A && git commit -q -m base
    git update-ref refs/remotes/origin/PROD_v9.9 HEAD
  )
  echo "$r"
}

# run_prod <repo> — runs the real script; sets OUT and RC.
run_prod() {
  local r="$1"
  OUT="$(cd "$r" && HOME="${TMP}/home-$(basename "$r")" PATH="${TMP}/bin:${PATH}" \
          NO_COLOR=1 bash ./run/run_prod.sh 2>&1)"
  RC=$?
}

REFUSE_MSG="Working tree has uncommitted changes"

# ---------- a. stray artifacts cleared, guard passes ------------------------
R="$(make_repo a)"
: >"${R}/${LOCK_REL}"; echo '{}' >"${R}/${CKPT_REL}"
echo db >"${R}/apps/aberp-ui/aberp.duckdb"   # gitignored in the real repo; must be left alone
printf 'aberp.duckdb\n' >"${R}/.gitignore"; (cd "$R" && git add .gitignore && git commit -q -m ign && git update-ref refs/remotes/origin/PROD_v9.9 HEAD)
run_prod "$R"
if [[ ! -e "${R}/${LOCK_REL}" && ! -e "${R}/${CKPT_REL}" ]]; then pass "a: both stray artifacts removed"; else fail "a: artifacts still present"; fi
[[ -f "${R}/apps/aberp-ui/aberp.duckdb" ]] && pass "a: repo-local db file left alone" || fail "a: db file was deleted"
grep -q "removed stale accidental test-run artifact: .*\.aberp-db-writer\.test\.lock" <<<"$OUT" \
  && grep -q "removed stale accidental test-run artifact: .*aberp\.duckdb\.ckpt-ok" <<<"$OUT" \
  && pass "a: one log line per cleared file" || fail "a: missing per-file log lines"
if grep -q "HEAD matches origin/PROD_v9.9" <<<"$OUT" && ! grep -q "$REFUSE_MSG" <<<"$OUT"; then
  pass "a: Frankenstein guard passes after clearing"
else fail "a: guard did not pass: $OUT"; fi

# ---------- b. real tracked modification still refuses ----------------------
R="$(make_repo b)"
: >"${R}/${LOCK_REL}"; echo '{}' >"${R}/${CKPT_REL}"
echo changed >>"${R}/apps/aberp-ui/src.rs"
run_prod "$R"
if [[ $RC -eq 1 ]] && grep -q "$REFUSE_MSG" <<<"$OUT"; then pass "b: tracked modification still refused (rc=1, same message)"
else fail "b: expected refusal, rc=$RC"; fi
[[ ! -e "${R}/${LOCK_REL}" ]] && pass "b: artifacts were still cleared (noise only)" || fail "b: artifact not cleared"

# ---------- b2. unexpected untracked file still refuses ---------------------
R="$(make_repo b2)"
: >"${R}/${LOCK_REL}"
echo x >"${R}/apps/aberp-ui/surprise.txt"
run_prod "$R"
if [[ $RC -eq 1 ]] && grep -q "$REFUSE_MSG" <<<"$OUT" && [[ -f "${R}/apps/aberp-ui/surprise.txt" ]]; then
  pass "b2: unknown untracked file refused and preserved"
else fail "b2: expected refusal + file kept, rc=$RC"; fi

# ---------- c. held lock is NOT removed -------------------------------------
R="$(make_repo c)"
: >"${R}/${LOCK_REL}"; echo '{}' >"${R}/${CKPT_REL}"
sleep 60 <"${R}/${LOCK_REL}" &
HOLDER_PID=$!
sleep 0.3
run_prod "$R"
kill "$HOLDER_PID" 2>/dev/null; wait "$HOLDER_PID" 2>/dev/null; HOLDER_PID=""
if [[ -e "${R}/${LOCK_REL}" && -e "${R}/${CKPT_REL}" ]]; then pass "c: held lock (and its marker) NOT removed"; else fail "c: removed while held"; fi
if [[ $RC -eq 1 ]] && grep -q "$REFUSE_MSG" <<<"$OUT" && grep -q "SKIP all" <<<"$OUT"; then
  pass "c: guard refuses and the skip is logged"
else fail "c: expected refusal with SKIP log, rc=$RC"; fi

# ---------- d. a TRACKED file with an artifact's name is never removed ------
R="$(make_repo d)"
: >"${R}/${LOCK_REL}"
(cd "$R" && git add -f "$LOCK_REL" && git commit -q -m trk && git update-ref refs/remotes/origin/PROD_v9.9 HEAD)
run_prod "$R"
if [[ -e "${R}/${LOCK_REL}" ]] && grep -q "tracked by git" <<<"$OUT"; then pass "d: tracked file untouched"; else fail "d: tracked file removed or unlogged"; fi

echo
if [[ $fails -eq 0 ]]; then echo "[PASS] run_prod_autoclear_test: all scenarios passed"; exit 0; fi
echo "[FAIL] run_prod_autoclear_test: $fails failure(s)" >&2
exit 1
