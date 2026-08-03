#!/usr/bin/env bash
#
# cut_gate_money_arith.sh — ADR-0108 §8 **T-8**. No arithmetic on a money,
# quantity or rate column in SQL; no lexicographic comparison of a TEXT-decimal
# one.
#
# WHY THIS IS LOAD-BEARING, not tidiness. §3.1's 2026-07-31 correction measured
# that `STRICT` does **not** protect an R2 (TEXT canonical-decimal) column:
# REAL -> TEXT converts losslessly, so a float written into `quantity` or
# `exchange_rate` is accepted and stringified, and `typeof()` still reads
# `'text'`, so the T-2 sweep is blind to it too. R2's guards are therefore
# exactly two — the Rust-side `Decimal` bind, and this gate. Until it landed,
# R2 had ONE, while three landed artefacts cited a mitigation that had never
# run (Step 6's M-1).
#
# CHECK T8-1 — the scan scope is non-empty and the scanner is present.
# CHECK T8-2 — LIVENESS. The scanner must demonstrably fire on a synthetic
#   fixture carrying each arm, and demonstrably NOT fire on a benign one, in an
#   isolated tmpdir. This is the D2a lesson made structural: a scanner that
#   cannot find its target reports "0 hits", which is indistinguishable from
#   "clean" — so the gate proves the scanner works before believing its silence.
# CHECK T8-3 — every finding in the tree is in the PENDING-FOLD REGISTER.
# CHECK T8-4 — every REGISTER ENTRY still matches a live finding. The register
#   is a RATCHET: it can only shrink. A fold that lands and leaves its entry
#   behind reds the gate, which is what stops "temporary" becoming "sanctioned"
#   (§3.3 makes the same call about N-2's one-entry allowlist).
#
# Scope: `apps/*/src`, `crates/*/src`, `modules/*/src` (`*.rs`, non-test items
# only) plus every `migrations/**.sql`. `#[cfg(test)]` items and `tests/` trees
# are out of scope for the same reason `cut_gate_sqlite_posture.sh` excludes
# them: a gate that cannot tell a PROHIBITION from its own PROOF gets deleted,
# and this tree's proofs deliberately contain the forbidden shapes
# (`adr0108_seam.rs` runs the lexicographic predicate; `adr0108_step5_billing_
# family.rs` writes `SET quantity = 0.1 + 0.2` to demonstrate the R2 hazard).
#
# The detection rule, the two classes and the scanner's stated blind spot are
# documented in `tools/adr0108_money_arith_scan.awk`. Read that before changing
# either file.
#
# Exit 0 = green.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail=0
note() { printf '  %s\n' "$*"; }
echo "ADR-0108 T-8 money/quantity SQL-arithmetic cut-gate — root: $ROOT"

SCANNER="tools/adr0108_money_arith_scan.awk"
REGISTER="tools/adr0108_money_arith_pending_folds.txt"
[[ -f "$SCANNER" ]] || { note "✗ FAIL (T8-1): the scanner $SCANNER is missing — the gate cannot be evaluated"; exit 1; }
[[ -f "$REGISTER" ]] || { note "✗ FAIL (T8-1): the pending-fold register $REGISTER is missing"; exit 1; }

# --- the scan ----------------------------------------------------------------
scan() {
  local root="$1" d f
  for d in "$root"/apps/*/src "$root"/crates/*/src "$root"/modules/*/src; do
    [[ -d "$d" ]] || continue
    find "$d" -name '*.rs' -type f | sort | while IFS= read -r f; do
      awk -v F="${f#"$root"/}" -v KIND=rs -f "$root/$SCANNER" "$f"
    done
  done
  find "$root" -name '*.sql' -type f -not -path '*/target/*' -not -path '*/node_modules/*' \
    | sort | while IFS= read -r f; do
      awk -v F="${f#"$root"/}" -v KIND=sql -f "$root/$SCANNER" "$f"
    done
}

RAW="$(scan "$ROOT")"
RECORDS="$(printf '%s\n' "$RAW" | awk -F'\t' '$1=="REC"{print $2}' | sort -u)"
DETAIL="$(printf '%s\n' "$RAW" | awk -F'\t' '$1=="REC"{printf "%s  (%s:%s)\n      %s\n", $2, $2, $3, $4}')"
STMTS="$(printf '%s\n' "$RAW" | awk -F'\t' '$1=="STMTS"{s+=$2} END{print s+0}')"
FILES="$(printf '%s\n' "$RAW" | awk -F'\t' '$1=="STMTS"' | wc -l | tr -d ' ')"

# --- CHECK T8-1: the scope is real -------------------------------------------
if [[ "$FILES" -eq 0 ]]; then
  note "✗ FAIL (T8-1): the scanner saw ZERO files — the gate would be vacuously green"
  exit 1
fi
if [[ "$STMTS" -eq 0 ]]; then
  note "✗ FAIL (T8-1): ZERO SQL statements were parsed out of $FILES file(s) — the"
  note "      literal walk is broken, and its silence would read as 'no arithmetic'"
  exit 1
fi
note "✓ T8-1: $STMTS SQL statement(s) parsed from $FILES scanned file(s)"

# --- CHECK T8-2: liveness, against a synthetic fixture ------------------------
# The gate does not trust its own scanner's silence until it has watched it
# speak. Three fixtures in an isolated tmpdir: the two real §3.4 shapes (which
# must fire, on the named arms) and a benign one (which must not).
LIVE="$(mktemp -d "${TMPDIR:-/tmp}/t8-liveness.XXXXXX")"
trap 'rm -rf "$LIVE"' EXIT
cat > "$LIVE/hot.sql" <<'EOF'
SELECT CAST(SUM(CAST(il.quantity AS DECIMAL(38,6)) * il.unit_price) AS VARCHAR)
  FROM invoice_line il;
SELECT id FROM products
 WHERE COALESCE(stock_qty, 0) < COALESCE(min_stock, 0)
 ORDER BY (COALESCE(stock_qty, 0) - COALESCE(min_stock, 0)) ASC;
EOF
cat > "$LIVE/cold.sql" <<'EOF'
SELECT id, quantity, unit_price FROM invoice_line ORDER BY id;
UPDATE purchase_order_lines SET received_quantity = received_quantity + ?3
 WHERE id = ?1;
EOF
hot="$(awk -v F=hot.sql -v KIND=sql -f "$SCANNER" "$LIVE/hot.sql" | awk -F'\t' '$1=="REC"{print $2}')"
cold="$(awk -v F=cold.sql -v KIND=sql -f "$SCANNER" "$LIVE/cold.sql" | awk -F'\t' '$1=="REC"{print $2}')"
live_fail=0
# One arm per failure mode the gate exists to catch. `arith-mul` is §3.4's
# `reports.rs` float-money shape, `cmp-lt` is the lexicographic `WHERE` that
# silently un-flags a low-stock product, `arith-sub` is its REAL-coercing
# `ORDER BY` deficit, and `agg-sum` is the inventory cache rebuild.
for arm in 'R2|arith-mul' 'R1|arith-mul' 'R2|agg-sum' 'R1|agg-sum' 'R2|cmp-lt' 'R2|arith-sub' 'R2|cmp-orderby'; do
  if ! printf '%s\n' "$hot" | grep -qF "|${arm}|"; then
    note "✗ FAIL (T8-2): the scanner is BLIND on arm ${arm} — it did not fire on the fixture"
    live_fail=1
  fi
done
if [[ -n "${cold//[[:space:]]/}" ]]; then
  note "✗ FAIL (T8-2): the scanner FALSE-ALARMS on a benign fixture (a bare projection,"
  note "      an ORDER BY on an identity column, and \`received_quantity\`, which is not a"
  note "      census column — \`_\` is a word character):"
  printf '      %s\n' "$cold"
  live_fail=1
fi
if [[ "$live_fail" -ne 0 ]]; then
  fail=1
else
  note "✓ T8-2: the scanner demonstrably fires on all 7 fixture arms and stays silent on the benign one"
fi

# --- the register ------------------------------------------------------------
ALLOWED="$(grep -v '^[[:space:]]*#' "$REGISTER" | sed 's/[[:space:]]*$//' | grep -v '^$' || true)"
ALLOWED_N="$(printf '%s\n' "$ALLOWED" | grep -c . || true)"

# --- CHECK T8-3: no finding outside the register ------------------------------
unregistered=""
while IFS= read -r rec; do
  [[ -n "$rec" ]] || continue
  printf '%s\n' "$ALLOWED" | grep -qxF "$rec" || unregistered="${unregistered}${rec}"$'\n'
done <<< "$RECORDS"
if [[ -n "${unregistered//[[:space:]]/}" ]]; then
  note "✗ FAIL (T8-3): SQL arithmetic/comparison on a §3.2 money, quantity or rate column:"
  while IFS= read -r rec; do
    [[ -n "$rec" ]] || continue
    printf '      %s\n' "$rec"
    printf '%s\n' "$RAW" | awk -F'\t' -v R="$rec" '$1=="REC" && $2==R {printf "        %s:%s\n        %s\n", $2, $3, $4}' | head -4
  done <<< "$unregistered"
  note "      Fold it in Rust over \`Decimal\`/\`i64\` (§3.4). Under R2 a TEXT column in"
  note "      SQL arithmetic coerces to REAL — float money — and a TEXT comparison is"
  note "      lexicographic, so \`'9' < '10'\` is FALSE."
  fail=1
else
  note "✓ T8-3: no unregistered SQL arithmetic on a money/quantity/rate column"
fi

# --- CHECK T8-4: the register is a ratchet, so it may not go stale ------------
stale=""
while IFS= read -r rec; do
  [[ -n "$rec" ]] || continue
  printf '%s\n' "$RECORDS" | grep -qxF "$rec" || stale="${stale}${rec}"$'\n'
done <<< "$ALLOWED"
if [[ -n "${stale//[[:space:]]/}" ]]; then
  note "✗ FAIL (T8-4): $REGISTER holds entries that match NOTHING in the tree:"
  printf '      %s\n' "${stale%$'\n'}"
  note "      Either the fold landed (delete the entry — the register only shrinks) or"
  note "      the function was renamed (the record is fn-keyed, not line-keyed). A"
  note "      register that outlives its sites is an allowlist nobody re-reads."
  fail=1
else
  note "✓ T8-4: all $ALLOWED_N register entries still match a live site (register is exact)"
fi

echo
if [[ "$fail" -eq 0 ]]; then echo "CUT-GATE: ✓ PASSED"; exit 0; fi
echo "CUT-GATE: ✗ FAILED"
exit 1
