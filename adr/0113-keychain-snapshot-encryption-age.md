# ADR-0113 — Keychain snapshot encryption: `age` replaces `zip -e`

- **Status:** Accepted — implemented and tested on branch `followups-1-6` (2026-10-31).
- **Date:** 2026-10-31
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR. It narrows the deferred "Backup, encryption-at-rest
  key management" ADR (README §Deferred) to one artefact: the pre-upgrade keychain dump written
  by `tools/snapshot-prod.sh`. Key escrow and the off-machine backup path stay deferred.
- **Related:** ADR-0055 (tenant-state inventory; row "Keychain entry"), ADR-0100 §0 decision 2
  (`sops`+`age` chosen as the secrets substrate), ADR-0020 (keychain ACL posture), ADR-0111
  (the follow-up review that raised this).

## Context

`snapshot-prod.sh` step 3 dumps the tenant's NAV credentials blob and SMTP password to JSON
and encrypts it with `zip -e`. That is the traditional PKWARE cipher (ZipCrypto):
- it is not authenticated, so a modified archive is not detected;
- it falls to a known-plaintext attack. The member name and the JSON prefix
  (`{\n  "tenant": "prod",`) are predictable, which gives an attacker the plaintext they need.

The file holds the most sensitive secrets ABERP has: the NAV technical-user signing and
exchange keys. They sit under `~/aberp-snapshots/` and accumulate one per upgrade.

There was also no restore tool. CUTOVER_RUNBOOK told the operator to unzip by hand and run
`security add-generic-password` for each entry.

## Decision

1. **Cipher.** Encrypt the dump with `age -p` (age v1, passphrase mode).
   - Key derivation is scrypt. The payload is ChaCha20-Poly1305 in 64 KiB chunks, with an
     HMAC over the header.
   - A wrong passphrase, a truncated file or a single flipped bit all fail with no output.
   - age is the tool ADR-0100 already chose for secrets. No second crypto tool enters the
     stack, and it is a single static binary from `brew install age`.
2. **Passphrase.** age reads it twice from the terminal, never from argv, env or stdin.
   - An empty answer makes age generate a strong passphrase and print it once.
   - The script never sees or stores the passphrase.
3. **File name.** The output is `<tenant>-<ts>-keychain.json.age`, mode 0600, written under
   umask 077.
4. **No fallback.** If `age` is missing, `snapshot-prod.sh` fails at preflight (exit 2) with
   `brew install age`, before it writes anything.
   - Because `upgrade_prod.sh` aborts on a failed snapshot, a missing tool blocks the upgrade
     instead of silently producing the weak format.
5. **Restore.** `tools/restore-keychain-snapshot.sh <file>` detects the format by magic bytes
   (`age-encryption.org/v1` or `PK\x03\x04`), so legacy `.zip` snapshots keep restoring.
   - It decrypts into a 0700 tempdir that is removed on exit.
   - It lists service/account pairs (never secrets) and asks before overwriting.
   - It re-imports each entry with `security add-generic-password -U`.
   - `--verify` is the restore drill: decrypt and validate, touch no keychain item.
   - `--yes` skips the confirmation.

### Considered

- **`openssl enc -aes-256-cbc -pbkdf2`.** Rejected: it is unauthenticated. The `enc` command
  does not support GCM, so it would need an encrypt-then-MAC construction built by hand.
- **`hdiutil` AES-256 encrypted sparse image.** It is native to macOS, but it is heavyweight
  for a 2 KB file and harder to script and test.
- **A 7-Zip AES-256 archive.** It is a new tool with a larger attack surface than age, and
  ADR-0100 already standardised on age.
- **age with a native X25519 key instead of a passphrase.** That moves the problem to storing
  the key, which is the deferred escrow ADR. A passphrase matches today's operator flow.

## Consequences

- **Prerequisite.** `age` must be installed on the prod MacBook (`brew install age`). It was
  verified present there (v1.3.2, `/opt/homebrew/bin/age`) on 2026-10-31.
- **Timing.** `upgrade_prod.sh` runs the **outgoing** release's `snapshot-prod.sh` before it
  switches branches. So:
  - the upgrade *to* the first release with this ADR still writes a `.zip`;
  - the first `.age` snapshot comes from the upgrade after that.

  The restore tool reads both, so this is harmless.
- **Old snapshots.** Existing `.zip` snapshots stay readable but weak. The operator may
  re-encrypt them or delete them once a newer `.age` snapshot exists. This ADR does not
  automate either step.
- **ACL after restore.** Re-imported items are created by `security`, so the aberp binary
  prompts once per item. Answer "Always Allow" for aberp (ADR-0020).
- **Exposure during restore.** Restore passes each secret to `security` on argv for the
  lifetime of that child process. On macOS only the same uid and root can read another
  process's arguments. This is the same exposure as the old manual runbook step.
- **Tests.** `run/tests/snapshot_keychain_roundtrip_test.sh` drives the real age and unzip
  prompts on a pty with expect(1). It uses a stub `security` and a throwaway HOME, so no real
  keychain is touched. It covers:
  - the age file is written, 0600 and contains no plaintext;
  - a byte-exact round trip with quote, backslash, `$` and UTF-8 secrets;
  - a wrong passphrase and a tampered file both import nothing;
  - `--verify` and answering "no" import nothing;
  - a legacy `zip -e` snapshot restores byte-exact;
  - a missing age fails before writing.

  The test needs a pty, so in Claude Code it runs outside the sandbox. It is a local gate
  step; CI runs on Linux and does not run `run/tests`.
