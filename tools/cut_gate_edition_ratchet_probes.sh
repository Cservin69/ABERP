#!/usr/bin/env bash
#
# cut_gate_edition_ratchet_probes.sh — teeth for the ADR-0093 saw-off ratchet.
#
# A gate never observed REFUSING is not a gate. For each arm of
# tools/cut_gate_edition_ratchet.sh this harness re-adds the artifact and asserts
# RED, then removes it and asserts GREEN. It also plants the four KNOWN
# false-positive sources (the prune records, ADR-0093, the version-regex test, the
# kept portable_demo_boot_e2e.rs pin, the worktree branch) and asserts they stay
# GREEN — a gate that cries wolf gets disabled, so the non-triggers are pinned as
# hard as the triggers.
#
# Probes run against SYNTHETIC minimal trees, not tree-copies of the repo: the
# copy-based harnesses drag in the CAD venv and node_modules and leak on
# interrupt. Each tree is a few files plus `git init`, built under a single
# mktemp -d that is trapped for cleanup.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="$ROOT/tools/cut_gate_edition_ratchet.sh"
BACKSTOP="$ROOT/tools/cut_gate_edition_ratchet_backstop.sh"
[[ -f "$GATE" ]]     || { echo "✗ FAIL: gate under test missing: $GATE"; exit 1; }
[[ -f "$BACKSTOP" ]] || { echo "✗ FAIL: liveness backstop missing: $BACKSTOP"; exit 1; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/edition-ratchet-probes.XXXXXX")" || exit 1
trap 'rm -rf "$TMP"' EXIT INT TERM
fail=0
echo "ADR-0093 saw-off ratchet — negative probes"

# A synthetic tree that mirrors the POST-saw-off repo: the legitimate historical
# record present, the live edition surface absent.
new_tree() {
  local t="$TMP/$1"; rm -rf "$t"
  mkdir -p "$t"/{run/tests,crates/aberp-db,apps/aberp/tests,docs,adr,tools}
  cp "$GATE" "$BACKSTOP" "$t/tools/"
  : > "$t/run/run_prod.sh"
  : > "$t/crates/aberp-db/Cargo.toml"
  # The four documented false-positive sources, verbatim in content.
  printf 'git show 6a51d4f:run/run_portable.sh\nPROD_Portable_v0.1.2\n' > "$t/docs/PRUNED_PORTABLE_REFS.md"
  printf 'PROD_Defense_v0.2.1 was the abandoned Defense tip.\n' > "$t/docs/PRUNED_DEFENSE_REFS.md"
  printf 'Defense -> ~/.aberp-defense/, Portable -> ~/.aberp-portable/\n' > "$t/adr/0093-product-line-sawoff-isolation.md"
  printf 'for v in PROD_Portable_v0.1.2 PROD_Defense_v0.2.1; do\n' > "$t/run/tests/upgrade_prod_version_re_test.sh"
  # The deliberately KEPT Portable-by-provenance boot pin.
  printf '// portable demo boot e2e\n' > "$t/apps/aberp/tests/portable_demo_boot_e2e.rs"
  ( cd "$t" && git init -q . && git checkout -q -b main 2>/dev/null
    git -c user.email=p@p -c user.name=p commit -q --allow-empty -m base
    git branch worktree-s1-portable-sawoff-scope ) >/dev/null 2>&1
  echo "$t"
}

run_gate() { ( cd "$1" && EDITION_RATCHET_ROOT="$1" bash "$1/tools/cut_gate_edition_ratchet.sh" ) >"$TMP/out" 2>&1; }

# assert <expect red|green> <tree> <label>
assert() {
  local want="$1" t="$2" label="$3"
  if run_gate "$t"; then got=green; else got=red; fi
  if [[ "$got" == "$want" ]]; then
    printf '  ✓ %-58s %s\n' "$label" "$want"
  else
    printf '  ✗ %-58s expected %s, got %s\n' "$label" "$want" "$got"
    sed 's/^/      /' "$TMP/out"
    fail=1
  fi
}

# --- baseline: the post-saw-off tree, with every known false positive present ---
t="$(new_tree baseline)"
assert green "$t" "baseline post-saw-off tree (prune records + kept e2e pin)"

# --- E1 launcher --------------------------------------------------------------
t="$(new_tree e1)"; : > "$t/run/run_portable.sh"
assert red "$t" "E1  run/run_portable.sh re-added"
rm "$t/run/run_portable.sh"; assert green "$t" "E1  removed again"
t="$(new_tree e1b)"; : > "$t/run/upgrade_defense.sh"
assert red "$t" "E1  run/upgrade_defense.sh re-added"
rm "$t/run/upgrade_defense.sh"; assert green "$t" "E1  removed again"

# --- E2 edition crate ---------------------------------------------------------
t="$(new_tree e2)"; mkdir -p "$t/crates/aberp-portable"
assert red "$t" "E2  crates/aberp-portable/ re-added"
rm -rf "$t/crates/aberp-portable"; assert green "$t" "E2  removed again"
# Disguised directory name — the manifest declaration is what cannot be hidden.
t="$(new_tree e2b)"; mkdir -p "$t/crates/legacy-bits"
printf '[package]\nname = "aberp-defense"\n' > "$t/crates/legacy-bits/Cargo.toml"
assert red "$t" "E2  aberp-defense package under a disguised dir name"
rm -rf "$t/crates/legacy-bits"; assert green "$t" "E2  removed again"

# --- E3 SAW-OFF.md ------------------------------------------------------------
t="$(new_tree e3)"; : > "$t/SAW-OFF.md"
assert red "$t" "E3  SAW-OFF.md re-added at repo root"
rm "$t/SAW-OFF.md"; assert green "$t" "E3  removed again"

# --- E4 refs ------------------------------------------------------------------
t="$(new_tree e4)"; ( cd "$t" && git branch PROD_Portable_v0.1.2 ) >/dev/null 2>&1
assert red "$t" "E4  PROD_Portable_v0.1.2 branch re-added"
( cd "$t" && git branch -D PROD_Portable_v0.1.2 ) >/dev/null 2>&1
assert green "$t" "E4  removed again"
t="$(new_tree e4b)"; ( cd "$t" && git tag PROD_Defense_v0.2.1 ) >/dev/null 2>&1
assert red "$t" "E4  PROD_Defense_v0.2.1 tag re-added"
( cd "$t" && git tag -d PROD_Defense_v0.2.1 ) >/dev/null 2>&1
assert green "$t" "E4  removed again"
# The archive namespace is the sanctioned resting place — must NOT fire.
t="$(new_tree e4c)"; ( cd "$t" && git tag archive/aberp-git/PROD_Portable_v0.1.2 ) >/dev/null 2>&1
assert green "$t" "E4  archive/aberp-git/PROD_Portable_v0.1.2 stays green"

# --- fail-closed: a de-gated scanner must pass the real artifacts -------------
t="$(new_tree degated)"; : > "$t/run/run_portable.sh"
if ( cd "$t" && ENFORCE_EDITION_RATCHET=0 EDITION_RATCHET_ROOT="$t" bash "$t/tools/cut_gate_edition_ratchet.sh" ) >/dev/null 2>&1; then
  printf '  ✓ %-58s green\n' "de-gated (ENFORCE=0) passes a planted artifact"
else
  printf '  ✗ %-58s ENFORCE=0 should not fail\n' "de-gated escape hatch"; fail=1
fi

# --- scanner liveness (CHECK E0) — a blind scanner must FAIL, not pass green ---
# The point of the whole backstop: red must mean red for the SCANNER too, not
# only for a policy hit. So these assert not just RED but RED-for-SCANNER-BROKEN,
# and mutation-verify both directions — break the find, expect broken; restore
# it, expect green. A liveness check that cannot fail is the very hole it closes.

# expect_broken <expect broken|clean> <cmd...> — run the gate <cmd>, require the
# right exit AND, when broken, that it named SCANNER BROKEN (a plain policy RED
# must not be mistaken for a liveness catch).
run_out() { "$@" >"$TMP/out" 2>&1; }
expect_broken() {
  local want="$1" label="$2"; shift 2
  if run_out "$@"; then got=green; else got=red; fi
  if [[ "$want" == clean ]]; then
    if [[ "$got" == green ]]; then printf '  ✓ %-58s green\n' "$label"
    else printf '  ✗ %-58s expected green, got %s\n' "$label" "$got"; sed 's/^/      /' "$TMP/out"; fail=1; fi
    return
  fi
  # want == broken
  if [[ "$got" == red ]] && grep -q 'SCANNER BROKEN' "$TMP/out"; then
    printf '  ✓ %-58s red (SCANNER BROKEN)\n' "$label"
  elif [[ "$got" == red ]]; then
    printf '  ✗ %-58s red but NOT flagged SCANNER BROKEN\n' "$label"; sed 's/^/      /' "$TMP/out"; fail=1
  else
    printf '  ✗ %-58s blind-green — a broken scanner PASSED\n' "$label"; sed 's/^/      /' "$TMP/out"; fail=1
  fi
}

# 1) find binary itself erroring — shim a `find` that exits non-zero and writes
#    stderr, ahead of the real one on PATH. Every arm's find now fails.
t="$(new_tree brokenfind)"
shim="$TMP/brokenbin"; mkdir -p "$shim"
printf '#!/usr/bin/env bash\necho "find: simulated failure" >&2\nexit 1\n' > "$shim/find"; chmod +x "$shim/find"
expect_broken broken "SABOTAGE: an erroring find is caught, not passed" \
  env PATH="$shim:$PATH" EDITION_RATCHET_ROOT="$t" bash "$t/tools/cut_gate_edition_ratchet.sh"
# A broken find must NOT be excusable via the policy escape hatch either.
expect_broken broken "SABOTAGE: ENFORCE=0 does NOT excuse a blind scanner" \
  env PATH="$shim:$PATH" ENFORCE_EDITION_RATCHET=0 EDITION_RATCHET_ROOT="$t" bash "$t/tools/cut_gate_edition_ratchet.sh"
# RESTORE: the same tree with the real find on PATH is clean green.
expect_broken clean "RESTORE: real find on the same tree is green" \
  env EDITION_RATCHET_ROOT="$t" bash "$t/tools/cut_gate_edition_ratchet.sh"

# 2) a live scan root that isn't there — find errors on the missing dir. This is
#    the realistic failure (a moved/renamed run/), no PATH shim involved.
t="$(new_tree norun)"; rm -rf "$t/run"
expect_broken broken "SABOTAGE: missing run/ makes the E1 scan error out" \
  env EDITION_RATCHET_ROOT="$t" bash "$t/tools/cut_gate_edition_ratchet.sh"
mkdir -p "$t/run"; : > "$t/run/run_prod.sh"
expect_broken clean "RESTORE: run/ back → green" \
  env EDITION_RATCHET_ROOT="$t" bash "$t/tools/cut_gate_edition_ratchet.sh"

echo
if [[ "$fail" -ne 0 ]]; then echo "PROBES: ✗ FAILED"; exit 1; fi
echo "PROBES: ✓ PASSED"
