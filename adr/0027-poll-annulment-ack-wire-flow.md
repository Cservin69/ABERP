# ADR-0027 — poll-annulment-ack wire flow — reuse queryTransactionStatus for the annulment transactionId, new wire-evidence EventKind, distinct from receiver-confirmation observation (ADR-0009 §6 + ADR-0026 closure)

- **Status:** Accepted
- **Date:** 2026-05-21
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR — closes the final
  ADR-0009 §6 observation gap (annulment-side ack poll), pairing
  with ADR-0026's wire submission. Structural parallel to ADR-0026
  (which paired manageInvoice's wire-evidence pattern to
  manageAnnulment); this ADR pairs queryTransactionStatus's poll
  pattern to the annulment transactionId. The load-bearing deltas
  are in §1 (CLI verb + arg shape), §2 (single new EventKind +
  payload — F12 four-edit ritual fires ONCE this PR, not twice),
  §3 (the surfaced conflict that REUSES queryTransactionStatus
  rather than inventing a new operation), §4 (precondition
  walker), and §5 (the explicit non-scope of receiver-side
  annulment confirmation observation). Does **not** supersede
  ADR-0009 §6, ADR-0025, or ADR-0026; all three remain in force.
- **Related:**
  - **ADR-0009 §6** (storno + modification chain + technical
    annulment + the wire observation surface — the surface this
    ADR closes the poll half of).
  - **ADR-0009 §2** (state machine — terminal `SAVED` / `ABORTED`
    semantics reused unchanged for the annulment poll).
  - **ADR-0009 §4** (NAV authentication — `queryTransactionStatus`
    uses the non-`manageInvoice` requestSignature form, same as it
    does for invoice ack polling).
  - **ADR-0009 §5** (bounded poll attempt cap + exponential
    backoff — re-used verbatim).
  - **ADR-0009 §8** (audit-evidence bundle — the new ack-status
    kind lands inside the `invoice.*` glob via the same prefix
    convention as every other lifecycle kind).
  - **ADR-0020 §1, §2** (NAV environment is explicit on the CLI;
    re-asserted for `poll-annulment-ack` per the same posture as
    `poll-ack`).
  - **ADR-0025** (technical-annulment request — PR-12 predicate;
    the operator-decision audit entry whose idempotency_key flows
    forward to PR-13's wire-evidence entries).
  - **ADR-0026** (submit-annulment wire flow — PR-13 predicate;
    its `InvoiceAnnulmentSubmissionResponse` carries the NAV-
    assigned `transaction_id` this ADR polls against).
  - Session 17 handoff F32 (queryAnnulmentStatus poll — the named
    trigger fires here; F32 closes with this PR).
  - Session 17 handoff F33 (envelope-shape verification on the
    first NAV-testbed annulment POST — unaffected by this ADR;
    fires on PR-13's path).
- **Source material:** `docs/research/nav-and-billingo.md` §"NAV
  Online Számla v3.0 — endpoints and operations" #4
  (`queryTransactionStatus`) + §"Storno and modification" clause 3
  (the receiver-confirms-in-NAV-web-UI half).

## Context

ADR-0026 + PR-13 landed the **wire submission** half of the
technical-annulment surface — `aberp submit-annulment` POSTs a
`<ManageAnnulmentRequest>` envelope to NAV's `manageAnnulment`
endpoint, persists the verbatim `request_xml` /
`response_xml` pair, and records the NAV-assigned annulment-side
`transaction_id` in
`InvoiceAnnulmentSubmissionResponsePayload.transaction_id`. The
boundary between PR-13 and this PR is the operator's next
question: **"Did NAV accept my annulment submission?"**

ADR-0026 §"Open questions" + §"Follow-on PRs unblocked"
explicitly named this as a separate PR (PR-14) and named the
trigger as "first operator request for 'is my annulment
confirmed yet?'." That trigger fires here. Session 17 handoff F32
is the index entry.

### What the research file actually says

`docs/research/nav-and-billingo.md` lists ten NAV v3.0 operations.
The **poll operation is one**: `queryTransactionStatus`
(operation #4 in the list). It is documented as "async per-
`transactionId` status query" — singular, against any
`transactionId` NAV has assigned. There is no
`queryAnnulmentStatus` listed; the research file does not name
one anywhere.

The receiver-confirmation step for technical annulments is
documented separately in §"Storno and modification" clause 3:

> Endpoint: `manageAnnulment`. Requires the issuer to mark it,
> then the receiver to confirm in the NAV web UI.

The receiver's confirmation happens **out of band** — on the NAV
web UI side, by the receiving party. ABERP cannot drive that
step. Whether it can **observe** it is a different question
(probably via `queryInvoiceData` / `queryInvoiceChainDigest` /
`queryInvoiceCheck`, none of which this PR ships); the wire-ack
of the annulment submission itself is whatever
`queryTransactionStatus` reports against the annulment
`transactionId`.

This is a surfaced conflict (§"Surfaced conflict 1" below) — and
the load-bearing decision this ADR makes. The handoff for session
17 listed PR-14 as adding
`crates/nav-transport/src/operations/query_annulment_status.rs`;
that recommendation was speculative (the file's "(Likely)"
hedge). Reading the research file confronts the speculation: NAV
v3.0 has one poll endpoint that takes any `transactionId`, and
inventing a separate Rust operation that issues a wire call NAV
does not document would be CLAUDE.md rule 2 (speculative
abstraction). This ADR commits to the evidence-backed reading.

### Surfaced conflicts (CLAUDE.md rule 7)

Three ambiguities the build phase will otherwise paper over:

1. **Whether to add a new `query_annulment_status` operation or
   reuse `query_transaction_status`.** Session 17 handoff
   recommended the former with a "(Likely)" hedge. The research
   file lists only `queryTransactionStatus` as the poll
   operation; `manageAnnulment` returns a `transactionId` (per
   ADR-0026 §3), and `queryTransactionStatus` is documented as
   "async per-`transactionId` status query." The two readings:

   - **Reuse `queryTransactionStatus`** (this ADR's pick).
     One wire call, no new Rust operation. The opaque
     `transaction_id` argument is exactly the abstraction that
     makes this work — `query_transaction_status::call` treats
     the id as opaque (per the existing module header: "Treated
     as opaque; ABERP does not parse its shape"). The response
     shape's enumeration (`RECEIVED` / `PROCESSING` / `SAVED` /
     `ABORTED`) is the same NAV state machine for any
     `transactionId`; the legal interpretation of `SAVED` differs
     (for an invoice submission: "NAV stored the invoice data";
     for an annulment submission: "NAV accepted the annulment
     request — pending receiver confirmation"), but the wire
     contract is identical.

   - **Add a new `query_annulment_status` operation.** Pre-empts
     a hypothetical future divergence where NAV adds annulment-
     specific status fields (e.g., a `<receiverConfirmed>`
     boolean inside the response). Costs one new file +
     duplicated SOAP renderer + duplicated error variants +
     duplicated test scaffolding. CLAUDE.md rule 2 says no.

   PR-14 commits to **reuse**. Rationale: every consulted source
   (research file, NAV v3.0 operation enumeration, the
   `transaction_id`-is-opaque convention `manage_annulment::call`
   already pins) backs reading A. The future-NAV-fields concern
   surfaces as a parse-time loud-fail in
   `ProcessingStatus::from_nav_str` if NAV adds an enum value
   (per `feedback_query_before_recommending` posture); the
   amendment trigger is the same name shape ADR-0009 §5 already
   uses. The kind-alone classification at the audit-ledger level
   is still preserved (§2 below: new `InvoiceAnnulmentAckStatus`
   variant), independent of which wire operation is used. Type-
   safety at the call site is preserved by a distinct orchestrator
   module (`poll_annulment_ack.rs`) — the operator path is named
   by the binary's module structure, not by a synthesized NAV
   operation that has no source-of-truth.

2. **What `SAVED` means for an annulment submission.** For an
   invoice submission, terminal-positive `SAVED` means "NAV
   stored the invoice data; legally issued and reported"
   (ADR-0009 §2). For an annulment submission, NAV's
   `queryTransactionStatus` will also report `SAVED` once the
   wire submission has been accepted — but the **receiver's
   confirmation** has not happened yet (and may never happen if
   the receiver disputes the annulment per
   `docs/research/nav-and-billingo.md` §"Storno and modification"
   clause 3). The interpretive split:

   - **Reading A (this ADR's pick):** `SAVED` on the annulment
     poll = "NAV accepted the annulment submission for
     processing; the receiver-side confirmation is asynchronous
     and observable separately." The wire-evidence is sufficient
     to record this terminal-of-the-wire state; the operator-
     visible message names the receiver-confirmation gap loud
     (CLAUDE.md rule 12).

   - **Reading B (rejected):** Defer treating `SAVED` as terminal
     until a separate receiver-confirmation poll lands. Treats
     the wire ack as intermediate-of-the-end-to-end-flow.

   Reading B confuses two distinct facts and surfaces the
   confusion as a missing terminal state. Reading A keeps the
   wire-flow's terminal/non-terminal classification clean
   (matches `poll_ack`'s semantics) and pushes the receiver-
   confirmation observation to its own future surface where it
   belongs (named trigger below).

3. **Whether to add the receiver-confirmation observation in
   this PR.** The research file is explicit that receiver
   confirmation happens in the NAV web UI; the operator can't
   drive it from ABERP. The candidate **observation** paths
   (`queryInvoiceData`, `queryInvoiceChainDigest`,
   `queryInvoiceCheck`) all exist as separate NAV operations and
   require their own SOAP renderers, audit-evidence shapes, and
   typestate transitions. Bundling any of them into PR-14 would
   add ~3x the surface area for what is by ADR-0009 §6 a
   distinct concern. **Rejected — out of scope.** PR-14 ships
   the wire-side ack poll only. The receiver-confirmation
   observation lands in its own future PR with its own predicate
   ADR; the named trigger fires on first operator request for
   "did the receiver actually confirm the annulment?"
   (operationally distinct from "did NAV accept my annulment
   submission?", which this PR answers).

## Decision

### 1. Operator CLI surface for poll-annulment-ack

**Subcommand name:** `aberp poll-annulment-ack`.

**Rationale for the verb.** Matches the existing `poll-ack`
shape one-for-one. The `poll-*` family per ADR-0026 §1 means
"query NAV for terminal state"; both members of the family now
key on a NAV-assigned `transactionId` that comes from a prior
`submit-*` call:

- `poll-ack` → polls the invoice-side `transactionId` from a
  prior `submit-invoice` (or `retry-submission`) call.
- `poll-annulment-ack` → polls the annulment-side
  `transactionId` from a prior `submit-annulment` call.

The verb-object family stays consistent; an operator reading
`aberp --help` sees a parallel pair, not a renamed sibling.

**Argument shape** (clap-flavoured) — same five fields as
`PollAckArgs`:

| Flag | Type | Default | Purpose |
|---|---|---|---|
| `--invoice-id` | `String` (`inv_<ULID>`) | none (required) | Base invoice id of the annulment. Used to look up the most-recent `InvoiceAnnulmentSubmissionResponse` audit entry whose payload carries the NAV `transaction_id`. |
| `--tax-number` | `String` | none (required) | Hungarian tax number. Same parser as `poll-ack`. |
| `--db` | `PathBuf` | `./aberp.duckdb` | Tenant DuckDB. |
| `--tenant` | `String` | `"default"` | Tenant identifier — drives the audit-ledger genesis hash and the keychain service-name lookup. |
| `--endpoint` | `NavEnv` (clap ValueEnum) | none (required) | `test` or `production`. Explicit per ADR-0020 §1; same posture as `poll-ack --endpoint`. |

**What `poll-annulment-ack` does NOT do.**

- **Does NOT call `manageAnnulment`.** This is a poll, not a
  submit; the wire submission has already happened in a prior
  `submit-annulment` run. Same posture as `poll-ack` vs
  `submit-invoice`.
- **Does NOT poll the receiver-confirmation status.** Per §3
  / §"Surfaced conflict 3", that observation is out of scope.
- **Does NOT mutate any billing row.** Annulment is not an
  invoice operation; the base invoice's typestate is unchanged
  per ADR-0025 §2 + ADR-0026 §"Open questions". The wire-ack
  result lands in the audit ledger only.
- **Does NOT extend `submit_invoice.rs::detect_operation_from_xml`.**
  Same posture as ADR-0026 §1 (no scope creep into the
  CREATE/MODIFY/STORNO detector).

### 2. New EventKind variant + payload struct

**One new variant:**

- `EventKind::InvoiceAnnulmentAckStatus` (storage form
  `"invoice.annulment_ack_status"`).

The `"invoice."` prefix is load-bearing for ADR-0009 §8's
per-invoice export-bundle glob — the same posture every prior
lifecycle kind takes (PR-7-B-3 / PR-8 / PR-10 / PR-11 / PR-12 /
PR-13). The F12 four-edit ritual re-fires once for this variant
(variant + `as_str` arm + `from_storage_str` arm + extended
`round_trip_for_every_variant` test list + one new
prefix-pinning test).

The ritual now closes cleanly the **eighth time** across PR-6.1,
PR-7-B-3, PR-8, PR-10, PR-11, PR-12, PR-13, and PR-14; mechanical
at this point.

**Why a new variant instead of reusing `InvoiceAckStatus`.**
Same posture as ADR-0026 §2 + §"Surfaced conflict 1": kind-
alone classification in the audit-evidence bundle is the load-
bearing inspector-facing property. A NAV inspector reading the
per-invoice export trail sees:

```
issuance → wire submit → ack-poll
 → annulment-request → annulment-wire submit
 → annulment-ack-poll
```

as a sequence of **distinct** kinds, not as "ack, ack" requiring
payload inspection to disambiguate. The wire endpoint reuse
(§"Surfaced conflict 1") does NOT collapse the audit-side fork
— the two facts are operationally distinct (one's the wire-ack
of an invoice-data submission, the other's the wire-ack of an
annulment submission) and the audit ledger keeps them
distinguishable.

**One new typed payload struct:**

- `audit_payloads::InvoiceAnnulmentAckStatusPayload` — fields:
  `invoice_id` (the BASE invoice id, NOT a new id),
  `transaction_id` (NAV's annulment-side tracking id from the
  prior `InvoiceAnnulmentSubmissionResponse`), `ack_status`
  (one of `"RECEIVED"` / `"PROCESSING"` / `"SAVED"` /
  `"ABORTED"` per NAV v3.0 — same enumeration as
  `InvoiceAckStatusPayload.ack_status`), and `response_xml`
  (verbatim `<QueryTransactionStatusResponse>` bytes).

**Why a distinct payload type instead of reusing
`InvoiceAckStatusPayload`.** Structurally the fields are
identical. Same fork-rationale as ADR-0026 §2 / `InvoiceStorno
IssuedPayload` vs `InvoiceModificationIssuedPayload`:
deliberately forked so the type system enforces the kind ⇄
payload binding even when the discriminator is correct. A
future audit-evidence-bundle reader that deserializes
`EventKind::InvoiceAnnulmentAckStatus` into an
`InvoiceAckStatusPayload` would not fail at runtime (the JSON
shape is the same), but the resulting code would lose the
semantic distinction that the annulment ack-status has its own
audit trail.

### 3. Reuse of queryTransactionStatus (decides §"Surfaced conflict 1")

**No new nav-transport operation.** PR-14 reuses
`aberp_nav_transport::operations::query_transaction_status::call`
verbatim. The opaque-`transaction_id` argument was designed
exactly for this (per `query_transaction_status.rs`'s module
header: "Treated as opaque; ABERP does not parse its shape").

**No new SOAP renderer.** `render_query_transaction_status_request`
is unchanged; PR-14 calls it with the annulment's
`transaction_id` (looked up from the
`InvoiceAnnulmentSubmissionResponse` audit entry per §4 below).

**No new NavTransportError variants.** The five
`QueryTransactionStatus*` variants already cover transport
failure, non-success HTTP, parse failure, and retryable /
non-retryable NAV-side errors. The same classification set
(`is_non_retryable` per `operations/mod.rs`) applies — ADR-0009
§5's retry classification is operation-agnostic.

**ProcessingStatus enum is unchanged.** The four NAV-side
values (`RECEIVED` / `PROCESSING` / `SAVED` / `ABORTED`) carry
forward unchanged. The interpretation of `SAVED` for the
annulment case is named in §"Surfaced conflict 2" + the
operator-visible message (§5 + §6 below).

**What this saves.** Comparing PR-14 against the speculative
"new operation" branch session 17's handoff floated:
- No new file under `crates/nav-transport/src/operations/`.
- No new `render_*` function.
- No new error variant group.
- No duplicated unit tests against parse-result-block /
  find-first-text shapes (those already exist for
  `queryTransactionStatus`).

The new code lands in the binary (`apps/aberp/src/`) and in the
audit-ledger crate's `EventKind` only — exactly the scope CLAUDE.md
rule 3 ("surgical changes") names.

### 4. Bounded poll loop + transaction-id lookup

**Loop shape:** identical to `poll_ack::poll_loop` per ADR-0009
§5. Max 5 attempts, exponential backoff 1s/2s/4s/8s/16s
(realized 1+2+4+8 = 15s before the final attempt; the 16s slot
is not used because the loop exits without sleeping after
attempt 5). Per-attempt commit per ADR-0009 §8 ("every response
across the chain" intent) so a crash mid-loop preserves every
completed poll's evidence.

The poll-loop module-level constants (`MAX_POLL_ATTEMPTS`,
`BACKOFF_BASE_MILLIS`) are duplicated in `poll_annulment_ack.rs`
rather than re-exported from `poll_ack.rs`. Same posture
`retry_submission.rs` takes for `parse_tax_number_8` — operator-
facing twins that may diverge in the future (e.g., a future
"poll-annulment-ack uses a larger backoff because the receiver-
side confirmation needs more time to land" amendment); a
speculative shared module would force a coupling that ADR-0009
§5 does not name.

**Transaction-id lookup** mirrors `poll_ack::lookup_transaction_id`
but reads the **annulment** wire-response entry, not the invoice
one. The walk:

1. Open `Ledger` read-only.
2. Walk `entries()` in reverse-seq order
   (`entries.iter().rev()`).
3. Filter to `EventKind::InvoiceAnnulmentSubmissionResponse`
   whose payload's `invoice_id` matches `--invoice-id`.
4. Read its `transaction_id` field.
5. If no matching entry is found: loud-fail with a message that
   explicitly steers the operator to run `aberp submit-annulment`
   first (CLAUDE.md rule 12; same shape as `poll_ack`'s no-
   submission-response error).

**Why "latest-by-seq" matters.** Per ADR-0026 §"Surfaced conflict 3",
a prior failed wire submission is permitted to be retried; multiple
`InvoiceAnnulmentSubmissionAttempt` entries with at most one
successful `InvoiceAnnulmentSubmissionResponse` is a legal trail
shape. The latest response is the operationally-current one to
poll against. If two responses exist (which the submit-annulment
precondition walker would have rejected — but the audit ledger
is append-only, so a misbehaved future writer could in principle
land them), the latest is still the right one to poll; the
precondition walker's reject is the integrity gate.

### 5. Operator-visible summary names the receiver-confirmation gap

On terminal `SAVED`, the operator-visible message must name the
**out-of-band-receiver-confirmation** half loud — CLAUDE.md
rule 12. Silently treating "NAV accepted the annulment wire
submission" as "the annulment is confirmed" is the exact
silent-omission failure mode rule 12 calls out.

The message shape (printed to stdout + emitted via
`tracing::error!` for the operator-visible escalation, same
posture as `submit_annulment.rs`'s closing log):

> poll-annulment-ack OK: invoice <id> -> NAV annulment
> transactionId <txid> reached SAVED after N polls (audit chain
> verified across M entries). NOTE: NAV-side SAVED means the
> annulment submission has been accepted for processing; the
> receiver must still confirm the annulment in the NAV web UI
> per ADR-0009 §6. ABERP does NOT yet observe receiver
> confirmation; a future query-receiver-confirmation PR will
> close that gap.

On `ABORTED` the message names that NAV rejected the annulment
itself; the operator must investigate (the receiver-confirmation
path is moot when NAV did not accept the submission to begin
with).

On `Stuck` / non-retryable / all-attempts-errored, the message
mirrors `poll_ack`'s shape with the kind label changed; the
operator-action-required posture per ADR-0009 §5 is unchanged.

### 6. Precondition walker for poll-annulment-ack

PR-14's `poll_annulment_ack::run` precondition is enforced by
the transaction-id lookup itself (§4) — the absence of an
`InvoiceAnnulmentSubmissionResponse` entry against the base
invoice id loud-fails before any NAV call. No additional walker
function is introduced beyond what `lookup_annulment_transaction_id`
already does: the precondition IS "the most-recent annulment
wire response for this invoice exists and has a non-empty
`transaction_id`."

This is deliberately narrower than `submit_annulment::check_annulment_is_submittable`
(which has a more complex retry-allowed / double-submission-
rejected matrix). The poll endpoint is idempotent on NAV's side
— polling the same `transactionId` twice is safe and produces
the same (or advancing) state — so the only loud-fail surface
is "this annulment has not yet been wire-submitted." No
"already-polled-once-with-terminal-state" guard is added; a
future operator polling a SAVED annulment again is a no-op that
writes one more audit entry. Same posture `poll_ack` takes
(re-polling a finalized invoice writes one more
`InvoiceAckStatus`; the typestate is re-confirmed terminal-
positive).

**F8 contract for the new ack-status entries.** Per the same
posture `poll_ack` uses (`apps/aberp/src/poll_ack.rs::write_ack_audit_entry`
passes `None` for the idempotency key per its inline note), the
new `InvoiceAnnulmentAckStatus` entries are written with
`Some(annulment_request_idempotency_key)` resolved from the
**same** lookup that resolved the transaction id (the wire-
response entry's `idempotency_key` field carries the annulment-
request key per ADR-0026 §"F8 contract"). The poll entries
share that key so the audit-evidence-bundle reader walks back
from any ack-poll entry to the originating
`InvoiceTechnicalAnnulmentRequested` operator-decision entry
via shared key — closing the full per-annulment audit lineage.

This is a deliberate **divergence** from `poll_ack`'s posture
(which passes `None`). Rationale: `poll_ack`'s entries anchor
on `invoice_id` + `transaction_id` because the invoice's
issuance key is already on the chain; the annulment poll's
walker would otherwise have no key carrying the annulment-
request lineage (the wire `InvoiceAnnulmentSubmissionResponse`
entry's `idempotency_key` matches the request's — preserve
that chain).

### 7. ABERP module layout — `poll_annulment_ack.rs`

New module: `apps/aberp/src/poll_annulment_ack.rs`. Mirror of
`apps/aberp/src/poll_ack.rs`'s shape with five deltas:

1. Reads `InvoiceAnnulmentSubmissionResponse` entries instead
   of `InvoiceSubmissionResponse`.
2. Writes `InvoiceAnnulmentAckStatus` entries instead of
   `InvoiceAckStatus`.
3. Passes the annulment-request idempotency key (looked up from
   the wire-response entry) on each per-poll
   `audit_ledger::append_in_tx` call (§6 above).
4. No billing-row load; no `into_submitted` typestate construct.
   The annulment is not an invoice typestate; the wire-ack
   result is purely an audit fact (same posture as
   `submit_annulment` for steps 6+).
5. Operator-visible message per §5 (names the receiver-
   confirmation gap on SAVED).

Per `feedback_rust_module_layout`, the orchestrator stays a
single file because it has one concept (the bounded poll
against the annulment transactionId) and minimal internal
structure beyond what `poll_ack.rs` already shows. If a future
extension (e.g., adding the receiver-confirmation observation
inline) grows the file past comfort, the directory-per-concept
posture applies then; not pre-emptively here per CLAUDE.md
rule 2.

## Open questions

Tracked against the next fortnightly adversarial review and
named external-check items:

- **Whether NAV's `queryTransactionStatus` response shape
  differs for annulment transactionIds.** Default reading per
  §"Surfaced conflict 1": same shape, same enumeration values.
  Verification fires on the first NAV-testbed `poll-annulment-ack`
  run. If NAV adds an enum value (e.g., `RECEIVER_CONFIRMED`),
  `ProcessingStatus::from_nav_str` loud-fails per CLAUDE.md
  rule 12 and the amendment is a one-PR enum extension + a new
  predicate ADR.
- **Whether `SAVED` for an annulment is in fact terminal at the
  audit-evidence level.** Per §"Surfaced conflict 2", PR-14
  treats `SAVED` as terminal-of-the-wire and names the receiver-
  confirmation gap loud. A future receiver-confirmation PR may
  re-classify `SAVED` as a non-terminal intermediate (the
  terminal-by-receiver state being a new `RECEIVER_CONFIRMED`
  audit fact). If that happens, this ADR's `SAVED` semantics
  carry forward unchanged at the wire level; the new fact lands
  alongside, not as a replacement.
- **Receiver-confirmation observation path.** Out of scope per
  §"Surfaced conflict 3". Candidate NAV operations:
  `queryInvoiceData`, `queryInvoiceChainDigest`,
  `queryInvoiceCheck`. The right one depends on which signal
  NAV actually exposes for "the receiver confirmed the
  annulment" — a research-file gap the next operator request
  surfaces.

## Consequences

**What gets easier**

- The ADR-0009 §6 design surface is now **fully closed at the
  wire-observation level**: an operator can issue end-to-end,
  cancel, correct, request a technical annulment, submit the
  annulment, AND poll NAV for the annulment-side ack status.
  The remaining ADR-0009 §6 surface is the receiver-confirmation
  observation (a separate future PR per §"Surfaced conflict 3"),
  which is a different fact entirely.
- The audit-evidence bundle (ADR-0009 §8) gains the annulment
  ack-status leg with no schema changes to the per-invoice
  walker: the `invoice.*` glob picks up
  `invoice.annulment_ack_status` alongside every other
  lifecycle kind. A NAV inspector can now reconstruct the
  full data-submission-withdrawal trail from the per-invoice
  export bundle, including NAV's ack on the withdrawal itself.
- The F12 four-edit ritual closes the eighth time (PR-6.1 +
  PR-7-B-3 + PR-8 + PR-10 + PR-11 + PR-12 + PR-13 + PR-14).
  Mechanical and trivially auditable at this point.
- The nav-transport crate is **unchanged at the operation level**.
  PR-14 reuses `query_transaction_status::call` directly; no
  new SOAP renderer, no new error variants, no new test
  scaffolding inside `crates/nav-transport/`. The surgical-
  changes posture (CLAUDE.md rule 3) is preserved.

**What gets harder**

- The CLI surface now has **twelve** subcommands
  (issue-invoice, submit-invoice, setup-nav-credentials,
  poll-ack, retry-submission, mark-abandoned, serve,
  issue-storno, issue-modification, request-technical-annulment,
  submit-annulment, poll-annulment-ack). The command-group
  split per ADR-0026 §"Consequences" remains the named future
  direction if operator feedback shows the flat list is
  unwieldy.
- A second poll module (`poll_annulment_ack`) duplicates the
  bounded-loop machinery from `poll_ack`. If a third poll
  surface lands (e.g., the future receiver-confirmation
  observation), the shared-loop extraction trigger fires; not
  pre-emptively here per CLAUDE.md rule 2.
- The audit-ledger schema gains one variant
  (`InvoiceAnnulmentAckStatus`). Adding a payload field is
  forward-compatible; renaming or removing requires a new
  variant per the schema-versioning convention in
  `audit_payloads.rs`.
- The operator-visible message for terminal `SAVED` carries
  a load-bearing caveat (the receiver-confirmation gap). A
  future contributor removing the caveat would break the
  CLAUDE.md rule 12 invariant; the integration test that
  pins the message text (per §"Adversarial review #4" below)
  catches the regression at commit time.

**What we lock ourselves into**

- Subcommand name `aberp poll-annulment-ack` and arg names
  (`--invoice-id`, `--tax-number`, `--db`, `--tenant`,
  `--endpoint`). Rename requires an amendment ADR.
- The `InvoiceAnnulmentAckStatus` EventKind storage string
  (`"invoice.annulment_ack_status"`). The `invoice.` prefix is
  load-bearing for ADR-0009 §8's glob.
- The `InvoiceAnnulmentAckStatusPayload` shape. Schema-
  evolution rules apply (additive forward-compat; renames
  require new variants).
- The decision to **reuse** `queryTransactionStatus` rather
  than invent a new operation. If NAV's testbed reveals
  annulment-poll-specific response fields, the amendment ADR
  introduces a new operation (and likely a new EventKind for
  any new-fact captures); this ADR's commitment to reuse holds
  for the v3.0 shape NAV documents today.
- The decision to **defer** receiver-confirmation observation
  to a future PR. The named trigger is "first operator request
  for 'did the receiver actually confirm the annulment?'"
  (operationally distinct from this PR's poll).
- The reuse of `ProcessingStatus` and its four-value
  enumeration. If NAV adds an annulment-specific status value,
  `ProcessingStatus::from_nav_str` loud-fails and the amendment
  trigger fires.

## Adversarial review

A hostile NAV inspector + a hostile-engineer review, alternating.
ADR-README bar is three; four surfaced because the
queryTransactionStatus reuse is the load-bearing decision and
the receiver-confirmation gap is operator-visible.

1. **"You commit to reusing `queryTransactionStatus` for the
   annulment poll without a NAV-testbed verification of the
   response shape. If NAV's actual response carries
   `<annulmentStatus>` instead of `<invoiceStatus>` for an
   annulment transactionId, every PR-14 poll will hit the
   `ProcessingStatus::from_nav_str` loud-fail path and operators
   will see `Stuck` for every annulment they try to track."**
   The risk is real; same shape ADR-0026 §"Adversarial review #1"
   accepted. Three mitigations:
   - The `query_transaction_status::call` path captures the
     verbatim response bytes BEFORE parsing (ADR-0009 §8 +
     `query_transaction_status.rs` step 3). Even a parse-failed
     poll leaves the verbatim NAV response in `response_xml` for
     triage; the operator can read what NAV actually returned.
   - The `ProcessingStatus::from_nav_str` loud-fail is exactly
     the right surface for a NAV-side shape change — the
     operator sees a parse error naming the unexpected
     enumeration value, NOT a silent coercion to the wrong
     terminal state.
   - The amendment, if NAV does diverge, is a one-PR addition:
     either an extra arm in `from_nav_str` (if the divergence is
     a new value) or a new operation per the §"Surfaced
     conflict 1" rejected branch (if the divergence is a new
     response shape). The audit-payload contract this PR pins
     does not depend on which branch wins; the `ack_status`
     field carries the parsed string, the `response_xml` field
     carries the verbatim bytes, and both survive a future
     amendment.
   **Accepted with trigger named.**

2. **"Your operator-visible message on terminal `SAVED` names
   the receiver-confirmation gap, but a future contributor
   editing the message string could silently drop the caveat
   and the operator would interpret SAVED as 'annulment is
   final.' The audit-ledger test does not catch text drift in
   the printed output."** Accepted, surfaced. The mitigation:
   PR-14 ships an integration test
   (`apps/aberp/tests/poll_annulment_ack_message_caveat.rs` or
   inline in the orchestrator's `mod tests`) that captures
   stdout/stderr and asserts the caveat text is present on the
   `SAVED` branch. The pin is by **substring match** on a
   load-bearing fragment ("receiver must still confirm" or
   equivalent), not full-byte equality — a future
   contributor rewording the message in a way that PRESERVES
   the intent still passes; a contributor REMOVING the intent
   fails the test loud. Same shape ADR-0026's "named-rejection-
   text-as-load-bearing-review-surface" test posture takes.

3. **"You added a new EventKind variant + payload type for
   annulment ack-status when the existing `InvoiceAckStatus`
   would have worked. You're paying the F12 ritual cost +
   audit-evidence-bundle-reader complexity for a discriminator
   that the bundle reader could derive from the linked
   wire-response entry's idempotency key."** The risk is real
   but priced in; same shape ADR-0026 §"Adversarial review #3"
   for the wire-submission split. Kind-alone classification at
   the audit-evidence-bundle level is the load-bearing
   inspector-facing property. The F12 ritual cost is
   mechanical at the eighth landing; the value is that an
   inspector reading the per-invoice trail sees distinct
   kinds, not "ack, ack, ack" requiring payload XML +
   idempotency-key chasing to disambiguate from the invoice
   poll. **Accepted — forward-compatible decision against the
   ADR-0009 §8 inspector-facing contract.**

4. **"`poll-annulment-ack` writes `InvoiceAnnulmentAckStatus`
   entries with the annulment-request's idempotency key on
   each append (§6), but `poll-ack` writes `InvoiceAckStatus`
   entries with `None`. The two operator-facing twins diverge
   silently. A future maintainer copy-pasting from `poll_ack.rs`
   into `poll_annulment_ack.rs` (or vice versa) will land the
   wrong key posture and the audit-evidence bundle will be
   inconsistent across the two flows."** Accepted, surfaced.
   The mitigation is inline-citation discipline: both modules
   carry an explicit comment naming why their idempotency-key
   posture is what it is (poll_ack: "no shared key; entries
   anchor on invoice_id + transaction_id"; poll_annulment_ack:
   "shared annulment-request key per ADR-0026 §F8 + ADR-0027
   §6 — closes the per-annulment audit lineage"). A future
   contributor changing either posture is forced to read the
   citation; the test posture (one test per module pins
   `idempotency_key` presence/absence on appended entries)
   catches accidental copy-paste regression at commit time.

## Alternatives considered

- **Add a new `query_annulment_status` nav-transport operation.**
  Rejected per §"Surfaced conflict 1" + §3. Costs new file +
  duplicated SOAP renderer + duplicated error variants;
  research file lists only one poll endpoint. CLAUDE.md rule 2.

- **Reuse `InvoiceAckStatus` for annulment-poll entries (no
  new EventKind).** Rejected per §2 + §"Adversarial review #3".
  Kind-alone classification at the audit-evidence-bundle level
  is load-bearing per ADR-0009 §8; same posture as ADR-0026 §2.

- **Bundle the receiver-confirmation observation into PR-14.**
  Rejected per §"Surfaced conflict 3". The wire-ack and the
  receiver-confirmation are operationally distinct facts;
  bundling them couples PR-14's scope to NAV operations that
  are not yet researched. CLAUDE.md rule 2 (no speculative
  abstractions; surgical changes per rule 3).

- **Treat SAVED as non-terminal pending receiver
  confirmation.** Rejected per §"Surfaced conflict 2". Confuses
  two distinct facts and creates a missing-terminal-state gap
  on the wire side. Reading A keeps the typestate semantics
  clean.

- **Default `--endpoint` to `Test` (lower blast radius).**
  Rejected per ADR-0020 §1 — explicit per-CLI value, no hidden
  default. Same posture every other `poll-*` / `submit-*`
  command uses.

- **Add an automatic retry-after-failed-poll loop wrapper.**
  Out of scope — the bounded poll loop with backoff already
  handles transient retryable errors per ADR-0009 §5. An outer
  retry wrapper (e.g., "if the entire 5-attempt sequence
  exhausts, try again after a longer backoff") would be a
  separate concern; CLAUDE.md rule 2 says no until the
  operational pattern names it.

- **Share the poll-loop machinery between `poll_ack` and
  `poll_annulment_ack` via a generic helper.** Rejected per
  CLAUDE.md rule 2. The two flows are operator-facing twins
  today; a speculative generic helper would couple the two
  operationally-distinct surfaces. If a third poll surface
  lands (receiver-confirmation observation), the trigger fires;
  not pre-emptively here.

## Follow-on PRs unblocked by this decision

- **PR-14 — poll-annulment-ack code.** Implements §1-§7 above
  plus:
  - `apps/aberp/src/poll_annulment_ack.rs` (orchestration).
  - One new `EventKind` variant + one new payload struct type
    + F12 four-edit ritual landing.
  - CLI `Command::PollAnnulmentAck` + `PollAnnulmentAckArgs`.
  - No changes inside `crates/nav-transport/`.

- **First NAV-testbed annulment poll.** Verifies §"Surfaced
  conflict 1" (response shape) + §"Surfaced conflict 2"
  (SAVED semantics for annulments).

- **Receiver-confirmation observation PR (future).** Adds the
  `queryInvoiceData` / `queryInvoiceChainDigest` /
  `queryInvoiceCheck` operation that observes the NAV-web-UI-
  side receiver confirmation. New predicate ADR; new EventKind
  variant; new orchestrator. Trigger: first operator request
  for "did the receiver actually confirm the annulment?"
  (operationally distinct from this PR's poll). Closes the
  final ADR-0009 §6 observation gap.

- **Per-invoice export bundle PR (gated on F5 + F10).**
  Consumes the new ack-status kind via the same `invoice.*`
  glob.

- **Operator unblock for an annulment-side stuck poll.** If a
  future operational pattern surfaces `poll-annulment-ack`
  reaching `Stuck` and the operator needs a re-poll surface
  beyond simply re-running the command, the named trigger
  fires for a `retry-annulment-poll` subcommand. Not
  pre-emptively here.
