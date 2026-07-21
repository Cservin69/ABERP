#!/usr/bin/env bash
#
# cut_gate_negative_probes.sh — proves tools/cut_gate_opener_census.sh has TEETH.
#
# MIGRATION-INVARIANT BY DESIGN. Every probe operates on a SYNTHETIC scratch file
# that the probe itself creates, registers, mutates, and deletes inside a
# throwaway COPY of the tree — NEVER a real source file. This is deliberate: an
# earlier revision planted its regressions in real files (mes_manager.rs,
# ledger_writer.rs), which broke the moment the H3 opener migration moved those
# files off the census (the grow/alias probes then fired "NEW unaccounted file"
# instead of "grew its openers" and escaped). Coupling the meta-test to real
# openers means it silently rots as the migration progresses — the one failure
# mode we cannot afford. Synthetic scratch files keep these probes valid through
# the ENTIRE migration, including full zero-residual (when no real in-serve file
# has a runtime opener left).
#
# Each probe asserts RED-before / GREEN-after where applicable, and the final
# META probe proves fail-closed: a de-gated census script lets a real opener
# through — which is exactly the escape these expect_fail probes catch, so the
# gate script (not luck) is what provides the teeth.
#
# Exit 0 = every probe behaved. Non-zero = a probe broke or a regression escaped.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="tools/cut_gate_opener_census.sh"
COUNTS="tools/adr0098_prod_frozen_residuals.txt"
FP="tools/adr0098_prod_opener_fingerprints.txt"
SCAN="tools/adr0098_opener_scan.awk"
# Scratch file lives in census scope (apps/aberp/src, not */tests/*, not
# /aberp-db/) but is NOT any real module — it exists only inside a throwaway copy.
SCRATCH="apps/aberp/src/zz_probe_scratch.rs"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/cutgate-probes.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0; bad=0

fresh() { # -> path to a fresh, clean copy of the tree (excludes .git + target)
  local d; d="$(mktemp -d "$WORK/copy.XXXXXX")"
  tar -C "$ROOT" --exclude=.git --exclude=target -cf - . | tar -C "$d" -xf -
  printf '%s' "$d"
}
gate_rc() { ( cd "$1" && bash "$GATE" ) >"$1/.out" 2>&1; echo $?; }

expect_pass() { # $1 dir  $2 label
  local rc; rc="$(gate_rc "$1")"
  if [[ "$rc" == "0" ]]; then printf '  ✓ %s\n' "$2"; pass=$((pass+1))
  else printf '  ✗ BROKEN: %s — expected CLEAN pass but gate exit=%s\n' "$2" "$rc"
    sed 's/^/        /' "$1/.out"; bad=$((bad+1)); fi
}
expect_fail() { # $1 dir  $2 signature  $3 label
  local rc; rc="$(gate_rc "$1")"
  if [[ "$rc" != "0" ]] && grep -qF -- "$2" "$1/.out"; then
    printf '  ✓ caught: %s  (exit=%s; matched: "%s")\n' "$3" "$rc" "$2"; pass=$((pass+1))
  else printf '  ✗ ESCAPED: %s  (exit=%s; expected non-zero + "%s")\n' "$3" "$rc" "$2"
    sed 's/^/        /' "$1/.out"; bad=$((bad+1)); fi
}
# Register the scratch file's CURRENT openers into a copy's frozen baselines, so
# the copy is CLEAN with the scratch present. Needed by the grow/swap probes,
# which test a DELTA from a frozen entry (P1 count-exceed / P2 fingerprint-swap).
register_scratch() { # $1 copydir  $2 count
  local d="$1" n="$2"
  printf '%s %s\n' "$n" "$SCRATCH" >> "$d/$COUNTS"
  ( cd "$d" && awk -f "$SCAN" "$SCRATCH" 2>/dev/null | sed 's/^[0-9]*://' \
      | while IFS= read -r s; do printf '%s|%s\n' "$SCRATCH" "$s"; done >> "$FP" )
}

echo "negative probes for the ADR-0099 PROD opener-census cut-gate (synthetic, migration-invariant)"
echo "root: $ROOT"; echo

echo "[sanity] a clean copy passes"
c="$(fresh)"; expect_pass "$c" "clean tree → CUT-GATE PASSED"

echo "[P1 new-file] a scratch file with a runtime opener, NOT on the census → RED"
c="$(fresh)"
printf 'fn _probe_new(p: &std::path::Path) {\n    let _ = duckdb::Connection::open(p);\n}\n' > "$c/$SCRATCH"
expect_fail "$c" "NEW unaccounted opener-bearing file" "P1 new-file: a newly-added runtime opener is DETECTED"
echo "[P1 new-file] …and GREEN once the opener is removed (detect ↔ clean symmetry)"
rm -f "$c/$SCRATCH"; expect_pass "$c" "P1 new-file: census returns clean after the opener is removed"

echo "[P1 grow] a registered residual (count 1) that GROWS to 2 → RED"
c="$(fresh)"
printf 'fn _a(p: &std::path::Path) { let _ = duckdb::Connection::open(p); }\n' > "$c/$SCRATCH"
register_scratch "$c" 1
expect_pass "$c" "P1 grow baseline: 1 registered opener matches its frozen count"
printf 'fn _b(p: &std::path::Path) { let _ = duckdb::Connection::open(p); }\n' >> "$c/$SCRATCH"
expect_fail "$c" "grew its openers" "P1 grow: a 2nd opener beyond the frozen count is DETECTED"

echo "[P1 alias] an ALIASED live-DB open (use … as X; X::open) → RED"
c="$(fresh)"
printf 'use duckdb::Connection as ZProbeAlias;\nfn _probe_alias(p: &std::path::Path) { let _ = ZProbeAlias::open(p); }\n' > "$c/$SCRATCH"
expect_fail "$c" "NEW unaccounted opener-bearing file" "P1 alias: an aliased Connection::open is DETECTED (not invisible)"

echo "[P1 re-export] a re-exported alias (pub use … as X; X::open) → RED"
c="$(fresh)"
printf 'pub use duckdb::Connection as ZProbeReexport;\nfn _probe_reexport(p: &std::path::Path) { let _ = ZProbeReexport::open(p); }\n' > "$c/$SCRATCH"
expect_fail "$c" "NEW unaccounted opener-bearing file" "P1 re-export: an opener via a re-exported alias is DETECTED"

echo "[cfg(test)] a scratch file whose ONLY opener is inside #[cfg(test)] → clean (ignored)"
c="$(fresh)"
printf '#[cfg(test)]\nmod zz {\n    fn t(p: &std::path::Path) { let _ = duckdb::Connection::open(p); }\n}\n' > "$c/$SCRATCH"
expect_pass "$c" "cfg(test): a #[cfg(test)] opener is correctly IGNORED (not a residual)"

# ── lexer traps (2026-07-21) ─────────────────────────────────────────────────
# The scanner was FAIL-OPEN for as long as this gate existed: a char literal
# holding a quote — out.push('"') at tenant_registry.rs:615 — flipped the string
# state ON and left the lexer stuck mid-string from line 623 to 1092. In that
# 470-line window it swallowed EVERYTHING: `mod tests {` at 880 (so 3 test-only
# openers were frozen into the census as runtime residuals) and, far worse, any
# genuine runtime opener anyone might have added there. 17 in-scope files carry
# quote char literals, serve.rs among them. These two probes pin BOTH halves —
# the blindness and the over-count — so the class cannot regress silently.
echo "[lexer traps] an opener AFTER char literals / lifetimes / raw strings → still RED"
c="$(fresh)"
cat > "$c/$SCRATCH" <<'PROBE'
fn _q(out: &mut String) { out.push('"'); out.push('\''); out.push('\\'); out.push('{'); }
fn _lt<'a>(s: &'a str) -> &'a str { 'outer: loop { break 'outer; } s }
fn _raw() -> &'static str { r#"a "b" \ c"# }
fn _probe_after_traps(p: &std::path::Path) { let _ = duckdb::Connection::open(p); }
PROBE
expect_fail "$c" "NEW unaccounted opener-bearing file" "lexer traps: a runtime opener downstream of a quote char literal is still SEEN (not swallowed)"

echo "[lexer traps] the scanner itself classifies a trap file EXACTLY: 1 runtime, 0 test"
# Asserted directly against the scanner, not through the gate: the gate is blind
# to the over-count half (an unregistered file that scans to 0 openers is simply
# skipped, so a swallowed file looks identical to a clean one). This is the probe
# that fails in BOTH directions — pre-fix it emits 0 lines (blind), and a naive
# `'`-to-next-`'` char rule mangles the lifetimes/labels below into a phantom hit.
trapfile="$WORK/lexer_traps.rs"
cat > "$trapfile" <<'PROBE'
fn _q(out: &mut String) { out.push('"'); out.push('\''); out.push('\\'); out.push('{'); }
fn _lt<'a>(s: &'a str) -> &'a str { 'outer: loop { break 'outer; } s }
fn _raw() -> &'static str { r#"a "b" \ c"# }
fn _after_traps(p: &std::path::Path) { duckdb::Connection::open(p); }
#[cfg(test)]
mod zz {
    fn t(p: &std::path::Path) { let _ = duckdb::Connection::open(p); }
}
PROBE
got="$(awk -f "$ROOT/$SCAN" "$trapfile" 2>/dev/null)"
want="4:_after_traps:fn _after_traps(p: &std::path::Path) { duckdb::Connection::open(p); }"
if [[ "$got" == "$want" ]]; then
  printf '  ✓ lexer traps: exactly the runtime opener is seen; the #[cfg(test)] one is not counted, lifetimes/labels produce no phantom\n'
  pass=$((pass+1))
else
  printf '  ✗ BROKEN: lexer traps: scanner output != expected\n      want: %s\n      got:  %s\n' "$want" "${got:-<nothing — the lexer is swallowing the file>}"
  bad=$((bad+1))
fi

echo "[P2 swap] a registered opener whose binding is renamed (count-preserving) → RED"
c="$(fresh)"
printf 'fn _p(p: &std::path::Path) { let conn_probe = duckdb::Connection::open(p); let _ = conn_probe; }\n' > "$c/$SCRATCH"
register_scratch "$c" 1
expect_pass "$c" "P2 swap baseline: the registered fingerprint matches"
# Count-preserving swap (same opener count, different fingerprint text). perl -i is
# cross-platform (avoids the BSD-vs-GNU `sed -i` split), so this probe runs
# identically on macOS and the Linux CI runner.
perl -i -pe 's/let conn_probe = duckdb::Connection::open/let conn_swapped = duckdb::Connection::open/' "$c/$SCRATCH"
expect_fail "$c" "opener fingerprint set DIVERGED" "P2 swap: a count-preserving intra-file opener swap is DETECTED"

echo "[META fail-closed] a de-gated census script must let a real opener through"
c="$(fresh)"
printf '#!/usr/bin/env bash\nexit 0\n' > "$c/$GATE"   # sabotage: gate always passes
printf 'fn _p(p: &std::path::Path) { let _ = duckdb::Connection::open(p); }\n' > "$c/$SCRATCH"
metarc="$(gate_rc "$c")"
if [[ "$metarc" == "0" ]]; then
  printf '  ✓ fail-closed: a de-gated gate (exit 0) passes a real opener (rc=0) — so the gate SCRIPT is load-bearing, and the expect_fail probes above would ESCAPE (harness RED) the moment anyone de-gates it. The teeth are real, not incidental.\n'
  pass=$((pass+1))
else
  printf '  ✗ META BROKEN: a de-gated gate reported non-zero (rc=%s) — the probes above are not measuring what runs the census, so a real de-gating could slip past.\n' "$metarc"
  bad=$((bad+1))
fi

echo
echo "probes passed: $pass   broken/escaped: $bad"
if [[ "$bad" -ne 0 ]]; then echo "NEGATIVE-PROBES: ✗ FAILED"; exit 1; fi
echo "NEGATIVE-PROBES: ✓ ALL CHECKS HAVE TEETH (synthetic; invariant under the migration)"
