#!/usr/bin/env bash
#
# cut_gate_nav_emit_door.sh — ADR-0106 NAV-emission door gate.
#
# THE DEFECT CLASS THIS EXISTS FOR. Repeatedly, a code path reached NAV filing
# while bypassing the single validation choke point `validate_invoice_preflight`
# — F1 (the ABERP_DB door), the Editions modification route (Editions PR #28),
# and the deferred Invariants P and C. Those are not three bugs, they are three
# instances of one shape: NAV bodies can be constructed from more than one
# place, and nothing enumerated the places.
#
# WHAT THIS GATE DOES. It makes the set of places a CLOSED, WRITTEN-DOWN,
# machine-diffed set, and it machine-checks the one door that claims to
# preflight:
#
#   CHECK N0 (liveness)     — the scanner is handed synthetic controls, both
#                             directions, including the two LEXER TRAPS that
#                             were live fail-opens elsewhere in this tree.
#                             ALWAYS enforced; a dead scanner is a broken tool,
#                             not a policy question. (finding F4)
#   CHECK N1 (record freeze)— the exact set of <file>|<fn>:<symbol> records may
#                             not add / remove / content-swap.
#                             ENFORCE_NAV_DOOR_RECORDS=0 disables (local probe).
#   CHECK N2 (closure)      — every function observed calling a reaching-set
#                             symbol is itself either in the reaching set or a
#                             registered DOOR. This is the arm that makes the
#                             class structurally un-reintroducible: a new route
#                             to NAV must call something censused, and the
#                             instant it does it is an unregistered caller.
#                             ENFORCE_NAV_DOOR_CLOSURE=0 disables (local probe).
#   CHECK N3 (preflight)    — every door declared `direct` must have a
#                             `validate_invoice_preflight` call observed INSIDE
#                             that function. Deleting the preflight call from
#                             the issue route reds N1 and N3 independently.
#                             ENFORCE_NAV_DOOR_PREFLIGHT=0 disables (local probe).
#
# WHAT IT DELIBERATELY DOES NOT DO. It does not assert that every door
# preflights — today two do not (`derived`) and one does not at all (`none`),
# and saying otherwise would be a lie the gate could not hold. Making preflight
# universal is Invariant P, a behaviour change on the highest-consequence path
# in the system and explicitly not this gate's job. See ADR-0106 §5 for why the
# type-level version (a witness token the emitters demand) is the FOLLOW-UP and
# not the thing built here: encoding an invariant that is currently false forces
# an escape hatch, and an escape hatch anyone may call is a scanner with worse
# ergonomics.
#
# Negative probes proving all four checks have teeth:
#   tools/cut_gate_nav_emit_door_probes.sh
#
# Exit 0 = gate green. Non-zero = a NAV door moved.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail=0
note() { printf '  %s\n' "$*"; }
echo "ADR-0106 NAV-emission door cut-gate — root: $ROOT"

SCAN="tools/adr0106_nav_door_scan.awk"
SYMS="tools/adr0106_nav_reach_symbols.txt"
DOORS="tools/adr0106_nav_door_registry.txt"
FPRINTS="tools/adr0106_nav_door_fingerprints.txt"
BACKSTOP="tools/cut_gate_scanner_backstop.sh"
for req in "$SCAN" "$SYMS" "$DOORS" "$FPRINTS" "$BACKSTOP"; do
  [[ -f "$req" ]] || { note "✗ FAIL: required gate asset missing: $req"; fail=1; }
done
[[ "$fail" -ne 0 ]] && { echo; echo "CUT-GATE: ✗ FAILED"; exit 1; }

# shellcheck source=tools/cut_gate_scanner_backstop.sh
. "$BACKSTOP"
bs_init

# Reaching set → the scanner's -v syms list. Built from the file so the two
# can never drift: there is no second copy of this list anywhere.
symlist="$(grep -vE '^[[:space:]]*(#|$)' "$SYMS" | tr -d ' \t' | paste -sd, -)"
if [[ -z "$symlist" ]]; then
  note "✗ FAIL: reaching set $SYMS is empty — the scan would be vacuous"
  echo; echo "CUT-GATE: ✗ FAILED"; exit 1
fi

# Scope: every app/module/crate source outside */tests/*. aberp-ui is IN scope
# on purpose — finding F5 (2026-07-21) was three gates that silently excluded
# apps/aberp-ui/src and scored 0 on a planted bypass there.
scope_files() { find apps/*/src modules crates -name '*.rs' | grep -vE '/tests/' | sort; }

# ── CHECK N0 — scanner liveness (ALWAYS enforced) ────────────────────────────
echo "[CHECK N0] scanner liveness — a planted NAV door must be seen (ALWAYS ENFORCED)"
ctl_syms="render_invoice_data,issue_from_parsed"

# Known-POSITIVE: a bare emit call, an aliased emit call, and a call sitting
# behind each of the two lexer traps that were live fail-opens in this tree.
bs_check "$SCAN" 4 "planted NAV emit doors are found (bare + alias + both lexer traps)" \
  -v syms="$ctl_syms" <<'CTL'
use crate::nav_xml::render_invoice_data as emit;
fn planted_bare() {
    let _ = render_invoice_data(&invoice);
}
fn planted_alias() {
    let _ = emit(&invoice);
}
fn planted_behind_char_literal_trap() {
    // LEXER TRAP 1 — a char literal holding a double quote. Before the
    // ADR-0098 lexer fix this flipped the string state ON and blinded the
    // scanner for the rest of the file.
    out.push('"');
    let _ = issue_from_parsed(&input);
}
fn planted_behind_raw_string_trap() {
    // LEXER TRAP 2 — a raw string holding a stray quote.
    let _ = r#"a stray " inside a raw string"#;
    let _ = issue_from_parsed(&input);
}
CTL

# Known-NEGATIVE: the same tokens in a comment, in a string, and inside a
# #[cfg(test)] module must NOT hit — or "it hits everything" would pass N0 too.
bs_check "$SCAN" 0 "commented / stringified / test-only NAV doors are NOT counted" \
  -v syms="$ctl_syms" <<'CTL'
// render_invoice_data(&invoice);
/// Doc comment mentioning render_invoice_data(&invoice) and issue_from_parsed(&i).
fn only_talks_about_it() {
    let sql = "render_invoice_data(&invoice)";
    let _ = format!("issue_from_parsed({})", x);
}
#[cfg(test)]
mod tests {
    fn test_only_door() {
        let _ = render_invoice_data(&invoice);
        let _ = issue_from_parsed(&input);
    }
}
CTL

# The `#emitter-def` arm must fire on a new emitter and must NOT fire on
# nav-transport's SOAP request builder (the false positive it was tightened for).
bs_check "$SCAN" 1 "a NEW nav_xml emitter definition is caught; a SOAP request builder is not" \
  -v syms="$ctl_syms" <<'CTL'
pub fn render_settlement_data(x: &X) -> Result<Vec<u8>> { todo!() }
pub fn render_query_invoice_data_request(x: &X) -> Result<Vec<u8>> { todo!() }
CTL

bs_controls_ok || fail=1

# ── the scan ─────────────────────────────────────────────────────────────────
cur="$(mktemp)"; froz="$(mktemp)"; obs="$(mktemp)"
while IFS= read -r f; do
  recs="$(bs_scan "$SCAN" "$f" -v syms="$symlist")"
  [[ -z "$recs" ]] && continue
  while IFS= read -r r; do
    printf '%s\n' "$f|${r#*:}"          # <file>|<fn>:<symbol>   (line-number free)
  done <<< "$recs"
done < <(scope_files) | sort -u > "$cur"
bs_scan_ok || fail=1

# ── CHECK N1 — frozen record set ─────────────────────────────────────────────
echo "[CHECK N1] frozen NAV door record set — no add / remove / content-swap (ENFORCED)"
enforceN1="${ENFORCE_NAV_DOOR_RECORDS:-1}"
flagN1() { note "$1"; if [[ "$enforceN1" == "1" ]]; then fail=1; else note "  (enforcement disabled — not failing)"; fi; }
grep -vE '^[[:space:]]*(#|$)' "$FPRINTS" | sort -u > "$froz"
if diff -q "$froz" "$cur" >/dev/null 2>&1; then
  note "✓ record set matches the frozen baseline ($(wc -l < "$froz" | tr -d ' ') records; no NAV door added, removed or swapped)"
else
  flagN1 "✗ NAV door record set DIVERGED from $FPRINTS — a path to NAV filing was added, removed or rerouted:"
  diff "$froz" "$cur" | sed 's/^/      /' | head -40
fi

# ── CHECK N2 — closure: every caller is censused or a registered door ────────
echo "[CHECK N2] closure — every caller of a reaching symbol is in the set or a registered DOOR (ENFORCED)"
enforceN2="${ENFORCE_NAV_DOOR_CLOSURE:-1}"
flagN2() { note "$1"; if [[ "$enforceN2" == "1" ]]; then fail=1; else note "  (enforcement disabled — not failing)"; fi; }
# The reaching set, normalised to the same `<basename>::<fn>` key the doors use.
# A QUALIFIED entry (`issue_storno::run`) satisfies closure ONLY inside its own
# file — otherwise any function anywhere named `run` would inherit the census,
# which is a wide hole given how generic that name is. A BARE entry satisfies
# closure in any file, which is correct: the bare name IS the census key.
grep -vE '^[[:space:]]*(#|$)' "$SYMS" | tr -d ' \t' | while IFS= read -r s; do
  case "$s" in
    *::*) printf '%s.rs::%s\n' "${s%%::*}" "${s##*::}" ;;
    *)    printf '*::%s\n' "$s" ;;
  esac
done | sort -u > "$obs"
n2_fail=0
while IFS= read -r rec; do
  [[ "$rec" == *'#emitter-def' ]] && continue
  file="${rec%%|*}"; rest="${rec#*|}"; caller="${rest%%:*}"
  base="$(basename "$file")"
  grep -qxF "*::${caller}" "$obs" && continue
  grep -qxF "${base}::${caller}" "$obs" && continue
  # `.` in the basename is escaped — an unescaped `serve.rs` would also match
  # `serveXrs`, which is harmless today but is not what the check means.
  grep -qE "^[[:space:]]*${base//./\\.}::${caller}[[:space:]]" "$DOORS" && continue
  flagN2 "✗ UNREGISTERED NAV door: ${base}::${caller} reaches NAV filing (${rest#*:}) but is neither in $SYMS nor declared in $DOORS"
  n2_fail=1
done < "$cur"
if [[ "$n2_fail" == "0" ]]; then
  nd="$(grep -cE '^[[:space:]]*[A-Za-z0-9_.]+::[A-Za-z0-9_]+[[:space:]]' "$DOORS")"
  note "✓ closure holds — every NAV-reaching caller is censused ($nd registered door(s))"
fi

# ── CHECK N3 — a door declared `direct` must actually preflight ──────────────
echo "[CHECK N3] declared-direct doors call validate_invoice_preflight (ENFORCED)"
enforceN3="${ENFORCE_NAV_DOOR_PREFLIGHT:-1}"
flagN3() { note "$1"; if [[ "$enforceN3" == "1" ]]; then fail=1; else note "  (enforcement disabled — not failing)"; fi; }
n3_seen=0; n3_fail=0
while read -r door disp _rest; do
  [[ "$disp" == "direct" ]] || continue
  n3_seen=$((n3_seen + 1))
  fn="${door##*::}"
  if grep -qE "\|${fn}:validate_invoice_preflight$" "$cur"; then
    note "✓ $door calls validate_invoice_preflight"
  else
    flagN3 "✗ $door is declared 'direct' in $DOORS but NO validate_invoice_preflight call was observed inside it — the choke point was removed, renamed, or moved out of the door"
    n3_fail=1
  fi
done < <(grep -E '^[[:space:]]*[A-Za-z0-9_.]+::[A-Za-z0-9_]+[[:space:]]' "$DOORS")
if [[ "$n3_seen" -eq 0 ]]; then
  flagN3 "✗ NO door in $DOORS is declared 'direct' — the choke point has no declared caller at all, which cannot be right while /invoices/issue exists"
elif [[ "$n3_fail" == "0" ]]; then
  note "✓ all $n3_seen declared-direct door(s) hold the choke point"
fi

rm -f "$cur" "$froz" "$obs"

echo
if [[ "$fail" -ne 0 ]]; then echo "CUT-GATE: ✗ FAILED"; exit 1; fi
echo "CUT-GATE: ✓ PASSED"
