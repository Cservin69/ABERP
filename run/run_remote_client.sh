#!/usr/bin/env bash
#
# run_remote_client.sh — ADR-0111 — launch the ABERP shell in REMOTE mode.
#
# This machine holds NO tenant DB. The shell connects over the Thunderbolt
# Bridge to the `aberp serve` running on the machine that owns the DB, so
# invoices issued here use the same DB, numbering series and audit chain.
#
# Usage:
#   ./run/run_remote_client.sh [--tenant <slug>]      (default tenant: prod)
#   ./run/run_remote_client.sh --help
#
# tenant=prod builds the shell with --features production (the backend
# refuses a profile mismatch at /remote/hello); any other tenant builds dev.
#
# One-time provisioning on THIS machine (values come from the DB machine):
#   1. ~/.aberp/remote/<tenant>.env, mode 600, containing:
#        ABERP_REMOTE_URL=https://10.69.0.1:8443
#        ABERP_REMOTE_FINGERPRINT=<contents of ~/.aberp/serve/<tenant>/loopback.fingerprint.sha256>
#   2. the remote bearer in this machine's keychain:
#        security add-generic-password -U -s aberp.nav.<tenant> -a remote_session_token -w
#      (prompts for the value; read it on the DB machine with
#        security find-generic-password -s aberp.nav.<tenant> -a remote_session_token -w )
#
# Revoking this machine: on the DB machine delete that keychain item and
# restart ABERP there; a fresh token is minted and this one stops working.

set -euo pipefail

if ! bash -n "$0" 2>/dev/null; then
  echo "[fail] $0 failed 'bash -n' syntax check — refusing to run" >&2
  bash -n "$0"
  exit 2
fi

readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

tenant="prod"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tenant) tenant="${2:?--tenant needs a value}"; shift 2 ;;
    --help|-h) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "[fail] unknown flag: $1" >&2; exit 2 ;;
  esac
done

readonly ENV_FILE="${HOME}/.aberp/remote/${tenant}.env"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "[fail] $ENV_FILE is missing — see --help for the one-time provisioning." >&2
  exit 3
fi
if [[ "$(stat -f '%Lp' "$ENV_FILE")" != "600" ]]; then
  echo "[fail] $ENV_FILE must be mode 600 (chmod 600 \"$ENV_FILE\")." >&2
  exit 3
fi
# Read exactly the two keys; the file is data, never sourced.
remote_url="$(sed -n 's/^ABERP_REMOTE_URL=//p' "$ENV_FILE" | tail -1)"
remote_fp="$(sed -n 's/^ABERP_REMOTE_FINGERPRINT=//p' "$ENV_FILE" | tail -1)"
if [[ -z "$remote_url" || -z "$remote_fp" ]]; then
  echo "[fail] $ENV_FILE needs ABERP_REMOTE_URL and ABERP_REMOTE_FINGERPRINT." >&2
  exit 3
fi
if ! security find-generic-password -s "aberp.nav.${tenant}" -a remote_session_token >/dev/null 2>&1; then
  echo "[fail] keychain has no aberp.nav.${tenant} / remote_session_token — see --help." >&2
  exit 3
fi

features=()
if [[ "$tenant" == "prod" ]]; then
  features=(--features production)
fi

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" >&2
echo "  ABERP REMOTE CLIENT — tenant=${tenant}" >&2
echo "  backend: ${remote_url} (Thunderbolt Bridge; DB lives there)" >&2
[[ "$tenant" == "prod" ]] && echo "  ⚠️  PRODUCTION — REAL NAV — REAL MONEY / ÉLES ÜZEM" >&2
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" >&2

cd "$REPO_ROOT"
readonly UI_DIR="${REPO_ROOT}/apps/aberp-ui/ui"
(cd "$UI_DIR" && npm install --silent) || { echo "[fail] npm install in $UI_DIR failed" >&2; exit 4; }
(cd "$UI_DIR" && npm run build) || { echo "[fail] npm run build in $UI_DIR failed" >&2; exit 4; }
if [[ ! -s "$UI_DIR/dist/index.html" ]]; then
  echo "[fail] SPA build did not produce $UI_DIR/dist/index.html" >&2
  exit 4
fi

cargo build --release ${features[@]+"${features[@]}"} --bin aberp-ui

ABERP_TENANT="$tenant" ABERP_REMOTE_URL="$remote_url" ABERP_REMOTE_FINGERPRINT="$remote_fp" \
  cargo run --release ${features[@]+"${features[@]}"} --bin aberp-ui
