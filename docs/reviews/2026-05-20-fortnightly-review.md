# 2026-05-20 — Fortnightly adversarial review (post-PR-6)

- **Reviewer:** Claude (session 6), under direction from Ervin.
- **Scope:** Everything merged since the pre-code spine review on
  2026-05-19, namely commits `fca5678..04cb75c` (PR-1 through PR-6).
  ADRs touched in that window: ADR-0021 (amended in place to close
  F1–F4). All other Spine ADRs are unchanged since the prior review
  and are not re-walked here — they were swept clean and adding noise
  to this pass would dilute the findings that are actually live.
- **Methodology:** For each open finding from the prior review, ask
  "has the trigger fired, or has the situation changed?" For each new
  surface (the seven landed PRs), ask: "what would an external
  auditor say is fragile, under-specified, or papered-over?"
  Particular attention on the PR-6 close-out — that is the freshest
  claim and the one most likely to hide a corner case.
- **Bias check:** I authored every line of code in commits
  `fca5678..04cb75c` and the prior review. Self-review of one's own
  recent work is the highest-blind-spot pass in this cadence.
  Compensating tactic in this review: I walk each finding from the
  posture of "what would I have caught if a different engineer
  delivered this PR to me?" — and I name three new findings against
  PR-6 specifically (F8, F9, F11) which were the easiest to miss
  while delivering it. If only "carried forward" findings appear,
  the bias check has failed.

---

## Headline

**Five new findings (F8 – F12)**, all against code rather than
against ADRs. None is a release blocker; F9 (ad-hoc JSON
construction) is the highest-priority fix because PR-7 (NAV
submission) will put verbatim NAV response bodies into audit
payloads and that path needs a JSON encoder it can trust on day one.

**Six carried-forward items** (F5, F6, F7, F10, F13-new, F15-new
naming, see §"Carried-forward"). None has had its named trigger
fire during this window.

**Pre-code blockers F1 – F4 are closed** by ADR-0021's in-place
amendment (`0da6dce`) and by the workspace `[workspace.dependencies]`
table landing in `fca5678`. **The "cross-crate transactional audit"
tracked deviation from PR-5 is closed** by PR-6 (`04cb75c`); see
F-CLOSED-X1.

---

## Closed since last review

### F-CLOSED-1 — DuckDB driver crate pinned. (was F1)

ADR-0021 §A10 pins `duckdb = { version = "1", features = ["bundled"] }`
(workspace `Cargo.toml` line 69). Exercised end-to-end by PR-3, PR-4,
PR-5, PR-6 — the audit-ledger and billing crates both use it, and
the binary now depends on it directly to own the tenant connection
per PR-6's transactional close-out.

### F-CLOSED-2 — Date / time crate pinned. (was F2)

ADR-0021 §A11 pins `time` with the agreed features. Used in
audit-ledger (`time_wall` RFC3339) and billing (`issue_date`,
`reserved_at`, `created_at`, etc.).

### F-CLOSED-3 — Canonical-serialization scheme pinned. (was F3)

ADR-0021 §A12 pins `ciborium`. The canonical encoder lives at
`crates/audit-ledger/src/canonical.rs` (one place, per the ADR-0021
amendment) and is exercised by the four `canonical::tests` unit
tests plus the chain-conformance test.

### F-CLOSED-4 — `ulid` crate enumerated. (was F4)

ADR-0021 §A13 names it explicitly. Used everywhere IDs appear.

### F-CLOSED-X1 — Cross-crate transactional audit. (was the tracked deviation in PR-5)

`apps/aberp/src/issue_invoice.rs::run_single_tx` now drives the
billing allocator and the audit-ledger appends inside one
`duckdb::Transaction`. Rollback is pinned by
`apps/aberp/tests/rollback_conformance.rs` with two variants
(drop-without-commit and panic-injection). The pre-PR-6 mitigation
("reconciliation scan would surface the orphan") is now defence in
depth; the primary guard is the tx itself.

The close-out does **not** close every ADR-0008 §Storage invariant
— see F10 below for the still-open mirror-file requirement.

---

## New findings

### F8. Audit-ledger `idempotency_key` column uses `format!("{:?}", IdempotencyKey)`. Tracked, medium severity.

**Where:** `apps/aberp/src/issue_invoice.rs:306`:

```rust
let idem_str = format!("{:?}", idempotency_key);
```

`IdempotencyKey` derives `Debug`, so today this emits
`IdempotencyKey(<26-char-ULID>)`. The string is written to the
audit-ledger `idempotency_key` column and used by future Layer-1
idempotency lookups against the ledger (ADR-0009 §5).

**Concern:** Debug derivations are not part of the type's API
contract. If a contributor adds a field to `IdempotencyKey`, switches
to a manual `Debug` impl for redaction, or migrates to a different
ID-format crate, the Debug output changes silently. Two binaries
across a Cargo bump would write incompatible idempotency-key
strings; the Layer-1 lookup would miss prior commits and re-burn
sequence numbers — exactly the failure mode ADR-0009 §5 names as
load-bearing.

**Recommendation:** Define a stable string form on `IdempotencyKey`
(method `to_canonical_string`) that returns the prefixed ULID
(`idem_<26-char>`) per ADR-0005, and call that everywhere. The
canonical-string contract becomes part of the public API; changes
require an ADR or a bump.

**Named trigger:** Fix before PR-7 lands. PR-7 adds
`invoice.submission_attempt` and `invoice.ack_status` entries whose
audit payloads will be compared against `idempotency_key` for
retry-on-replay logic.

### F9. Ad-hoc JSON construction in audit payloads. Tracked, high priority before PR-7.

**Where:** `apps/aberp/src/issue_invoice.rs:308–313, 324–328`:

```rust
let payload_seq = format!(
    "{{\"invoice_id\":\"{}\",\"seq\":{},\"reservation_id\":\"{}\"}}",
    invoice.id.to_prefixed_string(),
    invoice.sequence_number,
    reservation.id.to_prefixed_string(),
);
```

Today the only interpolated values are prefixed ULIDs (Crockford
base32, no special chars) and a `u64`. The output is well-formed
JSON.

**Concern:** ADR-0009 §8 says **every NAV submission audit entry
carries `payload.request_xml` and `payload.response_xml` verbatim**
plus `payload.request_parsed` / `response_parsed`. The moment a
verbatim NAV response with a quote, backslash, control character,
or unicode codepoint goes through the same `format!` pattern, the
audit payload becomes invalid JSON. The `payload` column accepts
arbitrary `BLOB`, so the corruption is silent — no SQL error, no
log, no test failure until something downstream tries to parse the
column back.

This is the **"completed successfully with 14% of records silently
skipped"** failure mode CLAUDE.md rule 12 names as the worst class
of bug.

**Recommendation:** Use a typed struct + `serde_json::to_vec` for
every audit payload, no `format!`-built JSON anywhere in the
codebase. The audit-ledger crate's surface is already
`Vec<u8>`-shaped so the change is at the call site, not at the
boundary. Suggest landing this in a small PR-6.1 *before* PR-7 so
the NAV payload path inherits a JSON encoder it can trust.

**Named trigger:** Before PR-7. Non-negotiable.

### F10. ADR-0008 §Storage mirror file `<tenant>.audit.log` still not implemented. Deferred, named trigger sharper.

**Status:** Carried from session-5 close. Re-stated here because
PR-6 closed one of ADR-0008 §Storage's two load-bearing invariants
("same transaction") but not the other.

**ADR text:**

> The ledger is also mirrored to an append-only file
> (`<tenant>.audit.log`) outside the DB on every commit, fsync'd.
> This protects against DB-file corruption and gives us a
> second-source artifact for export.

**Why it matters now:** With PR-6 the tenant DB file is the only
durable artifact for audit entries. DuckDB-file corruption (disk
fault, mid-write power loss outside DuckDB's own fsync window,
silent block-flip) cannot be recovered from. The mirror file is
the second source.

**Risk profile in commit-#1 scope:** Low. The binary runs once and
exits; the window for corruption is narrow. PR-7 lengthens the
window (NAV submission + polling can take minutes per invoice) but
still not catastrophic.

**Risk profile after the per-invoice export PR ships:** High. The
export bundle (ADR-0009 §8) requires "every audit-ledger entry for
that invoice, in order" — without a second-source artifact, the
export is single-pointed on the DuckDB file.

**Named trigger (sharper than the handoff's "likely with the
per-invoice export PR"):** The mirror file must land *before* the
per-invoice export PR is reviewed for merge. If the export PR
arrives without the mirror file, reject and split.

**Implementation note for the future PR:** The mirror file write
must ride the same DuckDB transaction's commit, not a separate
fsync. Suggest: `Ledger::append` (and `append_in_tx`) writes the
canonical-encoded entry to a buffer; on `tx.commit()` succeeding,
the buffer is fsync'd to the mirror file. If the mirror-file write
fails, surface as an error post-commit and add a `mirror.divergent`
audit entry on the next append. This violates "same transaction"
in the strictest reading — the mirror is *outside* DuckDB — but
matches the ADR's intent that the mirror is a second-source not a
co-primary.

### F11. `Ledger::open` re-runs `ensure_schema` after the binary already ran it. Tracked, low severity.

**Where:** `apps/aberp/src/issue_invoice.rs:184` re-opens
`Ledger::open(&args.db, tenant.clone(), binary_hash_bytes)` for
the verify-chain step. `Ledger::open` calls `Self::initialise`
which calls `ensure_schema(&conn)` — the same DDL the binary
already ran at line 240 (`audit_ledger::ensure_schema(&conn)`).

**Concern:** Cosmetic noise, not a correctness bug. `CREATE TABLE
IF NOT EXISTS` is idempotent. But it means the binary's intent
("schemas are set up; now I want verify-only access") cannot be
expressed in the type system.

**Recommendation (do not fix yet):** When a second caller appears
that wants verify-only access without re-running DDL, add a
`Ledger::adopt(conn, tenant_id, binary_hash) -> Result<Self>`
constructor that asserts the schema is present rather than
creating it. Until that second caller exists, the redundant call
is rule-2-acceptable (no speculative abstraction).

**Named trigger:** When the UI scaffold (PR-9) wants a read-only
`Ledger` view, this is the right moment.

### F12. `EventKind` row decoder is a closed set; growth requires hand-maintenance. Tracked, medium severity.

**Where:** `crates/audit-ledger/src/storage/mod.rs:232–237` (in
`row_to_entry`):

```rust
let kind = match kind_str.as_str() {
    "test" => EventKind::Test,
    "invoice.sequence_reserved" => EventKind::InvoiceSequenceReserved,
    "invoice.draft_created" => EventKind::InvoiceDraftCreated,
    _ => return Err(duckdb_decode_err("unknown event kind")),
};
```

**Concern:** Every new audit event kind (ADR-0008 §"What goes in
the ledger" lists at least ten event categories: invoice issued,
payment recorded, stock moved, order shipped, label printed,
robotics task dispatched, CAD artifact stored, NAV submit, NAV ack,
Billingo pull, auth events, config changes) requires three edits:
the enum variant in `entry/event_kind.rs`, the `as_str()` arm, and
the `row_to_entry` decode arm. Forgetting the decode arm produces
a runtime "unknown event kind" — only after a row with that kind
exists in storage, which can be after deployment.

**Concern is sharper given F9's PR-7 trigger:** PR-7 adds at least
three new event kinds (`invoice.submission_attempt`,
`invoice.submission_response`, `invoice.ack_status`). All three
must be added at three sites; the unit tests must exercise both
encode and decode for each.

**Recommendation:** Make `EventKind`'s as_str + parse a
round-trip-proven pair by deriving a single source of truth. Two
options:

1. Use `strum::EnumString + AsRefStr` to derive both from one set
   of `#[strum(serialize = "...")]` attributes. Adds a dep.
2. Hand-write a `from_storage_str(s: &str) -> Result<Self, &str>`
   helper next to `as_str()` and call it from `row_to_entry`. No
   new dep; one place to edit.

Per ADR-0021 §Items deferred ("no new deps without an ADR"),
option 2 is the conservative pick for PR-6.1.

**Named trigger:** PR-7 — when three new event kinds land.

---

## Carried-forward findings

### F5. ADR-0008 attestation signing key type. Deferred, unchanged.

Trigger: first PR that exercises attestation cadence (long-running
process or a test that forces a 1000-entry / 60-minute trigger).
**Closer to firing than at last review** — PR-7's NAV submission
path opens a window where a tenant could run continuously, but
PR-7 itself is unlikely to cross 1000 entries in one invocation.
The named trigger remains "cloud deployment PR" or "operator-loop
PR."

Recommendation when filed: Ed25519. No parameter choices, smaller
keys, faster signatures.

### F6. OS-keychain Rust binding crate. Deferred, **about to fire.**

Trigger: first PR that loads keychain-bound material in production
code. **This trigger fires in PR-7** — ADR-0009 §4 names
"technicalUserName / technicalUserPassword / signKey / exchangeKey"
as keychain-bound, and PR-7 is the first PR to use them in flight.

Recommendation: `keyring` crate (MIT/Apache-2.0). Pin in ADR-0021
Part A as item §14 in the same commit that introduces it.

### F7. ADR-0020 [OPEN] NAV response-body integrity. External-check, **no progress** in this window.

Status: unchanged. The external check by a Hungarian developer
with shipped NAV experience has not happened (no evidence in the
repo, no message in chat in this window).

**Risk if PR-7 ships without this resolved:** Medium-to-high.
ADR-0020 calls the response-body verification path [OPEN]; ABERP
will need to make a defensible choice about whether to trust NAV's
response body at face value (the current ADR draft does) or to
require an independent signature verification. Either choice is
defensible; an undeclared choice is not.

**Recommendation:** Before PR-7 merges, either (a) close the
[OPEN] in ADR-0020 with a documented decision, or (b) explicitly
mark the response-body integrity as "Layer-0 trust on NAV TLS only,
deferred to a future ADR" with a named trigger. Do not let PR-7
land with the [OPEN] still hanging in the source ADR.

### F13. Per-invoice export-bundle (ADR-0009 §8) is multiply-gated. **New as a meta-finding**, carried forward.

The export bundle requires, at minimum:

- Every audit entry for the invoice, in order. ✓ available today.
- Verbatim request/response XML for every NAV interaction. **Gated
  on PR-7.**
- Every `queryTransactionStatus` response across the chain. **Gated
  on PR-7's polling path.**
- Every attestation checkpoint covering the entries. **Gated on
  F5** (signing-key type) **and on the attestation cadence path
  shipping**.
- The binary hash + the schema hash. Binary hash ✓ available;
  schema hash **gated on the XSD validation crate** (ADR-0021 §A.deferred).
- A signature over the whole bundle. **Gated on F5** (signing key).

**Concern:** The export-bundle PR is downstream of at least four
gates: PR-7, F5, the XSD validation crate, and F10. If it lands as
one PR, that PR is unreviewable. Plan a split: (a) bundle
construction with the available pieces and the signature stubbed,
(b) signature wired once F5 lands, (c) attestation checkpoints
wired once the cadence path ships.

**Named trigger:** When operator says "we need to produce an
export bundle for a NAV audit visit." Not before.

### F14. ADR-0009 Annual reset still `AnnualResetUnimplemented`. Intentional fail-loud, no action.

`modules/billing/src/adapters/duckdb_store.rs` and `app/issue_invoice.rs`
both return `BillingError::AnnualResetUnimplemented` for the
`AnnualOnFiscalYear` reset policy. The original PR-4 review accepted
this as "fail-loud rather than silent fallback to Never." Nothing
has changed.

**Named trigger:** First operator request for an annual-reset
series. Not before.

### F15. Real authentication still `Actor::test_only()`. Carried, **about to fire.**

`apps/aberp/src/issue_invoice.rs:305` uses `Actor::test_only()` for
both audit entries. Comment: "Real auth lands in a later PR."

**This is the same trigger as F6** (keychain-bound material). PR-7
needs both. Treat as a single deliverable: PR-7-A wires the
keychain crate, the real `Actor` derivation from session state, and
removes `Actor::test_only` from any non-test code path.

---

## Sweep notes per PR landed this window

- **PR-1 (workspace scaffold):** Clean. Workspace deps centralized
  per ADR-0021. No findings.
- **PR-2 (cargo-deny / cargo-audit / CI):** Clean. License allow-list
  enforced. No findings.
- **PR-3 (audit-ledger crate):** Carries F12 (closed-set decoder).
  No other findings.
- **PR-4 (billing module):** Carries F14 (annual reset). Otherwise
  clean.
- **PR-5 (XML-on-disk binary):** The cross-crate transactional
  deviation has been closed by PR-6 (see F-CLOSED-X1). PR-5 itself
  introduced F8 (Debug-derived idempotency_key) and F9 (ad-hoc JSON
  payloads). The two were not caught in the PR-5 review window;
  this review surfaces them.
- **PR-6 (single-tx close-out):** Carries F11 (redundant
  `ensure_schema`). Inherits F8 and F9 from PR-5 unchanged. The
  rollback conformance test is the right shape and exercises the
  intended invariant.

---

## Cross-cutting check — is PR-7 buildable today?

| Requirement | ADR | Status |
|---|---|---|
| HTTPS client with platform root certs | 0021 §A5, ADR-0020 | ✓ pinned (`reqwest` + `rustls-native-certs`) |
| XML serialization | 0021 §A8 | ✓ pinned (`quick-xml`) |
| SHA-3-512 for `requestSignature` | 0021 §A9 | ✓ pinned (`sha3`) |
| AES-128-ECB for `exchangeToken` | 0021 §A9 | ✓ pinned (`aes`) |
| OS keychain for credentials | 0007 | **F6 — must land in PR-7** |
| Real `Actor` derivation | 0008 | **F15 — must land in PR-7** |
| Stable idempotency-key string | 0008 + 0009 | **F8 — should land before or with PR-7** |
| serde_json for audit payloads | (none today) | **F9 — must land before PR-7** |
| Round-trip-proven `EventKind` encoding | 0008 | **F12 — should land before PR-7's three new event kinds** |
| Response-body integrity decision | 0020 | **F7 — must close [OPEN] in ADR-0020 or defer with named trigger before PR-7 merges** |

**Pre-PR-7 deliverable suggested (PR-6.1):** Land F8, F9, F12 as
one small refactor PR. F6 + F15 ride PR-7 itself. F7 needs an ADR
amendment or supersede.

---

## Action plan

1. **PR-6.1 — fragile-edges hardening** (suggested size: small,
   ~150 LoC + tests):
   - F9: Replace every `format!`-built JSON in
     `apps/aberp/src/issue_invoice.rs` with `serde_json::to_vec` on
     a typed payload struct. Add a roundtrip-decode test for each
     payload kind that exists today.
   - F8: Add `IdempotencyKey::to_canonical_string(&self) -> String`
     returning `idem_<ULID>` per ADR-0005. Replace
     `format!("{:?}", ...)` call site. Document the canonical form
     in the type's doc comment as part of its API contract.
   - F12: Add `EventKind::from_storage_str(s: &str) ->
     Result<Self, &'static str>`. Replace the `match` in
     `row_to_entry`. Unit test that for every `EventKind` variant,
     `from_storage_str(EventKind::X.as_str()) == Ok(EventKind::X)`.

2. **ADR-0020 §6 [OPEN] resolution** (no code, just an ADR edit or
   supersede):
   - F7: Either close with a documented decision, or restate as
     "deferred with named trigger: post-MVP NAV audit" and re-mark
     the supersede chain. Do not let it stay [OPEN] when PR-7 hits
     review.

3. **PR-7 (NAV submission)** — split into three sub-PRs per the
   session-5 handoff:
   - **PR-7-A** Transport + auth: keychain integration (closes
     F6), real `Actor` derivation (closes F15), TLS posture
     verified against `api-test.onlineszamla.nav.gov.hu`.
   - **PR-7-B** Submit + persist: `manageInvoice` happy path,
     state transition `Ready → Submitted`,
     `invoice.submission_attempt` and `_response` audit entries
     (F8/F9/F12 already landed).
   - **PR-7-C** Poll + ack: `queryTransactionStatus` loop,
     `invoice.ack_status` per poll, terminal-state handling,
     non-retryable error mapping per ADR-0009 §"Adversarial review".

4. **F10 mirror file** — open a tracking item now (named
   `audit-mirror-file.md` in `_handoffs/` if needed) so the
   per-invoice export PR cannot be reviewed without it.

5. **F5 attestation key** — Ed25519 recommendation persists. No
   action this fortnight.

---

## Sign-off

This review records the state at commit `04cb75c`. Findings F8,
F9, F12 are net-new and concrete. F-CLOSED-X1 is the largest
close-out in this window. The PR-6 close-out's mechanical
correctness (DuckDB tx semantics, panic-unwind semantics) was
re-checked by inspection of the code path against DuckDB's
documented semantics; the rollback conformance tests are the
externally-pinned proof.

Next review: ~2026-06-03, after PR-7 lands (or sooner if any
finding's named trigger fires).
