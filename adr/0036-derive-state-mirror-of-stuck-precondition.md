# ADR-0036 — `serve.rs::derive_state` mirrors `audit_query::stuck_precondition` at the UI label level — closes F21 + F47 (joint) and surfaces the post-PR-10 / PR-19 / PR-20 / PR-21 lifecycle labels (Storno, Amended, Pending, PendingNavExists, Recovered) on the loopback HTTPS API

- **Status:** Accepted
- **Date:** 2026-05-22
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR — first
  read-side-of-the-UI PR after the operator-driven
  audit-evidence flow closed end-to-end at PR-22 (issue →
  check → drain → retry → recover → export → verify). Closes
  findings F21 (`serve.rs::derive_state` needs Storno +
  Amended + state-2 Pending + AttemptFailed + state-2 +
  Exists + recovered-Response labels) and F47
  (`serve.rs::derive_state` extension — same surface, named
  separately in the session-19 fortnightly adversarial review
  for the post-PR-20 state-2 + Exists + post-PR-21
  recovered-Response sub-states). PR-23 is the smallest-scope
  substantive PR available after PR-22: no new NAV operations,
  no new `EventKind` variant, no new audit payload, no new
  CLI subcommand, no Svelte shell change. The UI classifier
  becomes a verbatim mirror of the authoritative classifier
  that PR-19 / PR-20 / PR-21 already pinned at the
  command-side. Load-bearing deltas: §1 (the mirror
  invariant — the UI classifier MUST agree with the
  authoritative one on every state both can name), §2 (the
  full label set — eleven labels: Unknown, Ready, Pending,
  PendingNavExists, Submitted, Recovered, Finalized,
  Rejected, Storno, Amended, Abandoned), §3 (priority
  ordering — what wins when two facts about the same invoice
  classify it differently), §4 (chain-link detection for
  Storno / Amended via the per-payload `base_invoice_id`
  field — no payload change), §5 (recovered-Response
  detection via response_xml root-element prefix-match per
  ADR-0035 §4 / A91 — no payload change), §6 (Layer-2
  exists sub-label per ADR-0033 §6 — informational only at
  the UI level too), §7 (Svelte shell defers to a future PR
  per CLAUDE.md rule 3), §8 (test posture — one
  parameterized expected-label table per the session-26
  handoff lean), §9 (wire-shape preservation — `state` stays
  `&'static str` on `InvoiceListItem` + `InvoiceDetailResponse`),
  §10 (future-EventKind-extension obligation — adding an
  EventKind variant that drives a new label means a
  coordinated edit on derive_state AND the parameterized
  table). Does **not** supersede ADR-0008, ADR-0009 §2,
  ADR-0021, ADR-0032 §4, ADR-0033 §6, ADR-0034 §4, or
  ADR-0035 §4; all remain in force.
- **Related:**
  - **ADR-0009 §2** — the typestate enum the UI classifier
    labels against. PR-23 surfaces eight of the eleven
    typestate states at the UI; the three missing
    (`Draft`, `Voided`, `AckPending`) are either
    pre-issue-time invisible to the audit ledger
    (`Draft` collapses into `Ready` at the
    `InvoiceSequenceReserved`/`InvoiceDraftCreated` pair
    that the allocator writes in one transaction;
    `Voided` has no EventKind yet per the event_kind.rs
    "remaining invoice-lifecycle kinds" comment) or
    intentionally collapsed (`AckPending` collapses into
    `Submitted` — the UI label is coarser; the CLI's
    `aberp poll-ack` surface remains the authoritative
    per-poll view).
  - **ADR-0021 §A12** — the canonical encoding lives in
    one place. PR-23 does NOT introduce a second canonical
    encoder; the UI classifier reads the typed audit
    payloads via the existing `audit_payloads::*` types
    that the command-side already uses. No round-trip
    duplication.
  - **ADR-0032 §4** — the state-2 Pending classification
    contract `audit_query::stuck_precondition` honours.
    PR-23 surfaces state-2 Pending as the new `Pending`
    UI label.
  - **ADR-0033 §6** — Layer-2 `queryInvoiceCheck` is
    informational-only at the command-side classifier
    (`stuck_precondition` does NOT consult
    `InvoiceCheckPerformed` entries). PR-23 surfaces the
    informational signal at the UI label level as a
    distinct `PendingNavExists` label per §6 below — the
    operator wants to know NAV has the invoice; the
    command-side classifier's "informational" posture means
    the audit ledger carries the evidence but the
    classification stays Pending. The UI label exposes
    both facts: state-2 Pending + NAV-side Exists.
  - **ADR-0034 §4** — the recovered Response uses the
    existing `InvoiceSubmissionResponsePayload` shape with
    `<QueryInvoiceDataResponse>` bytes in `response_xml`
    (rather than `<ManageInvoiceResponse>`). PR-23
    surfaces the distinction at the UI label level as a
    distinct `Recovered` label per §5 below — same
    discriminator the verifier uses per A91.
  - **ADR-0035 §4 + A91** — the verifier's two-root-element
    acceptance + prefix-match local-name detection. PR-23
    mirrors A91's prefix-match at the UI to detect
    recovered Responses (substring scan for
    `QueryInvoiceDataResponse` in `response_xml`).
    Re-asserts ADR-0034 §4's chain-walk-by-order posture
    at the UI level: the byte-level signal is the
    discriminator, not the preceding entry kind.
  - **PR-9-1 (session 9)** — the loopback HTTPS surface
    `derive_state` lives on. PR-23 touches
    `apps/aberp/src/serve.rs` ONLY; every other PR-9-1
    seam (cert / key persistence, bearer-token auth,
    rcgen, tokio runtime) is unchanged.
- **Source material:** ADR-0009 §2 (typestate enum) +
  `audit_query::stuck_precondition` (the authoritative
  classifier) + ADR-0032 §4 + ADR-0033 §6 + ADR-0034 §4 +
  ADR-0035 §4 / A91.

## Context

PR-9-1 (session 9, PR-9-1-commit-message.txt) shipped the
loopback HTTPS API. Its `derive_state` walker classified
every invoice into one of six labels: `Unknown`, `Ready`,
`Submitted`, `Finalized`, `Rejected`, `Abandoned`. That
label set was complete for the post-PR-7-C / PR-8
state-machine surface (Draft → Ready → Submitted →
AckPending → Finalized | Rejected | Abandoned).

Five lifecycle PRs since then introduced new states the
authoritative classifier (`audit_query::stuck_precondition`)
knows about but the UI classifier does NOT:

- **PR-10 (ADR-0023, storno)** + **PR-11 (ADR-0024,
  modification)** added `InvoiceStornoIssued` +
  `InvoiceModificationIssued` chain-link entries that
  carry a `base_invoice_id` field pointing at the base
  invoice. The base invoice's typestate transition
  (`Finalized → Storno` or `Finalized → Amended`) is
  derived from the existence of this entry per ADR-0009
  §2. `derive_state` does not detect this transition
  today; the base invoice continues to display as
  `Finalized` after a chain-link entry is written.
- **PR-19 (ADR-0032 §4)** added the state-2 Pending
  classification: `InvoiceSubmissionAttempt` exists, no
  `InvoiceSubmissionResponse`, no `InvoiceMarkedAbandoned`.
  `audit_query::stuck_precondition` returns
  `Stuck(StuckStage::Pending)` for this; `derive_state`
  returns the misleading `Ready` (the `has_draft` short-
  circuit fires because the Attempt does NOT flip
  `has_submission_response`).
- **PR-20 (ADR-0033 §6)** added `InvoiceCheckPerformed`
  as the Layer-2 disambiguation surface. An invoice in
  state-2 Pending with the latest `InvoiceCheckPerformed`
  outcome `"exists"` is operator-relevant: NAV has the
  invoice; the operator should not re-POST without
  recovering. `audit_query::stuck_precondition` deliberately
  classifies this as Pending (Layer-2 is informational
  per ADR-0033 §6); the operator-visible message names
  the divergence loud. `derive_state` does not surface
  the divergence at the label level.
- **PR-21 (ADR-0034 §4)** added the recovered Response
  path: `recover-from-nav` writes ONE
  `InvoiceSubmissionResponse` entry carrying
  `<QueryInvoiceDataResponse>` bytes (rather than
  `<ManageInvoiceResponse>`). The state-3 invoice is
  authoritatively reconstructed but the audit-evidence
  bundle reader's chain walk distinguishes the recovered
  path from the originally-witnessed path by both the
  preceding entry kind AND the response_xml root element
  per ADR-0034 §4. `derive_state` collapses both into
  `Submitted`; the operator cannot see at the list level
  which invoices were reconstructed from NAV's side.

Five accumulated PRs of lifecycle state, zero UI label
extension. The reason — PR-10 / PR-11 / PR-19 / PR-20 /
PR-21 were each surgical to their NAV-or-allocator path
per CLAUDE.md rule 3; mixing the UI label set into each
would have inflated the PR scope. PR-23 collects the five
in one UI-side closure.

### Prerequisite-gate state at PR-23 time

- **F21 trigger** has been the joint-named carry-forward
  finding since PR-10 (session 12 handoff). Re-asserted
  in every session-19+ handoff under "F21 UNCHANGED — open."
- **F47 trigger** named the state-2 + Exists + recovered-
  Response label visibility specifically. Re-asserted in
  session-25 / PR-21 handoff + session-26 / PR-22 handoff.
- The `stuck_precondition` classifier is the
  authoritative source: PR-19 / PR-20 / PR-21 each pinned
  its decision matrix with parameterized tests
  (`pending_when_attempt_exists_and_no_response`,
  `check_performed_exists_does_not_change_state_2_classification`,
  etc.). PR-23 reuses that authority by mirroring its
  output at the UI label level rather than re-implementing
  the rules.
- No new `EventKind` variant lands in PR-23. F12
  four-edit ritual does NOT fire. Eleven landings of the
  ritual through PR-20 remain the count through PR-23.
- No new audit payload. ADR-0021 §A12 PRESERVED.
- No new CLI subcommand. No new NAV operation. No
  workspace dependency change.

### What surfaced during PR-23 design

The two design choices that actually had to be decided
(everything else was forced by the related ADRs):

1. **Should `PendingNavExists` and `Recovered` be distinct
   labels, or just sub-state badges on the existing
   `Pending` / `Submitted` labels?** Distinct labels per
   §6 / §5 below — the operator-visible signal is
   different enough to warrant a separate string. A future
   Svelte shell PR can render colour or icon variations on
   the distinct labels; a sub-state badge would require
   the Svelte side to understand both the label AND the
   badge to render correctly, which violates the
   single-source-of-truth posture this ADR pins.

2. **What signal detects a recovered Response?** ADR-0034
   §4 names two: (a) preceding-entry kind
   (`InvoiceCheckPerformed` vs `InvoiceRetryRequested` /
   `InvoiceSubmissionAttempt`), or (b) response_xml root
   element (`<QueryInvoiceDataResponse>` vs
   `<ManageInvoiceResponse>`). PR-23 uses (b) per §5 below
   to mirror ADR-0035 §4 / A91's verifier discriminator.

## Decision

### 1. The mirror invariant

`apps/aberp/src/serve.rs::InvoiceTrace::derive_state` is
the **UI-side mirror** of
`apps/aberp/src/audit_query::stuck_precondition`. For
every classification both can name, the two MUST agree.
The UI classifier is strictly an extension: it surfaces
states `stuck_precondition` does NOT classify (Storno,
Amended — terminal-by-chain-link; PendingNavExists — a
sub-label of state-2 that `stuck_precondition` collapses
into Pending per ADR-0033 §6; Recovered — a sub-label of
state-3 that `stuck_precondition` collapses into
AwaitingAck).

`stuck_precondition` remains the authoritative classifier
for command-side decisions (`retry-submission`,
`mark-abandoned`, `recover-from-nav`). PR-23 does NOT
touch `audit_query.rs`. The UI mirror is a read-only
extension over the same audit ledger.

The mirror invariant is enforced by the parameterized
table tests per §8 below: for every input ledger shape
the `audit_query` test module pins
`stuck_precondition`'s output against, the same input is
also fed to `derive_state` and the expected label is
pinned. Drift between the two surfaces (e.g., the UI
labels state-2 as `Ready` while `stuck_precondition`
returns `Stuck(Pending)`) fails the test loud per
CLAUDE.md rule 12.

### 2. The full label set

Eleven labels, each a `&'static str` that the JSON
serialiser emits as-is. The wire shape on
`InvoiceListItem.state` and `InvoiceDetailResponse.state`
is unchanged (still `&'static str` per §9 below).

| Label | When it fires | Mirrors |
|---|---|---|
| `Unknown` | No entries for this invoice id in the audit ledger. | (none — pre-issue-time) |
| `Ready` | `InvoiceDraftCreated` exists; no `InvoiceSubmissionAttempt`, no `InvoiceSubmissionResponse`, no chain-link, no abandon. | (none — pre-submission) |
| `Pending` | `InvoiceSubmissionAttempt` exists; no `InvoiceSubmissionResponse`; no `InvoiceMarkedAbandoned`; no `InvoiceCheckPerformed(outcome=exists)` for this invoice. | `stuck_precondition` → `Stuck(StuckStage::Pending)` (ADR-0032 §4) |
| `PendingNavExists` | Same precondition as `Pending` AND the most-recent `InvoiceCheckPerformed` for this invoice has `outcome=exists`. | `stuck_precondition` → `Stuck(StuckStage::Pending)` (ADR-0033 §6 — informational-only at command-side; UI surfaces the divergence loud) |
| `Submitted` | `InvoiceSubmissionResponse` exists; no terminal ack (`SAVED`/`ABORTED`); no abandon; not recovered. | `stuck_precondition` → `Stuck(StuckStage::AwaitingAck)` (ADR-0009 §5) |
| `Recovered` | Same precondition as `Submitted` AND the most-recent `InvoiceSubmissionResponse`'s `response_xml` carries `<QueryInvoiceDataResponse>` bytes (prefix-match per §5 below). | `stuck_precondition` → `Stuck(StuckStage::AwaitingAck)` (ADR-0034 §4 — same classification at command-side; UI surfaces the recovered-vs-original distinction) |
| `Finalized` | Most-recent `InvoiceAckStatus` is `"SAVED"`; no abandon; not a chain-link base. | `stuck_precondition` → `NotStuck(AlreadyFinalized)` |
| `Rejected` | Most-recent `InvoiceAckStatus` is `"ABORTED"`; no abandon; not a chain-link base. | `stuck_precondition` → `NotStuck(AlreadyRejected)` |
| `Storno` | An `InvoiceStornoIssued` entry exists whose `base_invoice_id` equals this invoice's id. | (none — chain-link-derived per ADR-0009 §2) |
| `Amended` | An `InvoiceModificationIssued` entry exists whose `base_invoice_id` equals this invoice's id. | (none — chain-link-derived per ADR-0009 §2) |
| `Abandoned` | `InvoiceMarkedAbandoned` exists. | `stuck_precondition` → `NotStuck(AlreadyAbandoned)` |

Labels NOT introduced (deliberate per CLAUDE.md rule 2):

- **`Voided`** — no `EventKind::InvoiceVoided` variant
  exists yet (per the event_kind.rs "remaining
  invoice-lifecycle kinds" comment). When the voided-
  before-submission surface lands, the UI label extends
  additively.
- **`AnnulmentRequested`** — annulment is a NAV-side
  data withdrawal, not a state-transition on the base
  invoice (ADR-0025 §"Decision" §2). The base's
  Finalized / Rejected / Storno / Amended label is
  unchanged by the annulment-request lifecycle. The
  per-invoice detail view exposes annulment entries via
  the `audit_entries` array; the list-level label stays
  the base's terminal state.

### 3. Priority ordering

The walker collects facts in one pass over the ledger,
then emits the label by walking the ladder below from top
to bottom. The first match wins. The ordering matches the
authoritative classifier's "Abandoned wins over
everything else" posture (ADR-0032 §4 step 1 +
`already_abandoned_overrides_pending_state_2` test) and
the storno/amended terminal precedence:

```
1. Abandoned             (matches stuck_precondition step 1)
2. Storno                (chain-link evidence; ADR-0009 §2)
3. Amended               (chain-link evidence; ADR-0009 §2)
4. Finalized             (matches stuck_precondition AlreadyFinalized)
5. Rejected              (matches stuck_precondition AlreadyRejected)
6. Recovered             (sub-label of Submitted; ADR-0034 §4)
7. Submitted             (matches stuck_precondition AwaitingAck)
8. PendingNavExists      (sub-label of Pending; ADR-0033 §6)
9. Pending               (matches stuck_precondition Pending; ADR-0032 §4)
10. Ready                 (Draft entered the ledger; no submit yet)
11. Unknown               (no entries for this invoice)
```

`Storno` / `Amended` lose to `Abandoned` deliberately —
mirrors `already_abandoned_overrides_pending_state_2`'s
terminal-by-operator-decision posture. In practice the
two should not co-occur (a `Finalized → Abandoned`
transition does not happen — abandon is for state-2 /
state-3 only); if the audit ledger ever carries both,
the UI surfaces `Abandoned` and the audit-entries view
exposes the chain-link evidence to the operator.

### 4. Chain-link detection (Storno / Amended)

`InvoiceStornoIssuedPayload` and
`InvoiceModificationIssuedPayload` do NOT carry a top-
level `invoice_id` field. They carry
`storno_invoice_id` / `modification_invoice_id` (the
chain invoice's own id) AND `base_invoice_id` (the
chain link's anchor).

The existing `extract_invoice_id` probe — a serde
deserialise that looks for a top-level `invoice_id`
field — returns `None` for these entries. The storno /
modification invoice's OWN appearance in the per-invoice
trace map comes from its own `InvoiceSequenceReserved` +
`InvoiceDraftCreated` pair (both carry `invoice_id =
storno_invoice_id` / `= modification_invoice_id`).

PR-23 adds a sister helper `extract_chain_base_link(entry)
→ Option<(EventKind, String)>` that returns
`Some((kind, base_invoice_id))` for the two chain-link
kinds and `None` for every other kind. The list-and-
detail walkers call it alongside `extract_invoice_id`
and tag the BASE invoice's trace with
`is_storno_base = true` / `is_amended_base = true`.

The probe path is narrow on purpose per CLAUDE.md rule 2:
no new payload type, no serde dispatch table, no
trait. Two specific kinds, two specific field names, one
new helper.

### 5. Recovered-Response detection

The recovered `InvoiceSubmissionResponse` per ADR-0034 §4
reuses the existing `InvoiceSubmissionResponsePayload`
shape with `<QueryInvoiceDataResponse>` bytes in
`response_xml` (rather than `<ManageInvoiceResponse>`).
The discriminator is the response_xml root element local
name; ADR-0035 §4 + A91 pins this at Reading A for the
verifier with a substring prefix-match.

PR-23 mirrors the verifier's prefix-match at the UI:

```rust
fn response_xml_is_recovered(response_xml: &[u8]) -> bool {
    let prefix_window = &response_xml[..response_xml.len().min(512)];
    // Local-name match per ADR-0035 §4 A91 — matches
    // both <QueryInvoiceDataResponse> and namespaced
    // forms like <ns0:QueryInvoiceDataResponse>.
    twoway_find(prefix_window, b"QueryInvoiceDataResponse").is_some()
}
```

The window is 512 bytes to bound the search; the root
element appears in the first ~200 bytes on every NAV-
emitted body (XML prolog + optional whitespace +
optional namespace-prefixed open tag). A future PR may
swap the substring scan for the verifier's exact
prefix-match helper if the audit-ledger or verifier
crate publishes one as a `pub` function; today the
substring scan keeps serve.rs's dep surface narrow (no
new dep on quick-xml or aberp-verify).

The walker tracks the most-recent
`InvoiceSubmissionResponse`'s `response_xml`'s
recovered-or-not bit. The state-3 ladder branches on
`Recovered` first, then `Submitted` per §3. If the
operator runs `recover-from-nav` on an invoice that
previously had an originally-witnessed Response (a
state-3 invoice that re-witnessed via recover-from-nav
— which the ADR-0034 §5 precondition guard prevents in
practice but the UI walker handles), the MOST-RECENT
Response's discriminator wins.

### 6. Layer-2 exists sub-label (PendingNavExists)

ADR-0033 §6 names Layer-2 entries as informational-only
at the command-side classifier. The UI label level
surfaces the divergence: an invoice with
`InvoiceSubmissionAttempt` + `InvoiceCheckPerformed(outcome=exists)`
+ no `InvoiceSubmissionResponse` + no
`InvoiceMarkedAbandoned` is labelled
`PendingNavExists` rather than `Pending`.

The walker tracks the most-recent
`InvoiceCheckPerformed`'s `outcome` for each invoice.
On state-2 classification, the ladder branches on
`PendingNavExists` first if the outcome is `"exists"`,
then `Pending` otherwise. `"absent"` outcomes (the
Layer-2 check confirmed NAV does NOT have it; the
retry orchestration proceeded to re-POST and either
succeeded — Response exists, state-3 — or failed —
AttemptFailed, classified state-2 by the precondition
walker) leave the label as `Pending` per
`check_performed_absent_does_not_change_state_2_classification`'s
mirror obligation.

`"failure"` outcomes (the Layer-2 check itself failed)
also leave the label as `Pending` — the operator's
next move per ADR-0033 §"Surfaced conflict 5" is to
re-run `retry-submission`, which produces a fresh
`InvoiceCheckPerformed` and re-classifies. The UI
label does not surface the failure class explicitly;
the audit-entries view exposes the per-poll failure
class.

The `PendingNavExists` label is deliberately distinct
from `Pending` (not a sub-state badge per §"What
surfaced" #1). The operator-visible signal is
qualitatively different: `Pending` means the operator
should consider `retry-submission`; `PendingNavExists`
means the operator should consider `recover-from-nav`
(the post-PR-21 affirmative path) — distinct command
recommendations warrant distinct labels.

### 7. Svelte shell — deferred to a future PR

PR-23 lands the Rust-side label set on the loopback
HTTPS API. The Svelte shell (PR-9-2) consumes
`InvoiceListItem.state` as a string. The new labels
will render as-is (no Svelte-side parse logic exists
today); a future PR adds Svelte-side display affordances
(colour, icon, sort order, filter dropdown) for the new
labels.

The split is deliberate per CLAUDE.md rule 3: mixing
Rust + Svelte changes in a single PR violates the
surgical-changes posture. A NAV inspector reading the
loopback API directly (curl, Postman) sees the full new
label set immediately; an operator using the Svelte
shell sees the labels rendered as plain strings until
the future Svelte PR adds display affordances.

### 8. Test posture — parameterized expected-label table

The session-26 handoff named two test-posture options:
"unit test per label" vs "parameterized table." PR-23
chooses parameterized table per the session-26 lean:
symmetry with the existing `stuck_precondition` pin
tests (which use named per-scenario tests but share
helper writers — the ledger-fixture pattern is the
parameterization).

Concretely PR-23 ships:

- **A parameterized table** `[(scenario_name,
  ledger_setup_fn, expected_label)]` in a new
  `derive_state_label_table_mirror` test in `mod tests`
  of `serve.rs`. Eleven scenarios, one per label. Each
  builds an in-memory ledger via the same writer
  helpers `audit_query.rs::tests` uses (the helpers
  are re-exported from `audit_query.rs` via a
  `#[cfg(test)] pub(crate)` surface so PR-23 doesn't
  duplicate them — see §"Adversarial review" #1 below
  for the alternative considered).
- **A mirror-invariant cross-check** test
  `derive_state_agrees_with_stuck_precondition_on_overlapping_states`
  that walks the seven mirror-able states (Pending,
  Submitted, Finalized, Rejected, Abandoned —
  Pending and Submitted both classify the same way at
  `stuck_precondition` and `derive_state` modulo the
  PendingNavExists / Recovered sub-labels) and asserts
  the two surfaces agree.
- **Per-label scenario tests** for the four UI-only
  labels (Storno, Amended, PendingNavExists,
  Recovered, plus the pre-mirror Unknown / Ready) that
  `stuck_precondition` does NOT name — covered by the
  parameterized table.

The existing `derive_state_ladder` ladder test in
`mod tests` stays (it pins the pre-PR-23 label set; its
seven assertions remain valid because PR-23 does not
remove any label).

### 9. Wire-shape preservation

`InvoiceListItem.state` and `InvoiceDetailResponse.state`
remain `&'static str` (no JSON shape change). The new
labels are added as additional return values of
`derive_state`; consumers that hard-coded the pre-PR-23
six-label set will simply see new strings they did not
expect (per the typical loose-JSON consumer posture).
The Tauri / Svelte shell currently renders the string
directly, so the new labels appear as plain text
without affordance.

Per CLAUDE.md rule 3, no migration of the wire shape to
an enum or string-enum-with-help is included here.
That's a future PR with its own ADR if it surfaces as
an operational concern.

#### 9.a Wire-shape tightening — LIFTED in PR-28

The "future PR" named above fired in PR-28 (session 32).
Trigger: four SPA surfaces (PR-24 + PR-25 + PR-26 + PR-27)
now consume typed wire fields with TS exhaustiveness pins,
and the SPA's `Record<InvoiceState, LabelMeta>` table in
`labels.ts` makes drift between the Rust ladder and the SPA
union expensive to discover (an unknown string falls back
to a muted "?" pill — visible per CLAUDE.md rule 12 but
not loud at build time on the Rust side).

PR-28 replaces the `&'static str` return type with a typed
fieldless `InvoiceState` enum in `apps/aberp/src/serve.rs`,
mirroring the SPA union member-for-member. The enum derives
`Serialize`; serde emits each variant identifier verbatim
as a JSON string, so the wire shape is byte-identical to
the pre-PR-28 emission (a `Submitted` Rust value still
serialises as `"Submitted"`). The SPA's TS string-union
stays hand-typed (compile-checked via `npm run check`) per
this ADR's §"Alternatives considered" lean — the Rust-emitted
schema path was not justified for a four-interface surface.

PR-28 pins the wire contract with a new Rust-side test
`invoice_state_wire_shape_pins_pascalcase_strings` that
serialises each of the eleven variants and asserts the
resulting JSON string equals the SPA's expected literal.
A `#[serde(rename_all = ...)]` accidentally added to the
enum, or a typo at either end, fires that test loud per
CLAUDE.md rule 12.

The §10 four-edit obligation gains a fifth edit on a new
label (the `InvoiceState` enum); §10 below is updated to
name the obligation explicitly.

### 10. Future-EventKind-extension obligation

Adding an `EventKind` variant that classifies into a
new UI label requires six coordinated edits per the
F12-ritual-style discipline this ADR carries forward
(post-PR-28; the pre-PR-28 four-edit form is preserved
in the bullet trail below):

1. Extend `InvoiceTrace`'s field set with the new fact.
2. Extend `merge_entry`'s match arm with the new variant.
3. Extend the `derive_state` ladder with the new label.
4. Extend `derive_state_label_table_mirror`'s table with
   one new row per new label.
5. **PR-28 / §9.a obligation** — extend the
   `serve::InvoiceState` enum with the new variant.
   The new variant's wire form is the variant name
   verbatim (no `#[serde(rename = ...)]`); the SPA's
   `InvoiceState` union and `LABELS` table MUST gain a
   matching member in lockstep or `npm run check` fires
   loud at the SPA side.
6. **PR-28 / §9.a obligation** — extend the
   `invoice_state_wire_shape_pins_pascalcase_strings`
   test's case array with one new row per new variant
   so the JSON contract stays under pin coverage.

These six are NOT enforced at compile time by an
exhaustive match — `merge_entry`'s `_ => {}` arm catches
unhandled variants silently. Per CLAUDE.md rule 12 the
silent miss is the failure mode; the parameterized
table is the loud catch. A future PR may extend
`merge_entry` to an exhaustive match against `EventKind`
to force the compiler to fire on the unhandled-variant
case (mirrors ADR-0035 §"Adversarial review" #2's
`extract_nav_xml_handles_every_known_event_kind`
canary).

## Consequences

**Positive**

- The UI label surface is now the verbatim mirror of
  the authoritative classifier `audit_query::stuck_precondition`
  for every state both can name. A NAV inspector
  loading the loopback API sees the same lifecycle
  state the operator sees on the CLI; the silent
  divergence that has been accumulating since PR-10 is
  closed.
- Two new UI labels (`PendingNavExists`, `Recovered`)
  surface the post-PR-20 / PR-21 NAV-side facts at the
  UI level. The operator can scan the invoice list and
  see at a glance which Pending invoices have a
  NAV-side Exists check (recommending `recover-from-nav`)
  and which Submitted invoices were reconstructed via
  `recover-from-nav` (audit-evidence transparency per
  ADR-0034 §4).
- Two new UI labels (`Storno`, `Amended`) surface the
  base-invoice terminal states that ADR-0009 §2 named
  but the UI never displayed. A NAV inspector can scan
  the invoice list and see which Finalized invoices
  have been stornod or amended; the per-invoice detail
  view exposes the chain-link entries via the existing
  `audit_entries` array.
- The mirror invariant is enforced by the parameterized
  table tests at compile / test time. A future PR that
  re-introduces drift (the UI labels state-2 as
  `Submitted` while `stuck_precondition` returns
  `Stuck(Pending)`) fails the test loud per CLAUDE.md
  rule 12.
- No new `EventKind` variant. F12 four-edit ritual does
  NOT fire. Eleven landings through PR-20 remain the
  count through PR-23.
- No new audit payload. ADR-0021 §A12 PRESERVED.
- No new CLI subcommand. No new NAV operation. No new
  workspace dependency. Surgical changes posture
  preserved (CLAUDE.md rule 3): only `apps/aberp/src/
  serve.rs` is modified; every NAV / billing /
  audit-ledger / serve-infrastructure seam is
  unchanged.

**Negative**

- The wire shape (`state: &'static str`) is more
  permissive than a typed enum would be. A consumer
  that hard-codes the pre-PR-23 six-label set will
  silently fail to recognise the five new strings.
  Mitigation: the Svelte shell currently renders the
  string verbatim with no display affordance; the new
  labels appear as plain text. A future PR that adds
  Svelte-side affordances can also tighten the wire
  shape to a typed enum if operational evidence
  surfaces.
- The substring-scan recovered-Response discriminator
  is heuristic, not parsed. A response_xml that
  contains `QueryInvoiceDataResponse` as a CDATA
  payload or inside a `<message>` body would
  false-positive into `Recovered`. Mitigation: the
  string only appears in NAV-emitted XML as the root
  element of the query-invoice-data response (per
  ADR-0028's verbatim-bytes posture) or as a literal
  inside a `<funcCode>` / `<message>` field — NAV's
  error responses don't carry that string in any
  observed field. A future PR can swap the substring
  scan for the verifier's exact prefix-match helper if
  the audit-ledger or verifier crate publishes one as
  a `pub` function. Per CLAUDE.md rule 2 (simplicity
  first), the substring scan stays for PR-23.
- The walker pre-allocates an `InvoiceTrace` per
  invoice id and walks the entire ledger once per
  list-or-detail request. Same posture as the pre-PR-23
  walker; the trace just carries more fields. Per-
  invoice cost is O(entries); per-list cost is
  O(entries × invoices) at the worst case (every
  invoice has Storno / Amended evidence to scan). Per
  the ADR-0009 §"Adversarial review" #1 posture this
  is bounded by per-tenant invoice volumes, not
  hyperscale; no change to the §"Adversarial review"
  trade-off is named.

**Locked in**

- The eleven-label set is the contract. Adding labels
  is additive (per §10's four-edit obligation); removing
  a label requires a superseding ADR.
- The mirror invariant: `derive_state` MUST agree with
  `stuck_precondition` on every overlapping state. The
  parameterized table enforces this at test time;
  future refactors of either surface must not drift.

## Adversarial review

A hostile NAV inspector and a hostile-engineer review,
in alternation.

1. **"You're sharing test fixture writers across
   `audit_query.rs::tests` and `serve.rs::tests`. The
   audit_query writers are `#[cfg(test)]` inside their
   `mod tests` block; sharing across modules means
   either moving them to a separate `#[cfg(test)]
   pub(crate)` module or duplicating them. Which?"**
   Duplication. The writers are small (4 of them, ~15
   LoC each: `write_submission_attempt`,
   `write_submission_response`, `write_ack_status`,
   `write_marked_abandoned`) plus the chain-link
   writers for storno / modification and the
   check-performed writer (5 / 6 LoC each via the
   `from_outcome` / `new` constructors). Cross-module
   sharing would require either a new `tests/common/`
   directory or a `#[cfg(test)] pub(crate)` surface
   on `audit_query`. Both add structural surface
   beyond what PR-23 needs. Per CLAUDE.md rule 2
   (simplicity first) + the per-PR-19 precedent
   (PR-19's `pending_when_attempt_*` tests added the
   `write_submission_attempt` helper in
   `audit_query::tests` without re-exporting it),
   PR-23 duplicates the handful of writers it needs.
   If a third module in `apps/aberp/src/` adds
   audit-ledger fixture tests, the future PR can lift
   the writers to a shared `#[cfg(test)] pub(crate)
   mod test_fixtures`.

2. **"Substring scan for `QueryInvoiceDataResponse`
   is brittle. A NAV inspector who writes a hand-crafted
   bundle to test ABERP's UI could embed the literal
   in a `<message>` field. False-positive recovered
   label."** Acknowledged + accepted. The substring
   appears in NAV-emitted XML as the root element of
   the query-invoice-data response per ADR-0028 §2 +
   the NAV v3.0 XSD; a `<message>` field that
   contained the literal would be NAV-side, not
   ABERP-injected. The brittleness affects only the
   `Submitted` ↔ `Recovered` distinction; mis-
   classification does NOT affect the operator's
   command recommendations (both classify as state-3
   AwaitingAck; both recommend `poll-ack`). A future
   PR can swap the substring scan for the verifier's
   exact prefix-match (which mirrors A91's local-name
   match) if operational evidence surfaces. Per
   CLAUDE.md rule 12, the substring-scan choice is
   named loud here in the ADR.

3. **"The walker carries more state per invoice now —
   `is_storno_base`, `is_amended_base`,
   `latest_response_is_recovered`,
   `latest_check_outcome`. That's four new fields on
   `InvoiceTrace` for the new labels. Per CLAUDE.md
   rule 13 (delete before optimize), should any of
   these collapse?"** No. Each new field carries a
   distinct fact:
   - `is_storno_base` / `is_amended_base` are
     orthogonal (an invoice can be the base of a
     storno AND a modification chain over its
     lifetime per ADR-0024 §7); collapsing into a
     single `chain_link_kind: Option<EventKind>` field
     would lose the both-true case.
   - `latest_response_is_recovered` is a bool, not an
     enum, because the discriminator is binary
     (recovered or not). Collapsing into
     `latest_response_root_element: Option<String>`
     would store the full local name for every
     invoice — speculative per CLAUDE.md rule 2.
   - `latest_check_outcome` is `Option<String>` to
     match the audit-payload field type;
     collapsing into a bool `latest_check_was_exists`
     would lose `"absent"` / `"failure"` outcomes the
     parameterized table tests pin. The
     three-outcome enum (`"exists"` / `"absent"` /
     `"failure"`) is what ADR-0033 §2 declares; the
     UI walker carries the string verbatim for fidelity.
   Per rule 13's "if you're not adding back at least
   10% of what you delete, you weren't aggressive
   enough" the inverse-check fires here: four fields
   added; zero deletable.

4. **"`PendingNavExists` couples the UI label to the
   ADR-0033 §6 Layer-2 surface. If a future ADR
   reclassifies Layer-2 entries as classification-
   bearing (ADR-0033 §6 explicitly names this as a
   future possibility — `NotStuck(StateRecoveryPending)`
   or similar), the UI label would need to change in
   lockstep with the command-side classifier. That's
   coupling between two seams."** Acknowledged. The
   coupling is deliberate per the mirror invariant
   (§1): if `stuck_precondition`'s classification
   ever changes for an `Attempt` + `CheckPerformed(exists)`
   ledger shape, the UI label MUST also change. The
   parameterized table tests are the load-bearing
   enforcer — a future refactor of `stuck_precondition`
   that returns `NotStuck(StateRecoveryPending)` for
   this shape WITHOUT also updating `derive_state` and
   the table fails the mirror invariant test loud per
   CLAUDE.md rule 12. The coupling is the feature, not
   the bug.

5. **"Eleven labels is a lot for a coarse list-level
   classifier. Operators reading the invoice list will
   need a legend. Is that a UI problem you've punted
   to PR-9-2?"** Yes — explicitly per §7. PR-23's job
   is to make the LABELS available on the wire; the
   Svelte shell's rendering (colour, icon, sort order,
   filter dropdown, hover-tooltip legend) is a future
   PR. A NAV inspector reading the API directly sees
   the labels as `&'static str` — operationally
   identical to reading `git status` output (no
   legend; the labels themselves are self-describing).
   The Svelte shell can add affordances additively
   without rev-locking the wire shape.

6. **"`derive_state`'s ladder is currently in
   `serve.rs`. The audit-payload decoders are also
   in `serve.rs`. The walker is in `serve.rs`. Is
   this becoming a 'serve.rs is the UI module' anti-
   pattern?"** Acknowledged in part. PR-23 keeps the
   walker in `serve.rs` per CLAUDE.md rule 3 (surgical
   changes); the alternative — extract a new
   `apps/aberp/src/ui_state_classifier.rs` module —
   would inflate the PR with file-create overhead +
   API-design questions (pub surface, naming,
   integration). The post-PR-23 `serve.rs` is ~970
   lines (823 → ~970 net), still in the "one-file
   readable" range per the codebase's existing
   norms. A future PR can extract if `serve.rs` grows
   past the readability threshold (the rough trigger
   from other modules in this repo is ~1500 lines).

7. **"The `extract_chain_base_link` probe is a second
   serde dispatch alongside `extract_invoice_id`.
   That's two passes (deserialise the entry's payload
   twice — once for the `invoice_id` probe, once for
   the `base_invoice_id` probe). Per CLAUDE.md rule 13
   (delete before optimize), should you collapse?"**
   Two passes do happen for the two chain-link kinds
   (StornoIssued + ModificationIssued) — every other
   kind short-circuits after the first probe returns
   `None`. The cost is bounded: chain-link entries
   are at most one per storno or modification (rare
   per per-tenant volumes per ADR-0009 §"Adversarial
   review" #1). Collapsing into one decoder that
   returns `(Option<String>, Option<(EventKind,
   String)>)` would couple the two unrelated checks
   in one helper for a single-digit-percent CPU
   saving on a path that already walks the full
   ledger. Per rule 13 the inverse-check applies:
   collapsing adds back zero (it doesn't delete a
   helper; it merges two), so the deletion is not
   aggressive enough — the two-probe shape stays.

## Alternatives considered

- **Move the classifier out of `serve.rs` into a new
  `ui_state_classifier.rs` module.** Rejected per
  CLAUDE.md rule 3 (surgical changes). The current
  walker is ~80 lines; extraction adds file-create
  overhead + pub-surface design without a load-bearing
  reason. A future PR can extract if `serve.rs`
  surfaces readability concerns.
- **Use the preceding entry kind to detect
  recovered Responses (ADR-0034 §4 reading B).**
  Rejected per ADR-0035 §4's verifier precedent —
  Reading A (root-element prefix-match) is the
  byte-level signal that survives bundle export /
  re-import. The preceding-entry signal couples the
  detector to ledger ordering; the byte-level signal
  works on any per-Response payload regardless of
  context.
- **Render the new labels as sub-state badges on the
  existing `Pending` / `Submitted` labels rather
  than as distinct labels.** Rejected per §"What
  surfaced" #1. The Svelte shell would need to
  understand both label AND badge to render
  correctly; the wire shape would need a
  `state: { label, sub_label }` object rather than a
  `&'static str`. Per CLAUDE.md rule 2 the
  single-string label is simpler.
- **Tighten the wire shape from `&'static str` to a
  typed enum in the same PR.** Rejected per
  CLAUDE.md rule 3. The wire-shape change is its own
  surface (Serde tag rendering, Svelte-side parse
  logic, backward-compat with pre-PR-23 consumers);
  mixing it with the label-set extension violates
  the surgical-changes posture. A future PR can
  tighten if operational evidence surfaces.
- **Add a `Voided` label.** Rejected — no
  `EventKind::InvoiceVoided` variant exists yet.
  Adding the variant + the void-before-submit
  command-side surface is a multi-PR build-phase
  surface per ADR-0009 §"Open questions" (the void
  treatment is an accountant question). PR-23
  stays out.
- **Add an `AnnulmentRequested` label.** Rejected —
  annulment is a NAV-side data withdrawal, not a
  state transition on the base invoice. The base
  invoice's typestate label is unchanged by the
  annulment lifecycle.

## Open questions

- **Should the Svelte shell render the new labels
  with colour / icon affordances?** Deferred to a
  future PR-23-followup (Svelte-side). PR-23 lands
  the wire-side labels only.
- **Should the wire shape tighten from `&'static
  str` to a typed enum?** Deferred to a future PR
  with operational evidence triggering. Today every
  consumer renders the string verbatim.
- **Should the recovered-Response discriminator
  upgrade from substring scan to the verifier's
  exact prefix-match?** Deferred to a future PR
  that depends on the verifier publishing a `pub`
  prefix-match helper (PR-22 / aberp-verify keeps
  the helper private today). The substring scan is
  the operational-evidence-driven trade-off named
  loud in §5 + §"Adversarial review" #2.
- **Should `derive_state` extend to detect a
  `Storno-of-a-Storno` or `Amended-of-a-Storno`
  chain?** Deferred. ADR-0023 + ADR-0024's
  chain-allocator logic supports the structure; the
  per-invoice label level does not surface the
  multi-step chain. A future PR may add a
  `StornoChainDepth` field on `InvoiceListItem` if
  the operator-visible signal warrants it.

## Follow-on ADRs unblocked by this decision

- **Future Svelte shell PR.** Render the eleven
  labels with colour / icon affordances; add a
  filter dropdown + sort order; add a hover-tooltip
  legend. No ABERP-side ADR needed if the wire
  shape stays `&'static str`; a typed-enum
  migration would file its own ADR.
- **Future `audit_query::stuck_precondition`
  classification extensions** (e.g.,
  ADR-0033 §6's named-but-deferred
  `NotStuck(StateRecoveryPending)` for state-2 +
  Exists). The mirror invariant requires
  `derive_state` to extend in lockstep; the
  parameterized table is the load-bearing
  enforcer.
- **Future EventKind extensions** that classify
  into new UI labels per §10's four-edit
  obligation.
