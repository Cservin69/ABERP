#!/usr/bin/env bash
#
# cut_gate_edition_ratchet.sh — ADR-0093 product-line saw-off RATCHET.
#
# The saw-off (S3/S5, 2026-07-21) DELETED the Portable and Defense surface from
# ABERP.git: the `run/run_portable.sh` launcher pair, the `PROD_Portable_v*` /
# `PROD_Defense_v*` refs, and `SAW-OFF.md`. Deletion is a one-time event. It was
# verified by EXPERIMENT that nothing stopped it coming back: `run/run_portable.sh`,
# `SAW-OFF.md` and a `crates/aberp-portable/` crate were all re-added to a tree copy
# and ALL EIGHT existing gates stayed green. This gate is the missing ratchet.
#
# CHECK E — zero LIVE Portable/Defense product surface in ABERP.git. Four arms:
#   E1 launcher   — any path under `run/` naming portable/defense
#   E2 edition crate — a `crates|apps/aberp-(portable|defense)/` directory, or any
#                      workspace Cargo.toml DECLARING such a package name
#   E3 SAW-OFF.md — the saw-off manifest re-appearing as a live root artifact
#   E4 refs       — a `PROD_(Portable|Defense)_v*` branch/tag, local or on origin
#
# CHECK E0 — scanner liveness (ALWAYS ENFORCED, never informational). Every arm
# above is `find … | grep`, zero-hits ⇒ green: a find that ERRORS (its stderr was
# swallowed by 2>/dev/null) or a matcher that silently stops matching both report
# ZERO and PASS — the gate goes blind-green having scanned nothing. This is the
# same hole finding F4 closed for the awk gates. The fix has two halves, both in
# tools/cut_gate_edition_ratchet_backstop.sh: rb_find CAPTURES find's stderr so a
# broken scan becomes SCANNER BROKEN (not an empty one), and CHECK E0 hands each
# arm's matcher a planted positive + look-alike negative on every run and asserts
# the count. A blind scanner is a broken tool, not a policy choice, so E0 and the
# find-health check ignore ENFORCE_EDITION_RATCHET entirely.
#
# ---------------------------------------------------------------------------
# WHERE THE LINE IS DRAWN — historical record vs. live artifact.
#
# `docs/PRUNED_PORTABLE_REFS.md`, `docs/PRUNED_DEFENSE_REFS.md`, ADR-0093 and
# `run/tests/upgrade_prod_version_re_test.sh` all legitimately contain the exact
# strings `run/run_portable.sh` and `PROD_Portable_v0.1.2` — that is what a prune
# record and a regression test are FOR. A content-grep gate would fire on every
# one of them and be switched off inside a week.
#
# So this gate NEVER greps file contents for these words. It matches only
# STRUCTURE — a path, a directory name, a git ref name, and a Cargo package
# `name =` DECLARATION (a manifest field, not prose). A document may say
# "run/run_portable.sh" as many times as it likes; it may not BE that file.
# Correspondingly `docs/` and `adr/` are out of scope entirely: that is exactly
# where the historical record is supposed to live.
#
# Two deliberate non-triggers, both currently in-tree and both must stay green:
#   * `apps/aberp/tests/portable_demo_boot_e2e.rs` — KEPT on purpose (ADR-0100 §5
#     classes it Portable by PROVENANCE, but in substance it is this repo's only
#     `build_router` / real-HTTP / `/health` pin). It is a test file, not a crate
#     directory, so E2 does not see it.
#   * `refs/heads/worktree-s1-portable-sawoff-scope` — a working branch. E4 matches
#     only the `PROD_(Portable|Defense)_v*` RELEASE shape, and skips any
#     `archive/` namespace (ADR-0093 archived the pruned refs under
#     `refs/tags/archive/aberp-git/*` in ABERP-Editions.git).
# ---------------------------------------------------------------------------
#
# ENFORCE_EDITION_RATCHET=0 disables the E1–E4 POLICY arms (local probe). It does
# NOT disable CHECK E0 or find-health — a blind scanner is broken, not exempt.
# EDITION_RATCHET_ROOT overrides the tree under test — used ONLY by
# tools/cut_gate_edition_ratchet_probes.sh to run the arms against synthetic
# trees; CI never sets it.
#
# Exit 0 = gate green (saw-off holds). Non-zero = Portable/Defense surface is
# back, OR the scanner could not be trusted to have looked.

set -uo pipefail
ROOT="${EDITION_RATCHET_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT" || { echo "CUT-GATE: ✗ FAILED (no such root: $ROOT)"; exit 1; }

# The liveness backstop is a code dependency of this gate and sits beside it,
# independent of the tree-under-test ROOT (which the probes point at synthetic
# trees). Source it from the script's own directory, not from ROOT.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKSTOP="$HERE/cut_gate_edition_ratchet_backstop.sh"
[[ -f "$BACKSTOP" ]] || { echo "CUT-GATE: ✗ FAILED (liveness backstop missing: $BACKSTOP)"; exit 1; }
# shellcheck source=tools/cut_gate_edition_ratchet_backstop.sh
source "$BACKSTOP"; rb_init

fail=0
note() { printf '  %s\n' "$*"; }
echo "ADR-0093 product-line saw-off ratchet — root: $ROOT"

enforce="${ENFORCE_EDITION_RATCHET:-1}"
flag() { note "$1"; if [[ "$enforce" == "1" ]]; then fail=1; else note "  (enforcement disabled — not failing)"; fi; }

EDITION='(portable|defense)'

# ── arm matchers ───────────────────────────────────────────────────────────
# Each takes <root> <errfile> and emits matched artifacts, one per line. find's
# stderr goes to <errfile> via rb_find (never /dev/null), so a broken scan is
# visible to rb_health. The SAME functions feed both the live E1–E4 scan and the
# CHECK E0 control, so the control can never drift from what actually runs.
arm_e1() {  # E1 — any file under run/ naming an edition
  rb_find "$2" "$1/run" -type f | grep -iE "$EDITION"
}
arm_e2_dir() {  # E2a — a crates|apps/aberp-(portable|defense)/ directory
  rb_find "$2" "$1/crates" "$1/apps" -mindepth 1 -maxdepth 1 -type d | grep -iE "/aberp-$EDITION$"
}
arm_e2_decl() {  # E2b — a Cargo package name = "aberp-(portable|defense)" declaration
  local m
  while IFS= read -r m; do
    [[ -z "$m" ]] && continue
    grep -nEi '^[[:space:]]*name[[:space:]]*=[[:space:]]*"aberp-(portable|defense)"' "$m" \
      | sed "s#^#$m:#"
  done < <(rb_find "$2" "$1/crates" "$1/apps" -maxdepth 2 -name Cargo.toml | sort)
}
arm_e3() {  # E3 — a live SAW-OFF.md outside the docs/adr historical record
  rb_find "$2" "$1" -path "$1/.git" -prune -o -path "$1/docs" -prune -o -path "$1/adr" -prune \
       -o -name target -prune -o -name node_modules -prune \
       -o -type f -iname 'SAW-OFF.md' -print
}
# E4's ref-name filter, factored so CHECK E0 exercises the exact regex the live
# arm uses: a PROD_(Portable|Defense)_v* release shape, archive/ namespaces exempt.
edition_ref_filter() { grep -E '(^|/)PROD_(Portable|Defense)_v' | grep -v '/archive/'; }

# ── CHECK E0 — scanner liveness (ALWAYS ENFORCED, never informational) ────────
# Hand each arm a synthetic tree with a planted positive AND a look-alike
# negative; assert exactly one hit. A count != 1 means the matcher is blind or
# indiscriminate — either way it cannot be trusted for the live scan below.
echo "[CHECK E0] scanner liveness — every arm sees a planted positive, rejects a look-alike (ALWAYS ENFORCED)"
ctl="$RB_DIR/ctl"; rm -rf "$ctl"
mkdir -p "$ctl/run" "$ctl/crates/aberp-defense" "$ctl/crates/legacy-bits" "$ctl/apps" "$ctl/docs"
: > "$ctl/run/run_portable.sh"                                             # E1 positive
: > "$ctl/run/run_prod.sh"                                                 # E1 negative
: > "$ctl/crates/aberp-defense/marker"                                     # E2a positive (dir)
printf '[package]\nname = "aberp-portable"\n' > "$ctl/crates/legacy-bits/Cargo.toml"   # E2b positive (disguised dir)
printf '[package]\nname = "aberp-db"\n'       > "$ctl/crates/aberp-defense/Cargo.toml" # E2b negative
: > "$ctl/SAW-OFF.md"                                                      # E3 positive
: > "$ctl/docs/SAW-OFF.md"                                                 # E3 negative (docs pruned)
cerr="$RB_DIR/ctl.err"; : > "$cerr"
rb_expect E1  "$(arm_e1      "$ctl" "$cerr" | grep -c .)" 1 "E1  run/ launcher (rejects run_prod.sh)"
rb_expect E2a "$(arm_e2_dir  "$ctl" "$cerr" | grep -c .)" 1 "E2  edition crate dir"
rb_expect E2b "$(arm_e2_decl "$ctl" "$cerr" | grep -c .)" 1 "E2  package name (rejects aberp-db)"
rb_expect E3  "$(arm_e3      "$ctl" "$cerr" | grep -c .)" 1 "E3  live SAW-OFF.md (rejects docs/)"
rb_expect E4  "$(printf 'refs/tags/PROD_Portable_v0.1.2\nrefs/tags/archive/aberp-git/PROD_Defense_v0.2.1\nrefs/heads/main\n' \
                   | edition_ref_filter | grep -c .)" 1 "E4  release ref (rejects archive/ + main)"
rb_health E0 "$cerr"
if ! rb_controls_ok || ! rb_live_ok; then echo; echo "CUT-GATE: ✗ FAILED (scanner liveness)"; exit 1; fi

echo "[CHECK E1] no Portable/Defense launcher under run/ (ENFORCED)"
n=0; err="$RB_DIR/e1.err"; : > "$err"
while IFS= read -r p; do
  [[ -z "$p" ]] && continue
  flag "✗ edition launcher re-added: $p — the saw-off removed the Portable/Defense launchers from this repo"
  n=$((n+1))
done < <(arm_e1 "$ROOT" "$err" | sort)
rb_health E1 "$err"
[[ "$n" -eq 0 ]] && note "✓ no edition launcher under run/"

echo "[CHECK E2] no Portable/Defense edition crate (ENFORCED)"
n=0; err="$RB_DIR/e2.err"; : > "$err"
while IFS= read -r d; do
  [[ -z "$d" ]] && continue
  flag "✗ edition crate re-added: $d/ — Portable/Defense build surface does not live in ABERP.git"
  n=$((n+1))
done < <(arm_e2_dir "$ROOT" "$err" | sort)
# A crate can also be re-added under an innocuous DIRECTORY name; the package
# `name =` declaration in its manifest is the thing that cannot be disguised.
while IFS= read -r ln; do
  [[ -z "$ln" ]] && continue
  flag "✗ edition package declared: $ln"
  n=$((n+1))
done < <(arm_e2_decl "$ROOT" "$err")
rb_health E2 "$err"
[[ "$n" -eq 0 ]] && note "✓ no edition crate directory or package declaration"

echo "[CHECK E3] no live SAW-OFF.md artifact (ENFORCED)"
n=0; err="$RB_DIR/e3.err"; : > "$err"
while IFS= read -r p; do
  [[ -z "$p" ]] && continue
  flag "✗ saw-off manifest re-added: $p — the saw-off is recorded in adr/0093 and docs/PRUNED_*_REFS.md, not as a live root artifact"
  n=$((n+1))
done < <(arm_e3 "$ROOT" "$err" | sort)
rb_health E3 "$err"
[[ "$n" -eq 0 ]] && note "✓ no SAW-OFF.md outside the docs/adr historical record"

echo "[CHECK E4] no PROD_(Portable|Defense)_v* ref, local or on origin (ENFORCED)"
n=0
refs() {
  git for-each-ref --format='%(refname)' 2>/dev/null
  git ls-remote --refs origin 2>/dev/null | awk '{print $2}'
}
while IFS= read -r r; do
  [[ -z "$r" ]] && continue
  flag "✗ edition release ref present: $r — the saw-off pruned the Portable/Defense lines from ABERP.git"
  n=$((n+1))
done < <(refs | edition_ref_filter | sort -u)
[[ "$n" -eq 0 ]] && note "✓ no PROD_(Portable|Defense)_v* ref (archive/ namespaces exempt)"

# Harvest find-health across the LIVE E1–E3 scans: if any of them errored, the
# "no surface found" verdict above was built on an incomplete scan and cannot be
# trusted. Always enforced, regardless of ENFORCE_EDITION_RATCHET.
if ! rb_live_ok; then echo; echo "CUT-GATE: ✗ FAILED (a live scan did not complete cleanly)"; exit 1; fi

echo
if [[ "$fail" -ne 0 ]]; then echo "CUT-GATE: ✗ FAILED"; exit 1; fi
echo "CUT-GATE: ✓ PASSED"
