# shellcheck shell=bash
#
# aberp_launch_agent.sh — ADR-0111 addendum 2 — sourced by run/run_prod.sh and
# run/upgrade_prod.sh. Pure helpers; nothing runs at source time.
#
# The MacBook's prod backend runs under the LaunchAgent `ch.aben.aberp.serve`
# (run/launchd/ch.aben.aberp.serve.plist). When it does, the prod window must
# ATTACH to it (ABERP_REMOTE_URL=https://127.0.0.1:18443 + the pinned
# fingerprint) instead of spawning a second backend, which the single-writer
# lock would refuse. That decision belongs to the code, not the operator
# ([[trust-code-not-operator]]): `aberp_agent_decide` returns exactly one of
#
#   spawn          no agent installed or loaded → behave exactly as before 0111
#   start          agent installed (plist in ~/Library/LaunchAgents) but not
#                  loaded, e.g. upgrade_prod.sh stopped it for the swap → the
#                  caller builds aberp, starts the agent, then decides again
#   attach         agent loaded, /health ok, and the agent runs THIS checkout's
#                  target/release/aberp (binary_hash == sha256 of the file)
#   fail:<reason>  agent loaded but not usable → fail closed, never spawn
#
# Test seams: ABERP_AGENT_UID, ABERP_AGENT_PLIST_DIR, ABERP_AGENT_HEALTH_TRIES
# and ABERP_AGENT_HEALTH_SLEEP let run/tests/run_prod_launch_agent_test.sh
# drive the real functions with stubbed `launchctl` / `curl` on PATH.

ABERP_AGENT_LABEL="ch.aben.aberp.serve"
# Must equal the `--port` in run/launchd/ch.aben.aberp.serve.plist (pinned by
# run/tests/run_prod_launch_agent_test.sh).
ABERP_AGENT_LOOPBACK_PORT="18443"

aberp_agent_uid() { echo "${ABERP_AGENT_UID:-$(id -u)}"; }
aberp_agent_target() { echo "gui/$(aberp_agent_uid)/${ABERP_AGENT_LABEL}"; }
aberp_agent_plist_installed_path() {
  echo "${ABERP_AGENT_PLIST_DIR:-${HOME}/Library/LaunchAgents}/${ABERP_AGENT_LABEL}.plist"
}
aberp_agent_url() { echo "https://127.0.0.1:${ABERP_AGENT_LOOPBACK_PORT}"; }

# Installed = the plist is in ~/Library/LaunchAgents (it is then loaded at
# every login). Loaded = launchd knows the job right now.
aberp_agent_installed() { [[ -f "$(aberp_agent_plist_installed_path)" ]]; }
aberp_agent_loaded() { launchctl print "$(aberp_agent_target)" >/dev/null 2>&1; }

# Unauthenticated loopback /health body, or empty on any failure. `-k` is
# fine for a liveness probe: the window itself pins the fingerprint.
aberp_agent_health() {
  curl -sk --max-time 3 "$(aberp_agent_url)/health" 2>/dev/null || true
}

# Poll /health until it reports ok, up to TRIES × SLEEP seconds (the agent may
# still be booting, e.g. just after login or a kickstart). Prints the last body.
aberp_agent_wait_healthy() {
  local tries="${ABERP_AGENT_HEALTH_TRIES:-30}" pause="${ABERP_AGENT_HEALTH_SLEEP:-1}" body="" i
  for ((i = 0; i < tries; i++)); do
    body="$(aberp_agent_health)"
    if [[ "$body" == *'"ok":true'* ]]; then
      printf '%s' "$body"
      return 0
    fi
    sleep "$pause"
  done
  printf '%s' "$body"
  return 1
}

aberp_agent_decide() {
  local repo_root="$1" fingerprint_file="$2" body reported on_disk
  if ! aberp_agent_loaded; then
    if aberp_agent_installed; then
      echo "start"
    else
      echo "spawn"
    fi
    return 0
  fi
  if ! body="$(aberp_agent_wait_healthy)"; then
    echo "fail:unhealthy"
    return 0
  fi
  if [[ ! -s "$fingerprint_file" ]]; then
    echo "fail:no-fingerprint"
    return 0
  fi
  reported="$(printf '%s' "$body" | sed -n 's/.*"binary_hash":"\([0-9a-f]\{64\}\)".*/\1/p')"
  if [[ ! -f "${repo_root}/target/release/aberp" ]]; then
    echo "fail:no-binary"
    return 0
  fi
  on_disk="$(shasum -a 256 "${repo_root}/target/release/aberp" | cut -c1-64)"
  if [[ -z "$reported" || "$reported" != "$on_disk" ]]; then
    echo "fail:version-skew"
    return 0
  fi
  echo "attach"
}

# Operator-facing explanation for a `fail:<reason>` decision (EN + HU).
aberp_agent_fail_message() {
  local reason="${1#fail:}" target log="${HOME}/Library/Logs/aberp-serve.log"
  target="$(aberp_agent_target)"
  case "$reason" in
    unhealthy)
      echo "the ABERP backend LaunchAgent (${ABERP_AGENT_LABEL}) is loaded but not healthy on $(aberp_agent_url).
   Not starting a second backend (the DB has exactly one writer).
   Look at: tail -50 ${log}
   Restart it: launchctl kickstart -k ${target}
HU: A háttér-szolgáltatás be van töltve, de nem válaszol. Második backendet nem indítok." ;;
    version-skew)
      echo "the ABERP backend LaunchAgent runs a different aberp build than this checkout.
   Upgrade properly instead: ./run/upgrade_prod.sh <PROD_vX.Y> (it restarts the agent on the new build).
HU: A háttér-szolgáltatás más verziót futtat, mint ez a checkout — használd az upgrade_prod.sh-t." ;;
    no-fingerprint)
      echo "the ABERP backend LaunchAgent is healthy but its TLS fingerprint file is missing,
   so the window cannot pin it. Restart it: launchctl kickstart -k ${target}
HU: Hiányzik a TLS ujjlenyomat-fájl — indítsd újra a szolgáltatást." ;;
    no-binary)
      echo "the ABERP backend LaunchAgent is healthy but this checkout has no target/release/aberp to compare it with.
   Upgrade properly instead: ./run/upgrade_prod.sh <PROD_vX.Y>
HU: Ebben a checkoutban nincs target/release/aberp — használd az upgrade_prod.sh-t." ;;
    *)
      echo "the ABERP backend LaunchAgent is in an unexpected state (${reason})." ;;
  esac
}

# Stop the agent and wait until its process is gone (graceful SIGTERM drain).
aberp_agent_stop() {
  local repo_root="$1" i
  launchctl bootout "$(aberp_agent_target)" >/dev/null 2>&1 || true
  for ((i = 0; i < 30; i++)); do
    pgrep -f "${repo_root}/target/release/aberp serve" >/dev/null 2>&1 || return 0
    sleep 1
  done
  return 1
}

# (Re)install the plist from THIS checkout and start the agent.
aberp_agent_start() {
  local repo_root="$1" dest
  dest="$(aberp_agent_plist_installed_path)"
  cp "${repo_root}/run/launchd/${ABERP_AGENT_LABEL}.plist" "$dest" || return 1
  launchctl bootstrap "gui/$(aberp_agent_uid)" "$dest"
}
