# ADR-0008 — Tamper-evident audit ledger

- **Status:** Accepted
- **Date:** 2026-05-19
- **Deciders:** Ervin

## Context

A NAV audit, a customer dispute, or a security incident all share one demand:
*can you prove what happened, in what order, by whom, and that the record has
not been altered after the fact?* A standard application log cannot answer this
— logs are mutable, easily rotated away, and trivially editable by anyone with
filesystem access.

We need a record that is:

- **Append-only** in the operational sense (no API to delete or edit).
- **Tamper-evident** in the cryptographic sense (alteration is detectable).
- **Complete enough** to reconstruct the lifecycle of any business entity.
- **Cheap enough** to write to from every state-changing command.
- **Exportable** for legal evidence.

## Decision

ABERP maintains, per tenant, a **hash-chained append-only audit ledger**.

### Entry shape

Each entry contains:

- `id` — ULID (entity prefix `aud`).
- `seq` — contiguous per-tenant 64-bit sequence number (the position in the chain).
- `prev_hash` — SHA-256 of the previous entry's canonical bytes; the first entry uses a tenant-specific genesis hash.
- `time_wall` — wall-clock timestamp (RFC3339 with timezone).
- `time_mono` — monotonic timestamp (nanoseconds since process start); for ordering checks.
- `actor` — session ID + user ID + capability set used.
- `binary_hash` — SHA-256 of the binary that produced the entry (recorded once per process start; referenced).
- `tenant_id` — for cross-tenant safety in exports.
- `kind` — typed event kind (e.g., `invoice.issued`, `stock.moved`, `label.printed`).
- `payload` — opaque structured data, schema versioned per `kind`.
- `idempotency_key` — the key the command was invoked with, if any.
- `entry_hash` — SHA-256 over the canonical-serialized entry minus this field.

### Hash chain

`entry_hash[N] = SHA-256(canonical(entry[N] with prev_hash = entry_hash[N-1]))`

Verification is a linear pass: recompute each hash, compare. Any divergence
identifies the first tampered entry.

### Storage

- The ledger lives in its own DuckDB table inside the tenant database, with
  a unique index on `seq` and `id`.
- Entries are written **in the same transaction** as the state change they
  describe. If the state change rolls back, the ledger entry rolls back too.
  No "log, then do" or "do, then log" gaps.
- The ledger is also mirrored to an append-only file (`<tenant>.audit.log`)
  outside the DB on every commit, fsync'd. This protects against DB-file
  corruption and gives us a second-source artifact for export.

### External attestation (lightweight)

- Every N entries (configurable, default 1000) or every M minutes (default 60),
  the latest `entry_hash` is recorded as an **attestation checkpoint** in a
  separate file with its own signature.
- Cloud deployments additionally publish attestation checkpoints to a tenant-
  external trust anchor (decided in ADR-0016).
- This means an attacker who alters the ledger must also alter every published
  checkpoint that covers the altered entry — significantly harder than
  rewriting one file.

### What goes in the ledger

- Every business state change (invoice issued, payment recorded, stock moved,
  order shipped, label printed, robotics task dispatched, CAD artifact stored).
- Every external submission and response (NAV submit, NAV ack, Billingo pull).
- Every authentication event (session created, capability used, session revoked).
- Every configuration change (tenant settings, sequence series, integration
  credentials rotated — credential values are **not** in the payload).

What does **not** go in the ledger:

- Read-only queries (those go to the normal log).
- Per-keystroke UI state.
- Anything containing a secret.

### Access

- **Append-only** API: `Ledger::append(entry)` is the only write path.
- **Read** APIs: `verify_chain`, `entries_for(entity_id)`, `export_range`.
- No update or delete API exists. Not "exists but disabled" — absent from
  the type system.

### Export

- A tenant's full ledger can be exported as a signed bundle: entries + every
  attestation checkpoint + the binary hashes referenced + the schema versions
  used. The bundle is independently verifiable.

## Consequences

- Every write path takes a small extra cost (hash, append, fsync on the
  mirror file). Measured, not assumed; budget is set per command and CI
  fails if a command exceeds it.
- Storage grows linearly with activity. Ledger compaction is **not** allowed.
  Cold-storage offload (older ranges signed and archived) is allowed and
  designed later.
- Disputes become tractable: "show me everything that happened to invoice
  inv_01J…" is a single query, returning a verifiable chain.

## Adversarial review

- *"The DB file is editable; what stops an attacker from rewriting both the
  DB and the mirror file?"* — Nothing locally, by themselves. The attestation
  checkpoints — signed and (on cloud) externally published — raise the bar:
  the attacker must also rewrite or compromise the checkpoint history. We
  document the residual risk in the threat model.
- *"What if the binary itself is replaced?"* — The `binary_hash` field
  records which binary produced each entry. A new binary appears as a new
  hash; the chain still verifies. If the new binary is malicious, the audit
  still tells the story of *when the binary changed*.
- *"What about clock manipulation?"* — Wall clock can be moved, monotonic
  cannot (within a process). On process start, the wall/mono pair is
  recorded; large divergence between processes is detectable.
- *"Hash-chain verification on every read is expensive."* — Verification is
  not done on every read. It is done on demand (export, audit, dispute) and
  periodically in a background task. The chain is structurally correct on
  write.
- *"Where does the attestation signing key live?"* — In the OS keychain on
  desktop, in a managed secret store on cloud. Rotation is supported; old
  keys remain valid for verification.

## Alternatives considered

- **Standard append-only log file, no hash chain** — easy to silently edit.
  Refused.
- **Database table with timestamps but no chain** — same problem; rows are
  editable.
- **External ledger service (e.g., a blockchain or QLDB equivalent)** —
  external dependency, network requirement, vendor lock. Refused for now.
  Cloud-time addition is allowed and would only strengthen attestation.
- **WORM filesystem** — platform-specific, not portable across local desktops
  and cloud. Doesn't replace the chain anyway.

## Open questions

- Attestation checkpoint cadence — defaults are starting points; tuned at
  first integration test.
- Long-term retention and cold-storage policy — separate ADR before scale
  forces the issue.
- Cross-tenant attestation publishing (cloud) — ADR-0016.
