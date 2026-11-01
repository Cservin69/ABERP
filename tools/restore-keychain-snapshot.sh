#!/usr/bin/env bash
#
# restore-keychain-snapshot.sh — ADR-0113 — re-import the per-tenant
# keychain entries captured by tools/snapshot-prod.sh.
#
# Reads both snapshot formats, detected by content (not file name):
#   - <tenant>-<ts>-keychain.json.age  (current — age passphrase file)
#   - <tenant>-<ts>-keychain.zip       (legacy — `zip -e`, pre-ADR-0113)
# Either way the tool asks for the passphrase / zip password on the
# terminal, decrypts into a 0700 tempdir that is removed on exit, and
# re-imports each entry with `security add-generic-password -U`.
#
# The ABERP binary will ask once per entry to use a re-imported item
# (the item's ACL now trusts `security`, which created it) — answer
# "Always Allow" for aberp, per ADR-0020.
#
# Usage:
#   ./tools/restore-keychain-snapshot.sh <snapshot-file>           # confirm, then import
#   ./tools/restore-keychain-snapshot.sh --verify <snapshot-file>  # decrypt + check only
#   ./tools/restore-keychain-snapshot.sh --yes <snapshot-file>     # import, no confirm
#   ./tools/restore-keychain-snapshot.sh --help
#
# --verify is the restore drill: it proves the passphrase opens the file
# and the JSON is well formed, prints service/account pairs (never the
# secrets) and touches no keychain item.
#
# Exit codes: 0 ok, 1 decrypt/import failed, 2 usage / missing tool.

set -euo pipefail

MODE="import"
ASSUME_YES=0
SNAPSHOT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h)
      sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    --verify) MODE="verify"; shift ;;
    --yes|-y) ASSUME_YES=1; shift ;;
    -*)
      echo "[fail] unknown flag: $1" >&2
      exit 2
      ;;
    *)
      if [[ -n "$SNAPSHOT" ]]; then
        echo "[fail] exactly one snapshot file expected" >&2
        exit 2
      fi
      SNAPSHOT="$1"
      shift
      ;;
  esac
done

if [[ -z "$SNAPSHOT" ]]; then
  echo "[fail] usage: $0 [--verify|--yes] <snapshot-file>" >&2
  exit 2
fi
if [[ ! -f "$SNAPSHOT" ]]; then
  echo "[fail] snapshot not found: $SNAPSHOT" >&2
  echo "[hiba] a mentés nem található: $SNAPSHOT" >&2
  exit 2
fi

# ---------- detect format by magic bytes ------------------------------------
HEADER="$(head -c 21 "$SNAPSHOT" | LC_ALL=C tr -d '\0')"
if [[ "$HEADER" == "age-encryption.org/v1" ]]; then
  FORMAT="age"
elif [[ "$(head -c 4 "$SNAPSHOT" | od -An -tx1 | tr -d ' \n')" == "504b0304" ]]; then
  FORMAT="zip"
else
  echo "[fail] not an age or zip keychain snapshot: $SNAPSHOT" >&2
  echo "[hiba] nem age vagy zip kulcstartó-mentés: $SNAPSHOT" >&2
  exit 2
fi

case "$FORMAT" in
  age) TOOL="age" ;;
  zip) TOOL="unzip" ;;
esac
if ! command -v "$TOOL" >/dev/null 2>&1; then
  echo "[fail] '$TOOL' not found — install it with: brew install $TOOL" >&2
  echo "[hiba] a(z) '$TOOL' nincs telepítve — telepítés: brew install $TOOL" >&2
  exit 2
fi

# ---------- decrypt into a private tempdir ----------------------------------
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/aberp-restore.XXXXXX")"
trap 'rm -rf "${TMP_DIR}"' EXIT
chmod 0700 "$TMP_DIR"
DUMP_FILE="${TMP_DIR}/dump.json"

echo "[....] decrypting ${FORMAT} snapshot: $SNAPSHOT" >&2
echo "[....] ${FORMAT} mentés visszafejtése: $SNAPSHOT" >&2
(
  umask 077
  if [[ "$FORMAT" == "age" ]]; then
    # age reads the passphrase from the terminal; a wrong one fails the
    # authenticated header check, so a tampered file never decrypts.
    age -d -o "$DUMP_FILE" "$SNAPSHOT"
  else
    # Legacy ZipCrypto: unzip prompts for the password on the terminal.
    # The archive holds exactly one JSON member (snapshot-prod.sh ≤ 2.36).
    unzip -p "$SNAPSHOT" > "$DUMP_FILE"
  fi
) || {
  echo "[fail] could not decrypt $SNAPSHOT (wrong passphrase or damaged file)" >&2
  echo "[hiba] a visszafejtés nem sikerült (rossz jelmondat vagy sérült fájl)" >&2
  exit 1
}

# ---------- validate, list, import ------------------------------------------
# Python (stdlib only) parses the dump and drives `security`; secrets go
# from the JSON straight into the child's argv and are never printed.
python3 - "$DUMP_FILE" "$MODE" "$ASSUME_YES" <<'PYEOF'
import json, subprocess, sys

dump_path, mode, assume_yes = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
try:
    with open(dump_path, encoding="utf-8") as f:
        dump = json.load(f)
    entries = dump["entries"]
    for e in entries:
        if not all(isinstance(e.get(k), str) for k in ("service", "account", "password")):
            raise ValueError("entry missing service/account/password")
except Exception as exc:  # noqa: BLE001 — any malformed dump is one failure
    print(f"[fail] decrypted dump is not a valid keychain snapshot: {exc}", file=sys.stderr)
    sys.exit(1)

print(f"[ ok ] tenant={dump.get('tenant', '?')} captured_at={dump.get('captured_at', '?')} "
      f"entries={len(entries)}", file=sys.stderr)
for e in entries:
    print(f"       {e['service']} / {e['account']}", file=sys.stderr)

if mode == "verify":
    print("[ ok ] verify only — no keychain item was changed", file=sys.stderr)
    print("[ ok ] csak ellenőrzés — a kulcstartó nem változott", file=sys.stderr)
    sys.exit(0)

if not assume_yes:
    try:
        with open("/dev/tty") as tty:
            sys.stderr.write("Overwrite these keychain items? / Felülírjam ezeket? [y/N] ")
            sys.stderr.flush()
            answer = tty.readline().strip().lower()
    except OSError:
        answer = ""
    if answer not in ("y", "yes", "i", "igen"):
        print("[stop] nothing imported / semmi nem lett importálva", file=sys.stderr)
        sys.exit(1)

for e in entries:
    r = subprocess.run(
        ["security", "add-generic-password", "-U",
         "-s", e["service"], "-a", e["account"], "-w", e["password"]],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
    )
    if r.returncode != 0:
        print(f"[fail] import {e['service']} / {e['account']}: {r.stderr.strip()}",
              file=sys.stderr)
        sys.exit(1)
    print(f"[ ok ] imported {e['service']} / {e['account']}", file=sys.stderr)
print("[ ok ] keychain restore complete / kulcstartó visszaállítva", file=sys.stderr)
PYEOF
