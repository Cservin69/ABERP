#!/usr/bin/env bash
#
# run_desktop.sh
#
# Launches the ABERP desktop app (Tauri + Svelte) and guarantees a graceful
# shutdown so DuckDB's exclusive write-lock is released cleanly on exit.
#
# Why this matters:
#   - DuckDB takes an exclusive file lock when opened in write mode.
#   - If the desktop process is killed with SIGKILL (or crashes), the lock
#     file (e.g. *.duckdb.wal or the OS-level fcntl lock) can persist or
#     leave the DB in a state where the next launch refuses to open it.
#   - Sending SIGTERM (the default behavior of Ctrl-C / Cmd-Q) lets the
#     Tauri shell + Rust app run their drop handlers, which closes the
#     DuckDB connection cleanly and releases the lock.
#
# What this script does:
#   1. Remembers your original working directory.
#   2. cd's into the ABERP repo root.
#   3. Launches the desktop app (cargo run with the right binary).
#   4. Traps SIGINT / SIGTERM so Ctrl-C in *this terminal* sends SIGTERM
#      (not SIGKILL) to the child process, and waits for clean exit.
#   5. On exit (success OR failure), cd's back to your original directory
#      so the terminal you launched from isn't left stranded inside the repo.
#   6. Reports the exit code.
#
# Usage:
#   ./run_desktop.sh                   # debug build (fast compile, slower runtime)
#   ./run_desktop.sh --release         # release build (slower compile, faster runtime)
#   ./run_desktop.sh --tenant test     # selects which tenant's creds + endpoint
#   ./run_desktop.sh -- --extra-arg    # everything after '--' is forwarded to the app
#
# Tested assumption: the desktop binary is launched via `cargo run` inside
# apps/aberp-ui/src-tauri/. If your repo uses a different launch pattern
# (e.g. `npm run tauri dev`), swap the LAUNCH_CMD below.

set -uo pipefail   # NOTE: no -e — we want to handle child exit code, not abort on it

# ---------- config (edit if your launch shape differs) -----------------------
readonly REPO_ROOT="/Users/aben/Documents/Claude/Projects/ABERP"
readonly DESKTOP_DIR="${REPO_ROOT}/apps/aberp-ui/src-tauri"
readonly DEFAULT_TENANT="test"
readonly SHUTDOWN_TIMEOUT_SECS=15

# Where to find each launch shape. Picked in order; first hit wins.
candidate_launch_for_mode() {
  local mode="$1"   # debug | release
  if [[ "$mode" == "release" ]]; then
    echo "cargo run --release --manifest-path ${DESKTOP_DIR}/Cargo.toml"
  else
    echo "cargo run --manifest-path ${DESKTOP_DIR}/Cargo.toml"
  fi
}

# ---------- arg parsing ------------------------------------------------------
mode="debug"
tenant="$DEFAULT_TENANT"
extra_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)      mode="release"; shift ;;
    --debug)        mode="debug"; shift ;;
    --tenant)       tenant="$2"; shift 2 ;;
    --help|-h)
      sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    --)             shift; extra_args=("$@"); break ;;
    *)              extra_args+=("$1"); shift ;;
  esac
done

# ---------- preserve original cwd -------------------------------------------
readonly ORIGINAL_CWD="$(pwd)"
return_to_cwd() {
  local code=$?
  echo
  echo "[exit] returning to ${ORIGINAL_CWD}"
  cd "$ORIGINAL_CWD" 2>/dev/null || true
  echo "[exit] desktop exited with code ${code}"
  exit "$code"
}
trap return_to_cwd EXIT

# ---------- preflight --------------------------------------------------------
cd "$REPO_ROOT" || { echo "repo not at $REPO_ROOT" >&2; exit 2; }

if [[ ! -d "$DESKTOP_DIR" ]]; then
  echo "[warn] expected desktop dir not found: $DESKTOP_DIR" >&2
  echo "       edit DESKTOP_DIR at the top of $0 if your layout differs" >&2
  exit 2
fi

# Warn (but don't abort) if a stale DuckDB lock looks present from a prior crash.
# DuckDB's lock is typically a `.wal` companion in the same dir as the .duckdb file.
# We can't reliably know the DB path without launching the app, so just check the
# common operator-default location.
readonly OPERATOR_DB_DEFAULT="$HOME/.aberp/serve/${tenant}/aberp.duckdb"
if [[ -f "${OPERATOR_DB_DEFAULT}.wal" ]] || [[ -f "${OPERATOR_DB_DEFAULT}.tmp" ]]; then
  echo "[warn] possible stale DuckDB lock companion files near ${OPERATOR_DB_DEFAULT}"
  echo "       (a .wal or .tmp file exists — usually fine, DuckDB will recover on open;"
  echo "       if launch fails with 'database is locked', stop here and inspect)"
fi

# ---------- launch ----------------------------------------------------------
launch_cmd="$(candidate_launch_for_mode "$mode")"
echo "[launch] mode=${mode} tenant=${tenant}"
echo "[launch] ${launch_cmd} ${extra_args[*]:-}"
echo "[launch] (Ctrl-C in this terminal sends SIGTERM to the app — graceful shutdown)"
echo

# Launch in background so we control the signal handling
# shellcheck disable=SC2086
$launch_cmd ${extra_args[@]:+"${extra_args[@]}"} &
child_pid=$!

# Forward Ctrl-C / SIGTERM to the child as SIGTERM (not SIGKILL).
# Then wait up to SHUTDOWN_TIMEOUT_SECS for the child to exit cleanly.
graceful_stop() {
  echo
  echo "[shutdown] forwarding SIGTERM to PID ${child_pid} (graceful close — DuckDB lock will release)"
  kill -TERM "$child_pid" 2>/dev/null || true

  # Wait up to SHUTDOWN_TIMEOUT_SECS for the process to actually exit
  local waited=0
  while kill -0 "$child_pid" 2>/dev/null; do
    if [[ $waited -ge $SHUTDOWN_TIMEOUT_SECS ]]; then
      echo "[shutdown] timeout after ${SHUTDOWN_TIMEOUT_SECS}s; escalating to SIGKILL"
      echo "[shutdown] WARNING: DuckDB lock may be left stale — next launch may need recovery"
      kill -KILL "$child_pid" 2>/dev/null || true
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done
}
trap 'graceful_stop' INT TERM

# Block until the child exits (either naturally or via our signal handler).
# `wait` returns the child's exit code; we propagate it via return_to_cwd().
wait "$child_pid"
