# ADR-0099 — in-process runtime AUDIT-LEDGER READ-FORK scanner (toolchain-free).
#
# The write-fork gate (CHECK 10M / adr0099_write_fork_scan.awk) catches an
# independent opener that APPENDS. It is STRUCTURALLY BLIND to the dual hazard: an
# independent opener that READS the audit ledger. Once ANY audit writer is on the
# shared aberp_db::Handle (waves 1-2e already are), the Handle's audit rows are
# WAL-resident (checkpoint disabled in H3). A FRESH `Ledger::open` / `Connection::
# open` reader then sees only the folded SUBSET on the main file — a silent torn
# read (proved in wave-2e: machine_crud's fresh read-back saw 1 of 3 events; the
# Handle read saw all 3). A consumer can ship fully green and still see a partial
# ledger. This scanner makes that class VISIBLE.
#
# Prints one "LINE:fname:READFORK" record per RUNTIME function (outside
# #[cfg(test)]) that READS the audit ledger through an INDEPENDENT opener and does
# NOT itself append (a fn that appends is a WRITE-fork — owned by CHECK 10M, and
# its verify/mirror read tail is removed by the same recipe migration, so it is
# excluded here to avoid double-counting). A read-fork is:
#     (a fresh `Ledger::open(` )  AND  ( a ledger READ:
#         .entries( / .verify_chain( / .sync_mirror( / list_notes_history( )
#   OR
#     (a fresh `Connection::open(` )  AND  ( a query reading `audit_ledger` )
#   AND  NO append ( .append( / .append_signed( / append_in_tx( / append_reopen( ).
#
# `from_connection` / `open_in_memory` are the sanctioned shared-instance seams
# (a `from_connection` reader rides the Handle — coherent), so they never trip it.
# Comment/string/char-literal aware. Boot/CLI/allow-listed fn names via -v allow=.
BEGIN{ depth=0; tdepth=-1; pending=0; inblk=0; instr=0; fn_depth=-1; fn_pending=0; n_allow=split(allow,A,",") }
function is_allowed(name,   k){ for(k=1;k<=n_allow;k++) if(A[k]==name) return 1; return 0 }
function flush(   is_read){
  # A read-fork reads the audit ledger via a fresh Ledger::open and does NOT
  # append. (Raw `Connection::open` + `SELECT ... FROM audit_ledger` is NOT
  # detected: the table name lives inside a stripped SQL string literal, and
  # ALL real audit reads go through the typed `Ledger` API — zero raw-SQL cases
  # today. That residual gap is flagged in ADR-0099 §CHECK N and is covered by
  # the proposed runtime tripwire, not this static scan.)
  is_read = (cur_ledopen && cur_read)
  if (cur_fn!="" && is_read && !cur_app && !is_allowed(cur_fn)) {
    printf "%d:%s:readfork@L%d\n", cur_open_ln, cur_fn, cur_open_ln
  }
  cur_ledopen=0; cur_read=0; cur_app=0; cur_open_ln=0
}
{
  line=$0
  if (match(line,/^[ \t]*(pub(\([^)]*\))?[ \t]+)?(async[ \t]+)?(unsafe[ \t]+)?fn[ \t]+[A-Za-z0-9_]+/)) {
    if (fn_depth<0 || depth<=fn_depth) {
      flush()
      f=substr(line,RSTART,RLENGTH); sub(/.*fn[ \t]+/,"",f); cur_fn=f; fn_pending=1
    }
  }
  st=line; sub(/^[ \t]+/,"",st)
  if (st ~ /^#\[cfg\(/ && st ~ /test/ && st !~ /not\(test\)/) pending=1
  was_in=(tdepth>=0)
  code=""; L=length(line)
  for(i=1;i<=L;i++){
    c=substr(line,i,1); d=substr(line,i,2)
    if(inblk){ if(d=="*/"){inblk=0;i++} ; continue }
    if(instr){ if(c=="\\"){i++;continue} ; if(c=="\""){instr=0} ; continue }
    if(d=="//"){ break }
    if(d=="/*"){ inblk=1;i++;continue }
    if(c=="\""){ instr=1; continue }
    if(c=="'"){
       if(substr(line,i,3) ~ /^'\\.'/){ i+=2 }
       else if(substr(line,i+2,1)=="'"){ i+=2 }
       continue
    }
    code=code c
    if(c=="{"){
      depth++
      if(pending && tdepth<0){ tdepth=depth; pending=0 }
      if(fn_pending){ fn_depth=depth; fn_pending=0 }
    } else if(c=="}"){
      if(tdepth==depth) tdepth=-1
      if(fn_depth>=0 && depth==fn_depth){ flush(); cur_fn=""; fn_depth=-1 }
      depth--
    }
  }
  now_in=(tdepth>=0); intest = was_in || now_in
  if (intest || cur_fn=="") next
  # fresh audit opener (Ledger::open); shared-instance seams excluded
  if (code ~ /Ledger::open[ \t]*\(/ && code !~ /from_connection/ && code !~ /open_in_memory/) {
    if(!cur_ledopen){ cur_ledopen=1; cur_open_ln=NR }
  }
  # audit-ledger READ signals
  if (code ~ /\.entries[ \t]*\(/ || code ~ /\.verify_chain[ \t]*\(/ || code ~ /\.sync_mirror[ \t]*\(/ \
      || code ~ /list_notes_history[ \t]*\(/) {
    cur_read=1
  }
  # append (exclusion — that makes the fn a WRITE-fork, owned by CHECK 10M)
  if (code ~ /\.append(_signed)?[ \t]*\(/ || code ~ /append_in_tx(_signed)?[ \t]*\(/ \
      || code ~ /append_reopen[ \t]*\(/) {
    cur_app=1
  }
}
END{ flush() }
