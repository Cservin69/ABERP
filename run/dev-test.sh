#!/usr/bin/env bash
#
# dev-test.sh
#
# S291 / PR-272 — unified local-loopback dev-test launcher.
#
# WHY
#   Pre-S291 the end-to-end auto-quote test path required FIVE separate
#   manual operator-discipline steps for every relaunch (the gap
#   `feedback_local_dev_test_path_gaps` documents from Ervin's
#   2026-06-08 evening test):
#
#     1. `lsof -iTCP:LISTEN | grep aberp` to find the dynamic port
#     2. Hand-edit the SPA's storefront URL
#     3. `openssl rand` + `security add-generic-password` for the
#        email-relay keychain entry
#     4. Five env vars on the storefront `npm run dev` launch
#     5. ABERP restart on every storefront port-reassignment
#
#   Per CLAUDE.md rule 12 ([[trust-code-not-operator]]) safety belongs
#   in code, not in operator memory. This launcher does all five in one
#   shell invocation; the supporting Rust changes (S291 in `serve.rs` +
#   `runtime_discovery.rs` + `storefront_credential.rs`) make the
#   matching env vars work.
#
# WHAT
#   1. Provision the email-relay keychain entry if missing (one
#      `security add-generic-password` call, idempotent; the first run
#      shows the macOS "Allow keychain access" prompt — by design).
#   2. Pin ABERP to `ABERP_HTTPS_PORT=${ABERP_HTTPS_PORT:-18443}`. If
#      the port is taken by a non-ABERP process, bail with a hint.
#   3. Set `ABERP_SISTER_SERVICE_BASE_URL=http://localhost:5173` and
#      `ABERP_DEV_MODE=1` so S289's prod-URL warning fires if the URL
#      ever drifts.
#   4. Launch ABERP via tauri-CLI in the background (same shape as
#      `run_desktop.sh`); wait for the discovery file to appear.
#   5. Locate the sister storefront repo (`ABERP-site` next to this
#      checkout, or `$ABERP_SITE_DIR` override). Bail if missing.
#   6. Read `~/.aberp/<tenant>/runtime.json`; export `ABERP_INTERNAL_BASE_URL`
#      + `ABERP_INTERNAL_TLS_FINGERPRINT` + bearer from keychain.
#   7. `npm install` (idempotent) + `npm run dev` storefront in
#      background.
#   8. After a few seconds, open `http://localhost:5173/quote` in the
#      default browser.
#   9. Ctrl-C: SIGTERM both PIDs; wait; SIGKILL stragglers; clean exit.
#
# USAGE
#   ./run/dev-test.sh                            # tenant=test, port=18443
#   ./run/dev-test.sh --tenant test              # explicit
#   ./run/dev-test.sh --port 19443               # override the pinned port
#   ./run/dev-test.sh --no-browser               # skip the auto-open
#   ABERP_SITE_DIR=/abs/path ./run/dev-test.sh   # storefront elsewhere
#
# NON-GOALS
#   - Cloudflare Tunnel: that lives in a separate runbook; this is
#     local-loopback only.
#   - Production: this launcher is dev-only and refuses tenant=prod.
#   - Replacing the keychain provisioning UX: first-launch popup is the
#     macOS standard. The launcher idempotently no-ops on re-runs.

set -uo pipefail

# ---------- self-syntax-check (mirrors run_desktop.sh PR-55) ---------------
if ! bash -n "$0" 2>/dev/null; then
  echo "[fail] $0 failed 'bash -n' syntax check — refusing to run" >&2
  bash -n "$0"
  exit 2
fi

# ---------- config -------------------------------------------------------
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
readonly DEFAULT_TENANT="test"
readonly DEFAULT_PORT="18443"
readonly STOREFRONT_PORT="5173"
readonly STOREFRONT_URL="http://localhost:${STOREFRONT_PORT}"
readonly KEYCHAIN_ACCOUNT="email_relay_token"   # mirrors email_relay_credentials::ITEM_EMAIL_RELAY_TOKEN
readonly DISCOVERY_WAIT_SECS=30                 # how long to wait for runtime.json
readonly SHUTDOWN_TIMEOUT_SECS=15

# ---------- arg parsing -------------------------------------------------
tenant="${ABERP_TENANT:-$DEFAULT_TENANT}"
port="${ABERP_HTTPS_PORT:-$DEFAULT_PORT}"
open_browser=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tenant)      tenant="$2"; shift 2 ;;
    --port)        port="$2"; shift 2 ;;
    --no-browser)  open_browser=0; shift ;;
    --help|-h)
      sed -n '2,55p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "[fail] unknown arg: $1" >&2
      echo "       try --help" >&2
      exit 2
      ;;
  esac
done

# ---------- prod-tenant refuse (mirror run_desktop.sh) ------------------
if [[ "${tenant}" == "prod" ]]; then
  echo "[fail] dev-test.sh refuses tenant=prod — this is a DEV-only launcher" >&2
  exit 1
fi

# ---------- preflight ---------------------------------------------------
if ! [[ "${port}" =~ ^[0-9]+$ ]] || (( port < 1024 || port > 65535 )); then
  echo "[fail] --port must be an integer in [1024, 65535]; got '${port}'" >&2
  exit 2
fi

readonly KEYCHAIN_SERVICE="aberp.email_relay.${tenant}"
readonly DISCOVERY_FILE="${HOME}/.aberp/${tenant}/runtime.json"

# Locate the storefront sibling. Override > sibling next to ABERP > giveup.
storefront_dir="${ABERP_SITE_DIR:-}"
if [[ -z "${storefront_dir}" ]]; then
  # Sibling search: ABERP-site next to the repo root.
  candidate="$(cd "${REPO_ROOT}/.." 2>/dev/null && pwd)/ABERP-site"
  if [[ -d "${candidate}" ]]; then
    storefront_dir="${candidate}"
  fi
fi
if [[ -z "${storefront_dir}" ]] || [[ ! -d "${storefront_dir}" ]]; then
  echo "[fail] sister storefront repo not found." >&2
  echo "       Looked at: \${ABERP_SITE_DIR}='${ABERP_SITE_DIR:-}'" >&2
  echo "       And: ${REPO_ROOT}/../ABERP-site" >&2
  echo "       Set ABERP_SITE_DIR=/absolute/path to the storefront checkout." >&2
  exit 2
fi
readonly STOREFRONT_DIR="${storefront_dir}"

# Required tools — fail loud if missing instead of mid-flight.
for tool in security lsof curl; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "[fail] required tool not on PATH: ${tool}" >&2
    exit 2
  fi
done
if ! command -v openssl >/dev/null 2>&1; then
  # openssl is only needed if we need to mint a new token.
  echo "[warn] openssl not on PATH — if no keychain token exists yet, this will fail later" >&2
fi

# ---------- ORIGINAL_CWD + cleanup --------------------------------------
ORIGINAL_CWD="$(pwd)"
readonly ORIGINAL_CWD
aberp_pid=""
storefront_pid=""

cleanup() {
  local rc=$?
  trap - EXIT INT TERM HUP
  echo
  echo "[shutdown] forwarding SIGTERM to launched children (rc=${rc})"

  if [[ -n "${storefront_pid}" ]] && kill -0 "${storefront_pid}" 2>/dev/null; then
    echo "[shutdown] storefront pid=${storefront_pid} → SIGTERM"
    kill -TERM "${storefront_pid}" 2>/dev/null || true
  fi
  if [[ -n "${aberp_pid}" ]] && kill -0 "${aberp_pid}" 2>/dev/null; then
    echo "[shutdown] aberp pid=${aberp_pid} → SIGTERM"
    kill -TERM "${aberp_pid}" 2>/dev/null || true
  fi

  # Give children up to SHUTDOWN_TIMEOUT_SECS to drain.
  local waited=0
  while [[ ${waited} -lt ${SHUTDOWN_TIMEOUT_SECS} ]]; do
    local alive=0
    if [[ -n "${storefront_pid}" ]] && kill -0 "${storefront_pid}" 2>/dev/null; then alive=1; fi
    if [[ -n "${aberp_pid}" ]] && kill -0 "${aberp_pid}" 2>/dev/null; then alive=1; fi
    [[ ${alive} -eq 0 ]] && break
    sleep 1
    waited=$((waited + 1))
  done

  # SIGKILL stragglers.
  if [[ -n "${storefront_pid}" ]] && kill -0 "${storefront_pid}" 2>/dev/null; then
    echo "[shutdown] storefront pid=${storefront_pid} did not exit — SIGKILL"
    kill -KILL "${storefront_pid}" 2>/dev/null || true
  fi
  if [[ -n "${aberp_pid}" ]] && kill -0 "${aberp_pid}" 2>/dev/null; then
    echo "[shutdown] aberp pid=${aberp_pid} did not exit — SIGKILL"
    kill -KILL "${aberp_pid}" 2>/dev/null || true
  fi

  # Belt-and-suspenders on storefront port.
  if command -v lsof >/dev/null 2>&1; then
    local stragglers
    stragglers="$(lsof -tiTCP:${STOREFRONT_PORT} -sTCP:LISTEN 2>/dev/null || true)"
    if [[ -n "${stragglers}" ]]; then
      echo "[shutdown] :${STOREFRONT_PORT} still held by ${stragglers} — killing"
      # shellcheck disable=SC2086
      kill -KILL ${stragglers} 2>/dev/null || true
    fi
  fi

  cd "${ORIGINAL_CWD}" 2>/dev/null || true
  echo "[shutdown] done."
  exit "${rc}"
}
trap cleanup EXIT INT TERM HUP

# ---------- step 1: keychain provisioning ------------------------------
echo "[step 1/9] check keychain entry: service=${KEYCHAIN_SERVICE} account=${KEYCHAIN_ACCOUNT}"
existing_token=""
if existing_token="$(security find-generic-password \
        -s "${KEYCHAIN_SERVICE}" \
        -a "${KEYCHAIN_ACCOUNT}" \
        -w 2>/dev/null)"; then
  echo "       keychain entry already present (${#existing_token} bytes) — reusing."
else
  if ! command -v openssl >/dev/null 2>&1; then
    echo "[fail] openssl required to mint a new email-relay token, but not on PATH" >&2
    exit 2
  fi
  echo "       no entry; minting a 32-byte token and writing to the keychain."
  echo "       (macOS may prompt for keychain access — click Allow.)"
  new_token="$(openssl rand -hex 32)"
  # -U overwrites any pre-existing entry; -T '' allows any process to
  # read (dev only — the laptop is single-user). For prod we'd scope to
  # the aberp binary, but this is the dev-only launcher.
  if ! security add-generic-password \
        -s "${KEYCHAIN_SERVICE}" \
        -a "${KEYCHAIN_ACCOUNT}" \
        -w "${new_token}" \
        -U \
        2>/dev/null; then
    echo "[fail] security add-generic-password failed" >&2
    exit 3
  fi
  existing_token="${new_token}"
  unset new_token
fi

# ---------- step 2: port preflight -------------------------------------
echo "[step 2/9] check ABERP port ${port}"
port_holder="$(lsof -tiTCP:"${port}" -sTCP:LISTEN 2>/dev/null | head -n1 || true)"
if [[ -n "${port_holder}" ]]; then
  # Could be a prior ABERP we want to reuse. Conservative: bail with a hint.
  port_cmd="$(ps -p "${port_holder}" -o comm= 2>/dev/null || echo unknown)"
  echo "[fail] port ${port} already held by pid=${port_holder} (${port_cmd})." >&2
  echo "       Pick another port with --port, or kill the holder." >&2
  exit 2
fi

# ---------- step 3: storefront port preflight ---------------------------
echo "[step 3/9] check storefront port ${STOREFRONT_PORT}"
storefront_holder="$(lsof -tiTCP:${STOREFRONT_PORT} -sTCP:LISTEN 2>/dev/null | head -n1 || true)"
if [[ -n "${storefront_holder}" ]]; then
  # Stale dev server from a prior run — kill it (storefront has no
  # operator state worth preserving on the dev box).
  echo "       storefront port held by pid=${storefront_holder} (stale) — killing"
  kill -TERM "${storefront_holder}" 2>/dev/null || true
  sleep 1
  kill -KILL "${storefront_holder}" 2>/dev/null || true
fi

# ---------- step 4: clear stale discovery file --------------------------
echo "[step 4/9] remove stale discovery file at ${DISCOVERY_FILE}"
rm -f "${DISCOVERY_FILE}" 2>/dev/null || true

# ---------- step 5: launch ABERP ----------------------------------------
echo "[step 5/9] launch ABERP (tenant=${tenant}, port=${port}, dev-mode=1)"
export ABERP_TENANT="${tenant}"
export ABERP_HTTPS_PORT="${port}"
export ABERP_SISTER_SERVICE_BASE_URL="${STOREFRONT_URL}"
export ABERP_DEV_MODE="1"

# We launch via run_desktop.sh so the working Tauri 2 shape from PR-52
# (double-build + tauri-CLI dev) is reused. The dev launcher does not
# accept --port; ABERP_HTTPS_PORT carries the value through.
"${REPO_ROOT}/run/run_desktop.sh" --tenant "${tenant}" &
aberp_pid=$!
echo "       aberp pid=${aberp_pid}"

# ---------- step 6: wait for discovery file -----------------------------
echo "[step 6/9] wait up to ${DISCOVERY_WAIT_SECS}s for ${DISCOVERY_FILE}"
waited=0
while [[ ! -f "${DISCOVERY_FILE}" ]]; do
  if ! kill -0 "${aberp_pid}" 2>/dev/null; then
    echo "[fail] aberp pid=${aberp_pid} exited before writing the discovery file" >&2
    exit 4
  fi
  if [[ ${waited} -ge ${DISCOVERY_WAIT_SECS} ]]; then
    echo "[fail] discovery file did not appear within ${DISCOVERY_WAIT_SECS}s" >&2
    echo "       check ABERP's log; the listener may have failed to bind." >&2
    exit 4
  fi
  sleep 1
  waited=$((waited + 1))
done
echo "       discovery file appeared after ${waited}s"

# ---------- step 7: parse discovery + cd to storefront ------------------
echo "[step 7/9] parse discovery + prepare storefront env"
# Minimal JSON read — `jq` is preferred but not always installed on a
# fresh laptop; fall back to a python one-liner. The discovery file
# shape is fixed (see runtime_discovery.rs) so a simple key=value match
# is fine.
read_discovery_key() {
  local key="$1"
  if command -v jq >/dev/null 2>&1; then
    jq -r --arg k "${key}" '.[$k] // empty' "${DISCOVERY_FILE}"
  else
    python3 -c "import json,sys; print(json.load(open('${DISCOVERY_FILE}')).get('${key}',''))"
  fi
}
internal_base_url="$(read_discovery_key base_url)"
internal_fingerprint="$(read_discovery_key tls_fingerprint)"
discovery_tenant="$(read_discovery_key tenant)"
if [[ -z "${internal_base_url}" ]] || [[ -z "${internal_fingerprint}" ]]; then
  echo "[fail] discovery file is missing base_url or tls_fingerprint" >&2
  cat "${DISCOVERY_FILE}" >&2 || true
  exit 5
fi
if [[ "${discovery_tenant}" != "${tenant}" ]]; then
  echo "[fail] discovery tenant '${discovery_tenant}' != requested '${tenant}' — stale file?" >&2
  exit 5
fi
echo "       base_url=${internal_base_url}"
echo "       tls_fingerprint=${internal_fingerprint:0:16}…"

# ---------- step 8: launch storefront -----------------------------------
echo "[step 8/9] launch storefront at ${STOREFRONT_DIR}"
cd "${STOREFRONT_DIR}" || { echo "[fail] cd ${STOREFRONT_DIR} failed" >&2; exit 5; }
if [[ ! -d node_modules ]]; then
  echo "       node_modules/ missing — running npm install (one-time, may take a minute)"
  npm install --no-audit --no-fund || { echo "[fail] npm install failed" >&2; exit 5; }
else
  echo "       node_modules/ present — skipping npm install"
fi

# Storefront env — these names mirror what the storefront's
# `lib/server/aberp.ts` reads for the internal sister-service call.
# The five vars that pre-S291 had to be hand-set per launch:
export ABERP_INTERNAL_BASE_URL="${internal_base_url}"
export ABERP_INTERNAL_TLS_FINGERPRINT="${internal_fingerprint}"
export ABERP_INTERNAL_BEARER="${existing_token}"
export ABERP_TENANT="${tenant}"
export NODE_TLS_REJECT_UNAUTHORIZED="0"   # loopback self-signed cert; pin enforced via fingerprint
unset existing_token

npm run dev &
storefront_pid=$!
echo "       storefront pid=${storefront_pid}"

# ---------- step 9: open browser ----------------------------------------
echo "[step 9/9] wait a few seconds then open browser"
# Give Vite some time to bind :5173 before we open the page.
sleep 5
if [[ ${open_browser} -eq 1 ]]; then
  if command -v open >/dev/null 2>&1; then
    open "${STOREFRONT_URL}/quote" || true
  else
    echo "       'open' not on PATH; visit ${STOREFRONT_URL}/quote manually"
  fi
else
  echo "       --no-browser: visit ${STOREFRONT_URL}/quote manually"
fi

cat <<EOF

[ready] both processes are running:
  - ABERP        pid=${aberp_pid}  https://127.0.0.1:${port}
  - storefront   pid=${storefront_pid}  ${STOREFRONT_URL}
  - tenant       ${tenant}
  - discovery    ${DISCOVERY_FILE}

Ctrl-C to shut down both cleanly.

EOF

# Wait on the FIRST child to exit (whichever dies first triggers cleanup).
# `wait -n` is bash-4+; macOS ships 3.2, so fall back to a poll loop.
while true; do
  if ! kill -0 "${aberp_pid}" 2>/dev/null; then
    echo "[notice] aberp exited"
    break
  fi
  if ! kill -0 "${storefront_pid}" 2>/dev/null; then
    echo "[notice] storefront exited"
    break
  fi
  sleep 2
done
