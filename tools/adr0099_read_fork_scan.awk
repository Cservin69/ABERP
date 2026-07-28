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
#        pending_from_ledger( / find_invoice_draft( ), OR
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
# ── 2026-07-27 (DEV invoice #62): the SAME blind spot, still open ──
# `print_invoice::render_to_bytes` opens a fresh `Ledger` and reads it through
# `find_invoice_draft(&ledger, id)` — one indirection away, exactly the shape
# above. It is reached IN-SERVE from `serve::get_invoice_pdf` (the PDF route AND
# the `POST /api/invoices/:id/email` compose path), so a normally-issued invoice
# whose `InvoiceDraftCreated` was still WAL-resident on the shared Handle read
# back as "no InvoiceDraftCreated audit entry found" — email/resend/PDF 404 on a
# finalized, NAV-acked invoice. The gate reported ZERO in-serve read-forks
# throughout. `find_invoice_draft(` joins the typed-read set.
#
# STRUCTURAL RESIDUAL (deferred, ADR-0099 §CHECK N): name-listing read helpers
# one at a time is whack-a-mole. The general shape — "a fn with a fresh opener
# that hands the opened `Ledger`/`Connection` to ANOTHER fn" — is what should be
# flagged; the three names below are the third patch to the same hole.
#
# `from_connection` / `open_in_memory` are the sanctioned shared-instance seams (a
# from_connection reader rides the Handle — coherent) and never trip it.
#
# ── D2 (2026-07-27): the BUSINESS-TABLE blind spot, and the name PIN ──
# Everything above is AUDIT-LEDGER-shaped: the typed-read tokens and the raw
# `FROM audit_ledger` SELECT both key off the audit ledger. A fresh opener that
# reads a BUSINESS table is invisible to all of it — and `serve::
# read_base_line_vat_kinds` was exactly that: `Connection::open(&state.db_path)`
# + `billing::load_ready_invoice_by_id`, on the live modification route. Its
# stale read returned an EMPTY Vec, the ADR-0101 S2 guard below it passed
# VACUOUSLY over that empty vector, and an exempt / reverse-charge base was
# re-filed to NAV as plain 0% VAT.
#
# ── D1 (2026-07-28): the STRUCTURAL rule — the name allow-list is CLOSED ──
# Everything above keys off a NAME: the typed-read token set (`find_invoice_draft`,
# `pending_from_ledger`, `list_notes_history`, …) is a list of helper names, and
# `PINNED` is a list of enclosing-fn names. FOUR instances of one bug class shipped
# through that (#40 render, #41 modification base, #42 auto-email, E1) because the
# same defective SHAPE under an UNLISTED name is invisible. A scanner that only
# catches names it was told about does not protect against the class.
#
# The shape, stated without reference to any name:
#
#     a FRESH live-DB opener bound to a local, whose handle (or anything derived
#     from it) is then READ THROUGH — a non-propagating method call, or the value
#     handed to ANOTHER function — inside a runtime fn that does not itself append.
#
# `Handle::open` / `Handle::open_default` JOIN the opener set here. They are absent
# from the ADR-0098 opener census (which by design treats the shared Handle as THE
# sanctioned seam — sound only while there is exactly ONE) and, until 2026-07-28,
# from the SERVE_HANDLE_LIVE tripwire. That is precisely how E1 hid: PR#40 moved
# `print_invoice::render_to_bytes` off a bare `Ledger::open` (censused AND hooked)
# onto its own `Handle::open_default` (neither), and every detector went quiet on a
# path that still forked. A second Handle is a second DuckDB instance; it does not
# replay the live WAL, so it reads the last-checkpointed subset exactly like the
# opener it replaced.
#
# PROPAGATE vs READ. `.transaction()` / `.read()` / `.write()` / `.try_clone()` and
# the unwrap family yield a DERIVATIVE of the forked handle, not a read — they
# extend the taint (`let tx = conn.transaction()?` taints `tx`) so the read is
# caught wherever it actually happens, one or more indirections later. Any other
# method call on a tainted value, or a tainted value passed as an argument, IS the
# read. Appends still belong to CHECK 10M (`cur_app` suppresses, as before).
#
# FACTORY CARVE-OUT and its residual. A fn that opens and RETURNS the handle
# (`Ok(handle)`, `Ok(Arc::new(h))`, a bare tail `h`) is not itself a read-fork —
# `serve::open_tenant_handle` is the sanctioned boot opener and must stay green.
# The cost is honest and bounded: a fork SPLIT across two fns (a factory here, the
# read in its caller) has no opener textually in the reading fn and this scanner
# cannot see it. Closing that needs the call graph. The SERVE_HANDLE_LIVE runtime
# tripwire IS call-graph-complete — it fires on the OPEN itself, whoever did it —
# and as of 2026-07-28 it hooks `Handle::open` too. The two halves are complements,
# not duplicates: neither alone would have caught all four instances.
#
# `PINNED` stays as a BACKSTOP under the structural rule (it fires on the opener
# alone, with no read required and no allow-list exemption). It is now a ratchet on
# names already migrated rather than the primary signal.
BEGIN{ n_pin=split("read_base_line_vat_kinds,read_base_currency",P,",") }
# Method calls that yield a DERIVATIVE of the forked handle rather than reading
# through it — they extend the taint instead of tripping the rule.
BEGIN{ split("transaction try_clone read write clone as_ref as_mut borrow borrow_mut lock unwrap expect ok context with_context",PR_," "); for(i_ in PR_) PROP[PR_[i_]]=1 }
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
function is_pinned(name,   k){ for(k=1;k<=n_pin;k++) if(P[k]==name) return 1; return 0 }
# The single simple `let` binding a line introduces, or "" — `let x =`, `let mut x:
# T =`. Destructuring (`let Some(x) = …`, `let (a, b) = …`) yields "" on purpose:
# the taint is only followed through bindings this scanner can name exactly.
function let_bind(c,   b){
  if (!match(c, /(^|[^A-Za-z0-9_])let[ \t]+(mut[ \t]+)?[A-Za-z_][A-Za-z0-9_]*[ \t]*[:=]/)) return ""
  b = substr(c, RSTART, RLENGTH)
  sub(/.*[^A-Za-z0-9_]let[ \t]+/, "", b); sub(/^let[ \t]+/, "", b)
  sub(/^mut[ \t]+/, "", b); sub(/[ \t]*[:=]$/, "", b)
  return b
}
function has_word(c, w){ return match(c, "(^|[^A-Za-z0-9_])" w "($|[^A-Za-z0-9_])") }
# Is this line NOTHING BUT a return/wrap of `w`? Then the fn is a FACTORY for the
# opened handle, not a reader of it (see the factory carve-out in the header).
function is_factory_line(c, w,   s){
  s = c; gsub(/[ \t]/, "", s)
  return s ~ ("^(return)?(Ok\\(|Some\\(|Arc::new\\(|Rc::new\\(|Box::new\\()*&?(mut)?" w "[?]?\\)*[;,]?$")
}
# Does this line READ THROUGH tainted value `w`? Either a non-propagating method
# call on it, or `w` handed to another fn as an argument (the read-via-helper shape
# that four shipped defects all had).
function reads_through(c, w,   m, meth){
  if (match(c, "(^|[^A-Za-z0-9_])" w "[ \t]*\\.[ \t]*[A-Za-z_][A-Za-z0-9_]*[ \t]*\\(")) {
    meth = substr(c, RSTART, RLENGTH)
    sub(/^.*[ \t]*\.[ \t]*/, "", meth); sub(/[ \t]*\($/, "", meth)
    if (!(meth in PROP)) return 1
  }
  if (is_factory_line(c, w)) return 0
  # `w` sitting in an argument list: preceded by `(` or `,` (only `&`/`mut` between)
  # and closed by `,` or `)`. A struct-literal field (`db: h,`) has `:` before the
  # value and so does NOT match — those store the handle, they do not read it.
  if (match(c, "(\\(|,)[ \t]*&?[ \t]*(mut[ \t]+)?" w "[ \t]*[?]?[ \t]*(,|\\))") \
      && c ~ /[A-Za-z_][A-Za-z0-9_:]*[ \t]*\(/) return 1
  return 0
}
function flush(   is_read, pinned_fork, struct_fork){
  is_read = (cur_ledopen && cur_read) || (cur_connopen && cur_auditsel)
  # A PINNED name forks on the OPENER ALONE — no typed-read token required (its
  # reads are business-table, which this scanner cannot recognise) and no
  # allow-list exemption honoured.
  pinned_fork = (cur_ledopen || cur_connopen) && is_pinned(cur_fn)
  # D1 — the STRUCTURAL signal: a fresh opener whose handle is read through, under
  # ANY fn name. Honours the allow-list (flock-fenced CLI one-shots are coherent).
  struct_fork = cur_freshopen && cur_structread && !is_allowed(cur_fn)
  # TAGS route the two verdicts the gate keeps SEPARATE, and must stay distinct:
  #   readfork@   — the audit-ledger / PINNED rules. ENFORCED AT ZERO (unchanged).
  #   structfork@ — the D1 shape rule. Ratcheted against a frozen baseline, because
  #                 turning it on surfaced 28 pre-existing forks (see the baseline
  #                 file). A hit that is BOTH reports as `readfork` — one record per
  #                 fn, and the stricter verdict owns it.
  if (cur_fn!="" && !cur_app) {
    if (pinned_fork || (is_read && !is_allowed(cur_fn)))
      printf "%d:%s:readfork@L%d\n", cur_open_ln, cur_fn, cur_open_ln
    else if (struct_fork)
      printf "%d:%s:structfork@L%d\n", cur_open_ln, cur_fn, cur_open_ln
  }
  cur_ledopen=0; cur_connopen=0; cur_read=0; cur_auditsel=0; cur_app=0; cur_open_ln=0
  cur_freshopen=0; cur_structread=0; split("", T)
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
  # TEST-ONLY, not "mentions test". `#[cfg(any(test, feature = "test-support"))]`
  # and `#[cfg(feature = "test-support")]` COMPILE INTO NON-TEST BUILDS, and the
  # old `st ~ /test/` predicate skipped both — a door behind either was invisible
  # to every scanner in this tree. Only a cfg that REQUIRES `test` is a test
  # region: bare `#[cfg(test)]`, or `all(...)` with `test` as a bare conjunct.
  # Anything else is scanned (fail-CLOSED: an extra record must be censused,
  # which is the safe direction).
  if (st ~ /^#\[cfg\(test\)\]/ \
      || (st ~ /^#\[cfg\(all\(/ && st ~ /(\(|,)[ \t]*test[ \t]*(,|\))/ && st !~ /not\(test\)/)) pending=1
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
        || code ~ /list_notes_history[ \t]*\(/ || code ~ /pending_from_ledger[ \t]*\(/ \
        || code ~ /find_invoice_draft[ \t]*\(/) { cur_read=1 }
    if (codenc ~ /[Ff][Rr][Oo][Mm][ \t]+audit_ledger/) { cur_auditsel=1 }
    if (code ~ /\.append(_signed)?[ \t]*\(/ || code ~ /append_in_tx(_signed)?[ \t]*\(/ \
        || code ~ /append_reopen[ \t]*\(/ || codenc ~ /[Ii][Nn][Tt][Oo][ \t]+audit_ledger/) { cur_app=1 }

    # ── D1 STRUCTURAL RULE (2026-07-28) ──
    # Order matters within the line: a READ through an already-tainted value is
    # checked BEFORE this line's own binding is taint-tracked, so `let n =
    # tx.query_row(…)` counts as a read (and also propagates), while the opener
    # line's own `let h = Handle::open(…)` does not read itself.
    bind = let_bind(code)
    for (t_ in T) if (reads_through(code, t_)) { cur_structread=1; break }
    # Fresh live-DB opener — by OPENER TYPE, never by enclosing-fn name. Handle::open
    # is in the set (see the header: its absence is how E1 hid). The two sanctioned
    # shared-instance seams are excluded exactly as above.
    if (code ~ /(^|[^A-Za-z0-9_])(Ledger|Connection|Handle|DuckDbBillingStore|Database)::open(_default|_with_flags)?[ \t]*\(/ \
        && code !~ /from_connection/ && code !~ /open_in_memory/) {
      if (bind != "") { T[bind]=1; cur_freshopen=1; if(!cur_open_ln) cur_open_ln=NR }
    } else if (bind != "") {
      # Taint PROPAGATION: a binding whose initialiser mentions a tainted value is
      # itself tainted (`let conn = handle.read()?` / `let tx = conn.transaction()?`),
      # so a read an indirection or two downstream is still attributed to the fork.
      for (t_ in T) if (t_ != bind && has_word(code, t_)) { T[bind]=1; break }
    }
  }
  if (fnclose) { flush(); cur_fn=""; fn_depth=-1 }
}
END{ flush() }
