# ABERP production cutover runbook

**Last updated:** 2026-05-30 — Session 169 / PR-169.
**Audience:** Ervin (sole operator).
**Language:** EN-primary; HU clarifications inline where they help at the
machine.

Real-money invoicing for **Áben Consulting Kft.** (tax `24904362-2-41`)
begins Monday **2026-06-01**, starting at invoice number
`ABERP/2026/0001`. This runbook is the step-by-step for the manual
cutover the afternoon of 2026-05-30.

The runbook is **hülye-biztos** by design — every step that touches the
real tax authority or real money has a confirmation gate, and any guard
that fails refuses to proceed.

---

## Pre-flight checklist

Before you touch anything, confirm all of the below. If any line is not
true, **stop**, fix it, then restart the runbook.

- [ ] **NAV production technical-user creds collected**: login,
  password, XML SIGN key, XML CHANGE/EXCHANGE key. Four secrets. The
  NAV credentials wizard (Step 4) will read them silently via the SPA.
- [ ] **SMTP working in test**: a recent test invoice email landed
  successfully from the dev tenant. The prod SMTP creds will be the
  same Zoho account; only the **host is different** — Zoho EU mailboxes
  use `smtppro.zoho.eu` (NOT `smtp.zoho.com` or `smtp.zoho.eu` — those
  are different hosts on Zoho's own infrastructure).
- [ ] **~15-minute uninterrupted window**: cutover itself is fast, but
  if a smoke invoice fails you want to debug while the binary is fresh
  in your head.
- [ ] **Audit ledger reviewed**: `git log --oneline -20` on `main`.
  Confirm the HEAD commit you're cutting over from is the one you
  expect.
- [ ] **Working tree clean** on the dev repo: `git status` shows
  nothing pending.
- [ ] **Coffee**.

---

## Step 1 — Publish the release branch (from the dev clone)

The S169 release model uses **per-release branches** on origin, not
tags. Dev publishes the ref; the prod machine clones from that ref and
builds locally. This decouples the dev tooling from the prod artifact —
an icons/ regression on dev cannot silently reach prod (the 2026-05-30
white-screen failure mode this PR exists to prevent).

```bash
# On the DEV machine, from the dev checkout (~/Documents/Claude/Projects/ABERP):
cd ~/Documents/Claude/Projects/ABERP
git status                         # must be clean
git checkout main
git pull --ff-only origin main     # match origin/main exactly
git push origin main               # if any local-only commits remain

# Publish the release branch:
./run/release.sh PROD_v1.0
```

`release.sh` will:

1. Refuse to run if invoked from inside `/Documents/Claude/Projects/` —
   the dev-workspace sentinel guards against running it from the wrong
   clone after cutover.
2. Refuse unless you're on `main` with a clean tree.
3. Validate the version matches `PROD_vMAJOR.MINOR` (uppercase, underscore).
4. Refuse if `PROD_v1.0` already exists on origin.
5. `git push origin main:refs/heads/PROD_v1.0`.
6. Print the operator's `git clone --branch …` command for Step 2.

> **Bootstrap caveat for the very first cutover only:** the dev-sentinel
> is in the script to support the *post-cutover* steady state, where
> release.sh is run from the prod clone. For the first cutover, you
> are necessarily running from the dev clone — bypass the sentinel
> just this once by doing the push manually:
>
> ```bash
> git push origin main:refs/heads/PROD_v1.0
> ```
>
> After cutover, all future releases run release.sh from the prod
> clone (Step 9), and the sentinel does its job.

**HU:** A release.sh feltolja a `PROD_vX.Y` ágat az originra; a
következő lépésben az éles gépen klónozod le erről az ágról.

---

## Step 2 — Clone the prod repo on the prod machine

On the prod machine (or in a fresh directory outside the dev workspace):

```bash
cd ~
git clone --branch PROD_v1.0 <origin-url> ABERP-prod
cd ABERP-prod
```

This gives you a clean working tree pinned to the release ref. All
subsequent steps run from inside `ABERP-prod/`.

> **Why a clone (not a worktree, not a copy)?** A clone is the smallest,
> most explicit unit. It carries its own `.git`, can be pulled
> independently for future releases, and is impossible to confuse with
> the dev checkout. (Background: parallel dev sessions occasionally
> `reset --hard` the shared dev checkout — a fresh clone is immune.)

**HU:** Klónozd a `PROD_v1.0` ágról egy fejlesztői munkamappán kívüli
helyre. Minden következő lépés ebből a mappából fut.

---

## Step 2a — Verify Tauri icons are populated (CRITICAL)

**Tauri 2 fails silently on missing or malformed app icons.** The build
succeeds, the window opens, but the WebView never initialises and the
operator sees a blank white screen with no error in the logs. The
2026-05-30 cutover hit this exact failure mode on a fresh clone.

The release branch ships placeholder icons at
`apps/aberp-ui/icons/`. Verify they exist and have non-zero size before
building:

```bash
ls -l apps/aberp-ui/icons/
# Expect: 32x32.png, 128x128.png, 128x128_2x.png,
#         icon.png, icon.icns, icon.ico
```

If any file is missing or zero-byte, regenerate from the script (the
icons are derived; the source script is committed):

```bash
python3 tools/generate-icons.py
```

> **Áben branding (deferred):** the icons in the repo today are a
> deliberately simple placeholder (dark navy + white "ABERP" wordmark).
> Real Áben branding will land in a follow-up PR when the logo file is
> available. To swap, drop a square PNG (≥1024×1024) at
> `tools/source-logo.png` and re-run `python3 tools/generate-icons.py`.

**HU:** A Tauri 2 hibásan vagy hiányosan megadott ikonok esetén csendben
üres fehér ablakot mutat (NSImage init nil, hibaüzenet a logban nincs).
A release-ág placeholder ikonokat tartalmaz; ellenőrizd, hogy léteznek
és nem 0 bájtosak, mielőtt buildelnél.

---

## Step 3 — Set up the prod seller config (via SPA wizard)

The prod tenant lives at `~/.aberp/prod/`. The launcher creates the
directory on first run; you populate `seller.toml` **through the SPA
seller wizard** (PR-51 / session 71). No hand-editing required.

```bash
./run/run_prod.sh
```

What happens:

1. Backend boots, detects `~/.aberp/prod/seller.toml` is absent (or
   identity-incomplete) → boot state `NeedsSellerConfig`.
2. SPA renders the seller wizard. Fill the form:
   - Legal name: `Áben Consulting KFT.`
   - Tax number: `24904362-2-41` (the S166 boot sanity check refuses
     any other value for prod — this is intentional)
   - EU VAT number: `HU24904362`
   - Address (country `HU`, postal code, city, street + house number)
   - Bank details (account number, IBAN, bank name, SWIFT/BIC)
3. SPA POSTs the form; backend atomically writes
   `~/.aberp/prod/seller.toml`, including the
   `[seller.numbering]` section that yields `ABERP/2026/0001`
   (annual reset).
4. SPA reloads; boot state transitions to the next missing-thing
   (likely `NeedsKeychainCreds`, see Step 4).

> **Why a wizard, not hand-edited TOML?** The wizard's atomic write
> preserves invariants the operator can't easily replicate (S148
> seller.toml write-path invariant — bank section and identity section
> must round-trip without clobbering each other). Hand-editing risks
> losing one section if you forget to re-append the other.

**HU:** Az eladó-konfigurációt a SPA-ban beépített varázslóval töltsd
ki — ne kézzel szerkeszd a TOML-t. A backend atomikusan írja ki és
megőrzi az invariánsokat (S148).

---

## Step 4 — Populate NAV credentials (via SPA wizard)

After Step 3 saves, the backend re-evaluates boot state. If NAV
credentials are absent **and** the first-launch ceremony has not yet
been acknowledged, the SPA renders the NAV credentials wizard (S133
keychain prompt chaining).

Fill the four fields:

| Field                       | Source                            |
|-----------------------------|-----------------------------------|
| Technical-user login        | NAV invoice service registration  |
| Technical-user password     | NAV invoice service registration  |
| Software ID                 | NAV registered software ID        |
| Exchange (CHANGE) key       | NAV invoice service registration  |

Click **Save**. The SPA POSTs to the keychain setup route, which writes
the macOS keychain entry at:

- service: `aberp.nav.prod`
- account: `nav_credentials_blob` (consolidated blob per PR-57; four
  secrets inside)

The ACL persists across launches; you will not be re-prompted on next
boot.

**Verify** the keychain write landed (optional sanity check; the SPA
already confirmed):

```bash
security find-generic-password -s "aberp.nav.prod" -a "nav_credentials_blob" -g
# (prompts for your macOS login password; you don't need to read the
#  value — just confirm the entry exists.)
```

**HU:** A NAV technikai-felhasználói adatokat a SPA varázsló kéri be
és írja a macOS kulcstartóba. Nincs CLI-parancs erre — a varázsló az
egyetlen ajánlott út.

---

## Step 5 — Set up SMTP credentials (via SPA Tenant Settings)

SMTP is configured through **Tenant Settings → SMTP delivery** in the
SPA (PR-92, ADR-0047). The password is write-only in the UI — once
saved it is never re-displayed, only re-entered.

Navigate to: Tenant Settings → SMTP delivery → fill in:

| Field         | Value                                             |
|---------------|---------------------------------------------------|
| Host          | **`smtppro.zoho.eu`** (NOT `.com`, NOT plain `.eu`) |
| Port          | `465`                                              |
| Security      | TLS (implicit) — Zoho EU's `smtppro` listener on 465 |
| From address  | your Zoho mailbox address                          |
| Username      | your Zoho mailbox address (same as From)           |
| Password      | Zoho **app-specific password** (NOT your account password) |

Click **Test Connection** (PR-98) — the backend opens a TLS connection
to the host, runs AUTH, and sends a one-line test email to yourself.
**Do not proceed past this step until Test Connection succeeds.** A
failed Test Connection now means a failed smoke-invoice email in
Step 7.

On success, click **Save**. The backend writes the password to the
macOS keychain at:

- service: `aberp.smtp.prod`
- account: `smtp_password`

> **Zoho EU pitfall** — `smtp.zoho.com` is Zoho's US infra; `smtp.zoho.eu`
> exists but is for non-pro accounts. The Zoho **Workplace Pro** EU
> tenant uses `smtppro.zoho.eu` specifically. Authenticating with the
> wrong host will surface as TLS handshake or AUTH failures; the SPA's
> "Test Connection" button is the fast way to confirm before the first
> live email.

**HU:** SMTP-t a SPA Beállítások → SMTP-szállítás oldaláról állítod be.
A Zoho EU **Workplace Pro** host pontos neve: `smtppro.zoho.eu` (a
sima `smtp.zoho.eu` MÁS, és nem fog működni). A **Kapcsolat tesztelése**
gomb sikeres futása előtt NE LÉPJ TOVÁBB.

---

## Step 6 — First-launch ceremony

After Steps 3–5, the SPA renders the **first-prod-launch modal** (S166).
The touchfile `~/.aberp/prod/.first-launch-acknowledged` is absent on
first boot, so all main routes are blocked behind a confirmation modal.

You must type **`ABERP`** (uppercase, exact). On submit:

- The touchfile is written with an RFC3339 timestamp.
- A `FirstProdLaunchAcknowledged` entry is appended to the audit ledger.

This is one-time. Subsequent launches skip the modal.

What the binary checked at boot (S166 `sanity_check_environment` —
informational, in case you see the loud-fail banner):

- **A. Seller identity** — `seller.toml` must have
  `tax_number = "24904362-2-41"`. Mismatch = fatal. Missing file =
  deferred to the seller wizard (Step 3).
- **B. NAV credentials** — keychain entry must exist if the
  first-launch ceremony was previously acknowledged. On first boot this
  gate is permissive (the wizard populates).
- **C. SMTP** — missing `[seller.smtp]` is a **warning**, not fatal;
  configure via Step 5 after launch.

The loud-banner module also prints "PRODUCTION BUILD — REAL NAV — REAL
MONEY" in red/yellow on the launching terminal (bilingual EN+HU) before
the binary takes over.

**HU:** Az első éles indítás során egy megerősítő ablak jelenik meg —
gépeld be: `ABERP` (csupa nagybetű, pontosan). Ez egyszeri; a
megerősítést `~/.aberp/prod/.first-launch-acknowledged` rögzíti és az
audit ledgerbe is bekerül (`FirstProdLaunchAcknowledged`).

---

## Step 7 — Smoke invoice

The point of the smoke invoice is to prove the full prod path —
NAV submit, ack poll, PDF, email — works end-to-end before you sit
down to issue the real first invoice on Monday.

1. Pick a small, internal-target invoice (e.g. a HUF 1 line to a partner
   you control). It will be a **real** NAV submission and a **real**
   email — pick the recipient accordingly.
2. Issue it from the SPA.
3. Watch the invoice-detail page:
   - The number should be **`ABERP/2026/0001`** (no `TEST-` prefix —
     prod builds drop it).
   - NAV submit transitions to `RECEIVED` → `PROCESSING` → `DONE`
     (the S161 poll daemon handles the transitions automatically).
   - Email delivery status reaches `Sent`.
4. Open the PDF — verify the seller block (legal name, tax, address,
   bank) renders from your Step-3 `seller.toml`.

**If anything fails:**

- NAV submit `ABORTED` with a business rule violation: read the ack
  XML. `customerAddress` is a classic gotcha for PRIVATE_PERSON buyers
  (see `[[reference_nav_gotchas]]` memory).
- Email `Failed`: check the SMTP host. `smtppro.zoho.eu` is the right
  host for the EU pro tenant (Step 5).
- PDF wrong: re-check the seller wizard data (you can edit values via
  the SPA after first-launch; in extreme cases clear
  `~/.aberp/prod/seller.toml` to retrigger the wizard).
- White window with no logs: see **Troubleshooting** at the end of this
  runbook.

If the smoke invoice lands cleanly, **prod is live**. The S168 PDF fix
(re-source PRIVATE_PERSON buyer name/address from input.json) is
already in this release; you should not see address-empty regressions.

**HU:** A smoke invoice egy valódi (de általad kontrollált) NAV-beadás
+ e-mail küldés. Csak akkor lépj tovább a valódi szabályos
számlázásra, ha a teljes lánc lement.

---

## Step 8 — Rollback procedure

If something goes wrong after a release lands and you've already
issued one or more invoices, **do not panic and do not delete
anything**.

The audit ledger is append-only (ADR-0008) and `ensure_schema` is
idempotent — rolling back to a previous prod build will not corrupt
any prior invoice's audit trail.

To roll back to a previous release branch:

```bash
# 1. Stop the running app (Ctrl-C in the run_prod.sh terminal).
# 2. Inside the prod clone, switch to the previous release branch.
cd ~/ABERP-prod
git fetch origin
git checkout PROD_v0.9      # whichever previous release-branch you want
# 3. Relaunch — run_prod.sh rebuilds with the new HEAD.
./run/run_prod.sh
```

The DuckDB file at `~/.aberp/prod/aberp.duckdb` is preserved across
binary versions; migrations are forward-only and run at boot
(`ensure_schema`). A rollback launches against the existing DB with
the previous code; if the previous code's schema is older, **do not
roll back across a destructive migration without first restoring a
DB snapshot from before the migration ran**.

> **Snapshot the DB before any cutover or upgrade.**
> Standard belt-and-suspenders backup:
> ```bash
> cp ~/.aberp/prod/aberp.duckdb \
>    ~/.aberp/prod/aberp.duckdb.snapshot-$(date +%Y%m%d-%H%M%S)
> ```
> Do this before Step 2 and before any future Step 9 upgrade. DuckDB
> is a single file; the snapshot IS the rollback target.

**HU:** A rollback biztonságos, mert az audit ledger csak hozzáfűzhető,
és a séma-migrációk ütközésmentesek (`ensure_schema` idempotens). DB-
snapshotot mindig csinálj BÁRMILYEN frissítés előtt.

**If you need to invalidate an issued invoice**, that's NOT a rollback
— that's a stornó. Use the SPA's storno workflow (S156); don't try to
fix it by reverting code.

---

## Step 9 — Ongoing update workflow

Routine: dev work continues to land on `main`. When you want a fix or
feature to reach prod:

```bash
# 1. (Optional but recommended) DB snapshot first.
cp ~/.aberp/prod/aberp.duckdb \
   ~/.aberp/prod/aberp.duckdb.snapshot-$(date +%Y%m%d-%H%M%S)

# 2. From the DEV clone: publish a new release branch.
cd ~/Documents/Claude/Projects/ABERP
git checkout main
git pull --ff-only origin main
./run/release.sh PROD_v1.1     # bump the minor

# 3. On the PROD clone: pull the new release branch.
cd ~/ABERP-prod
git fetch origin
git checkout PROD_v1.1

# 4. Stop the running app (Ctrl-C in the run_prod.sh terminal),
#    then relaunch:
./run/run_prod.sh

# 5. Smoke-test on a low-stakes path before bulk-issuing.
```

**Schema migrations** are automatic — `ensure_schema` runs at boot
and applies any new migrations forward. The DB write-lock is released
when the old binary exits (the run_prod.sh process group sends SIGTERM
on Ctrl-C, drop handlers release the lock). If the relaunch fails with
"database is locked", check no stray aberp/aberp-ui process is running:
`pgrep -f aberp`.

**HU:** A `main` ág a fejlesztés gerince; minden éles frissítéshez egy
új `PROD_vX.Y` ágat hozunk létre (release.sh push-olja). Az éles gépen
csak `git fetch && git checkout PROD_vX.Y && ./run/run_prod.sh` — a
többi automatikus.

---

## Troubleshooting

### Blank white window on launch (no logs, no error)

**Symptom:** `./run/run_prod.sh` finishes the cargo build, launches
aberp-ui, but the window renders fully white. No errors in stdout,
stderr, or the Console.app system log.

**Cause:** missing or malformed Tauri app icons under
`apps/aberp-ui/icons/`. macOS `NSImage` init returns nil on a
zero-byte or missing icon file; the WebView never reaches `loadURL`;
the window stays at the initial white frame.

**Fix:**

```bash
ls -l apps/aberp-ui/icons/
# Any zero-byte file → regenerate.
python3 tools/generate-icons.py
# Then rebuild + relaunch:
./run/run_prod.sh
```

If `tools/generate-icons.py` itself errors with "PIL not installed":
`pip3 install --break-system-packages Pillow` and retry.

### SMTP "Test Connection" times out

Usually wrong host. `smtppro.zoho.eu` (NOT `.com`, NOT plain `.eu`).
See Step 5.

If you confirmed the host is right and it still times out: check the
firewall — port 465 outbound must be open. macOS Application Firewall
(System Settings → Network → Firewall) is fine by default; corporate
VPNs sometimes block 465 specifically.

### NAV submit stuck at `RECEIVED` for >5 minutes

The S161 poll daemon is meant to escalate from 1/2/4/8/16/30/60s to
steady 60s. If `RECEIVED` does not transition to `PROCESSING`:

- Check the NAV status page: <https://onlineszamla.nav.gov.hu/>
- The audit ledger entry shows the request URL and timestamps —
  compare against the daemon's expected next-poll time.
- `pgrep -f aberp` should show the running binary; if it's not there,
  the daemon died — relaunch the app and the boot-recovery path
  (S161) will resume polling.

### "Working tree dirty" from release.sh after a clean pull

Usually `target/` or `node_modules/` getting touched by an editor's
sidecar process. `git status --short` will name the culprit; the
fixes are usually either:

- `cargo clean` followed by a fresh build (target/), OR
- `rm -rf node_modules apps/aberp-ui/ui/node_modules && (cd apps/aberp-ui/ui && npm install)`.

If `git status --short` shows changes you didn't make and don't
recognise, **stop and investigate** — do not blindly `git checkout .`,
that destroys in-progress dev work from parallel sessions (see memory
`feedback_aberp_shared_checkout_concurrent_branch_hopping`).

### `PROD_vX.Y already exists on origin` error

Pick the next minor. The script suggests it in the error message.

---

## Appendix A — File and keychain inventory

What lives where after a successful cutover:

| Location | Owner | Lifetime |
|----------|-------|----------|
| `~/.aberp/prod/seller.toml` | SPA seller wizard (PR-51) | tenant-lifetime |
| `~/.aberp/prod/aberp.duckdb` | DuckDB | tenant-lifetime |
| `~/.aberp/prod/.first-launch-acknowledged` | first-launch ceremony | tenant-lifetime |
| `~/.aberp/serve/prod/` | TLS cert + key for the loopback listener | regenerated as needed |
| `~/.aberp/prod/invoices/<id>/` | side-store per-invoice artifacts (input.json, nav_xml, PDF) | invoice-lifetime |
| macOS keychain: `aberp.nav.prod` / `nav_credentials_blob` | SPA NAV creds wizard (S133) | tenant-lifetime |
| macOS keychain: `aberp.smtp.prod` / `smtp_password` | SPA Tenant Settings → SMTP PUT (PR-92) | tenant-lifetime |
| macOS keychain: `aberp.nav.prod` / `session_token` | serve at boot | per-binary-build |

**Backups:** the DuckDB file IS the database. Snapshot it before any
upgrade (Step 8/9 instructions). Side-store directories
(`~/.aberp/prod/invoices/<id>/`) are also load-bearing — `input.json`
and `nav_xml` are referenced by audit replay and the PDF print path.
A backup strategy that covers `~/.aberp/prod/` entirely is the
simplest correct posture.

---

## Appendix B — Why the dev-DB-disposable rule reverses at prod cutover

During dev, `main` may include destructive migrations or schema
rewrites against a dev tenant's DuckDB. The dev DB is disposable —
delete and re-issue.

From the prod cutover onwards, **the prod DB is the legal record of
issued invoices**. Hungarian tax law requires retention of issued
invoices for years. The prod DB is no longer disposable; every
schema change must be forward-compatible, and the audit ledger is
append-only (ADR-0008).

This is the single most important behavioural shift at cutover, and
the reason the runbook bangs the snapshot-the-DB drum at every step.

---

## Appendix C — Quick reference card

| Need to... | Command |
|------------|---------|
| Publish a release branch (from dev) | `./run/release.sh PROD_vX.Y` |
| Clone a release on the prod machine | `git clone --branch PROD_vX.Y <origin-url> ABERP-prod` |
| Launch the prod app | `./run/run_prod.sh` |
| Set up / rotate NAV creds | SPA NAV credentials wizard (boot route, S133) |
| Set up / rotate SMTP creds | SPA → Tenant Settings → SMTP → Test Connection → Save |
| Regenerate placeholder icons | `python3 tools/generate-icons.py` |
| Verify NAV creds are in keychain | `security find-generic-password -s "aberp.nav.prod" -a "nav_credentials_blob"` |
| Verify SMTP creds are in keychain | `security find-generic-password -s "aberp.smtp.prod" -a "smtp_password"` |
| Snapshot the DB | `cp ~/.aberp/prod/aberp.duckdb ~/.aberp/prod/aberp.duckdb.snapshot-$(date +%Y%m%d-%H%M%S)` |
| See recent audit entries | (via the SPA's audit timeline on the invoice detail page) |
| Roll back to previous release | `cd ABERP-prod && git fetch && git checkout PROD_vX.Y-prev && ./run/run_prod.sh` |
| Re-trigger first-launch modal | `rm ~/.aberp/prod/.first-launch-acknowledged && ./run/run_prod.sh` |

---

*This runbook is the single source of truth for the prod cutover.*
*If you find a step that doesn't match reality, fix the runbook — it*
*is the artifact that survives across sessions.*
