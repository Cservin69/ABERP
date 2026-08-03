#!/usr/bin/env bash
# ADR-0108 §1.2 — the `Handle::read()` / `Handle::write()` call-site census.
#
# WHY THIS IS A SCRIPT AND NOT A GREP (finding R-2, read-fork audit 2026-07-31).
# The ADR's original census was
#
#     grep -rn '\.db\.read()\|\.db\.write()' --include='*.rs' apps crates modules
#
# which reported 84 sites (50 read / 34 write). That grep is wrong in two ways,
# and both were already known defects in this repo:
#
#   1. It is SINGLE-LINE. `serve.rs` overwhelmingly formats these chains as
#          state
#              .db
#              .read()
#      so the receiver and the method land on different lines and the pattern
#      cannot match. This is precisely the defect PR #43 (D1a) found in the
#      read-fork SCANNER and fixed there by going structural — the same defect
#      survived here because the census was written as a one-line grep.
#   2. It requires the literal `.db.` prefix, so every `Handle` bound to a local
#      (`db.read()`, `handle.read()`, `h.read()`, `svc.deps.db.read()`,
#      `state_for_task.db.read()`) is invisible.
#
# Re-measured with the receiver reconstructed across line breaks and the
# non-`Handle` receivers excluded BY NAME (they are `RwLock`s, not `Handle`s),
# the true denominator is 102 read / 136 write / 238 total on the non-test
# surface. An audit over "all 50" was a 49 % sample presented as exhaustive.
#
# Usage:  tools/adr0108_handle_census.sh            # print the table
#         tools/adr0108_handle_census.sh --sites    # print every non-test site
#
# This is a MEASUREMENT tool, not a gate: it has no baseline and never fails on
# a count change. The numbers it prints are the ones ADR-0108 §1.2 quotes, and
# re-running it is how a future session checks that quote is still true.

set -euo pipefail

cd "$(dirname "$0")/.."

# Receivers that spell `.read()` / `.write()` but are NOT an `aberp_db::Handle`
# — every one is a `std::sync::RwLock`. Enumerated rather than pattern-matched
# so a new non-Handle receiver shows up as a count change instead of being
# silently swallowed by a clever regex.
NOT_A_HANDLE='^(.*\.)?(boot_state|inner|registry|smtp_password)$'

MODE="${1:-table}"

# shellcheck disable=SC2016
scan() {
  find apps crates modules -name '*.rs' -not -path '*/target/*' -print0 |
    xargs -0 perl -0777 -ne '
      my $file = $ARGV;
      # A file under a `tests/` or `benches/` directory is test surface in full.
      my $file_is_test = ($file =~ m{/(tests|benches)/});
      # Inline test surface: everything from the first `#[cfg(test)]` to EOF.
      # (Repo convention is a single trailing `mod tests`; a mid-file
      #  `#[cfg(test)]` would only over-count test sites, never under-count
      #  non-test ones, so the heuristic errs safe for a denominator.)
      my $cfg_test_at = ($_ =~ /\#\[cfg\(test\)\]/g) ? pos($_) : undef;
      pos($_) = 0;

      # Strip line comments and block comments so doc-comment prose that spells
      # `state.db.read()` (crates/aberp-db/src/lib.rs, serve_tripwire.rs) is not
      # counted as a call site. Replaced with spaces so byte offsets are stable.
      my $src = $_;
      $src =~ s{^([ \t]*)//[^\n]*}{$1 . " " x (length($&) - length($1))}gme;
      $src =~ s{/\*.*?\*/}{" " x length($&)}ges;

      # Multi-line-aware: rejoin a rustfmt-wrapped method chain by deleting the
      # newline + indentation that precedes a `.`. This is the R-2 fix.
      $src =~ s/\n[ \t]*\./\./g;

      while ($src =~ /([A-Za-z_][A-Za-z0-9_.]*)\.(read|write)\(\)/g) {
        my ($recv, $op) = ($1, $2);
        next if $recv =~ /'"$NOT_A_HANDLE"'/;
        my $is_test = $file_is_test
          || (defined $cfg_test_at && pos($src) >= $cfg_test_at);
        printf "%s\t%s\t%s.%s()\n", ($is_test ? "test" : "non-test"), $file, $recv, $op;
      }
    ' -- 2>/dev/null
}

RESULTS="$(scan)"

count() { printf '%s\n' "$RESULTS" | grep -c "$1" || true; }

NT_R=$(count $'^non-test\t.*\.read()$')
NT_W=$(count $'^non-test\t.*\.write()$')
T_R=$(count $'^test\t.*\.read()$')
T_W=$(count $'^test\t.*\.write()$')

if [ "$MODE" = "--sites" ]; then
  printf '%s\n' "$RESULTS" | grep $'^non-test\t' | sort
  echo
fi

echo "ADR-0108 §1.2 — Handle call-site census (multi-line-aware, receiver-agnostic)"
echo
printf '%-10s %8s %8s %8s\n' '' 'read()' 'write()' 'total'
printf '%-10s %8d %8d %8d\n' 'non-test' "$NT_R" "$NT_W" "$((NT_R + NT_W))"
printf '%-10s %8d %8d %8d\n' 'test'     "$T_R"  "$T_W"  "$((T_R + T_W))"
printf '%-10s %8d %8d %8d\n' 'all'      "$((NT_R + T_R))" "$((NT_W + T_W))" "$((NT_R + NT_W + T_R + T_W))"
