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
# ENFORCE_EDITION_RATCHET=0 disables (local probe). EDITION_RATCHET_ROOT overrides
# the tree under test — used ONLY by tools/cut_gate_edition_ratchet_probes.sh to
# run the arms against synthetic trees; CI never sets it.
#
# Exit 0 = gate green (saw-off holds). Non-zero = Portable/Defense surface is back.

set -uo pipefail
ROOT="${EDITION_RATCHET_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT" || { echo "CUT-GATE: ✗ FAILED (no such root: $ROOT)"; exit 1; }
fail=0
note() { printf '  %s\n' "$*"; }
echo "ADR-0093 product-line saw-off ratchet — root: $ROOT"

enforce="${ENFORCE_EDITION_RATCHET:-1}"
flag() { note "$1"; if [[ "$enforce" == "1" ]]; then fail=1; else note "  (enforcement disabled — not failing)"; fi; }

EDITION='(portable|defense)'

echo "[CHECK E1] no Portable/Defense launcher under run/ (ENFORCED)"
n=0
while IFS= read -r p; do
  [[ -z "$p" ]] && continue
  flag "✗ edition launcher re-added: $p — the saw-off removed the Portable/Defense launchers from this repo"
  n=$((n+1))
done < <(find run -type f 2>/dev/null | grep -iE "$EDITION" | sort)
[[ "$n" -eq 0 ]] && note "✓ no edition launcher under run/"

echo "[CHECK E2] no Portable/Defense edition crate (ENFORCED)"
n=0
while IFS= read -r d; do
  [[ -z "$d" ]] && continue
  flag "✗ edition crate re-added: $d/ — Portable/Defense build surface does not live in ABERP.git"
  n=$((n+1))
done < <(find crates apps -mindepth 1 -maxdepth 1 -type d 2>/dev/null | grep -iE "/aberp-$EDITION$" | sort)
# A crate can also be re-added under an innocuous DIRECTORY name; the package
# `name =` declaration in its manifest is the thing that cannot be disguised.
while IFS= read -r m; do
  [[ -z "$m" ]] && continue
  ln="$(grep -nEi '^[[:space:]]*name[[:space:]]*=[[:space:]]*"aberp-(portable|defense)"' "$m" | head -1)"
  [[ -z "$ln" ]] && continue
  flag "✗ edition package declared: $m:${ln%%:*} — ${ln#*:}"
  n=$((n+1))
done < <(find crates apps -maxdepth 2 -name Cargo.toml 2>/dev/null | sort)
[[ "$n" -eq 0 ]] && note "✓ no edition crate directory or package declaration"

echo "[CHECK E3] no live SAW-OFF.md artifact (ENFORCED)"
n=0
while IFS= read -r p; do
  [[ -z "$p" ]] && continue
  flag "✗ saw-off manifest re-added: $p — the saw-off is recorded in adr/0093 and docs/PRUNED_*_REFS.md, not as a live root artifact"
  n=$((n+1))
done < <(find . -path ./.git -prune -o -path ./docs -prune -o -path ./adr -prune \
           -o -name target -prune -o -name node_modules -prune \
           -o -type f -iname 'SAW-OFF.md' -print 2>/dev/null | sort)
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
done < <(refs | grep -E '(^|/)PROD_(Portable|Defense)_v' | grep -v '/archive/' | sort -u)
[[ "$n" -eq 0 ]] && note "✓ no PROD_(Portable|Defense)_v* ref (archive/ namespaces exempt)"

echo
if [[ "$fail" -ne 0 ]]; then echo "CUT-GATE: ✗ FAILED"; exit 1; fi
echo "CUT-GATE: ✓ PASSED"
