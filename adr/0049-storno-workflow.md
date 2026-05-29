# ADR-0049 — Storno workflow (initiation → NAV emit → render → audit)

**Status:** Accepted — session 155 (2026-05-29). Canonical reference for the
multi-surface storno flow. Pins the workflow as it exists today and records the
one open NAV-emit defect (§NAV emit) plus the two SPA gaps (§Screen render,
§Initiation) that session 156 fixes against this contract.

Extends ADR-0023 (storno chain-link) + ADR-0036 (`derive_state` Storno label).
Does **not** supersede them. No schema change (ADR-0019 holds).

## Context

Session 155 diagnosed four operator-reported symptoms (Ervin, 2026-05-29
12:13–12:23). Three were real-but-bounded, one was a non-bug. The workflow had
drifted across the CLI, the loopback HTTPS route, the NAV emitter, the PDF
renderer, and two SPA surfaces; this ADR pins the contract so future sessions
cite it rather than re-derive it.

## Decision

### Initiation

Two operator entry points, **one backend route** (`POST
/api/invoices/:id/storno` → `storno_invoice_request` → `issue_storno::storno_from_inputs`):

1. **Detail-modal** (`InvoiceDetail.svelte`) — the canonical path. An **in-app
   inline confirm panel** (`stornoConfirmOpen`, PR-80) with the optional
   reason field. This is the path that works today.
2. **Row quick-action** (`InvoiceList.svelte::triggerRowStorno`) — a one-click
   list affordance. **DEFECT (§Initiation gap):** still gated by
   `window.confirm()`, which PR-80 deliberately abandoned in the modal because
   it is unreliable in the Tauri webview (returns falsy → `if (!ok) return`
   bails → "storno did not start"). **Decision:** the row action MUST route
   into the detail-modal inline panel (open the modal pre-armed), not fire its
   own `window.confirm`. One confirm surface, one reason-capture surface.
   Until session 156 lands that, the row action is the known-broken path.

A storno is legal **only** when the base is in `Finalized` state
(`check_base_is_finalized`); any other state is a `409 Conflict` naming the
current state.

### Required inputs

- Base invoice id (path param).
- **Storno reason: OPTIONAL** (PR-83 stance, held). Trimmed; empty-after-trim →
  `None`. Reuses the `invoice_note` column; never leaks into the NAV XML
  (ADR-0042). Buyer-facing only (PDF/email + screen).

### Backend flow

`storno_invoice_request` (`serve.rs`):

1. Precondition: derive base state; require `Finalized` (else 409).
2. Resolve base `nav_xml_path` from its newest `InvoiceDraftCreated` (ADR-0031).
3. Read sibling `<ULID>.input.json` (PR-47α side-store); loud-fail → CLI fallback.
4. Mint a fresh server-side XML output path for the storno.
5. Dispatch `issue_storno::storno_from_inputs`: reserve the storno's **own**
   sequence slot (the storno is itself an invoice — ADR-0023 §3), write its
   billing rows + NAV XML, write the chain-link audit entries, return summary.
6. **NAV is NOT called here.** The operator drives the subsequent submit
   (`POST /api/invoices/:id/submit`) — the storno appears in the list as a new
   invoice with a submit affordance. This is by design, not a duplicate
   (see §Idempotency).

### NAV emit

`nav_xml::render_storno_data` emits an `<invoiceReference>` block
(`originalInvoiceNumber` + `modifyWithoutMaster=false` + `modificationIndex`)
and **negates** line + summary amounts (the buyer-facing PDF reads this XML).

**DEFECT (open — session 156, needs NAV-XSD verification before shipping):**
NAV rejects the storno with `ABORTED` /
`validationErrorCode=LINE_MODIFICATION_EXPECTED`:

> "Tételsort tartalmazó módosító okirat esetén a tételsor módosítás jellegének
> megadása kötelező." (pointer:
> `InvoiceData/invoiceMain/invoice/invoiceLines/line/lineModificationReference`)

Any invoice carrying `<invoiceReference>` (storno **and** modification) must
emit a `<lineModificationReference>` as the **first child of each `<line>`**,
carrying `<lineNumberReference>` + `<lineOperation>`. The shared `write_lines`
path emits a plain NORMAL line, so **both** `render_storno_data` and
`render_modification_data` carry this latent gap. The `lineOperation` enum value
(`CREATE` vs `MODIFY`) MUST be confirmed against the NAV OSA 3.0 XSD before the
fix ships. The SOAP-envelope operation (`STORNO`/`MODIFY`, set at submit time)
is a separate concern and is NOT a substitute for the per-line element.

### Screen render (SPA)

The storno detail dialog shows the storno's own invoice number, a reference to
the base invoice number, and the optional reason.

**GAP (session 156):** amounts on the SPA detail/list show **positive**. The
billing tables store the storno line with a **positive** `unit_price`
(negation lives only in the NAV-XML render path), and the SPA `total_gross`
sums the positive rows. The screen MUST present negated amounts to match the
buyer-facing PDF and the operator's mental model.

### Email render (PDF)

**Already correct — verified session 155.** The PDF
(`print_invoice` → `invoice-pdf`) parses the negated NAV XML; `native_to_minor`
preserves the sign and `format_huf_forints` prefixes the minus. A rendered
storno PDF shows unit price `-131 175 Ft`, net `-9 313 425 Ft`, VAT
`-2 514 624 Ft`, total `-11 828 049 Ft`. **No change needed.** (The operator
report assumed positive; the on-disk render disproves it.)

### Audit trail

`InvoiceStornoIssued` is the chain-link entry. Payload
(`InvoiceStornoIssuedPayload`) carries `storno_invoice_id` + `storno_seq` +
`base_invoice_id` + `base_sequence_number` + `modification_index`. The storno
also writes its own `InvoiceSequenceReserved` + `InvoiceDraftCreated`. The
`Finalized → Storno` UI transition is **derived** (`derive_state`'s
`is_storno_base` arm via `base_invoice_id`), never stored.

### Idempotency — what prevents a "phantom duplicate"

**There is no phantom duplicate.** Session 155 read the full ledger for the
reported episode: exactly one `InvoiceStornoIssued` and four invoices total
(0031, 0032, 0033, and the storno 0034). The "third invoice" the operator saw
**is the storno** — a distinct invoice by design (ADR-0023 §3). The submit that
"failed" was the storno's own NAV submit hitting the §NAV-emit defect.

The single-storno guarantee is enforced by the `Finalized`-only precondition: a
second storno of the same base bounces 409 (the base is now in `Storno` state).
The CLI's `issue-storno` can still allocate `modification_index = 2+` for
legitimate multi-entry chains; the SPA route is scoped to the single-storno
operator path. **No backend path creates an invoice in response to a storno
succeeding** — the new invoice IS the storno.

### Closed vocab

Storno-related `EventKind` variants (audit ledger): `InvoiceSequenceReserved`,
`InvoiceDraftCreated`, `InvoiceStornoIssued`, `InvoiceSubmissionAttempt`,
`InvoiceSubmissionResponse` (or `InvoiceSubmissionAttemptFailed`),
`InvoiceAckStatus`, `InvoiceEmailedSent`. Derived UI state label: `Storno`
(ADR-0036). No new variant is introduced by this ADR (F12 ritual not fired).

## Consequences

- Session 156 has three scoped fixes against a fixed contract: (1) NAV
  `<lineModificationReference>` on storno **and** modification lines
  (verify `lineOperation` first); (2) SPA row-action routes into the modal
  panel; (3) SPA screen negates storno amounts. The PDF is done.
- The "phantom duplicate" is closed as a non-bug; future sessions must not
  re-chase it.
- The §NAV-emit defect is the only thing blocking a SAVED storno end-to-end.
