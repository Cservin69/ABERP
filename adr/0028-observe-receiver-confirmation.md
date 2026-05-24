# ADR-0028 — observe-receiver-confirmation — queryInvoiceData against the base invoice as the NAV-side receiver-confirmation observation, new audit-evidence EventKind, one-shot (not bounded-poll) (ADR-0009 §6 + ADR-0027 §"Surfaced conflict 3" closure)

- **Status:** Accepted
- **Date:** 2026-05-22
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR — closes the final
  ADR-0009 §6 observation gap (receiver-confirmation of a
  technical annulment), pairing with ADR-0027's wire-ack poll.
  After this ADR + PR-15 land, the operator can drive the full
  technical-annulment lifecycle end-to-end AND observe NAV-side
  receiver confirmation. Load-bearing deltas: §1 (CLI verb +
  arg shape), §2 (single new EventKind + payload — F12 four-edit
  ritual fires once), §3 (which NAV operation to use — the
  three-way pick the session 18 handoff named, decided in
  §"Surfaced conflict 1"), §4 (one-shot vs bounded-poll — §
  "Surfaced conflict 2"), §5 (what field in the response carries
  the receiver-confirmed signal — §"Surfaced conflict 3"; the
  honest answer is "verbatim bytes only, parsing deferred to
  NAV-testbed amendment"), §6 (precondition walker), and §7
  (F8 idempotency-key flow). Does **not** supersede ADR-0009 §6,
  ADR-0025, ADR-0026, or ADR-0027; all four remain in force.
- **Related:**
  - **ADR-0009 §6** (storno + modification chain + technical
    annulment + the observation surface — the surface this ADR
    closes the receiver-confirmation half of).
  - **ADR-0009 §4** (NAV authentication — `queryInvoiceData`
    uses the non-`manageInvoice` requestSignature form, same as
    every other NAV query operation).
  - **ADR-0009 §8** (audit-evidence bundle — the new entry
    lands inside the `invoice.*` glob via the same prefix
    convention as every other lifecycle kind).
  - **ADR-0020 §1, §2** (NAV environment is explicit on the CLI;
    re-asserted for `observe-receiver-confirmation` per the
    same posture as every other NAV-call subcommand).
  - **ADR-0025** (technical-annulment request — PR-12 predicate;
    the operator-decision audit entry whose idempotency_key
    flows forward through PR-13 → PR-14 → PR-15).
  - **ADR-0026** (submit-annulment wire flow — PR-13 predicate;
    the wire submission that produced the annulment-side
    `transactionId` PR-14's poll keys on).
  - **ADR-0027** (poll-annulment-ack wire flow — PR-14 predicate;
    its terminal-`SAVED` operator-visible message NAMED THE
    RECEIVER-CONFIRMATION GAP LOUD and named the trigger this
    ADR closes: "first operator request for 'did the receiver
    actually confirm the annulment?'").
  - **Session 18 handoff F34** — receiver-confirmation
    observation not implemented (named trigger fires here; F34
    closes with this PR).
  - **Deferred ADR — NAV historical / reconciliation read path**
    (called out in ADR-0010 §Deferred and `adr/README.md`
    §Deferred). PR-15 lands ONE NAV query operation
    (`queryInvoiceData`) for the receiver-confirmation slice
    only; the broader reconciliation surface
    (`queryInvoiceDigest`, `queryInvoiceChainDigest`,
    `queryTransactionList`, `queryInvoiceCheck`) remains
    deferred per the same trigger ("first PR wiring a NAV-side
    reconciliation pass against migrated invoices, or the first
    NAV-audit operator view"). The receiver-confirmation
    observation is operationally distinct from reconciliation
    and surfaces here as a discrete F34 closure, NOT as
    pre-emptive reconciliation infrastructure (CLAUDE.md
    rule 2).
- **Source material:** `docs/research/nav-and-billingo.md`
  §"Submission flow" operations #5-#9 (the query family) +
  §"Storno and modification" clause 3 (the receiver-confirms-
  in-NAV-web-UI half).

## Context

ADR-0027 + PR-14 landed the **wire-poll half** of the
technical-annulment surface — `aberp poll-annulment-ack` queries
NAV's `queryTransactionStatus` against the annulment-side
`transactionId` and records the wire-side ack (`RECEIVED` /
`PROCESSING` / `SAVED` / `ABORTED`). The terminal-`SAVED`
operator-visible message NAMED THE RECEIVER-CONFIRMATION GAP
LOUD per CLAUDE.md rule 12 + ADR-0027 §5:

> NOTE: NAV-side SAVED means the annulment submission has been
> accepted for processing; the receiver must still confirm the
> annulment in the NAV web UI per ADR-0009 §6. ABERP does NOT
> yet observe receiver confirmation; a future query-receiver-
> confirmation PR will close that gap.

That trigger fires here. Session 18 handoff F34 is the index
entry. The operator's next question after observing wire-SAVED
is: **"Has the receiver actually confirmed the annulment in the
NAV web UI?"**

### What the research file actually says

`docs/research/nav-and-billingo.md` lists ten NAV v3.0
operations. The relevant query operations are:

- **#5 `queryInvoiceData`** — "full invoice data by invoice
  number; caller must be supplier or customer." This returns
  the complete NAV-side record of an invoice; if NAV exposes
  receiver-confirmation state of an annulment anywhere, the
  invoice data response is the most direct candidate carrier.
- **#6 `queryInvoiceCheck`** — "boolean existence check." Too
  narrow for receiver-confirmation: a `true`/`false` on
  invoice-existence does NOT distinguish "annulled and
  receiver-confirmed" from "annulled but not yet confirmed"
  from "not annulled at all."
- **#9 `queryInvoiceChainDigest`** — "paginated traversal of
  base + every amendment/storno in the chain, across systems.
  Important when ABERP amends an invoice originally issued in
  Billingo." Scoped to amendments and stornos in the invoice's
  chain. Annulment is NOT an amendment or storno — it is a
  data-submission withdrawal per ADR-0025 §1. The chain digest
  is unlikely to surface annulment confirmation; it covers a
  different concept entirely.

The research file is **silent on which signal NAV actually
exposes for "the receiver confirmed the annulment."** §"Storno
and modification" clause 3 names the workflow:

> Endpoint: `manageAnnulment`. Requires the issuer to mark it,
> then the receiver to confirm in the NAV web UI.

— but does not name the operation NAV uses to surface that
confirmation. This is a research-file gap the session-19 build
must navigate honestly (CLAUDE.md rule 7: surface conflicts,
don't average them) rather than pretend resolved.

### Surfaced conflicts (CLAUDE.md rule 7)

Three ambiguities the build phase will otherwise paper over:

1. **Which NAV operation observes receiver-confirmation of a
   technical annulment.** The session 18 handoff named the
   three candidates (`queryInvoiceData` /
   `queryInvoiceChainDigest` / `queryInvoiceCheck`) and
   explicitly asked the ADR-0028 author to pick. The three
   readings:

   - **Reading A: `queryInvoiceData`** (this ADR's pick).
     Returns the FULL invoice data for the base invoice. If
     NAV annotates an annulled invoice's data record with any
     receiver-confirmation marker, it is in this response.
     ADR-0009 §8's verbatim-bytes-before-parse discipline
     captures whatever NAV emits, parseable or not; the field
     ABERP eventually parses (a future amendment ADR after
     NAV-testbed verification) lives inside the recorded
     bytes from day one.

   - **Reading B: `queryInvoiceChainDigest`.** Designed for
     base + amendment/storno chain traversal. Annulment is
     NOT in that chain per ADR-0025 §1 — annulment withdraws
     a data submission; it does not legally cancel the
     invoice as a document. Adding annulment to the chain-
     digest reading conflates legal-cancellation with data-
     submission-withdrawal exactly along the line ADR-0025
     §"Surfaced conflict 1" + ADR-0009 §6 specifically
     separated.

   - **Reading C: `queryInvoiceCheck`.** Too narrow (boolean
     existence only); cannot disambiguate the three states an
     operator needs to distinguish (not annulled / annulled
     not confirmed / annulled and confirmed). Reading C
     loud-fails the operator's question by construction.

   PR-15 commits to **Reading A**. Rationale: every consulted
   source backs the data-response as the most-direct carrier
   for "current state of this invoice." `queryInvoiceData` is
   the only operation in the v3.0 list whose stated purpose
   ("full invoice data by invoice number") would naturally
   carry annulment-confirmation as a sub-field IF NAV exposes
   one at all. The future-NAV-fields concern surfaces as a
   parse-time loud-fail in the new operation's response
   parser (per CLAUDE.md rule 12); the amendment trigger is
   the same name shape ADR-0026 + ADR-0027 already use
   ("first NAV-testbed run reveals the actual response
   field").

2. **One-shot query vs bounded-poll loop.** ADR-0027 + ADR-0009
   §5 prescribe a bounded poll loop with exponential backoff
   for the **wire-side** ack — that loop runs in seconds
   because NAV's wire-processing is seconds-paced. The
   receiver-confirmation is **human-paced** (the receiver
   logs into the NAV web UI, reviews, decides to confirm or
   dispute). Polling that with a 1s/2s/4s/8s/16s schedule is
   structurally wrong: the operator's question fires once
   ("right now, has the receiver confirmed?"), and if the
   answer is no, the operator's next move is to wait days,
   not seconds, before re-running the command.

   The two readings:

   - **Reading A: One-shot query** (this ADR's pick). Run
     `queryInvoiceData` once; write one audit entry; report
     the operator-visible result naming whether the response
     bytes contain a recognizable confirmation marker (with
     the named-trigger amendment surface from §3 below).
     Operator re-runs the command when they want a fresh
     observation. Same posture every NAV *query* operation
     takes when there is no ADR-0009 §5-class transient-
     retryable expectation.

   - **Reading B: Bounded-poll loop, longer backoff** (e.g.,
     30s/1min/5min/30min/1h). Mirrors `poll-annulment-ack`'s
     shape but with human-pace backoff. Rejected — couples
     `observe-receiver-confirmation` to a fixed wait
     schedule that may not match the receiver's actual pace
     (some confirm in minutes; some take days). Operator
     workflow drives the re-query cadence better than a
     hard-coded loop.

   PR-15 commits to **one-shot**. The familiar bounded-poll
   trigger fires only if NAV's `queryInvoiceData` returns
   transient `OPERATION_FAILED`-class errors that warrant
   the same in-call retry the wire path gets; that is **per-
   call retry**, not multi-call polling, and is the new
   nav-transport operation's responsibility (§3).

3. **What field in the `queryInvoiceData` response carries
   the receiver-confirmation signal.** The research file does
   NOT name one. Three candidate shapes (none verified):

   - A top-level `<annulmentStatus>` element with text values
     like `INITIATED` / `CONFIRMED` / `REJECTED`.
   - An `<annulments>` block with `<annulmentTimestamp>` +
     `<receiverConfirmationTimestamp>` fields that are present
     iff the receiver confirmed.
   - A general-purpose `<invoiceStatus>` field whose
     enumeration includes an annulment-specific value.

   ALL THREE are speculative — CLAUDE.md rule 2 forbids
   pre-emptive parsing of fields the research file has not
   surfaced. The two readings:

   - **Reading A: Verbatim-bytes only** (this ADR's pick).
     PR-15 records the verbatim `<QueryInvoiceDataResponse>`
     bytes in the audit ledger. The operator inspects the
     bytes (via a future export-bundle reader or by hand) to
     determine receiver-confirmation state. The operator-
     visible message names the verbatim-bytes-in-ledger as
     the source of truth and explicitly does NOT claim
     receiver-confirmation status. F34 closes at the AUDIT-
     EVIDENCE level (ABERP can now show "I queried NAV
     about invoice X at time T and these are the bytes NAV
     returned"); the SEMANTIC-INTERPRETATION layer lands in
     a future amendment ADR after NAV-testbed reveals the
     actual response shape.

   - **Reading B: Speculative parse for one of the three
     candidates above.** Rejected — CLAUDE.md rule 2. A
     parser that targets a field NAV does not emit either
     silently returns "unknown" on every real call (Reading
     B-1: the field is wrong but absent harmlessly) OR
     loud-fails on every real call (Reading B-2: the field
     is wrong AND the parser hard-fails). Both are worse
     than recording bytes and naming the gap loud.

   PR-15 commits to **verbatim-bytes only**. The named-
   trigger amendment is "first NAV-testbed
   `observe-receiver-confirmation` run reveals the actual
   response shape," which fires the same external-check
   item the rest of the build-phase has used (ADR-0026 §3
   for the manageAnnulment envelope; ADR-0027 §3 for the
   poll response shape).

## Decision

### 1. Operator CLI surface for observe-receiver-confirmation

**Subcommand name:** `aberp observe-receiver-confirmation`.

**Rationale for the verb.** Distinct from the `poll-*` family
because the surface is one-shot, not a bounded loop per §
"Surfaced conflict 2". The verb-object family now includes:

- `issue-*` (issue-invoice / issue-storno / issue-modification)
  → produce a new on-disk XML body + audit entries; do not call
  NAV.
- `request-*` (request-technical-annulment) → record an
  operator decision + an on-disk XML body; do not call NAV.
- `submit-*` (submit-invoice / submit-annulment) → POST a
  pre-rendered XML body to NAV + write audit entries.
- `poll-*` (poll-ack / poll-annulment-ack) → bounded loop;
  query NAV for terminal wire state, expecting seconds-pace
  resolution.
- `observe-*` (observe-receiver-confirmation) → **one-shot
  NAV query**; expecting human-pace state-change observation
  that the operator re-runs at their cadence.
- `retry-*` / `mark-*` → operator unblock decisions.

The split is load-bearing per CLAUDE.md rule 12: an operator
seeing `poll-receiver-confirmation` in `--help` would
reasonably assume "this loops until terminal," which is
exactly the wrong mental model for a human-paced confirmation
step.

**Argument shape** (clap-flavoured) — five fields, parallel to
`PollAnnulmentAckArgs`:

| Flag | Type | Default | Purpose |
|---|---|---|---|
| `--invoice-id` | `String` (`inv_<ULID>`) | none (required) | Base invoice id of the annulment. Used to (a) precondition-check the prior annulment-request + wire-submission audit entries, (b) load the base invoice's NAV-facing invoice number from the billing store, and (c) carry the annulment-request idempotency_key on the new audit entry per §7. |
| `--tax-number` | `String` | none (required) | Hungarian tax number. Same parser as `poll-annulment-ack`. |
| `--db` | `PathBuf` | `./aberp.duckdb` | Tenant DuckDB. |
| `--tenant` | `String` | `"default"` | Tenant identifier — drives the audit-ledger genesis hash and the keychain service-name lookup. |
| `--endpoint` | `NavEnv` (clap ValueEnum) | none (required) | `test` or `production`. Explicit per ADR-0020 §1; same posture as every other `submit-*` / `poll-*` command. |

**What `observe-receiver-confirmation` does NOT do.**

- **Does NOT loop.** One `queryInvoiceData` call per
  invocation per §"Surfaced conflict 2". If transient
  retryable errors fire the per-call backoff is in the
  nav-transport operation; the binary does not re-call.
- **Does NOT parse a receiver-confirmation field.** Per §
  "Surfaced conflict 3" the verbatim-bytes-only posture
  applies until NAV-testbed verification surfaces the actual
  response shape.
- **Does NOT mutate any billing row.** Annulment is not an
  invoice operation; the base invoice's typestate is unchanged
  per ADR-0025 §2. The query result lands in the audit ledger
  only.
- **Does NOT take `--nav-invoice-number` from the operator.**
  The NAV-facing invoice number is looked up from the billing
  store via the base invoice's `ReadyInvoice` row + its
  `InvoiceSeries` row (same pattern `submit_invoice.rs` /
  `issue_storno.rs` / `issue_modification.rs` use). Avoids a
  typo class on operator-supplied secondary keys.
- **Does NOT touch `crates/nav-xsd-validator/`.** This is a
  *read* operation against a NAV response, not a render of an
  outgoing body. There is no `<InvoiceData>` / `<InvoiceAnnulment>`
  to validate; the XSD-validator allowlists apply to ABERP-
  emitted bodies, not NAV-received ones (which carry their
  own schema invariants ABERP cannot enforce).

### 2. New EventKind variant + payload struct

**One new variant:**

- `EventKind::InvoiceAnnulmentReceiverConfirmation` (storage
  form `"invoice.annulment_receiver_confirmation"`).

The `"invoice."` prefix is load-bearing for ADR-0009 §8's
per-invoice export-bundle glob — same posture every prior
lifecycle kind takes (PR-7-B-3 / PR-8 / PR-10 / PR-11 / PR-12 /
PR-13 / PR-14). The F12 four-edit ritual re-fires once for
this variant (variant + `as_str` arm + `from_storage_str` arm
+ extended `round_trip_for_every_variant` test list + one new
prefix-pinning test).

The ritual now closes cleanly the **ninth time** across
PR-6.1 / PR-7-B-3 / PR-8 / PR-10 / PR-11 / PR-12 / PR-13 /
PR-14 / PR-15; mechanical at this point.

**Why a new variant instead of reusing
`InvoiceAnnulmentAckStatus`.** Same posture as ADR-0026 §2 +
ADR-0027 §2: kind-alone classification in the audit-evidence
bundle is the load-bearing inspector-facing property. A NAV
inspector reading the per-invoice export trail sees:

```
issuance → wire submit → ack-poll
 → annulment-request → annulment-wire submit
 → annulment-ack-poll → annulment-receiver-confirmation
```

as a sequence of **distinct** kinds, not as "ack-poll, ack-
poll" requiring payload inspection to disambiguate the wire-
side observation from the receiver-side observation. The two
facts are operationally distinct (NAV-side wire-processing vs
human-pace receiver decision); the audit ledger keeps them
distinguishable.

**One new typed payload struct:**

- `audit_payloads::InvoiceAnnulmentReceiverConfirmationPayload`
  — fields: `invoice_id` (the BASE invoice id), `nav_invoice_number`
  (the NAV-facing string, e.g. `"INV-default/00042"` — recorded
  so an audit-evidence-bundle reader sees what was queried
  without re-deriving from `series.code + seq`),
  `annulment_transaction_id` (NAV's annulment-side `transactionId`
  from the prior `InvoiceAnnulmentSubmissionResponse` — pinned so
  the reader walks back to the annulment lineage by ID without
  re-walking the ledger), `annulment_idempotency_key` (F8 carry-
  forward — same posture as `poll_annulment_ack`'s entries per
  ADR-0027 §6), and `response_xml` (verbatim
  `<QueryInvoiceDataResponse>` bytes).

**Why a distinct payload type instead of reusing
`InvoiceAnnulmentAckStatusPayload`.** Structurally similar
(both carry an invoice_id + transaction_id + response_xml)
but semantically distinct: this payload has NO `ack_status`
field (per §"Surfaced conflict 3" the verbatim-bytes-only
posture means no parsed enumeration), and has TWO additional
fields the wire-poll payload does not carry
(`nav_invoice_number` because `queryInvoiceData` keys on the
invoice number not the transaction id; `annulment_transaction_id`
to anchor the back-walk per the F8 lineage). Same fork-
rationale as ADR-0026 §2 / ADR-0027 §2: the type system
enforces the kind ⇄ payload binding even when the
discriminator is correct.

### 3. New nav-transport operation: queryInvoiceData

**New nav-transport module:**
`crates/nav-transport/src/operations/query_invoice_data.rs`.

**Public surface:** `pub async fn call(transport, credentials,
tax_number_8, invoice_number, invoice_direction) -> Result<
QueryInvoiceDataOutcome, NavTransportError>` where
`invoice_direction: InvoiceDirection` is a typed enum.

```rust
pub enum InvoiceDirection {
    Outbound,  // ABERP is the supplier
    Inbound,   // ABERP is the customer (not in PR-15's path)
}
```

PR-15's call site passes `InvoiceDirection::Outbound`
explicitly — ABERP is always the supplier for invoices it
issued. The `Inbound` variant is included today because it
is part of NAV's v3.0 enumeration and a future PR
(Billingo-migrated invoice reconciliation, per the deferred
NAV historical / reconciliation read-path ADR) will use it;
holding both at variant-declaration time is the same posture
ADR-0027's `ProcessingStatus` takes (declare every NAV-side
enum value the v3.0 XSD names, parse-fail loud on unknowns).

**Return shape:** `QueryInvoiceDataOutcome { request_xml:
Vec<u8>, response_xml: Vec<u8> }` — no parsed fields per §
"Surfaced conflict 3". The verbatim bytes are the audit-
evidence per ADR-0009 §8; the binary's audit-write code path
wraps them in `InvoiceAnnulmentReceiverConfirmationPayload`.

**Error variants** in `nav_transport::error::NavTransportError`
— five new variants in the same shape as the
`queryTransactionStatus` group (group #9 per the existing
section-comment discipline):

- `QueryInvoiceDataHttp(#[source] reqwest::Error)` — transport-
  layer failure.
- `QueryInvoiceDataHttpStatus { status: u16 }` — non-success
  HTTP.
- `QueryInvoiceDataResponseParse(String)` — body parse
  failure.
- `QueryInvoiceDataNonRetryable { code: String, message: String }` —
  ADR-0009 §5 non-retryable bucket.
- `QueryInvoiceDataRetryable { code: String, message: String }` —
  ADR-0009 §5 retryable bucket.

Mapping reuses `is_non_retryable` from `operations/mod.rs` —
ADR-0009 §5's retry-classification set is operation-agnostic.
The binary's call site treats `QueryInvoiceDataRetryable` as
an operator-action-required surface (not an automatic retry)
per the one-shot posture of §"Surfaced conflict 2"; the
operator re-runs the command after the transient cause
resolves.

**New SOAP renderer:** `crates/nav-transport/src/soap/mod.rs::
render_query_invoice_data_request` — structural mirror of
`render_query_transaction_status_request` (the closest
existing template: also a non-`manageInvoice` call, also
takes a single keyed argument). The XSD-sequence body order
per NAV v3.0:

  1. `invoiceNumberQuery` — wraps:
     - `invoiceNumber` — text content (e.g.
       `"INV-default/00042"`).
     - `invoiceDirection` — `"OUTBOUND"` / `"INBOUND"` text
       content.
     - `batchIndex` — `"1"` (PR-15 single-invoice path; same
       posture as `submit-invoice`'s single-invoice batch).

Element-name verification fires at the first NAV-testbed
`observe-receiver-confirmation` run; the amendment is
mechanical (one-PR rename + the recorded verbatim response
unaffected).

**URL:** `{endpoint_base_url}/queryInvoiceData` — same
base-URL + operation-path pattern as every other operation
in the family.

**`InvoiceOperation` enum NOT extended.** This is a *read*
operation; the operation-detection logic that consumes
`InvoiceOperation::{Create, Modify, Storno}` in
`submit_invoice.rs::detect_operation_from_xml` is unrelated
and stays unchanged per CLAUDE.md rule 3.

### 4. One-shot query, not bounded-poll loop (decides §"Surfaced conflict 2")

PR-15's `observe_receiver_confirmation::run` makes ONE
`queryInvoiceData` call per invocation. No loop, no backoff
schedule, no retry-attempt cap. If the NAV call returns a
retryable error per ADR-0009 §5, the operator-visible
message names the diagnostic and exits non-zero; the operator
re-runs the command after the transient cause resolves.

**Why this differs from `poll-annulment-ack`.** ADR-0027's
poll loop targets wire-side processing that resolves in
seconds; ABERP runs five attempts with exponential backoff
on the assumption that NAV's queue will progress past
`PROCESSING` within ~15 seconds. The receiver-confirmation is
human-paced: the receiver logs into the NAV web UI on their
own schedule, which is unobservable from the supplier side.
Polling at seconds-cadence five times in a row is
guaranteed to give the same answer ("not yet confirmed")
five times in a row 99% of the time; the loop adds load
without information value. Per CLAUDE.md rule 2 (no
speculative abstractions), the right shape is one-shot.

**Per-call retry inside `queryInvoiceData::call`.** Not
shipped in PR-15. ADR-0009 §5 names the retry-classification
set; a future PR may add a one-call retry-after-transient
wrapper if the operational pattern surfaces transient
failures that would resolve in a second call. Not pre-
emptively here.

### 5. Operator-visible message names the verbatim-bytes-as-evidence (decides §"Surfaced conflict 3")

On success (HTTP 200 + `funcCode` = `OK`), the operator-visible
message names the audit-ledger entry as the load-bearing
source of truth and explicitly does NOT claim a parsed
receiver-confirmation state. The message shape (printed to
stdout + emitted via `tracing::error!` for the operator-
visible escalation, same posture as
`poll_annulment_ack`'s closing log):

> observe-receiver-confirmation OK: invoice <id> (NAV number
> <nav-number>, annulment txid <annul-txid>) -> queryInvoiceData
> returned <N> bytes (audit chain verified across <M>
> entries). NOTE: ABERP recorded the verbatim NAV response in
> the audit ledger as InvoiceAnnulmentReceiverConfirmation;
> the receiver-confirmation status field within the response
> is NOT parsed by ABERP today (per ADR-0028 §"Surfaced
> conflict 3"). To determine whether the receiver has
> confirmed the annulment, inspect the response_xml field of
> the latest InvoiceAnnulmentReceiverConfirmation audit
> entry for this invoice, OR consult the NAV web UI directly.
> A future amendment ADR will parse the receiver-confirmation
> field once NAV-testbed verification surfaces its shape.

On NAV-side `ERROR`-funcCode (non-retryable or retryable),
the message names the diagnostic and exits non-zero; the
operator re-runs after the cause resolves OR escalates
(non-retryable means operator-action-required at the
credentials/signature level per ADR-0009 §5).

On HTTP transport failure (DNS, connection reset), the
message names the diagnostic with the same retryable
posture; the operator's re-run is the unblock surface.

**Why the message names "consult the NAV web UI directly"
loud.** CLAUDE.md rule 12 — silently treating "ABERP made
the query" as "ABERP knows the answer" is exactly the
silent-omission failure mode rule 12 calls out. The audit-
evidence is the response bytes; the operator's question is
about a sub-field within those bytes that ABERP does not
parse today. Naming the NAV web UI as the alternate truth
source preserves operator agency without making ABERP
claim more than it knows.

### 6. Precondition walker

PR-15's `observe_receiver_confirmation::run` precondition
walker requires:

- At least one prior `InvoiceAnnulmentSubmissionResponse`
  audit entry against the base `invoice_id` with a non-empty
  `transaction_id`. Same precondition shape ADR-0027 §6 used
  for `poll-annulment-ack`: there must be a wire-submitted
  annulment to observe; observing receiver-confirmation of
  an annulment that was never submitted is malformed.

And **loud-rejects:**

- No prior `InvoiceAnnulmentSubmissionResponse` for this
  invoice. The named-error message explicitly steers the
  operator to run `aberp submit-annulment` first (CLAUDE.md
  rule 12).
- An empty `transaction_id` on the wire-response entry
  (defence-in-depth; same as `poll_annulment_ack`'s
  `lookup_rejects_empty_transaction_id` pin).

**Does NOT reject:**

- A prior `InvoiceAnnulmentAckStatus` of `ABORTED` (the wire
  submission was NAV-rejected). Two readings: (a) "ABORTED
  means the annulment never happened, so there is nothing
  to observe at the receiver level — reject the query as
  meaningless"; (b) "ABORTED is a wire-level fact;
  observing what NAV currently shows for the invoice's
  state is operationally useful regardless." PR-15 commits
  to (b) — the query lands the verbatim NAV-side state
  bytes in the audit ledger; the operator interprets. Same
  posture `poll_ack` takes (re-polling a Rejected invoice
  writes one more `InvoiceAckStatus` audit entry; the
  audit-evidence value of the call is what matters).
- Multiple prior `InvoiceAnnulmentReceiverConfirmation`
  entries against the same base (re-observing is idempotent
  and writes one more entry, surfacing latency-curve
  evidence per ADR-0009 §8 "every response across the
  chain").

This is deliberately narrower than
`submit_annulment::check_annulment_is_submittable` (which
default-rejects double successful wire submission). The
query is idempotent on NAV's side — repeating it twice
produces the same (or newer) state and writes one more
audit entry. No "already-observed-once-with-terminal-state"
guard is added; receiver-confirmation has no terminal-final
state from ABERP's side until the future amendment ADR
parses the field.

### 7. F8 contract for the receiver-confirmation entries

The new entries carry the **annulment-request's**
idempotency key (same as `poll-annulment-ack` per ADR-0027
§6, NOT `None` like `poll-ack`). The lookup walker returns
the key alongside the `transaction_id` in one pass over the
ledger (same shape as
`poll_annulment_ack::lookup_annulment_poll_inputs`):

  1. Walk `entries()` in reverse-seq order.
  2. Find the most-recent
     `InvoiceAnnulmentSubmissionResponse` whose
     `invoice_id` matches.
  3. Read its `transaction_id` (carried into the new audit
     payload as `annulment_transaction_id`) AND its
     `idempotency_key` (carried into the new audit payload as
     `annulment_idempotency_key` AND passed to
     `audit_ledger::append_in_tx` as the F8 lineage key).

The shared-key chain now extends end-to-end through:

```
InvoiceTechnicalAnnulmentRequested
  └─ InvoiceAnnulmentSubmissionAttempt
       └─ InvoiceAnnulmentSubmissionResponse
            └─ InvoiceAnnulmentAckStatus (one or more)
                 └─ InvoiceAnnulmentReceiverConfirmation (one or more)
```

— all five lifecycle entries share the annulment-request's
idempotency key. The audit-evidence-bundle reader walks the
chain forward or backward by that single key.

**Inline citation discipline at the audit-write site.**
`observe_receiver_confirmation::write_receiver_confirmation_audit_entry`
carries the same `Some(annulment_idempotency_key)` posture
as `poll_annulment_ack::write_annulment_ack_audit_entry`,
with the inline comment naming ADR-0028 §7 + ADR-0027 §6 +
ADR-0026 §F8. A future contributor copy-pasting from
`poll_ack` (where the posture is `None`) surfaces the
divergence via the citation rather than landing the wrong
posture silently. Same divergence-surfacing the rest of the
annulment lineage uses.

### 8. ABERP module layout — `observe_receiver_confirmation.rs`

New module:
`apps/aberp/src/observe_receiver_confirmation.rs`. Mirror of
`apps/aberp/src/poll_annulment_ack.rs`'s shape with five
deltas:

1. Calls `query_invoice_data::call` instead of
   `query_transaction_status::call`.
2. Loads the BASE invoice's billing row (via
   `billing::load_ready_invoice_by_id`) and its series row
   (via the `BillingStore::find_series_by_id` port) to
   construct the NAV-facing invoice number.
3. Writes ONE `InvoiceAnnulmentReceiverConfirmation` audit
   entry per call (one-shot per §4), NOT a per-poll-attempt
   loop.
4. Operator-visible message per §5 (verbatim-bytes-as-
   evidence).
5. No `LoopTerminus` enum, no `AttemptError` enum, no
   `poll_loop` function — the one-shot posture has no terminal
   state to classify at ABERP level until the future amendment
   ADR parses the field.

Per `feedback_rust_module_layout`, the orchestrator stays a
single file because it has one concept (one-shot query +
audit-write) and minimal internal structure. If a future
extension (e.g., adding a parsed `receiver_state` enum after
NAV-testbed surfaces the field) grows the file past comfort,
the directory-per-concept posture applies then; not pre-
emptively here per CLAUDE.md rule 2.

## Open questions

Tracked against the next fortnightly adversarial review and
named external-check items in
`docs/research/nav-and-billingo.md`:

- **The `<QueryInvoiceDataRequest>` envelope element names.**
  Default reading per §3: `invoiceNumberQuery` /
  `invoiceNumber` / `invoiceDirection` / `batchIndex`.
  Verification trigger fires on first NAV-testbed
  `observe-receiver-confirmation` run. Same pattern ADR-0026
  §3 + ADR-0027 §3 used; amendment is mechanical if NAV's
  testbed names different elements.

- **The receiver-confirmation field within the response.**
  Default reading per §"Surfaced conflict 3":
  not-parsed. Verification trigger fires on first NAV-testbed
  run with a CONFIRMED annulment in the test data. The
  amendment ADR (likely ADR-0029) adds the parsed-field
  extension once NAV's testbed reveals the actual shape.

- **The `batchIndex` value for a single-invoice query.**
  Default reading: `"1"` (same as `submit-invoice`'s single-
  invoice batch). NAV may reject `batchIndex=1` for a CREATE
  invoice that was submitted in a multi-invoice batch
  (operationally rare for ABERP — single-invoice-per-call
  is the established posture per ADR-0009 §3); the
  amendment is a per-PR widening if the operational pattern
  produces multi-invoice batches.

- **Whether `queryInvoiceData` requires
  `requestVersion=3.0` or accepts whatever ABERP sends.**
  Default per `crate::soap::parts::write_header`'s existing
  `<common:requestVersion>3.0</common:requestVersion>` —
  unchanged.

- **NAV `queryInvoiceData`-specific error codes.** None are
  conjectured here. If the testbed surfaces a code not in
  `is_non_retryable`'s allowlist (e.g.,
  `NOT_REGISTERED_INVOICE` per the conventional NAV reading),
  the amendment is a one-line addition to the shared
  allowlist.

## Consequences

**What gets easier**

- The ADR-0009 §6 design surface is now **fully closed at
  the wire AND audit-evidence levels**: an operator can
  issue end-to-end, cancel, correct, request a technical
  annulment, submit the annulment, poll for the wire ack,
  AND observe NAV-side receiver-confirmation evidence.
  The remaining ADR-0009 §6 surface is the
  semantic-interpretation layer (parsing the receiver-
  confirmation field within the response bytes — a future
  amendment ADR per §"Surfaced conflict 3"), which is a
  type-extension on the existing payload, not a new
  observation surface.
- The audit-evidence bundle (ADR-0009 §8) gains the
  receiver-confirmation leg with no schema changes to the
  per-invoice walker: the `invoice.*` glob picks up
  `invoice.annulment_receiver_confirmation` alongside every
  other lifecycle kind. A NAV inspector can now reconstruct
  the FULL annulment lineage including the supplier-side
  observation of the receiver-side decision.
- The F12 four-edit ritual closes the ninth time (PR-6.1 +
  PR-7-B-3 + PR-8 + PR-10 + PR-11 + PR-12 + PR-13 + PR-14 +
  PR-15). Mechanical and trivially auditable at this point.
- The nav-transport crate gains one new operation and one
  new error-variant group; the variant-grouping discipline
  in `error.rs` absorbs the new group cleanly as group #9
  (queryInvoiceData operation).
- The deferred NAV historical / reconciliation read-path
  ADR is now partially-grounded: one of its named
  operations (`queryInvoiceData`) ships here with a
  precedent shape (typed `InvoiceDirection` enum, verbatim-
  bytes-as-evidence posture). When the reconciliation ADR
  is filed, it inherits the precedent without re-litigating
  the operation-introduction shape.

**What gets harder**

- The CLI surface now has **thirteen** subcommands
  (issue-invoice, submit-invoice, setup-nav-credentials,
  poll-ack, retry-submission, mark-abandoned, serve,
  issue-storno, issue-modification, request-technical-
  annulment, submit-annulment, poll-annulment-ack,
  observe-receiver-confirmation). The command-group split
  per ADR-0026 §"Consequences" + ADR-0027 §"Consequences"
  remains the named future direction if operator feedback
  shows the flat list is unwieldy.
- A third `observe-*`-family or query-side module would
  re-trigger the shared-helper-extraction discussion (one-
  shot read flows are now two: this one and the future
  reconciliation-side surfaces). Not pre-emptively
  extracted here per CLAUDE.md rule 2.
- The audit-ledger schema gains one variant
  (`InvoiceAnnulmentReceiverConfirmation`). Forward-
  compatible field additions to its payload (e.g., the
  future parsed `receiver_state` enum field) are
  expected per ADR-0028 §"Surfaced conflict 3" + the
  schema-versioning convention.
- The operator-visible message carries a load-bearing
  caveat (the not-yet-parsed receiver-confirmation field).
  A future contributor removing the caveat would break
  the CLAUDE.md rule 12 invariant; the integration test
  that pins the message text (per §"Adversarial review
  #3" below) catches the regression at commit time.

**What we lock ourselves into**

- Subcommand name `aberp observe-receiver-confirmation` and
  arg names (`--invoice-id`, `--tax-number`, `--db`,
  `--tenant`, `--endpoint`). Rename requires an amendment
  ADR.
- The `InvoiceAnnulmentReceiverConfirmation` EventKind
  storage string
  (`"invoice.annulment_receiver_confirmation"`). The
  `invoice.` prefix is load-bearing for ADR-0009 §8's glob.
- The `InvoiceAnnulmentReceiverConfirmationPayload` shape.
  Schema-evolution rules apply (additive forward-compat;
  renames require new variants). The future
  parsed-`receiver_state` field lands additively per §
  "Surfaced conflict 3".
- The decision to **commit to `queryInvoiceData`** rather
  than `queryInvoiceChainDigest` or `queryInvoiceCheck`
  per §"Surfaced conflict 1". If NAV's testbed reveals
  that receiver-confirmation lives in a different operation
  entirely, the amendment ADR introduces a different
  operation (likely additive — both operations stay; the
  one we land first becomes the historic record).
- The decision to **ship one-shot, not bounded-poll** per
  §"Surfaced conflict 2". If operational feedback later
  surfaces a need for human-pace bounded retries (e.g., an
  unattended scheduled observation), the amendment ADR
  introduces a wrapping `observe-receiver-confirmation
  --schedule` flag, not a redesign of this command.
- The decision to **not parse the receiver-confirmation
  field** per §"Surfaced conflict 3". The verbatim-bytes-
  as-evidence shape is the canonical PR-15 contract;
  future parsing lands additively.
- The reuse of `is_non_retryable` from `operations/mod.rs`
  for the new operation. If NAV's `queryInvoiceData`-
  specific error codes warrant a different bucket split,
  the shared helper grows per-operation overrides (named
  trigger; not here).

## Adversarial review

A hostile NAV inspector + a hostile-engineer review,
alternating. ADR-README bar is three; four surfaced because
the queryInvoiceData choice + the verbatim-bytes-only posture
are load-bearing decisions that both diverge from prior PR
shapes.

1. **"You commit to `queryInvoiceData` for receiver-
   confirmation observation without a research-file citation
   that names it as the carrier of receiver-confirmation
   state. If NAV actually exposes receiver-confirmation only
   via `queryInvoiceChainDigest` (because annulment is a
   chain event in NAV's internal model) or via a non-public
   admin API, every PR-15 run will return invoice data with
   NO receiver-confirmation signal and operators will see
   'audit evidence recorded' messages forever without ever
   confirming anything."** The risk is real — same shape
   ADR-0026 §"Adversarial review #1" + ADR-0027 §"Adversarial
   review #1" accepted. Three mitigations:
   - The verbatim-bytes-as-evidence posture per §
     "Surfaced conflict 3" means a wrong-operation pick
     STILL records useful audit evidence — NAV's
     `queryInvoiceData` response carries the invoice data
     ABERP issued, which is itself an operator-visible
     reconciliation fact (the inspector can see ABERP
     queried, NAV returned X, the receiver-confirmation
     question is then operator interpretation).
   - The amendment surface is mechanical if NAV reveals
     the canonical operation lives elsewhere: a new
     ADR-0029 introducing the right operation, plus a new
     EventKind variant for its evidence. The
     `InvoiceAnnulmentReceiverConfirmation` kind PR-15
     pins remains valid for queryInvoiceData calls
     historically.
   - The operator-visible message names "consult the NAV
     web UI directly" per §5, so operators are NEVER
     misled into thinking ABERP knows the receiver state
     from the queryInvoiceData call alone.
   **Accepted with trigger named.**

2. **"You ship a one-shot query for what may turn out to be
   a multi-stage observation (e.g., NAV exposes 'annulment
   initiated' immediately but 'annulment confirmed' only
   after the receiver acts, AND the only signal is the
   transition between two states). An operator running
   `observe-receiver-confirmation` once and seeing
   'initiated' may never re-run, OR may re-run via a manual
   cron, but ABERP's audit-evidence bundle then has a single
   point-in-time observation that doesn't reflect later
   state changes."** Accepted, surfaced. The one-shot posture
   is the right cadence for human-pace state changes per §
   "Surfaced conflict 2"; the alternative (bounded poll
   with human-pace backoff) couples ABERP to a fixed
   schedule that the receiver does not match. The
   mitigation: an operator who wants a multi-observation
   record runs the command multiple times; each call lands
   one audit entry; the audit-evidence bundle shows the
   full timeline per ADR-0009 §8. A future
   `--watch`-flag extension that polls at a configurable
   cadence is the named trigger if operational pattern
   shows the manual re-run is too friction-laden.

3. **"Your operator-visible message names 'consult the NAV
   web UI directly' as the alternate truth source, but a
   future contributor editing the message string could
   silently drop the 'NOT parsed by ABERP today' caveat and
   the operator would interpret the 'OK' message as 'the
   receiver confirmed.'"** Accepted, surfaced. The
   mitigation: PR-15 ships an integration test that
   captures stdout/stderr and asserts the caveat substring
   is present on the OK branch. The pin is by **substring
   match** on a load-bearing fragment ("NOT parsed by
   ABERP today" or equivalent), not full-byte equality —
   a future contributor rewording the message in a way
   that PRESERVES the intent still passes; a contributor
   REMOVING the intent fails the test loud. Same
   substring-match-as-load-bearing-review-surface posture
   ADR-0027 §"Adversarial review #2" uses for the wire-
   SAVED caveat.

4. **"You're introducing the *first* `observe-*`-family
   verb without any other member to anchor the
   convention. The next future PR introducing a similar
   one-shot NAV-read may invent a different verb
   (`query-*`, `check-*`, `inspect-*`) and you end up
   with three near-synonyms across the CLI. The
   convention discipline that the `submit-*` / `poll-*` /
   `issue-*` / `request-*` families exhibit is solid
   today; PR-15 risks fragmenting it."** The risk is
   real but priced in. Two mitigations: (a) the verb
   `observe-*` is chosen precisely to NAME the one-shot
   posture (vs `poll-*` which conveys looped, vs
   `query-*` which is the NAV-side operation name and
   would conflict with the user-visible CLI). The verb
   is operator-mental-model-shaped, not NAV-API-shape-
   shaped. (b) The next `observe-*` verb is the trigger
   for a CLI-convention ADR — if a future operation
   wants a different verb, the question is forced
   into ADR form rather than silently splitting the
   family. **Accepted — forward-compatible decision
   against current operator-mental-model needs.**

## Alternatives considered

- **`queryInvoiceChainDigest` instead of `queryInvoiceData`.**
  Rejected per §"Surfaced conflict 1". The chain digest is
  scoped to amendments + stornos; annulment is operationally
  distinct per ADR-0025 §1 + ADR-0009 §6.

- **`queryInvoiceCheck` instead of `queryInvoiceData`.**
  Rejected per §"Surfaced conflict 1". Boolean existence
  cannot disambiguate the three states an operator needs
  to distinguish.

- **Reuse `InvoiceAnnulmentAckStatus` for the receiver-
  confirmation entries (no new EventKind).** Rejected per
  §2 + §"Adversarial review #4"-equivalent in ADR-0027 §
  "Adversarial review #3". Kind-alone classification at
  the audit-evidence-bundle level is load-bearing per
  ADR-0009 §8; the wire-side and receiver-side
  observations are operationally distinct facts.

- **Bounded poll loop with human-pace backoff (e.g.,
  30s/1min/5min/30min/1h).** Rejected per §"Surfaced
  conflict 2". Couples ABERP to a fixed schedule the
  receiver does not match; operator-driven re-run cadence
  is structurally correct.

- **Parse a speculative `<annulmentStatus>` /
  `<receiverConfirmationTimestamp>` / `<invoiceStatus>`
  field at PR-15 time.** Rejected per §"Surfaced conflict
  3" + CLAUDE.md rule 2. Speculative parsing of fields the
  research file does not name produces either silent-
  unknown or loud-fail-on-every-call; both are worse than
  verbatim-bytes-as-evidence + named-trigger amendment.

- **Take the NAV-facing invoice number as an operator-
  supplied `--nav-invoice-number` flag.** Rejected per
  §1's "What this command does NOT do" — operator typo
  on a denormalized secondary key is the exact class of
  silent-misroute CLAUDE.md rule 12 names. Loading from
  the billing store is one extra read against an
  already-open Connection; the cost is mechanical.

- **Default `--endpoint` to `Test`.** Rejected per
  ADR-0020 §1 — explicit per-CLI value, no hidden
  default. Same posture every other `submit-*` /
  `poll-*` command uses.

- **Bundle the future amendment ADR-0029 (parsed
  receiver-state field) into PR-15.** Rejected per §
  "Surfaced conflict 3". The amendment depends on
  NAV-testbed verification of the actual response
  shape; pre-emptively shipping a parser is the
  speculative-abstraction failure mode CLAUDE.md rule 2
  names.

## Follow-on PRs unblocked by this decision

- **PR-15 — observe-receiver-confirmation code.**
  Implements §1-§8 above plus:
  - `apps/aberp/src/observe_receiver_confirmation.rs`
    (orchestration).
  - `crates/nav-transport/src/operations/query_invoice_data.rs`
    (new NAV operation).
  - `crates/nav-transport/src/soap/mod.rs::render_query_invoice_data_request`
    (envelope renderer).
  - `crates/nav-transport/src/operations/mod.rs::InvoiceDirection`
    (new typed enum) — likely lives in the per-operation
    module rather than `mod.rs` to keep the shared
    surface narrow.
  - One new `EventKind` variant + one new payload struct
    type + F12 four-edit ritual landing.
  - Five new `NavTransportError` variants.
  - CLI `Command::ObserveReceiverConfirmation` +
    `ObserveReceiverConfirmationArgs`.

- **First NAV-testbed observe-receiver-confirmation run.**
  Verifies §3 (envelope shape) + §"Surfaced conflict 1"
  (operation choice) + §"Surfaced conflict 3" (response
  shape — the load-bearing data point for the future
  amendment ADR-0029).

- **Future amendment ADR (likely ADR-0029) — parsed
  receiver-confirmation state.** After NAV-testbed reveals
  the actual response field, the amendment adds a parsed
  `receiver_state` enum (likely values:
  `Initiated` / `Confirmed` / `Rejected` / something
  NAV-specific) to `InvoiceAnnulmentReceiverConfirmationPayload`
  and an `as_str` / `from_nav_str` round-trip pair to the
  new typed enum. Same shape PR-7-C-1 used for
  `ProcessingStatus`.

- **Future `--watch`-flag extension** (if operational
  pattern shows manual re-run is too friction-laden per §
  "Adversarial review #2"). Adds a configurable polling
  cadence; not pre-emptively shipped per CLAUDE.md
  rule 2.

- **NAV historical / reconciliation read-path ADR** (the
  deferred one per `adr/README.md` §Deferred). Inherits
  the `queryInvoiceData` operation + the
  `InvoiceDirection` typed enum + the verbatim-bytes-as-
  evidence shape from PR-15; the reconciliation surface
  adds `queryInvoiceDigest` / `queryInvoiceChainDigest` /
  `queryTransactionList` on top.

- **Per-invoice export bundle PR (gated on F5 + F10).**
  Consumes the new receiver-confirmation kind via the
  same `invoice.*` glob.
