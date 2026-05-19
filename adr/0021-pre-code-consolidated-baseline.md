# ADR-0021 — Pre-code consolidated baseline (stack + wire protocol)

- **Status:** Accepted
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Class:** Cornerstone (Spine)
- **Source material:** `docs/research/stack-baseline.md`
- **Related:**
  - ADR-0001 (Backend language: Rust) — names this ADR as the place
    the stack baseline is decided.
  - ADR-0004 (Frontend: Tauri + Svelte) — names this ADR as the
    place the wire protocol is decided.
  - ADR-0006 (Module boundaries) — already commits the in-process
    event bus to `tokio::sync::broadcast`, which this ADR confirms.
  - ADR-0007 (Security baseline) — license allow-list, supply-chain
    rules, loopback-TLS posture, structured logging, secrets and
    `zeroize`, Tauri allow-list.
  - ADR-0009 (NAV invoice issuing) — calls into the crates this ADR
    pins for HTTP, XML, and crypto.
  - ADR-0016 (Cloud sync) — wire-protocol choice is the one the
    cloud topology inherits.
  - ADR-0020 (NAV transport and credential correction) — pins
    `rustls` root-store usage for NAV traffic.

## Context

ADR-0001 names a Rust backend but defers the async runtime, error
crate, logging crate, CLI crate, HTTP client, XML/SOAP, and
cryptography choices to a later "stack baseline" ADR before
commit #1. ADR-0004 defers the UI ↔ backend wire-protocol choice
between gRPC, HTTPS+JSON, and a Tauri-IPC shortcut to the same
gate. These are the two remaining cornerstone-class decisions that
must close before any line of business code lands.

End-of-session-3 framing (Ervin, 2026-05-19): session 3 was the
last pure-ADR session; session 4 files **one** consolidated
"pre-code" ADR that resolves the minimum subset and explicitly
defers the rest to build-phase, just-in-time ADRs. Session 5
starts code. The five items listed in §"Items deferred to build
phase" below are out of scope by that framing and are not
re-litigated here.

Two decisions land in this document; a third sub-decision (Billingo
read-path posture) lands in a separate, module-level ADR-0010 in
the same session per the architectural-hygiene principle from
ADR-0006 (module-level concerns live in module-level ADRs).

## Decision

### Part A — Stack baseline

The following crate choices are pinned. Versions cited reflect the
current minor as of 2026-05-19; `Cargo.lock` is the load-bearing
artifact per ADR-0007 §Supply chain, and the exact lock state at
commit #1 is the authoritative pin.

1. **Async runtime — `tokio`** (features: `rt-multi-thread`,
   `macros`, `net`, `fs`, `time`, `sync`, `signal`). Pinned at
   the current minor line. This confirms ADR-0006 §Event-bus phase
   1's reference to `tokio::sync::broadcast`. License: MIT.

2. **Error crates — `thiserror` for library crates, `anyhow` for
   the binary / application boundary.** Every `modules/<name>/`
   crate exposes a typed `Error` enum via its `api.rs` using
   `thiserror`. The binary crate uses `anyhow::Result` at the
   outermost call sites. A library crate that imports `anyhow` in
   non-test code is a conformance failure; ADR-0006 §Conformance
   may grow a check for this if needed in practice. Both pinned at
   the current 1.x minor. License: MIT/Apache-2.0 dual.

3. **Logging / tracing — `tracing` + `tracing-subscriber`**
   (features on `tracing-subscriber`: `env-filter`, `json`, `fmt`,
   `registry`). JSON formatter in production; human-readable
   formatter in development. ADR-0007 §Logging already names this
   stack; this ADR pins the version line. `tracing` 0.1.x;
   `tracing-subscriber` 0.3.20. License: MIT.

4. **CLI crate — `clap` with the `derive` feature.** Pinned at
   the 4.6.x minor. License: MIT/Apache-2.0 dual.

5. **HTTP client — `reqwest` with `rustls-tls` feature.** Used by
   the NAV adapter (ADR-0009 / ADR-0020) and the Billingo
   migration read path (ADR-0010). The NAV client is constructed
   with `add_root_certificate()` against the embedded NAV issuing
   root per ADR-0020 §1; the Billingo client uses the platform
   roots via `rustls-native-certs` per ADR-0010. Features:
   `rustls-tls`, `gzip`, `stream`, `json`. Pinned at the current
   0.13.x minor. License: MIT/Apache-2.0 dual.

6. **HTTP server (for the UI ↔ backend wire protocol) — `axum` +
   `axum-server` with `tls_rustls::RustlsConfig`.** The loopback
   listener is TLS-terminated by `rustls`; the self-signed cert is
   generated at first launch per installation (see §Sub-decisions
   below for the cert-generation crate). Fingerprint is verified
   by the Tauri shell on every connect, per ADR-0007 §Transport.
   License: MIT.

7. **JSON serialization — `serde` + `serde_json`** (both at the
   current 1.x minor; `serde` with the `derive` feature). The
   same derives are reused on `quick-xml`'s `serialize` feature for
   NAV XML payloads. License: MIT/Apache-2.0 dual.

8. **XML / SOAP — `quick-xml` with the `serialize` feature.** The
   NAV SOAP envelope is hand-rolled as a thin wrapper around the
   payload structs; codegen-from-XSD is rejected for this scope.
   Pinned at the current 0.36.x / 0.37.x minor. License: MIT.

9. **Cryptography — RustCrypto `sha2`, `sha3`, `aes`.** SHA-512
   for the NAV `passwordHash`; SHA3-512 for the NAV
   `requestSignature`; AES-128/ECB for the NAV `exchangeToken`
   decryption — all per ADR-0020 §2. ABERP uses AES-128/ECB
   **only** for the protocol-imposed exchange-token decryption;
   the pattern must not generalize to anything ABERP controls, and
   a call-site comment is required. `zeroize` (ADR-0007 §Secrets)
   pinned at the current 1.x minor alongside. All four pinned at
   their current minors. Licenses: MIT/Apache-2.0 dual.

10. **Embedded database driver — `duckdb` crate with the
    `bundled` feature.** Pinned at the current minor. The
    `bundled` feature compiles the DuckDB C++ library into the
    ABERP binary, preserving the single-static-binary posture
    that ADR-0001 §Consequences names and keeping the
    reproducible-build story under ADR-0007 §Supply-chain
    self-contained. The DuckDB binding sits in the adapter layer
    per ADR-0006; the `unsafe` it contains is permitted per
    ADR-0001 ("`unsafe` is permitted only in well-isolated
    adapters"). License: MIT. This pin closes finding F1 of the
    pre-code adversarial review
    (`docs/reviews/2026-05-19-pre-code-spine-review.md`).

11. **Date and time — `time` crate** with the `formatting`,
    `parsing`, `macros`, `serde`, `serde-well-known` features.
    Pinned at the current 0.3.x minor. ADR-0008 §"Entry shape"
    requires RFC3339 timestamps; ADR-0020 §2 and the NAV
    research file require `YYYYMMDDhhmmss` UTC for the NAV
    `requestTimestamp`. The `time` crate supports both formats
    natively and has a narrower, more correctness-focused API
    surface than `chrono`. License: MIT/Apache-2.0 dual. This
    pin closes finding F2 of the pre-code adversarial review.

12. **Canonical byte encoding for the audit-ledger hash chain —
    `ciborium` with CBOR canonical encoding (RFC 8949 §4.2.1).**
    Pinned at the current 0.2.x minor. ADR-0008 §"Hash chain"
    states *"`entry_hash[N] = SHA-256(canonical(entry[N] with
    prev_hash = entry_hash[N-1]))`"* but did not specify the
    `canonical` byte mapping. This ADR concretizes it: the
    canonical encoding is CBOR per RFC 8949 §4.2.1, produced
    by `ciborium`'s deterministic-encoding mode, and the
    encoder function lives in **one place inside the
    audit-ledger crate** (not at call sites). A conformance
    check ensures no other module computes `entry_hash`
    directly. License: MIT/Apache-2.0 dual. This pin closes
    finding F3 of the pre-code adversarial review.

13. **ULID generation — `ulid` crate.** Pinned at the current
    1.x minor. Already named by ADR-0005 ("The `ulid` crate
    handles this; we wrap it in an injectable `IdProvider`
    port"); enumerated here for symmetry with the other
    explicit pins and to ensure `Cargo.toml` reflects an
    intentional version line. License: MIT. This pin closes
    finding F4 of the pre-code adversarial review.

#### Sub-decisions

- **TLS backend = `rustls` everywhere.** No `native-tls`,
  `openssl`, `Schannel`, or `Secure Transport` surface. Rationale:
  ADR-0020 §1's pinned-root posture is a static `RootCertStore`
  construction, ADR-0007 §Supply-chain's reproducible-build
  requirement prefers pure Rust, and the runtime side already
  picks `rustls` via `reqwest 0.13`'s default and `axum-server`'s
  `tls_rustls` module.
- **Self-signed loopback cert generation crate — `rcgen`** (the
  de-facto pure-Rust pick, MIT/Apache-2.0 dual). Used at first
  launch to generate the cert that ADR-0007 §Transport calls for.
  This is the smallest crate in the baseline; pinning it here
  avoids a surprise gate when the wire-protocol code lands.
- **Rust edition = 2021, MSRV ≥ 1.85** (the RustCrypto current
  minors require this). `rust-toolchain.toml` (named by ADR-0001)
  pins the exact stable version used by ABERP CI; any bump beyond
  the MSRV floor is operational, not architectural.

### Part B — Wire protocol (UI ↔ backend)

**HTTPS + JSON over loopback** is the wire protocol. Concretely:

- The backend exposes an HTTPS listener on `127.0.0.1` (or the OS
  loopback equivalent) using `axum-server` with
  `tls_rustls::RustlsConfig`. The cert is self-signed at first
  launch, stored next to the keychain-bound material; the
  fingerprint persists so the Tauri shell can verify on every
  connect.
- The Svelte UI inside Tauri makes JSON requests against that
  listener using the browser's `fetch` API. The Tauri shell
  intercepts the TLS handshake and validates the server cert
  against the persisted fingerprint before allowing the request
  surface to come up.
- Requests carry an `Authorization: Bearer <session-token>`
  header per ADR-0007 §Authentication. The token is obtained at
  Tauri-shell launch from the backend, presenting OS-keychain-
  bound material.
- Each backend route maps to exactly one capability per ADR-0007
  §Authorization. The capability is resolved from the session
  token before route handlers run. A route without a declared
  capability is a CI conformance failure (extending ADR-0006
  §Conformance).
- JSON payloads use the same `serde` derives as the backend's
  internal types; there is no separate DTO layer until a route's
  external shape needs to diverge from its internal shape.
- The cloud topology (ADR-0016) reuses the same protocol shape on
  a non-loopback listener. There is no "local fast path" that
  bypasses the wire.

#### Why HTTPS + JSON over loopback rather than the other two

Rationale stated against named consequences; alternatives are not
averaged with the pick.

- **Tauri commands (`#[tauri::command]`) is rejected** despite
  being lowest-ceremony on the desktop side. The cost is structural:
  ADR-0004 explicitly requires the **same wire protocol on local
  and cloud** so that ADR-0016's cloud surface is reachable to
  design today, not paid for later. Tauri commands do not exist on
  the cloud topology. Picking them would create exactly the kind
  of two-path divergence (local fast path; cloud retrofit) that
  ADR-0004 §Adversarial-review names as the failure mode. The
  serialization savings on the desktop side are measured in
  microseconds for typed structs (ADR-0004 §Consequences); the
  divergence cost is measured in years of debugging the cloud
  path.

- **gRPC over loopback is rejected** despite stronger typing on
  the wire. Three costs: (a) HTTP/2 plus a separate code-gen
  toolchain (`tonic` + `prost`) adds a build-surface that is
  oversized for ABERP's near-term scope; (b) browser-side gRPC
  (gRPC-Web or Connect-RPC) on Svelte requires a translation
  layer and an additional bundle on the UI side that buys little
  the Tauri shell does not already give; (c) `tonic`'s wire
  evolution and the gRPC-Web compatibility matrix are a recurring
  re-litigation surface that does not pay for itself on a
  single-process local app. The cloud topology can adopt gRPC if
  it later wants to; the **local** wire is the constraining
  decision and HTTPS+JSON is the simpler floor for it.

- **HTTPS + JSON over loopback wins** because (a) it satisfies
  ADR-0007 §Transport "All UI ↔ backend traffic over TLS, even
  loopback" with the same primitives the cloud surface uses; (b)
  it makes ADR-0007 §Authorization's capability mapping a
  route-table check, which is more legible than a
  `#[tauri::command]` enum match; (c) it lets the Svelte side use
  plain `fetch` with no Tauri-IPC bridge in the request path,
  keeping the Svelte side closer to the cloud-UI codebase
  (ADR-0004) that consumes the same protocol; (d) the
  fingerprint-pinning posture ADR-0007 §Transport requires is a
  single check at TLS handshake time, not a permission-list
  enumeration.

#### Surfaced cost (not papered over)

HTTPS+JSON over loopback adds capability-mapping ceremony that
Tauri commands would not require, and the self-signed-cert
fingerprint persistence adds a small piece of platform-specific
storage. Both are accepted. The first is the same ceremony the
cloud topology needs anyway; the second is one file alongside the
keychain artifacts ADR-0007 already enumerates.

## Items deferred to build phase

The following decisions are required at some point but are **not**
required before commit #1. They are deferred to just-in-time ADRs
that ship with the code PR that needs them. **Soft assertion of
any of these in advance of the triggering PR is forbidden**
(CLAUDE.md rule 12).

| Item | Triggering condition for filing the ADR |
|---|---|
| Backup encryption + key escrow | First PR that writes the encrypted backup path. Was deferred by ADR-0007. |
| GDPR data retention + erasure workflow | First PR that wires a `forget-tenant` or `erase-customer` workflow. Was deferred by ADR-0002. |
| LLM use policy (which paths use models, which providers) | First PR that adds an LLM-using code path (per ADR-0007 §Adversarial-review, an LLM path declares its inputs untrusted and outputs as suggestions). |
| Font family selection with Hungarian diacritic coverage | First PR that produces a printed invoice — print path or PDF path. Was deferred by ADR-0017. |
| Print rendering path (browser print vs Rust-side PDF) | Same trigger as fonts; either fills in or is filed alongside. Was deferred by ADR-0017. |

Two further items surfaced in `docs/research/stack-baseline.md`
and are also deferred:

- **XSD runtime validation crate** (libxml FFI vs hand-rolled
  invariant check vs pure-Rust validator-when-mature). Trigger:
  first PR implementing schema-drift detection per ADR-0009 §1.
  Commit #1 does not need runtime XSD validation.
- **`cargo-deny` lint for `anyhow`-in-library / `thiserror`-in-
  binary discipline.** Trigger: first time a library crate
  imports `anyhow` in non-test code. Likely added as an
  ADR-0006 conformance check rather than its own ADR.

Two further items surfaced in the first pre-code adversarial
review (`docs/reviews/2026-05-19-pre-code-spine-review.md`) and
are deferred to build phase:

- **Attestation signing-key type for ADR-0008's external
  attestation checkpoints.** Trigger: first PR that exercises
  attestation cadence — a long-running process, an integration
  test that crosses the cadence threshold (default 1000 entries
  / 60 minutes), or a cloud-deployment PR that publishes
  attestation to an external trust anchor per ADR-0016.
  Recommendation when filed: Ed25519 (smaller, faster, no
  parameter choices). Commit #1's XML-on-disk binary runs once
  and exits well below cadence, so this is not exercised at
  commit #1.
- **OS-keychain Rust binding crate** for ADR-0007 §Secrets.
  Trigger: first PR that loads keychain-bound material in
  production code (typically the first PR that performs a real
  NAV submission). Likely pick: `keyring` (MIT/Apache-2.0
  dual). Commit #1 does not perform a real submission and does
  not need keychain access; test fixtures or environment-
  variable overrides cover the credential path for the
  XML-on-disk binary.

## Consequences

**What gets easier**

- Commit #1 has no surprise gates. Every "required before code"
  item is either pinned in this ADR or named-and-deferred above.
- The cloud topology (ADR-0016) inherits the wire protocol from
  this ADR rather than re-inventing it. Cloud-readiness work is
  paid for once, at the local-wire decision.
- The Rust workspace can be initialized with a known, license-
  clean dependency set on day one; `cargo-deny` and `cargo-audit`
  (ADR-0007) can be wired before the first business code is
  written.
- Module conformance (ADR-0006) gains a clean check: every module
  crate uses `thiserror`, the binary uses `anyhow`, and divergence
  is mechanically detectable.

**What gets harder**

- Picking `tokio` and the `tokio`-shaped surrounding stack
  (`reqwest`, `axum`, `tracing`) locks ABERP to that ecosystem's
  release cadence and breaking-change history. The lock is
  intentional — the alternative is an inconsistent runtime story
  across the HTTP client, HTTP server, and event bus — but it
  means future runtime migrations are not surgical.
- HTTPS+JSON over loopback adds the capability-mapping ceremony
  noted in Part B §"Surfaced cost." That cost is accepted as the
  price of shape-parity with cloud.
- The hand-rolled SOAP envelope (Part A §8) is correctness-by-
  inspection rather than codegen-by-XSD. The trade is small for
  a single counterparty (NAV), but it is a deliberate choice; a
  follow-up ADR can revisit if the maintenance pain accumulates.

**What we lock ourselves into**

- The license allow-list as enforced by `cargo-deny` (ADR-0007).
  Crates that move to a forbidden license after we adopt them
  become a release blocker until alternative is found or
  exception is documented. This is the cost of supply-chain
  hygiene we explicitly chose.
- The `rustls`-only TLS posture. If a future counterparty's TLS
  stack is incompatible with `rustls` (e.g., requires a
  proprietary cipher suite), that integration becomes a
  cross-cutting ADR rather than a per-adapter switch.
- The HTTPS+JSON wire-protocol shape. Switching to gRPC or
  Tauri-IPC later requires re-litigating ADR-0004 and ADR-0016
  jointly; this ADR will be the supersede target.

## Adversarial review

Cornerstone-class. Bar is ≥5 concerns answered or accepted.

- *"You picked seven-plus crates in one ADR. Combining decisions
  that should be individually reviewable is exactly the averaging
  failure mode the project disclaims."* — Acknowledged and
  refused as a framing. The combined ADR is the **session-3 close
  constraint** (Ervin: "session 3 was the last pure-ADR session").
  The individual decisions are still discrete inside Part A; each
  one has its own line item, its own rationale, and its own
  rejected alternatives. The combined document is a packaging
  choice; it does not collapse the per-crate reasoning. If a
  future maintainer wants to revisit (say) the XML crate alone,
  the line-item structure here supports a surgical supersede of
  Part A §8 without touching the rest. Accepted.

- *"Pinning at the current minor line lets patch-level CVEs ship
  unnoticed; pinning at the patch level forces manual bumps that
  will rot. You did not commit to either."* — Correct, and the
  resolution is that `Cargo.lock` (ADR-0007 §Supply chain) is the
  load-bearing artifact. The version lines named in this ADR are
  the **floor**; the `Cargo.lock` at commit #1 is the **exact
  pin**. `cargo-audit` runs against the lockfile in CI per
  ADR-0007, so CVE drift is caught at build time. The minor
  pinning posture in this ADR is what allows `Cargo.lock`-
  managed patch bumps to land without re-litigating the ADR.
  Accepted.

- *"AES-128/ECB is a footgun. Pinning a crate that exposes it in
  the cornerstone ADR institutionalizes a primitive that, three
  years from now, someone will reuse for the wrong thing."* —
  Real risk; surfaced and mitigated, not papered over. The use
  is **protocol-imposed by NAV** per ADR-0020 §2 — ABERP does not
  choose ECB, NAV does, for the `exchangeToken` envelope. Part A
  §9 requires a call-site comment that names this. The mitigation
  is conformance-checkable: the `aes` crate's `Aes128` cipher
  should appear in exactly one adapter module (the NAV adapter);
  CI can grep for additional call sites and fail. That check is
  filed as a tracking item against the conformance suite. The
  alternative — refusing to depend on the `aes` crate and
  re-implementing ECB inline — is materially worse for supply
  chain. Accepted.

- *"`rustls` everywhere means that a future counterparty whose
  TLS stack is `rustls`-incompatible becomes an architectural
  problem rather than a per-adapter switch. You have given up
  flexibility you may need."* — Real cost. The trade is in
  ADR-0020 §1 already: the NAV issuing-root pin is a static
  `RootCertStore` construction that is materially cleaner under
  `rustls` than under platform TLS. The ADR-0007 reproducible-
  build posture also prefers pure Rust over an OpenSSL FFI
  surface. If a future counterparty needs platform TLS, that
  adapter files its own ADR superseding Part A §5–§6 for that
  specific code path, not for the project. The cross-cutting
  posture remains `rustls`. Accepted with the escape hatch named.

- *"HTTPS+JSON over loopback adds capability-mapping ceremony that
  Tauri commands would not require. Part B notes the cost but
  does not quantify it; a contractor reading this ADR walks away
  expecting 'a small piece of work' that is in fact several days
  of plumbing per route."* — Quantification: each new backend
  route declares its capability via a derive macro or a route
  attribute; the dispatcher resolves the session-token-derived
  capability set against the route's required capability before
  the handler runs. The ceremony is one line per route plus a
  shared dispatcher. The work to **build** the shared dispatcher
  is one-time and lives in a backend infrastructure crate. The
  per-route cost is genuinely small; the dispatcher is the
  load-bearing piece, and it is a known design pattern. The
  alternative — `#[tauri::command]` with a permission-list
  enumeration — does not avoid this work; it relocates it into
  the Tauri allow-list (ADR-0007 §Tauri allow-list), which is
  itself a per-command declaration. The ceremony does not
  evaporate. Accepted.

- *"The wire-protocol decision pre-commits ADR-0016 (cloud sync)
  to a protocol shape before its own decisions have landed.
  ADR-0016 is still a stub. You are deciding for a future ADR."* —
  Correct in form, intentional in substance. ADR-0004 §Decision
  already requires "the same backend command/query API as the
  local UI" for the cloud topology. ADR-0021 Part B is the
  protocol-shape implementation of that ADR-0004 commitment. If
  ADR-0016 later determines that the cloud topology needs a
  different shape (e.g., for multi-user collaboration semantics
  that don't fit HTTPS+JSON cleanly), ADR-0016 supersedes Part B
  with an explicit rationale. Until then, the local-only protocol
  is the cloud-future floor — which is exactly the ADR-0004
  posture. Accepted.

- *"You deferred the LLM use policy to build phase. ADR-0007
  §Adversarial-review already implicitly mandates a 'judgment
  calls only' posture for LLM paths. By deferring, you risk a
  first PR that adds an LLM path quietly and uses the deferral
  as cover."* — The deferral here is the **policy ADR**, not the
  posture. ADR-0007 §Adversarial-review (and CLAUDE.md rule 5)
  already binds the posture: LLM only for classification,
  extraction, drafting, summarization; never for routing,
  retries, status-code handling, deterministic transforms. The
  policy ADR is what spells out provider supply chain, model
  identity, and audit-evidence handling. A first PR that adds an
  LLM path triggers that ADR before merge. This is the
  loud-on-trigger pattern; soft-asserting a policy now would
  be the failure mode CLAUDE.md rule 12 names. Accepted.

## Alternatives considered

- **One ADR per crate.** Rejected. Session-3 close constraint
  (Ervin: "this is the last session which writes ADR, there
  should be some coding soon"). The per-crate review is preserved
  by the line-item structure in Part A; the packaging is the
  concession to the trajectory constraint.

- **Defer the wire-protocol decision to ADR-0016 (cloud sync).**
  Rejected. ADR-0004 explicitly names this gate as a "before
  commit #1" item, and the local-UI wire is the immediate need.
  Deferring would push ADR-0016 — currently a stub — onto the
  pre-code critical path.

- **Tauri commands (`#[tauri::command]`).** Rejected — see Part B
  §"Why HTTPS + JSON…". The fast-local / slow-cloud divergence is
  the named failure mode in ADR-0004 §Adversarial review.

- **gRPC over loopback.** Rejected — see Part B §"Why HTTPS +
  JSON…". HTTP/2 + codegen + gRPC-Web translation is oversized
  for the local-app scope; cloud is free to adopt gRPC later as a
  superseding decision if multi-user collaboration semantics
  demand it.

- **Native-TLS / OpenSSL surface for `reqwest` and `axum-server`.**
  Rejected. Larger supply chain, harder reproducible builds,
  awkward fit with ADR-0020 §1's pinned-root posture.

- **`xmltree` or `roxmltree` for XML.** Rejected — slower,
  smaller serde integration, DOM-only patterns; `quick-xml` is
  the consensus pick (see `docs/research/stack-baseline.md`).

- **`ring` for crypto.** Rejected — does not expose AES/ECB,
  and the cc-driven build complicates ADR-0007's reproducible-
  build posture.

- **`async-std` runtime.** Rejected — would re-litigate ADR-0006
  §Event bus and force a parallel ecosystem for HTTP and tracing.

## Open questions

These are tracked against the next adversarial-review cadence; none
of them block commit #1.

- **`cargo-deny` configuration for the thiserror/anyhow split.**
  Whether a static lint can enforce "library crates do not import
  anyhow" without false positives in test files. Resolution path:
  experiment in the first CI pass; if false-positive prone, the
  conformance check moves into the ADR-0006 module-conformance
  suite as a custom rule.

- **`rcgen` posture under FIPS-style review.** ABERP's
  reproducible-build posture is satisfied by `rcgen`, but if a
  future tenant requires FIPS-validated cert generation (cloud-
  topology concern more than local), that constraint files its
  own ADR rather than touching this one.

- **`tracing-subscriber` JSON format stability.** The 0.x release
  series means breaking changes are still in scope upstream.
  Mitigation: pin at the current 0.3.x minor and gate version
  bumps through the same adversarial-review cadence as other
  cornerstone-touching changes.

- **XSD runtime validator decision** (tracked in
  `docs/research/stack-baseline.md`). Filed as deferred to build
  phase; trigger named there.

- **First full-spine adversarial review.** Per the session-4 plan,
  runs at the close of session 4 before code starts. This ADR
  enters that review as Accepted; if the review surfaces a
  blocking concern, it supersedes the affected clause via a new
  ADR before session 5 begins.

## Amendment — 2026-05-19, post first adversarial review

The first full-spine adversarial review
(`docs/reviews/2026-05-19-pre-code-spine-review.md`) was
conducted at the close of session 4. It surfaced four findings
against this ADR; all four are resolved in this amendment, in
place, on the same day the ADR was filed. Status remains
Accepted because this review is the gate ADR-status-lifecycle
calls for to advance an ADR from Proposed (drafted) to Accepted
(reviewed).

Resolved findings:

- **F1 — DuckDB driver crate.** Folded into Part A as item §10.
- **F2 — Date / time crate.** Folded into Part A as item §11.
- **F3 — Canonical-byte encoding for the audit-ledger hash
  chain.** Folded into Part A as item §12; concretizes
  ADR-0008's "canonical-serialized" by reference to RFC 8949
  §4.2.1.
- **F4 — `ulid` crate enumeration.** Folded into Part A as
  item §13.

Tracked findings (deferred, not blocking commit #1):

- **F5 — attestation signing-key type for ADR-0008.** Added to
  §"Items deferred to build phase" with named trigger.
- **F6 — OS-keychain Rust binding crate.** Added to §"Items
  deferred to build phase" with named trigger.
- **F7 — ADR-0020 [OPEN] on NAV response-body integrity.**
  Unchanged; carried forward against external check.

A future review walking this amendment top-to-bottom will read
the amendment paragraph here, then Part A's items §10–§13, then
the §"Items deferred to build phase" entries, and have the full
amendment trail in one document.

## Amendment — 2026-05-19, post-PR-3 verification

§Sub-decisions wording *"`rust-toolchain.toml` pins the exact
stable version used by ABERP CI"* is **amended** to: *"`rust-
toolchain.toml` pins the stable **channel**; the architectural
MSRV floor lives in each `Cargo.toml`'s `rust-version` field;
reproducibility responsibility shifts entirely to `Cargo.lock`
per ADR-0007 §Supply chain."*

Rationale: an exact-version pin (e.g. `1.88.0`) is a ceiling,
not a floor. Every contributor cloning the repo years later
would have to install that exact rustc to build, and CI would
lock onto a rustc that ages out of upstream security updates.
The maintainable-through-years pattern is to track the stable
channel and let `Cargo.lock` plus per-crate `rust-version` carry
the load-bearing pins.

Surfaced during PR-3 verification when the original `channel =
"1.88.0"` pin was caught as a ceiling rather than a floor; the
working-agreement preference is "set a floor to rust not a
ceiling, I want this repo to be maintainable through years
maybe" (Ervin, 2026-05-19).

Operational state at time of amendment (commit `7fd09f9` and
its predecessor `456a648`):

- `rust-toolchain.toml` `channel = "stable"`
- workspace `rust-version = "1.88"` (bumped from the original
  1.85 floor because the locked dep tree, principally
  `time` 0.3.47 and `icu_*` 2.2, requires it; this bump is
  operational per the §Sub-decisions "any bump beyond the MSRV
  floor is operational, not architectural" clause)
- CI uses `dtolnay/rust-toolchain@stable`

If a future release-engineering effort requires strict
bit-reproducible builds for a specific shipped binary, that
branch can temporarily re-introduce an exact-version pin
without disturbing the main-branch maintainability posture.
ADR-0007 §Supply chain's reproducible-build requirement is
satisfied by `Cargo.lock` plus the binary-hash recording in
ADR-0008 §Entry shape — which is independent of compiler
version drift across patches.

The §Items deferred to build phase entries are unchanged. The
ADR's architectural MSRV floor (`MSRV ≥ 1.85`) is unchanged;
the operational pin is what moved.
