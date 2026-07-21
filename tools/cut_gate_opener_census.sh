#!/usr/bin/env bash
#
# cut_gate_opener_census.sh — ADR-0099 (PROD SAFE lane) live-DB opener-census gate.
#
# PORTED, HONESTLY SCOPED subset of the editions ADR-0093/0098 cut-gate
# (tools/cut_gate_db_isolation.sh in Cservin69/ABERP-Editions). The editions gate
# has 10 CHECK families; MOST of them (CHECK 1-9, 10a-10h, 10j) assert the
# presence/shape of DURABILITY + edition-saw-off code — the aberp-db shared
# Handle, crash_safe.rs atomic-rename checkpoint, mirror.rs preserve-and-refuse,
# build_profile Edition binding, SAW-OFF.md, per-edition launchers, storefront
# gating, the PRAGMA disable_checkpoint_on_shutdown on residual openers. NONE of
# that exists in the frozen prod tree yet: it is the durability work (H1/H3),
# a FOLLOW-UP prod session. Porting those checks verbatim would go RED (or force
# durability code into this SAFE lane) — so they are DELIBERATELY NOT ported here.
#
# What IS ported is the census-freeze mechanism (editions CHECK 10i + 10k), which
# needs ZERO durability code and is GREEN on the frozen prod tree by construction:
#   CHECK P1 (count freeze  · editions 10i) — each file's RUNTIME independent-opener
#            count may not EXCEED its frozen baseline, and no NEW opener-bearing
#            file may appear unlisted.  ENFORCE_OPENER_CENSUS=0 disables (local probe).
#   CHECK P2 (fingerprint freeze · editions 10k) — the exact SET of per-opener
#            fingerprints (<file>|<fname>:<text>) may not add/remove/content-swap.
#            Catches a count-preserving intra-file swap 10i cannot see.
#            ENFORCE_OPENER_FINGERPRINTS=0 disables (local probe).
#
# This freezes prod's PRE-H3 opener surface (289 openers / 42 files, ALL currently
# ALLOWED) so it cannot silently GROW while durability lands. It does NOT yet
# require zero openers or a shared Handle. Negative probes proving these checks
# have teeth live in tools/cut_gate_negative_probes.sh.
#
# Exit 0 = gate green. Non-zero = the census grew / an opener changed.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail=0
note() { printf '  %s\n' "$*"; }
echo "ADR-0099 PROD opener-census cut-gate — root: $ROOT"

SCAN="tools/adr0098_opener_scan.awk"
COUNTS="tools/adr0098_prod_frozen_residuals.txt"
FPRINTS="tools/adr0098_prod_opener_fingerprints.txt"
for req in "$SCAN" "$COUNTS" "$FPRINTS"; do
  [[ -f "$req" ]] || { note "✗ FAIL: required gate asset missing: $req"; fail=1; }
done
[[ "$fail" -ne 0 ]] && { echo; echo "CUT-GATE: ✗ FAILED"; exit 1; }

# Scope: apps/aberp/src + modules + crates, minus */tests/*. ADR-0099 H3
# introduces the ONE sanctioned shared-instance seam — `crates/aberp-db` (the
# shared DuckDB `Handle`) — which legitimately owns the single runtime
# `Connection::open` every other site now routes through. Like the editions
# gate, that seam is EXCLUDED from the census (its opener is the fix, not a
# residual); every OTHER runtime opener remains a frozen residual to migrate.
# SCOPE FIX (finding F5, 2026-07-21): this was `apps/aberp/src modules crates`,
# which silently EXCLUDED apps/aberp-ui/src — a crate that resolves the prod DB
# path itself (apps/aberp-ui/src/lib.rs:762 reads $ABERP_DB), so it is squarely
# in the blast radius. A planted `Connection::open($ABERP_DB)` + `DELETE FROM
# invoices` there scored census 0 / read_fork 0 / write_fork 0: the scanner
# detects it fine when run on the file directly, the GATE just never handed it
# over. Now `apps/*/src`, so a future third app cannot re-open the same hole.
# (The keychain-seam gate already covered aberp-ui; these three did not.)
scope_files() { find apps/*/src modules crates -name '*.rs' | grep -vE '/tests/|/aberp-db/' | sort; }

# ── CHECK P1 — frozen census: no file exceeds its count; no new unlisted file ──
echo "[CHECK P1] frozen opener census — no file grows, no new unlisted opener-bearing file (ENFORCED)"
enforceP1="${ENFORCE_OPENER_CENSUS:-1}"
flagP1() { note "$1"; if [[ "$enforceP1" == "1" ]]; then fail=1; else note "  (enforcement disabled — not failing)"; fi; }
p1_fail=0
while IFS= read -r f; do
  actual="$(awk -f "$SCAN" "$f" 2>/dev/null | wc -l | tr -d ' ')"
  [[ "${actual:-0}" -eq 0 ]] && continue
  frozen="$(awk -v p="$f" '$1!="#" && $2==p{print $1}' "$COUNTS")"
  if [[ -z "$frozen" ]]; then
    flagP1 "✗ NEW unaccounted opener-bearing file $f ($actual runtime opener(s)) — add it to $COUNTS (and $FPRINTS) or route it onto the (future) Handle"
    p1_fail=1
  elif [[ "$actual" -gt "$frozen" ]]; then
    flagP1 "✗ $f grew its openers ($actual > frozen $frozen) — the frozen census may not grow"
    p1_fail=1
  fi
done < <(scope_files)
if [[ "$p1_fail" == "0" ]]; then
  ft="$(grep -vE '^#' "$COUNTS" | awk '{s+=$1} END{print s}')"
  ff="$(grep -vcE '^#' "$COUNTS")"
  note "✓ census holds — no file exceeds its frozen count, no new unlisted opener ($ft frozen openers across $ff files)"
fi

# ── CHECK P2 — frozen fingerprint set: no add/remove/content-swap ─────────────
echo "[CHECK P2] frozen opener fingerprints — intra-file count-preserving swap cannot hide (ENFORCED)"
enforceP2="${ENFORCE_OPENER_FINGERPRINTS:-1}"
flagP2() { note "$1"; if [[ "$enforceP2" == "1" ]]; then fail=1; else note "  (enforcement disabled — not failing)"; fi; }
cur="$(mktemp)"; froz="$(mktemp)"
while IFS= read -r f; do
  sigs="$(awk -f "$SCAN" "$f" 2>/dev/null | sed 's/^[0-9]*://')"
  [[ -z "$sigs" ]] && continue
  while IFS= read -r sig; do printf '%s|%s\n' "$f" "$sig"; done <<< "$sigs"
done < <(scope_files) | sort > "$cur"
grep -vE '^#' "$FPRINTS" | sort > "$froz"
if diff -q "$froz" "$cur" >/dev/null 2>&1; then
  note "✓ opener fingerprint set matches the frozen baseline ($(grep -vcE '^#' "$FPRINTS") openers; no add/remove/swap)"
else
  flagP2 "✗ opener fingerprint set DIVERGED from $FPRINTS (an opener was added, removed, or content-swapped):"
  diff "$froz" "$cur" | sed 's/^/      /' | head -40
fi
rm -f "$cur" "$froz"

echo
if [[ "$fail" -ne 0 ]]; then echo "CUT-GATE: ✗ FAILED"; exit 1; fi
echo "CUT-GATE: ✓ PASSED"
