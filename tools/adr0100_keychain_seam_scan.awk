# ADR-0100 Phase 1 — comment/string/cfg(test)-aware DIRECT-KEYCHAIN scanner.
# Toolchain-free (awk only). Structurally mirrors tools/adr0098_opener_scan.awk
# (same comment/string/cfg(test) stripping), retargeted at the keychain seam.
#
# Phase 1 routed every keychain access through the `aberp-secret-store`
# `SecretStore` seam (ADR-0100 §5). The invariant this scanner enforces:
# NO direct `keyring` access exists in RUNTIME code outside the seam crate and
# its designated test mocks. This scan is the detector; the gate
# (tools/cut_gate_keychain_seam.sh) owns the scope (it excludes the seam crate
# `crates/aberp-secret-store/` and the `*/tests/*` integration mocks) and the
# pass/fail.
#
# Prints "LINE:text" for each RUNTIME (outside #[cfg(test)]) line that reaches
# the keychain backend directly, catching the bypass forms the adversarial
# review named:
#   * any `keyring::` path token — covers `keyring::Entry`, fully-qualified
#     `::keyring::`, `use keyring::*`, and `use keyring::Entry as X` (the alias
#     import line itself carries `keyring::`, so a later `X::new(` call needs no
#     separate rule), plus `keyring::Error` / `keyring::set_default_credential_builder`.
#   * a `.get_password(` / `.set_password(` / `.delete_password(` method call —
#     catches a renamed-type call whose import somehow evaded the path rule.
#   * `new_with_target(` — keyring's `Entry::new_with_target` targeted-entry ctor.
#
# Strings, // line comments and /* */ block comments are stripped, so the many
# doc/comments that mention `keyring::Entry` or `get_password()` (serve.rs,
# setup_nav_credentials.rs, …) never trip the scan — only real code does.
# #[cfg(test)] regions are skipped: inline test mocks are "designated test
# mocks" and never run in production (the seam is a runtime concern).
#
# LEXER (fixed 2026-07-21 — this scanner was FAIL-OPEN before this, and the
# bypass was proven to pass the real CI gate end-to-end):
#   - CHAR LITERALS are lexed. Before this, a char literal holding a quote —
#     tenant_registry.rs's out.push('"') — flipped `instr` ON and stranded the
#     scanner mid-string until the next stray quote (488 stranded lines in
#     tenant_registry.rs, 410 in serve.rs, 49 in numbering.rs). A planted
#     `keyring::Entry::new` at tenant_registry.rs:5 failed the gate; the SAME
#     bypass at :700 PASSED it. Lifetimes/labels (&'a str, 'static, 'outer:) are
#     NOT char literals and must not be lexed as such.
#   - RAW STRINGS r"..." / r#"..."# / br##"..."## are lexed with their hash count
#     and across lines — a SECOND, independent blind vector: `r##"a "# b"##`
#     shielded a real `keyring::Entry` from this scanner (and from
#     tools/adr0099_{read,write}_fork_scan.awk, whose char-literal rule did NOT
#     save them here) before this fix.
#   - Block comments nest, as in rustc.
# The block below is the lexer from tools/adr0098_opener_scan.awk, ported
# verbatim — ONE idiom across all four scanners, not a second dialect.
BEGIN{ depth=0; tdepth=-1; pending=0; inblk=0; instr=0; inraw=0; rawh=0 }
{
  line=$0
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
      # raw-string opener?  <non-ident> [b] r #* "   — count the #s backwards.
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
  if (!intest) {
    if (code ~ /keyring::/ \
        || code ~ /\.get_password[ \t]*\(/ || code ~ /\.set_password[ \t]*\(/ \
        || code ~ /\.delete_password[ \t]*\(/ || code ~ /new_with_target[ \t]*\(/) {
      t=line; sub(/^[ \t]+/,"",t); printf "%d:%s\n",NR,substr(t,1,90)
    }
  }
}
