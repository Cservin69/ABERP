# DEV invoice #62 — every follow-up read of a live invoice 404'd (2026-07-27)

**Status:** FIXED. **Ship-blocking for PROD_v2.33.0:** no — the defect is
PRE-EXISTING and predates `PROD_v2.32.1` on both sides. It is, however, a live
prod-affecting defect on the currently-deployed line, so the fix should ride the
v2.33.0 cut rather than wait.

## Symptom

`inv_01KYJB52GGX6W2HGW44M58ZSJJ` (TEST-ABERPNEW2026/0062, domestic HU, gross
3 136 900 Ft) issued cleanly: sequence reserved, draft created, NAV ack `SAVED`,
state `Finalized`. Every follow-up operation that re-reads it failed:

| operation | result |
|---|---|
| auto-email on issue | `compose` failure: `render printed PDF for SMTP email attachment: no InvoiceDraftCreated audit entry found for invoice id inv_01KYJB52…` |
| Újraküldés (`POST /api/invoices/:id/email`) | 404 `invoice inv_… not found in audit ledger` |
| PDF route / cancel-side re-render | same |

SMTP was verified healthy by the operator's own SMTP test — the failure is
entirely compose-side.

## Root cause — a half-migrated read (ADR-0099 H3 / CLAUDE.md rules 13–14)

The issue path writes the audit ledger **and** the `invoice` / `invoice_line`
rows through the ONE shared `aberp_db::Handle` (`issue_from_parsed(&state.db)`).
Under H3 `HandleConfig::checkpoint_enabled` is `false`, so those rows stay
WAL-resident for the life of the serve process.

`print_invoice::render_to_bytes` — reached in-serve from
`serve::get_invoice_pdf`, which the PDF route, the auto-email compose path and
the manual resend handler all funnel through — opened **three** independent
DuckDB instances on the same file:

```
Ledger::open(db)                       -> the InvoiceDraftCreated lookup
Connection::open(db)  (notes)          -> invoice.invoice_note + invoice_line.note
Connection::open(db)  (bank snapshot)  -> the PR-73 bank_account_* columns
```

A separate instance does not replay the live writer's WAL (documented on
`Handle::read`), so all three read the last-checkpointed **subset** of the file.
The draft lookup found nothing and minted its operator-facing "wrong
`--id`/`--db`/`--tenant`" error — which is why the message pointed at tenant
scoping. Tenant scoping was never wrong: `state.db_path` and `state.tenant` are
the same values the issue path used.

### Why it looked intermittent

A co-resident fresh open *sometimes* folds the Handle's WAL into the main file
and therefore sometimes sees the rows. The same DEV DB emailed fine on
2026-07-20 (audit seq 147 was folded in time) and failed on 2026-07-27 (seq 158
was not). Nondeterminism is the defect, not a mitigating factor.

### Collateral: the DEV DB is torn

`apps/aberp-ui/aberp.duckdb` stopped advancing at audit seq **154** (file mtime
19:47:20, the boot heal's checkpoint) while its mirror reached seq **163**. The
whole 2026-07-27 session — including #62's `invoice.sequence_reserved`,
`invoice.draft_created`, `invoice.ack_status` — exists only in
`aberp.duckdb.audit.log`. The next boot's `mirror_ahead_heal` will replay
155–163 into the audit chain, but the **business rows never will**: the mirror
carries audit entries only, so `invoice` / `invoice_line` /
`invoice_sequence_reservation` for #62 are gone (verified: 0 rows). The same
tear ate seq 148–152 on 2026-07-20. DEV data is disposable; the point is that
the tear is real and had been silently eating rows for at least a week.

## Is it in the v2.33.0 delta?

**No.** Both halves predate `PROD_v2.32.1`:

* `print_invoice.rs`'s `Ledger::open` dates to "before session 58" (commit
  `a3458db`).
* `issue_from_parsed`'s `&Handle` parameter is a **context** line in the
  `PROD_v2.32.1..main` diff — the Handle migration landed earlier. The delta's
  only change to that file is the ADR-0103 B4 community-VAT normalisation.

The delta neither introduced nor aggravated it. It is orthogonal to ADR-0103
B2/B3/B4.

## Why no gate caught it

`tools/adr0099_read_fork_scan.awk` pairs a fresh opener with a typed ledger read
**in the same function**. `render_to_bytes` holds the `Ledger::open`;
`find_invoice_draft` holds the `.entries()`. The scanner's read-helper name list
(`list_notes_history(`, `pending_from_ledger(`) did not include
`find_invoice_draft(`, so CHECK N reported **ZERO** in-serve read-forks
throughout. This is the third patch to the same hole — see the residual below.

The `SERVE_HANDLE_LIVE` runtime tripwire *would* have caught it, but it is
disarmed by default (`ABERP_SERVE_HANDLE_TRIPWIRE`) and no test drove the
issue→render journey with a registered Handle.

## Fix

* `print_invoice::render_to_bytes_on_conn` — new Handle-routed core; the ledger
  read goes through `Ledger::from_connection(conn.try_clone())`, and both
  business-table loaders now take the caller's `&Connection`.
* `serve::get_invoice_pdf` passes `state.db.read()` (a `try_clone` of the ONE
  instance).
* `print_invoice::render_to_bytes` (CLI one-shot) opens a **process-local
  `Handle`** and reads through it — the `issue_invoice::run` idiom — so the file
  keeps no separate runtime opener (CHECK M).
* Opener census: **86 → 83** openers across **21 → 20** files.
* `find_invoice_draft(` added to the read-fork scanner's typed-read set. It is
  RED on the pre-fix tree (`print_invoice.rs:162:render_to_bytes:readfork@L162`)
  and green after.
* `tests/serve_invoice_journey_handle_coherence.rs` — issue a domestic HUF
  invoice → render the email attachment → resend → drive to `Finalized` via a
  NAV ack → storno → render the storno's own PDF, all with the tripwire armed.
  Verified RED on the pre-fix tree (tripwire panic at `Ledger::open`), green
  after.

## Deferral ledger

| # | item | closes with |
|---|---|---|
| D1 | The read-fork scanner still name-lists read helpers one at a time. The general shape — *a fn with a fresh opener that hands the opened `Ledger`/`Connection` to another fn* — is what should be flagged. | a structural CHECK N rewrite |
| D2 | `serve::read_base_currency` (`serve.rs:9622`) and `read_base_line_vat_kinds` do `Connection::open(&state.db_path)` in-serve on the modification route. They read **business** tables, so CHECK N (audit-only) is blind and CHECK M exempts `serve.rs`. Same stale-read hazard for a just-issued base invoice. | a business-table read-fork sweep of `serve.rs` |
| D3 | `serve::record_upgrade_snapshot_mismatch_audit` (`serve.rs:6149`) appends via a fresh `Ledger::open` at boot. Believed safe (runs before the Handle exists, process exits after) but unguarded. | the H3 boot-path sweep |
| D4 | Arm `ABERP_SERVE_HANDLE_TRIPWIRE` in prod boot — "arm at zero" is now closer, but D2's openers are still outstanding. | after D2 |
