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
BEGIN{ depth=0; tdepth=-1; pending=0; inblk=0; instr=0 }
{
  line=$0
  st=line; sub(/^[ \t]+/,"",st)
  if (st ~ /^#\[cfg\(/ && st ~ /test/ && st !~ /not\(test\)/) pending=1
  was_in=(tdepth>=0)
  # Build a "code-only" version of the line (strip comments/strings) for matching.
  code=""; L=length(line)
  for(i=1;i<=L;i++){
    c=substr(line,i,1); d=substr(line,i,2)
    if(inblk){ if(d=="*/"){inblk=0;i++} ; continue }
    if(instr){ if(c=="\\"){i++;continue} ; if(c=="\""){instr=0} ; continue }
    if(d=="//"){ break }
    if(d=="/*"){ inblk=1;i++;continue }
    if(c=="\""){ instr=1; continue }
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
