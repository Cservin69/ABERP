#!/usr/bin/env bash
#
# cut_gate_sqlite_posture_probes.sh — proves the ADR-0108 SQLite posture gate
# (`tools/cut_gate_sqlite_posture.sh`) has TEETH.
#
# Every other cut-gate in this tree ships with a `_probes.sh` companion
# (nav_emit_door, read_fork, write_fork, keychain_seam, edition_ratchet,
# opener_census). The posture gate landed in PR #51 without one, and the S449
# adversarial pass found THREE live bypasses in it inside ten minutes — P1, P2
# and P3 below. That is the whole argument for the convention: a gate never
# observed catching its own target is decoration.
#
# Each probe plants ONE synthetic instance of the defect class into a THROWAWAY
# COPY of the tree, asserts the gate goes RED, then asserts the unmutated copy
# is GREEN. No real source file is ever mutated, so the probes stay valid as the
# tree moves.
#
#   P1  a forbidden feature enabled with TOML SINGLE quotes     (S449: was blind)
#   P2  an `attach database` written in lower case              (S449: was blind)
#   P3  a bare open through the `engine::Connection` SEAM ALIAS (S449: was blind)
#   P4  a forbidden feature enabled with double quotes          (control)
#   P5  a bare `rusqlite::Connection::open` outside the seam    (control)
#   P6  a forbidden rusqlite API call                           (control)
#   P7  the seam file is deleted — the gate must refuse, not go green
#   P8  an unrelated DuckDB opener must NOT trip the gate       (no false alarm)
#
# P3 is the one that matters for Steps 5-9. ADR-0108 §2.3's type seam means no
# file outside `crates/aberp-db/src/engine.rs` ever names either engine crate
# again: from Step 5 the tree writes `use aberp_db::engine::Connection`, which
# under `--features sqlite-engine` IS `rusqlite::Connection`. The pre-S449 S4
# predicate only looked at files naming `rusqlite`, so it would have gone
# structurally blind at exactly the step it exists to police.
#
# Exit 0 = every probe behaved.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="tools/cut_gate_sqlite_posture.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sqliteposture-probes.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0; bad=0

# A copy holding only what the gate reads: its own assets, the scanned scope,
# and every Cargo.toml. Copying the whole tree would drag target/ along.
fresh() {
  local d; d="$(mktemp -d "$WORK/copy.XXXXXX")"
  ( cd "$ROOT" && tar -cf - tools apps/*/src apps/*/Cargo.toml crates modules Cargo.toml \
      --exclude='target' 2>/dev/null ) | tar -C "$d" -xf - 2>/dev/null
  printf '%s' "$d"
}
run_gate() { ( cd "$1" && bash "$GATE" > "$1/gate.out" 2>&1; printf '%s' "$?" ); }

expect_red() {
  local label="$1" dir="$2" want="$3" rc
  rc="$(run_gate "$dir")"
  if [[ "$rc" == "0" ]]; then
    printf '  ✗ BLIND: %s — gate PASSED on a planted defect\n' "$label"; bad=$((bad+1)); return
  fi
  if [[ -n "$want" ]] && ! grep -q "$want" "$dir/gate.out"; then
    printf '  ✗ WRONG ARM: %s — gate failed, but %s did not fire\n' "$label" "$want"
    sed 's/^/      /' "$dir/gate.out"; bad=$((bad+1)); return
  fi
  printf '  ✓ reds: %s%s\n' "$label" "${want:+ (via $want)}"; pass=$((pass+1))
}

expect_green() {
  local label="$1" dir="$2" rc
  rc="$(run_gate "$dir")"
  if [[ "$rc" != "0" ]]; then
    printf '  ✗ FALSE ALARM: %s — gate FAILED on a benign tree\n' "$label"
    sed 's/^/      /' "$dir/gate.out"; bad=$((bad+1)); return
  fi
  printf '  ✓ stays green: %s\n' "$label"; pass=$((pass+1))
}

echo "ADR-0108 SQLite posture gate — probes"

# --- baseline: the unmutated copy must be green, or nothing below means anything
d="$(fresh)"; expect_green "the unmutated tree" "$d"

# --- P1: TOML single-quoted feature name (S449 bypass, now closed) -----------
# `features = ['load_extension']` is valid TOML — a literal string — and cargo
# accepts it. The original S1 matched only `"load_extension"`.
d="$(fresh)"
perl -0pi -e "s/rusqlite = \{ version = \"0\.40\", features = \[\"bundled\", \"limits\"\] \}/rusqlite = { version = \"0.40\", features = ['bundled', 'limits', 'load_extension'] }/" "$d/Cargo.toml"
grep -q "load_extension" "$d/Cargo.toml" || { printf '  ! P1 could not plant its mutation — probe is vacuous\n'; bad=$((bad+1)); }
expect_red "P1 forbidden feature via TOML single quotes" "$d" "FAIL (S1)"

# --- P2: lower-case ATTACH (S449 bypass, now closed) -------------------------
# SQL keywords are case-insensitive; `attach database 'x' as y` executes
# identically. The original S3 pattern was case-sensitive.
d="$(fresh)"
cat >> "$d/crates/aberp-db/src/schema.rs" <<'EOF'

pub fn s449_probe_p2(c: &crate::engine::Connection) -> crate::engine::Result<usize> {
    c.execute("attach database '/tmp/x.db' as evil", [])
}
EOF
expect_red "P2 lower-case \`attach database\`" "$d" "FAIL (S3)"

# --- P3: a bare open through the engine SEAM ALIAS (S449 bypass, now closed) --
# THE Steps-5-9 shape. No mention of `rusqlite` anywhere in the file.
d="$(fresh)"
cat >> "$d/crates/aberp-db/src/schema.rs" <<'EOF'

use crate::engine::Connection as S449ProbeConn;
pub fn s449_probe_p3(p: &std::path::Path) -> crate::engine::Result<S449ProbeConn> {
    S449ProbeConn::open(p)
}
EOF
expect_red "P3 bare open via the \`engine::Connection\` seam alias" "$d" "FAIL (S4)"

# --- P4: double-quoted forbidden feature (control) ---------------------------
d="$(fresh)"
perl -0pi -e 's/features = \["bundled", "limits"\]/features = ["bundled", "limits", "load_extension"]/' "$d/Cargo.toml"
expect_red "P4 forbidden feature via double quotes (control)" "$d" "FAIL (S1)"

# --- P5: a bare rusqlite open outside the seam (control) ---------------------
d="$(fresh)"
cat >> "$d/crates/aberp-db/src/schema.rs" <<'EOF'

use rusqlite::Connection as S449ProbeRaw;
pub fn s449_probe_p5(p: &std::path::Path) -> rusqlite::Result<S449ProbeRaw> {
    S449ProbeRaw::open(p)
}
EOF
expect_red "P5 bare \`rusqlite::Connection::open\` outside the seam (control)" "$d" "FAIL (S4)"

# --- P6: a forbidden rusqlite API call (control) -----------------------------
d="$(fresh)"
cat >> "$d/crates/aberp-db/src/schema.rs" <<'EOF'

pub fn s449_probe_p6(c: &crate::engine::Connection) {
    let _ = c.update_hook(None::<fn(i32, &str, &str, i64)>);
}
EOF
expect_red "P6 forbidden rusqlite API \`update_hook\`" "$d" "FAIL (S2)"

# --- P7: the seam is gone — refuse, do not go green ---------------------------
# The K0/N0/M0 lesson: a scanner whose subject vanished must fail loud.
d="$(fresh)"; rm -f "$d/crates/aberp-db/src/sqlite.rs"
expect_red "P7 the sanctioned seam is deleted" "$d" "FAIL (S4)"

# --- P8: an ordinary DuckDB opener must NOT trip S4 --------------------------
# `duckdb::Connection::open` is everywhere in this tree and is governed by the
# ADR-0098 opener census, not by this gate. A posture gate that reds on it would
# be turned off within a week.
d="$(fresh)"
cat >> "$d/crates/aberp-db/src/debounce.rs" <<'EOF'

pub fn s449_probe_p8(p: &std::path::Path) -> duckdb::Result<duckdb::Connection> {
    duckdb::Connection::open(p)
}
EOF
expect_green "P8 an ordinary DuckDB opener (no false alarm)" "$d"

echo
if [[ "$bad" -eq 0 ]]; then echo "PROBES: ✓ $pass/$pass behaved"; exit 0; fi
echo "PROBES: ✗ $bad probe(s) misbehaved ($pass ok)"
exit 1
