# ADR-0099 — in-process runtime AUDIT-LEDGER WRITE-FORK scanner (toolchain-free).
#
# The seq-369/416/428/515 forks were NOT the narrow "opener + rogue sync_mirror"
# class CHECK 10L froze. The TRUE fork primitive is simpler and broader: ANY
# independent audit-ledger opener that then APPENDS on the live DB, inside the
# `serve` process, outside the ONE shared aberp_db::Handle. Two such openers off
# the same head both self-assign the next seq (snapshot daemon `snapshot.created`
# racing quote-intake — seq 515). A rogue `sync_mirror` is NOT required; 10L's
# model was too narrow, and 10i merely FROZE these openers instead of banning
# them. This scanner is the corrected model: it fails the build on the fork
# primitive itself.
#
# ── ADR-0099 Addendum 3 (this session): the STORE-SHAPE / split-fork blind spot ──
# The per-function model above ONLY saw a fork when the opener AND the append sat
# in the SAME fn. The core invoicing path SPLITS them across a function boundary
# via an owned `Connection` that is MOVED:
#     pre_tx_setup(): DuckDbBillingStore::open(db).into_connection() -> Connection
#     run_single_tx(conn): conn.transaction(); audit_ledger::append_in_tx(&tx, …)
# The opener half (`pre_tx_setup`) has no append; the append half (`run_single_tx`)
# has no opener token — so `issue_invoice`, `issue_storno`, `issue_modification`,
# `submit_invoice`, `mark_invoice_paid`, `poll_ack` were UNCOUNTED in-serve write
# forks. The move can chain through intermediaries (poll_ack_from_inputs -> conn ->
# poll_loop(&mut conn) -> write_ack_audit_entry(&mut conn) -> append_in_tx), so a
# one-hop rule is NOT enough. This scanner now follows the connection transitively.
#
# THE MODEL (two record classes, both emitted; the wrapper filters the allow-list):
#
#   • COLOCATED fork — a RUNTIME fn (outside #[cfg(test)]) that contains BOTH an
#     independent live-DB opener AND an audit append. (The original primitive.)
#
#   • SPLIT fork — a RUNTIME fn that contains an independent opener, NO in-fn
#     append, whose opened Connection reaches an audit append in ANOTHER fn. The
#     escape is proven by EITHER:
#       – the fn calls `.into_connection()` (a constructor that hands out the raw
#         owned Connection — the DuckDbBillingStore store-shape), OR
#       – the fn calls, directly or transitively, an "audit-writer helper": a fn
#         that takes a `Connection`/`Transaction` and appends. The helper set is
#         the transitive closure A* over the file's call graph (fixpoint below).
#
# An INDEPENDENT live-DB opener is one of
#     Connection::open(_with_flags)? / Ledger::open / DuckDbBillingStore::open /
#     Database::open / append_reopen(            (open_in_memory & from_connection
#     are the sanctioned shared-instance seams, excluded).
# An AUDIT APPEND is
#     .append( / .append_signed( / append_in_tx( / append_in_tx_signed( /
#     append_reopen(                             (append_reopen is itself an
#     open+append, so it alone makes a fn a colocated write-fork).
#
# WHY the file's transitive closure is SOUND (no cross-file call graph needed):
# CHECK M (always enforced) guarantees a Handle-routed file retains NO independent
# opener. So a Handle-served append (which acquires its tx from `.db.write()` in
# its own fn) never coexists with an independent opener in the same file, and the
# split rule cannot mistake a migrated Handle append for a fork. The connection an
# independent opener hands to a same-file helper is, by CHECK M's invariant, a
# forked connection.
#
# Comment/string/char-literal aware (a token inside a doc-comment or string never
# trips it). Boot/CLI/allow-listed fn names are passed via -v allow="a,b"; the
# wrapper (cut_gate_write_fork.sh) also filters the emitted records against
# tools/adr0099_write_fork_allowlist.txt. A fn on the allow-list is a SANCTIONED
# opener (pre-serve boot create/recover, or a separate-process CLI one-shot fenced
# by the F-E whole-DB writer flock) and is skipped.
BEGIN{ depth=0; tdepth=-1; pending=0; inblk=0; instr=0; fn_depth=-1; fn_pending=0; insig=0; nfn=0; n_allow=split(allow,A,",") }
function is_allowed(name,   k){ for(k=1;k<=n_allow;k++) if(A[k]==name) return 1; return 0 }
# Record the fn whose body we just closed into the per-file buffer (indexed by a
# unique fid so same-named fns never collide). The END block resolves A* and emits.
function flush(   id){
  if (cur_fn!="") {
    id = ++nfn
    F_name[id]=cur_fn; F_open[id]=cur_open; F_openln[id]=cur_open_ln
    F_app[id]=cur_app; F_appln[id]=cur_app_ln
    F_takes[id]=cur_takes; F_into[id]=cur_into; F_calls[id]=cur_calls
  }
  cur_open=0; cur_app=0; cur_open_ln=0; cur_app_ln=0; cur_takes=0; cur_into=0; cur_calls=""
}
{
  line=$0; decl_line=0; fnclose=0
  # fn-name + body-brace tracking. A new top-level fn decl flushes the previous.
  if (match(line,/^[ \t]*(pub(\([^)]*\))?[ \t]+)?(async[ \t]+)?(unsafe[ \t]+)?fn[ \t]+[A-Za-z0-9_]+/)) {
    # Only treat as a NEW function when we are at (or above) the fn-body depth,
    # i.e. not a closure/nested item mid-body. Track the depth the fn body opens
    # at; when we return to it, the fn is done.
    if (fn_depth<0 || depth<=fn_depth) {
      flush()
      f=substr(line,RSTART,RLENGTH); sub(/.*fn[ \t]+/,"",f); cur_fn=f; fn_pending=1; insig=1; decl_line=1
    }
  }
  st=line; sub(/^[ \t]+/,"",st)
  if (st ~ /^#\[cfg\(/ && st ~ /test/ && st !~ /not\(test\)/) pending=1
  was_in=(tdepth>=0)
  # Capture the signature-region flag BEFORE the tokenizer runs: on a single-line
  # `fn NAME(c: &Connection) {` the `{` flips insig→0 mid-line, so a post-loop
  # `insig` read would miss the param type. `was_insig` pins it for this line.
  was_insig=insig
  # code-only view (strip strings / // and /* */ comments / char literals)
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
      if(fn_pending){ fn_depth=depth; fn_pending=0; insig=0 }
    } else if(c=="}"){
      if(tdepth==depth) tdepth=-1
      if(fn_depth>=0 && depth==fn_depth){ fnclose=1 }   # DEFER the flush past this line's detection (single-line fns)
      depth--
    }
  }
  now_in=(tdepth>=0); intest = was_in || now_in
  if (intest || cur_fn=="") next
  # signature region: does this fn take (or hand back) a Connection/Transaction?
  # Used to identify audit-writer helpers (append + takes_conn) and the
  # intermediaries that forward a conn. A return-type mention is harmless: a
  # non-appending opener that merely returns a Connection is caught via
  # `.into_connection()` / its callee closure, not via A0 membership.
  if (was_insig && (code ~ /Connection/ || code ~ /Transaction/)) cur_takes=1
  # opener?
  if ((code ~ /(Connection::open(_with_flags)?|Ledger::open|DuckDbBillingStore::open|Database::open)\(/ \
       || code ~ /append_reopen[ \t]*\(/) \
      && code !~ /open_in_memory/ && code !~ /from_connection/) {
    if(!cur_open){ cur_open=1; cur_open_ln=NR }
  }
  # append?
  if (code ~ /\.append(_signed)?[ \t]*\(/ || code ~ /append_in_tx(_signed)?[ \t]*\(/ \
      || code ~ /append_reopen[ \t]*\(/) {
    if(!cur_app){ cur_app=1; cur_app_ln=NR }
  }
  # owned-Connection escape (the DuckDbBillingStore store-shape)
  if (code ~ /\.into_connection[ \t]*\(/) cur_into=1
  # callee names (word immediately followed by '(') — the intra-file call graph.
  # Skip the fn declaration line so a fn never counts as calling itself.
  if (!decl_line) {
    tmp=code
    while (match(tmp,/[A-Za-z_][A-Za-z0-9_]*\(/)) {
      nm=substr(tmp,RSTART,RLENGTH-1); cur_calls=cur_calls " " nm " "
      tmp=substr(tmp,RSTART+RLENGTH)
    }
  }
  # Deferred flush: now that this line's opener/append/takes/into/calls are all
  # recorded, close the fn if its body-brace shut on this line.
  if (fnclose) { flush(); cur_fn=""; fn_depth=-1 }
}
END{
  flush()
  # A0 — audit-writer helpers: fns that append AND take a Connection/Transaction
  # (i.e. append on a passed-in / owned-moved conn, not one they opened locally).
  for(k=1;k<=nfn;k++) if(F_app[k] && F_takes[k]){ INA[k]=1; NAMEINA[F_name[k]]=1 }
  # Fixpoint: a conn-taking fn that calls (any arm of) A* is itself on the write
  # path — it forwards the forked connection toward the append.
  changed=1; rounds=0
  while(changed && rounds<64){ changed=0; rounds++
    for(k=1;k<=nfn;k++){
      if(INA[k] || !F_takes[k]) continue
      m=split(F_calls[k],C," ")
      for(j=1;j<=m;j++){ if(C[j]!="" && NAMEINA[C[j]]){ INA[k]=1; NAMEINA[F_name[k]]=1; changed=1; break } }
    }
  }
  # Emit. COLOCATED preserves the original record verbatim. SPLIT is a new class.
  for(k=1;k<=nfn;k++){
    if(is_allowed(F_name[k])) continue
    if(F_open[k] && F_app[k]){
      printf "%d:%s:opener@L%d+append@L%d\n", F_openln[k], F_name[k], F_openln[k], F_appln[k]
      continue
    }
    if(F_open[k] && !F_app[k]){
      via=""
      if(F_into[k]) via="into_connection"
      else { m=split(F_calls[k],C," "); for(j=1;j<=m;j++){ if(C[j]!="" && NAMEINA[C[j]]){ via=C[j]; break } } }
      if(via!="") printf "%d:%s:opener@L%d+split_append_via_%s\n", F_openln[k], F_name[k], F_openln[k], via
    }
  }
}
