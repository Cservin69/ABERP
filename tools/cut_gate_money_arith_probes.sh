#!/usr/bin/env bash
#
# cut_gate_money_arith_probes.sh — proves ADR-0108 T-8
# (`tools/cut_gate_money_arith.sh`) has TEETH.
#
# Every cut-gate in this tree ships a `_probes.sh` companion, and T-8 needs one
# more than most: it is a gate that is GREEN on the tree it lands in, with its
# four real sites parked in a register. A green gate nobody has watched go red
# is indistinguishable from a gate that cannot see. Step 6 found T-8 cited by
# three landed artefacts and implemented by none — this file is what stops the
# next reviewer having to take the implementation on trust either.
#
# Each probe plants ONE synthetic instance of the defect class into a THROWAWAY
# COPY of the tree under $TMPDIR, asserts the gate goes RED on the named arm,
# then the unmutated copy is re-asserted GREEN. No real source file is ever
# mutated, so the probes stay valid as the tree moves.
#
#   P1   `SUM(unit_price * quantity)` — the §3.4 float-money shape
#   P2   `COALESCE(stock_qty,0) < COALESCE(min_stock,0)` — the wrapped
#        lexicographic comparison whose operand next to `<` is a `)`, not a
#        column. The shape a naive grep structurally cannot see (F2).
#   P3   `ORDER BY (COALESCE(stock_qty,0) - COALESCE(min_stock,0))` — the same
#        statement's REAL-coercing deficit ordering, mutated SEPARATELY,
#        because reddening on one of §3.4's two breaks is not evidence of
#        coverage for the other (F2 again, explicitly).
#   P4   the same predicate in `count_low_stock_products` shape, i.e. a SECOND
#        statement in a second function — §3.4's `:585` lesson
#   P5   arithmetic added to a `.sql` MIGRATION file — the 7 `.sql` files are
#        §1.1 G-2's finding: both source documents grepped `*.rs` only
#   P6   the column and the operator on DIFFERENT PHYSICAL LINES of one literal
#        — the multi-line requirement, which a line-oriented grep fails
#   P7   `exchange_rate` divided, in a lower-case `r#"..."#` raw literal — case
#        and raw-string awareness in one probe
#   P8   a fold lands but its register entry is left behind — the RATCHET must
#        red (T8-4), or the register becomes a permanent allowlist
#   P9   the register is emptied — the four real sites must red (T8-3), which
#        proves the register is consulted rather than assumed
#   P10  the scanner is DELETED — the gate must refuse, not go green (T8-1)
#   P11  the scanner is NEUTERED (its `emit` is stubbed out) — T8-2's liveness
#        must catch it. This is the F4 silent-green shape: a rule-less scanner
#        reports zero findings, which reads exactly like a clean tree.
#   P12  `received_quantity = received_quantity + ?` and `ORDER BY id` must
#        stay GREEN — `_` is a word character, and a gate that cries wolf on
#        purchasing gets switched off within a week
#   P13  the hazard inside a `#[cfg(test)]` item must stay GREEN — the tree's
#        own proofs contain the forbidden shapes on purpose
#
# Exit 0 = every probe behaved.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="tools/cut_gate_money_arith.sh"
SCAN="tools/adr0108_money_arith_scan.awk"
REG="tools/adr0108_money_arith_pending_folds.txt"
PLANT="crates/aberp-db/src/schema.rs"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/moneyarith-probes.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0; bad=0

# A copy holding only what the gate reads: its own assets and the scanned scope.
fresh() {
  local d; d="$(mktemp -d "$WORK/copy.XXXXXX")"
  ( cd "$ROOT" && tar -cf - tools apps/*/src crates modules --exclude='target' 2>/dev/null ) \
    | tar -C "$d" -xf - 2>/dev/null
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
  if [[ "$rc" != "0" ]]; then
    printf '  ✗ FALSE ALARM: %s — gate FAILED on a benign tree\n' "$label"
    sed 's/^/      /' "$dir/gate.out"; bad=$((bad+1)); return
  fi
  printf '  ✓ stays green: %s\n' "$label"; pass=$((pass+1))
}

# Guards the probe against being vacuous: if the mutation did not land, the
# gate's green means nothing and the probe must say so rather than count itself.
planted() {
  local dir="$1" file="$2" needle="$3" label="$4"
  grep -qF "$needle" "$dir/$file" && return 0
  printf '  ! %s could not plant its mutation — probe is vacuous\n' "$label"
  bad=$((bad+1)); return 1
}

echo "ADR-0108 T-8 money/quantity SQL-arithmetic gate — probes"

# --- baseline ----------------------------------------------------------------
d="$(fresh)"; expect_green "the unmutated tree" "$d"

# --- P1: the §3.4 float-money shape ------------------------------------------
d="$(fresh)"
cat >> "$d/$PLANT" <<'EOF'

pub fn t8_probe_p1(c: &crate::engine::Connection) -> crate::engine::Result<i64> {
    c.query_row(
        "SELECT SUM(il.unit_price * il.quantity) FROM invoice_line il WHERE il.invoice_id = ?",
        [],
        |r| r.get(0),
    )
}
EOF
planted "$d" "$PLANT" "unit_price * il.quantity" "P1" && \
  expect_red "P1 \`SUM(unit_price * quantity)\` — float money" "$d" "FAIL (T8-3)"

# --- P2: the wrapped lexicographic comparison --------------------------------
d="$(fresh)"
cat >> "$d/$PLANT" <<'EOF'

pub fn t8_probe_p2(c: &crate::engine::Connection) -> crate::engine::Result<usize> {
    c.execute(
        "SELECT id FROM products WHERE COALESCE(stock_qty, 0) < COALESCE(min_stock, 0)",
        [],
    )
}
EOF
expect_red "P2 wrapped \`COALESCE(stock_qty,0) < COALESCE(min_stock,0)\`" "$d" "cmp-lt"

# --- P3: the same statement's deficit ORDER BY, mutated separately -----------
d="$(fresh)"
cat >> "$d/$PLANT" <<'EOF'

pub fn t8_probe_p3(c: &crate::engine::Connection) -> crate::engine::Result<usize> {
    c.execute(
        "SELECT id FROM products
          ORDER BY (COALESCE(stock_qty, 0) - COALESCE(min_stock, 0)) ASC, name ASC",
        [],
    )
}
EOF
expect_red "P3 the deficit \`ORDER BY (… - …)\` (separate mutation)" "$d" "arith-sub"

# --- P4: a SECOND statement in a SECOND function -----------------------------
d="$(fresh)"
cat >> "$d/$PLANT" <<'EOF'

pub fn t8_probe_p4_count(c: &crate::engine::Connection) -> crate::engine::Result<u32> {
    c.query_row(
        "SELECT COUNT(*) FROM products WHERE COALESCE(stock_qty, 0) < COALESCE(min_stock, 0)",
        [],
        |r| r.get(0),
    )
}
EOF
expect_red "P4 the predicate's twin in a second function (the \`:585\` lesson)" "$d" "t8_probe_p4_count"

# --- P5: a .sql MIGRATION file (§1.1 G-2 — both source docs missed these) ----
d="$(fresh)"
cat >> "$d/crates/aberp-inventory/migrations/V001__inventory.sql" <<'EOF'

CREATE VIEW t8_probe_p5 AS
  SELECT product_id, SUM(qty_delta) * 2 AS doubled
    FROM stock_movements
   GROUP BY product_id;
EOF
expect_red "P5 arithmetic added to a .sql migration file" "$d" "V001__inventory.sql"

# --- P6: the column and the operator on DIFFERENT LINES ----------------------
d="$(fresh)"
cat >> "$d/$PLANT" <<'EOF'

pub fn t8_probe_p6(c: &crate::engine::Connection) -> crate::engine::Result<i64> {
    c.query_row(
        "SELECT SUM(il.quantity
                    * il.unit_price)
           FROM invoice_line il",
        [],
        |r| r.get(0),
    )
}
EOF
expect_red "P6 operand and operator on different physical lines" "$d" "t8_probe_p6"

# --- P7: lower-case SQL inside a raw `r#\"…\"#` literal ------------------------
d="$(fresh)"
cat >> "$d/$PLANT" <<'EOF'

pub fn t8_probe_p7(c: &crate::engine::Connection) -> crate::engine::Result<i64> {
    c.query_row(
        r#"select huf_equivalent_total / exchange_rate from invoice where id = ?"#,
        [],
        |r| r.get(0),
    )
}
EOF
expect_red "P7 lower-case division inside a raw \`r#\"…\"#\` literal" "$d" "arith-div"

# --- P8: a fold lands, its register entry does not ---------------------------
# The RATCHET. Both inventory SUMs are folded away here; their two register
# entries then match nothing and must red, or the register becomes permanent.
d="$(fresh)"
perl -0pi -e 's/COALESCE\(SUM\(qty_delta\), 0\)/COALESCE(qty_delta, 0)/g' "$d/crates/aberp-inventory/src/repository.rs"
# NB: the guard asks for the AGGREGATE, not the bare string — `repository.rs`
# also names `SUM(qty_delta)` inside an error message, and grepping for that
# would report a successful fold as a vacuous probe.
if grep -qF "COALESCE(SUM(qty_delta), 0)" "$d/crates/aberp-inventory/src/repository.rs"; then
  printf '  ! P8 could not fold the sites — probe is vacuous\n'; bad=$((bad+1))
else
  expect_red "P8 a landed fold with its register entry left behind" "$d" "FAIL (T8-4)"
fi

# --- P9: the register is emptied ---------------------------------------------
d="$(fresh)"
: > "$d/$REG"
expect_red "P9 an emptied register — the four real sites must surface" "$d" "FAIL (T8-3)"

# --- P10: the scanner is deleted ---------------------------------------------
d="$(fresh)"; rm -f "$d/$SCAN"
expect_red "P10 the scanner is deleted — refuse, do not go green" "$d" "FAIL (T8-1)"

# --- P11: the scanner is NEUTERED (F4 silent-green) --------------------------
# A rule-less scanner reports zero findings, which reads exactly like a clean
# tree. T8-2's liveness fixture is the only thing between that and a green cut.
d="$(fresh)"
perl -0pi -e 's/function emit\(arm, cls\) \{ HITS\[cls "\|" arm\] = 1 \}/function emit(arm, cls) { }/' "$d/$SCAN"
if grep -qF 'HITS[cls "|" arm] = 1' "$d/$SCAN"; then
  printf '  ! P11 could not neuter the scanner — probe is vacuous\n'; bad=$((bad+1))
else
  expect_red "P11 a neutered scanner that finds nothing" "$d" "FAIL (T8-2)"
fi

# --- P12: benign shapes must NOT trip ----------------------------------------
d="$(fresh)"
cat >> "$d/$PLANT" <<'EOF'

pub fn t8_probe_p12(c: &crate::engine::Connection) -> crate::engine::Result<usize> {
    c.execute(
        "UPDATE purchase_order_lines SET received_quantity = received_quantity + ?3
          WHERE id = ?1",
        [],
    )?;
    c.execute("SELECT id, quantity, unit_price FROM invoice_line ORDER BY id", [])
}
EOF
expect_green "P12 \`received_quantity + ?\` and \`ORDER BY id\` (no false alarm)" "$d"

# --- P13: the hazard inside a `#[cfg(test)]` item must not trip ---------------
d="$(fresh)"
cat >> "$d/$PLANT" <<'EOF'

#[cfg(test)]
mod t8_probe_p13 {
    #[test]
    fn the_r2_hazard_is_demonstrated_not_committed() {
        let sql = "SELECT SUM(quantity * unit_price) FROM invoice_line";
        assert!(sql.contains("SUM"));
    }
}
EOF
expect_green "P13 the hazard inside a \`#[cfg(test)]\` item (proof, not prohibition)" "$d"

echo
if [[ "$bad" -eq 0 ]]; then echo "PROBES: ✓ $pass/$pass behaved"; exit 0; fi
echo "PROBES: ✗ $bad probe(s) misbehaved ($pass ok)"
exit 1
