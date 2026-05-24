# ADR-0025 — Technical annulment — operator surface, no chain interaction, audit-payload pin, AnnulmentData emitter (ADR-0009 §6 amendment)

- **Status:** Accepted
- **Date:** 2026-05-21
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR — extends ADR-0009 §6 with
  the concrete pins PR-12 needs in order to land the **technical
  annulment** surface without re-litigating naming, audit-payload
  shape, or the boundary between this PR and the future
  `submit-annulment` PR (the NAV wire call). Structural parallel to
  ADR-0023 (storno) and ADR-0024 (modify); the key deltas are surfaced
  in §1, §3, and §6. Does **not** supersede ADR-0009; the §6 decisions
  there (technical annulment is distinct from storno and modify; uses
  a different NAV endpoint; requires receiver confirmation in the NAV
  web UI; used only for true submission-side errors; distinct command
  type `RequestTechnicalAnnulment`) remain in force.
- **Related:**
  - **ADR-0009 §6** (storno + modification chain + technical annulment
    — the surface this ADR pins for build, technical-annulment third
    leg).
  - **ADR-0009 §2** (invoice state machine — `InvoiceTechnicalAnnulmentRequested`
    already named as one of the audit-ledger typed kinds; no derived
    typestate is changed by an annulment — the base invoice's
    `Finalized` / `Rejected` / `Stuck` / `Abandoned` state does NOT
    transition under an annulment request, see §2 below).
  - **ADR-0023** (storno chain amendment — the structural template
    this ADR mirrors).
  - **ADR-0024** (modification chain amendment — the second template
    this ADR mirrors; the F12 four-edit ritual carries forward
    unchanged).
  - **ADR-0008** (audit ledger — typed `EventKind`, the F12 closed-
    set decoder, the per-payload typed struct discipline).
  - **ADR-0019** (no foreign keys — the annulment payload's link to
    the base invoice is ULID-by-payload).
  - **ADR-0020 §1, §2** (NAV environment is explicit on the CLI;
    re-asserted for the future `submit-annulment` command, NOT for
    this PR's `request-technical-annulment` which does not call NAV).
  - **ADR-0022** (NAV runtime XSD validator — InvoiceData only;
    AnnulmentData is a separate schema and a separate validator-
    extension trigger named in §4).
  - Session 15 handoff: "Technical annulment does NOT exercise the
    chain allocator. The detector in `submit_invoice.rs` is NOT
    extended — technical annulment goes through a separate NAV
    endpoint (`manageAnnulment`, not `manageInvoice`). A new
    operation lands in `crates/nav-transport/src/operations/`."
- **Source material:** `docs/research/nav-and-billingo.md` §"Storno
  and modification" clause 3.

## Context

ADR-0023 pinned the STORNO half of ADR-0009 §6 build-ready. ADR-0024
pinned the MODIFY half. PR-10 + PR-11 landed the corresponding code.
Both operations share the `manageInvoice` shape and exercise the
chain allocator (`<invoiceReference>` + `<modificationIndex>`). The
**third and final leg** of ADR-0009 §6 is technical annulment — a
structurally distinct surface that ADR-0009 §6 already flags as
**not** a chain operation. Session 15's handoff names PR-12 as the
technical-annulment code surface and flags three pins as the work
session 16 must close before PR-12 can land without re-litigation:

1. **Operator command shape.** ADR-0009 §6 names the command type
   `RequestTechnicalAnnulment` but does not name the `aberp`
   subcommand, its argument vocabulary, or its preconditions.
2. **EventKind variant + typed payload struct.** ADR-0009 §2 already
   names `InvoiceTechnicalAnnulmentRequested` (storage form
   `invoice.technical_annulment_requested`). What is **not** named:
   the matching Rust payload struct in `apps/aberp/src/audit_payloads.rs`,
   its field shape, or the F12 four-edit ritual call-out so PR-12
   follows the discipline closed in PR-6.1 / PR-7-B-3 / PR-8 / PR-10 /
   PR-11 without re-discovering it.
3. **Wire-submission scope.** PR-12 is the **request** PR (the
   operator-decision audit entry + the AnnulmentData XML on disk).
   The NAV `manageAnnulment` POST belongs to a follow-on
   `submit-annulment` PR; the boundary between PR-12 and that
   follow-on needs to be pinned so neither overshoots.

This ADR closes those three pins. It does not introduce any decision
that conflicts with ADR-0009 §6; it makes §6's technical-annulment
paragraph build-ready.

### Surfaced conflicts (CLAUDE.md rule 7)

Three ambiguities the build phase will otherwise paper over:

1. **Whether the annulment XML body's root element name is
   `<InvoiceAnnulment>` or `<AnnulmentData>`** (paralleling
   `<InvoiceData>`). The research file
   (`docs/research/nav-and-billingo.md` §"Storno and modification"
   clause 3) does not name the element. NAV's v3.0 schemas are not
   vendored in this repo today; verifying requires a NAV-testbed
   POST. PR-12 commits to **`<InvoiceAnnulment>`** (NAV v3.0's
   conventional annul-namespace root name; the `manageInvoice` body
   is `<InvoiceData>` and the annulment counterpart is by convention
   `<InvoiceAnnulment>` under the `OSA/3.0/annul` namespace). If
   NAV's testbed rejects, the amendment is mechanical (rename the
   root + update the `submit-annulment` PR's references; the audit
   payload shape this ADR pins does not change). **Named trigger
   for verification:** the first PR exercising
   `aberp submit-annulment` against NAV's `api-test` endpoint.

2. **The set of valid `<annulmentCode>` values.** The research file
   does not enumerate. PR-12 commits to the **four NAV v3.0 codes**
   conventional in the documented open-source NAV clients: `ERRATIC_DATA`,
   `ERRATIC_INVOICE_NUMBER`, `ERRATIC_INVOICE_ISSUE_DATE`,
   `ERRATIC_ELECTRONIC_HASH_VALUE`. Exposed at the CLI as a clap
   `ValueEnum` (loud-fail on an unknown code at parse time; same
   posture as `NavEnv` for `--endpoint`). If NAV reports a fifth
   code at testbed time, the amendment is a one-line ValueEnum
   extension + a fifth match arm. **Named trigger:** first NAV-
   testbed annulment POST that returns an `UNSUPPORTED_ANNULMENT_CODE`-
   shape error against any of the four (in which case the offending
   code is dropped) or first NAV-testbed POST exercising a fifth
   code (in which case the ValueEnum extends).

3. **Whether technical annulment of an annulled base is permitted.**
   ADR-0009 §6 does not name a position on double-annulment. NAV's
   API permits multiple annulment requests against the same invoice
   in principle. PR-12 default: **reject loudly** (precondition
   walker rejects if any prior `InvoiceTechnicalAnnulmentRequested`
   payload points at the same base). The accountant-question shape
   is the same as ADR-0023 §7's storno-of-a-storno deferral. **Named
   trigger:** accountant review.

## Decision

### 1. Operator CLI surface for technical annulment

**Subcommand name:** `aberp request-technical-annulment`.

**Rationale for the verb.** ADR-0009 §6 names the command type
`RequestTechnicalAnnulment`. The CLI verb-object form
`request-technical-annulment` matches the audit-ledger event kind
(`invoice.technical_annulment_requested`) and matches the existing
verb-object family (`issue-storno`, `issue-modification`,
`mark-abandoned`, `retry-submission`, `submit-invoice`,
`setup-nav-credentials`).

**Why `request-` and NOT `issue-` (in contrast to
`issue-storno` / `issue-modification`):**

- A technical annulment is **not itself an invoice.** It does not burn
  a sequence number; it does not consume an allocator slot; the
  `issue-*` verb family in ABERP signals "burns a sequence slot and
  produces a fresh invoice" (per ADR-0023 §1 / ADR-0024 §1). Reusing
  `issue-*` for an operation that does not produce an invoice would
  break the operator's pattern-matching when reading `aberp --help`.
- A technical annulment is **operator-action-required at NAV's side
  too** (ADR-0009 §6: "requires the receiver to confirm in the NAV
  web UI"). The `request-` verb signals "ABERP records the
  operator's request; NAV-side fulfillment is asynchronous and
  involves a human in the loop". An operator who reads
  `aberp request-technical-annulment` should not be surprised that
  the NAV-side confirmation is a separate step.
- The audit-event kind already named in ADR-0009 §2 carries the
  `_requested` suffix (`invoice.technical_annulment_requested`).
  The CLI verb matches the audit shape.

**Argument shape** (clap-flavoured):

| Flag | Type | Default | Purpose |
|---|---|---|---|
| `--references` | `String` (prefixed `inv_<ULID>`) | none (required) | The base invoice the annulment withdraws the data submission for. Must be in a state with at least one `InvoiceSubmissionResponse` on the audit ledger (i.e., the data submission to NAV actually happened — there is something to annul). See §6. |
| `--code` | `AnnulmentCode` ValueEnum | none (required) | NAV's annulment classification — one of `erratic-data` / `erratic-invoice-number` / `erratic-invoice-issue-date` / `erratic-electronic-hash-value`. Operator-required: a silent default would hide the case where the operator is unsure of which code applies (CLAUDE.md rule 4 — no hidden defaults on audit-bearing fields). |
| `--reason` | `String` | none (required) | Free-form operator-supplied justification. Same shape + posture as `mark-abandoned --reason` / `retry-submission --reason`: required at the CLI surface so the audit-evidence bundle (ADR-0009 §8) always carries human-readable justification for the annulment decision. |
| `--out` | `PathBuf` | none (required) | Path to write the annulment's `<InvoiceAnnulment>` XML. Same on-disk gate posture as `issue-invoice --out` / `issue-storno --out` / `issue-modification --out`; the future `submit-annulment` PR will POST these bytes. |
| `--db` | `PathBuf` | `./aberp.duckdb` | Tenant DuckDB. |
| `--tenant` | `String` | `"default"` | Tenant identifier. |

**What `request-technical-annulment` does NOT do.**

- **Does NOT call NAV.** Same posture as `issue-storno` /
  `issue-modification`. The NAV `manageAnnulment` POST is the future
  `submit-annulment` PR's responsibility. The boundary is loud:
  PR-12's CLI output names the next step the operator must take
  (`aberp submit-annulment ...` once that command lands).
- **Does NOT burn a sequence number.** A technical annulment is not
  an invoice. The standard allocator path (`InvoiceSequenceReserved`
  + `InvoiceDraftCreated` audit entries) is NOT exercised; the
  annulment writes exactly ONE audit entry —
  `InvoiceTechnicalAnnulmentRequested` (§3). Key contrast with
  PR-10's storno and PR-11's modification, both of which write three
  audit entries (sequence reservation + draft created + chain link).
- **Does NOT exercise the chain allocator.** No `<invoiceReference>`
  block. No `<modificationIndex>`. The annulment payload's only link
  to the base invoice is the `invoice_id` field; the annulment is
  not a chain entry and does not appear in
  `next_modification_index_in_tx`'s walk in `issue_storno.rs` /
  `issue_modification.rs`.
- **Does NOT extend `submit_invoice.rs::detect_operation_from_xml`.**
  The annulment body is never seen by `submit-invoice` — it goes to
  a different NAV endpoint. The three-way detector (Create / Modify
  / Storno) closed by ADR-0024 §3 / F22 remains a three-way
  detector. The annulment dispatch happens in the future
  `submit-annulment` command, which calls a NEW operation in
  `crates/nav-transport/src/operations/manage_annulment.rs`.

### 2. EventKind variant + on-disk storage form + the derived-typestate non-transition

**New EventKind variant:** `EventKind::InvoiceTechnicalAnnulmentRequested`.

**Storage form:** `"invoice.technical_annulment_requested"`. Same
dot-separated `invoice.` prefix convention as every other lifecycle
kind; the per-invoice export bundle (ADR-0009 §8) `invoice.*` glob
picks this up alongside storno and modification entries. The
F12-discipline `pr_12_technical_annulment_kind_uses_invoice_prefix`
test pins this in `event_kind.rs`'s test module — same shape as
PR-10's `pr_10_storno_kind_uses_invoice_prefix` and PR-11's
`pr_11_modification_kind_uses_invoice_prefix`.

**No derived typestate transition.** Key contrast with STORNO and
MODIFY:

- A STORNO entry against a base invoice causes the base's derived
  typestate to transition `Finalized → Storno` (ADR-0023 §2).
- A MODIFY entry against a base invoice causes the base's derived
  typestate to transition `Finalized → Amended` (ADR-0024 §2).
- A `InvoiceTechnicalAnnulmentRequested` entry against a base
  invoice does **NOT** transition the base's typestate. The
  annulment withdraws a NAV-side data submission; the base
  invoice's legal-document status is unchanged by the annulment
  *request* alone. NAV-side fulfillment (the receiver confirming in
  the NAV web UI) is the actual state change at NAV, and ABERP
  observes that asynchronously through a future
  `query-transaction-status` poll (out of scope for PR-12).

This is the cleanest model: ABERP's local typestate reflects what
ABERP **knows** at the time of the request, not what NAV will
**confirm** later. The annulment-pending state is read from the
audit ledger by the existence of an unconfirmed
`InvoiceTechnicalAnnulmentRequested` entry; no separate
"annulment_pending" typestate transition is recorded.

**No second variant added.** Same posture as ADR-0023 §2 / ADR-0024
§2 for STORNO / MODIFY. The annulment's future NAV-submission
attempt + response will reuse the existing
`InvoiceSubmissionAttempt` + `InvoiceSubmissionResponse` variants,
or — if the audit-evidence bundle reader needs to distinguish
"submit of an invoice" from "submit of an annulment" — a future PR
adds an `InvoiceAnnulmentSubmissionAttempt` + `…Response` pair (the
trigger is named in the future `submit-annulment` PR, not this one).

### 3. Typed payload struct + the F12 four-edit ritual

**Payload type name:** `InvoiceTechnicalAnnulmentRequestedPayload` in
`apps/aberp/src/audit_payloads.rs`. Trailing `Payload` per the
convention every other typed payload follows.

**Field shape:**

```rust
pub struct InvoiceTechnicalAnnulmentRequestedPayload {
    /// The **base invoice's** id — prefixed `inv_<ULID>`. The
    /// annulment is FOR this invoice (not a new invoice), so the
    /// payload's `invoice_id` field is the base id directly — no
    /// separate `annulment_id` / `base_invoice_id` split (key
    /// contrast with the storno/modify chain-link payloads, which
    /// carry both the new invoice's id AND the base's id).
    pub invoice_id: String,
    /// The idempotency key of the `RequestTechnicalAnnulmentCommand`.
    /// Same shape + role as `InvoiceMarkedAbandonedPayload::idempotency_key`
    /// — an operator-decision idempotency key, distinct from the
    /// base invoice's issuance idempotency key.
    pub idempotency_key: String,
    /// The base invoice's NAV `transactionId` (from the most-recent
    /// prior `InvoiceSubmissionResponse` entry against the base).
    /// Captured here so the audit-evidence bundle (ADR-0009 §8) makes
    /// the annulment-target submission unambiguously identifiable
    /// without a second walk back to the response entry. Same posture
    /// as `InvoiceRetryRequestedPayload::prior_transaction_id` /
    /// `InvoiceMarkedAbandonedPayload::prior_transaction_id`.
    pub prior_transaction_id: String,
    /// One of the four NAV annulment codes — stored as the canonical
    /// `SCREAMING_SNAKE_CASE` wire form (`ERRATIC_DATA`, etc.) rather
    /// than the lowercased clap-flavour (`erratic-data`). The wire
    /// form is the canonical record (per ADR-0024 §5's
    /// `modification_issue_date` posture — store the form that
    /// crosses the audit boundary canonically). The CLI's clap-
    /// ValueEnum is pre-converted to this form at command-parse time.
    pub annulment_code: String,
    /// Free-form operator-supplied reason. Same posture as
    /// `InvoiceRetryRequestedPayload::reason` /
    /// `InvoiceMarkedAbandonedPayload::reason`.
    pub reason: String,
}
```

The `to_bytes(&self) -> Vec<u8>` shape matches every other payload.
The constructor is `new(invoice_id, idempotency_key,
prior_transaction_id, annulment_code, reason)` — `new()` (not
`from_outcome(...)`) because the payload's fields cross the operator
decision (code + reason) and the audit chain (invoice id + prior
transaction id + idempotency); no single domain struct carries them
all, and a speculative `AnnulmentRequestOutcome` type would be a
CLAUDE.md rule-2 violation.

**The F12 four-edit ritual** (carries forward from ADR-0023 §3 /
ADR-0024 §5):

| # | File | Edit |
|---|---|---|
| 1 | `crates/audit-ledger/src/entry/event_kind.rs` | Add `InvoiceTechnicalAnnulmentRequested` variant + `as_str` arm (storage form `"invoice.technical_annulment_requested"`) + `from_storage_str` arm + extend the `round_trip_for_every_variant` test's variant list + a new `pr_12_technical_annulment_kind_uses_invoice_prefix` test (mirror PR-10 / PR-11). Same four sub-edits as ADR-0024 §5's row 1; F12 closed-set discipline. |
| 2 | `apps/aberp/src/audit_payloads.rs` | New `InvoiceTechnicalAnnulmentRequestedPayload` struct + `new(...)` + `to_bytes(&self)` + at least two round-trip unit tests (one happy-path; one with hostile reason text to pin the F9 trap-closing posture, parallel to `marked_abandoned_round_trips_with_hostile_reason`). |
| 3 | `apps/aberp/src/cli.rs` | New `Command::RequestTechnicalAnnulment(RequestTechnicalAnnulmentArgs)` variant + the args struct per §1 above + a new `AnnulmentCode` clap `ValueEnum` for `--code`. |
| 4 | `apps/aberp/src/request_technical_annulment.rs` | New file — `run` mirroring `mark_abandoned.rs`'s shape (single audit entry, no NAV call, no chain interaction) with the annulment-specific pre-flight (base must have a prior `InvoiceSubmissionResponse`, NOT already annulled per §6) and the AnnulmentData XML render+write step. |

**Plus three derivative edits in PR-12 that consequence-of-the-above
covers (NOT a five-edit ritual extension — these are mechanical):**

- `apps/aberp/src/lib.rs`: `pub mod request_technical_annulment;`.
- `apps/aberp/src/main.rs`: dispatch arm
  `cli::Command::RequestTechnicalAnnulment(a) =>
  request_technical_annulment::run(&a),`.
- `apps/aberp/src/nav_xml.rs`: new `pub fn render_annulment_data(...)`
  function + new `AnnulmentReference` (or equivalent) input struct +
  new `AnnulmentCode → wire string` conversion helper.

**No XSD validator edit in PR-12.** Same explicit posture as §4
below — the runtime validator's `validate_invoice_data` walks the
`<InvoiceData>` schema; `<InvoiceAnnulment>` is a separate schema.
Extending the validator to `validate_annulment_data` is the
named-trigger work for the future `submit-annulment` PR (or
sooner — see §4).

### 4. AnnulmentData XML emitter + the deferred runtime validator

**Render function:** `nav_xml::render_annulment_data` produces the
`<InvoiceAnnulment>` body to bytes. Root element + namespace per
§"Surfaced conflict 1":

- Root: `<InvoiceAnnulment>`.
- Namespace: `http://schemas.nav.gov.hu/OSA/3.0/annul` (the NAV v3.0
  annul namespace; the conventional pinning per ADR-0009 §1's NAV
  v3.0 target).
- Children, in document order:
  - `<annulmentReference>` — the base invoice's NAV-facing number
    (e.g., `INV-default/00007`), built by the caller the same way
    `ModificationReference::base_invoice_number` is built in
    `apps/aberp/src/issue_modification.rs::run` step 10.
  - `<annulmentTimestamp>` — UTC `xs:dateTime` (`YYYY-MM-DDTHH:MM:SSZ`)
    captured at render time. ABERP-server-clock-only per ADR-0007
    §Operator-as-threat-actor controls (no operator override).
  - `<annulmentCode>` — one of the four wire-form codes (§3).
  - `<annulmentReason>` — the operator's reason text verbatim
    (XML-escaped by `quick_xml` — same escaping discipline every
    other text-element write goes through).

**Why the timestamp is server-clock-only.** The annulment timestamp
is an audit-bearing field that fixes when the operator decided to
withdraw. An operator-supplied date would mask the case where an
audit reader needs to know "when did ABERP actually log this" vs
"what date did the operator type". The same posture as ADR-0007
§Operator-as-threat-actor controls for invoice issue date.

**Why NOT a `--annulment-date` operator arg (in contrast to
ADR-0024 §1's `--modification-date`):**

- A modification's `<modificationIssueDate>` is the **legal date of
  the corrected invoice's issuance** — accountants legitimately set
  it to a backdated value (§1 conflict in ADR-0024 §1). It is part
  of the invoice's legal content.
- An annulment's `<annulmentTimestamp>` is the **technical timestamp
  of the data-submission withdrawal** — there is no legitimate
  reason to backdate it. NAV uses it to order the withdrawal
  request against the prior submission's timestamp.

So the two surfaces look superficially parallel (both date-like
audit-bearing fields) but pull apart on the operator-vs-server-clock
question. PR-12 commits to server-clock for annulments; CLAUDE.md
rule 12 (fail loud on audit-bearing decisions) — the server clock
is unambiguous, the operator clock would be a surprise vector.

**Runtime XSD validator extension: DEFERRED.** ADR-0022 names the
runtime validator's job (`validate_invoice_data`). Extending the
validator to `validate_annulment_data` would require either
vendoring the NAV v3.0 annul XSD or hand-rolling a parallel
invariant check. PR-12 does **NOT** ship the extension. Instead, a
minimal call-site sanity check in `request_technical_annulment::run`
verifies the rendered bytes:

- Parse the rendered XML as well-formed via `quick_xml::Reader`.
- Confirm the four required children are present (loud-fail if any
  is missing — would indicate an emitter bug at run time).

This is **not** a substitute for the XSD validator. It is the
minimum loud-fail surface so a future emitter regression (e.g.,
someone forgets `<annulmentCode>` after a refactor) is caught at
the `request-technical-annulment` boundary instead of at NAV
submission time. **Named trigger for the full validator
extension:** the first PR that lands `submit-annulment` (i.e., the
first PR that actually POSTs an annulment to NAV) MUST also ship
`validate_annulment_data` so the wire surface enforces the schema
invariant the same way `validate_invoice_data` does for issuance.

### 5. Idempotency

Layer 1 (client-side ULID on the `RequestTechnicalAnnulmentCommand`)
applies. The audit-ledger lookup for prior payloads with the same
`idempotency_key` is the standard replay-safety surface — re-running
the same command returns the prior result without re-writing the
audit entry.

Layer 2 (NAV-side disambiguation) does NOT fire for
`request-technical-annulment` directly. The future
`submit-annulment` PR's `manageAnnulment` call inherits the same
Layer 2 posture as `manageInvoice` (per ADR-0009 §5).

### 6. Precondition walker

PR-12's `request_technical_annulment::run` precondition walker accepts:

- Base has at least one `InvoiceSubmissionResponse` payload (i.e.,
  the data submission to NAV actually happened — there is something
  to annul).

And **loud-rejects**:

- Base never submitted (no `InvoiceSubmissionResponse`). Annulment
  of a never-submitted invoice is malformed; the operator should
  use `mark-abandoned` instead (which is the local-decision
  terminator for unsubmitted-but-abandoned invoices).
- Base already has a prior `InvoiceTechnicalAnnulmentRequested`
  payload pointing at it (default-reject double annulment; §
  "Surfaced conflict 3" — accountant question, open).

**Does NOT reject:**

- Base in `Finalized` (NAV `SAVED`) — the canonical use case
  (annulling a successful submission whose data was wrong).
- Base in `Rejected` (NAV `ABORTED`) — the test-invoice-reached-prod
  scenario per ADR-0009 §6's named example: even if NAV rejected
  the prior submission, the operator may still want to file a
  technical annulment to clear the audit trail at NAV's side.
- Base in `Stuck` — if the submission landed but the ack never
  finalized, the operator may want to annul without waiting for the
  poll resolution.
- Base in `Abandoned` (`InvoiceMarkedAbandoned`) — abandonment is a
  local-only fact; annulment is the NAV-side withdrawal. The two
  are orthogonal.
- Base already STORNO'd or AMENDED — technical annulment is data-
  submission withdrawal, not legal cancellation. The two surfaces
  don't conflict; an invoice that was legally stornoed but whose
  data submission was also wrong may legitimately receive both a
  storno entry AND a technical annulment.

Each rejection produces a named-reason error message per CLAUDE.md
rule 12 — same shape as `issue_storno::check_base_is_finalized` /
`issue_modification::check_base_is_modifiable` error texts.

**Why "no double annulment" by default.** A second annulment
against an already-annulled base is the wrong operation: the first
annulment is sufficient to mark the data submission as withdrawn;
a second one would create operator-decision noise in the audit
trail without changing the NAV-side outcome. The accountant
question of "may a single base receive two annulment requests with
different codes" is filed as an open question (§8) with default-
reject.

### 7. No chain-allocator interaction

PR-12 does **NOT** widen `next_modification_index_in_tx` in
`issue_storno.rs` or `issue_modification.rs` to walk
`InvoiceTechnicalAnnulmentRequested` entries.

**Why.** NAV's `modificationIndex` uniqueness is per
`invoiceReference` for chain operations (STORNO + MODIFY). Technical
annulment is NOT a chain operation — it does not carry an
`<invoiceReference>` block, does not get a `modificationIndex`, and
NAV's uniqueness rule for chain-index does not apply to it. The
chain allocator's two-kind walk introduced in ADR-0024 §7 remains
two-kind; the symmetric extraction trigger (third chain kind) does
NOT fire here because annulment is not a chain kind.

This is the inverse of the case where ADR-0024 §7 extended the
storno-only walker to also see modifications: there the new kind
WAS in the chain, so the allocator had to widen. Here the new kind
is NOT in the chain, so the allocator does NOT widen. The two cases
are symmetric in the opposite direction.

## Open questions

Tracked against the next fortnightly adversarial review and the
named external-check items in `docs/research/nav-and-billingo.md`:

- **Double-annulment accountant practice.** Default-reject until
  accountant resolves (§"Surfaced conflict 3", §6, §8). If the
  accountant resolves to permit, the precondition walker drops the
  already-annulled rejection branch and the payload schema may
  acquire an `annulment_sequence` field to disambiguate multiple
  entries against the same base.
- **`<InvoiceAnnulment>` root element name + namespace.** Default
  reading per §"Surfaced conflict 1". Verification deferred to first
  NAV-testbed annulment POST (the future `submit-annulment` PR).
- **The set of valid `<annulmentCode>` values.** Default four codes
  per §"Surfaced conflict 2". Verification deferred to first
  NAV-testbed annulment POST. NAV returning an
  `UNSUPPORTED_ANNULMENT_CODE` for any of the four would drop it
  from the ValueEnum; NAV requiring a fifth would add it.
- **AnnulmentData XSD runtime validator.** Deferred to the future
  `submit-annulment` PR per §4. The trigger is named: the first PR
  that ships `submit-annulment` MUST also ship
  `validate_annulment_data`.
- **`<annulmentTimestamp>` precise format.** PR-12 commits to ISO
  8601 / `xs:dateTime` (`YYYY-MM-DDTHH:MM:SSZ`). NAV may require a
  compressed `YYYYMMDDhhmmss` form (which is what `requestTimestamp`
  uses in the SOAP header); if the testbed rejects, the change is
  a one-line formatter swap in `nav_xml::render_annulment_data`.

## Consequences

**What gets easier**

- PR-12 lands without re-litigating naming, audit-payload shape,
  or the boundary between this PR and the future `submit-annulment`
  PR. The pre-flight reading is this ADR plus ADR-0023 plus
  ADR-0024 plus `apps/aberp/src/mark_abandoned.rs` (the closest
  template for a single-audit-entry, no-NAV-call operator command)
  plus `apps/aberp/src/issue_modification.rs` (the closest template
  for an XML-emitter + audit-entry + precondition-walker
  combination).
- The audit-evidence bundle (ADR-0009 §8) gains the technical-
  annulment leg with no schema changes to the per-invoice walker:
  the `invoice.*` glob picks up
  `invoice.technical_annulment_requested` alongside everything else.
- The future `submit-annulment` PR has a clear boundary: PR-12
  produces the annulment XML on disk + the operator-decision audit
  entry; `submit-annulment` consumes the XML, calls NAV, and writes
  the wire-attempt audit entries. The same boundary the existing
  `issue-storno` / `submit-invoice` split follows.

**What gets harder**

- The audit-payload schema versioning rule
  (ADR-0023 §"What we lock ourselves into" / ADR-0024 §"What gets
  harder") applies to `InvoiceTechnicalAnnulmentRequestedPayload`
  too: adding a field is forward-compatible (older readers see the
  pre-extension shape); removing or renaming requires a new
  `EventKind` variant.
- The CLI surface now has nine subcommands (issue-invoice,
  submit-invoice, setup-nav-credentials, poll-ack, retry-submission,
  mark-abandoned, serve, issue-storno, issue-modification, and
  request-technical-annulment makes ten). The `--help` output is
  getting dense; a future PR may want to split into command groups
  (e.g., `aberp invoice issue-storno` etc.). Out of scope for PR-12;
  the trigger would be operator feedback that the flat list is
  unwieldy.
- The minimal call-site sanity check in
  `request_technical_annulment::run` (§4) is **not** an XSD
  validator. The audit-evidence-bundle reader cannot assume the
  rendered AnnulmentData on disk is schema-valid; the assumption
  becomes valid only after the future `submit-annulment` PR ships
  `validate_annulment_data`. This is a known soft-gap, surfaced
  loud in §4 with a named trigger.

**What we lock ourselves into**

- Subcommand name `aberp request-technical-annulment` and arg
  names (`--references`, `--code`, `--reason`, `--out`, `--db`,
  `--tenant`). Rename requires an amendment ADR.
- Payload struct name `InvoiceTechnicalAnnulmentRequestedPayload` +
  field names + the `annulment_code: String` (wire-form) shape.
- The four-code ValueEnum (§"Surfaced conflict 2"). Adding a fifth
  is a one-line extension; dropping one is a payload-decode-
  compatibility question per the schema versioning rule.
- The decision to NOT extend the chain allocator (§7). If a future
  use case treats annulment as chain-affecting (it does not today
  and NAV's API does not require it), the allocator extension is
  the named trigger.
- The decision to NOT extend `detect_operation_from_xml` (§1). The
  detector remains three-way (Create / Modify / Storno); annulment
  bodies never reach `submit-invoice`.

## Adversarial review

A hostile NAV inspector + a hostile-engineer review, alternating.
ADR-README bar is three; four surfaced because the technical-
annulment surface is operator-action-required at NAV's side and the
audit trail must be unambiguous for an inspector.

1. **"Your precondition walker permits annulment of an `ABORTED`
   base. A NAV inspector reading the audit trail sees the prior
   submission was rejected, then sees the operator filed an
   annulment 'to clear the audit trail'. The inspector argues the
   annulment is a no-op because NAV already rejected the data —
   nothing was actually stored at NAV that needs withdrawing."**
   The use case ADR-0009 §6 names ("a test invoice accidentally
   sent to prod") covers exactly this scenario: a prod submission
   that NAV may have either accepted (SAVED) or rejected (ABORTED),
   either way the operator wants the audit trail at NAV's side to
   reflect "this was wrong". NAV's `manageAnnulment` endpoint
   accepts annulment of both SAVED and ABORTED submissions; the
   inspector's "no-op" argument is correct in the SAVED case
   (something is being withdrawn) and is the operator's call in the
   ABORTED case (preemptive cleanliness, costs nothing). The
   walker permits both; the reason text is the operator's record
   of why. **Accepted, surfaced.**

2. **"The annulment's audit entry is the ONLY entry written by
   `request-technical-annulment` — there is no
   `InvoiceSequenceReserved` or `InvoiceDraftCreated`. A future
   contributor reading the codebase will look at PR-10's three
   entries and PR-11's three entries and assume PR-12 should also
   write three entries. They will add a sequence reservation that
   burns an unnecessary sequence slot, silently. The gap-free
   invariant breaks."** The single-entry shape is the load-bearing
   structural difference between annulment and chain operations.
   PR-12's `request_technical_annulment.rs` module-header comment
   explicitly names this: "single audit entry, no NAV call, no
   chain interaction, no sequence-slot burn." Any future
   contributor's diff that adds a second audit entry would have to
   also delete the module-header comment — visible in PR review.
   Additionally, the F12 four-edit ritual's `audit_payloads.rs`
   tests pin the payload shape, and a hostile contributor
   introducing a sequence-reservation against an annulled invoice
   would have to extend three other walkers (mark-abandoned's
   precondition, issue-storno's allocator, issue-modification's
   allocator) — the surface area of the change makes the silent
   regression mechanically hard. **Accepted, surfaced via inline
   comment + ADR §1 + the precondition walker's named errors.**

3. **"You commit to `<InvoiceAnnulment>` as the root element name
   without a NAV-testbed verification. If NAV's actual XSD uses a
   different name (e.g., `<AnnulmentData>` paralleling
   `<InvoiceData>`), every test environment will reject every
   annulment ABERP produces. PR-12 ships dead code."** The risk is
   real; the same risk pattern ADR-0024 §1 conflict 1 accepted for
   `<modificationIssueDate>`'s position. The mitigation has three
   legs:
   - PR-12 does NOT call NAV. Dead code at the wire is not dead
     code at the audit trail — the operator-decision entry is the
     primary load-bearing artifact regardless of whether the wire
     submission ever succeeds.
   - The future `submit-annulment` PR is the first PR exercising
     the assumption; the trigger to amend is named (§"Surfaced
     conflict 1"). A NAV-testbed rejection at that point is a
     one-line emitter change.
   - The audit-payload's `annulment_code` + `reason` + `invoice_id`
     are the inspector-facing primary records; the on-disk XML is
     secondary evidence that can be re-rendered after the emitter
     fix.
   **Accepted with trigger named.**

4. **"The CLI surface now has ten subcommands. Operator pattern-
   matching on `--help` output starts to fail; an operator looking
   for 'cancel an invoice' might pick `request-technical-annulment`
   when they want `issue-storno`. The naming is correct but the
   surface area is the problem."** The verb-object family
   discipline (§1) is the mitigation: `issue-*` is for operations
   that produce an invoice (storno + modification + fresh issuance),
   `request-*` is for operator decisions that involve NAV-side
   asynchronous fulfillment, `mark-*` is for terminal local
   decisions. An operator who reads the verb prefix gets the
   right semantic bucket. The CLI's `--help` output additionally
   carries the per-command description (the doc comment on each
   `Command` variant in `cli.rs`) — PR-12's
   `RequestTechnicalAnnulment` description explicitly contrasts
   itself against `issue-storno` to short-circuit the
   misclassification. **Accepted — the verb-family discipline plus
   the inline help text is the mitigation; if operator feedback
   later shows the flat list is unwieldy, the command-group split
   is the natural future direction.**

## Alternatives considered

- **Use `issue-technical-annulment`** (matching the `issue-storno` /
  `issue-modification` family). Rejected per §1 — a technical
  annulment is not an invoice; the `issue-*` family signals
  sequence-slot consumption, which annulment does not do. The
  `request-*` verb correctly signals operator-decision +
  NAV-asynchronous fulfillment.

- **Bundle the NAV `manageAnnulment` call into PR-12.** Rejected —
  matches the storno / modification two-phase split (`issue-*`
  writes XML; `submit-invoice` POSTs). Bundling would couple PR-12
  to the NAV operation extension in
  `crates/nav-transport/src/operations/manage_annulment.rs` plus
  the AnnulmentData XSD validator extension; the resulting PR would
  be larger than PR-10 + PR-11 combined, and the boundary between
  "operator decision" and "wire submission" would blur.

- **Re-use `EventKind::InvoiceMarkedAbandoned` for technical
  annulment.** Rejected — abandonment is a local-only decision
  ("ABERP stops retrying"); technical annulment is a NAV-side
  withdrawal request. Conflating the two would make the audit-
  evidence bundle unable to distinguish "operator gave up locally"
  from "operator filed a NAV-side withdrawal" — the exact failure
  mode CLAUDE.md rule 12 names.

- **Default `--code` to `ERRATIC_DATA` (the most generic).**
  Rejected per §1 — `--code` is an audit-bearing field that the
  operator must consciously pick; a silent default would mask the
  case where the operator is unsure and would have benefitted from
  thinking about which code applies. CLAUDE.md rule 4 (no hidden
  defaults on audit-bearing inputs).

- **Default `--out` to a generated path (e.g.,
  `./annulment-<invoice-id>-<timestamp>.xml`).** Rejected — same
  posture as `issue-invoice --out` / `issue-storno --out` /
  `issue-modification --out`: the path is operator-required so the
  artifact's location is unambiguous and explicit, not hidden in a
  pwd-relative scratch file.

- **Capture the `<annulmentTimestamp>` in the audit payload too.**
  Rejected — the audit ledger's `appended_at` column already
  carries the entry's server-clock timestamp (per ADR-0008). A
  second timestamp field in the payload would be a redundant source
  of truth; ADR-0024 §"Alternatives considered"'s "no duplicate
  field" posture applies symmetrically.

- **Store the annulment code as a typed enum rather than a
  String.** Rejected per the same posture ADR-0024 §"Alternatives
  considered" uses for `modification_issue_date: String`: the
  audit payload's serialization shape is the canonical record; a
  typed-enum wrapper would force serde-with adapters for a value
  that is canonical on the wire. The CLI's clap-ValueEnum keeps the
  loud-fail surface at the operator boundary.

## Follow-on PRs unblocked by this decision

- **PR-12 — Technical annulment request (code).** Implements the
  four edits in §3 above plus
  `apps/aberp/src/request_technical_annulment.rs` and the
  AnnulmentData emitter in `nav_xml.rs`.
- **First per-invoice export-bundle PR (gated on F5 + F10 per
  session 12 handoff).** Consumes the
  `InvoiceTechnicalAnnulmentRequested` payloads via the same
  `invoice.*` glob as storno and modification entries.
- **`submit-annulment` PR.** Adds the NAV `manageAnnulment`
  operation in `crates/nav-transport/src/operations/manage_annulment.rs`,
  the `validate_annulment_data` runtime validator in
  `aberp-nav-xsd-validator`, and the matching CLI command. Trigger
  for verifying §"Surfaced conflict 1" + §"Surfaced conflict 2"
  + §4's deferred validator extension.
- **First NAV-testbed annulment POST.** Verifies the
  `<InvoiceAnnulment>` root + namespace assumption (§"Surfaced
  conflict 1") and the four-code set (§"Surfaced conflict 2").
- **Annulment-receiver-confirmation observation PR.** Per ADR-0009
  §6, NAV requires the receiver to confirm the annulment in the
  NAV web UI. ABERP cannot drive that step, but it can observe the
  outcome via a future `query-annulment-status` poll. Trigger:
  first operator request for "is my annulment confirmed yet?".
