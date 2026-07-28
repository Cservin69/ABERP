# PRE-CUT INTEGRATION adversarial — PROD_v2.33.1 (2026-07-28)

**Delta reviewed:** `PROD_v2.33.0` (`c1c7431`) → `origin/main` (`d16f5f5`) — PR #39
(`250725e`), PR #40 (`7665416`), PR #41 (`3d9532d`), plus the `6dc1489` findings
commit.

**Verdict: GO**, with one defect found and FIXED in this session (E1 below). No
path was found on which a WRONG NAV FILING can still occur through this delta.

---

## E1 — FIXED — the email attachment render still forked the DB (PR #40 incomplete)

**Rank: highest of this session's findings, but NOT a NAV mis-filing.** It fails
loudly, audits the failure, and files nothing to NAV. The consequence is that the
buyer does not receive the invoice.

PR #40's finding doc states that `print_invoice::render_to_bytes` is

> reached in-serve from `serve::get_invoice_pdf`, which the PDF route, the
> auto-email compose path and the manual resend handler all funnel through

The auto-email compose path does **not** funnel through `get_invoice_pdf`.
`serve::send_invoice_email_route` (reached by BOTH `auto_send_after_issue` /
`SendTrigger::AutoOnIssue` AND the manual `POST /api/invoices/:id/email` resend)
passed `state.db_path` into `email_invoice::send_invoice_email`, which rendered
the attachment itself through the path-taking `print_invoice::render_to_bytes`.

Post-#40 that function opens its own `aberp_db::Handle` — a second DuckDB
instance co-resident with serve's. Under H3 checkpointing is disabled, so an
invoice issued in this serve process is WAL-resident and the second instance
reads the last-checkpointed subset.

The doc's own symptom table corroborates this. Row 1 reads

    auto-email on issue | compose failure: `render printed PDF for SMTP email
                          attachment: no InvoiceDraftCreated audit entry found …`

and `render printed PDF for SMTP email attachment` is `email_invoice`'s own
`.context()` string — not `get_invoice_pdf`'s. The operator's auto-email failure
came through the render call #40 did not touch. #40 genuinely closed rows 2 and
3 (the resend 404 and the PDF route / storno re-render).

### Detector gap, closed with it

`SERVE_HANDLE_LIVE` hooks `Ledger::open` and `DuckDbBillingStore::open`. It did
**not** hook `Handle::open`. So #40 moving this path from a bare `Ledger::open`
(hooked) to `Handle::open_default` (unhooked) took the strongest detector *off* a
path that still forked. `Handle::open` now calls `assert_no_serve_handle`; serve
registers its tripwire after its own boot open, so it cannot trip on itself.

`disable_checkpoint_on_shutdown` (which `Handle::open` applies, and the
`snapshot.rs` doc-comment cites as the reason a Handle "is not an independent
opener") stops a second Handle **tearing** the live WAL. It never stopped it
**reading stale**.

### Fix

* CAUSE — `send_invoice_email_route` renders via `get_invoice_pdf` (shared
  Handle, same renderer, byte-identical attachment) and passes the bytes down.
* CLASS — `SendInvoiceEmailInput` carries `pdf_bytes: &[u8]` instead of
  `db_path` + `seller_toml_path`. The capability is removed, not re-routed
  (rule 12): `email_invoice` can no longer open the DB at all.

### Mutation evidence, stated precisely

`tests/serve_email_attachment_handle_coherence.rs` drives the real auto-send tail
with the tripwire armed and SMTP pointed at a closed local port, asserting the
failure class is `transport` (i.e. the render completed). Asserted *positively* —
`!= "compose"` also holds for every early return above the render, so the same
test bounds that vacuous-pass path by exercising the recipient guard in-process.

* Revert `email_invoice.rs` + `serve.rs`, **keep** the `Handle::open` tripwire arm
  → **RED**, panic naming the fork verbatim.
* Revert the tripwire arm as well → the pre-fix tree **PASSES**. The stale read is
  nondeterministic (a co-resident fresh open sometimes replays the Handle's WAL) —
  precisely the reasoning `serve_modification_base_read_coherence.rs` gives for
  refusing to pin on observable staleness.

So the pin's teeth come from the detector, and the missing detector is why the
pre-#40 tree could not see this path either. Both halves ship together.

---

## What was attacked and HELD

### PR #41 (D2a) — the modification-route silent 0% re-file

* **Ordering verified in the post-fix tree, not assumed.** `derive_state_for`
  (`serve.rs:9446`, step 1) → `read_base_line_vat_kinds` (`:9501`) →
  `read_base_currency` (`:9525`) → `drop(base_conn)` (`:9527`) → dispatch (`:9616`).
  One `state.db.read()` serves both reads and is dropped before the write path, so
  no `write()` nests inside a held guard (rule 13).
* **The guard now BLOCKS on an empty/absent base read.** `read_base_line_vat_kinds`
  returns `Err` when `kinds.is_empty()`; `load_ready_invoice_by_id` returns
  `Ok(None)` only when the `invoice` row is absent, and that also lands in the
  empty branch. The `.find(|k| !k.is_percent())` guard can no longer pass
  vacuously.
* **The e2e genuinely reds pre-#41.** Reverting `serve.rs` to `baf5095`:
  `modification_must_block_when_the_base_vat_kinds_read_comes_back_empty` FAILS,
  and the failure message shows a modification was really issued —
  `TEST-INV-default/00002` — off an all-`Percent` 0% body. Not a near-miss.
* **Attacks that did not land:** only one in-serve dispatch into
  `issue_modification::modification_from_inputs` exists (`serve.rs:9616`); the CLI
  arm is a separate process that replays the base `input.json` verbatim. A
  mixed-kind base is caught (`.find` scans every line). Kinds come from the
  persisted `invoice_line.vat_rate_kind` column, populated by both the SPA and CLI
  allocators, so there is no CLI-issued gap to slip through.
* **Scanner teeth confirmed empirically.** A `Connection::open` planted inside
  `read_base_line_vat_kinds` makes `adr0099_read_fork_scan.awk` report
  `9727:read_base_line_vat_kinds:readfork@L9727`. The same body under an unpinned
  name is still missed — which the #41 commit states openly as D1. The ratchet is
  honest about its reach.

### PR #40 — the render fork

`get_invoice_pdf` is Handle-routed and the journey e2e covers issue → render →
resend → storno → storno render. The remaining in-serve fork was E1 above; with it
fixed, `print_invoice.rs:166` (`Handle::open_default`) is reached only from the CLI
one-shot. Every other non-test `Handle::open*` in the tree is a CLI one-shot, a
test fixture, or targets a *different* DB file (`serve.rs:4746`, the new tenant's
own ledger).

### PR #39 — foreign-partner save

* **Did not over-open.** Only the `Other` arm of `validate_partner_inputs`
  changed. `Domestic` still requires a well-formed HU ADÓSZÁM via
  `validate_tax_number`; `PrivatePerson` still forbids one; the HU-ADÓSZÁM-forbidden
  rule survives on `Other`.
* **No mis-file is reachable.** `issue_preflight.rs`'s `Other` arm independently
  requires a non-empty `community_vat_number`, runs `validate_community_vat_number`
  on it, and requires an `[A-Z]{2}` country code — all before a sequence number is
  burnt. Below it, `nav_xml.rs:1331` hard-errors when `community_vat_number` is
  `None` at render time. A foreign partner saved without a VAT number therefore
  bounces loudly at issue, and cannot reach NAV.

### I1 — `finalize_rate` — CONFIRMED LOCAL-ONLY, stays deferred

`huf_equivalent_total` has **zero** occurrences in `nav_xml.rs`. The wire derives
every HUF figure from `RateMetadata.rate` through `huf_equivalent_for`, applied
per line and per `summaryByVatRate` bucket (`nav_xml.rs:1671/1840/1858/1962-1964`).
`huf_equivalent_total` is consumed only by the PDF (`invoice-pdf/src/lib.rs:812`),
the `invoice` DB column, the SPA list/detail rows (`serve.rs:6481`) and
`reports.rs`. The whole-invoice single-rounding is genuinely local books + PDF.
**Deferred to a later cut, unchanged.**

### Cross-cutting gates

All 14 cut gates green on the merged tree, including the ADR-0106 NAV-emission
door gate (29 frozen records, closure holds, 4 registered doors, the 1
declared-direct door holds `validate_invoice_preflight`) and the opener census
(P1 + P2). No new read-fork, uncensused opener, or preflight bypass was introduced
by any of the three fixes. E1's fix adds no opener — it removes one call site and
adds a tripwire arm.

---

## Deferral ledger

| id | item | closes at |
|---|---|---|
| D1 | The read-fork scanner's `PINNED` ratchet is a name list; the same forking body under an unpinned name is invisible. Confirmed empirically this session. | the structural D1 rewrite (flag ANY fn holding a fresh opener whose reads leave the fn) |
| D2c′ | **Correction to #41's ledger.** It records the remaining mark-paid forks as reading "loudly like D2b". `read_invoice_total_gross_minor` (`serve.rs:10590`) does not: it returns `Option<i64>`, and `mark_paid_eligibility`'s `_ => Eligible` arm passes vacuously on `None` — structurally the same vacuous-pass shape as D2a. Blast radius is local only (mark-paid files nothing to NAV) and the storno gate above it is Handle-routed, so it cannot mis-file. | the mark-paid Handle migration |
| D4 | Arm `ABERP_SERVE_HANDLE_TRIPWIRE` in prod boot. With E1 fixed, no known in-serve `Handle::open` fork remains — "arm at zero" is closer, but D2c′'s bare `Connection::open` sites are unhooked by that tripwire anyway. | after the mark-paid migration |
| I1 | `finalize_rate` whole-invoice HUF rounding vs the wire's per-bucket rounding. Local books + PDF only, confirmed above. | a later cut |
| R7 | ADR-0106: `POST /invoices/:id/submit` files to NAV with no preflight and is absent from the door registry. Carried unchanged — no delta commit touches it. | the ADR-0106 door-registry widening |
