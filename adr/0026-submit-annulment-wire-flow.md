# ADR-0026 — submit-annulment wire flow — manageAnnulment NAV operation, AnnulmentData runtime XSD validator, new wire-evidence EventKind pair (ADR-0009 §6 + ADR-0025 §4 extension)

- **Status:** Accepted
- **Date:** 2026-05-21
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR — extends ADR-0009 §6 with
  the wire half of the technical-annulment surface, pairing ADR-0025
  (the request half landed in PR-12). Structural parallel to ADR-0020
  §2 (NAV manageInvoice wire posture) and ADR-0022 (NAV InvoiceData
  XSD runtime validator); the load-bearing deltas are in §2 (new
  EventKind pair), §3 (manageAnnulment envelope shape), §4
  (`validate_annulment_data` allowlist + the F30 closure), and §6
  (precondition walker). Does **not** supersede ADR-0009 §6 or
  ADR-0025; both remain in force.
- **Related:**
  - **ADR-0009 §6** (storno + modification chain + technical
    annulment — the surface this ADR closes the wire half of).
  - **ADR-0009 §4** (NAV authentication — `manageAnnulment` uses the
    same exchangeToken + per-invoice-index signature extension as
    `manageInvoice`).
  - **ADR-0009 §5** (idempotency + retry classification — reused for
    the annulment wire path).
  - **ADR-0009 §8** (audit-evidence bundle — the new wire-evidence
    pair lands inside the `invoice.*` glob via the same prefix
    convention as every other lifecycle kind).
  - **ADR-0020 §1, §2** (NAV environment is explicit on the CLI;
    re-asserted for `submit-annulment` per the same posture as
    `submit-invoice`).
  - **ADR-0022** (NAV runtime XSD validator — this ADR extends
    `validate_invoice_data`'s pattern to `validate_annulment_data`,
    closing F30).
  - **ADR-0025** (technical-annulment request — PR-12 predicate; the
    boundary between PR-12 and this PR is the `--annulment-xml ...`
    file on disk).
  - Session 16 handoff F27 (default-series assumption in
    `request_technical_annulment.rs`'s precondition walker —
    explicitly NOT closed by this PR; see §6 below).
  - Session 16 handoff F28, F29 (NAV-testbed verification triggers
    for the `<InvoiceAnnulment>` root + the four-code ValueEnum) —
    both fire on this PR's first NAV-testbed POST.
- **Source material:** `docs/research/nav-and-billingo.md` §"Storno
  and modification" clause 3 + §4 (NAV auth + signature input
  extension).

## Context

ADR-0025 + PR-12 landed the **request** half of the technical-
annulment surface — a single audit entry
(`InvoiceTechnicalAnnulmentRequested`) plus the on-disk
`<InvoiceAnnulment>` XML, with no NAV call. The boundary between
PR-12 and this PR is the operator's next step:
`aberp submit-annulment --annulment-xml <path> --invoice-id <id>
--tax-number <num> --tenant <t> --endpoint {test|production}`.

ADR-0025 named the wire half explicitly as out of scope for PR-12
and deferred four named items to this PR's predicate ADR:

1. **The on-the-wire operation name + envelope shape.**
   ADR-0025 §1 + §2 pin the body shape (root `<InvoiceAnnulment>`,
   namespace `OSA/3.0/annul`) and the NAV operation (`manageAnnulment`,
   not `manageInvoice`). The envelope shape itself — the SOAP-style
   wrapper that carries the body plus the per-operator authentication
   block plus the per-invoice-index signature — is NOT pinned. NAV's
   v3.0 XSD for `<ManageAnnulmentRequest>` is not vendored in this
   repo; the conventional reading per
   `docs/research/nav-and-billingo.md` §"Storno and modification"
   clause 3 and the two consulted open-source clients
   (`pzs/php-nav-online-szamla`, `angro-kft/nav-online-szamla`)
   mirrors `<ManageInvoiceRequest>`'s shape one-for-one with the
   element renamed.

2. **The audit-evidence shape for the wire submission.** ADR-0025 §2
   left this open: "the annulment's future NAV-submission attempt +
   response will reuse the existing `InvoiceSubmissionAttempt` +
   `InvoiceSubmissionResponse` variants, or — if the audit-evidence
   bundle reader needs to distinguish 'submit of an invoice' from
   'submit of an annulment' — a future PR adds an
   `InvoiceAnnulmentSubmissionAttempt` + `…Response` pair (the
   trigger is named in the future `submit-annulment` PR, not this
   one)." This ADR makes the call (§2 below).

3. **The runtime XSD validator extension.** ADR-0025 §4 deferred
   `validate_annulment_data` with the named trigger "first PR that
   ships `submit-annulment`." That trigger fires here. This ADR §4
   names the allowlist + the F30 closure.

4. **The precondition walker for the wire side.** Symmetric to the
   request side's `check_base_is_annullable` but with a different
   admit/reject set — the wire submit must REJECT a base whose
   annulment request was never made, must REJECT a base whose
   annulment was already wire-submitted (default — accountant
   question, §"Surfaced conflict 3" below), and must ACCEPT
   re-submission after a prior wire failure (the retry case).
   ADR-0025 §6 named the request-side precondition; the wire-side
   precondition is named here.

This ADR closes those four pins. It does not introduce any decision
that conflicts with ADR-0009, ADR-0020, ADR-0022, or ADR-0025; it
extends each of them along the named seams those ADRs left.

### Surfaced conflicts (CLAUDE.md rule 7)

Three ambiguities the build phase will otherwise paper over:

1. **Whether to reuse `InvoiceSubmissionAttempt` /
   `InvoiceSubmissionResponse` or add new EventKind variants for the
   annulment wire submission.** ADR-0025 §2 explicitly left this
   open. Two competing readings:
   - Reuse: the payload **shape** is identical (verbatim request +
     response XML, NAV transaction id, endpoint label, idempotency
     key). The audit-evidence bundle reader can distinguish "submit
     of an invoice" from "submit of an annulment" by walking back
     from the wire entry to the operator-decision entry that shares
     the idempotency key (the annulment-request's
     `InvoiceTechnicalAnnulmentRequested` carries a different
     idempotency key from the base invoice's issuance entries, so
     no ambiguity).
   - New variants: the kind discriminator alone tells you which
     wire endpoint was hit — `manageInvoice` vs `manageAnnulment`.
     The audit-evidence bundle reader does not need to inspect the
     payload's XML root to classify. Future query-annulment-status
     polls would naturally land their own ack kind, and the new-
     variant precedent here sets that pattern correctly.
   This ADR commits to **new variants** (§2 below). Rationale: kind-
   alone classification is the load-bearing inspector-facing
   property — a NAV inspector reading the per-invoice export bundle
   (ADR-0009 §8) should see "ABERP issued → submitted → NAV SAVED →
   ABERP requested technical annulment → ABERP submitted the
   annulment to NAV → NAV responded with TXID-Q" as a sequence of
   distinct kinds, not as "submit, submit, submit" requiring
   payload XML inspection to disambiguate. The F12 four-edit
   ritual re-fires twice (once per new variant) — a known cost
   amortized over PR-10, PR-11, PR-12, and now PR-13.

2. **Whether `<ManageAnnulmentRequest>`'s body shape matches the
   conventional reading.** The research file
   (`docs/research/nav-and-billingo.md` §"Storno and modification"
   clause 3) does not name the envelope. NAV's v3.0 XSDs are not
   vendored. PR-13 commits to **the structural mirror of
   `<ManageInvoiceRequest>`** with `invoiceOperations` →
   `annulmentOperations`, `invoiceOperation` → `annulmentOperation`
   (both at the wrapper and at the per-item nested element), and
   `invoiceData` → `invoiceAnnulment`. The per-item operation value
   is the canonical `"ANNUL"` string (NAV's `ManageAnnulmentOperationType`
   has exactly one value per the consulted clients). The
   exchangeToken + common header + user + software wrapping is
   identical (NAV's envelope discipline carries across operations
   per ADR-0009 §4). If NAV's testbed rejects the envelope shape,
   the amendment is mechanical — renaming the
   `annulmentOperations` / `annulmentOperation` /
   `invoiceAnnulment` elements without changing the audit-payload
   contract this PR pins. **Named trigger for verification:** the
   first PR exercising `aberp submit-annulment` against NAV's
   `api-test` endpoint. This is the same posture ADR-0025 §
   "Surfaced conflict 1" used for the body root element name.

3. **Whether re-submission of a previously-submitted annulment is
   permitted.** Two cases the precondition walker on the wire side
   must handle:
   - **Prior wire submission FAILED** (manageAnnulment returned
     ERROR; the audit entry pair shows the attempt but the
     response was non-OK or the response is absent because the
     POST itself never returned). The operator may want to retry
     the wire submission against the same annulment-request audit
     entry. PR-13 default: **permit** — same posture as
     `retry-submission`'s permit-after-stuck design (ADR-0009 §5).
     The wire-submit precondition walker checks for the absence of
     a successful `InvoiceAnnulmentSubmissionResponse` against the
     same annulment-request idempotency key; presence of a prior
     `InvoiceAnnulmentSubmissionAttempt` without a successful
     `InvoiceAnnulmentSubmissionResponse` is the retry signal.
   - **Prior wire submission SUCCEEDED** (NAV returned OK with a
     transaction id). PR-13 default: **reject loudly** — a second
     successful submission of the same annulment-request to NAV
     creates duplicate NAV-side transaction ids referring to the
     same annulment decision, which would confuse the receiver-
     confirmation workflow per ADR-0009 §6. The operator who wants
     a fresh annulment-request must run `aberp request-technical-
     annulment` again (which is itself default-rejected per
     ADR-0025 §6's double-annulment posture).

   **Named trigger for change:** accountant review.

## Decision

### 1. Operator CLI surface for submit-annulment

**Subcommand name:** `aberp submit-annulment`.

**Rationale for the verb.** Matches the existing `submit-invoice`
posture (PR-7-B-3): both commands take pre-rendered XML on disk and
POST it to NAV. Both names also share the same arg-shape so an
operator reading `aberp --help` reads the verb-object family
consistently:

- `issue-*` family (issue-invoice, issue-storno, issue-modification)
  → produce a new on-disk XML body + audit entries; do not call NAV.
- `request-*` family (request-technical-annulment) → record an
  operator decision + an on-disk XML body; do not call NAV.
- `submit-*` family (submit-invoice, submit-annulment) → POST a
  pre-rendered XML body to NAV + write audit entries.
- `poll-*` family (poll-ack) → query NAV for terminal state.
- `retry-*` / `mark-*` → operator unblock decisions.

**Argument shape** (clap-flavoured):

| Flag | Type | Default | Purpose |
|---|---|---|---|
| `--annulment-xml` | `PathBuf` | none (required) | Path to the `<InvoiceAnnulment>` XML written by a prior `aberp request-technical-annulment --out ...`. The bytes on disk are the body POSTed to NAV (base64-encoded inside the SOAP envelope, same wrapping as `submit-invoice`). |
| `--invoice-id` | `String` (`inv_<ULID>`) | none (required) | Base invoice id of the annulment. Used to look up the prior `InvoiceTechnicalAnnulmentRequested` audit entry so the new wire-evidence entries share its idempotency key per the F8 contract. |
| `--tax-number` | `String` | none (required) | Hungarian tax number. Accepted forms: `12345678`, `12345678-1`, `12345678-1-42`. Same parser as `submit-invoice`. |
| `--db` | `PathBuf` | `./aberp.duckdb` | Tenant DuckDB. |
| `--tenant` | `String` | `"default"` | Tenant identifier — drives the audit-ledger genesis hash and the keychain service-name lookup. |
| `--endpoint` | `NavEnv` (clap ValueEnum) | none (required) | `test` or `production`. Explicit per ADR-0020 §1 — silently submitting an annulment to production when the operator meant test is exactly the failure mode CLAUDE.md rule 12 names. Same posture as `submit-invoice --endpoint`. |

**What `submit-annulment` does NOT do.**

- **Does NOT mutate the on-disk XML.** The bytes on disk produced by
  `request-technical-annulment` are the canonical record; the wire
  submission reads them verbatim.
- **Does NOT mint a new operator-decision idempotency key.** The F8
  contract requires the annulment-request's
  `InvoiceTechnicalAnnulmentRequested` idempotency key (stored in
  the payload's `idempotency_key` field) to flow into the new wire-
  evidence entries — `InvoiceAnnulmentSubmissionAttempt` and
  `InvoiceAnnulmentSubmissionResponse`. The operator does NOT pass
  the idempotency key on the CLI; it is looked up from the audit
  ledger.
- **Does NOT extend `submit_invoice.rs::detect_operation_from_xml`.**
  The annulment body never reaches `submit-invoice` — it goes to a
  separate orchestration module (`apps/aberp/src/submit_annulment.rs`)
  and a separate nav-transport operation
  (`crates/nav-transport/src/operations/manage_annulment.rs`). The
  three-way detector (Create / Modify / Storno) remains a three-way
  detector per ADR-0024 §3 / ADR-0025 §1.
- **Does NOT poll for annulment confirmation.** NAV's annulment
  fulfillment is asynchronous and requires the receiver to confirm
  in the NAV web UI per ADR-0009 §6. The future
  `query-annulment-status` poll (whenever it lands) is the
  observation path; `submit-annulment` only POSTs the request and
  records the wire-evidence pair.

### 2. New EventKind variants + payload structs (decides ADR-0025 §"Surfaced conflict" carry-forward)

**Two new variants:**

- `EventKind::InvoiceAnnulmentSubmissionAttempt` (storage form
  `"invoice.annulment_submission_attempt"`).
- `EventKind::InvoiceAnnulmentSubmissionResponse` (storage form
  `"invoice.annulment_submission_response"`).

Both carry the `"invoice."` prefix so the per-invoice export bundle
(ADR-0009 §8) `invoice.*` glob picks them up alongside every other
lifecycle kind. The F12 four-edit ritual re-fires for each variant
in this PR — variant + `as_str` arm + `from_storage_str` arm +
extended `round_trip_for_every_variant` test's variant list + two
new prefix-pinning tests
(`pr_13_annulment_submission_attempt_kind_uses_invoice_prefix` +
`pr_13_annulment_submission_response_kind_uses_invoice_prefix`). The
ritual has now closed cleanly across PR-6.1, PR-7-B-3, PR-8, PR-10,
PR-11, and PR-12; PR-13 is the seventh exercise and the cost is
mechanical at this point.

**Two new typed payload struct types:**

- `audit_payloads::InvoiceAnnulmentSubmissionAttemptPayload` —
  fields: `invoice_id` (the BASE invoice id, NOT a new id), the
  annulment-request's `idempotency_key`, `endpoint` (`"test"` or
  `"production"`), and `request_xml` (verbatim
  `<ManageAnnulmentRequest>` bytes).
- `audit_payloads::InvoiceAnnulmentSubmissionResponsePayload` —
  fields: `invoice_id` (the BASE invoice id), the annulment-
  request's `idempotency_key`, `transaction_id` (NAV's annulment
  transaction id), and `response_xml` (verbatim
  `<ManageAnnulmentResponse>` bytes).

**Why two distinct payload types instead of reusing
`InvoiceSubmissionAttemptPayload` / `InvoiceSubmissionResponsePayload`.**
Structurally the fields are identical. Forking the types is
deliberate so the type system enforces the kind ⇄ payload binding
even when the discriminator is correct — a future audit-evidence
bundle reader that deserializes `EventKind::InvoiceAnnulmentSubmissionAttempt`
into an `InvoiceSubmissionAttemptPayload` would not fail at runtime
(the JSON shape is the same), but the resulting code would lose the
semantic distinction that the annulment wire path has its own
audit trail. Forking the types makes the type signature carry the
distinction. Same posture as `InvoiceStornoIssuedPayload` vs
`InvoiceModificationIssuedPayload` — structurally similar, forked
deliberately so a future maintainer cannot accidentally pass one
where the other is meant.

**Why NOT a third "annulment ack status" variant in this PR.** The
NAV `manageAnnulment` response itself does NOT carry an
`<invoiceStatus>` — the receiver-confirmation workflow at NAV's
side is observed by a future `query-annulment-status` poll (not in
scope here). The wire-evidence pair this PR adds is sufficient to
record the submission attempt + response; the ack-status kind for
annulments is the next named extension (its own future ADR if and
when that observation path lands). Same posture as ADR-0024 §2's
"no second variant added" — single contributing surface only.

### 3. NAV manageAnnulment operation + SOAP envelope

**New nav-transport module:**
`crates/nav-transport/src/operations/manage_annulment.rs`.

**Public surface:** `pub async fn call(transport, credentials,
tax_number_8, exchange_token, items) -> Result<ManageAnnulmentOutcome,
NavTransportError>` where `items: &[ManageAnnulmentItem<'_>]` with
`ManageAnnulmentItem { invoice_annulment_xml: &'a [u8] }`. Note the
absence of an `operation` field per-item: NAV's annulment operation
type has exactly one value (`ANNUL`), so the operation is implicit
in the envelope; carrying a redundant per-item operation field would
be the speculative abstraction CLAUDE.md rule 2 names.

**Return shape:** `ManageAnnulmentOutcome { transaction_id: String,
request_xml: Vec<u8>, response_xml: Vec<u8> }` — same field shape as
`ManageInvoiceOutcome` so the binary's audit-write code path mirrors
`submit_invoice`'s. The verbatim-bytes-before-parse posture per
ADR-0009 §8 is reused.

**Error variants** in `nav_transport::error::NavTransportError` —
five new variants in the same shape as the `manageInvoice` group:
- `ManageAnnulmentHttp(#[source] reqwest::Error)` — transport-layer
  failure.
- `ManageAnnulmentHttpStatus { status: u16 }` — non-success HTTP.
- `ManageAnnulmentResponseParse(String)` — body parse failure.
- `ManageAnnulmentNonRetryable { code: String, message: String }` —
  ADR-0009 §5 non-retryable bucket.
- `ManageAnnulmentRetryable { code: String, message: String }` —
  ADR-0009 §5 retryable bucket.

Mapping reuses `is_non_retryable` from `operations/mod.rs` —
ADR-0009 §5's retry-classification set is operation-agnostic. No
new error codes from NAV are conjectured here; if the testbed
returns an annulment-specific code (e.g.,
`UNSUPPORTED_ANNULMENT_CODE` per ADR-0025 §"Surfaced conflict 2"),
the future amendment is a one-line addition to `is_non_retryable`'s
match list.

**New SOAP renderer:**
`crates/nav-transport/src/soap/render_manage_annulment_request` —
structural mirror of `render_manage_invoice_request` with three
element-name renames (§"Surfaced conflict 2"):
- Root: `<ManageAnnulmentRequest>` (replaces
  `<ManageInvoiceRequest>`).
- Per-batch wrapper: `<annulmentOperations>` (replaces
  `<invoiceOperations>`).
- Per-item wrapper: `<annulmentOperation>` (replaces
  `<invoiceOperation>` — both the outer wrapper element AND the
  inner value element keep this name in NAV's convention, which
  matches the manageInvoice precedent).
- Per-item payload: `<invoiceAnnulment>` (replaces `<invoiceData>`).

The per-item operation string is `"ANNUL"` always (one-value enum).
The per-invoice-index signature input uses
`"ANNUL" || base64(invoice_annulment_xml)` — same shape NAV's spec
names for `manageInvoice` (per the consulted clients), with the
operation string substituted.

**`InvoiceOperation` enum NOT extended.** The existing
`InvoiceOperation::{Create, Modify, Storno}` enum stays three-way.
Annulment is NOT a manageInvoice operation; it goes through a
different endpoint with a separate operation enum that has exactly
one value. Adding `Annul` to `InvoiceOperation` would either (a)
require an extra `unreachable!` arm in `detect_operation_from_xml`
(annulment bodies never reach that path) or (b) widen the detector
in a way that makes the three-way classification fuzzy. Both options
violate CLAUDE.md rule 2. The annulment operation is implicit at the
envelope level — no enum is created on the Rust side; the literal
string `"ANNUL"` is the wire form.

**URL:** `{endpoint_base_url}/manageAnnulment` — same base-URL +
operation-path pattern as `submit-invoice` and `poll-ack`.

### 4. AnnulmentData runtime XSD validator (closes F30)

**Extension in `crates/nav-xsd-validator/src/validate.rs`.** New
public function:

```rust
pub fn validate_annulment_data(xml: &[u8]) -> Result<(), NavXsdValidationError>
```

Same shape as `validate_invoice_data`. Walks the in-memory bytes
against the v3.0 `<InvoiceAnnulment>` allowlist; on any divergence
returns the same typed `NavXsdValidationError` variants. No new
error variants are introduced — the existing eight cover the failure
classes (UnexpectedRoot, UnexpectedRootNamespace, MissingRequiredChild,
UnexpectedElement, MalformedDate, NonNumericAmount, MalformedXml,
ChildOrderViolation). The `NoInvoiceLines` variant does not apply
(annulments have no `<line>` children).

**Allowlist** — exhaustive for the v3.0 annulment body shape per
ADR-0025 §4:

- Root: `<InvoiceAnnulment>` in namespace
  `http://schemas.nav.gov.hu/OSA/3.0/annul`.
- Required children, in document order:
  1. `<annulmentReference>` — text content; the base invoice's NAV-
     facing number.
  2. `<annulmentTimestamp>` — text content; ISO 8601 UTC
     (`YYYY-MM-DDTHH:MM:SSZ`) per ADR-0025 §4. NAV-compressed
     `YYYYMMDDhhmmss` is a deferred amendment (ADR-0025 §"Open
     questions"; named trigger fires on first NAV-testbed POST).
  3. `<annulmentCode>` — text content; one of the four NAV codes
     per ADR-0025 §"Surfaced conflict 2".
  4. `<annulmentReason>` — text content; free-form operator-supplied
     reason.

The walker does NOT enforce the four-code closed-set on
`<annulmentCode>` (the CLI-side clap-ValueEnum is the loud-fail
boundary for unknown codes per ADR-0025 §3; the validator's job is
schema-shape conformance, not enumeration validity). The walker
does NOT date-shape-check `<annulmentTimestamp>` against ISO 8601
(NAV's v3.0 XSD declares it as `xs:dateTime` rather than `xs:date`;
the request-side emitter pins the shape via `OffsetDateTime::now_utc()`
formatting per `nav_xml::render_annulment_data`, and the validator
accepts whatever well-formed text the operator's hand-edit could
produce — same posture as `<lineDescription>` text content).

**Wiring** in `apps/aberp/src/submit_annulment.rs`: same call shape
as `submit_invoice.rs` step 3a — after reading the bytes from disk
and before any NAV call. Loud-fail per CLAUDE.md rule 12; no
tokenExchange happens if validation fails.

**F30 closure** — this PR's load-bearing F30 closure. The trigger
named in ADR-0025 §4 ("the first PR that lands `submit-annulment`
MUST also ship `validate_annulment_data`") fires here and is closed
by this ADR + the code in `nav-xsd-validator/src/validate.rs`.

**ALLOWLIST SOURCE OF TRUTH discipline preserved.** The validator's
allowlist is the runtime-enforced source of truth for "what
`request_technical_annulment::render_annulment_data` is allowed to
emit." A second source of truth is the emitter itself
(`apps/aberp/src/nav_xml.rs::render_annulment_data`). Divergence
between the two is the same failure mode CLAUDE.md rule 7 names.
Two trap-doors close the divergence (mirror of ADR-0022 §"Trap-doors
against drift"):

- A new integration test
  `apps/aberp/tests/nav_xsd_validator_annulment_round_trip.rs`
  renders a fresh annulment via the emitter and validates the bytes
  with `validate_annulment_data`. A future emitter change that
  diverges from the allowlist fails this test loud at commit time.
- The existing `error_variants_have_distinct_display` test in the
  validator's `mod tests` keeps the variant-pairwise-distinct
  invariant across the annulment extension.

### 5. Idempotency + retry posture for the wire path

**Layer 1 (client-side ULID).** The new wire-evidence audit entries
share the annulment-request's `idempotency_key` (looked up from the
ledger via the most-recent `InvoiceTechnicalAnnulmentRequested`
against this `invoice_id`). Re-running `submit-annulment` against
the same on-disk XML with the same `--invoice-id` does NOT mint a
fresh idempotency key; the wire submission is retried against the
same operator-decision lineage.

**Layer 2 (NAV-side disambiguation).** Inherits the same posture
ADR-0009 §5 names for `manageInvoice`: NAV's `requestId` +
`requestTimestamp` are minted fresh per attempt (per
`crate::soap::parts::new_request_id`); NAV's server-side dedup
window applies. If NAV returns `INVOICE_NUMBER_NOT_UNIQUE`-shaped
errors against the annulment wire path (NAV's annulment endpoint is
not currently known to surface this code, but the ADR-0009 §5
classification set is operation-agnostic), the loud-fail surface
in `manage_annulment.rs` lands the operator on the same Layer-2
gap ADR-0009 §5 surfaces for `manageInvoice` — out of scope for
PR-13, named trigger is the same (`queryAnnulmentCheck` or
equivalent, not yet implemented).

**Retry classification reuse.** `is_non_retryable` in
`operations/mod.rs` is shared across operations — manageAnnulment's
classification uses the same code list (`INVALID_SECURITY_USER`,
`INVALID_REQUEST_SIGNATURE`, `INCORRECT_REQUEST_SCHEMA`,
`SCHEMA_VIOLATION`, `INVOICE_NUMBER_NOT_UNIQUE`). The PR-13 path
does NOT implement automatic retries; the operator runs
`submit-annulment` again manually if the wire-side failure is
transient. A future `retry-annulment` command (parallel to
`retry-submission`) is the named trigger if the operational pattern
calls for it.

### 6. Precondition walker for submit-annulment

PR-13's `submit_annulment::run` precondition walker accepts:

- The base invoice has at least one prior
  `InvoiceTechnicalAnnulmentRequested` audit entry (i.e., the
  operator's annulment-request decision was actually recorded). This
  is the required ancestor — wire-submitting an annulment without a
  request audit entry is malformed.

And **loud-rejects**:

- No prior `InvoiceTechnicalAnnulmentRequested` audit entry against
  the base. The operator must run `request-technical-annulment`
  first; the named error message in the walker explicitly steers
  the operator there (CLAUDE.md rule 12).
- A prior `InvoiceAnnulmentSubmissionResponse` exists against the
  same annulment-request idempotency key. Per §"Surfaced conflict
  3", this is the "already-successful wire submission" case;
  default-reject loudly.

**Does NOT reject:**

- A prior `InvoiceAnnulmentSubmissionAttempt` without a successful
  `InvoiceAnnulmentSubmissionResponse`. This is the retry-after-
  failed-wire case (§"Surfaced conflict 3"); default-permit. The
  operator may retry the wire submission against the same
  annulment-request.

**F8 contract.** The new wire-evidence entries carry the annulment-
request's idempotency key, NOT the base invoice's issuance
idempotency key. Rationale: the annulment is a distinct operator
decision (per ADR-0025 §3 — the annulment-request mints its own
operator-decision idempotency key). The audit-evidence bundle
reader walks back from the wire entry to the request entry via
shared idempotency key; the request entry's `invoice_id` field is
the base, so the chain is reconstructable.

**F27 (default-series assumption) — NOT closed by this PR.** The
session 16 handoff suggested closing F27 in PR-13 on the assumption
that the wire submission would naturally load the base invoice's
billing row. It does not: `submit-annulment` reads the annulment
XML from disk (already containing the `base_invoice_number` baked
in by `request_technical_annulment::run`), looks up the annulment-
request audit entry by `invoice_id`, and POSTs. The base billing
row is not on the transactional path. Closing F27 still requires
editing `request_technical_annulment.rs` to load the billing row +
read the series code at request-side; PR-13 is the wire half and
that edit is out of scope. The trigger remains as named in the
session 16 handoff: first non-default-series annulment request
(which would surface as a NAV "reference does not match a known
invoice" error at submit-annulment time per CLAUDE.md rule 12 —
loud-fail at NAV, not silently misrouted).

### 7. Idempotency-key lookup discipline

The submit-annulment orchestrator must resolve the annulment-
request's idempotency key from the audit ledger. The lookup is:

1. Open `Ledger` read-only.
2. Walk `entries()` filtered to `EventKind::InvoiceTechnicalAnnulmentRequested`.
3. Filter to payloads whose `invoice_id` matches `--invoice-id`.
4. Pick the LATEST (highest seq) — handles the future case where
   the accountant resolves to permit double-annulment per ADR-0025
   §"Surfaced conflict 3" (then there could be multiple entries;
   the latest is the operationally-current one).
5. Read its `idempotency_key` field.

If no matching entry is found: loud-fail with a message that
explicitly steers the operator to run
`aberp request-technical-annulment` first (CLAUDE.md rule 12).

The lookup is in the same scope as the precondition walker — both
read from the audit ledger and the walker can return the resolved
idempotency key alongside its precondition verdict, in one walk.

## Open questions

Tracked against the next fortnightly adversarial review and the
named external-check items in `docs/research/nav-and-billingo.md`:

- **`<ManageAnnulmentRequest>` envelope element names.** Default
  reading per §"Surfaced conflict 2". Verification trigger fires on
  first NAV-testbed annulment POST.
- **The `"ANNUL"` operation literal.** Default per §3.
  Verification deferred to first NAV-testbed POST. NAV reporting
  `INCORRECT_REQUEST_SCHEMA` against a malformed operation field is
  the one-line fix surface (change the literal); the audit-payload
  shape this ADR pins does not depend on the operation field's
  string spelling.
- **`<annulmentTimestamp>` precise format.** ADR-0025 §4 commits to
  ISO 8601 / `xs:dateTime`. NAV may require the compressed
  `YYYYMMDDhhmmss` form; PR-13 ships the ISO 8601 form, with the
  named-trigger fix in `nav_xml::render_annulment_data`.
- **NAV annulment-specific error codes** — none are conjectured
  here. If the testbed surfaces a code not in
  `is_non_retryable`'s allowlist, the amendment is a one-line
  addition. ADR-0025 §"Surfaced conflict 2" already names the
  ValueEnum-side amendment trigger for the annulment-code closed
  set.
- **Annulment-receiver-confirmation observation PR.** NAV requires
  the receiver to confirm the annulment in the NAV web UI per
  ADR-0009 §6. ABERP cannot drive that step; the future
  `query-annulment-status` poll (whenever that lands) is the
  observation path. PR-13 does NOT ship the poll. Trigger: first
  operator request for "is my annulment confirmed yet?".

## Consequences

**What gets easier**

- The ADR-0009 §6 design surface is now **fully built** on the wire
  side: an operator can issue end-to-end via `aberp issue-invoice →
  submit-invoice → poll-ack`, cancel via `aberp issue-storno →
  submit-invoice → poll-ack`, correct via `aberp issue-modification
  → submit-invoice → poll-ack`, AND annul a prior data submission
  via `aberp request-technical-annulment → submit-annulment`. The
  annulment-receiver-confirmation observation is the only remaining
  ADR-0009 §6 surface (future PR; named trigger above).
- The audit-evidence bundle (ADR-0009 §8) gains the annulment wire
  leg with no schema changes to the per-invoice walker: the
  `invoice.*` glob picks up `invoice.annulment_submission_attempt`
  + `invoice.annulment_submission_response` alongside every other
  lifecycle kind. A NAV inspector can now reconstruct the full
  withdrawal-of-data-submission trail from the per-invoice export
  bundle — the affirmative answer to ADR-0025 §"Working-agreement
  reminders for session 17"'s "would a NAV inspector accept this?"
  bar.
- The `nav-xsd-validator` crate now covers both the `<InvoiceData>`
  body (ADR-0022) and the `<InvoiceAnnulment>` body (this ADR).
  Future v3.x or v4 migrations bump both allowlists in the same PR;
  the constant `NAV_XSD_VERSION = "3.0"` covers both.
- The F12 four-edit ritual has now landed for seven distinct
  variant additions (PR-6.1 ack + PR-7-B-3 attempt/response + PR-8
  retry/abandon + PR-10 storno + PR-11 modify + PR-12 annulment-
  request + PR-13 annulment-wire-attempt/response). The ritual is
  doing exactly what F12 named.

**What gets harder**

- The CLI surface now has eleven subcommands (issue-invoice,
  submit-invoice, setup-nav-credentials, poll-ack,
  retry-submission, mark-abandoned, serve, issue-storno,
  issue-modification, request-technical-annulment, submit-annulment).
  ADR-0025 §"Consequences" already flagged the surface-area concern;
  the command-group split (e.g., `aberp invoice issue` /
  `aberp invoice annul`) remains the named future direction if
  operator feedback shows the flat list is unwieldy. Out of scope
  for PR-13.
- The audit-payload schema versioning rule applies to two new
  payload types. Adding a field is forward-compatible; removing or
  renaming requires a new `EventKind` variant. The wire-evidence
  pair this PR pins is now part of the closed set.
- `nav_transport::error::NavTransportError` grows by five variants.
  The variant grouping discipline (the `// ── N. Group ──` section
  comments in `error.rs`) absorbs them cleanly as group #8
  (manageAnnulment operation). Future variant additions for new
  operations land in their own numbered group.
- The `is_non_retryable` allowlist now serves three operations
  (manageInvoice, queryTransactionStatus, manageAnnulment). If a
  future operation needs a different retry-classification set, the
  shared function would need to grow per-operation overrides or
  split into per-operation lookups. Out of scope here; named for
  the trigger.

**What we lock ourselves into**

- Subcommand name `aberp submit-annulment` and arg names
  (`--annulment-xml`, `--invoice-id`, `--tax-number`, `--db`,
  `--tenant`, `--endpoint`). Rename requires an amendment ADR.
- The two EventKind variants + storage strings. The `invoice.`
  prefix is load-bearing for ADR-0009 §8's glob; renaming is an
  amendment ADR.
- The two payload struct types + their field shapes. Schema
  evolution rules apply (additive forward-compat; renames require
  new variants).
- The `ManageAnnulmentOutcome` shape (mirrors
  `ManageInvoiceOutcome`). Adding a field is forward-compatible
  for callers that destructure named-field; removing breaks
  callers.
- The `"ANNUL"` operation-string literal + the
  `<ManageAnnulmentRequest>` / `<annulmentOperations>` /
  `<annulmentOperation>` / `<invoiceAnnulment>` element-name
  reading. NAV-testbed verification is the named-trigger amendment
  surface.
- The decision to NOT add a third annulment-ack-status variant in
  this PR. The future query-annulment-status PR (whenever it lands)
  is the named trigger.
- The decision to NOT close F27 here. The trigger remains "first
  non-default-series annulment request" (session 16 handoff).

## Adversarial review

A hostile NAV inspector + a hostile-engineer review, alternating.
ADR-README bar is three; four surfaced because the technical-
annulment wire surface is operator-action-required at NAV's side
and the audit trail must be unambiguous for an inspector AND must
survive a re-submission-after-failed-wire retry without
double-recording the annulment decision.

1. **"You commit to the conventional `<ManageAnnulmentRequest>`
   envelope shape without a NAV-testbed verification. If NAV's
   actual XSD uses different element names — e.g.,
   `<annulmentSubmissions>` instead of `<annulmentOperations>` —
   every test environment will reject every annulment ABERP
   produces. PR-13 ships dead code on the wire path."** The risk is
   real; same shape ADR-0025 §"Surfaced conflict 1" + ADR-0024 §1
   conflict 1 accepted. The mitigation has three legs:
   - The envelope-construction code is one function
     (`render_manage_annulment_request`) with element names
     centralized as string literals; a NAV-testbed rejection at
     first POST is a mechanical one-PR fix.
   - The audit-payload's `request_xml` field carries the verbatim
     bytes, so even a wire-rejected attempt is recorded —
     re-rendering with a corrected envelope after the fix is
     possible without re-running the operator-decision path.
   - The wire-evidence pair's `transaction_id` field on the response
     entry is the load-bearing inspector-facing field — it is NAV-
     assigned, not ABERP-derived. A NAV inspector verifying the
     audit trail walks the request → wire-attempt → wire-response
     → transaction-id chain; the chain holds regardless of which
     envelope shape NAV finally accepts.
   **Accepted with trigger named.**

2. **"Your wire-precondition walker rejects a second SUCCESSFUL wire
   submission against the same annulment-request idempotency key,
   but accepts a re-submission after a FAILED wire attempt. A NAV
   inspector reading the audit trail sees two
   `InvoiceAnnulmentSubmissionAttempt` entries against the same
   key with one preceding failure and one succeeding. The inspector
   asks: 'Did ABERP retry against my potentially-already-processed
   request?' The answer is 'yes, intentionally,' but the trail must
   make that intent visible — otherwise an inspector sees what
   looks like double-submission and asks for receipts."** Accepted,
   surfaced. The wire-precondition walker's failure modes that
   trigger the retry path are named (no response received, or
   non-OK response). The audit-evidence-bundle reader can
   distinguish the two cases (intentional retry after failure vs
   unintentional duplicate) by inspecting the
   `InvoiceAnnulmentSubmissionResponse` entries between the two
   attempts: if the first attempt's response is absent or non-OK,
   the retry is intentional; if the first attempt's response is OK,
   the second attempt would never have been written (the walker
   rejected it). The retry case is the same shape `retry-submission`
   uses for manageInvoice per ADR-0009 §5; the trail is reading-
   compatible across both operations.

3. **"You add two new EventKind variants instead of reusing the
   existing `InvoiceSubmissionAttempt` / `InvoiceSubmissionResponse`.
   You have committed the audit-ledger schema to a new pair. If NAV
   ever consolidates `manageInvoice` and `manageAnnulment` into one
   endpoint, you have two distinct entry kinds for what becomes one
   operation, and the per-invoice export bundle has to walk both."**
   The risk is real but priced in. NAV's v3.0 split between
   `manageInvoice` and `manageAnnulment` is a long-standing API
   decision; the two endpoints handle different operations
   (`<InvoiceData>` vs `<InvoiceAnnulment>` bodies, with different
   receiver semantics — invoice submissions are NAV-side
   one-sided; annulment requests require receiver confirmation).
   NAV consolidating the two into one endpoint would be a v4-level
   change that would also require re-pinning ABERP's XSD allowlist,
   the operation-detection logic, and the `submit-*` commands; the
   two-variant audit kind would be the smallest of those concerns.
   The cost of the new-variant decision today is the F12 ritual cost
   (mechanical at this point); the value is kind-alone
   classification in the audit-evidence bundle. **Accepted —
   forward-compatible decision against current NAV semantics.**

4. **"`validate_annulment_data` shares the
   `NavXsdValidationError` enum with `validate_invoice_data`. A
   future emitter regression that, e.g., emits `<annulmentReference>`
   inside `<InvoiceData>` (or vice versa) would fire `UnexpectedElement`
   with a confusingly-named `parent` field. The error type is too
   loose."** Partial concern; the error type already includes
   `parent` and `element` as String, and the call site at the
   submit-annulment orchestrator carries the file path + operation
   context per `with_context` chaining. The `UnexpectedElement`
   diagnostic is interpretable across both validators because the
   `parent` field carries the actual XSD parent name (e.g.,
   `"InvoiceAnnulment"` or `"InvoiceData"`), not the validator-
   function name. A future refactor splitting the enum per body
   shape (e.g., `NavXsdValidationError::Invoice(...)` /
   `NavXsdValidationError::Annulment(...)`) would be the named
   trigger only if a real triage incident traces back to this
   ambiguity. **Accepted with the surface intentionally shared;
   refactor trigger named.**

## Alternatives considered

- **Reuse `InvoiceSubmissionAttempt` / `InvoiceSubmissionResponse`.**
  Rejected per §2 + §"Surfaced conflict 1". The fork is deliberate
  for kind-alone classification.

- **Single `submit-annulment` step that also runs
  `request-technical-annulment` if no prior request entry exists.**
  Rejected. Same posture as `submit-invoice` / `issue-invoice` —
  the request half and the wire half are two operator decisions
  separated by the on-disk XML boundary. Bundling them would couple
  the operator-decision audit entry's timing to the wire-call's
  success/failure, exactly the failure mode ADR-0025 §"Adversarial
  review #2" named for the request-side single-entry shape.

- **Default `--endpoint` to `Test` (lower blast radius).** Rejected
  per ADR-0020 §1 — explicit per-CLI value, no hidden default. Same
  posture every other `submit-*` / `poll-*` / `retry-*` command
  uses; consistency across the family is load-bearing for operator
  pattern-matching.

- **Bundle the runtime XSD validator extension into a separate
  follow-on PR.** Rejected. ADR-0025 §4 explicitly named "the first
  PR that lands `submit-annulment` MUST also ship
  `validate_annulment_data`" — splitting the validator extension
  off would leave the wire path enforcing a weaker invariant than
  the request path's call-site sanity check, which is exactly
  backward (the wire is where loud-fail matters most per ADR-0022).

- **Add a third "annulment ack status" EventKind in this PR to
  prepare for the future `query-annulment-status` poll.** Rejected
  per §2 — out of scope; the variant lands in its own PR with its
  own ADR when the poll surface is built. CLAUDE.md rule 2 (no
  speculative abstractions).

- **Close F27 by loading the base invoice's billing row in the
  submit-annulment orchestrator.** Rejected per §6. The wire path
  does not need the base row; loading it just to read the series
  code would be a speculative dependency. The correct closure site
  is the request side (`request_technical_annulment.rs`); F27's
  trigger remains as named in the session 16 handoff.

- **Add automatic retry on `ManageAnnulmentRetryable` errors.**
  Rejected for PR-13. Same posture as PR-7-B-3 for `manageInvoice`
  — the retry loop with exponential backoff is a separate concern;
  the operator runs `submit-annulment` again on transient failure.
  A future `retry-annulment` command (parallel to
  `retry-submission`) is the named trigger if the operational
  pattern calls for it.

## Follow-on PRs unblocked by this decision

- **PR-13 — submit-annulment wire flow (code).** Implements §1-§7
  above plus:
  - `apps/aberp/src/submit_annulment.rs` (orchestration).
  - `crates/nav-transport/src/operations/manage_annulment.rs` (NAV
    operation).
  - `crates/nav-transport/src/soap/mod.rs::render_manage_annulment_request`
    (envelope renderer).
  - `crates/nav-xsd-validator/src/validate.rs::validate_annulment_data`
    (closes F30).
  - Two new `EventKind` variants + two new payload struct types +
    F12 four-edit ritual landings.
  - Five new `NavTransportError` variants.
- **First NAV-testbed annulment POST.** Verifies §3 (envelope
  shape), §"Surfaced conflict 2" (operation literal), ADR-0025
  §"Surfaced conflict 1" (root element name), ADR-0025 §"Surfaced
  conflict 2" (four-code set), ADR-0025 §"Open questions"
  (timestamp format).
- **`query-annulment-status` PR.** Future polling PR that observes
  NAV-side receiver confirmation. Adds a new operation in
  `crates/nav-transport/src/operations/query_annulment_status.rs`
  and (likely) a new ack-status EventKind for annulments. Trigger:
  first operator request for confirmation-status visibility.
- **Per-invoice export bundle PR (gated on F5 + F10).** Consumes
  the two new wire-evidence kinds via the same `invoice.*` glob.
- **F27 closure PR (request-side).** Loads the base invoice's
  billing row in `request_technical_annulment::check_base_is_annullable`
  to read the actual series code instead of the `INV-default`
  fallback. Trigger: first non-default-series annulment request
  per session 16 handoff.
