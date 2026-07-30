#!/usr/bin/env bash
#
# run_desktop_stale_writer_reap_test.sh — S445
#
# Teeth for the DEV-only stale-orphan writer reap in run/run_desktop.sh.
#
# Why it exists: crash-testing with `kill -9` on the Tauri shell leaves its
# `aberp serve` child alive holding the cross-process whole-DB writer flock
# (apps/aberp/src/db_writer_lock.rs), so the next launch dies at "refusing to
# boot: another writer holds the tenant DB". The launcher now reaps that orphan.
# Killing a process on a predicate is exactly the kind of code that must not be
# trusted on inspection, so this test drives the REAL functions (sourced, never
# copied — they cannot silently drift from the shipped predicate) against LIVE
# mock processes, and asserts BOTH directions:
#
#   - it DOES reap a true stale orphan (the convenience actually works), and
#   - it does NOT kill a legitimately-running writer, a sibling of this run, a
#     non-`aberp serve` holder, another tenant's writer, a lock-holder that is
#     not on this exact db file, or ANYTHING at all once the situation is
#     ambiguous — and it refuses outright outside the dev checkout / on prod.
#
# Exit 0 = all pass; non-zero = failure (CI/operator gate).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
RUN_DESKTOP="${SCRIPT_DIR}/../run_desktop.sh"

if [[ ! -f "$RUN_DESKTOP" ]]; then
  echo "[fail] run_desktop.sh not found at $RUN_DESKTOP" >&2
  exit 1
fi

# Shorten the reap bounds so the SIGKILL-escalation scenario is quick. The
# launcher takes these as `: "${VAR:=default}"`, so exporting wins.
export STALE_WRITER_TERM_WAIT_SECS=2
export STALE_WRITER_KILL_WAIT_SECS=2

# Source the pure helpers only (no build, no launch).
# shellcheck source=/dev/null
ABERP_RUN_DESKTOP_LIB_ONLY=1 source "$RUN_DESKTOP"

for fn in stale_orphan_writer_pids reap_stale_orphan_writer; do
  if ! declare -F "$fn" >/dev/null 2>&1; then
    echo "[fail] ${fn}() not defined after sourcing — test seam broken" >&2
    exit 1
  fi
done

fails=0
pass() { echo "[ ok ] $1"; }
fail() { echo "[FAIL] $1" >&2; fails=$((fails + 1)); }

# ---------- scratch + cleanup -----------------------------------------------
# NOTE: every scenario gets its OWN directory (own db file, own lock file) so
# one scenario's holders can never be seen by another's predicate call.
TMP="$(mktemp -d "${TMPDIR:-/tmp}/s445-stale-writer.XXXXXX")"
MOCK_PIDS=""
cleanup() {
  local p
  for p in $MOCK_PIDS; do
    kill -KILL "$p" 2>/dev/null
  done
  wait 2>/dev/null
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

# ---------- comm-faithful mock `aberp` --------------------------------------
# A COMPILED binary, not a shell script: argv[0] must be the real path and the
# process name must be `aberp`, or the predicate under test would be exercised
# against a `bash` process and the test could not fail against the bug it
# guards. (Same reasoning as run/tests/upgrade_prod_running_check_test.sh.)
# Copying a SYSTEM binary is SIGKILL'd on macOS for an invalid signature; a
# freshly linked one is linker-adhoc-signed and runs.
CC="$(command -v cc || command -v gcc || command -v clang || true)"
if [[ -z "$CC" ]]; then
  echo "[skip] no C compiler (cc/gcc/clang) on PATH — cannot build comm-faithful" >&2
  echo "[skip] mock binaries; skipping. Install Xcode CLT or gcc to run this test." >&2
  exit 0
fi

SRC="${TMP}/mock_aberp.c"
cat >"$SRC" <<'MOCK_C'
/* Stand-in for `aberp serve`. Real argv shape, per
 * apps/aberp-ui/src/backend.rs::spawn:
 *   <abs>/aberp serve --tenant <t> --db <abs-db> --port 0
 * Mock-only trailing flags (invisible to the predicate, which looks only for
 * `aberp serve` and `--tenant <t>`):
 *   --mock-lock <p>     flock(LOCK_EX) and HOLD p — the whole-DB writer lock
 *   --mock-pidfile <p>  write the holding pid to p once the fds are open
 *   --mock-orphan       fork; parent exits, so the holder is reparented (ppid 1)
 *   --mock-detach       setsid() — own process group, as any writer left over
 *                       from a PREVIOUS launcher run necessarily has. Without
 *                       it a background job stays in this script's process
 *                       group (non-interactive shells have no job control),
 *                       which is the one thing clause (5) exists to reject.
 *   --mock-no-db        hold the lock file but NOT the db file
 *   --mock-ignore-term  SIG_IGN on SIGTERM, forcing the SIGKILL escalation
 *   --mock-morph-on-term  on SIGTERM, exec /bin/sleep — SAME pid, different
 *                       argv, fds (and therefore the flock) still held. A
 *                       deterministic stand-in for "this pid is no longer the
 *                       process we attributed", which is what a pid RECYCLED
 *                       during the SIGTERM wait looks like to the launcher.
 *                       Real pid recycling cannot be provoked on demand; this
 *                       reproduces the only thing the guard can actually see.
 */
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>

static void morph_on_term(int sig) {
  (void)sig;                      /* execve is async-signal-safe */
  execl("/bin/sleep", "sleep", "300", (char *)0);
  _exit(7);
}

int main(int argc, char **argv) {
  const char *db = NULL, *lock = NULL, *pidfile = NULL;
  int orphan = 0, detach = 0, no_db = 0, ignore_term = 0, morph = 0, i, fd;
  for (i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "--db") && i + 1 < argc) db = argv[++i];
    else if (!strcmp(argv[i], "--mock-lock") && i + 1 < argc) lock = argv[++i];
    else if (!strcmp(argv[i], "--mock-pidfile") && i + 1 < argc) pidfile = argv[++i];
    else if (!strcmp(argv[i], "--mock-orphan")) orphan = 1;
    else if (!strcmp(argv[i], "--mock-detach")) detach = 1;
    else if (!strcmp(argv[i], "--mock-no-db")) no_db = 1;
    else if (!strcmp(argv[i], "--mock-ignore-term")) ignore_term = 1;
    else if (!strcmp(argv[i], "--mock-morph-on-term")) morph = 1;
  }
  if (orphan) {
    pid_t p = fork();
    if (p < 0) return 1;
    if (p > 0) return 0;   /* parent exits -> child reparents to launchd (1) */
  }
  if (detach && setsid() < 0) return 6;
  if (ignore_term) signal(SIGTERM, SIG_IGN);
  if (morph) signal(SIGTERM, morph_on_term);
  if (lock) {                                  /* fds held open on purpose */
    fd = open(lock, O_RDWR | O_CREAT, 0644);
    if (fd < 0) return 2;
    if (flock(fd, LOCK_EX | LOCK_NB) != 0) return 3;
  }
  if (db && !no_db) {
    fd = open(db, O_RDWR | O_CREAT, 0644);
    if (fd < 0) return 4;
  }
  if (pidfile) {
    FILE *f = fopen(pidfile, "w");
    if (!f) return 5;
    fprintf(f, "%d\n", (int)getpid());
    fclose(f);
  }
  for (;;) sleep(60);
  return 0;
}
MOCK_C

BIN_DIR="${TMP}/bin"
mkdir -p "$BIN_DIR"
for name in aberp aberp-ui; do
  "$CC" -o "${BIN_DIR}/${name}" "$SRC" \
    || { echo "[fail] could not compile mock ${name}" >&2; exit 1; }
done

# ---------- helpers ----------------------------------------------------------
# scenario_dir <tag> -> prints a fresh dir holding an empty db + lock file
scenario_dir() {
  local d="${TMP}/$1"
  mkdir -p "$d"
  : >"${d}/aberp.duckdb"
  : >"${d}/.aberp-db-writer.test.lock"
  printf '%s' "$d"
}

# The `--port` the mocks advertise. aberp-ui's spawn hardcodes `--port 0`
# (apps/aberp-ui/src/backend.rs), which is what clause (3c) keys on; a scenario
# overrides this to model a serve an operator started by hand on a real port.
MOCK_PORT=0

# spawn_mock <bin> <tenant> <dir> [extra mock flags...] -> prints holder pid
# Blocks until the holder has BOTH fds open (pidfile is written last), so no
# scenario can race the predicate.
#
# The `</dev/null >/dev/null 2>&1` is load-bearing, not hygiene: this function
# runs inside a `$(...)` command substitution, and a background child that
# inherits the substitution's stdout pipe holds it open forever — the caller
# would hang instead of getting the pid.
spawn_mock() {
  local bin="$1" tenant="$2" dir="$3"
  shift 3
  local pf="${dir}/pidfile.$$.${RANDOM}"
  : >"$pf"
  "$bin" serve --tenant "$tenant" --db "${dir}/aberp.duckdb" --port "$MOCK_PORT" \
    --mock-lock "${dir}/.aberp-db-writer.test.lock" --mock-pidfile "$pf" "$@" \
    </dev/null >/dev/null 2>&1 &
  local pid="" i=0
  while [[ $i -lt 60 ]]; do
    pid="$(tr -d ' \n' <"$pf" 2>/dev/null)"
    [[ -n "$pid" ]] && break
    sleep 0.1
    i=$((i + 1))
  done
  if [[ -z "$pid" ]]; then
    echo "[fail] mock ${bin} never reported a holder pid (dir ${dir})" >&2
    exit 1
  fi
  MOCK_PIDS+=" ${pid}"
  printf '%s' "$pid"
}

# spawn_mock_parented <bin> <tenant> <dir> [flags...] -> sets PARENTED_PID
# A holder whose parent STAYS ALIVE. It cannot go through spawn_mock: that runs
# inside a `$(...)`, and the substitution's subshell exits the moment it returns
# — which reparents its background child to pid 1 and turns a "live-parented
# writer" scenario into an orphan one. (That is not hypothetical: it silently
# defeated this test's most important assertion until the assertion failed.)
# Spawned directly from the test shell instead, so the parent outlives it.
PARENTED_PID=""
spawn_mock_parented() {
  local bin="$1" tenant="$2" dir="$3"
  shift 3
  local pf="${dir}/pidfile.parented.$$"
  : >"$pf"
  "$bin" serve --tenant "$tenant" --db "${dir}/aberp.duckdb" --port "$MOCK_PORT" \
    --mock-lock "${dir}/.aberp-db-writer.test.lock" --mock-pidfile "$pf" "$@" \
    </dev/null >/dev/null 2>&1 &
  PARENTED_PID=""
  local i=0
  while [[ $i -lt 60 ]]; do
    PARENTED_PID="$(tr -d ' \n' <"$pf" 2>/dev/null)"
    [[ -n "$PARENTED_PID" ]] && break
    sleep 0.1
    i=$((i + 1))
  done
  if [[ -z "$PARENTED_PID" ]]; then
    echo "[fail] parented mock never reported a holder pid (dir ${dir})" >&2
    exit 1
  fi
  MOCK_PIDS+=" ${PARENTED_PID}"
}

alive()      { kill -0 "$1" 2>/dev/null; }
reap_quiet() { reap_stale_orphan_writer "$@" >/dev/null 2>&1; }
# gone <pid> — bounded wait for a pid to disappear
gone() {
  local i=0
  while [[ $i -lt 30 ]]; do
    alive "$1" || return 0
    sleep 0.1
    i=$((i + 1))
  done
  return 1
}
predicate_rc() {
  stale_orphan_writer_pids "$1" "$2" >/dev/null 2>&1
  echo $?
}

# ============================================================================
# 1. nobody holds the lock -> "nothing to do", boot proceeds
# ============================================================================
d="$(scenario_dir s1)"
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "1" ]]; then
  pass "no holder: predicate reports 'nothing to do' (rc 1)"
else
  fail "no holder: expected rc 1, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
if reap_quiet "$TMP" "${d}/aberp.duckdb" test; then
  pass "no holder: reap returns clear-to-boot"
else
  fail "no holder: reap must return clear-to-boot"
fi

# ============================================================================
# 2. THE headline case — a true stale orphan IS reaped, and the flock is free
# ============================================================================
d="$(scenario_dir s2)"
orphan="$(spawn_mock "${BIN_DIR}/aberp" test "$d" --mock-orphan --mock-detach)"
if [[ "$(ps -o ppid= -p "$orphan" | tr -d ' ')" == "1" ]]; then
  pass "orphan setup: mock holder is reparented to pid 1"
else
  fail "orphan setup: mock holder ppid is not 1 — scenario invalid"
fi
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "0" ]]; then
  pass "stale orphan: predicate qualifies it (rc 0)"
else
  fail "stale orphan: expected rc 0, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
if [[ "$(stale_orphan_writer_pids "${d}/aberp.duckdb" test)" == "$orphan" ]]; then
  pass "stale orphan: predicate names exactly the orphan pid"
else
  fail "stale orphan: predicate did not name pid ${orphan}"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" test
if gone "$orphan"; then
  pass "stale orphan: reaped (pid ${orphan} gone)"
else
  fail "stale orphan: pid ${orphan} survived the reap"
fi
# The kernel releases a flock on exit — prove it by taking the lock for real.
# Run inline (not via spawn_mock): the mock exits 3 when the flock is still
# held, so "did it stay alive" is the assertion, not a hard test abort.
"${BIN_DIR}/aberp" serve --tenant test --db "${d}/aberp.duckdb" --port 0 \
  --mock-lock "${d}/.aberp-db-writer.test.lock" </dev/null >/dev/null 2>&1 &
taker=$!
MOCK_PIDS+=" ${taker}"
sleep 0.5
if alive "$taker"; then
  pass "stale orphan: the whole-DB writer flock is genuinely acquirable again"
else
  fail "stale orphan: flock NOT released — a fresh writer could not take it"
fi
kill -KILL "$taker" 2>/dev/null

# ============================================================================
# 3. a LIVE-PARENTED writer is never killed (the data-loss case). Detached, so
#    it models the realistic danger — a writer from ANOTHER launcher run, in its
#    own process group, whose parent is alive — and so clause (4) is the ONLY
#    clause that can reject it. Kill this scenario and the feature becomes a
#    data-loss bug, not a convenience.
# ============================================================================
d="$(scenario_dir s3)"
spawn_mock_parented "${BIN_DIR}/aberp" test "$d" --mock-detach
live="$PARENTED_PID"
# Pin the setup itself: if this ever degrades back into an orphan, the scenario
# below would "pass" while testing nothing.
if [[ "$(ps -o ppid= -p "$live" | tr -d ' ')" == "$$" ]]; then
  pass "live-parented setup: holder's parent is this test script and is alive"
else
  fail "live-parented setup: holder ppid is $(ps -o ppid= -p "$live" | tr -d ' '), not $$ — scenario invalid"
fi
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "2" ]]; then
  pass "live-parented writer: predicate reports AMBIGUOUS (rc 2)"
else
  fail "live-parented writer: expected rc 2, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" test
if alive "$live"; then
  pass "live-parented writer: NOT killed by the reap"
else
  fail "live-parented writer pid ${live} WAS KILLED — data-loss regression"
fi
kill -KILL "$live" 2>/dev/null

# ============================================================================
# 4. an orphan that is NOT `aberp serve` is never killed
# ============================================================================
d="$(scenario_dir s4)"
notserve="$(spawn_mock "${BIN_DIR}/aberp-ui" test "$d" --mock-orphan --mock-detach)"
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "2" ]]; then
  pass "non-\`aberp serve\` orphan: predicate reports AMBIGUOUS (rc 2)"
else
  fail "non-\`aberp serve\` orphan: expected rc 2, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" test
if alive "$notserve"; then
  pass "non-\`aberp serve\` orphan: NOT killed"
else
  fail "non-\`aberp serve\` orphan pid ${notserve} WAS KILLED"
fi
kill -KILL "$notserve" 2>/dev/null

# ============================================================================
# 5. another tenant's orphan is never killed
# ============================================================================
d="$(scenario_dir s5)"
other="$(spawn_mock "${BIN_DIR}/aberp" demo "$d" --mock-orphan --mock-detach)"
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "2" ]]; then
  pass "other-tenant orphan: predicate reports AMBIGUOUS (rc 2)"
else
  fail "other-tenant orphan: expected rc 2, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" test
if alive "$other"; then
  pass "other-tenant orphan: NOT killed"
else
  fail "other-tenant orphan pid ${other} WAS KILLED"
fi
kill -KILL "$other" 2>/dev/null

# ============================================================================
# 6. a lock-holder that does NOT have THIS exact db file open is never killed
#    (the absolute-path clause — the same identification the manual lsof used)
# ============================================================================
d="$(scenario_dir s6)"
nodb="$(spawn_mock "${BIN_DIR}/aberp" test "$d" --mock-orphan --mock-detach --mock-no-db)"
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "2" ]]; then
  pass "lock-holder without the exact db open: predicate reports AMBIGUOUS (rc 2)"
else
  fail "lock-holder without the exact db open: expected rc 2, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" test
if alive "$nodb"; then
  pass "lock-holder without the exact db open: NOT killed"
else
  fail "lock-holder without the exact db open pid ${nodb} WAS KILLED"
fi
kill -KILL "$nodb" 2>/dev/null

# ============================================================================
# 7. AMBIGUITY: a qualifying orphan alongside a live-parented holder ->
#    NEITHER is killed. One unattributable holder disqualifies the whole set.
# ============================================================================
d="$(scenario_dir s7)"
amb_orphan="$(spawn_mock "${BIN_DIR}/aberp" test "$d" --mock-orphan --mock-detach)"
# The second holder cannot also take the flock (it is exclusive and the orphan
# has it), so model the live one as a process that merely has the lock file
# OPEN — which is exactly what lsof reports, and exactly the situation the
# launcher must not try to reason its way through. Live-parented (its ppid is
# this test) and not `aberp serve`: two independent reasons to keep hands off.
/bin/sh -c "exec 9<\"${d}/.aberp-db-writer.test.lock\"; exec sleep 300" \
  </dev/null >/dev/null 2>&1 &
amb_live=$!
MOCK_PIDS+=" ${amb_live}"
sleep 0.5
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "2" ]]; then
  pass "ambiguous set: predicate reports AMBIGUOUS (rc 2)"
else
  fail "ambiguous set: expected rc 2, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" test
if alive "$amb_orphan" && alive "$amb_live"; then
  pass "ambiguous set: NOTHING was killed — not even the qualifying orphan"
else
  fail "ambiguous set: a pid was killed (orphan=${amb_orphan} live=${amb_live})"
fi
kill -KILL "$amb_orphan" "$amb_live" 2>/dev/null

# ============================================================================
# 8. HARD gate B — a db OUTSIDE the dev checkout is never touched, even with a
#    textbook orphan holding it. (Prod/operator DBs live under ~/.aberp/.)
# ============================================================================
d="$(scenario_dir s8)"
outside="$(spawn_mock "${BIN_DIR}/aberp" test "$d" --mock-orphan --mock-detach)"
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "0" ]]; then
  pass "gate B setup: the orphan WOULD otherwise qualify (rc 0)"
else
  fail "gate B setup: orphan does not qualify — scenario cannot prove the gate"
fi
# repo_root deliberately elsewhere, so the db is not under it.
reap_quiet "${TMP}/some-other-checkout" "${d}/aberp.duckdb" test
if alive "$outside"; then
  pass "gate B: db outside the dev checkout — refused, orphan left alone"
else
  fail "gate B: pid ${outside} was killed for a db outside the checkout"
fi

kill -KILL "$outside" 2>/dev/null

# ============================================================================
# 9. HARD gate A — tenant=prod refuses outright.
#
#    The orphan's OWN argv must say `--tenant prod`, or clause (3b) rejects it
#    on the tenant mismatch and the scenario passes without gate A existing.
#    (It did: deleting gate A left the whole suite green until this scenario
#    was given a prod-argv holder.) Gate B is satisfied deliberately — repo_root
#    is $TMP and the db is under it — so gate A is the ONLY thing left to stop
#    the kill.
# ============================================================================
d="$(scenario_dir s9)"
prodish="$(spawn_mock "${BIN_DIR}/aberp" prod "$d" --mock-orphan --mock-detach)"
if [[ "$(predicate_rc "${d}/aberp.duckdb" prod)" == "0" ]]; then
  pass "gate A setup: the holder WOULD otherwise qualify on tenant=prod (rc 0)"
else
  fail "gate A setup: holder does not qualify — scenario cannot prove the gate"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" prod
if alive "$prodish"; then
  pass "gate A: tenant=prod — reap refuses to run at all"
else
  fail "gate A: pid ${prodish} was killed on tenant=prod"
fi
kill -KILL "$prodish" 2>/dev/null

# ============================================================================
# 10. SIGTERM is preferred; SIGKILL escalates only on a bounded timeout
# ============================================================================
d="$(scenario_dir s10)"
stubborn="$(spawn_mock "${BIN_DIR}/aberp" test "$d" --mock-orphan --mock-detach --mock-ignore-term)"
out="$(reap_stale_orphan_writer "$TMP" "${d}/aberp.duckdb" test 2>&1)"
if echo "$out" | grep -q "SIGTERM"; then
  pass "escalation: SIGTERM is tried first"
else
  fail "escalation: no SIGTERM step in the reap output"
fi
if echo "$out" | grep -q "escalating to SIGKILL"; then
  pass "escalation: SIGKILL only after the ${STALE_WRITER_TERM_WAIT_SECS}s bound"
else
  fail "escalation: never escalated to SIGKILL — output: ${out}"
fi
if gone "$stubborn"; then
  pass "escalation: SIGTERM-ignoring orphan is ultimately reaped"
else
  fail "escalation: pid ${stubborn} survived SIGKILL"
fi

# ============================================================================
# 11. clause (5) — a holder in THIS run's own process group is never killed,
#     even at ppid 1. Same mock as scenario 2 minus --mock-detach: a background
#     job in a non-interactive shell inherits the script's process group, which
#     is exactly the "sibling of this launcher run" shape clause (5) rejects.
# ============================================================================
d="$(scenario_dir s11)"
sibling="$(spawn_mock "${BIN_DIR}/aberp" test "$d" --mock-orphan)"
if [[ "$(ps -o pgid= -p "$sibling" | tr -d ' ')" == "$(ps -o pgid= -p $$ | tr -d ' ')" ]]; then
  pass "own-process-group setup: holder shares this script's pgid"
else
  fail "own-process-group setup: holder is in a different pgid — scenario invalid"
fi
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "2" ]]; then
  pass "own-process-group holder: predicate reports AMBIGUOUS (rc 2)"
else
  fail "own-process-group holder: expected rc 2, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" test
if alive "$sibling"; then
  pass "own-process-group holder: NOT killed"
else
  fail "own-process-group holder pid ${sibling} WAS KILLED — clause (5) has no teeth"
fi
kill -KILL "$sibling" 2>/dev/null

# ============================================================================
# 12. clause (3c) — an orphan on a REAL port is never killed.
#     PPID 1 alone does NOT mean "dead session". A serve an operator started by
#     hand (docs/walkthroughs/dr-playbook.md §3 tells them to) and then nohup'd,
#     `disown`ed, or whose `cargo run` parent was killed is reparented to 1
#     while still LIVE and still reachable on its port. aberp-ui's spawn always
#     passes `--port 0`, so requiring it is what separates "our crashed shell's
#     child" from "a writer a human is actively using".
# ============================================================================
d="$(scenario_dir s12)"
MOCK_PORT=8899
handrun="$(spawn_mock "${BIN_DIR}/aberp" test "$d" --mock-orphan --mock-detach)"
MOCK_PORT=0
if [[ "$(ps -o ppid= -p "$handrun" | tr -d ' ')" == "1" ]]; then
  pass "hand-started setup: holder is at ppid 1 and differs ONLY in its port"
else
  fail "hand-started setup: holder ppid is not 1 — scenario invalid"
fi
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "2" ]]; then
  pass "orphan on a real port: predicate reports AMBIGUOUS (rc 2)"
else
  fail "orphan on a real port: expected rc 2, got $(predicate_rc "${d}/aberp.duckdb" test)"
fi
reap_quiet "$TMP" "${d}/aberp.duckdb" test
if alive "$handrun"; then
  pass "orphan on a real port: NOT killed"
else
  fail "orphan on a real port pid ${handrun} WAS KILLED — a live writer died"
fi
kill -KILL "$handrun" 2>/dev/null

# ============================================================================
# 13. SIGKILL is re-attributed, not fired on a bare `kill -0`.
#     `kill -0` proves only that SOME process owns the pid. If the pid we
#     SIGTERM'd exits during the wait and the kernel recycles it, the escalation
#     would SIGKILL whatever inherited it — the one operation here that cannot
#     be undone. Real recycling cannot be provoked on demand, so the mock execs
#     /bin/sleep on SIGTERM: same pid, still holding the flock, no longer the
#     process we attributed. That is precisely what the guard can see, and the
#     escalation must refuse it.
# ============================================================================
d="$(scenario_dir s13)"
morph="$(spawn_mock "${BIN_DIR}/aberp" test "$d" --mock-orphan --mock-detach --mock-morph-on-term)"
if [[ "$(predicate_rc "${d}/aberp.duckdb" test)" == "0" ]]; then
  pass "recycle setup: holder qualifies before the signal (rc 0)"
else
  fail "recycle setup: holder does not qualify — scenario cannot prove the guard"
fi
out="$(reap_stale_orphan_writer "$TMP" "${d}/aberp.duckdb" test 2>&1)"
if [[ "$(ps -ww -o args= -p "$morph" 2>/dev/null)" == *sleep* ]]; then
  pass "recycle setup: pid ${morph} changed identity during the SIGTERM wait"
else
  fail "recycle setup: pid ${morph} did not morph — scenario invalid (argv: $(ps -ww -o args= -p "$morph" 2>/dev/null))"
fi
if alive "$morph"; then
  pass "recycled pid: NOT SIGKILLed — escalation re-attributed first"
else
  fail "recycled pid ${morph} WAS SIGKILLed — the escalation trusts a stale attribution"
fi
if echo "$out" | grep -q "no longer the stale orphan — NOT escalating"; then
  pass "recycled pid: the refusal is announced, not silent"
else
  fail "recycled pid: no escalation-refusal line — output: ${out}"
fi
# ...and the reap still reports the honest verdict rather than claiming success.
if echo "$out" | grep -q "STILL held after the reap attempt"; then
  pass "recycled pid: reap reports the lock as STILL held (boot falls through)"
else
  fail "recycled pid: reap did not report the still-held lock — output: ${out}"
fi
kill -KILL "$morph" 2>/dev/null

# ---------- result ----------------------------------------------------------
echo
if [[ $fails -eq 0 ]]; then
  echo "[pass] all S445 stale-orphan writer-reap assertions passed"
  exit 0
fi
echo "[fail] ${fails} assertion(s) failed" >&2
exit 1
