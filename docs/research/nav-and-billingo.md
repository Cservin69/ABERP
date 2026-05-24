# NAV Online Invoice 3.0 and Billingo — research findings

> **Research, not decision.** ADR-0009 and ADR-0010 are the decisions; this
> file is the source material they cite. Open questions in this document are
> for external check — a Hungarian developer with shipped NAV experience for
> protocol questions, and the project's accountant for fiscal-rule questions.

- **Compiled:** 2026-05-19
- **Scope:** NAV Online Számla API v3.0 (primary); Billingo as a one-time
  migration source (secondary). Self-billing covered only as a deferred-item
  note.
- **Author note:** every non-trivial protocol claim has a URL next to it.
  Items marked **[OPEN]** need external verification before they bind a
  decision in ADR-0009 or ADR-0010.

## Decided project framing this research must respect

- API version target: **NAV v3.0** (confirm still current as of May 2026).
- Submission posture: **direct to NAV from day one**. Billingo is not on the
  issuance path.
- Billingo's role: **one-time migration source only** — bulk import of a
  switching customer's prior customer list and historical invoice archive.
  Also a client-acquisition convenience. No ongoing sync.
- Self-billing (önszámlázás): **deferred** to a later ADR.
- Currency: **HUF only** for v1.
- Certification posture: NAV conformance achieved **before first real
  customer goes live**.
- Offline alert policy: alert on either **N queued invoices OR M minutes
  since oldest unsubmitted**, whichever trips first. Concrete N/M to be
  decided in ADR-0009.

## ⚠ Conflict surfaced for resolution before ADR-0009

**ADR-0007 §Transport states: "mTLS where the counterparty supports it (NAV
does)."** The research evidence below does not support this. NAV's public
spec, every consulted open-source NAV client (pzs/PHP, angro-kft/Node), and
all community technical-user setup guides describe auth as **application-
level**: a per-request `<user>` block with `passwordHash` (SHA-512) +
`requestSignature` (SHA3-512), plus an AES-128/ECB-decrypted `exchangeToken`,
over plain HTTPS to `api.onlineszamla.nav.gov.hu`. No request shows a client
X.509 certificate. Until external check confirms otherwise, ADR-0007's
"mTLS to NAV" wording is likely wrong and should be rewritten as
**"technical-user password + `xmlSignKey` + `xmlChangeKey` held in OS
keychain"**. This affects how ADR-0009's security section is written.

---

# NAV Online Invoice 3.0 — research findings

## API version status

- **Current schema: v3.0.** As of 15 May 2025 NAV ended support for XML v2.0;
  all live submissions use 3.0. ([Marosa](https://marosavat.com/vat-news/hungary-changes-to-real-time-reporting-validations);
  [VATupdate](https://www.vatupdate.com/2025/09/02/nav-online-invoice-system-2025-stricter-validation-rules-effective-september-15-impacting-businesses/))
- **No v3.1 / v4.0 announced** publicly. Minor point releases continue;
  third-party indexing cites **v3.0.4** as of Feb 2026. **[OPEN]** Confirm
  exact current patch level from
  `https://github.com/nav-gov-hu/Online-Invoice/blob/master/src/schemas/nav/gov/hu/OSA/CHANGELOG_3.0.md`
  (GitHub fetch was blocked from the research environment; Ervin to verify).
- **`requestVersion`** must be `3.0`; **`headerVersion`** must be `1.0`.
  ([pzs/nav-online-invoice README](https://github.com/pzs/nav-online-invoice))
- **Canonical interface spec PDF (HU):**
  `https://onlineszamla.nav.gov.hu/api/files/container/download/Online_Szamla_interfesz%20specifikacio_HU_v3.0.pdf`.
  English mirror exists. **[OPEN]** Verify the PDF footer date matches the
  active patch level — NAV updates the schema and changelog more often than
  the consolidated PDF.
- **Wider regulatory backdrop:** Hungary is in consultation (NGM/NAV, period
  running through Jan 2026) on ViDA-driven mandatory structured e-invoicing,
  phasing in 2026–2028. Policy level, not a near-term schema break.
  ([Sovos](https://sovos.com/regulatory-updates/vat/hungary-plans-to-introduce-mandatory-e-invoicing-and-real-time-reporting-revealed/);
  [Banqup](https://www.banqup.com/resources/blog/vida-hungary-e-invoicing-rtir-guide))
- **September 15, 2025 validation tightening:** 15 messages reclassified
  from WARN to ERROR; 3 new WARN messages. ABERP integration tests must
  include the post-Sept-2025 validator surface. ([RSM](https://www.rsm.hu/blogs/tax/nav-online-invoice-system-stricter-validations-from-september-underscore-navs-commitment);
  [Accace](https://accace.com/hungarian-online-invoice-system-changes/);
  [Grant Thornton HU](https://grantthornton.hu/en/news/nav-online-invoice-system-changes-2025))

## Submission flow

Endpoints under `/invoiceService/v3/`:

- Production: `https://api.onlineszamla.nav.gov.hu/invoiceService/v3/`
- Test: `https://api-test.onlineszamla.nav.gov.hu/invoiceService/v3/`

Operations (REST POST, XML in/out, every call carries an authenticated
`<user>` block):

1. **`tokenExchange`** — returns an opaque `<encodedExchangeToken>` that the
   client must decrypt with **AES-128/ECB** using the taxpayer's
   `xmlChangeKey`. Token validity is reported as ~5 minutes; used as input
   to `manageInvoice` / `manageAnnulment`. ([RSM blog](https://www.rsm.hu/en/blog/2020/05/connectax-online-invoice-what-to-prepare-for-july))
2. **`manageInvoice`** — submits a batch of 1..N homogeneous operations
   (CREATE / MODIFY / STORNO). Returns a `transactionId`. In v3.0 the
   `technicalAnnulment` boolean is removed from this op — annulment is its
   own endpoint.
3. **`manageAnnulment`** — submits a **technical annulment** (data-submission
   correction; does NOT cancel the invoice as a legal document). Split out
   from `manageInvoice` in v3.0.
4. **`queryTransactionStatus`** — async per-`transactionId` status query.
   Per-invoice (`index`) results with severity WARN / ERROR / INFO. States
   include PROCESSING / SAVED / DONE / ABORTED. SAVED is terminal-success;
   ABORTED is rejection.
5. **`queryInvoiceData`** — full invoice data by invoice number; caller must
   be supplier or customer.
6. **`queryInvoiceCheck`** — boolean existence check.
7. **`queryTaxpayer`** — VAT-number validation, returns null / false /
   `TaxpayerDataType` with name and address.
8. **`queryInvoiceDigest`** — paginated search by direction
   OUTBOUND / INBOUND.
9. **`queryInvoiceChainDigest`** — paginated traversal of base + every
   amendment/storno in the chain, across systems. Important when ABERP
   amends an invoice originally issued in Billingo.
10. **`queryTransactionList`** — paginated list of submissions in a time
    range, scoped to the taxpayer.

**Submitting an invoice = two calls:** `tokenExchange`, then `manageInvoice`
with the decoded token in the request.

**Submission is asynchronous by design.** NAV does not synchronously run
business rules; `manageInvoice` returns a `transactionId` and ABERP must
poll `queryTransactionStatus` (PROCESSING → SAVED / ABORTED, typically a
few seconds). The state machine in ADR-0009 must reflect this — there is
no synchronous "ack" beyond the schema-validation result on the immediate
HTTP response.

**Response signing — [OPEN]:** Did not find a clear statement that NAV
signs HTTP response bodies beyond TLS. Treat response signing as not
required from clients unless §1.5/§1.6 of the v3.0 PDF says otherwise.
External check required.

## Auth and request signing

- **Technical user** created by the taxpayer in their NAV Online Számla web
  UI; not provisioned by the software vendor. Two crypto keys are issued
  alongside the technical user:
  - **`xmlSignKey`** — input to the per-request signature.
  - **`xmlChangeKey`** — AES key used to decrypt the exchange token.
  ([BerényiSoft how-to](https://berenyisoft.com/en/how-to-create-a-technical-user-on-the-nav-online-invoice-interface/);
  [tudastar.szamlazz.hu](https://tudastar.szamlazz.hu/en/gyik/technical-guide-for-nav-online-invoicing-data-registration))
- **`<user>` block per request contains:**
  - `login` — technical user login
  - `passwordHash` (SHA-512 of password, attribute `cryptoType="SHA-512"`)
  - `taxNumber` (8-digit base of taxpayer's tax number)
  - `requestSignature` (SHA3-512, attribute `cryptoType="SHA3-512"`)
- **`requestSignature` formula:**
  - Non-`manageInvoice` calls: `SHA3-512(requestId + requestTimestamp(UTC, YYYYMMDDhhmmss) + xmlSignKey)`.
  - `manageInvoice` / `manageAnnulment`: same input plus, **per invoice index**, a SHA3-512 hash of `operation + base64(invoiceData)` concatenated in index order. (Previous CRC32 design was replaced by SHA3-512 in v2.0; this still holds in v3.0.) ([pzs README](https://github.com/pzs/nav-online-invoice))
- **Exchange-token decryption:** `<encodedExchangeToken>` is AES-128/ECB.
  Key is `xmlChangeKey`. Decryption yields a plaintext token to include in
  `manageInvoice` / `manageAnnulment`.
- **mTLS to NAV — [OPEN, high-priority]:** Public docs and every consulted
  client library transmit over plain HTTPS with **no client certificate**.
  This contradicts ADR-0007 §Transport. Most likely outcome: ADR-0007 to be
  rewritten so the keychain holds the technical-user password,
  `xmlSignKey`, and `xmlChangeKey` — not an "mTLS cert". External check
  with a Hungarian NAV integrator confirms whether mTLS appears anywhere in
  the contract.
- **Exchange-token lifetime — [OPEN]:** Community reports ~5 minutes;
  confirm exact value from the interface PDF.

## Idempotency model

- **No `Idempotency-Key` header.** NAV's identifiers are:
  - `transactionId` — NAV-assigned per batch, returned by `manageInvoice`.
  - `index` — client-assigned position within a batch (1..N).
- **Server-side duplicate guard** is on `invoiceNumber` per supplier
  taxpayer. A second CREATE for the same `invoiceNumber` rejects with
  `INVOICE_NUMBER_NOT_UNIQUE`.
- **For MODIFY / STORNO,** uniqueness is `(invoiceReference, modificationIndex)`.
- **What ABERP must build:**
  - **Client-side idempotency key** = the ULID of the issuance command (per
    ADR-0005). Persisted with the row that records the submission attempt.
  - On retry: if a prior `transactionId` was received, **do not resubmit** —
    poll `queryTransactionStatus`. If no `transactionId` was received,
    resubmitting is safe; NAV's `invoiceNumber` duplicate guard catches a
    true double-send.
  - **Network-reset disambiguation:** on connect-reset after the request
    has reached the server but before the response returned, immediately
    call `queryInvoiceCheck` / `queryInvoiceDigest` for the invoice number
    to determine whether the prior submission landed. (Pattern observed in
    angro-kft/nav-connector.)
- **[OPEN]** Does NAV expose any server-side dedup window keyed on the
  client `requestId`? Spec PDF §"Hibakezelés és újrapróbálkozás" should
  clarify.

## Error taxonomy

- **Top-level result codes** seen across libraries:
  - `OK` — accepted
  - `ERROR` — synchronous reject (schema, signature, auth failures)
  - `WARN` — accepted with caveat (post-Sept-2025: many former WARNs are now ERROR)
- **Common result-code values:**
  - `INVALID_REQUEST_SIGNATURE` — signature math mismatch. **Not retryable.**
  - `INVALID_SECURITY_USER` — bad technical user / keys. **Not retryable.**
  - `INCORRECT_REQUEST_SCHEMA` / `SCHEMA_VIOLATION` — XML schema failure.
    **Not retryable.**
  - `INVOICE_NUMBER_NOT_UNIQUE` — business uniqueness failure. **Not
    retryable.** Indicates duplicate or that the prior attempt already
    landed (see Idempotency).
  - `OPERATION_FAILED` — server-side transient. **Retryable** with backoff.
  - HTTP 504 — **retryable** with backoff.
  - HTTP 500 / connect-reset — retryable, treat as suspect, disambiguate
    with `queryInvoiceCheck`.
- **Per-invoice business validation** is reported inside
  `queryTransactionStatus` per `index`, severity WARN / ERROR / INFO. From
  15 Sept 2025: 15 former-WARN messages became ERROR. Code paths that
  previously tolerated a WARN as "accepted, will fix later" now produce
  rejection. Examples: customer tax-number checksum errors, missing/wrong
  VAT exemption codes, product-fee discrepancies. ([Accace](https://accace.com/hungarian-online-invoice-system-changes/);
  [WTS Klient](https://wtsklient.hu/en/2025/06/13/failed-data-reporting/))
- **[OPEN]** Authoritative full error-code list, especially the 15
  reclassified messages, lives in the v3.0 PDF "Hibakódok" section and the
  developer log at `https://onlineszamla.nav.gov.hu/fejlesztoi_naplo`.
  Compare against pzs library's code map.

## Storno and modification

Three distinct concepts — do not conflate:

1. **Modification (módosítás / helyesbítés).** A new invoice referencing the
   base via `<invoiceReference>` with `<modificationIssueDate>`,
   `<modifyWithoutMaster>`, and `<modificationIndex>` (starts at 1, increments
   per amendment in the chain). Submitted via `manageInvoice` with
   `operation=MODIFY`.
2. **Storno (sztornó).** The legal cancellation. Identical API shape to
   MODIFY (uses `<invoiceReference>` and `<modificationIndex>`) but
   `operation=STORNO`. A storno is itself an invoice with its own number; it
   consumes a slot in the gap-free sequence.
3. **Technical annulment (technikai érvénytelenítés).** Not a legal
   cancellation — withdraws an erroneous *data submission* only (e.g. a test
   invoice accidentally sent to prod). Endpoint: `manageAnnulment`. Requires
   the issuer to mark it, then the receiver to confirm in the NAV web UI.

**Chain rules:**

- `modificationIndex` is unique per `invoiceReference`. NAV enforces
  uniqueness; gap-detection is "still being investigated" per a NAV
  comment on Issue #174 — clients must still assign contiguous indices.
- **Storno of a storno — [OPEN, accountant]:** API-permitted to chain a
  MODIFY against the storno's own invoice number with
  `modificationIndex=1`. Whether Hungarian accounting practice prefers
  this over a fresh corrective invoice is an accountant question.
- `queryInvoiceChainDigest` returns the full chain across invoicing
  systems — useful for migrated-from-Billingo invoices when ABERP needs
  to know the next `modificationIndex`.

## Sequence-number rules

- **Statutory basis:** Act CXXVII of 2007 on VAT (Áfa törvény), §169,
  transposing EU Directive 2006/112/EC Art. 226. ([EC Hungary VAT rules](https://vat-one-stop-shop.ec.europa.eu/national-vat-rules/hungary-vat-rules_en);
  [ICNL HU VAT Act EN](https://www.icnl.org/research/library/hungary_vatact_eng/);
  [NAV booklet 18](https://nav.gov.hu/pfile/file?path=%2Fen%2Ftaxation%2FBooklets%2F18---basic-rules-of-issuing-invoices-and-receipts))
- **Implementing decree — [OPEN, accountant]:** Government Decree
  23/2014 (VI. 30.) NGM on invoicing rules. Confirm not superseded.
- **Concrete rules:**
  - **Gap-free continuous numbering** within each numbering range used by
    the taxpayer (per VAT-ID).
  - **Multiple parallel series allowed** (e.g., domestic / foreign /
    branch / document type). Each series individually gap-free.
  - **Annual reset optional.** Sequence may roll across fiscal years or
    restart at year boundary; both legal as long as continuity within
    each series is maintained.
  - **Storno and modification invoices consume sequence slots.**
- **Implementation implication for ABERP:** with ULID as canonical row
  identity (ADR-0005) and no FKs (ADR-0019), the invoice number is a
  separate generated field with strict gap-free invariants. The reservation
  pattern is essentially mandatory — once reserved, the number must be
  used. Discarding silently is non-compliant.
- **[OPEN, accountant]** What does Hungarian practice require for a
  reserved number whose invoice issuance failed before submission? A
  void / cancellation record, or must the gap be filled with a corrective
  storno?

## Self-billing (deferred — brief note only)

- Hungarian term: **önszámlázás.** Customer issues the invoice on behalf
  of the supplier under a written agreement.
- v3.0 schema marker: `<selfBillingIndicator>` boolean.
- **Data-reporting obligation remains with the supplier**, even though
  the customer's software generates the XML. This is the practical
  complication that justifies deferral. ([ado.hu](https://ado.hu/szamvitel/meghatalmazotti-szamlakibocsatas-onszamlazas-iii-resz/);
  [nav-gov-hu Issue #326](https://github.com/nav-gov-hu/Online-Invoice/issues/326))
- **Out of scope for v1**, ADR-0009, and the rest of this research.

## Certification and conformance

- **Today (May 2026):** no formal mandatory software certification. NAV
  does not currently run an accreditation programme. Compliance is
  established by behaviour — schema-compliant XML, successful submission,
  passing validations.
- **Planned mandatory accreditation** is in the consultation pipeline as
  part of ViDA implementation: preliminary accreditation for software
  vendors and taxpayer accreditation for in-house software, with a 30-day
  grace before sanctions. **No firm date.** ([Dr. Ildikó Nagy](https://drnagyildiko.hu/en/blog/mandatory-e-invoicing-nav-compliance-hungary/))
- **Test environment:**
  - Web UI: `https://onlineszamla-test.nav.gov.hu/`
  - API: `https://api-test.onlineszamla.nav.gov.hu/invoiceService/v3/`
  - Test technical users self-provisioned per integrator; no shared
    credentials.
- **Audit-evidence inspectors typically request:**
  - Invoice numbering policy doc — gap-free proof.
  - Audit log of every submitted `transactionId`, response, and per-`index`
    status.
  - XML actually submitted for any sampled invoice, parsed and verbatim
    (ABERP's hash-chained ledger per ADR-0008 covers this directly).
  - Evidence that all issued invoices were submitted (no missing
    submissions). ([Grant Thornton audit simulation](https://grantthornton.hu/en/services/digital-services/nav-online-invoice-audit-simulation);
  [RSM](https://www.rsm.hu/blogs/tax/nav-online-invoice-system-stricter-validations-from-september-underscore-navs-commitment))
- **[OPEN]** Whether NAV publishes a recommended test-suite covering all
  CRUD/MODIFY/STORNO/ANNUL paths or whether each integrator constructs
  their own. Best authoritative source: `https://onlineszamla.nav.gov.hu/dokumentaciok`.

## Reference open-source clients (architectural comparison only — not deps)

1. **pzs/nav-online-invoice (PHP)** — most complete community library
   against v3.0. Closely tracks NAV changelogs. Distinguishes the
   `manageInvoice` per-index signature math from other operations. Performs
   local XSD validation before submitting. Does **not** bake retry or
   idempotency — leaves both to the caller.
   ([repo](https://github.com/pzs/nav-online-invoice))
2. **angro-kft/nav-connector (Node.js)** — Axios-based. Defines a clear
   retryable-error matrix (HTTP 504, `OPERATION_FAILED`, network-shaped
   errors retryable; everything else caller-must-fix). Token exchange +
   manage in one method; retry loop owned by the caller.
   ([repo](https://github.com/angro-kft/nav-connector))
3. **chilkat code samples** — reference signature math for C#, Go, Python,
   classic ASP. Useful as cross-check when building the Rust signing path.
4. **Szotasz/nav-online-invoice-mcp** — MCP server wrapping v3.0; useful as
   a sanity check on operation names and tool coverage.

**Architectural takeaways for ABERP:**

- None of these clients implement an internal retry/idempotency cache.
  They treat NAV's invoice-number uniqueness as the authoritative dedup
  guard and require the application to track `transactionId`.
- All compute `requestSignature` deterministically per-request; per-invoice
  partial hashes for `manageInvoice` computed at submit time.
- **None implement mTLS to NAV** — second confirmation of the conflict
  with ADR-0007 §Transport.
- pzs's local XSD pre-validation before paying the network round-trip is
  worth adopting in ABERP's command handler.

---

# Billingo — one-time migration research

## Auth and rate limits

- **Auth method (v3):** API key in `X-API-KEY` header (Bearer prefix
  optional). Generated in the user's Billingo dashboard. No public OAuth
  flow in v3 SDKs. ([Billingo support](https://support.billingo.hu/content/446136358);
  [yunehu/billingo-v3](https://github.com/yunehu/billingo-v3))
- **Base URL:** `https://api.billingo.hu/v3`
- **Rate limits — [OPEN]:** The legacy v2 docs explicitly capped the invoice-
  download endpoint at 10 req/min/IP. Current v3 rate limits are **not
  publicly documented**. Pace at ≤1 req/sec conservatively, and contact
  Billingo support before scheduling a multi-thousand-invoice migration.
- **Lifecycle:** the migration tool consumes the customer's API key, runs
  the migration, then **discards** the key. ABERP does not store Billingo
  credentials long-term.

## Customer export

- **Endpoint:** `GET /partners`, paginated by `page` and `per_page`. Hard
  max `per_page` = 50; default 20. Pagination via Link header `rel="next"`.
- **Partner shape:** `id`, `name`, `address{country_code, post_code, city, address}`,
  `emails[]`, `taxcode`, `tax_number`, `eu_tax_number`,
  `small_taxpayer` (bool), `cash_settled` (bool).
- **Migration mapping for ABERP:**
  - Billingo `id` → ABERP ULID via a mapping table held in the migration
    audit-ledger entries.
  - `emails[]` → preferred-email + secondary list, normalized.
  - `small_taxpayer` flag matters for HU VAT logic and must survive.
- **[OPEN]** Whether `GET /partners` exposes deleted/archived partners via
  a query parameter (`include_deleted` or similar). If not, soft-deletes
  silently drop during export.

## Invoice-archive export

- **Endpoints under `/documents`** (Billingo's unified term for invoice,
  receipt, proforma):
  - `GET /documents` — paginated list with filters (date, partner, type).
  - `GET /documents/{id}` — full record with line items, VAT breakdown.
  - `GET /documents/{id}/download` — PDF binary.
  - `POST /document-exports` + `GET /document-exports/{id}` +
    `/download` — async bulk export, returns a binary archive.
- **Line-item availability:** YES per-document. Summary-only is not the
  only option.
- **Archive depth:** API surfaces all documents the customer has,
  regardless of age — no documented "X years back only" cap. Hungarian VAT
  archival requires 8 years; customers should have at least that available.
  **[OPEN]** Whether Billingo gates archive depth by plan tier.
- **NAV-chain integration during migration — important:** Historical
  invoices in Billingo were already submitted to NAV by Billingo. ABERP
  **must not re-submit** during migration. Approach: store imported
  invoices with `source = "billingo_migration"` provenance marker in the
  audit ledger; reconcile against NAV via `queryInvoiceCheck` and
  `queryInvoiceData` for sample / spot-check verification.
- **[OPEN]** Whether Billingo's API exposes the NAV `transactionId` it
  received for each invoice. If yes — capture during migration. If no —
  reconcile by `(invoiceNumber, supplier tax_number)` against NAV.

## One-time-migration quirks

- **Pagination caps are aggressive** (50/page on partners; documents
  conservatively assumed similar). A long-lived customer = thousands of
  HTTP calls. Budget a multi-hour batch at ~1 req/sec.
- **Bulk export is async** — `POST /document-exports` returns an export ID;
  poll the state endpoint; download when ready. Operator-facing UI must
  show progress and survive operator-walks-away (resumable from the export
  ID).
- **Two parallel sources for line items.** Bulk export gives a compact
  archive; per-document GET gives full structured fidelity. **For a
  faithful migration, prefer per-document GET** even though it's slower —
  bulk export's structured-data fidelity is **[OPEN]** and the doc archive
  may be PDF-centric.
- **Tax-number formats** in Billingo records are not always normalized
  (HU prefix sometimes present, sometimes absent; spaces). Canonicalize to
  8-digit base + 1-digit VAT code + 2-digit county code before storing in
  ABERP and before any `queryTaxpayer` validation.
- **Out of scope (explicit non-research per project framing):** webhooks,
  ongoing sync, invoice issuance via Billingo. Not investigated.

---

# Consolidated open questions for external check

These need resolution before or during ADR-0009 / ADR-0010 finalization.
Suggested owner in **bold**.

**Hungarian developer with shipped NAV experience:**

1. Confirm current NAV v3.0 patch level from
   `https://github.com/nav-gov-hu/Online-Invoice/blob/master/src/schemas/nav/gov/hu/OSA/CHANGELOG_3.0.md`.
2. Confirm the v3.0 interface PDF footer date matches the active patch
   level.
3. **mTLS to NAV — does it exist?** Most likely no. If confirmed no,
   ADR-0007 §Transport needs rewriting and the keychain content list
   changes from "mTLS cert" to "technical-user password + `xmlSignKey` +
   `xmlChangeKey`".
4. Does NAV sign HTTP response bodies beyond TLS? (Spec §1.5 / §1.6.)
5. Exact exchange-token lifetime (community says ~5 minutes).
6. Complete enumerated error-code list for v3.0, including the 15 messages
   reclassified from WARN to ERROR on 15 Sept 2025.
7. Server-side dedup window: does NAV cache anything against `requestId`
   that mitigates connect-reset double-sends, or is `invoiceNumber`
   uniqueness the only guard?
8. NAV behaviour on connect-reset between server receipt and client
   response — confirm pattern (poll `queryInvoiceCheck` to disambiguate).
9. Existence of a NAV-published conformance test-suite or checklist
   (`https://onlineszamla.nav.gov.hu/dokumentaciok`).

**Accountant:**

10. Government Decree 23/2014 (VI. 30.) NGM — confirm current consolidated
    version is in force.
11. Treatment of a reserved-but-unused invoice number whose issuance
    failed pre-submission. Void marker acceptable, or must the gap be
    filled with a corrective storno?
12. Storno-of-a-storno practice — is a chained MODIFY against the storno
    acceptable, or must a fresh corrective invoice be issued?
13. Confirm parallel-series rules (per-series gap-free; annual reset
    optional) are still standing practice in 2026.

**Billingo support (direct contact):**

14. Current v3 rate limits — concrete numbers for `/partners`,
    `/documents`, `/document-exports`.
15. Filter to include deleted/archived partners and documents
    (`include_deleted` or equivalent).
16. Bulk-export (`/document-exports`) structured-data fidelity — does the
    output include line items and VAT rows in machine-readable form, or is
    per-document GET required for faithful migration?
17. Archive depth by plan tier — does plan affect how far back the invoice
    archive is retrievable?
18. Does Billingo store and expose (via API) the NAV `transactionId` it
    received for each submitted invoice?

---

# Sources

**NAV official**

- Online Számla landing — `https://onlineszamla.nav.gov.hu/`
- v3.0 interface spec (HU PDF) — `https://onlineszamla.nav.gov.hu/api/files/container/download/Online_Szamla_interfesz%20specifikacio_HU_v3.0.pdf`
- Documentation hub — `https://onlineszamla.nav.gov.hu/dokumentaciok`
- Developer log — `https://onlineszamla.nav.gov.hu/fejlesztoi_naplo`
- Test web UI — `https://onlineszamla-test.nav.gov.hu/`
- Test API — `https://api-test.onlineszamla.nav.gov.hu/`
- Public schema repo — `https://github.com/nav-gov-hu/Online-Invoice`
- v3.0 changelog — `https://github.com/nav-gov-hu/Online-Invoice/blob/master/src/schemas/nav/gov/hu/OSA/CHANGELOG_3.0.md`
- v2.0 changelog (signature-algorithm history) — `https://github.com/nav-gov-hu/Online-Invoice/blob/master/src/schemas/nav/gov/hu/OSA/CHANGELOG_2.0.md`
- NAV English booklet 18, "Basic Rules of Issuing Invoices and Receipts" — `https://nav.gov.hu/pfile/file?path=%2Fen%2Ftaxation%2FBooklets%2F18---basic-rules-of-issuing-invoices-and-receipts`

**Legal basis**

- Act CXXVII of 2007 on VAT (Áfa törvény), English text — `https://www.icnl.org/research/library/hungary_vatact_eng/`
- European Commission Hungary VAT rules — `https://vat-one-stop-shop.ec.europa.eu/national-vat-rules/hungary-vat-rules_en`
- Hungarian VAT guide — `https://www.vatcalc.com/hungary/hungary-vat-country-guide/`

**Regulatory analysis 2025–2026**

- RSM "stricter validations Sept 2025" — `https://www.rsm.hu/blogs/tax/nav-online-invoice-system-stricter-validations-from-september-underscore-navs-commitment`
- Grant Thornton 2025 changes — `https://grantthornton.hu/en/news/nav-online-invoice-system-changes-2025`
- Grant Thornton NAV eInvoice glossary — `https://grantthornton.hu/en/glossary/nav-einvoice`
- Grant Thornton audit simulation — `https://grantthornton.hu/en/services/digital-services/nav-online-invoice-audit-simulation`
- VATupdate Sept 2025 — `https://www.vatupdate.com/2025/09/02/nav-online-invoice-system-2025-stricter-validation-rules-effective-september-15-impacting-businesses/`
- Accace Sept 2025 changes — `https://accace.com/hungarian-online-invoice-system-changes/`
- Marosa validation changes — `https://marosavat.com/vat-news/hungary-changes-to-real-time-reporting-validations`
- WTS Klient on failed reporting — `https://wtsklient.hu/en/2025/06/13/failed-data-reporting/`
- Dr. Ildikó Nagy 2026 compliance — `https://drnagyildiko.hu/en/blog/mandatory-e-invoicing-nav-compliance-hungary/`
- Sovos on mandatory e-invoicing plans — `https://sovos.com/regulatory-updates/vat/hungary-plans-to-introduce-mandatory-e-invoicing-and-real-time-reporting-revealed/`
- Banqup ViDA + RTIR — `https://www.banqup.com/resources/blog/vida-hungary-e-invoicing-rtir-guide`
- Comarch HU e-invoicing — `https://www.comarch.com/trade-and-services/data-management/e-invoicing/e-invoicing-in-hungary/`
- ddd Invoices HU NAV RTIR — `https://dddinvoices.com/learn/e-invoicing-hungary`
- European Commission eInvoicing in Hungary — `https://ec.europa.eu/digital-building-blocks/sites/spaces/DIGITAL/pages/467108888/eInvoicing+in+Hungary`

**Self-billing (deferred topic)**

- ado.hu önszámlázás III — `https://ado.hu/szamvitel/meghatalmazotti-szamlakibocsatas-onszamlazas-iii-resz/`
- ecovis.hu önszámlázás data reporting — `https://ecovis.hu/online-adatszolgaltatasi-kotelezettseg-onszamlazas-eseten`
- nav-gov-hu Issue #326 (selfBillingIndicator) — `https://github.com/nav-gov-hu/Online-Invoice/issues/326`

**Community NAV libraries**

- pzs/nav-online-invoice (PHP) — `https://github.com/pzs/nav-online-invoice`
- angro-kft/nav-connector (Node.js) — `https://github.com/angro-kft/nav-connector`
- Szotasz/nav-online-invoice-mcp — `https://github.com/Szotasz/nav-online-invoice-mcp`

**NAV technical-user setup**

- BerényiSoft how-to — `https://berenyisoft.com/en/how-to-create-a-technical-user-on-the-nav-online-invoice-interface/`
- tudastar.szamlazz.hu technical guide — `https://tudastar.szamlazz.hu/en/gyik/technical-guide-for-nav-online-invoicing-data-registration`

**Billingo**

- v3 reference page — `https://support.billingo.hu/content/446136358`
- API support hub — `https://support.billingo.hu/content/96207530`
- v3 FAQ — `https://support.billingo.hu/content/2092400787`
- yunehu/billingo-v3 SDK — `https://github.com/yunehu/billingo-v3`
- yunehu DocumentApi.md — `https://github.com/yunehu/billingo-v3/blob/master/docs/Api/DocumentApi.md`
- Alphaws PartnerApi.md — `https://github.com/Alphaws/billingo-api-v3/blob/master/docs/Api/PartnerApi.md`
- deviddev PHP SDK DocumentApi — `https://github.com/deviddev/billingo-api-v3-php-sdk/blob/main/docs/Api/DocumentApi.md`
- v2 (deprecated) docs for rate-limit precedent — `https://billingo.readthedocs.io/`
