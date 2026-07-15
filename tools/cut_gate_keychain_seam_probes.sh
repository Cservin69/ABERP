#!/usr/bin/env bash
#
# cut_gate_keychain_seam_probes.sh — proves tools/cut_gate_keychain_seam.sh has TEETH.
#
# Mirrors tools/cut_gate_negative_probes.sh: every regression is planted in a
# SYNTHETIC scratch file the probe itself creates inside a throwaway COPY of the
# tree — NEVER a real source file — so these probes stay valid as the real code
# evolves. Each probe asserts RED (the gate FAILS + emits the expected signature)
# for a real bypass, or GREEN for a case that must NOT trip (comment mention,
# string literal, #[cfg(test)] inline mock, the seam crate's own keyring use).
# The final META probe proves fail-closed: a de-gated scanner
# (ENFORCE_KEYCHAIN_SEAM=0) lets a real bypass through — so a green gate is the
# script's doing, not luck.
#
# Exit 0 = every probe behaved. Non-zero = a probe broke or a regression escaped.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="tools/cut_gate_keychain_seam.sh"
# Scratch lives IN census scope (apps/aberp/src, not */tests/*, not the seam
# crate) but is NOT any real module — it exists only inside a throwaway copy.
SCRATCH="apps/aberp/src/zz_keychain_probe_scratch.rs"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/keyseam-probes.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0; bad=0

fresh() { # -> path to a fresh, clean copy of the tree (excludes .git + target)
  local d; d="$(mktemp -d "$WORK/copy.XXXXXX")"
  tar -C "$ROOT" --exclude=.git --exclude=target -cf - . | tar -C "$d" -xf -
  printf '%s' "$d"
}
gate_rc() { ( cd "$1" && bash "$GATE" ) >"$1/.out" 2>&1; echo $?; }

expect_pass() { # $1 dir  $2 label
  local rc; rc="$(gate_rc "$1")"
  if [[ "$rc" == "0" ]]; then printf '  ✓ %s\n' "$2"; pass=$((pass+1))
  else printf '  ✗ BROKEN: %s — expected CLEAN pass but gate exit=%s\n' "$2" "$rc"
    sed 's/^/        /' "$1/.out"; bad=$((bad+1)); fi
}
expect_fail() { # $1 dir  $2 signature  $3 label
  local rc; rc="$(gate_rc "$1")"
  if [[ "$rc" != "0" ]] && grep -qF -- "$2" "$1/.out"; then
    printf '  ✓ caught: %s  (exit=%s)\n' "$3" "$rc"; pass=$((pass+1))
  else printf '  ✗ ESCAPED: %s  (exit=%s; expected non-zero + "%s")\n' "$3" "$rc" "$2"
    sed 's/^/        /' "$1/.out"; bad=$((bad+1)); fi
}

echo "cut_gate_keychain_seam.sh negative probes — proving the gate has teeth"

# 0 — the unmodified tree passes clean (the seam crate's own keyring::Entry use
#     and the tests/ integration mocks are correctly excluded from scope).
d="$(fresh)"; expect_pass "$d" "clean tree passes (seam + test mocks excluded)"

# 1 — planted qualified `keyring::Entry::new` in an app module → RED.
d="$(fresh)"
printf 'fn planted() {\n    let _e = keyring::Entry::new(s, a).unwrap();\n}\n' > "$d/$SCRATCH"
expect_fail "$d" "direct keychain access" "qualified keyring::Entry::new"

# 2 — aliased import `use keyring::Entry as X;` → RED (the import line carries keyring::).
d="$(fresh)"
printf 'use keyring::Entry as Kc;\nfn planted() {\n    let _e = Kc::new(s, a).unwrap();\n}\n' > "$d/$SCRATCH"
expect_fail "$d" "direct keychain access" "aliased use keyring::Entry as X"

# 3 — wildcard import `use keyring::*;` → RED.
d="$(fresh)"
printf 'use keyring::*;\nfn planted() {\n    let _e = Entry::new(s, a).unwrap();\n}\n' > "$d/$SCRATCH"
expect_fail "$d" "direct keychain access" "wildcard use keyring::*"

# 4 — a `.get_password(` method call → RED.
d="$(fresh)"
printf 'fn planted(e: &Handle) {\n    let _ = e.get_password();\n}\n' > "$d/$SCRATCH"
expect_fail "$d" "direct keychain access" ".get_password( method call"

# 5 — `Entry::new_with_target(` targeted-entry ctor → RED.
d="$(fresh)"
printf 'fn planted() {\n    let _ = Entry::new_with_target(t, s, a);\n}\n' > "$d/$SCRATCH"
expect_fail "$d" "direct keychain access" "Entry::new_with_target ctor"

# 6 — a COMMENT / string mention must NOT trip (false-positive guard).
d="$(fresh)"
printf 'fn planted() {\n    // keyring::Entry and e.get_password() in a comment\n    let _ = "keyring::Entry in a string";\n}\n' > "$d/$SCRATCH"
expect_pass "$d" "comment/string mention does not trip"

# 7 — a #[cfg(test)] inline mock must NOT trip (designated test mock, never runtime).
d="$(fresh)"
printf 'fn planted() {}\n#[cfg(test)]\nmod tests {\n    fn mk() { let _ = keyring::Entry::new(s, a); }\n}\n' > "$d/$SCRATCH"
expect_pass "$d" "#[cfg(test)] inline mock is skipped"

# 8 — META fail-closed: a de-gated scanner (ENFORCE_KEYCHAIN_SEAM=0) lets a real
#     bypass through. If THIS didn't pass, a green gate would be luck, not teeth.
d="$(fresh)"
printf 'fn planted() {\n    let _e = keyring::Entry::new(s, a).unwrap();\n}\n' > "$d/$SCRATCH"
rc="$( ( cd "$d" && ENFORCE_KEYCHAIN_SEAM=0 bash "$GATE" ) >"$d/.out2" 2>&1; echo $? )"
if [[ "$rc" == "0" ]] && grep -qF "enforcement disabled" "$d/.out2"; then
  printf '  ✓ fail-closed: de-gated scanner passes the real bypass (exit=%s)\n' "$rc"; pass=$((pass+1))
else
  printf '  ✗ META BROKEN: de-gated scanner did not pass the planted bypass (exit=%s)\n' "$rc"
  sed 's/^/        /' "$d/.out2"; bad=$((bad+1))
fi

echo
echo "probes: $pass passed, $bad failed"
[[ "$bad" -eq 0 ]] || exit 1
echo "TEETH: ✓ the keychain-seam gate fails on every planted bypass and only on real ones"
