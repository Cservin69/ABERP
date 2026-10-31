# ADR-0112 — Client idempotency key on `POST /invoices/issue` (ADR-0009 §5 amendment)

- **Status:** Proposed — implemented and tested on branch `adr-0111-remote-client` (2026-10-31).
- **Date:** 2026-10-31
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR. It extends ADR-0009 §5 Layer 1 and does **not**
  supersede it. The Layer-1 contract stays as written: the `IssueInvoiceCommand` ULID is the
  idempotency key, and the allocator returns the prior outcome on a repeat. This ADR only makes
  the HTTP route honour it.
- **Related:** ADR-0005 (`idem_<ULID>` prefix), ADR-0008 (append-only; one event per business
  fact), ADR-0009 §3 (gap-free allocator) and §5, ADR-0111 (remote client over the Thunderbolt
  Bridge — the trigger), ADR-0110 D3/D7 (durable ack and WAL fence; unaffected).

## Context

ADR-0009 §5 Layer 1 says the caller generates the command's idempotency key once and retries
with the same key. The billing allocator already implements the server half:
- `allocate_in_tx` looks up `invoice.idempotency_key` (UNIQUE).
- On a hit it returns `AllocateOutcome::Replay` without burning a number.
- `issue_invoice::run_single_tx` then writes no audit entries.

The route never let a caller supply the key. `build_command` minted `IdempotencyKey::new()` on
every request. Locally that was invisible, because a loopback response is never lost. With
ADR-0111 a response travels over a cable, and T2b of the ADR-0111 test plan fails:
1. The cable drops after the commit but before the response arrives.
2. The operator clicks Issue again.
3. A **second invoice** is issued. It has a valid next number, so the audit chain stays intact,
   but it is a real business duplicate that needs a storno.
4. If submit-on-issue is on, there is a **second NAV submission** and a second email to the buyer.

## Decision

1. **Wire.** `IssueInvoiceRequest` gains an optional `idempotencyKey` field (canonical
   `idem_<ULID>`).
   - If it is absent, a fresh key is minted, as before (CLI, fixtures, older clients).
   - If it is malformed, `validate_issue_request` returns **400** before anything is allocated.
     It is never silently replaced by a fresh key.
2. **Pipeline.** `issue_invoice_request` → `issue_from_parsed(…, client_idempotency_key)` →
   `build_command`. `IssuedInvoiceSummary` gains `replayed: bool`, set from `!was_fresh`.
3. **Replay has no side effects.**
   - No number is burned and no audit entry is written (the allocator and `run_single_tx`
     already guaranteed this).
   - The NAV XML is **not** rewritten: the render closure only renders the number.
   - The side-stored `input.json` written for the retry is removed.
   - The route does **not** re-run the post-issue email or NAV submit.
   - The response carries `replayed: true` and the invoice's **current** derived state, read
     from the ledger. It does not claim a fresh `Ready`.
4. **SPA.** `IssueInvoice.svelte` mints one key per mounted form (`mintIdempotencyKey`, a ULID in
   Crockford base32, pinned by a spec vector) and resends it on every retry. A successful issue
   unmounts the form, so the next form gets a fresh key.

## Consequences

- A retry after a lost response, a double-click, or a retry racing the original all resolve to
  **one** invoice. Numbering stays gap-free; ledger writes serialise on the shared Handle's
  writer.
- The CLI keeps one-call-one-invoice semantics.
- **Known limits:**
  - **The key is bound to its first request.** If the first attempt committed and the operator
    then edits the form and resubmits, they get the *original* invoice back (`replayed: true`),
    not their edit. That is the safe outcome: no second invoice. The SPA navigates to the
    invoice, so the operator sees what was actually issued. A body-hash mismatch check (409) is
    left for later.
  - **The replayed number is re-rendered from the current numbering template.** It matches the
    original unless `[seller.numbering]` changed between the two requests.
  - **Some pre-allocation work still runs on a replay.** The EUR MNB rate fetch and, on dev
    builds, the NAV number pre-flight run before the allocator detects the replay. Both are
    reads with no persistent effect.

## Tests (`apps/aberp/tests/serve_issue_route.rs`)

- `t2b_retry_with_same_idempotency_key_replays_instead_of_double_issuing` checks that a retry:
  - returns the same id and number, with `replayed: true`;
  - adds no ledger entries (one `InvoiceSequenceReserved`, one `InvoiceDraftCreated`) and leaves
    `verify_chain` passing;
  - writes no new XML or side-store;
  - and that the next fresh key gets number + 1.

  **It fails on the pre-0112 code** (checked by temporarily reinstating the fresh-key-per-request
  behaviour).
- `t2b_control_retry_without_key_issues_a_second_invoice` pins the keyless semantics.
- `malformed_idempotency_key_is_refused_before_allocation` checks that nothing is allocated for
  a malformed key.
- `t1_concurrent_issues_stay_gap_free_and_same_key_racers_collapse`, on a multi-thread runtime:
  - two same-key racers produce one invoice, one Fresh and one Replay;
  - 20 concurrent distinct keys produce contiguous, unique numbers;
  - `verify_chain` passes.
- SPA: `mintIdempotencyKey` is checked against the ULID spec vector `01ARYZ6S41…`, the alphabet,
  and the top bound.
