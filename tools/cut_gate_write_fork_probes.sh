#!/usr/bin/env bash
#
# cut_gate_write_fork_probes.sh — proves the ADR-0099 write-fork gate has TEETH,
# including the Addendum-3 SPLIT / store-shape / moved-Connection detection.
#
# MIGRATION-INVARIANT BY DESIGN. Every probe operates on a SYNTHETIC snippet the
# probe feeds to the scanner on stdin, or on a throwaway COPY of the tree — NEVER
# a real source file. So the probes stay valid through the entire write-fork
# migration (24 → 0), including full zero-residual.
#
# Detection probes assert on the SCANNER (adr0099_write_fork_scan.awk) directly:
# EMIT = the scanner records a write-fork, SILENT = it stays quiet. Gate probes
# exercise the allow-list + the fail-closed property (a de-gated scanner passes
# real write-forks). The BLIND-SPOT probe pins the reason this gate was extended:
# the OLD colocated-only model is silent on the invoice store-shape, so flipping
# the gate to ENFORCING at "residual zero" under that model would have certified a
# tree in which invoicing was still forked. Exit 0 = every probe behaved.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCAN="tools/adr0099_write_fork_scan.awk"
GATE="tools/cut_gate_write_fork.sh"
ALLOW="tools/adr0099_write_fork_allowlist.txt"
SCRATCH="apps/aberp/src/zz_writefork_scratch.rs"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/writefork-probes.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0; bad=0

emits() { printf '%s' "$1" | awk -f "$ROOT/$SCAN" 2>/dev/null; }
expect_emit() {
  if [[ -n "$(emits "$2")" ]]; then printf '  ✓ detects: %s\n' "$1"; pass=$((pass+1))
  else printf '  ✗ MISSED: %s — scanner emitted nothing\n' "$1"; bad=$((bad+1)); fi
}
expect_silent() {
  if [[ -z "$(emits "$2")" ]]; then printf '  ✓ correctly ignores: %s\n' "$1"; pass=$((pass+1))
  else printf '  ✗ FALSE POSITIVE: %s — scanner emitted: %s\n' "$1" "$(emits "$2")"; bad=$((bad+1)); fi
}
fresh() { local d; d="$(mktemp -d "$WORK/copy.XXXXXX")"; tar -C "$ROOT" --exclude=.git --exclude=target -cf - . | tar -C "$d" -xf -; printf '%s' "$d"; }

echo "negative probes for the ADR-0099 write-fork gate (synthetic, migration-invariant)"
echo "root: $ROOT"; echo

# ── detection correctness (scanner) ──────────────────────────────────────────
# Snippets are MULTI-LINE like real code; the deferred flush also covers the
# single-line forms (W1b/W3b) below.
expect_emit "W1 colocated fork: Connection::open + append_in_tx in ONE fn" \
'fn f(p: &std::path::Path) {
    let c = duckdb::Connection::open(p).unwrap();
    aberp_audit_ledger::append_in_tx(&c, m, k, v, a, None).unwrap();
}'

expect_emit "W1b colocated on a single line (deferred-flush)" \
'fn f(p: &std::path::Path) { let c = duckdb::Connection::open(p).unwrap(); aberp_audit_ledger::append_in_tx(&c, m, k, v, a, None).unwrap(); }'

expect_emit "W2 SPLIT store-shape: DuckDbBillingStore::open(...).into_connection() — the invoice blind spot" \
'fn pre_tx_setup(p: &std::path::Path) -> Connection {
    let s = DuckDbBillingStore::open(p).unwrap();
    s.into_connection()
}
fn run_single_tx(mut c: Connection) {
    let tx = c.transaction().unwrap();
    aberp_audit_ledger::append_in_tx(&tx, m, k, v, a, None).unwrap();
}'

expect_emit "W3 SPLIT via appender-helper: opener hands &mut conn to a conn-taking append helper" \
'fn opener(p: &std::path::Path) {
    let mut c = duckdb::Connection::open(p).unwrap();
    write_audit(&mut c);
}
fn write_audit(c: &mut Connection) {
    aberp_audit_ledger::append_in_tx(c, m, k, v, a, None).unwrap();
}'

expect_emit "W4 SPLIT TRANSITIVE (2-hop): opener -> mid(&mut conn) -> helper.append (the poll_ack shape)" \
'fn poll_ack_from_inputs(p: &std::path::Path) {
    let mut c = duckdb::Connection::open(p).unwrap();
    poll_loop(&mut c);
}
fn poll_loop(c: &mut Connection) {
    write_ack_audit_entry(c);
}
fn write_ack_audit_entry(c: &mut Connection) {
    aberp_audit_ledger::append_in_tx(c, m, k, v, a, None).unwrap();
}'

# ── silence correctness (no false positives) ─────────────────────────────────
expect_silent "W5 pure reader: Connection::open reading a business table, NO append" \
'fn r(p: &std::path::Path) {
    let c = duckdb::Connection::open(p).unwrap();
    let _ = c.prepare("SELECT id FROM vendors");
}'

expect_silent "W6 Handle-served append: db.write() guard + append, NO independent opener (the migrated shape)" \
'fn h(state: &AppState) {
    let mut g = state.db.write().unwrap();
    let tx = g.transaction().unwrap();
    aberp_audit_ledger::append_in_tx(&tx, m, k, v, a, None).unwrap();
}'

expect_silent "W6b Handle-served append handed to a conn-taking helper — still no opener in the file" \
'fn h(state: &AppState) {
    let mut g = state.db.write().unwrap();
    let tx = g.transaction().unwrap();
    write_audit(&tx);
}
fn write_audit(tx: &Transaction) {
    aberp_audit_ledger::append_in_tx(tx, m, k, v, a, None).unwrap();
}'

expect_silent "W7 shared-instance seam: from_connection (not a fresh open)" \
'fn s(g: Connection) {
    let store = DuckDbBillingStore::from_connection(g);
    let c = store.into_connection();
    aberp_audit_ledger::append_in_tx(&c, m, k, v, a, None).unwrap();
}'

expect_silent "W7b open_in_memory seam is not a live-DB opener" \
'fn m() {
    let s = DuckDbBillingStore::open_in_memory().unwrap();
    let c = s.into_connection();
    aberp_audit_ledger::append_in_tx(&c, m, k, v, a, None).unwrap();
}'

expect_silent "W8 cfg(test) ignored: a colocated fork inside #[cfg(test)]" \
'#[cfg(test)]
mod z {
    fn f(p: &std::path::Path) {
        let c = duckdb::Connection::open(p).unwrap();
        aberp_audit_ledger::append_in_tx(&c, m, k, v, a, None).unwrap();
    }
}'

expect_silent "W9 appender-helper ALONE (takes conn + appends, NO opener): the fork is the opener that feeds it, not the helper" \
'fn write_audit(c: &mut Connection) {
    aberp_audit_ledger::append_in_tx(c, m, k, v, a, None).unwrap();
}'

# ── Addendum 4 — the RETURNED-`Ledger` escape (the 2026-07-19 incident) ───────
# W10 is THE regression probe: it is the snapshot daemon's exact pre-incident
# shape. Before Addendum 4 the scanner was SILENT on this and the gate reported
# GREEN while prod forked its ledger three times in two weeks. If W10 ever goes
# silent again, the fix has been reverted and the blind spot is back.
expect_emit "W10 RETURNED-Ledger fork: opener hands a Ledger back, caller appends (the snapshot-daemon shape — seq 8056)" \
'fn open_ledger(p: &std::path::Path, t: &TenantId, b: BinaryHash) -> Result<Ledger> {
    Ledger::open(p, t.clone(), b).map_err(|e| anyhow::anyhow!("{e}"))
}
fn take_and_emit(p: &std::path::Path, t: &TenantId, b: BinaryHash, a: Actor) -> Result<()> {
    let mut ledger = open_ledger(p, t, b)?;
    ledger.append(EventKind::SnapshotCreated, payload.to_bytes(), a, None)?;
    Ok(())
}'

expect_emit "W10b RETURNED-Ledger, append reached via a same-file appender helper (transitive)" \
'fn open_ledger(p: &std::path::Path) -> Result<Ledger> {
    Ledger::open(p, t, b).map_err(|e| anyhow::anyhow!("{e}"))
}
fn emit(p: &std::path::Path) -> Result<()> {
    let mut l = open_ledger(p)?;
    push(&mut l);
    Ok(())
}
fn push(l: &mut Ledger) {
    l.append(EventKind::Test, v, a, None).unwrap();
}'

# The false-positive guards. `Ledger` is ALSO the ledger READ/walk API, and the
# tree has eight runtime read-only `Ledger::open` sites (print_invoice,
# reports, export_invoice_bundle, the resolve_*_precondition helpers, the drain
# runs). Those belong to the READ-fork gate; if the write gate starts flagging
# them it has become a false-positive machine and duplicates that gate.
expect_silent "W11 read-only Ledger::open (entries/verify_chain), NO append, NOT returned — a read-fork-gate concern" \
'fn resolve_precondition(p: &std::path::Path) -> Result<StuckPrecondition> {
    let ledger = Ledger::open(p, t, b).context("open audit ledger")?;
    let e = ledger.entries().context("read entries")?;
    audit_query::stuck_precondition(&ledger, id)
}'

expect_silent "W11b Ledger opened and RETURNED, but no caller ever appends (a pure read-side handout)" \
'fn open_ledger(p: &std::path::Path) -> Result<Ledger> {
    Ledger::open(p, t, b).map_err(|e| anyhow::anyhow!("{e}"))
}
fn report(p: &std::path::Path) -> Result<Vec<Row>> {
    let l = open_ledger(p)?;
    walk_ledger(&l, window)
}'

expect_silent "W11c appending fn TAKES a &mut Ledger but opens nothing — the fork is the opener, not the helper" \
'fn push(l: &mut Ledger) {
    l.append(EventKind::Test, v, a, None).unwrap();
}'

# ── the BLIND-SPOT invariant: the OLD colocated-only model misses the split ──
# A minimal reference implementation of the PRE-Addendum-3 semantics (emit only
# when an opener AND an append sit in the SAME fn). If this ever EMITS on the
# store-shape, the split forks were catchable colocated all along; if the REAL
# scanner ever goes SILENT on it, the extension has been reverted. Either way the
# ADR's load-bearing sentence — "flipping to ENFORCING at residual zero under the
# old model would have certified forked invoicing" — needs re-checking.
OLD="$WORK/old_colocated_only.awk"
cat > "$OLD" <<'AWK'
# Reference: the pre-Addendum-3 COLOCATED-ONLY write-fork model.
BEGIN{depth=0;tdepth=-1;pending=0;inblk=0;instr=0;fn_depth=-1;fn_pending=0}
function flush(){ if(cur_fn!=""&&cur_open&&cur_app) print cur_open_ln":"cur_fn; cur_open=0;cur_app=0;cur_open_ln=0 }
{
  line=$0
  if (match(line,/^[ \t]*(pub(\([^)]*\))?[ \t]+)?(async[ \t]+)?(unsafe[ \t]+)?fn[ \t]+[A-Za-z0-9_]+/)) {
    if (fn_depth<0||depth<=fn_depth){ flush(); f=substr(line,RSTART,RLENGTH); sub(/.*fn[ \t]+/,"",f); cur_fn=f; fn_pending=1 }
  }
  st=line; sub(/^[ \t]+/,"",st); if(st~/^#\[cfg\(/&&st~/test/&&st!~/not\(test\)/)pending=1
  was_in=(tdepth>=0); code="";L=length(line)
  for(i=1;i<=L;i++){c=substr(line,i,1);d=substr(line,i,2)
    if(inblk){if(d=="*/"){inblk=0;i++};continue}
    if(instr){if(c=="\\"){i++;continue};if(c=="\""){instr=0};continue}
    if(d=="//"){break};if(d=="/*"){inblk=1;i++;continue};if(c=="\""){instr=1;continue}
    if(c=="'"){if(substr(line,i,3)~/^'\\.'/){i+=2}else if(substr(line,i+2,1)=="'"){i+=2};continue}
    code=code c
    if(c=="{"){depth++;if(pending&&tdepth<0){tdepth=depth;pending=0};if(fn_pending){fn_depth=depth;fn_pending=0}}
    else if(c=="}"){if(tdepth==depth)tdepth=-1;if(fn_depth>=0&&depth==fn_depth){flush();cur_fn="";fn_depth=-1};depth--}
  }
  now_in=(tdepth>=0); if(was_in||now_in||cur_fn=="")next
  if((code~/(Connection::open(_with_flags)?|Ledger::open|DuckDbBillingStore::open|Database::open)\(/||code~/append_reopen[ \t]*\(/)&&code!~/open_in_memory/&&code!~/from_connection/){if(!cur_open){cur_open=1;cur_open_ln=NR}}
  if(code~/\.append(_signed)?[ \t]*\(/||code~/append_in_tx(_signed)?[ \t]*\(/||code~/append_reopen[ \t]*\(/){if(!cur_app)cur_app=1}
}
END{flush()}
AWK
STORE_SHAPE='fn pre_tx_setup(p: &std::path::Path) -> Connection {
    let s = DuckDbBillingStore::open(p).unwrap();
    s.into_connection()
}
fn run_single_tx(mut c: Connection) {
    let tx = c.transaction().unwrap();
    aberp_audit_ledger::append_in_tx(&tx, m, k, v, a, None).unwrap();
}'
old_out="$(printf '%s' "$STORE_SHAPE" | awk -f "$OLD" 2>/dev/null)"
new_out="$(emits "$STORE_SHAPE")"
if [[ -z "$old_out" && -n "$new_out" ]]; then
  printf '  ✓ BLIND-SPOT proven: the old colocated-only model is SILENT on the store-shape, the current scanner EMITS it — the ENFORCING-at-zero flip under the old model would have certified forked invoicing.\n'
  pass=$((pass+1))
else
  printf '  ✗ BLIND-SPOT invariant broken: old=[%s] new=[%s]\n' "$old_out" "$new_out"; bad=$((bad+1))
fi

# ── gate wiring: allow-list ──────────────────────────────────────────────────
count_worklist() { ( cd "$1" && bash "$GATE" ) 2>&1 | grep -oE '[0-9]+ non-allow-listed in-serve write-fork' | grep -oE '^[0-9]+'; }

echo "[W10 allow-list] a scratch split-fork added to the allow-list is dropped from the residual"
c="$(fresh)"
printf 'fn probe_opener(p: &std::path::Path) {\n    let mut cc = duckdb::Connection::open(p).unwrap();\n    probe_write_audit(&mut cc);\n}\nfn probe_write_audit(cc: &mut Connection) {\n    aberp_audit_ledger::append_in_tx(cc, m, k, v, a, None).unwrap();\n}\n' > "$c/$SCRATCH"
base="$(count_worklist "$c")"
printf '%s:probe_opener\n' "$SCRATCH" >> "$c/$ALLOW"
withallow="$(count_worklist "$c")"
if [[ "$base" -gt 0 && "$withallow" -eq $((base-1)) ]]; then
  printf '  ✓ allow-list entry honoured (%s → %s)\n' "$base" "$withallow"; pass=$((pass+1))
else printf '  ✗ BROKEN: allow-list entry not dropped (base=%s withallow=%s)\n' "$base" "$withallow"; bad=$((bad+1)); fi

echo "[META fail-closed] a de-gated scanner must let real write-forks through ENFORCE"
c="$(fresh)"
printf 'END{}\n' > "$c/$SCAN"   # sabotage: scanner emits nothing for every file
rc=0; ( cd "$c" && ENFORCE_WRITE_FORK=1 bash "$GATE" ) >/dev/null 2>&1 || rc=$?
if [[ "$rc" -eq 0 ]]; then
  printf '  ✓ fail-closed: a de-gated scanner (emits nothing) makes ENFORCE pass the real write-forks (rc=0) — so the SCANNER is load-bearing; the detection probes above flip to MISSED the moment anyone neuters it.\n'
  pass=$((pass+1))
else printf '  ✗ META BROKEN: de-gated scanner still non-zero under ENFORCE (rc=%s)\n' "$rc"; bad=$((bad+1)); fi

echo
echo "probes passed: $pass   broken/escaped: $bad"
if [[ "$bad" -ne 0 ]]; then echo "WRITE-FORK PROBES: ✗ FAILED"; exit 1; fi
echo "WRITE-FORK PROBES: ✓ ALL CHECKS HAVE TEETH (synthetic; invariant under the migration)"
