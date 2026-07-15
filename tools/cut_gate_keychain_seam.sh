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
[[ -f "$SCAN" ]] || { note "✗ FAIL: required gate asset missing: $SCAN"; echo; echo "CUT-GATE: ✗ FAILED"; exit 1; }

# Scope: runtime Rust outside the seam crate and the integration-test dirs.
# The seam crate (crates/aberp-secret-store) OWNS `keyring` — its access is the
# fix, not a residual — so it is excluded, exactly as the opener-census gate
# excludes the shared-Handle seam crate (crates/aberp-db).
scope_files() {
  find apps/aberp/src apps/aberp-ui/src modules crates -name '*.rs' \
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
  done < <(awk -f "$SCAN" "$f" 2>/dev/null)
done < <(scope_files)
if [[ "$hits" -eq 0 ]]; then
  nf="$(scope_files | wc -l | tr -d ' ')"
  note "✓ seam holds — zero direct keychain access across $nf runtime file(s) outside the seam"
fi

echo
if [[ "$fail" -ne 0 ]]; then echo "CUT-GATE: ✗ FAILED"; exit 1; fi
echo "CUT-GATE: ✓ PASSED"
