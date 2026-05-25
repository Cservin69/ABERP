#!/usr/bin/env bash
#
# run_desktop.sh
#
# Launches the ABERP desktop app (Tauri 2 + Svelte) via `tauri dev`.
# This is the canonical Tauri 2 dev-loop shape: tauri-CLI spawns Vite
# (via tauri.conf.json's `beforeDevCommand`) AND runs the Rust shell,
# all in one process group, with hot-reload for the SPA.
#
# Why `tauri dev` (and NOT plain `cargo run`):
#   The Tauri webview loads the URL set by `tauri.conf.json.build.devUrl`
#   — by default `http://localhost:5173`. That URL is served by Vite,
#   which Tauri starts via `beforeDevCommand`. If the launcher invokes
#   `cargo run` (or `target/debug/aberp-ui` directly), tauri-CLI is
#   bypassed, `beforeDevCommand` never fires, Vite never starts, and
#   the webview opens to a blank page. Sessions 63+66 made this
#   regression by running the binary directly with the codesign-then-
#   exec pattern; PR-46δ restores the working pattern.
#
# Why we still pre-build + codesign:
#   `tauri dev` ultimately runs `cargo run`, which produces a binary
#   at `target/<profile>/aberp-ui` and execs it. macOS keychain ACLs
#   ("Always Allow") key off the binary's cdhash; a fresh ad-hoc
#   signature (`codesign --sign -`) gives the binary a deterministic
#   cdhash so the ACL persists across consecutive launches that don't
#   touch Rust source. We do `cargo build` + codesign BEFORE invoking
#   `tauri dev`, so by the time tauri's internal `cargo run` fires,
#   the build is up-to-date (cargo's mtime check → no-op rebuild),
#   and the codesigned bytes are what executes.
#
#   Limitation: if a Rust source change between pre-build and
#   `tauri dev`'s cargo run causes a relink, the freshly-relinked
#   binary will be unsigned for that launch (keychain may re-prompt).
#   In a single script run this race is essentially impossible. The
#   regular workflow — operator edits Rust, runs this script, repeats
#   — codesigns exactly once per source change, which is the right
#   trade.
#
# What this script does:
#   1. Remembers your original working directory.
#   2. cd's into the ABERP repo root for the cargo build step.
#   3. Pre-builds both binaries with `cargo build` so codesign has
#      stable bytes to sign.
#   4. Ad-hoc codesigns both Mach-O binaries on Darwin (--no-codesign
#      to opt out).
#   5. Frees TCP port 5173 if a prior run left Vite stranded there.
#   6. cd's into `apps/aberp-ui/` and runs `./ui/node_modules/.bin/tauri dev`.
#      tauri-CLI then:
#        - executes `beforeDevCommand` from tauri.conf.json (which is
#          `{ "script": "npm run dev", "cwd": "ui" }`, i.e. cd into
#          `apps/aberp-ui/ui/` and run `npm run dev` → Vite serves
#          http://localhost:5173)
#        - runs `cargo run --bin aberp-ui` (a no-op rebuild since we
#          pre-built; execs the codesigned binary)
#        - the Tauri webview loads `devUrl` (http://localhost:5173)
#          and the SPA mounts with hot-reload enabled.
#   7. Puts the whole thing in one process group; Ctrl-C in this
#      terminal sends SIGTERM to the group so Vite, cargo, and the
#      aberp-ui binary all shut down gracefully. The SIGTERM lets
#      the aberp-ui drop handlers release the DuckDB write-lock.
#   8. Belt-and-suspenders: after the wait returns, force-kill any
#      stray PID on port 5173 (the failure mode SituationRoom's
#      run_desktop.sh guards against).
#   9. cd's back to the original cwd.
#
# Usage:
#   ./run_desktop.sh                   # debug profile
#   ./run_desktop.sh --release         # release profile (--release passed to tauri dev too)
#   ./run_desktop.sh --tenant default  # which tenant the backend uses (default: test)
#   ./run_desktop.sh --db PATH         # DuckDB file path (default: ./aberp.duckdb)
#   ./run_desktop.sh --no-codesign     # skip the ad-hoc macOS codesign post-build step
#   ./run_desktop.sh -- --extra-arg    # everything after '--' is forwarded to tauri dev
#
# Verified layout (per repo inspection 2026-05-25):
#   apps/aberp-ui/Cargo.toml           — Tauri Rust shell; [[bin]] name = "aberp-ui"
#   apps/aberp-ui/tauri.conf.json      — Tauri config (devUrl, beforeDevCommand, frontendDist)
#   apps/aberp-ui/ui/package.json      — Svelte SPA front-end (vite dev/build)
#   apps/aberp-ui/ui/node_modules/.bin/tauri — local tauri-CLI binary
#   apps/aberp/Cargo.toml              — CLI; [[bin]] name = "aberp"
#
# Config is via ENV VARS (the Tauri shell takes no CLI args):
#   ABERP_TENANT (default "test")    — which tenant's NAV creds + DB to use
#   ABERP_DB     (default "./aberp.duckdb") — DuckDB file path
#   ABERP_BIN    (optional)          — path to the `aberp` CLI binary; auto-resolves
#                                      to a sibling next to the Tauri binary if unset
#
# FIRST-TIME SETUP:
#   As of PR-46α / session 62, NAV credentials are populated by the in-window
#   wizard on first launch. When the backend handshake reports
#   `state=needs-setup`, the SPA renders a four-field wizard; submit writes
#   the four artifacts to the macOS keychain and the wizard hands off to the
#   normal invoice list. CLI fallback for automation:
#     cargo run --bin aberp -- setup-nav-credentials --tenant <id>
#
# SITUATIONROOM REFERENCE PATTERN:
#   The working-Tauri-2 launcher shape lives at
#     /Users/aben/Documents/Claude/Projects/SituationRoom/scripts/run_desktop.sh
#   Key invariants we copied:
#     - tauri-CLI runs from the dir containing tauri.conf.json's parent
#       (for SR: apps/desktop/, where npx finds tauri in node_modules/.bin/;
#        for ABERP: apps/aberp-ui/, where we call ./ui/node_modules/.bin/tauri
#        because tauri-CLI is installed in the SPA subdir's node_modules).
#     - beforeDevCommand in tauri.conf.json starts Vite; never bypass it.
#     - process-group SIGTERM + lsof :5173 belt-and-suspenders on exit.
#

set -uo pipefail   # NOTE: no -e — we want to handle child exit code, not abort on it

# ---------- config (edit if your launch shape differs) -----------------------
readonly REPO_ROOT="/Users/aben/Documents/Claude/Projects/ABERP"
readonly DESKTOP_DIR="${REPO_ROOT}/apps/aberp-ui"
readonly TAURI_CLI_REL="./ui/node_modules/.bin/tauri"   # relative to DESKTOP_DIR
readonly TAURI_BIN_NAME="aberp-ui"
readonly ABERP_BIN_NAME="aberp"
readonly DEFAULT_TENANT="test"
readonly DEV_PORT="${ABERP_DEV_PORT:-5173}"
readonly SHUTDOWN_TIMEOUT_SECS=15

# ---------- arg parsing ------------------------------------------------------
mode="debug"
tenant="${ABERP_TENANT:-$DEFAULT_TENANT}"
db_path="${ABERP_DB:-./aberp.duckdb}"
codesign_enabled=1
extra_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)       mode="release"; shift ;;
    --debug)         mode="debug"; shift ;;
    --tenant)        tenant="$2"; shift 2 ;;
    --db)            db_path="$2"; shift 2 ;;
    --no-codesign)   codesign_enabled=0; shift ;;
    --help|-h)
      sed -n '2,99p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    --)              shift; extra_args=("$@"); break ;;
    *)               extra_args+=("$1"); shift ;;
  esac
done

# ---------- preserve original cwd -------------------------------------------
readonly ORIGINAL_CWD="$(pwd)"

# ---------- preflight --------------------------------------------------------
cd "$REPO_ROOT" || { echo "repo not at $REPO_ROOT" >&2; exit 2; }

if [[ ! -f "${DESKTOP_DIR}/Cargo.toml" ]]; then
  echo "[fail] no Cargo.toml at ${DESKTOP_DIR}" >&2
  exit 2
fi
if [[ ! -f "${DESKTOP_DIR}/tauri.conf.json" ]]; then
  echo "[fail] no tauri.conf.json at ${DESKTOP_DIR}" >&2
  exit 2
fi
if [[ ! -x "${DESKTOP_DIR}/${TAURI_CLI_REL}" ]]; then
  echo "[fail] tauri-CLI not found at ${DESKTOP_DIR}/${TAURI_CLI_REL}" >&2
  echo "       run \`cd ${DESKTOP_DIR}/ui && npm install\` first" >&2
  exit 2
fi

# Warn (but don't abort) if a stale DuckDB lock looks present from a prior crash.
readonly OPERATOR_DB_DEFAULT="$HOME/.aberp/serve/${tenant}/aberp.duckdb"
if [[ -f "${OPERATOR_DB_DEFAULT}.wal" ]] || [[ -f "${OPERATOR_DB_DEFAULT}.tmp" ]]; then
  echo "[warn] possible stale DuckDB lock companion files near ${OPERATOR_DB_DEFAULT}"
  echo "       (a .wal or .tmp file exists — usually fine, DuckDB will recover on open;"
  echo "       if launch fails with 'database is locked', stop here and inspect)"
fi

# ---------- pre-build (so codesign has stable bytes) -------------------------
# tauri-CLI's `dev` runs `cargo run --bin aberp-ui` internally. If we don't
# pre-build, codesign has nothing to sign. By pre-building from the workspace
# root with the same profile tauri-CLI will use, tauri's internal cargo run
# becomes a no-op rebuild and execs the bytes we just signed.
if [[ "$mode" == "release" ]]; then
  bin_dir="${REPO_ROOT}/target/release"
  build_cmd=(cargo build --release --bin "${ABERP_BIN_NAME}" --bin "${TAURI_BIN_NAME}")
else
  bin_dir="${REPO_ROOT}/target/debug"
  build_cmd=(cargo build --bin "${ABERP_BIN_NAME}" --bin "${TAURI_BIN_NAME}")
fi
echo "[build] ${build_cmd[*]}"
"${build_cmd[@]}" || { echo "[fail] cargo build failed" >&2; exit 4; }

# ---------- ad-hoc codesign (macOS keychain ACL stability) ------------------
# See the long header comment at the top of this file. Short version: a
# stable ad-hoc identity means the macOS keychain's "Always Allow" ACL
# persists across launches when the binary content is unchanged.
if [[ "$(uname -s)" == "Darwin" && $codesign_enabled -eq 1 ]]; then
  for cs_bin in "$ABERP_BIN_NAME" "$TAURI_BIN_NAME"; do
    if [[ -f "${bin_dir}/${cs_bin}" ]]; then
      codesign --force --sign - "${bin_dir}/${cs_bin}" 2>/dev/null \
        && echo "[codesign] ad-hoc signed ${bin_dir}/${cs_bin}" \
        || echo "[codesign] could not sign ${bin_dir}/${cs_bin} (continuing — keychain may re-prompt)"
    fi
  done
  unset cs_bin
elif [[ $codesign_enabled -eq 0 ]]; then
  echo "[codesign] skipped (--no-codesign)"
fi

# ---------- free port 5173 if a prior run left it stranded ------------------
# SituationRoom's run_desktop.sh documents this failure mode: a second
# Ctrl-C while Rust is still compiling can detach Vite and leave it
# owning :5173. We pre-flight free the port so this run isn't blocked.
if command -v lsof >/dev/null 2>&1; then
  if lsof -tiTCP:"$DEV_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    held_by="$(lsof -tiTCP:"$DEV_PORT" -sTCP:LISTEN | tr '\n' ' ')"
    echo "[port] :${DEV_PORT} held by pids ${held_by} (stale Vite from prior run); freeing"
    # shellcheck disable=SC2086
    kill -TERM $held_by 2>/dev/null || true
    sleep 1
    # shellcheck disable=SC2086
    kill -KILL $held_by 2>/dev/null || true
    unset held_by
  fi
fi

# ---------- cleanup hook (group SIGTERM + port-5173 belt-and-suspenders) ----
# Whatever path we exit through (graceful, signal, error), kill the whole
# process group and double-check the dev port is free. Pattern copied from
# SituationRoom/scripts/run_desktop.sh — the working Tauri 2 reference.
cleanup() {
  local rc=$?
  trap - EXIT INT TERM HUP   # avoid recursive cleanup
  echo
  echo "[shutdown] forwarding SIGTERM to process group (rc=${rc})"

  # 1. Polite SIGTERM to the whole group. -$$ targets pgid == our pid.
  if kill -0 -- "-$$" 2>/dev/null; then
    kill -TERM -- "-$$" 2>/dev/null || true
  fi

  # 2. Give children up to SHUTDOWN_TIMEOUT_SECS to exit gracefully so
  #    the aberp-ui drop handlers can release the DuckDB write-lock.
  local waited=0
  while pgrep -g "$$" >/dev/null 2>&1; do
    if [[ $waited -ge $SHUTDOWN_TIMEOUT_SECS ]]; then
      echo "[shutdown] timeout after ${SHUTDOWN_TIMEOUT_SECS}s; escalating to SIGKILL"
      echo "[shutdown] WARNING: DuckDB lock may be left stale — next launch may need recovery"
      kill -KILL -- "-$$" 2>/dev/null || true
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done

  # 3. Belt-and-suspenders: if anything is still on :5173, kill it.
  if command -v lsof >/dev/null 2>&1; then
    local stragglers
    stragglers="$(lsof -tiTCP:"$DEV_PORT" -sTCP:LISTEN 2>/dev/null || true)"
    if [[ -n "$stragglers" ]]; then
      echo "[shutdown] :${DEV_PORT} still held by ${stragglers} — killing"
      # shellcheck disable=SC2086
      kill -TERM $stragglers 2>/dev/null || true
      sleep 1
      # shellcheck disable=SC2086
      kill -KILL $stragglers 2>/dev/null || true
    fi
  fi

  echo "[shutdown] done."
  cd "$ORIGINAL_CWD" 2>/dev/null || true
  echo "[exit] returning to ${ORIGINAL_CWD}"
  echo "[exit] desktop exited with code ${rc}"
  exit "$rc"
}
trap cleanup EXIT INT TERM HUP

# ---------- launch via tauri-CLI --------------------------------------------
# Export the env vars the Tauri shell reads.
export ABERP_TENANT="$tenant"
export ABERP_DB="$db_path"

tauri_args=(dev)
if [[ "$mode" == "release" ]]; then
  tauri_args+=(--release)
fi
if [[ ${#extra_args[@]} -gt 0 ]]; then
  tauri_args+=(-- "${extra_args[@]}")
fi

echo "[launch] mode=${mode}"
echo "[launch] ABERP_TENANT=${tenant} ABERP_DB=${db_path}"
echo "[launch] cd ${DESKTOP_DIR} && ${TAURI_CLI_REL} ${tauri_args[*]}"
echo "[launch] tauri-CLI will run beforeDevCommand (vite at :${DEV_PORT}) + cargo run."
echo "[launch] (Ctrl-C in this terminal sends SIGTERM to the group — graceful shutdown.)"
echo "[launch] First-run NAV-credentials setup is in the SPA itself; no terminal step needed."
echo

cd "$DESKTOP_DIR" || { echo "[fail] cd ${DESKTOP_DIR} failed" >&2; exit 2; }
"$TAURI_CLI_REL" "${tauri_args[@]}" &
tauri_pid=$!

# `wait` returns when the child exits OR when a signal arrives. Either way
# the EXIT trap above does the cleanup (group kill + :5173 sweep + cd back).
wait "$tauri_pid"
