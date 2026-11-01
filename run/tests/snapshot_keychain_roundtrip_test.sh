#!/usr/bin/env bash
#
# snapshot_keychain_roundtrip_test.sh — ADR-0113
#
# Round trip for the keychain part of tools/snapshot-prod.sh and
# tools/restore-keychain-snapshot.sh:
#
#   1. snapshot-prod.sh writes an age file (not a zip), 0600, with no
#      plaintext secret in it;
#   2. restore with the right passphrase re-imports every entry byte for
#      byte (quotes, backslashes, `$`, spaces, UTF-8, a JSON blob);
#   3. a wrong passphrase and a tampered file both fail and import nothing
#      (age is authenticated — ZipCrypto was not);
#   4. a LEGACY `zip -e` snapshot (the pre-ADR-0113 format) still restores;
#   5. --verify decrypts and lists but imports nothing;
#   6. without age on PATH, snapshot-prod.sh fails before writing anything.
#
# Real terminal prompts are driven through a pty with expect(1): age and
# unzip read passphrases from /dev/tty, not stdin, and that is exactly
# what the operator sees. `security` is a PATH stub recording imports to a
# log, so no real keychain item is read or written. HOME points at a
# throwaway dir, so ~/.aberp and ~/aberp-snapshots are fakes.
#
# Needs: macOS, age, expect, zip, unzip. Exit 0 = all assertions passed.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SNAPSHOT_SH="${REPO_ROOT}/tools/snapshot-prod.sh"
RESTORE_SH="${REPO_ROOT}/tools/restore-keychain-snapshot.sh"

for t in age expect zip unzip python3; do
  command -v "$t" >/dev/null 2>&1 || { echo "FAIL: '$t' is required for this test" >&2; exit 1; }
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/snapshot-rt.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
FAKE_HOME="${WORK}/home"
STUB_BIN="${WORK}/bin"
FIXTURES="${WORK}/fixtures"
IMPORT_LOG="${WORK}/imports.log"
mkdir -p "${FAKE_HOME}/.aberp/rt" "$STUB_BIN" "$FIXTURES"
echo "placeholder" > "${FAKE_HOME}/.aberp/rt/aberp.duckdb"

PASS_OK='correct horse — ő/ű "battery" staple'
PASS_BAD='wrong passphrase'

fails=0
pass() { echo "ok   - $1"; }
fail() { echo "FAIL - $1" >&2; fails=$((fails + 1)); }

# ---------- fixtures: the secrets the stub keychain holds --------------------
# Tricky on purpose: quoting, escapes, shell metachars, UTF-8, JSON.
printf '%s' '{"login":"tech-user","password":"p\"a\\ss $HOME `x`","sign_key":"ő-ű-€"}' \
  > "${FIXTURES}/aberp.nav.rt|nav_credentials_blob"
printf '%s' "smtp pw with 'single' and \"double\" quotes \\ end" \
  > "${FIXTURES}/aberp.smtp.rt|smtp_password"

# ---------- security stub ----------------------------------------------------
cat > "${STUB_BIN}/security" <<STUB
#!/usr/bin/env python3
import sys
args = sys.argv[1:]
def opt(flag):
    return args[args.index(flag) + 1]
if args[0] == "find-generic-password":
    try:
        data = open("${FIXTURES}/" + opt("-s") + "|" + opt("-a"), encoding="utf-8").read()
    except FileNotFoundError:
        sys.exit(44)
    sys.stdout.write(data + "\n")   # real \`security -w\` appends a newline
elif args[0] == "add-generic-password":
    assert "-U" in args, "restore must update in place (-U)"
    with open("${IMPORT_LOG}", "a", encoding="utf-8") as log:
        log.write(repr((opt("-s"), opt("-a"), opt("-w"))) + "\n")
else:
    sys.exit(2)
STUB
chmod +x "${STUB_BIN}/security"

# expect driver: run a command on a pty, answer every passphrase/password
# prompt with $ANSWER, return the command's exit status.
cat > "${WORK}/drive.exp" <<'EXP'
set timeout 60
log_user 0
spawn -noecho {*}$argv
expect {
  -re {(?i)(passphrase|password)[^\n]*: ?$} { send -- "$env(ANSWER)\r"; exp_continue }
  -re {\[y/N\] ?$} { send -- "n\r"; exp_continue }
  eof
}
lassign [wait] pid spawnid os_error status
exit $status
EXP

run_pty() { # ANSWER=… run_pty cmd args…
  HOME="$FAKE_HOME" PATH="${STUB_BIN}:${PATH}" expect -f "${WORK}/drive.exp" "$@"
}
imports() { [[ -f "$IMPORT_LOG" ]] && wc -l < "$IMPORT_LOG" | tr -d ' ' || echo 0; }
expected_imports() {
  python3 - "$FIXTURES" <<'PY'
import os, sys
d = sys.argv[1]
for name in ["aberp.nav.rt|nav_credentials_blob", "aberp.smtp.rt|smtp_password"]:
    s, a = name.split("|")
    print(repr((s, a, open(os.path.join(d, name), encoding="utf-8").read())))
PY
}

# ---------- 1) snapshot writes an authenticated age file ---------------------
if ANSWER="$PASS_OK" run_pty "$SNAPSHOT_SH" rt >/dev/null 2>&1; then
  pass "snapshot-prod.sh completes with age"
else
  fail "snapshot-prod.sh exited non-zero"
fi
SNAP_DIR="${FAKE_HOME}/aberp-snapshots"
AGE_FILE="$(ls "$SNAP_DIR"/rt-*-keychain.json.age 2>/dev/null | head -1 || true)"
if [[ -n "$AGE_FILE" ]]; then pass "age snapshot written"; else fail "no .json.age snapshot in $SNAP_DIR"; fi
if ls "$SNAP_DIR"/*.zip >/dev/null 2>&1; then fail "a zip snapshot was still written"; else pass "no zip written"; fi
if [[ -n "$AGE_FILE" ]]; then
  [[ "$(head -c 21 "$AGE_FILE")" == "age-encryption.org/v1" ]] && pass "age header present" || fail "missing age header"
  [[ "$(stat -f '%Lp' "$AGE_FILE")" == "600" ]] && pass "snapshot is mode 0600" || fail "snapshot mode is $(stat -f '%Lp' "$AGE_FILE")"
  if grep -aq "tech-user\|smtp pw" "$AGE_FILE"; then fail "plaintext secret visible in the age file"; else pass "no plaintext in the age file"; fi
fi

# ---------- 2) wrong passphrase: fails, imports nothing ----------------------
rm -f "$IMPORT_LOG"
if ANSWER="$PASS_BAD" run_pty "$RESTORE_SH" --yes "$AGE_FILE" >/dev/null 2>&1; then
  fail "restore accepted a wrong passphrase"
else
  [[ "$(imports)" == "0" ]] && pass "wrong passphrase refused, nothing imported" || fail "wrong passphrase imported entries"
fi

# ---------- 3) tampered ciphertext: fails, imports nothing -------------------
TAMPERED="${WORK}/tampered.json.age"
python3 - "$AGE_FILE" "$TAMPERED" <<'PY'
import sys
b = bytearray(open(sys.argv[1], "rb").read())
b[-10] ^= 0x01          # flip one bit in the last payload chunk
open(sys.argv[2], "wb").write(b)
PY
rm -f "$IMPORT_LOG"
if ANSWER="$PASS_OK" run_pty "$RESTORE_SH" --yes "$TAMPERED" >/dev/null 2>&1; then
  fail "restore accepted a tampered file"
else
  [[ "$(imports)" == "0" ]] && pass "tampered file refused, nothing imported" || fail "tampered file imported entries"
fi

# ---------- 4) --verify: decrypts, lists, imports nothing --------------------
rm -f "$IMPORT_LOG"
if ANSWER="$PASS_OK" run_pty "$RESTORE_SH" --verify "$AGE_FILE" >/dev/null 2>&1 && [[ "$(imports)" == "0" ]]; then
  pass "--verify succeeds without importing"
else
  fail "--verify failed or imported"
fi

# ---------- 5) declining the confirmation imports nothing --------------------
rm -f "$IMPORT_LOG"
if ANSWER="$PASS_OK" run_pty "$RESTORE_SH" "$AGE_FILE" >/dev/null 2>&1; then
  fail "restore without --yes proceeded after 'n'"
else
  [[ "$(imports)" == "0" ]] && pass "answering n at the prompt imports nothing" || fail "'n' still imported"
fi

# ---------- 6) age round trip: byte-exact re-import --------------------------
rm -f "$IMPORT_LOG"
if ANSWER="$PASS_OK" run_pty "$RESTORE_SH" --yes "$AGE_FILE" >/dev/null 2>&1 \
   && diff <(expected_imports) "$IMPORT_LOG" >/dev/null; then
  pass "age snapshot restores every entry byte for byte"
else
  fail "age restore did not reproduce the fixtures"
fi

# ---------- 7) legacy zip snapshot still restores ----------------------------
# Same JSON shape the pre-ADR-0113 script zipped with `zip -e`.
LEGACY_DIR="${WORK}/legacy"; mkdir -p "$LEGACY_DIR"
python3 - "$FIXTURES" "${LEGACY_DIR}/keychain-rt.json" <<'PY'
import json, os, sys
d, out = sys.argv[1], sys.argv[2]
entries = []
for name in ["aberp.nav.rt|nav_credentials_blob", "aberp.smtp.rt|smtp_password"]:
    s, a = name.split("|")
    entries.append({"service": s, "account": a,
                    "password": open(os.path.join(d, name), encoding="utf-8").read()})
json.dump({"tenant": "rt", "captured_at": "2026-10-01T00:00:00Z", "entries": entries},
          open(out, "w", encoding="utf-8"), indent=2)
PY
LEGACY_ZIP="${WORK}/rt-20261001-000000-keychain.zip"
(cd "$LEGACY_DIR" && zip -q -P "$PASS_OK" "$LEGACY_ZIP" keychain-rt.json)
rm -f "$IMPORT_LOG"
if ANSWER="$PASS_OK" run_pty "$RESTORE_SH" --yes "$LEGACY_ZIP" >/dev/null 2>&1 \
   && diff <(expected_imports) "$IMPORT_LOG" >/dev/null; then
  pass "legacy zip snapshot restores every entry byte for byte"
else
  fail "legacy zip restore did not reproduce the fixtures"
fi

# ---------- 8) missing age: refuse before writing anything -------------------
NOAGE_HOME="${WORK}/noage-home"; mkdir -p "${NOAGE_HOME}/.aberp/rt"
set +e
OUT="$(HOME="$NOAGE_HOME" PATH="${STUB_BIN}:/usr/bin:/bin" "$SNAPSHOT_SH" rt 2>&1)"; rc=$?
set -e
if [[ $rc -eq 2 && "$OUT" == *"brew install age"* && ! -e "${NOAGE_HOME}/aberp-snapshots" ]]; then
  pass "missing age fails up front with the install hint"
else
  fail "missing age: rc=$rc, snapshots dir exists=$([[ -e "${NOAGE_HOME}/aberp-snapshots" ]] && echo yes || echo no)"
fi

echo
if [[ $fails -eq 0 ]]; then
  echo "PASS: snapshot keychain round trip (age + legacy zip)"
else
  echo "FAIL: $fails assertion(s)" >&2
  exit 1
fi
