#!/usr/bin/env bash
#
# cut_gate_nav_emit_door_probes.sh — proves the ADR-0106 NAV-emission door gate
# has TEETH.
#
# A gate never observed catching its own target is decoration. Each probe below
# plants ONE synthetic instance of the defect class into a THROWAWAY COPY of the
# tree, asserts the gate goes RED, then asserts the unmutated copy is GREEN. No
# real source file is ever mutated, so the probes stay valid as the tree moves.
#
# The probes are the defect class, one per historical instance:
#
#   P1  a synthetic route emits a NAV body with no preflight   (the headline)
#   P2  a new handler calls an existing NAV helper             (Editions PR #28)
#   P3  the preflight call is deleted from the issue route     (F1)
#   P4  an EIGHTH nav_xml emitter appears                      (emitter growth)
#   P5  the emitter is reached through a `use … as` alias      (alias evasion)
#   P6  the scanner itself is broken                           (F4 silent-green)
#   P7  an unrelated non-NAV route is added                    (no false alarm)
#
# Exit 0 = every probe behaved.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="tools/cut_gate_nav_emit_door.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/navdoor-probes.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0; bad=0

# A copy holding only what the gate reads: its own assets plus the scanned
# scope. Copying the whole tree would drag target/ and node_modules/ along.
fresh() {
  local d; d="$(mktemp -d "$WORK/copy.XXXXXX")"
  ( cd "$ROOT" && tar -cf - tools apps/*/src modules crates ) | tar -C "$d" -xf -
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
  if [[ "$rc" == "0" ]]; then printf '  ✓ green: %s\n' "$label"; pass=$((pass+1))
  else
    printf '  ✗ FALSE ALARM: %s — gate FAILED with nothing planted\n' "$label"
    sed 's/^/      /' "$dir/gate.out"; bad=$((bad+1))
  fi
}

echo "negative probes for the ADR-0106 NAV-emission door gate (synthetic, throwaway copies)"
echo "root: $ROOT"; echo

# ── P0 — the control: an unmutated copy must be GREEN ────────────────────────
# Without this, every "reds" result below could just mean "the gate is always
# red", which proves nothing.
base="$(fresh)"
expect_green "P0 control — unmutated tree" "$base"

# ── P1 — THE HEADLINE MUTATION ──────────────────────────────────────────────
# A synthetic route that constructs and emits a NAV invoice body, reaching the
# emitter directly, with no preflight anywhere. This is the shape the gate was
# built to make impossible, so this probe is the gate's reason to exist.
d="$(fresh)"
cat > "$d/apps/aberp/src/zz_navdoor_scratch.rs" <<'RS'
//! Synthetic probe route — NOT part of the product.
use crate::nav_xml;

/// A NAV body straight to the wire. No `validate_invoice_preflight` anywhere
/// on this path.
pub async fn handle_backdoor_issue(request: IssueInvoiceRequest) -> Response {
    let xml = nav_xml::render_invoice_data(&request.invoice, &series, &parties, Currency::Huf, None)
        .expect("emit");
    submit(xml).await
}
RS
expect_red "P1 synthetic route emits a NAV body with no preflight" "$d" "CHECK N1"
expect_red "P1 (same mutation, closure arm) unregistered NAV door" "$d" "UNREGISTERED NAV door"

# ── P2 — the Editions PR #28 shape ──────────────────────────────────────────
# The realistic version: the new door does NOT construct a body itself, it
# calls an existing helper that does. An emitter-only census would be blind to
# this; the reaching set is why it is not.
d="$(fresh)"
cat > "$d/apps/aberp/src/zz_navdoor_scratch.rs" <<'RS'
//! Synthetic probe route — NOT part of the product.

/// New handler reusing the existing storno helper. Constructs no NAV body of
/// its own, yet reaches NAV filing all the same.
pub async fn handle_bulk_storno(state: &AppState, ids: Vec<String>) -> Response {
    for id in ids {
        let _ = storno_invoice_request(state, &id, Default::default());
    }
    ok()
}
RS
expect_red "P2 new handler reaches NAV via an existing helper (no body of its own)" "$d" "UNREGISTERED NAV door"

# ── P3 — the F1 shape: the choke point is removed from a `direct` door ──────
d="$(fresh)"
perl -0pi -e 's/^(\s*)let preflight = validate_invoice_preflight\(&request\);/$1let preflight: Vec<InvoicePreflightError> = Vec::new();/m' \
  "$d/apps/aberp/src/serve.rs"
grep -q 'let preflight = validate_invoice_preflight' "$d/apps/aberp/src/serve.rs" \
  && { echo "  ✗ PROBE SETUP FAILED: P3 did not remove the preflight call"; bad=$((bad+1)); }
expect_red "P3 preflight call deleted from the issue route (record set)" "$d" "CHECK N1"
expect_red "P3 preflight call deleted from the issue route (declared-direct arm)" "$d" "declared 'direct'"

# ── P4 — an EIGHTH NAV body emitter appears in nav_xml.rs ───────────────────
d="$(fresh)"
cat >> "$d/apps/aberp/src/nav_xml.rs" <<'RS'

/// Synthetic probe emitter — NOT part of the product.
pub fn render_settlement_data(reference: &AnnulmentReference) -> Result<Vec<u8>> {
    todo!()
}
RS
expect_red "P4 an eighth nav_xml wire-body emitter is added" "$d" "CHECK N1"

# ── P5 — alias evasion ──────────────────────────────────────────────────────
# `use crate::nav_xml::render_storno_data as emit;` then `emit(...)`. The
# ADR-0098 opener scanner was evadable exactly this way before R4 finding H.
d="$(fresh)"
cat > "$d/apps/aberp/src/zz_navdoor_scratch.rs" <<'RS'
//! Synthetic probe route — NOT part of the product.
use crate::nav_xml::render_storno_data as emit;

pub fn handle_aliased_storno(storno: &Storno) -> Vec<u8> {
    emit(storno, &series, &parties, &reference, Currency::Huf, None).unwrap()
}
RS
expect_red 'P5 emitter reached through a `use … as` alias' "$d" "UNREGISTERED NAV door"

# ── P6 — a BROKEN scanner must not read as a clean tree (finding F4) ────────
# Before the backstop existed, three gates in this tree scored "0 hits ⇒ green"
# on a scanner that had stopped working. N0 is the reason this one cannot.
d="$(fresh)"
printf '\nthis is not valid awk {{{\n' >> "$d/tools/adr0106_nav_door_scan.awk"
expect_red "P6 a broken scanner fails the gate instead of silently passing it" "$d" "SCANNER BROKEN"

# A scanner that parses but has had its rules gutted is the subtler half: it
# emits nothing, so N1 sees an empty record set against a 29-line baseline.
d="$(fresh)"
printf 'BEGIN{exit 0}\n' > "$d/tools/adr0106_nav_door_scan.awk"
expect_red "P6b a silent (rule-less) scanner fails the gate" "$d" "CUT-GATE: ✗ FAILED"

# ── P7 — no false alarm on an unrelated route ───────────────────────────────
# A gate that reds on every new route gets switched off. This pins that the
# blast radius is NAV-reaching code only.
d="$(fresh)"
cat > "$d/apps/aberp/src/zz_navdoor_scratch.rs" <<'RS'
//! Synthetic probe route — NOT part of the product. Touches nothing NAV.
pub async fn handle_list_widgets(state: &AppState) -> Response {
    let widgets = state.db.read().query_widgets().unwrap();
    Json(widgets).into_response()
}
RS
expect_green "P7 an unrelated non-NAV route does not red the gate" "$d"

echo
printf 'probes: %s passed, %s failed\n' "$pass" "$bad"
[[ "$bad" -eq 0 ]] || { echo "PROBES: ✗ FAILED — the gate does not have the teeth it claims"; exit 1; }
echo "PROBES: ✓ PASSED — every arm of the gate was observed catching its target"
