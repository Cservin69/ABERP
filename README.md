# ABERP

**A free desktop ERP for small manufacturing shops.** Clone it, run one
command, and in about five minutes you have a working system on your own
Mac — quoting, invoicing, partners, products, machines, an approved-vendor
list, material traceability, and a tamper-evident audit trail. No SaaS, no
account to create, no monthly bill, no Docker. It runs locally as a single
desktop app and your data never leaves your machine.

ABERP started as a tool for Hungarian shops filing invoices through the
NAV Online Számla system. It has since grown a **Portable** edition that
anyone, anywhere can use — with the Hungarian tax integration switched off
and a demo company pre-loaded so the very first launch already has data to
click around in. It is multi-tenant (run several companies side by side),
multi-currency, and every change you make lands in an append-only,
hash-chained ledger you can inspect and verify.

> **License — free for non-commercial use (PolyForm Noncommercial 1.0.0).**
> You may use, run, modify, and share ABERP for any non-commercial purpose
> at no cost. Commercial use needs a separate arrangement — see
> [License](#license) below and [`LICENSE`](LICENSE) for the full terms.
> (Note: PolyForm Noncommercial is *source-available* and free, but it is
> not an OSI-approved "open-source" license, because it restricts
> commercial use.)

---

## Two editions

> **Both editions ship from a different repository.** This repo
> (`Cservin69/ABERP`) carries the **PROD** (HU production) and **DEV**
> launchers and the legacy unified `PROD_v2.*` line — nothing else. The
> **Portable** and **Defense** lines both live on
> **[`Cservin69/ABERP-Editions`](https://github.com/Cservin69/ABERP-Editions)**
> and are installed from there. Defense's `PROD_Defense_v*` refs were
> pruned from this repo on 2026-07-11 (see
> [`docs/PRUNED_DEFENSE_REFS.md`](docs/PRUNED_DEFENSE_REFS.md)); Portable's
> launcher pair was deleted on 2026-07-21 for the same reason — a launcher
> in the wrong repo is what misleads the next reader. **Nothing in this
> README installs either edition; follow the Editions repo.**

| | **Portable** | **Defense (HU production)** |
|---|---|---|
| Repo | `Cservin69/ABERP-Editions` | `Cservin69/ABERP-Editions` |
| Latest | **no release cut yet** — the line is parked | see the Editions repo |
| For | Anyone, anywhere — evaluating, or running outside Hungary | Hungarian manufacturing shops with NAV obligations + defense / aerospace compliance needs |
| Tax filing | **Off by default** — invoices stay local (LocalOnly) | Live NAV Online Számla 3.0 e-invoicing |
| First boot | Demo company pre-seeded — data to explore immediately | Your own seller profile + real NAV credentials |
| Build | Dev profile — structurally cannot reach the live NAV endpoint | `--features production` — the real-money build |
| Install | per the Editions repo, once a release is cut | per the Editions repo |

**Portable** is the path most newcomers want. It is the same application —
quoting, manufacturing, the audit ledger, all of it — with the Hungarian
NAV submission turned off per tenant. You can enter tax numbers for your
own country (they are stored as opaque strings for now; country-specific
tax modules are on the [roadmap](#roadmap)).

**Defense (HU production)** adds live NAV Online Számla 3.0 invoicing plus
the defense/aerospace compliance stack: approved-vendor screening,
purchase orders gated on that AVL, lot/heat material traceability, per-unit
part UID marking, an NCR/CAPA quality workflow with shipment gates, QC
inspection plans, and the production build that talks to the real NAV
endpoint. It is what Hungarian shops with real NAV submission obligations
run for real money.

> **The legacy unified `PROD_v2.27.76` line is frozen.** Up to that tag,
> Portable and Defense shipped as one build. New work now lands on the two
> dedicated lines above — Portable here, Defense on `ABERP-Editions` — so
> each edition gets a launcher and an upgrade path scoped to it. Existing
> `PROD_v2.27.76` installs keep working; there is just no `PROD_v2.27.77`.

---

## Quick start — Portable

**Portable is not installed from this repo.** It ships from
**[`Cservin69/ABERP-Editions`](https://github.com/Cservin69/ABERP-Editions)**,
alongside Defense. This repo's Portable launcher pair
(`run/run_portable.sh`, `run/upgrade_portable.sh`) was deleted on
2026-07-21: it pointed at a line this repo no longer owns, and a launcher
that should not be run is exactly the artifact that misleads the next
reader into treating a parked line as live.

**There is deliberately no Portable installer here right now.** The
Editions repo has not cut a Portable release yet — the line is parked on
purpose. Until it is cut, there is no install path; that gap is intended,
not a regression. Watch the Editions repo.

The `PROD_Portable_v0.1.0`–`v0.1.2` refs remain on this repo's origin —
they are ancestors of `main`, so nothing is unreachable and the old
launchers are recoverable from any of them
(`git show PROD_Portable_v0.1.2:run/run_portable.sh`). They are history,
not an install path.

> **macOS only, for now.** Shipped builds target macOS (the desktop shell
> and keychain integration need per-OS work). Linux and Windows are
> [roadmap](#roadmap) items — honestly not there yet.

### Prerequisites

The launcher needs these on your `PATH`; install them once if missing:

- **Rust** (stable channel) — `rust-toolchain.toml` pins the version, so
  `rustup` resolves it on first build.
- **Node.js 20+** with **npm**.
- **Python 3.11+** — only for the CAD geometry pipeline; quoting works
  without it, you just won't get geometry-driven machining estimates.

That's it. Build artifacts stay under `target/` and `apps/aberp-ui/ui/dist/`;
your runtime data lives under `~/.aberp/<tenant>/`.

---

## Defense (HU production) — installed from another repo

**For Hungarian operators with real NAV credentials and live NAV
submission obligations.** The Defense edition builds with
`--features production`, talks to the real NAV Online Számla endpoint, and
files invoices for real.

**It is not installable from this repo.** The live Defense line and its
`PROD_Defense_v*` releases are on
**<https://github.com/Cservin69/ABERP-Editions>** — clone that repo and
follow its README. This repo's `PROD_Defense_v0.1.x`–`v0.2.1` refs were
abandoned and pruned on 2026-07-11
([`docs/PRUNED_DEFENSE_REFS.md`](docs/PRUNED_DEFENSE_REFS.md)); do not
install from them.

Portable is **also** installed from that repo — see
[Quick start — Portable](#quick-start--portable). This repo installs
neither edition; it carries the PROD and DEV launchers and the legacy
unified line only.

---

## What it does

Organized the way an operator actually works. Tags mark where a feature
lives: **[both]** ships in Portable and Defense, **[Defense]** is part of
the HU-production compliance stack.

**Quote → price → win the job**

- **Quoting (CAD-aware)** *[both]*. Drop in an STL or STEP file → it
  extracts the geometry → estimates machining time → applies the margin
  profile for that customer type → shows a lead-time chip (green / yellow /
  red) → renders a customer-ready PDF. Quotes that would price below the
  margin floor are refused outright, not silently shipped.

**Procure → make → inspect → ship**

- **Approved Vendor List** *[Defense]*. Vendor CRUD with screening and
  approval categories (ITAR, EAR99, Aerospace, Defense, Nuclear), plus a
  purchase-order eligibility gate so unscreened vendors can't slip through.
- **Purchasing / purchase orders** *[Defense]*. Raise POs against the AVL
  (suspended or revoked vendors are blocked at create and issue); receiving
  a failed inspection auto-raises an NCR; defense lines require a heat lot
  captured at receipt.
- **Material traceability** *[Defense]*. Record heat-lot numbers and MTR
  (mill test report) URLs against inventory; for defense quotes the system
  refuses to start a work order until the heat lot is assigned — a
  chain-of-custody view shows the trail.
- **QC inspection plans** *[Defense]*. Record manual inspection results
  against a plan; the verdict math is calibration-stale-aware and grades by
  tolerance tier (1× / 2× the limit), auto-raising an NCR on the failing
  tier. The calibration-staleness window is per-tenant configurable.
- **Per-unit Part UID marking** *[Defense]*. Mint a per-unit UID and a
  DataMatrix payload for each part; the system **refuses to mark a defense
  shipment until every unit carries its UID**, with forward/reverse trace.
- **NCR / CAPA quality workflow** *[Defense]*. Non-conformance reports and
  corrective actions with a closed state machine; an open NCR **blocks the
  shipment**, and a Critical NCR escalates if not actioned within 24 hours.

**File the invoice**

- **Invoicing** *[both]*. Hungarian shops file directly to **NAV Online
  Számla 3.0** (issue, credit-note/storno, modification, with XSD
  validation and status polling). Everyone else runs **LocalOnly** — full
  invoices, no tax-office submission.

**Run the shop**

- **Master data** *[both]*. Partners, products, and machines, each with
  audited edits and an archive-don't-delete policy.
- **Multi-tenant + demo + NAV-off toggle** *[both]*. Run several companies
  from one install, switch between them, and flip NAV on or off per tenant.
  A bundled demo tenant seeds fresh installs so the first launch already
  has data to click through — this is what makes Portable boot straight
  into a populated dashboard.

**Prove what happened**

- **Audit ledger + audit screen** *[both]*. Every state change lands in a
  hash-chained, append-only ledger with an operator-visible screen (filter,
  sort, per-row hash check, whole-chain verdict). Sensitive payloads are
  redacted by default.
- **Snapshot system** *[both]*. Periodic, *validated* DuckDB snapshots
  (logical exports, smoke-tested on the way out) plus AES-256-GCM-encrypted
  CAD storage back the ledger up — a real rollback path, not a hopeful file
  copy.
- **Audit-chain DÁP / QES signing — coming soon** *[Defense]*. The
  scaffolding to anchor each ledger entry to a Hungarian government digital
  identity (DÁP eAzonosítás) and a NETLOCK qualified timestamp has landed
  on `main`, but is **not yet shippable**: the real DÁP and NETLOCK
  integrations are still pending (see [roadmap](#roadmap)).

---

## Why this is interesting

A few things under the hood that engineers tend to enjoy:

- **A hash-chained, immutable audit trail.** Every change is an
  append-only ledger entry chained to the one before it, so tampering is
  detectable from the bytes alone. `aberp-verify` re-checks an exported
  evidence bundle without trusting the running app.
- **One binary, no infrastructure.** A Rust backend with a Tauri 2 +
  Svelte 5 desktop shell, running in-process. No containers, no database
  server, no cloud — it launches like any other Mac app.
- **DuckDB for storage.** The embedded analytical database means
  finance-style aggregate queries (revenue, VAT, aging, cashflow) run
  against your live data without a separate warehouse.
- **Encrypted CAD at rest.** Uploaded CAD blobs are AES-256-GCM encrypted,
  with a read-audit trail and decrypt-to-temp handling for the extractor.
- **Corruption-recovery built in.** Periodic, *validated* DuckDB snapshots
  (logical exports, smoke-tested on the way out) give a real rollback path
  — not a hopeful file copy.

---

## Status

- **Portable — not released from this repo, and parked.** The line moved
  to [`Cservin69/ABERP-Editions`](https://github.com/Cservin69/ABERP-Editions),
  which has not cut a Portable release yet. This repo's launcher pair was
  deleted on 2026-07-21; `PROD_Portable_v0.1.2` (cut 2026-06-16) remains
  on origin as history, not as an install path.
- **Defense — not released from this repo.** The HU-production build with
  live NAV plus the defense/aerospace compliance stack (AVL, purchasing,
  heat/lot, part UID, NCR/CAPA, QC inspection) ships from
  [`Cservin69/ABERP-Editions`](https://github.com/Cservin69/ABERP-Editions).
  Its release refs and install procedure live there.
- **Legacy unified `PROD_v2.27.76` — frozen.** The last release before the
  Portable / Defense split. Still installable via
  `./run/upgrade_prod.sh PROD_v2.27.76` for existing operators (see the
  [runbook](docs/CUTOVER_RUNBOOK.md)), but no longer the path forward — new
  releases ship on the two lines above.

The test NAV path is the default for any build that does not pass
`--features production`; the production NAV endpoint is structurally
unreachable from a non-production build. That is exactly why Portable is
safe to hand to anyone.

---

## HU production install

The complete procedure — first-time prod branch, `seller.toml` template,
NAV + SMTP credentials, smoke-invoice checklist, rollback, and the ongoing
update workflow — lives in:

→ **[`docs/CUTOVER_RUNBOOK.md`](docs/CUTOVER_RUNBOOK.md)**

That runbook covers the `PROD_v2.*` line shipped from this repo, driven by
`./run/run_prod.sh` and `./run/upgrade_prod.sh`. **For the Defense edition,
use the [`ABERP-Editions`](https://github.com/Cservin69/ABERP-Editions)
repo and its own launchers** — there is no Defense install path here.

The versioning rules (when to bump patch vs minor vs major) are pinned in
[`adr/0056-versioning-policy.md`](adr/0056-versioning-policy.md).

---

## Roadmap

Honest about what isn't built yet:

- **Real DÁP / QES audit-chain signing (HU)** — the structural floor has
  landed: traits for the DÁP transport and a timestamp authority, an
  ed25519 session key, three signature columns on the ledger, and a
  per-tenant `dap_enabled` toggle (default off). What is still mocked: the
  real **DÁP eAzonosítás** operator-identity flow and the **NETLOCK
  qualified-timestamp** integration. Until those are wired, the chain
  signs with mocks and is not shippable as a compliance feature.
- **On-machine probe ingestion (real machine)** — the QC inspection
  workflow ships today with manual result entry; the **DMG MORI** (MTConnect)
  and **Renishaw** probe sources that would feed inspection values
  automatically are designed and stubbed, not yet talking to real hardware.
- **International tax modules** — Portable currently stores foreign tax
  numbers as opaque strings. Country-specific tax/e-invoicing modules are
  future work.
- **Linux / Windows** — macOS only today.

---

## Contributing

The repo lives at **<https://github.com/Cservin69/ABERP>**. Bug reports and
PRs are welcome — open an issue with a minimal repro. This is a
single-maintainer project, so there is no SLA, and unsolicited large
rewrites are unlikely to land.

Be aware the bar for a green build is high — every change runs through:

- `cargo fmt` (no diffs) and `cargo clippy` (zero warnings)
- `cargo test --workspace` — the full Rust suite, including the real-Python
  CAD smoke tests
- `vitest` and `svelte-check` for the SPA

The non-negotiable working principles (think before coding, simplicity
first, surgical changes, fail loud, …) are in [`CLAUDE.md`](CLAUDE.md). PRs
that ignore them get sent back.

---

## Project structure

```
ABERP/
  README.md            ← you are here
  LICENSE              ← PolyForm Noncommercial 1.0.0
  FOUNDATION.md        ← architectural spine — every ADR must be consistent with it
  CLAUDE.md            ← project-wide working agreement
  Cargo.toml           ← workspace manifest, pinned deps
  adr/                 ← Architecture Decision Records, numbered + indexed
  docs/
    CUTOVER_RUNBOOK.md ← prod cutover + update workflow (the source of truth)
    threat-model.md
  crates/              ← audit-ledger, nav-transport, quote-engine, inventory,
                         work-orders, qa, dispatch, mes, compliance, digital-id, …
  modules/billing/     ← NAV invoice issuing (ADR-0009)
  apps/
    aberp/             ← the Rust backend (HTTPS+JSON localhost service)
    aberp-ui/          ← Tauri 2 shell + Svelte 5 SPA (ADR-0004)
  run/                 ← launcher scripts (run_prod / upgrade_prod — PROD;
                         run_desktop / dev-test — DEV; release)
  tools/               ← operational scripts (snapshot, icons)
```

---

## License

ABERP is licensed under the **PolyForm Noncommercial License 1.0.0**. In
plain terms: free to use, run, modify, and share for any non-commercial
purpose; commercial use requires a separate arrangement with the
maintainer. The full text is in [`LICENSE`](LICENSE), and the canonical
terms are at <https://polyformproject.org/licenses/noncommercial/1.0.0>.

> *Required Notice: Copyright 2026 Ervin Aben*

---

## Credits & contact

Built in Hungary by Ervin Aben. Issues and pull requests:
**<https://github.com/Cservin69/ABERP>**.

> **Hungarian invoicing law is the operator's responsibility.** When NAV
> submission is on, ABERP files per the v3.0 spec — but the operator is the
> legally responsible party for the content of their invoices. ABERP is a
> tool; compliance is yours.

---

## Operator runbook — hülye-biztos cookbook

Field-tested commands, written against the legacy `run_prod.sh` /
`upgrade_prod.sh` launcher names with a `<VERSION>` placeholder. Swap for
your edition:

- **PROD (HU production)** — `*_prod.sh` and a `PROD_v2.*` tag.
- **Legacy unified line** — `*_prod.sh` and a `PROD_v2.*` tag.

Portable and Defense operators: these recipes do not apply — both editions
use their own launchers in the
[`ABERP-Editions`](https://github.com/Cservin69/ABERP-Editions) repo.

### 1. Upgrade to a new release (Frissítés új verzióra)

Kills running aberp, syncs to the release branch, snapshots, swaps the
binary, launches.

```bash
cd ~/ABERP && \
pgrep -f aberp | xargs -r kill 2>/dev/null; sleep 2; \
pgrep -f aberp | xargs -r kill -9 2>/dev/null; \
git fetch origin && git reset --hard origin/<VERSION> && \
./run/upgrade_prod.sh <VERSION>
```

### 2. Just relaunch (Újraindítás verzióváltás nélkül)

After a Ctrl-C or shutdown, when nothing changed and you want the app back up.

```bash
cd ~/ABERP && \
pgrep -f aberp | xargs -r kill 2>/dev/null; sleep 2; \
pgrep -f aberp | xargs -r kill -9 2>/dev/null; \
./run/run_prod.sh
```

### 3. Kill stuck aberp processes (Lefagyott aberp folyamatok kilövése)

When graceful shutdown didn't drain everything.

```bash
pgrep -f aberp | xargs -r kill 2>/dev/null; sleep 2; \
pgrep -f aberp | xargs -r kill -9 2>/dev/null
```

### 4. Emergency bypass — launch with a dirty tree (Vészhelyzeti megkerülés)

For dev workflows or when you've verified state by hand and know the git
check is a false positive. NEVER for casual prod use.

```bash
cd ~/ABERP && ABERP_SKIP_GIT_CHECK=1 ./run/run_prod.sh
```

### 5. Verify remote branch + tag SHAs before resetting (Távoli állapot ellenőrzése)

Sanity-check before any `git reset --hard origin/<VERSION>`.

```bash
git ls-remote https://github.com/Cservin69/ABERP.git \
  refs/heads/main refs/heads/PROD_v2.32.1 \
  refs/tags/PROD_v2.32.1
```

### 6. DuckDB snapshot / restore — the panic button (DuckDB pillanatkép)

Snapshots **just the tenant DuckDB** (binary-validated via
`PRAGMA verify_external_invariants`) to `~/Documents/ABERP-snapshots/` —
outside the repo and outside `~/.aberp/`. **Take one before every upgrade**,
especially across a one-way DuckDB storage bump. Best run with the app
stopped. `--db` defaults to `./aberp.duckdb`, so always pass the real path.

```bash
cd ~/ABERP
# Take a snapshot
cargo run -p aberp --release --bin aberp -- \
  snapshot --tenant prod --db ~/.aberp/prod/aberp.duckdb
# ... if an upgrade goes sideways, stop the app, then restore:
pgrep -f aberp | xargs -r kill -9 2>/dev/null
ls -lt ~/Documents/ABERP-snapshots/prod-*.duckdb | head -3
cargo run -p aberp --release --bin aberp -- restore-snapshot \
  --tenant prod --db ~/.aberp/prod/aberp.duckdb \
  --from ~/Documents/ABERP-snapshots/prod-TIMESTAMP.duckdb
```

`restore-snapshot` refuses while a server still holds the DB lock, and
refuses a backup that fails its own validity check — so it never clobbers a
working DB with a broken one.

### 7. Set up NAV creds + SMTP on a fresh box (Új gépen alapbeállítás)

For any **NAV-on (HU production)** install, after cloning and before the
first prod launch. (Portable needs none of this — NAV is off.)

```bash
cd ~/ABERP && ./run/setup_nav_creds.sh
# Then in Tenant Settings → SMTP → enter the SMTP password
# Then in Tenant Settings → Quote Intake (if enabled) → bearer token
```

### Forensics

- Audit ledger: `~/.aberp/<tenant>/audit-ledger.duckdb` + JSONL mirror
- DuckDB: `~/.aberp/<tenant>/aberp.duckdb`
- Seller config: `~/.aberp/<tenant>/seller.toml`
- Snapshots: `~/Documents/ABERP-snapshots/` (DuckDB) and
  `~/aberp-snapshots/` (encrypted tenant tarballs)
- Logs (Tauri): `~/Library/Logs/aberp/`

---

## Branding (optional)

- **Printed invoice:** drop a PNG at `~/.aberp/<tenant>/logo.png` (≤ 512×512,
  aspect preserved, fit into a 50×50-point box top-left). A malformed PNG
  loud-fails the render rather than shipping a logo-less PDF silently.
- **App header:** drop a PNG at `apps/aberp-ui/ui/static/aberp-logo.png`
  *before* building; the topbar wordmark swaps from text to your image. The
  directory is gitignored, so your asset stays private.

Both are pure filesystem convention — no config knob, no DB column.
Absent file → text-only header.

---

## Further reading

1. [`FOUNDATION.md`](FOUNDATION.md) — the architectural spine.
2. [`adr/README.md`](adr/README.md) — how ADRs work; numbered, in order.
3. [`docs/CUTOVER_RUNBOOK.md`](docs/CUTOVER_RUNBOOK.md) — the prod cutover +
   update procedure.
</content>
</invoke>
