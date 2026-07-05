#!/usr/bin/env bash
#
# cut_gate_negative_probes.sh — proves tools/cut_gate_opener_census.sh has TEETH.
#
# PORTED from the editions harness (Cservin69/ABERP-Editions). Scoped to the two
# checks the PROD SAFE-lane gate actually enforces (CHECK P1 count-freeze + CHECK
# P2 fingerprint-freeze). For each, plant the corresponding regression in a
# throwaway COPY of the tree, run the gate against the copy, and assert it EXITS
# NON-ZERO with the matching message. A green census gate is only meaningful if it
# would have gone red on a real opener-surface growth/swap — this is that proof.
#
# The working tree is NEVER mutated; every probe operates on a fresh mktemp copy
# removed on exit. Runs in CI alongside the gate (cut-gate.yml).
#
# Exit 0 = every probe behaved (clean copy passes; each regression is caught;
#          each cfg(test) false-positive control is correctly IGNORED).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="tools/cut_gate_opener_census.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/cutgate-probes.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0; bad=0

fresh() {  # -> path to a fresh, clean copy of the tree (excludes .git)
  local d; d="$(mktemp -d "$WORK/copy.XXXXXX")"
  tar -C "$ROOT" --exclude=.git -cf - . | tar -C "$d" -xf -
  printf '%s' "$d"
}
gate_rc() { ( cd "$1" && bash "$GATE" ) >"$1/.out" 2>&1; echo $?; }
expect_pass() {  # $1 dir  $2 label
  local rc; rc="$(gate_rc "$1")"
  if [[ "$rc" == "0" ]]; then printf '  ✓ %s\n' "$2"; pass=$((pass+1))
  else printf '  ✗ BROKEN: %s — clean/ignored copy should PASS but gate exit=%s\n' "$2" "$rc"
    sed 's/^/        /' "$1/.out"; bad=$((bad+1)); fi
}
expect_fail() {  # $1 dir  $2 signature  $3 label
  local rc; rc="$(gate_rc "$1")"
  if [[ "$rc" != "0" ]] && grep -qF -- "$2" "$1/.out"; then
    printf '  ✓ caught: %s  (exit=%s; matched: "%s")\n' "$3" "$rc" "$2"; pass=$((pass+1))
  else printf '  ✗ ESCAPED: %s  (exit=%s; expected non-zero + "%s")\n' "$3" "$rc" "$2"
    sed 's/^/        /' "$1/.out"; bad=$((bad+1)); fi
}

echo "negative probes for the ADR-0099 PROD opener-census cut-gate"
echo "root: $ROOT"; echo

echo "[sanity] a clean copy passes"
c="$(fresh)"; expect_pass "$c" "clean tree → CUT-GATE PASSED"

echo "[CHECK P1] an existing residual GROWS its opener count (mes_manager.rs +1)"
c="$(fresh)"; printf '\nfn _probe_grow(p: &std::path::Path) {\n    let _ = duckdb::Connection::open(p);\n}\n' >> "$c/apps/aberp/src/mes_manager.rs"
expect_fail "$c" "grew its openers" "P1 — residual opener count grew beyond frozen baseline"

echo "[CHECK P1] a BRAND-NEW opener-bearing file not on the frozen census"
c="$(fresh)"; printf 'fn _probe_new_opener() {\n    let _ = duckdb::Connection::open("/x");\n}\n' > "$c/apps/aberp/src/zz_probe_opener.rs"
expect_fail "$c" "NEW unaccounted opener-bearing file" "P1 — a new unlisted runtime-opener file is rejected"

echo "[CHECK P1] a NEW opener in a business CRATE (aberp-qa) — R4 crates-scope must see it"
c="$(fresh)"; mkdir -p "$c/crates/aberp-qa/src"; printf 'pub fn _probe(p:&std::path::Path)->Result<duckdb::Connection,duckdb::Error>{\n    duckdb::Connection::open(p)\n}\n' > "$c/crates/aberp-qa/src/zz_probe_opener.rs"
expect_fail "$c" "NEW unaccounted opener-bearing file" "P1/crates — a new separate opener in crates/ is rejected"

echo "[CHECK P1] a Connection::open INSIDE #[cfg(test)] must NOT trip (cfg(test)-aware precision)"
c="$(fresh)"; printf '\n#[cfg(test)]\nmod zz_probe_test {\n    fn t(p:&std::path::Path){ let _ = duckdb::Connection::open(p); }\n}\n' >> "$c/apps/aberp/src/mes_manager.rs"
expect_pass "$c" "P1 — Connection::open inside #[cfg(test)] is correctly IGNORED (not a residual)"

echo "[CHECK P2] a COUNT-PRESERVING opener swap (rename the binding on a frozen opener line)"
c="$(fresh)"
sed -i 's/let mut conn = duckdb::Connection::open(&db_path)/let mut conn_swapped = duckdb::Connection::open(\&db_path)/' \
    "$c/crates/aberp-mes/src/ledger_writer.rs"
expect_fail "$c" "opener fingerprint set DIVERGED" "P2 — a count-preserving intra-file opener swap is caught by the fingerprint freeze"

echo "[CHECK P1/alias] an ALIASED live-DB open (use duckdb::Connection as X; X::open) — alias-evasion must be caught"
c="$(fresh)"; printf '\nuse duckdb::Connection as ProbeAliasConn;\nfn _probe_alias(p:&std::path::Path){ let _ = ProbeAliasConn::open(p); }\n' >> "$c/apps/aberp/src/mes_manager.rs"
expect_fail "$c" "grew its openers" "P1/alias — an aliased Connection::open (alias-evasion) is caught, not invisible"

echo "[CHECK P1/alias] an aliased open INSIDE #[cfg(test)] must NOT trip (alias scan is cfg(test)-aware)"
c="$(fresh)"; printf '\n#[cfg(test)]\nmod zz_alias_test {\n    use duckdb::Connection as TAlias;\n    fn t(p:&std::path::Path){ let _ = TAlias::open(p); }\n}\n' >> "$c/apps/aberp/src/mes_manager.rs"
expect_pass "$c" "P1/alias — an aliased open inside #[cfg(test)] is correctly IGNORED"

echo
echo "probes passed: $pass   broken/escaped: $bad"
if [[ "$bad" -ne 0 ]]; then echo "NEGATIVE-PROBES: ✗ FAILED"; exit 1; fi
echo "NEGATIVE-PROBES: ✓ ALL CHECKS HAVE TEETH"
