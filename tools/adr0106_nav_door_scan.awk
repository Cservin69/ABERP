# ADR-0106 — NAV-emission door scanner. Toolchain-free (awk only).
#
# WHAT IT FINDS. Every RUNTIME (outside #[cfg(test)]) call to a symbol in the
# NAV-emission REACHING SET, plus every call to the validation choke point
# `validate_invoice_preflight`, plus every definition of a NAV body emitter.
# Prints one record per hit:
#
#     <LINE>:<enclosing_fn>:<symbol>
#
# The reaching set is passed in with -v syms="a,b,c" (see
# tools/adr0106_nav_door_registry.txt — the gate builds the list from the
# registry so the two can never drift). A qualified symbol ("issue_storno::run")
# is matched literally; a bare symbol ("storno_from_inputs") is matched as a
# call in any path position.
#
# WHY A REACHING SET AND NOT JUST THE EMITTERS. The defect class this gate
# exists for is "a new door reaches NAV filing without passing the choke
# point" — and such a door usually does NOT construct the body itself, it
# calls an existing helper that does (that is exactly how the modification
# route landed). Censusing only `nav_xml::render_*_data*` would not see it.
# The reaching set is closed under calls: to reach an emitter you must call
# something already in the set, so ANY new reaching function necessarily
# produces a new call record here. The gate's closure check (N2) then forces
# that function to be either added to the set (extending the census one hop)
# or declared an ENTRYPOINT with a written preflight disposition. There is no
# third option that keeps the gate green.
#
# EMITTER DEFINITIONS. A `pub fn render_<...>_data<...>(` definition line emits
# the pseudo-symbol `#emitter-def`, so growing nav_xml.rs a SEVENTH emitter is
# a registry change, not a silent one.
#
# ALIAS EVASION (same vector ADR-0098 R4 finding H closed for openers). A
# `use crate::nav_xml::render_invoice_data as emit;` rename followed by
# `emit(...)` is caught: aliases of watched symbols are learned per file from
# `use ... <sym> as <alias>;` and matched as calls to the alias.
#
# LEXER. Lifted verbatim from tools/adr0098_opener_scan.awk (hardened
# 2026-07-21 after that scanner was found FAIL-OPEN). Strings, char literals,
# raw strings r#"…"#, nesting block comments and line comments are all lexed
# out, and #[cfg(test)] regions are tracked by brace depth, so a watched
# symbol inside a doc comment, a string, or a test module never trips the
# scan — and a stray quote inside a char literal or raw string cannot wedge
# the scanner mid-string and blind it for the rest of the file.
BEGIN{
  depth=0; tdepth=-1; pending=0; inblk=0; instr=0; inraw=0; rawh=0
  n_syms=split(syms,S,",")
  for(k=1;k<=n_syms;k++){ if(S[k]!="") WATCH[S[k]]=1 }
  WATCH["validate_invoice_preflight"]=1
}
{
  line=$0
  if (match(line,/^[ \t]*(pub(\([^)]*\))?[ \t]+)?(async[ \t]+)?(unsafe[ \t]+)?fn[ \t]+[A-Za-z0-9_]+/)) {
    fn=substr(line,RSTART,RLENGTH); sub(/.*fn[ \t]+/,"",fn); fname=fn; isdef=1
  } else isdef=0
  st=line; sub(/^[ \t]+/,"",st)
  if (st ~ /^#\[cfg\(/ && st ~ /test/ && st !~ /not\(test\)/) pending=1
  was_in=(tdepth>=0)
  # Build a "code-only" version of the line (strip comments/strings) for matching.
  code=""; L=length(line)
  for(i=1;i<=L;i++){
    c=substr(line,i,1); d=substr(line,i,2)
    if(inraw){
      if(c=="\""){
        ok=1; for(k=1;k<=rawh;k++) if(substr(line,i+k,1)!="#"){ ok=0; break }
        if(ok){ inraw=0; i+=rawh }
      }
      continue
    }
    if(inblk){ if(d=="*/"){inblk--;i++} else if(d=="/*"){inblk++;i++} ; continue }
    if(instr){ if(c=="\\"){i++;continue} ; if(c=="\""){instr=0} ; continue }
    if(d=="//"){ break }
    if(d=="/*"){ inblk++;i++;continue }
    if(c=="\""){
      h=0; j=i-1
      while(j>=1 && substr(line,j,1)=="#"){ h++; j-- }
      if(j>=1 && substr(line,j,1)=="r" \
         && (j==1 || substr(line,j-1,1) !~ /[A-Za-z0-9_]/ \
             || (substr(line,j-1,1)=="b" && (j-1==1 || substr(line,j-2,1) !~ /[A-Za-z0-9_]/)))) {
        inraw=1; rawh=h; continue
      }
      instr=1; continue
    }
    if(c=="'"){
      n1=substr(line,i+1,1); n2=substr(line,i+2,1)
      if(n1=="\\"){                       # '\'' '\\' '\n' '\x41' '\u{1F600}'
        j=index(substr(line,i+3),"'"); if(j>0) i=i+2+j
        continue
      }
      if(n2=="'"){ i=i+2; continue }      # 'X'
      if(n1 !~ /^[A-Za-z_]$/){            # not a lifetime start → multi-byte 'é'
        j=index(substr(line,i+2),"'"); if(j>0){ i=i+1+j; continue }
      }
      continue                            # lifetime / loop label — consumes nothing
    }
    code=code c
    if(c=="{"){ depth++; if(pending && tdepth<0){ tdepth=depth; pending=0 } }
    else if(c=="}"){ if(tdepth==depth) tdepth=-1; depth-- }
  }
  now_in=(tdepth>=0); intest = was_in || now_in
  # Learn per-file aliases of watched symbols: `use ...::<sym> as <alias>;`.
  # A `use` line is never itself a call, so learning inside a test region is
  # harmless (a test-only alias is only ever used in test code, which is
  # skipped below anyway).
  if (code ~ /(^|[^A-Za-z0-9_])use([^A-Za-z0-9_])/) {
    for (s in WATCH) {
      if (s ~ /::/) continue
      if (match(code, "(^|[^A-Za-z0-9_])" s "[ \t]+as[ \t]+[A-Za-z_][A-Za-z0-9_]*")) {
        a=substr(code,RSTART,RLENGTH); sub(/.*as[ \t]+/,"",a); ALIAS[a]=s
      }
    }
  }
  if (intest) next
  # A NEW NAV body emitter in nav_xml.rs is a registry event, not a silent one.
  # `render_<what>_data` / `render_<what>_data_with_number` — the NAV wire-body
  # emitter naming convention. Deliberately anchored at `_data`(`_with_number`)
  # END-of-name: nav-transport's `render_query_invoice_data_request` is a SOAP
  # *request* builder, not an invoice body, and must not be censused as one.
  if (isdef && code ~ /(^|[^A-Za-z0-9_])fn[ \t]+render_[A-Za-z0-9_]*_data(_with_number)?[ \t]*\(/) {
    printf "%d:%s:#emitter-def\n", NR, fname
  }
  for (s in WATCH) {
    if (s ~ /::/) {
      if (code ~ ("(^|[^A-Za-z0-9_:])" s "[ \t]*\\(")) printf "%d:%s:%s\n", NR, fname, s
      continue
    }
    # A watched symbol's own `fn` definition line is a definition, not a call.
    if (isdef && fname == s) continue
    if (code ~ ("(^|[^A-Za-z0-9_])" s "[ \t]*\\(")) printf "%d:%s:%s\n", NR, fname, s
  }
  for (a in ALIAS) {
    if (isdef && fname == a) continue
    if (code ~ ("(^|[^A-Za-z0-9_])" a "[ \t]*\\(")) printf "%d:%s:%s (alias of %s)\n", NR, fname, a, ALIAS[a]
  }
}
