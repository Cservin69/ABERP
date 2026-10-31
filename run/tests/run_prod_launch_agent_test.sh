#!/usr/bin/env bash
#
# run_prod_launch_agent_test.sh — ADR-0111 addendum 2
#
# Pins the decision run/run_prod.sh takes about the always-on LaunchAgent
# backend (ch.aben.aberp.serve), using the REAL functions from
# run/lib/aberp_launch_agent.sh with `launchctl` and `curl` stubbed on PATH:
#
#   1. no agent installed/loaded            → spawn  (pre-0111 behaviour)
#   2. agent installed, not loaded          → start  (upgrade stopped it)
#   3. loaded + healthy + same binary       → attach
#   4. loaded + /health not ok              → fail:unhealthy   (never spawn)
#   5. loaded + healthy + different binary  → fail:version-skew
#   6. loaded + healthy, fingerprint absent → fail:no-fingerprint
#   7. the shipped plist agrees with the helper (label, loopback port, bridge
#      bind/peer, ~/ABERP binary path)
#   8. run_prod.sh and upgrade_prod.sh actually source the helper
#   9. every fail reason has an operator message
#
# Exit 0 = all pass; non-zero = failure.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
RUN_DIR="${SCRIPT_DIR}/.."
LIB="${RUN_DIR}/lib/aberp_launch_agent.sh"
PLIST="${RUN_DIR}/launchd/ch.aben.aberp.serve.plist"

fails=0
pass() { echo "[pass] $*"; }
fail() { echo "[FAIL] $*" >&2; fails=$((fails + 1)); }

work="$(mktemp -d "${TMPDIR:-/tmp}/aberp-agent-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/agents" "$work/repo/target/release"
printf 'fake aberp binary\n' > "$work/repo/target/release/aberp"
on_disk_hash="$(shasum -a 256 "$work/repo/target/release/aberp" | cut -c1-64)"
fp_file="$work/fingerprint"
printf 'ab%.0s' $(seq 1 32) > "$fp_file"

# Stubs: `launchctl print` succeeds iff FAKE_LOADED=1; `curl` prints FAKE_HEALTH.
cat > "$work/bin/launchctl" <<'EOF'
#!/usr/bin/env bash
[[ "$1" == "print" && "${FAKE_LOADED:-0}" == "1" ]]
EOF
cat > "$work/bin/curl" <<'EOF'
#!/usr/bin/env bash
printf '%s' "${FAKE_HEALTH:-}"
EOF
chmod +x "$work/bin/launchctl" "$work/bin/curl"

export PATH="$work/bin:$PATH"
export ABERP_AGENT_UID=501 ABERP_AGENT_PLIST_DIR="$work/agents"
export ABERP_AGENT_HEALTH_TRIES=2 ABERP_AGENT_HEALTH_SLEEP=0
# shellcheck source=../lib/aberp_launch_agent.sh
source "$LIB"

healthy() { echo "{\"ok\":true,\"binary_hash\":\"$1\",\"is_production_build\":true}"; }
expect() {
  local want="$1" label="$2" got
  got="$(aberp_agent_decide "$work/repo" "$fp_file")"
  if [[ "$got" == "$want" ]]; then pass "$label → $got"; else fail "$label: want $want, got $got"; fi
}

FAKE_LOADED=0 FAKE_HEALTH="" expect spawn "1 no agent"

touch "$work/agents/ch.aben.aberp.serve.plist"
FAKE_LOADED=0 FAKE_HEALTH="" expect start "2 installed, not loaded"

FAKE_LOADED=1 FAKE_HEALTH="$(healthy "$on_disk_hash")" expect attach "3 loaded, healthy, same binary"
FAKE_LOADED=1 FAKE_HEALTH="" expect fail:unhealthy "4 loaded, no /health answer"
FAKE_LOADED=1 FAKE_HEALTH='{"ok":false}' expect fail:unhealthy "4b loaded, /health not ok"
FAKE_LOADED=1 FAKE_HEALTH="$(healthy "$(printf '0%.0s' $(seq 1 64))")" expect fail:version-skew "5 loaded, different binary"

mv "$fp_file" "$fp_file.away"
FAKE_LOADED=1 FAKE_HEALTH="$(healthy "$on_disk_hash")" expect fail:no-fingerprint "6 fingerprint file missing"
mv "$fp_file.away" "$fp_file"

# 7. The shipped plist must agree with the helper.
plist_val() { /usr/bin/plutil -extract "$1" raw -o - "$PLIST" 2>/dev/null; }
if [[ "$(plist_val Label)" == "$ABERP_AGENT_LABEL" ]]; then pass "7 plist Label = $ABERP_AGENT_LABEL"; else fail "7 plist Label != $ABERP_AGENT_LABEL"; fi
port="$(plist_val ProgramArguments.7)"
if [[ "$(plist_val ProgramArguments.6)" == "--port" && "$port" == "$ABERP_AGENT_LOOPBACK_PORT" ]]; then pass "7 plist --port = $port"; else fail "7 plist --port ($port) != $ABERP_AGENT_LOOPBACK_PORT"; fi
if [[ "$(plist_val ProgramArguments.0)" == "/Users/aben/ABERP/target/release/aberp" ]]; then pass "7 plist runs ~/ABERP release binary"; else fail "7 plist binary path"; fi
if [[ "$(plist_val EnvironmentVariables.ABERP_REMOTE_BIND)" == "10.69.0.1:8443" && "$(plist_val EnvironmentVariables.ABERP_REMOTE_PEER)" == "10.69.0.2" ]]; then pass "7 plist carries the bridge bind + peer"; else fail "7 plist bridge bind/peer"; fi

# 8. Both launchers source the helper (no drifting copy).
for s in run_prod.sh upgrade_prod.sh; do
  if grep -q 'source "${REPO_ROOT}/run/lib/aberp_launch_agent.sh"' "${RUN_DIR}/$s"; then pass "8 $s sources the helper"; else fail "8 $s does not source the helper"; fi
done

# 9. Every fail reason produces an operator message.
for r in unhealthy version-skew no-fingerprint no-binary; do
  if [[ -n "$(aberp_agent_fail_message "fail:$r")" ]]; then pass "9 message for $r"; else fail "9 no message for $r"; fi
done

# 10. The REAL run_prod.sh, end to end up to the build step, in a throwaway
# repo that passes its Frankenstein guard. `npm` is shadowed to fail, so a run
# that gets past the agent decision stops at exit 4 before building anything.
repo="$work/e2e"
mkdir -p "$repo/run/lib" "$repo/apps/aberp-ui/ui" "$repo/target/release" "$work/home/.aberp/prod" "$work/home/.aberp/serve/prod"
cp "${RUN_DIR}/run_prod.sh" "$repo/run/run_prod.sh"
cp "$LIB" "$repo/run/lib/aberp_launch_agent.sh"
printf '/target\n' > "$repo/.gitignore"
(
  cd "$repo" || exit 1
  git init -q . && git config user.email t@example.invalid && git config user.name t
  git add -A && git commit -q -m base && git update-ref refs/remotes/origin/PROD_v9.9 HEAD
) || fail "10 could not build the fixture repo"
printf 'fake aberp binary\n' > "$repo/target/release/aberp"
printf 'ab%.0s' $(seq 1 32) > "$work/home/.aberp/serve/prod/loopback.fingerprint.sha256"
cat > "$work/bin/npm" <<'EOF'
#!/usr/bin/env bash
echo "npm-reached" >&2
exit 1
EOF
chmod +x "$work/bin/npm"
run_real() { (cd "$repo" && HOME="$work/home" NO_COLOR=1 bash ./run/run_prod.sh 2>&1); }

out="$(FAKE_LOADED=1 FAKE_HEALTH="" run_real)"; rc=$?
if [[ $rc -eq 3 && "$out" == *"not healthy"* && "$out" != *"npm-reached"* ]]; then
  pass "10 loaded+unhealthy agent: run_prod.sh fails closed (rc 3) before any build/spawn"
else
  fail "10 loaded+unhealthy: rc=$rc, output: $(printf '%s' "$out" | tail -3)"
fi

out="$(FAKE_LOADED=1 FAKE_HEALTH="$(healthy "$on_disk_hash")" run_real)"; rc=$?
if [[ "$out" == *"[attach]"* && "$out" == *"npm-reached"* ]]; then
  pass "10 loaded+healthy agent: run_prod.sh decides attach and proceeds to the build"
else
  fail "10 loaded+healthy: rc=$rc, output: $(printf '%s' "$out" | tail -3)"
fi

out="$(FAKE_LOADED=0 FAKE_HEALTH="" run_real)"; rc=$?
if [[ "$out" == *"[agent]"*"installed but stopped"* && "$out" == *"npm-reached"* ]]; then
  pass "10 installed but stopped agent: run_prod.sh plans the restart and proceeds to the build"
else
  fail "10 installed+stopped: rc=$rc, output: $(printf '%s' "$out" | tail -3)"
fi

rm -f "$work/agents/ch.aben.aberp.serve.plist"
out="$(FAKE_LOADED=0 FAKE_HEALTH="" run_real)"; rc=$?
if [[ "$out" != *"[attach]"* && "$out" != *"[agent]"* && "$out" == *"npm-reached"* ]]; then
  pass "10 no agent: run_prod.sh behaves as before (no attach, no agent handling)"
else
  fail "10 no agent: rc=$rc, output: $(printf '%s' "$out" | tail -3)"
fi

if ((fails > 0)); then
  echo "$fails failure(s)" >&2
  exit 1
fi
echo "all launch-agent checks passed"
