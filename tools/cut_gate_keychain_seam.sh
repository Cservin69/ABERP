#!/usr/bin/env bash
#
# cut_gate_keychain_seam.sh — ADR-0100 Phase 1 keychain-seam cut-gate.
#
# Phase 1 introduced the `aberp-secret-store::SecretStore` seam and routed all
# ten direct keychain sites through it (ADR-0100 §5). The invariant "no direct
# keychain access outside the seam" HELD at the cut, but nothing ENFORCED it
# against future drift. This gate is that enforcement — it mirrors the ADR-0099
# opener-census cut-gate style (toolchain-free bash + a comment/string-aware awk
# scanner + a negative-probe teeth harness in tools/cut_gate_keychain_seam_probes.sh).
#
# CHECK K — zero direct keychain access outside the seam. Scans every runtime
# `.rs` under apps/*/src, modules, crates EXCEPT:
#   * crates/aberp-secret-store/  — THE SEAM: it legitimately owns `keyring`
#     (its `keyring::Entry` calls ARE the sanctioned access, not a residual).
#   * */tests/*                   — the designated integration-test mocks
#     (nav_credentials_blob.rs, serve_setup_nav_credentials_route.rs,
#      secrets_cache_boot.rs, serve_settings_routes.rs) build `keyring::Entry`
#      mock backends / seed legacy entries; that is test scaffolding, not a
#      runtime seam bypass.
# The scanner also skips #[cfg(test)] regions inside src files (inline test
# mocks never run in production). ENFORCE_KEYCHAIN_SEAM=0 disables (local probe).
#
# Exit 0 = gate green (seam holds). Non-zero = a direct keychain access appeared
# outside the seam — route it through `aberp_secret_store::SecretStore`.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail=0
note() { printf '  %s\n' "$*"; }
echo "ADR-0100 Phase 1 keychain-seam cut-gate — root: $ROOT"

SCAN="tools/adr0100_keychain_seam_scan.awk"
BACKSTOP="tools/cut_gate_scanner_backstop.sh"
for req in "$SCAN" "$BACKSTOP"; do
  [[ -f "$req" ]] || { note "✗ FAIL: required gate asset missing: $req"; echo; echo "CUT-GATE: ✗ FAILED"; exit 1; }
done
# shellcheck source=tools/cut_gate_scanner_backstop.sh
source "$BACKSTOP"; bs_init

# ── CHECK K0 — scanner liveness (ALWAYS ENFORCED, not covered by ENFORCE_*) ──
# This gate was "zero hits ⇒ green": a crashed or silent scanner PASSED it. And
# on 2026-07-21 the scanner was proven blind for real — the same planted
# `keyring::Entry::new` FAILED the gate at tenant_registry.rs:5 and PASSED it at
# :700, shielded by the `out.push('"')` char literal at :615. The two lexer-trap
# controls below are that exploit, frozen as a permanent tripwire.
echo "[CHECK K0] scanner liveness — the scanner sees planted bypasses, incl. behind lexer traps"
bs_check "$SCAN" 1 "positive: direct keyring::Entry::new" <<'RS'
fn control() { let _e = keyring::Entry::new(s, a).unwrap(); }
RS
bs_check "$SCAN" 1 "positive: bypass shielded by a char literal — out.push('\"')" <<'RS'
fn control() {
    out.push('"');
    let _e = keyring::Entry::new(s, a).unwrap();
}
RS
bs_check "$SCAN" 1 "positive: bypass shielded by a raw string — r##\"a \"# b\"##" <<'RS'
fn control() {
    let s = r##"a "# b"##;
    let _e = keyring::Entry::new(s, a).unwrap();
}
RS
bs_check "$SCAN" 1 "positive: bypass shielded by a MULTI-LINE raw string" <<'RS'
fn control() {
    let s = r#"line one " still raw
       still raw "#;
    let _e = keyring::Entry::new(s, a).unwrap();
}
RS
bs_check "$SCAN" 0 "negative: comment + string mention only (must NOT hit)" <<'RS'
fn control() {
    // keyring::Entry::new and e.get_password() in a comment
    let _ = "keyring::Entry in a string";
}
RS
bs_controls_ok || { echo; echo "CUT-GATE: ✗ FAILED (scanner liveness)"; exit 1; }
echo

# Scope: runtime Rust outside the seam crate and the integration-test dirs.
# The seam crate (crates/aberp-secret-store) OWNS `keyring` — its access is the
# fix, not a residual — so it is excluded, exactly as the opener-census gate
# excludes the shared-Handle seam crate (crates/aberp-db).
scope_files() {
  # `apps/*/src` — same idiom as the three ADR-0099 gates since finding F5
  # (they had hardcoded apps/aberp/src only and missed apps/aberp-ui/src; this
  # gate already covered both, but the glob keeps all four in one shape and
  # cannot be re-narrowed by a future app appearing).
  find apps/*/src modules crates -name '*.rs' \
    | grep -vE '/tests/|/aberp-secret-store/' | sort
}

echo "[CHECK K] zero direct keychain access outside the aberp-secret-store seam (ENFORCED)"
enforceK="${ENFORCE_KEYCHAIN_SEAM:-1}"
flagK() { note "$1"; if [[ "$enforceK" == "1" ]]; then fail=1; else note "  (enforcement disabled — not failing)"; fi; }
hits=0
while IFS= read -r f; do
  while IFS= read -r rec; do
    [[ -z "$rec" ]] && continue
    ln="${rec%%:*}"; txt="${rec#*:}"
    flagK "✗ direct keychain access $f:$ln — route it through aberp_secret_store::SecretStore:"
    note "      $txt"
    hits=$((hits+1))
  done < <(bs_scan "$SCAN" "$f")
done < <(scope_files)
bs_scan_ok || fail=1
if [[ "$hits" -eq 0 ]]; then
  nf="$(scope_files | wc -l | tr -d ' ')"
  note "✓ seam holds — zero direct keychain access across $nf runtime file(s) outside the seam"
fi

echo
if [[ "$fail" -ne 0 ]]; then echo "CUT-GATE: ✗ FAILED"; exit 1; fi
echo "CUT-GATE: ✓ PASSED"
