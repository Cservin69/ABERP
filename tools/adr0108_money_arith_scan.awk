# adr0108_money_arith_scan.awk — ADR-0108 §8 T-8's scanner.
#
# Finds every place a MONEY / QUANTITY / RATE column (the §3.2 A-D census) is
# ARITHMETIC-ED or (for the TEXT-decimal class) COMPARED **inside a SQL string**.
#
# Why this is load-bearing and not tidiness (§3.1, corrected 2026-07-31):
# `STRICT` does NOT protect an R2 column. REAL -> TEXT converts losslessly, so a
# float written into `quantity` is accepted and stringified, and `typeof()` reads
# `'text'` either way. R2's guards are the Rust-side `Decimal` bind and THIS
# scanner. On the SQL side the two failure modes are:
#
#   * arithmetic  — `TEXT * INTEGER` coerces to REAL: float money, silently.
#                   On the R1 INTEGER side `SUM` is exact but overflows i64.
#   * comparison  — `TEXT < TEXT` is LEXICOGRAPHIC: stock `'9'` vs min `'10'`
#                   compares `'9' > '1...'` -> the low-stock row is not flagged.
#
# Invocation (one file per call; the shell drives it):
#
#     awk -v F="<path>" -v KIND="rs|sql" -f adr0108_money_arith_scan.awk <path>
#
# Output, tab-separated, one line per finding plus one summary line:
#
#     REC \t <file>|<enclosing_fn>|<class>|<arm>|<column>  \t <line> \t <sql>
#     STMTS \t <count of SQL statements this file contributed>
#
# The RECORD carries NO line number — same convention as the ADR-0098 opener
# fingerprints and the ADR-0106 door records — so an unrelated edit that shifts
# lines cannot red the gate. What is pinned is WHICH FUNCTION does WHAT to WHICH
# COLUMN. The line number rides alongside for the human reading the failure.
#
# --- how a hit is decided --------------------------------------------------
#
# Naive `grep 'SUM('` cannot see `repository.rs`'s real shape, where both
# operands are wrapped: `COALESCE(stock_qty,0) < COALESCE(min_stock,0)`. The
# operand adjacent to `<` is a `)`, not a column. So the statement is reduced by
# TAINT PROPAGATION instead of matched by pattern:
#
#   1. every census column occurrence becomes the marker `~m~`
#      (`il.quantity` too — the table qualifier is eaten; `received_quantity`
#      is NOT a hit, `_` is a word character);
#   2. innermost parenthesised groups collapse, outermost-last:
#        - a group with no marker  -> a neutral `~x~`   (`DECIMAL(38,6)`)
#        - a group WITH a marker   -> `~m~`, and the taint survives the wrapper
#          (`COALESCE(...)`, `CAST(... AS ...)`, `ROUND(...)`, bare parens);
#      a group whose preceding identifier is an AGGREGATE is itself a finding;
#   3. at every collapse and once on the residue, a marker adjacent to an
#      operator is a finding.
#
# `CAST(SUM(CAST(il.quantity AS DECIMAL(38,6)) * il.unit_price) AS VARCHAR)`
# therefore reduces `CAST(~m~ AS DECIMAL ~x~)` -> `~m~`, giving `SUM(~m~ * ~m~)`
# — which fires the `*` arm AND the `SUM` arm, on two different columns.
#
# --- the two classes, and why their arms differ ----------------------------
#
# R1 (INTEGER minor units) gets the ARITHMETIC arm only. Comparing or ordering
# an INTEGER money column in SQLite is exact and correct; a gate that reddened
# on `ORDER BY unit_price` would be crying wolf, and a gate that cries wolf gets
# switched off (the same call §3.3 makes for T-5(d)/(e)). R2 (TEXT canonical
# decimal) gets BOTH arms, because that is the class where `<`, `ORDER BY`,
# `MIN`/`MAX` and `BETWEEN` are lexicographic. This is §3.4's own Q2 sweep
# scope, which is per-column over the R2 ten; §8 T-8's "A-D" wording is wider
# than the hazard and is corrected to this.
#
# --- what this scanner cannot see (stated, not hidden — rule 11) ------------
#
# A statement assembled from SEPARATE string literals (`"SUM(qty" + "_delta)"`)
# is scanned literal-by-literal, so a split that straddles the hazard evades it.
# That is deliberate evasion, not a shape anyone writes by accident; the tree's
# dynamic SQL is `format!` over ONE multi-line literal with `{fragment}` holes,
# and each fragment is itself a literal that gets scanned. Doc comments are
# stripped: three of this tree's `T-8` citations are `///` lines, and a gate
# that cannot tell a prohibition from its own documentation gets deleted.

function is_ident_char(c) { return c ~ /[a-z0-9_]/ }

# `r###"…"###` closes on a quote followed by exactly RAWH hashes.
function hashes(k, s) { s = ""; while (k-- > 0) s = s "#"; return s }

# Replace every standalone occurrence of `col` (and any `alias.` before it)
# with the taint marker.
function mark_one(s, col, out, rest, p, prevch, nextch, pre, cl) {
  out = ""; rest = s; cl = length(col)
  while ((p = index(rest, col)) > 0) {
    prevch = (p > 1) ? substr(rest, p - 1, 1) : ""
    nextch = substr(rest, p + cl, 1)
    if (is_ident_char(prevch) || is_ident_char(nextch)) {
      out = out substr(rest, 1, p + cl - 1)
      rest = substr(rest, p + cl)
      continue
    }
    pre = substr(rest, 1, p - 1)
    if (prevch == ".") sub(/[a-z0-9_]*\.$/, "", pre)
    out = out pre "~m~"
    rest = substr(rest, p + cl)
  }
  return out rest
}

function mark_columns(s, cls, i) {
  for (i = 1; i <= NCOL; i++) if (COLCLASS[i] == cls) s = mark_one(s, COLNAME[i])
  return s
}

# The column a marker came from, for the record. A statement can taint several
# columns at once; the record names all of them, sorted, joined by `+`.
function tainted_columns(s, cls, i, out) {
  out = ""
  for (i = 1; i <= NCOL; i++)
    if (COLCLASS[i] == cls && mark_one(s, COLNAME[i]) != s)
      out = (out == "") ? COLNAME[i] : out "+" COLNAME[i]
  return out
}

function emit(arm, cls) { HITS[cls "|" arm] = 1 }

# A marker standing next to an operator.
function check_ops(t, cls, rest, i, left, right, lc, rc, rc2) {
  rest = t
  while ((i = index(rest, "~m~")) > 0) {
    left = substr(rest, 1, i - 1)
    right = substr(rest, i + 3)
    sub(/[ \t]+$/, "", left)
    sub(/^[ \t]+/, "", right)
    lc = substr(left, length(left), 1)
    rc = substr(right, 1, 1)
    rc2 = substr(right, 1, 2)
    if (lc == "*" || rc == "*") emit("arith-mul", cls)
    if (lc == "/" || rc == "/") emit("arith-div", cls)
    if (lc == "+" || rc == "+") emit("arith-add", cls)
    if (lc == "-" || rc == "-") emit("arith-sub", cls)
    if (cls == "R2") {
      if (lc == "<" || rc == "<") emit("cmp-lt", cls)
      if (lc == ">" || rc == ">") emit("cmp-gt", cls)
      if (rc2 == "<=" || rc2 == ">=") emit("cmp-lte", cls)
      if (right ~ /^between/) emit("cmp-between", cls)
    }
    rest = right
  }
}

function check_orderby(t, cls, rest, i, tail) {
  rest = t
  while ((i = index(rest, "order by")) > 0) {
    tail = substr(rest, i + 8)
    sub(/(limit|offset)[^a-z0-9_].*$/, "", tail)
    if (index(tail, "~m~") > 0) emit("cmp-orderby", cls)
    rest = substr(rest, i + 8)
  }
}

# The identifier immediately before a `(` — the wrapper/aggregate name.
function trailing_ident(pre, m) {
  m = pre
  if (match(m, /[a-z_][a-z0-9_]*$/)) return substr(m, RSTART, RLENGTH)
  return ""
}

function analyze_class(sql, cls, s, guard, content, ident, pre, repl, cut, gs, gl) {
  s = mark_columns(sql, cls)
  if (index(s, "~m~") == 0) return
  guard = 0
  while (guard++ < 400 && match(s, /\([^()]*\)/)) {
    # RSTART/RLENGTH must be banked BEFORE `trailing_ident` — it calls
    # `match()` itself and would otherwise silently retarget the splice below
    # onto the identifier it just found. (It did, and the reduction then ate
    # the wrong span and lost every hit but one.)
    gs = RSTART; gl = RLENGTH
    content = substr(s, gs + 1, gl - 2)
    pre = substr(s, 1, gs - 1)
    ident = trailing_ident(pre)
    if (index(content, "~m~") > 0) {
      if (ident == "sum" || ident == "avg" || ident == "total") emit("agg-" ident, cls)
      if (cls == "R2" && (ident == "min" || ident == "max")) emit("agg-" ident, cls)
      check_ops(content, cls)
      repl = "~m~"
    } else {
      repl = " ~x~ "
    }
    cut = (ident == "") ? gs - 1 : gs - 1 - length(ident)
    s = substr(s, 1, cut) repl substr(s, gs + gl)
  }
  check_ops(s, cls)
  if (cls == "R2") check_orderby(s, cls)
}

function analyze(raw, ln, sql, k, cls, arm, cols, shown) {
  STMTS++
  sql = tolower(raw)
  gsub(/~/, " ", sql)
  gsub(/[ \t]+/, " ", sql)
  for (k in HITS) delete HITS[k]
  analyze_class(sql, "R1")
  analyze_class(sql, "R2")
  shown = raw
  gsub(/[ \t]+/, " ", shown)
  if (length(shown) > 200) shown = substr(shown, 1, 197) "..."
  for (k in HITS) {
    split(k, PARTS, "\\|")
    cls = PARTS[1]; arm = PARTS[2]
    cols = tainted_columns(sql, cls)
    printf "REC\t%s|%s|%s|%s|%s\t%d\t%s\n", F, (FN == "" ? "-" : FN), cls, arm, cols, ln, shown
  }
}

function looks_like_sql(t, l) {
  l = tolower(t)
  if (l ~ /(^|[^a-z_])(select|insert into|update|delete from|create table|alter table)([^a-z_]|$)/) return 1
  if (l ~ /(^|[^a-z_])(where|order by|group by|having|set)([^a-z_]|$)/) return 1
  return 0
}

function flush_literal(ln) {
  if (looks_like_sql(BUF)) analyze(BUF, ln)
  BUF = ""
}

BEGIN {
  # ---- the §3.2 census, enumerated. R1 = INTEGER minor units (A + B's
  # `huf_equivalent_total`); R2 = TEXT canonical decimal (B's `est_cost_huf`,
  # C, and D's five float-money columns that convert in Step 8).
  #
  # §3.4's prose calls R2 "all ten" and lists ten NAMES. It omits
  # `quantity_dec` (C's transient S157 ladder column) and BOTH
  # `quoting_parameters` rate columns, which §3.2 D does list. The census here
  # is the union of the TABLES in §3.2, not the prose count.
  split("unit_price unit_price_minor total_net_minor total_vat_minor total_gross_minor line_total_minor subtotal_minor vat_minor total_minor huf_equivalent_total", R1, " ")
  split("est_cost_huf exchange_rate quantity quantity_dec qty_target qty_per_unit qty_delta stock_qty min_stock total_price_eur cost_per_kg_eur cad_cam_rate_eur_per_hour machining_rate_eur_per_minute", R2, " ")
  NCOL = 0
  for (i in R1) { NCOL++; COLNAME[NCOL] = R1[i]; COLCLASS[NCOL] = "R1" }
  for (i in R2) { NCOL++; COLNAME[NCOL] = R2[i]; COLCLASS[NCOL] = "R2" }
  INSTR = 0; ISRAW = 0; RAWH = 0; BUF = ""; BUFLINE = 0; SKIP = 0; FN = ""; STMTS = 0
  SQLBUF = ""; SQLLINE = 0
}

# ---- plain .sql files: the whole file is SQL, split on `;` -------------------
KIND == "sql" {
  line = $0
  sub(/--.*$/, "", line)
  SQLBUF = SQLBUF " " line
  if (SQLLINE == 0 && line ~ /[^ \t]/) SQLLINE = NR
  while ((p = index(SQLBUF, ";")) > 0) {
    analyze(substr(SQLBUF, 1, p - 1), SQLLINE)
    SQLBUF = substr(SQLBUF, p + 1)
    SQLLINE = NR
  }
  next
}

# ---- .rs files: a real string-literal walk ----------------------------------
# `#[cfg(test)]` items are skipped PER ITEM (column-0 `#[cfg(test)]` to the next
# column-0 `}`) — not by truncating the file at the first one, which is the
# fail-open `cut_gate_sqlite_posture.sh` documents at its `nontest_source`.
# Only a cfg that REQUIRES `test` counts; `#[cfg(feature = "...")]` and
# `#[cfg(not(test))]` compile into non-test builds and stay scanned.
{
  if (SKIP == 1) { if ($0 ~ /^\}/) SKIP = 0; next }
  if (INSTR == 0) {
    if ($0 ~ /^#\[cfg\(test\)\]/) { SKIP = 1; next }
    if ($0 ~ /^#\[cfg\(all\(/ && $0 ~ /(\(|,)[ \t]*test[ \t]*(,|\))/ && $0 !~ /not\(test\)/) { SKIP = 1; next }
    if ($0 ~ /^[ \t]*\/\//) next
    if (match($0, /(^|[^a-z0-9_])fn [a-z_][a-z0-9_]*/)) {
      fnm = substr($0, RSTART, RLENGTH)
      sub(/^.*fn /, "", fnm)
      FN = fnm
    }
  }

  line = $0
  n = length(line)
  i = 1
  SQLC = 0
  while (i <= n) {
    c = substr(line, i, 1)
    # A `--` inside the literal starts a SQL comment that runs to the end of
    # the PHYSICAL line. Its text is not executed and must not be scanned:
    # `quote_pricing_jobs.rs`'s DDL carries `-- pre-PR-271 schema` and
    # `-- ... v1.` right beside the column list, and those hyphens read as
    # subtraction once the newlines are folded away. The walk CONTINUES
    # through the comment (only the appending stops) because the Rust literal
    # may well close inside it — stopping the walk there would swallow the
    # rest of the file into one literal, which is a fail-open.
    if (INSTR == 1 && SQLC == 0 && c == "-" && substr(line, i + 1, 1) == "-") {
      SQLC = 1; BUF = BUF " "; i += 2; continue
    }
    if (INSTR == 0) {
      if (c == "/" && substr(line, i + 1, 1) == "/") break
      if (c == "r" && (i == 1 || !is_ident_char(tolower(substr(line, i - 1, 1))))) {
        j = i + 1; h = 0
        while (substr(line, j, 1) == "#") { h++; j++ }
        if (substr(line, j, 1) == "\"") {
          INSTR = 1; ISRAW = 1; RAWH = h; BUF = ""; BUFLINE = NR; i = j + 1; continue
        }
      }
      if (c == "\"") { INSTR = 1; ISRAW = 0; RAWH = 0; BUF = ""; BUFLINE = NR; i++; continue }
      i++
    } else if (ISRAW == 1) {
      if (c == "\"" && substr(line, i + 1, RAWH) == hashes(RAWH)) {
        INSTR = 0; SQLC = 0; i = i + 1 + RAWH; flush_literal(BUFLINE); continue
      }
      if (SQLC == 0) BUF = BUF c
      i++
    } else {
      if (c == "\\") { if (SQLC == 0) BUF = BUF " "; i += 2; continue }
      if (c == "\"") { INSTR = 0; SQLC = 0; i++; flush_literal(BUFLINE); continue }
      if (SQLC == 0) BUF = BUF c
      i++
    }
  }
  if (INSTR == 1) BUF = BUF " "
}

END { printf "STMTS\t%d\n", STMTS }
