# ADR-0023 — Storno + modification chain — operator surface, chain-link allocator, audit-payload pin (ADR-0009 §6 amendment)

- **Status:** Accepted
- **Date:** 2026-05-20
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR — extends ADR-0009 §6 with
  the concrete pins PR-10 needs in order to land code without re-
  litigating naming, allocator semantics, or audit-payload shape.
  Does **not** supersede ADR-0009; the §6 decisions there (storno is
  itself an invoice; STORNO and MODIFY share API shape; technical
  annulment is separate via `manageAnnulment`; sequence numbers are
  never reused; chain link is ULID-keyed without cross-table FK)
  remain in force unchanged. Same extension pattern as ADR-0022 vs
  ADR-0009 §1.
- **Related:**
  - **ADR-0009 §6** (storno + modification chain — the surface this
    ADR pins for build).
  - **ADR-0009 §2** (invoice state machine — `Storno` / `Amended`
    side paths off `Finalized`; `InvoiceStornoIssued` already named
    as one of the audit-ledger typed kinds).
  - **ADR-0009 §3** (sequence allocator — storno and modification
    invoices consume sequence slots; the allocator path defined
    there is the path the storno command also walks).
  - **ADR-0009 §5** (idempotency Layer 1 + Layer 2 — both apply to
    the storno command the same way they apply to issuance).
  - **ADR-0009 §8** (audit-evidence retention — the per-invoice
    export bundle must traverse the storno chain across multiple
    ULID-linked invoices).
  - **ADR-0008** (audit ledger — typed `EventKind`, the F12 closed-
    set decoder, the per-payload typed struct discipline closed by
    F9 / PR-6.1).
  - **ADR-0019** (no foreign keys — chain link is ULID-by-payload,
    not a SQL FK to a parent row).
  - **ADR-0020 §1, §2** (NAV environment is explicit on the CLI
    surface — no default `--endpoint`).
  - **ADR-0022** (NAV runtime XSD validator — the storno's
    `<InvoiceData>` walks the same on-disk validation gate as a
    fresh issuance; no parallel validator).
  - Fortnightly review 2026-05-20, finding **F12** (`EventKind` row
    decoder is a closed set; growth requires hand-maintenance —
    closed in PR-6.1 by `from_storage_str`; the maintenance ritual
    remains and applies to PR-10's new variant).
- **Source material:** `docs/research/nav-and-billingo.md` §Storno
  and modification.

## Context

ADR-0009 §6 names the storno + modification chain decisions at the
right level for design phase: storno is its own invoice, MODIFY and
STORNO share the `manageInvoice` shape, the chain link is
`<invoiceReference>` + `<modificationIndex>`, technical annulment is
separate. The section was deliberately scoped to *what* not *how*;
session 12's handoff (`_handoffs/13-session-12-close.md`) names PR-10
as the storno-chain code surface and flags four pins as the work
session 13 must close before PR-10 can land without re-litigation:

1. **Operator command shape.** ADR-0009 §6 mentions
   `operation = STORNO` but does not name the `aberp` subcommand,
   its argument vocabulary, or its preconditions.
2. **EventKind variant + typed payload struct.** ADR-0009 §2 already
   names `InvoiceStornoIssued` (storage form `invoice.storno_issued`).
   What is **not** named: the matching Rust payload struct in
   `apps/aberp/src/audit_payloads.rs`, its field shape, or the F12
   four-edit ritual call-out so PR-10 follows the discipline closed
   in PR-6.1 / PR-7-B-3 / PR-8 without re-discovering it.
3. **`modificationIndex` allocator.** ADR-0009 §6 says the index
   "starts at 1 per base invoice; increments". It does not say where
   the allocation happens (same DuckDB transaction as the new
   invoice's sequence-number reservation?), nor how a migrated-from-
   Billingo base invoice gets its first ABERP-side index (the
   research file's `queryInvoiceChainDigest` path).
4. **MODIFY scope.** PR-10 is the **storno** PR. Whether MODIFY ships
   in PR-10 or PR-11 has not been pinned.

This ADR closes those four pins. It does not introduce any decision
that conflicts with ADR-0009 §6; it makes §6 build-ready.

### Surfaced conflict (CLAUDE.md rule 7)

Two adjacent documents describe the discipline PR-10 must follow,
and they count edits differently:

- `crates/audit-ledger/src/entry/event_kind.rs` (the source) names a
  **three coordinated edits** ritual: enum variant + `as_str` arm +
  `from_storage_str` arm. The "three" is what F12 / PR-6.1 closed
  the decoder side of.
- `_handoffs/13-session-12-close.md` (the handoff) names a **four-
  edit ritual**: enum + Display + audit_payloads typed struct +
  matching call site.

Both are partly right; neither is wrong about what code has to land.
This ADR pins **one** number to avoid Claude blending them at PR-10
time (CLAUDE.md rule 7 — surface conflicts, don't average). See §3.

## Decision

### 1. Operator CLI surface for storno

**Subcommand name:** `aberp issue-storno`.

**Rationale for the verb.** The CLI vocabulary today is
`issue-invoice` for the issuance path and `submit-invoice` /
`retry-submission` / `mark-abandoned` / `poll-ack` for the wire +
ledger paths. A storno is itself an invoice (ADR-0009 §6), so it
parallels `issue-invoice` rather than `submit-invoice`. The verb
`issue-storno` (not `storno`, not `cancel-invoice`, not
`void-invoice`) is chosen because:

- `cancel-invoice` is the wrong word — Hungarian law treats a storno
  as a *new invoice that legally negates a prior one*, not a
  cancellation of the prior row.
- `void-invoice` collides with the **void path** for an unsubmitted
  reservation (ADR-0009 §3, `Reserved → Voided`), which is a
  pre-submission accountant-treatment surface, not a storno.
- `storno` as a bare verb is two syllables shorter but loses the
  parallelism with `issue-invoice` that operators reading
  `aberp --help` will pattern-match on.

**Argument shape** (clap-flavoured, no defaults that hide a
decision; same posture as `submit-invoice` / `poll-ack` per
ADR-0020 §1):

| Flag | Type | Default | Purpose |
|---|---|---|---|
| `--references` | `String` (prefixed `inv_<ULID>`) | none (required) | The base invoice this storno cancels. **Must already be `Finalized` in the local typestate** (NAV terminal `SAVED`) — a storno against a not-yet-finalized invoice loud-fails before any ledger write. |
| `--in` | `PathBuf` (JSON spec) | none (required) | The storno's own line content. NAV requires the storno's `<InvoiceData>` to mirror the base invoice's structure with negated quantities or sign per the v3.0 schema; the JSON spec is the same shape as `issue-invoice --in` plus an implicit "this is a storno" flag set by the subcommand. |
| `--out` | `PathBuf` | none (required) | Path to write the storno's `<InvoiceData>` XML. Same on-disk gate as `issue-invoice --out`; the resulting bytes are what `submit-invoice` later POSTs. |
| `--db` | `PathBuf` | `./aberp.duckdb` | Tenant DuckDB file (same convention as the other subcommands). |
| `--tenant` | `String` | `"default"` | Tenant identifier (same convention). |
| `--series` | `String` | `"INV-default"` | Series the storno's own sequence number is drawn from. By default the same series as the base invoice; the operator can override iff the accountant has set up a dedicated storno series. **No silent series switch.** |

**What `issue-storno` does NOT do.** It does not call NAV. It walks
the same `issue-invoice` allocator path inside one DuckDB
transaction (ADR-0009 §3), writes the storno's own
`<InvoiceData>` XML on disk via ADR-0022's runtime validator, and
writes the storno-specific audit-ledger entries (§3 below). The
operator's next step is `submit-invoice --invoice-xml <storno.xml>
--invoice-id <storno-id> --endpoint {test|production}` against the
same wire path that `issue-invoice` outputs feed into. There is **no
second wire surface** for storno; PR-7-B-3's `submit-invoice` reads
the `operation` field out of the XML envelope.

**MODIFY is NOT part of PR-10.** A separate subcommand
`aberp issue-modification` will land in a later PR (likely PR-11 or
PR-12). The two share so much code that the storno code path is the
honest first build; the MODIFY-specific delta (no terminal status
change on the base invoice's typestate the way STORNO produces
`Storno`) is enough to justify a separate command surface.

### 2. EventKind variant + on-disk storage form

The storno's audit-ledger entry uses the variant **already named** in
ADR-0009 §2:

- Rust variant: `EventKind::InvoiceStornoIssued`.
- Storage form (per the `as_str` / `from_storage_str` round-trip
  contract): `"invoice.storno_issued"`.

No second variant is added. The audit ledger does not distinguish
"storno requested" from "storno reserved" from "storno XML written" —
the issuance itself is one transactional step (ADR-0009 §3) and a
single ledger entry suffices. The follow-up submit / poll-ack /
retry events for the storno's own invoice id reuse the existing
variants `InvoiceSubmissionAttempt` / `InvoiceSubmissionResponse` /
`InvoiceAckStatus` / `InvoiceRetryRequested` / `InvoiceMarkedAbandoned`
unchanged.

**The base invoice does NOT get a new ledger entry from the storno
issuance.** Its typestate transition (`Finalized → Storno` per
ADR-0009 §2) is **derived from** the existence of a successfully
issued storno whose `<invoiceReference>` field points at its
invoice number; no separate `invoice.amended_by` / `invoice.stornoed`
entry is written against the base. The chain link is carried in the
storno's own payload (§3) and the per-invoice export bundle
(ADR-0009 §8) traverses the chain by reading those payloads.

### 3. Typed payload struct + the F12 four-edit ritual

**Payload type name:** `InvoiceStornoIssuedPayload` in
`apps/aberp/src/audit_payloads.rs`. The trailing `Payload` matches
every other typed payload in that file (`InvoiceSequenceReservedPayload`,
`InvoiceDraftCreatedPayload`, `InvoiceSubmissionAttemptPayload`,
`InvoiceSubmissionResponsePayload`, `InvoiceAckStatusPayload`,
`InvoiceRetryRequestedPayload`, `InvoiceMarkedAbandonedPayload`).

**Field shape:**

```rust
pub struct InvoiceStornoIssuedPayload {
    /// The storno's own invoice id — prefixed `inv_<ULID>` form.
    pub storno_invoice_id: String,
    /// The storno's own sequence number (allocated in the same
    /// DuckDB transaction per ADR-0009 §3).
    pub storno_seq: u64,
    /// The storno's own sequence-reservation id (ULID-keyed,
    /// matches `InvoiceSequenceReservedPayload::reservation_id`).
    pub storno_reservation_id: String,
    /// The idempotency key of the `IssueStornoCommand` — same shape
    /// + role as on `InvoiceSequenceReservedPayload`.
    pub idempotency_key: String,
    /// The **base invoice's** id — prefixed `inv_<ULID>` form. This
    /// is the chain link: ULID-keyed per ADR-0019 (no cross-table
    /// FK), explicit per ADR-0009 §6.
    pub base_invoice_id: String,
    /// The **base invoice's** NAV-facing sequence number, captured
    /// verbatim so the per-invoice export bundle (ADR-0009 §8) can
    /// reconstruct the `<invoiceReference>` value without
    /// re-querying the base row. This is a denormalized field by
    /// design; the base row's `sequence_number` is the authoritative
    /// source and a periodic integrity scan (ADR-0009 §3 — startup
    /// reconciliation) detects any drift.
    pub base_sequence_number: u64,
    /// The `<modificationIndex>` this storno asserts against the
    /// base invoice's chain. Starts at 1 for the first chain entry
    /// against the base, increments for each subsequent storno or
    /// modification. Allocation rules per §4 below.
    pub modification_index: u32,
}
```

The `to_bytes(&self) -> Vec<u8>` shape matches every other payload
in the file (closes F9 trap — typed `serde_json::to_vec`, never
`format!`-built).

**The F12 four-edit ritual, pinned.** PR-10 lands **four** edits in
**four** files. The number "four" is what session 13 commits to;
event_kind.rs's older comment naming "three coordinated edits" was
written before PR-6.1 introduced typed payloads and counted only the
edits inside event_kind.rs itself. The four edits PR-10 must land:

| # | File | Edit |
|---|---|---|
| 1 | `crates/audit-ledger/src/entry/event_kind.rs` | Add `InvoiceStornoIssued` variant + matching `as_str` arm (storage form `"invoice.storno_issued"`) + matching `from_storage_str` arm + extend the `round_trip_for_every_variant` test's variant list. **All four sub-edits travel together by F12's closed-set discipline; a missing arm fails compilation, which is the property the closed set buys.** Counted as one edit-kind because all four sub-edits live in the same file and must land in the same commit. Line-36 doc comment in this file already mentions `StornoIssued` as an *anticipated* future kind; PR-10 graduates the doc-comment hint to an actual variant. |
| 2 | `apps/aberp/src/audit_payloads.rs` | New `InvoiceStornoIssuedPayload` struct + `new(...)` constructor + `to_bytes(&self)` + round-trip test on hostile inputs (mirroring the existing `*_round_trips_*` test fixtures, especially the JSON-hostile-bytes fixture that closes F9). |
| 3 | `apps/aberp/src/cli.rs` | New `Command::IssueStorno(IssueStornoArgs)` variant + the `IssueStornoArgs` struct per §1 above. |
| 4 | `apps/aberp/src/issue_storno.rs` | New file — `run` function plus its `run_single_tx`-shaped helper that emits `EventKind::InvoiceStornoIssued` via the typed payload, walking the §4 allocator path. The transactional discipline matches `apps/aberp/src/issue_invoice.rs`'s `run_single_tx` shape; cross-crate transactional deviation is the failure mode F-CLOSED-X1 closed and PR-10 inherits that closure. |

This ADR is the canonical definition of the four edits for storno.
If PR-10 review surfaces a fifth file needing a change, the fifth
file is a *consequence* of a §1-§4 decision being unclear, not a
five-edit ritual; that is a PR-10 review finding, not a PR-10
re-architecting.

### 4. `modificationIndex` allocator

**Rule.** The `modification_index` for a new storno against a base
invoice is allocated as `max(existing chain indices) + 1`, where the
"existing chain indices" are:

- All `InvoiceStornoIssuedPayload::modification_index` values in the
  audit ledger whose `base_invoice_id` equals the new storno's
  target base.
- (When MODIFY lands in a later PR) all
  `InvoiceModificationIssuedPayload::modification_index` values
  against the same base.

If the chain is empty, the first index is `1` (matches NAV's
spec). The allocation happens **in the same DuckDB transaction** as
the storno's own sequence-number reservation + invoice-row insert +
audit-ledger entry — same transactional shape ADR-0009 §3 names for
issuance, extended by one more query that reads the prior chain
indices.

**Why a single transaction.** Two concurrent storno commands against
the same base invoice (operator double-clicks, retry crosses with
original) would otherwise race on the index. DuckDB's single-writer
serialization (ADR-0009 §3) closes this today; on the Postgres-per-
tenant adapter (ADR-0016) the equivalent is a `SELECT ... FOR
UPDATE` on the base invoice row keyed by `inv_<ULID>` — the base
row is the natural serialization point.

**Migrated-from-Billingo base invoices.** ADR-0009 §6 already names
the `queryInvoiceChainDigest` path for base invoices that were
originally issued in Billingo and reported to NAV by Billingo. The
PR-10 path is:

1. If the base invoice's local row carries `origin = Billingo`
   (ADR-0010's migration-read shape), `issue-storno` first calls
   `queryInvoiceChainDigest` against NAV to learn the canonical
   chain (which may include amendments Billingo issued that ABERP
   has no local audit record of).
2. The new ABERP-issued storno's `modification_index` is set to
   `max(NAV-returned chain indices) + 1`.
3. The `queryInvoiceChainDigest` request + response XML are written
   to the audit ledger as **one** `InvoiceSubmissionAttempt` +
   `InvoiceSubmissionResponse` pair tagged with the storno's own
   invoice id (so the per-invoice export bundle for the storno
   carries the chain-discovery evidence the NAV inspector will want
   to see).

If `queryInvoiceChainDigest` fails (NAV transient, schema mismatch,
or any non-success), the storno transaction **rolls back loudly**
(CLAUDE.md rule 12 — no `modification_index = 1` fallback on a
migrated base, ever). The operator alert mirrors `SubmissionStuck`.

**Local base invoices (issued by ABERP).** The local audit ledger
is authoritative; no `queryInvoiceChainDigest` call is made. The
chain-indices query reads every `EventKind::InvoiceStornoIssued`
entry (later also `InvoiceModificationIssued`) and filters by
`base_invoice_id` in Rust after `serde_json::from_slice` on the
payload. The audit-ledger storage layer's surface today is
`Vec<u8>`-shaped payloads (ADR-0008 + `apps/aberp/src/audit_payloads.rs`
header); whether PR-10 introduces a query helper on the audit-ledger
crate or scans-and-parses in `issue_storno.rs` is a PR-10
implementation detail, not pinned here. **What is pinned:** the
chain-link source of truth is the payload, not a separate index
column. The same periodic integrity scan that ADR-0009 §3 names for
sequence reservations is extended to also verify chain-index
continuity per base invoice; gaps surface as
`invoice.reconciliation_anomaly` ledger entries (ADR-0009 §3
startup reconciliation).

### 5. Idempotency for `issue-storno`

Both layers of ADR-0009 §5 apply unchanged.

- **Layer 1 — client-side idempotency key.** The
  `IssueStornoCommand` carries its own ULID
  (`IdempotencyKey::new()`); on retry of the same key, the prior
  storno is returned, no second allocation happens.
- **Layer 2 — NAV-side reconciliation.** Does **not** fire for
  `issue-storno` directly. The storno's own NAV submission goes
  through `submit-invoice`, which inherits the §5 Layer-2 path
  unchanged (`queryInvoiceCheck` on the storno's own invoice
  number).

**Cross-storno idempotency.** If the operator runs `issue-storno
--references inv_A` twice without retrying the same command, the
second invocation produces a **second** storno against `inv_A`. NAV
will accept that (the second storno gets `modification_index = 2`)
because two storno operations against the same base are
API-permitted. The accountant treatment of "did you mean to do that"
is out of scope for code; the per-invoice export bundle makes both
storno entries visible and an audit catches the policy violation.
**[OPEN, accountant]** Whether ABERP should refuse a second storno
against an already-stornoed base by default; if yes, this becomes a
typestate constraint at the §1 precondition check (`--references`
target must be in `Finalized`, not `Storno`). The data model
supports either. Default until the accountant question resolves:
**allow** (matches NAV API permissiveness).

### 6. Technical annulment remains distinct (re-asserted)

Technical annulment is **still not** a storno. ADR-0009 §6 already
draws the line; this ADR re-asserts it because the storno code surface
is the most likely place an operator (or a future contributor) will
conflate the two. Concretely:

- A `RequestTechnicalAnnulment` command is **not** in scope for
  PR-10. It will land in a separate PR (likely PR-11).
- `aberp issue-storno` does not call `manageAnnulment`. It calls
  nothing on the NAV side; the storno's own `submit-invoice` later
  calls `manageInvoice` with `operation = STORNO`.
- The keychain artifacts the storno path uses are the same four
  named in ADR-0020 §3. Technical annulment, when filed, uses the
  same four (no new keychain artifact is introduced by either).
- The audit-ledger entry **`invoice.technical_annulment_requested`**
  already named in ADR-0009 §2 belongs to the technical-annulment
  PR, not to PR-10. PR-10 must **not** add it speculatively
  (CLAUDE.md rule 2 — no speculative abstractions).

### 7. Storno-of-a-storno — [OPEN, accountant] re-scoped

ADR-0009 §"Open questions" carries "Storno-of-a-storno practice
(accountant question). API-permitted; Hungarian accounting
convention may prefer a fresh corrective. Affects operator command
vocabulary, not the data model." That posture is preserved.

**What this ADR adds:** PR-10's `issue-storno --references` accepts
a target whose typestate is `Finalized` *or* `Storno` (no compile-
time refusal of storno-of-a-storno; the API permits it, ABERP
permits it, and the accountant chooses by policy). When the
accountant question resolves:

- **If Hungarian practice forbids storno-of-a-storno**, the §1
  precondition `--references` target must be in `Finalized` (not
  `Storno`); PR-10's `issue_storno::run` adds the precondition
  check and rejects loudly with a named error. This is a one-line
  change, not a re-architecting.
- **If Hungarian practice permits storno-of-a-storno**, the
  default behaviour stays as-is.

**Decision until resolved:** permit, with a tracking item in the
fortnightly review. The accountant review is the gating event.

### 8. Open questions inherited from ADR-0009 §6

These are **not** changed by this ADR; they are re-listed so PR-10
review has the full picture:

- **Void treatment for unused reservations** (accountant question,
  ADR-0009 §3). Unaffected by storno; the void path is the
  pre-submission cancel surface, not the storno surface.
- **Storno-of-a-storno practice** — see §7 above for the build-
  phase posture.
- **NAV response signing** (ADR-0020 §6 open question). The storno
  walks the same TLS-only response path the issuance walks; if NAV
  signing lands, ADR-0020's amendment trigger absorbs the storno
  responses verbatim from the ledger without code change.

## Consequences

**What gets easier**

- PR-10 lands without re-litigating naming, allocator shape, payload
  fields, or the four-edit count. The pre-flight reading for PR-10 is
  this ADR plus ADR-0009 §3 + §6 + §8 plus `apps/aberp/src/issue_invoice.rs`.
- The per-invoice export bundle (ADR-0009 §8) can traverse a storno
  chain by following `InvoiceStornoIssuedPayload::base_invoice_id` —
  no separate "chain table" is needed; the audit ledger *is* the
  chain.
- A future MODIFY PR has a template: `InvoiceModificationIssuedPayload`
  mirrors `InvoiceStornoIssuedPayload`'s shape with one extra field
  (the `<modificationIssueDate>` timestamp NAV requires for MODIFY
  but not for STORNO). The four-edit ritual structure ports directly.

**What gets harder**

- The denormalized `base_sequence_number` field in the storno
  payload introduces a drift risk between the payload value and the
  base row's `sequence_number`. Mitigated by the periodic integrity
  scan extension (§4) and the immutability of the base row's
  sequence number after issuance — drift can only happen via direct
  DB tampering, which is what the audit ledger's tamper-evident
  hash chain (ADR-0008) makes visible.
- Migrated-from-Billingo storno requires a synchronous NAV call
  (`queryInvoiceChainDigest`) inside the `issue-storno` transaction.
  The §4 rollback-loudly posture on a NAV failure is the right
  trade — operators can re-run when NAV is reachable — but it
  couples the issuance UX to NAV reachability for the migrated-base
  case. ABERP-issued base storno has no such coupling.

**What we lock ourselves into**

- Subcommand name `aberp issue-storno` and arg names (`--references`,
  `--in`, `--out`, `--db`, `--tenant`, `--series`). Rename requires
  an amendment ADR; operators will have learned the surface.
- Payload struct name `InvoiceStornoIssuedPayload` and field names.
  Adding a field is backward-compatible per
  `apps/aberp/src/audit_payloads.rs`'s schema-versioning note;
  removing or renaming a field requires a new `EventKind` variant
  (the rule that file already pins). PR-10 must not pre-add fields
  it does not need.
- The four-edit ritual at four. A fifth file needing a change for a
  later `EventKind` variant is a PR review finding against this
  ADR, not a silent re-shape of the ritual.

## Adversarial review

A hostile NAV inspector and a hostile-engineer review, in alternation.
The ADR-README bar is three; four are surfaced because the storno
chain is the surface the project owner has named "would a NAV
inspector accept this?" most explicitly.

1. **"Two operators issue `issue-storno` against the same base
   invoice at the same wall-clock moment. The local audit ledger
   sees two `modification_index = 1` allocations. NAV rejects the
   second with `INVOICE_NUMBER_NOT_UNIQUE`. ABERP now has a stuck
   storno whose `modification_index` is locally consistent and
   NAV-inconsistent."** The §4 same-DuckDB-transaction allocator
   closes the local-side race: under DuckDB single-writer, the
   second `issue-storno` blocks on the first's transaction, reads
   the post-commit chain state, and allocates `modification_index =
   2`. The hostile case the reviewer names is the *cross-process*
   race — two `aberp` invocations against the same DB file. DuckDB
   file-locking serializes them; the second will see the first's
   committed chain entry before allocating. The Postgres-per-tenant
   variant (ADR-0016) uses `FOR UPDATE` on the base row. **Accepted.**

2. **"`base_sequence_number` is denormalized — you copied a fact
   from the base row into the storno payload. The base row could be
   tampered with later; the payload is hash-chained but only its
   own bytes are. The reviewer can falsify the base row's
   `sequence_number` after the fact and the chain still verifies."**
   Two protections close this. (a) The base row's `sequence_number`
   is allocated inside the same transaction as the
   `InvoiceSequenceReservedPayload` ledger entry whose `seq` field
   carries the same number — so any tampering of the base row is
   detected by comparing the row against the original
   `InvoiceSequenceReservedPayload` (ADR-0008 §"Reconciliation").
   (b) The startup integrity scan (§4 extension) cross-checks the
   storno chain's `base_sequence_number` against the base row's
   `sequence_number`; mismatch produces an
   `invoice.reconciliation_anomaly` entry. The denormalization is
   *not* defended by hash-chain alone — it is defended by the
   integrity scan + the immutability discipline ADR-0019 already
   establishes. **Accepted with the integrity-scan extension pinned
   in §4.**

3. **"You permit storno-of-a-storno by default with an [OPEN]
   accountant question. A hostile NAV inspector argues the
   accountant convention forbids it; you have shipped code that
   permits it. You will have to retroactively flag the chain."**
   The §7 default-permit posture is the API-permitted posture and
   matches the research-file's open question. If the accountant
   review reverses to forbid, the one-line precondition tightening
   in `issue_storno::run` rejects the offending command at the
   operator boundary going forward; the in-flight chain is not
   retroactively rewritten (audit-ledger entries are immutable per
   ADR-0008). The inspector's reconstruction reads the chain as it
   was issued; the policy change is dated. **Accepted — the open
   question is the right shape, soft-asserting "forbidden" today
   would be the soft-assertion failure mode CLAUDE.md rule 12
   names.**

4. **"PR-10 does not ship MODIFY. An operator who learns the storno
   surface will reach for MODIFY before PR-11 lands and find
   nothing. The audit ledger will carry two PRs of stornos that
   were *actually* modifications a strict accountant would have
   filed differently. You have built a partial surface."** Yes, on
   purpose. The project's framing (`aberp_project.md`) is NAV-
   compliant invoicing as the keystone; the storno surface is the
   higher-stakes one because a *legal cancellation* error is
   harder to correct than an amendment error. PR-10 ships storno,
   PR-11 ships MODIFY (or PR-11 ships technical annulment and
   PR-12 ships MODIFY; the order between the two is a separate
   sequencing decision tracked in the handoff). The interim
   stornos-issued-where-MODIFY-would-be-cleaner case is an operator
   policy question; the per-invoice export bundle makes it
   visible. **Accepted as scope.**

## Alternatives considered

- **Single subcommand `aberp issue-chain-invoice --operation
  {storno|modify}`.** Rejected. The two operations have *different*
  preconditions (storno requires base in `Finalized`; MODIFY's base
  may be in `Finalized` or already `Amended`) and *different*
  audit-payload shapes (MODIFY carries `<modificationIssueDate>`,
  storno does not). Forcing them through one CLI flag at the cost
  of `clap` field gymnastics makes the operator-visible surface
  *less* clear, not more. CLAUDE.md rule 2 (no speculative
  abstractions).

- **Compute `modification_index` from NAV every time via
  `queryInvoiceChainDigest`.** Rejected for local base invoices.
  The local audit ledger is authoritative for invoices ABERP itself
  issued; querying NAV on every storno against a local base couples
  issuance to NAV reachability for no audit-evidence gain (NAV's
  view is a strict subset of ABERP's). For migrated-from-Billingo
  bases the trade-off reverses (NAV is authoritative because
  Billingo issued the base) and §4 keeps `queryInvoiceChainDigest`
  in the path there.

- **Add `EventKind::InvoiceStornoRequested` as a separate "operator
  intent" entry alongside `InvoiceStornoIssued`.** Rejected. The
  `IssueInvoiceCommand` path does not write a separate
  `InvoiceIssueRequested` entry; issuance is one transactional
  step with one ledger entry. The storno path mirrors that. The
  one-entry posture also closes a potential ordering bug where a
  "requested" entry is committed but the "issued" entry is not —
  with one entry, there is no inconsistent intermediate state.

- **Refuse second storno against an already-stornoed base by
  default.** Rejected as a §7 default; tracked as the open
  accountant question. The data model supports either. Soft-
  asserting "forbidden" today is the rule-12 failure mode.

- **Write a new `invoice.amended_by` ledger entry against the base
  invoice when a storno is issued.** Rejected. The base row's
  typestate (`Finalized → Storno`) is derived from the existence of
  a successful storno's payload pointing at the base; a separate
  ledger entry against the base would be a second source of truth
  for the same fact, and the audit ledger is the *only* source of
  truth in this codebase (ADR-0008). Derivation by query is the
  consistent posture.

- **Run `issue-storno`'s `queryInvoiceChainDigest` call outside the
  DuckDB transaction (sequence-allocation only inside the tx,
  chain-discovery before).** Rejected. The chain-discovery must
  see the same world the allocator sees; an interleaving operator
  command could land between the two and shift the chain. Inside
  the same transaction is the simpler invariant. The NAV-call-
  inside-transaction smell is mitigated by the rollback-loudly
  posture on failure — the failure mode is "no storno issued" not
  "half-issued storno", which is what we want.

## Open questions

Tracked against the next adversarial-review cadence and the named
external-check items in `docs/research/nav-and-billingo.md`:

- **Storno-of-a-storno accountant practice.** Re-listed; default-
  permit until resolved (§7).
- **Second-storno-against-finalized accountant practice.** Re-listed
  from §5; default-allow until resolved.
- **MODIFY PR sequencing.** Whether PR-11 is MODIFY or technical
  annulment is a separate decision tracked in the handoff
  (`_handoffs/13-session-12-close.md`), not this ADR.
- **`queryInvoiceChainDigest` exact response shape for migrated
  bases.** The NAV response carries the full chain including the
  Billingo-issued entries; the PR-10 parser must tolerate Billingo's
  `<softwareId>` field present in those entries. Tracked in the
  research file's open-questions list.
- **Integrity-scan extension cadence.** ADR-0009 §3 names a startup
  reconciliation; §4 above extends it to also verify chain-index
  continuity. Whether the extension runs only at startup or also at
  a scheduled cadence (daily, like the §8 `queryInvoiceDigest`
  reconciliation) is deferred to the first integrity-scan PR.

## Follow-on PRs unblocked by this decision

- **PR-10 — Storno chain (code).** Implements the four edits in §3
  above plus `apps/aberp/src/issue_storno.rs` and the matching
  unit + integration tests. Per the session-12 handoff: the F12
  four-edit ritual is named, the subcommand shape is named, the
  payload shape is named, the allocator semantics are named.
  Nothing in PR-10 should require an ADR re-read at code-review
  time.
- **PR-11 (or PR-12) — MODIFY chain (code).** Mirrors PR-10's
  structure with the MODIFY-specific delta (`<modificationIssueDate>`,
  no base typestate transition to `Storno`). The four-edit ritual
  template ports.
- **Technical annulment PR (PR-11 or later).** Distinct command
  surface (`aberp request-technical-annulment`), distinct NAV
  endpoint (`manageAnnulment`), distinct ledger entry kind
  (`invoice.technical_annulment_requested` — already named in
  ADR-0009 §2). Not in scope for PR-10 (§6 re-assert).
- **First per-invoice export-bundle PR (gated on F5 + F10 per
  session 12 handoff).** Consumes the storno-chain payloads via
  the `base_invoice_id` ULID traversal pinned in §3.
