# ADR-0099 — in-process runtime AUDIT-LEDGER READ-FORK scanner (toolchain-free).
#
# The write-fork gate (CHECK 10M / adr0099_write_fork_scan.awk) catches an
# independent opener that APPENDS. It is STRUCTURALLY BLIND to the dual hazard: an
# independent opener that READS the audit ledger. Once ANY audit writer is on the
# shared aberp_db::Handle (waves 1-2e already are) the Handle's audit rows are
# WAL-resident (checkpoint disabled in H3), so a FRESH `Ledger::open` / `Connection::
# open` reader sees only the folded SUBSET on the main file — a silent torn read
# (proved in wave-2e: machine_crud's fresh read-back saw 1 of 3 events; the Handle
# read saw all 3). A gate that cannot see a bug class does not protect against it.
#
# Prints one "LINE:fname:READFORK" record per RUNTIME function (outside
# #[cfg(test)]) that READS the audit ledger through an INDEPENDENT opener and does
# NOT itself append (an appender is a WRITE-fork — owned by CHECK 10M, whose
# verify/mirror read tail the recipe migration removes anyway — so it is excluded
# here to avoid double-counting). A read-fork is a fn with NO append AND either:
#   * a fresh `Ledger::open(` AND a typed ledger READ
#       (.entries( / .verify_chain( / .sync_mirror( / list_notes_history( /
#        pending_from_ledger( ), OR
#   * a fresh `Connection::open(` AND a raw `... FROM audit_ledger` SELECT.
#
# ── ADR-0099 Addendum 3 (this session): the read-VIA-HELPER blind spot ──
# `submission_queue::count_pending` (submission_queue.rs:180, reached in-serve from
# issue_invoice:563 / issue_storno:279 / issue_modification:242) opens a fresh
# `Ledger` then reads it through `pending_from_ledger(&ledger)` (which calls
# `.entries()` one indirection away). The typed-read token list above only saw a
# read when `.entries(`/`.verify_chain(`/`.sync_mirror(`/`list_notes_history(`
# appeared textually IN the fn, so `count_pending` (and the CLI one-shot
# `drain_submission_queue::run`, same shape) were UNCOUNTED reads — the SERVE_HANDLE
# tripwire (which hooks `Ledger::open`) caught `count_pending` while BOTH static
# gates missed it (it is not a write, and its read hid behind the helper). Adding
# `pending_from_ledger(` to the typed-read set closes it, the same way
# `list_notes_history(` (also a read-helper name, not a `.method`) already is.
#
# `from_connection` / `open_in_memory` are the sanctioned shared-instance seams (a
# from_connection reader rides the Handle — coherent) and never trip it.
#
# TWO parsing views are built per line so both shapes above are visible:
#   code   — strings AND comments stripped: for openers / typed reads / appends
#            (a `Connection::open` inside a string or comment must not count).
#   codenc — comments stripped, STRINGS KEPT: for `FROM audit_ledger`, whose table
#            name lives inside a SQL string literal `code` would erase.
# Function records are flushed AFTER a line's detection runs (deferred), so an
# opener sharing a line with the fn's closing `}` (a one-liner) is NOT missed.
BEGIN{ depth=0; tdepth=-1; pending=0; inblk=0; instr=0; inraw=0; rawh=0; fn_depth=-1; fn_pending=0; n_allow=split(allow,A,",") }
function is_allowed(name,   k){ for(k=1;k<=n_allow;k++) if(A[k]==name) return 1; return 0 }
function flush(   is_read){
  is_read = (cur_ledopen && cur_read) || (cur_connopen && cur_auditsel)
  if (cur_fn!="" && is_read && !cur_app && !is_allowed(cur_fn)) {
    printf "%d:%s:readfork@L%d\n", cur_open_ln, cur_fn, cur_open_ln
  }
  cur_ledopen=0; cur_connopen=0; cur_read=0; cur_auditsel=0; cur_app=0; cur_open_ln=0
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
  fnclose=0
  # LEXER: the tools/adr0098_opener_scan.awk block, ported verbatim (2026-07-21),
  # with the raw/normal string bodies still fed to `codenc` (this scanner's
  # strings-KEPT view — a raw SQL string `r#"… FROM audit_ledger"#` must stay
  # visible). The previous hand-rolled char rule mishandled '\\' / '\u{…}' and —
  # the live fail-open — had NO raw-string rule: `let s = r##"a "# b"##;`
  # stranded this scanner and hid a real `Ledger::open` + `.entries()` read-fork
  # on the following line. ONE lexer idiom across all four scanners.
  code=""; codenc=""; L=length(line)
  for(i=1;i<=L;i++){
    c=substr(line,i,1); d=substr(line,i,2)
    if(inraw){                       # raw-string body: kept in codenc, dropped from code
      codenc=codenc c
      if(c=="\""){
        ok=1; for(kk=1;kk<=rawh;kk++) if(substr(line,i+kk,1)!="#"){ ok=0; break }
        if(ok){ inraw=0; codenc=codenc substr(line,i+1,rawh); i+=rawh }
      }
      continue
    }
    if(inblk){ if(d=="*/"){inblk--;i++} else if(d=="/*"){inblk++;i++} ; continue }
    if(instr){                       # inside a string: kept in codenc, dropped from code
      codenc=codenc c
      if(c=="\\"){ codenc=codenc substr(line,i+1,1); i++; continue }
      if(c=="\""){ instr=0 }
      continue
    }
    if(d=="//"){ break }
    if(d=="/*"){ inblk++;i++;continue }
    if(c=="\""){
      h=0; jj=i-1
      while(jj>=1 && substr(line,jj,1)=="#"){ h++; jj-- }
      if(jj>=1 && substr(line,jj,1)=="r" \
         && (jj==1 || substr(line,jj-1,1) !~ /[A-Za-z0-9_]/ \
             || (substr(line,jj-1,1)=="b" && (jj-1==1 || substr(line,jj-2,1) !~ /[A-Za-z0-9_]/)))) {
        inraw=1; rawh=h; codenc=codenc c; continue
      }
      instr=1; codenc=codenc c; continue
    }
    if(c=="'"){
      n1=substr(line,i+1,1); n2=substr(line,i+2,1)
      if(n1=="\\"){ jj=index(substr(line,i+3),"'"); if(jj>0) i=i+2+jj; continue }
      if(n2=="'"){ i=i+2; continue }
      if(n1 !~ /^[A-Za-z_]$/){ jj=index(substr(line,i+2),"'"); if(jj>0){ i=i+1+jj; continue } }
      continue                            # lifetime / loop label — consumes nothing
    }
    code=code c; codenc=codenc c
    if(c=="{"){
      depth++
      if(pending && tdepth<0){ tdepth=depth; pending=0 }
      if(fn_pending){ fn_depth=depth; fn_pending=0 }
    } else if(c=="}"){
      if(tdepth==depth) tdepth=-1
      if(fn_depth>=0 && depth==fn_depth){ fnclose=1 }   # DEFER the flush past detection
      depth--
    }
  }
  now_in=(tdepth>=0); intest = was_in || now_in
  if (!intest && cur_fn!="") {
    if (code ~ /Ledger::open[ \t]*\(/ && code !~ /from_connection/ && code !~ /open_in_memory/) {
      if(!cur_open_ln){ cur_open_ln=NR }
      cur_ledopen=1
    }
    if (code ~ /Connection::open(_with_flags)?[ \t]*\(/ && code !~ /from_connection/ && code !~ /open_in_memory/) {
      if(!cur_open_ln){ cur_open_ln=NR }
      cur_connopen=1
    }
    if (code ~ /\.entries[ \t]*\(/ || code ~ /\.verify_chain[ \t]*\(/ || code ~ /\.sync_mirror[ \t]*\(/ \
        || code ~ /list_notes_history[ \t]*\(/ || code ~ /pending_from_ledger[ \t]*\(/) { cur_read=1 }
    if (codenc ~ /[Ff][Rr][Oo][Mm][ \t]+audit_ledger/) { cur_auditsel=1 }
    if (code ~ /\.append(_signed)?[ \t]*\(/ || code ~ /append_in_tx(_signed)?[ \t]*\(/ \
        || code ~ /append_reopen[ \t]*\(/ || codenc ~ /[Ii][Nn][Tt][Oo][ \t]+audit_ledger/) { cur_app=1 }
  }
  if (fnclose) { flush(); cur_fn=""; fn_depth=-1 }
}
END{ flush() }
