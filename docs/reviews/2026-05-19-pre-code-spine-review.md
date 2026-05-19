# 2026-05-19 — Pre-code full-spine adversarial review

- **Reviewer:** Claude (session 4), under direction from Ervin.
- **Scope:** All currently-Accepted Spine ADRs (0001, 0002, 0004,
  0005, 0006, 0007, 0008, 0019, 0020, 0021) plus the now-Accepted
  module-level ADR-0010 and ADR-0009.
- **Gate:** This review is the pre-code gate per the session-4
  plan. Code may begin in session 5 only after every finding below
  is either resolved or explicitly deferred with a named trigger.
- **Methodology:** Read each ADR top-to-bottom against the
  near-term build target ("smallest thing that exercises ADR-0008,
  ADR-0009, ADR-0019 end-to-end — a binary that generates a
  NAV-compatible invoice XML on disk without submitting" — project
  memory). For each ADR, ask: what does commit #1 need from this
  ADR that the ADR does not yet pin?
- **Bias check:** This is the first adversarial review of
  ADR-0021 specifically; that ADR was authored in the same session
  as this review by the same agent. Self-review carries a known
  blind spot — findings below try to compensate by checking the
  ADR against a different question (commit-#1 buildability)
  rather than re-running its own §Adversarial-review.

## Headline

**Three blocking findings**, all against ADR-0021 Part A. The
pre-code gate is **NOT cleared** until ADR-0021 amendments below
close them. Recommendation: amend ADR-0021 in place today (it has
just been filed; the amendment is surgical), then re-mark Accepted
once the amendments land.

Four further findings are tracked items, not blockers.

## Findings

### F1. ADR-0021 Part A omits the DuckDB driver crate. **BLOCKER.**

ADR-0019 (storage strategy) and ADR-0008 (audit ledger) both
require a working DuckDB integration at commit #1: ADR-0008
§Storage states *"The ledger lives in its own DuckDB table inside
the tenant database"*; project memory names ADR-0008 as one of
the three ADRs that commit #1 exercises end-to-end. ADR-0021 Part
A enumerates nine crate categories and does not include a DuckDB
binding.

The Rust ecosystem pick is the `duckdb` crate (MIT) — a thin
binding over the DuckDB C/C++ library via FFI. ADR-0001 already
permits `unsafe` in adapters; the DuckDB binding sits in the
adapter layer per ADR-0006.

**Resolution required:** ADR-0021 Part A gets an item §10 pinning
`duckdb` at the current minor with a `bundled` feature so the
DuckDB native library ships in the binary (cleaner for ADR-0007's
reproducible-build posture than relying on a system DuckDB).

### F2. ADR-0021 Part A omits a date / time crate. **BLOCKER.**

ADR-0008 §"Entry shape" requires RFC3339 timestamps. ADR-0020 §2
+ research file §"Auth and request signing" require NAV's
`requestTimestamp` in `YYYYMMDDhhmmss` UTC format. Both are
date/time operations; commit #1 cannot generate a NAV-compatible
XML without one and cannot write an audit-ledger entry without
one.

Rust options: `time` (MIT/Apache-2.0 dual; modern, designed for
correctness, smaller surface) or `chrono` (MIT/Apache-2.0 dual;
older, larger API surface). The `time` crate is the conservative
pick for a fresh project — narrower API, no timezone-database
dependency unless explicitly opted into.

**Resolution required:** ADR-0021 Part A gets an item §11 pinning
`time` at the current 0.3.x minor with the `formatting`,
`parsing`, `macros`, `serde`, `serde-well-known` features.

### F3. ADR-0021 Part A does not pin a canonical-serialization scheme for the audit-ledger hash chain. **BLOCKER.**

ADR-0008 §"Hash chain" defines `entry_hash[N] = SHA-256(canonical
(entry[N] with prev_hash = entry_hash[N-1]))` but does not specify
what *canonical* means in bytes. ADR-0021 Part A does not pin a
deterministic encoder either. Two implementations of ADR-0008 by
two engineers will produce diverging chains.

Realistic Rust options:

- **CBOR with the canonical / deterministic encoding rules** via
  `ciborium` (MIT/Apache-2.0 dual). CBOR has an RFC-defined
  canonical mode (RFC 8949 §4.2.1).
- **Hand-rolled byte layout** with length-prefixed fields in a
  fixed order. No external crate but more code to review.
- **Canonical JSON** via `serde_canonical_json` or similar. Less
  mature, less audited.

CBOR via `ciborium` is the conservative pick: RFC-defined
canonical mode, mature library, no FFI, dual MIT/Apache-2.0,
small dependency footprint.

**Resolution required:** ADR-0021 Part A gets an item §12 pinning
`ciborium` with a note that ADR-0008's "canonical-serialized" is
concretized as CBOR canonical encoding (RFC 8949 §4.2.1). The
note should also call out that the hash-chain function lives in
the audit-ledger crate, not at every call site, so the canonical-
encoding semantics are conformance-checkable.

### F4. ADR-0021 Part A does not explicitly enumerate the `ulid` crate. Tracked, not blocking.

ADR-0005 names `ulid` ("The `ulid` crate handles this"). ADR-0021
Part A pins eight other named crates explicitly; the `ulid`
crate is implicit by reference to ADR-0005. For symmetry and
because `Cargo.toml` will need an explicit dependency line at
commit #1, ADR-0021 should enumerate it.

**Resolution suggested (non-blocking):** Add `ulid` to Part A as
an item §13 (or fold into a "and the following names already
pinned by other ADRs" subsection). License: MIT.

### F5. ADR-0008 attestation signing key — type not pinned. Tracked, not blocking commit #1.

ADR-0008 §"External attestation" names an attestation signing key
in the OS keychain but does not specify the key algorithm
(RSA-2048? Ed25519?). Commit #1 (XML on disk without submitting)
produces well under 1000 audit-ledger entries; the default
attestation cadence is 1000 entries OR 60 minutes (ADR-0008 §
External attestation). A binary that runs once and exits within
60 minutes never triggers attestation, so commit #1 does not
exercise this.

**Resolution suggested (deferred to build phase):** Add to
ADR-0021 §Items deferred to build phase: "Attestation signing
key type. Trigger: first PR that exercises attestation
checkpointing — i.e., a long-running process or a test that
forces a cadence trigger." Recommend Ed25519 when filed
(smaller, faster, no parameter choices).

### F6. Keychain Rust binding crate — not pinned. Tracked, not blocking commit #1.

ADR-0007 §Secrets requires OS-keychain access across macOS,
Windows, and Linux. ADR-0021 does not pin a Rust binding. The
ecosystem pick is the `keyring` crate (MIT/Apache-2.0 dual).
Commit #1 (XML on disk, no submission) does not need a real
keychain credential — it can read from environment variables or
a test fixture for the technical-user credentials. Real keychain
access is exercised at commit #N where N is the first PR that
talks to NAV.

**Resolution suggested (deferred to build phase):** Add to
ADR-0021 §Items deferred to build phase: "OS-keychain Rust
binding crate. Trigger: first PR that performs a real NAV
submission or any other path that loads keychain-bound material
in production code." Recommend `keyring` when filed.

### F7. ADR-0020 [OPEN] on NAV response-body integrity — unchanged. Tracked.

ADR-0020 §6 surfaces that response-body signing is [OPEN]; the
research file calls for an external check with a Hungarian
developer with shipped NAV experience. This is unresolved as of
2026-05-19 and remains an external-check item. **Does not block
commit #1** because commit #1 does not submit to NAV.

**Resolution:** No action this session. The item carries forward.

## Sweep notes (per ADR)

- **ADR-0001 (Rust).** No findings. ADR-0021 closes the
  stack-baseline gate ADR-0001 named.
- **ADR-0002 (DB-per-tenant).** No findings. Tenant-registry
  implementation is build-phase work; not a pre-code gate.
- **ADR-0004 (Tauri + Svelte).** Wire-protocol gate closed by
  ADR-0021 Part B. No further findings.
- **ADR-0005 (ULIDs).** See F4.
- **ADR-0006 (Module boundaries).** No findings. Conformance test
  is build-phase wiring; not a pre-code gate.
- **ADR-0007 (Security baseline).** See F6 (keychain crate).
  Otherwise consistent with ADR-0020 partial supersede and with
  ADR-0021 pinning `rustls` + JSON-via-`tracing-subscriber` for
  the logging stack. License allow-list cleared against every
  ADR-0021 pin.
- **ADR-0008 (Audit ledger).** See F3 (canonical serialization)
  and F5 (attestation signing key).
- **ADR-0009 (NAV invoice issuing).** No findings. Schema-drift
  detection (§1) is the named trigger for the deferred XSD-
  validation crate decision, already tracked in ADR-0021.
- **ADR-0010 (Billingo migration).** No findings. The keychain
  entry `billingo.api_key` is named explicitly in §1; lifecycle
  is migration-bounded. NAV historical read path is deferred
  with named trigger.
- **ADR-0019 (Storage strategy).** See F1 (DuckDB binding).
- **ADR-0020 (NAV transport correction).** See F7. Otherwise
  consistent.
- **ADR-0021 (Pre-code consolidated baseline).** See F1, F2, F3,
  F4. The ADR is structurally sound; the gaps are real but
  surgical to close.

## Cross-cutting check: would commit #1 build today?

Walking through the artifacts a commit-#1 binary requires:

| Artifact | ADR | Crate pinned? |
|---|---|---|
| Async runtime | 0021 §A1 | yes (`tokio`) |
| Error types | 0021 §A2 | yes (`thiserror`, `anyhow`) |
| Structured logs | 0021 §A3 | yes (`tracing` + subscriber) |
| CLI parsing | 0021 §A4 | yes (`clap`) |
| HTTP client (NAV — not used in commit #1) | 0021 §A5 | yes (`reqwest`) — N/A for commit #1 |
| HTTP server (UI — not used in commit #1) | 0021 §A6 | yes (`axum` + `axum-server`) — N/A for commit #1 |
| JSON serialization | 0021 §A7 | yes (`serde` + `serde_json`) |
| XML / SOAP serialization | 0021 §A8 | yes (`quick-xml`) |
| Cryptography (SHA-512 / SHA3-512 / AES) | 0021 §A9 | yes (`sha2`, `sha3`, `aes`) |
| ULID generation | 0005 | named, not enumerated in 0021 — **F4** |
| DuckDB binding | 0019, 0008 | **not pinned — F1 (BLOCKER)** |
| Date / time | 0008, 0020 | **not pinned — F2 (BLOCKER)** |
| Canonical-encoded bytes for audit hash chain | 0008 | **not pinned — F3 (BLOCKER)** |
| Self-signed cert generation (UI — not used in commit #1) | 0021 §Sub-decisions | yes (`rcgen`) — N/A for commit #1 |
| OS keychain (NAV — not used in commit #1) | 0007 | not pinned — **F6** (N/A for commit #1) |
| Attestation signing key (cadence not triggered in commit #1) | 0008 | not pinned — **F5** (N/A for commit #1) |

The pre-code gate is blocked on F1, F2, F3. F4 is tracked.
F5, F6, F7 are deferred with named triggers.

## Action plan

1. **Amend ADR-0021 Part A in place today.** Add §10 (`duckdb`
   with `bundled` feature), §11 (`time` with the listed
   features), §12 (`ciborium` for audit-ledger canonical
   encoding), §13 (`ulid`, explicit reference). Add a paragraph
   in §Consequences noting the amendment and dating it
   2026-05-19 (same day, first review). Status remains Accepted
   after amendment because the review is the gate that advances
   it.
2. **Append a §Amendment note** to ADR-0021 calling out F1–F4 and
   their resolution. This makes the in-place amendment
   self-documenting for a future reader.
3. **Append to ADR-0021 §Items deferred to build phase:** F5
   (attestation signing key type) and F6 (keychain Rust binding
   crate), each with its named trigger.
4. **Append the four new crates** to
   `docs/research/stack-baseline.md` so the research record stays
   in lockstep with the ADR.
5. **Add a row** to `docs/threat-model.md` §Review log pointing
   at this review file.

## Sign-off criteria for session 5

Session 5 may begin writing code when:

- ADR-0021 amendments in step 1 above are in place.
- No new finding has surfaced between this review and the start
  of session 5.

If Ervin disagrees with any finding's blocker status, he can
downgrade it explicitly (in conversation; the downgrade is
recorded in the session-5 handoff). Soft-asserting a finding as
resolved without naming it is forbidden (CLAUDE.md rule 12).
