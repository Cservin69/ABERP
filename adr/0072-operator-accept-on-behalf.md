# ADR-0072 — Operator accept-on-behalf for quotes (S354 / PR-42, audit U16)

- **Status:** Accepted
- **Date:** 2026-06-11
- **Deciders:** Ervin (via S354 / PR-42 brief, closing audit U16 from S346 `docs/findings/s346-audit-quote-workflow.md`)
- **Supersedes:** none.
- **Related:** ADR-0005 (storefront HMAC accept-link / typed-ACCEPT — this ADR's storefront counterpart is the ADR-0005 *amendment* in the ABERP-site repo), ADR-0004 (priced-quote writeback), ADR-0008 (audit ledger), ADR-0067 (DEAL saga atomicity — the post-accept consumer), and `[[trust-code-not-operator]]`.

## Context

A customer can accept a quote two ways in real life: by clicking the unique DEAL link they were e-mailed (the customer-owned typed-ACCEPT path, ADR-0005), or by telling the operator off-channel — phone, e-mail reply, in person. The storefront's typed-ACCEPT scheme **deliberately forbids** setting `approved` over a plain Bearer (`src/routes/api/quotes/[id]/status/+server.ts` — "approved is only settable by the customer accept POST"); that refusal is the whole point of ADR-0005. The consequence (audit U16): an off-channel acceptance had **no path at all**. The quote sat at `quoted` until the 30-day link expired and never reached DEAL or an invoice — an operator dead-end.

The storefront stays the **source of truth** for quote state (ADR-0002 split). So ABERP cannot just flip a local flag; it must advance the storefront's state. But it must do so without weakening the ADR-0005 guarantee that a plain Bearer can't forge a customer acceptance.

## Decision

**Add a second, distinct accept path: ABERP records an operator-accept locally (audit-of-record) and POSTs a Bearer + HMAC-signed `operator_accepted` signal to the storefront, which advances the quote to the same terminal `approved` only when the HMAC validates.**

### 1. Backend route

`POST /api/quote-pricing-jobs/:id/accept` — Bearer + Ready gated like its siblings. Body `{ channel, note, customer_confirmation_path? }` where `channel ∈ {phone, email, in_person, other}` and `note` is required.

Pre-flight (`accept_quote_precheck`, tenant-scoped):
- Row must be `JobState::Posted` — priced **and** delivered to the storefront (⇒ storefront `quoted`, awaiting acceptance). Any other state → 409 `JobNotAcceptable`.
- Foreign-tenant row → 404 (never 403), matching the detail route.
- A prior **successful** operator-accept (a `quote.operator_accepted` ledger entry with `outcome=="success"`) → 409 `JobAlreadyAccepted`. A prior **failed**-writeback accept does NOT block — the storefront state was never advanced, so a retry is allowed.

On a clear pre-flight: sign, POST to `{base_url}/api/quotes/{id}/status` (via the S351 `resolved_writeback_url` helper, so trailing slashes are stripped), classify the response with the **same** S347 `WritebackOutcome` gate as the priced-writeback, emit the local audit **regardless of writeback success**, then 200 (synced) or 502 (sync failed, carrying the classified outcome + `retry_available: true`).

### 2. The HMAC and which secret

ABERP signs `HMAC-SHA256(key = storefront Bearer secret, msg = "{quote_id}|operator_accept|{channel}|{accepted_at_ms}|{operator_user_id}")`, lowercase-hex, and sends it as `hmac_signature`. The storefront recomputes it and timing-safe-compares.

**Which secret.** The key is the storefront **Bearer** shared secret (`ABERP_SITE_ADMIN_TOKEN` on the storefront; `StorefrontCredentialSnapshot::bearer` in ABERP). ABERP does **not** possess the storefront's customer-token `QUOTE_STATUS_SIGNING_KEY`, so the Bearer is the only secret shared between the two services — and signing with it is what binds the accept to ABERP's identity. (The brief's "the same secret used for `/priced`" is therefore correct: `/priced` authenticates with this Bearer.)

**Security honesty (flagged).** The Bearer alone already authenticates the request, so the HMAC is **not** an additional authentication factor — anyone who can present the Bearer can also compute the HMAC. Its real jobs are: (a) **bind the semantic fields** `{quote_id, channel, accepted_at_ms, operator_user_id}` so they cannot be altered independently of the token, and (b) **gate the otherwise-forbidden transition** — the storefront keeps refusing plain-Bearer `approved`, and only the explicitly-signed `operator_accepted` intent may reach `approved` operator-side. The domain separator `operator_accept` (mirroring ADR-0005's `status` / `accept` markers) prevents the signature being replayed as any other signed surface. Replay of the *same* operator-accept is naturally blocked: a second attempt hits the storefront's already-`approved` 409 (and ABERP's local already-accepted 409 first). We do **not** enforce a freshness window on `accepted_at_ms` (operator clocks, deliberate human latency); idempotency, not timestamp expiry, is the replay defense.

### 3. Terminal state mirrors the customer path

Operator-accept advances the storefront quote to **`approved`** — the *same* terminal state the customer accept reaches — not a new status string. The only difference is the recorded provenance (`accepted_via: 'operator'` + `operator_user_id` / `operator_channel` / `operator_note`). This keeps every downstream consumer (DEAL saga polls `approved`; `invoiced ← approved`) working identically whether the customer or the operator confirmed. The brief's wording "transition to `accepted`" maps to the storefront's actual `approved` status (customer-facing label "Elfogadva / Accepted").

### 4. Local audit-of-record

A new `EventKind::QuotePricingOperatorAccepted` (`quote.operator_accepted`, F12 ritual) is emitted on **every** attempt — success records the committed accept; a failure records the attempt + its classified reason — so the SPA detail timeline and a forensic walker see both, and a failed sync surfaces as a retry rather than vanishing. Idempotency key is per-attempt (`quote_operator_accepted:<quote_id>:<accepted_at_ms>`) so a retry never collides at the ledger UNIQUE.

### 5. SPA

`PricingJobDetail.svelte` grows an inline **Elfogadás / Accept** affordance (channel select + required note + optional confirmation-path) shown only on a `Posted` row that has not already been operator-accepted (the backend 409 is the safety net). Inline-expand rather than a nested `<dialog>` to match the component's existing material-edit affordance.

## Consequences

- Off-channel acceptances now reach `approved` → the DEAL / invoice path (ADR-0067, audit U18) is unblocked.
- The ADR-0005 plain-Bearer-`approved` refusal is **preserved**; the new path is gated behind a distinct signed intent.
- The "file attach" in the brief is recorded as a **path/reference string only** — the backend stores it in the audit, it does **not** ingest bytes (there is no upload surface). A true file-ingest pipeline is out of scope (flagged for a later session if an operator workflow needs it).
- Out of scope (named in the brief): resend DEAL token (U17 / S355), convert accepted → ABERP invoice (U18 / S356), decline/Lost workflow (U19), bulk operator-accept.
