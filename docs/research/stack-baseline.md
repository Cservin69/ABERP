# Stack baseline — research findings

> **Research, not decision.** ADR-0021 is the decision; this file is the
> source material it cites. Crate picks below are recommendations to
> ADR-0021, not commitments in themselves. Items marked **[OPEN]** are
> deferred to a build-phase ADR with a named trigger.

- **Compiled:** 2026-05-19
- **Scope:** the smallest set of Rust crate choices needed to land
  commit #1 (async runtime, error crate, logging crate, CLI crate, HTTP
  client + TLS backend, XML / SOAP handling, cryptography, HTTP server
  for the UI ↔ backend wire protocol).
- **Author note:** every version line below was checked against
  `crates.io` and the upstream repo on 2026-05-19; license posture is
  checked against the ADR-0007 §Supply-chain allow-list (MIT,
  Apache-2.0, BSD-3-Clause, MPL-2.0; anything else needs a documented
  exception).

## Decided project framing this research must respect

- **Rust stable channel, edition 2021** (ADR-0001).
- **License allow-list:** MIT, Apache-2.0, BSD-3-Clause, MPL-2.0
  (ADR-0007 §Supply chain). Dual MIT/Apache-2.0 is the dominant
  pattern in the Rust ecosystem and clears the gate.
- **Pinned dependency versions, `Cargo.lock` checked in, `--locked`
  builds** (ADR-0007 §Supply chain). Pre-1.0 crates pin at the patch
  level; post-1.0 crates pin at the minor with caret semantics, unless
  there is reason to pin tighter.
- **In-process event bus is `tokio::sync::broadcast`** (ADR-0006
  §"Event bus" phase 1). This already commits the async runtime
  choice; the stack-baseline ADR confirms it rather than re-opens it.
- **`tracing` is already named** (ADR-0007 §Logging: "Structured
  logging (`tracing` crate, JSON in production)"). Confirm + pin.
- **Cryptography needed at commit #1 (eventually):** SHA-512,
  SHA3-512, AES-128/ECB for the NAV adapter (ADR-0020 §2). Pure-Rust
  picks from the RustCrypto collection clear the supply-chain bar.

---

## Async runtime — `tokio`

- **Pick:** `tokio` with the `rt-multi-thread`, `macros`, `net`,
  `fs`, `time`, `sync`, `signal` features. Pin at the current minor
  line.
- **Version line (2026-05-19):** 1.52.x. Recent point releases:
  1.50.0 on 2026-03-03; 1.52.x as the current series. Active,
  Tokio-team maintained.
  ([crates.io/tokio](https://crates.io/crates/tokio);
  [docs.rs/tokio](https://docs.rs/crate/tokio/latest/source/Cargo.toml.orig);
  [github.com/tokio-rs/tokio](https://github.com/tokio-rs/tokio))
- **License:** MIT. Clears the allow-list.
- **Rationale.** ADR-0006 §Event-bus phase 1 already names
  `tokio::sync::broadcast`. The NAV adapter (ADR-0009) needs async
  HTTP, and the audit ledger (ADR-0008) benefits from async file IO
  for hash-chain append batches. `tokio` is also the de-facto runtime
  the rest of the recommended stack (`reqwest`, `axum`, `hyper`,
  `tracing`) is built against. Picking a different runtime forces a
  rebuild of the integration surface and contradicts an Accepted ADR.
- **Alternatives rejected:**
  - `async-std` — divergent from `axum` / `hyper` / `reqwest`'s
    integration story; far smaller maintainer pool today; would
    re-litigate ADR-0006.
  - Sync-only with a thread pool — the NAV submission path is
    request-response over the network with retries and a poll loop
    against `queryTransactionStatus`; doing this synchronously costs
    a thread per outstanding request. Not a fit.

---

## Error crates — `thiserror` for libraries, `anyhow` for binaries

- **Pick:** `thiserror` inside every `modules/<name>/` library crate
  to define a typed `Error` enum exposed through `api.rs`;
  `anyhow::Result` in the binary crate (`bin/aberp` or equivalent)
  and in integration glue where the caller is the application
  boundary and only needs a printable, chainable error.
- **Version lines (2026-05-19):** `thiserror` 1.x current series;
  `anyhow` 1.x current series. Both stable, both pre-2.0, both
  long-maintained by `dtolnay`. Pin both at the current minor.
- **License:** dual MIT / Apache-2.0 for both. Clears the allow-list.
- **Rationale.** ADR-0006 mandates `api.rs` as a wire-shape contract.
  Module callers must be able to match on specific error variants —
  `IssueInvoiceError::SequenceExhausted` is observable, not a string.
  `thiserror` produces those typed enums with minimum ceremony and
  preserves the source chain via `#[source]` / `#[from]`. At the
  application boundary the cross-module error set is large and
  rarely matched on — `anyhow::Error` carries the chain for
  logging and `?` flows. Community consensus on the split is
  consistent across 2026 references.
  ([carolinemorton.co.uk](https://www.carolinemorton.co.uk/blog/rust-error-handling-anyhow-thiserror/);
  [lpalmieri.com](https://www.lpalmieri.com/posts/error-handling-rust/);
  [oneuptime.com](https://oneuptime.com/blog/post/2026-01-25-error-types-thiserror-anyhow-rust/view))
- **Discipline.** A library crate that pulls in `anyhow` is a
  conformance failure — ADR-0006 §Conformance can grow a check for
  this if it surfaces in practice. Marked as a tracking item below,
  not a near-term action.
- **Alternatives rejected:**
  - One crate everywhere (`thiserror` only, or `anyhow` only) —
    either forces every binary call site to define a domain enum
    it does not care about, or collapses module-level type
    information at the API surface ADR-0006 protects.
  - A hand-rolled error trait — adds maintenance for zero gain over
    `thiserror`'s derive.

---

## Logging / tracing — `tracing` + `tracing-subscriber`

- **Pick:** `tracing` for instrumentation, `tracing-subscriber` for
  collection, with the `env-filter`, `json`, `fmt`, `registry`
  features. JSON formatter in production, human-readable formatter
  in dev.
- **Version lines (2026-05-19):** `tracing` 0.1.x current;
  `tracing-subscriber` 0.3.20 (current per docs.rs, 2026-03-13).
  Pin both at the current minor; tracking the 0.x release series is
  the upstream-stable signal here, not the 1.0 absence.
  ([crates.io/tracing](https://crates.io/crates/tracing);
  [crates.io/tracing-subscriber](https://crates.io/crates/tracing-subscriber);
  [docs.rs/tracing-subscriber](https://docs.rs/crate/tracing-subscriber/latest);
  [github.com/tokio-rs/tracing](https://github.com/tokio-rs/tracing))
- **License:** MIT. Clears the allow-list.
- **Rationale.** Already named in ADR-0007 §Logging. JSON output is
  the production format because the audit-evidence pipeline
  (ADR-0008) cares about structured fields, and an external log
  analyser can ingest the JSON without bespoke parsing. `env-filter`
  is the dev affordance for narrowing during debug sessions; it
  must not be the production filter mechanism — production filters
  live in config that the build provenance can attest to.
- **Alternatives rejected:**
  - `log` + `env_logger` — works, but `tracing`'s span model is the
    reason the NAV adapter and audit ledger can carry correlation
    IDs through async call graphs without manual plumbing.
  - `slog` — older, smaller community, the `tracing` ecosystem
    overtook it years ago.

---

## CLI crate — `clap`

- **Pick:** `clap` with the `derive` feature for the binary's
  command-line surface.
- **Version line (2026-05-19):** 4.6.0 (released 2026-03-12). Pin at
  4.6.x.
  ([crates.io/clap](https://crates.io/crates/clap);
  [docs.rs/clap](https://docs.rs/crate/clap/latest))
- **License:** dual MIT / Apache-2.0. Clears the allow-list.
- **Rationale.** De-facto Rust standard. The first binary (per
  project memory: "smallest thing that exercises ADR-0008 +
  ADR-0009 + ADR-0019 — likely a binary that generates a
  NAV-compatible invoice XML on disk without submitting") is a CLI
  tool. `clap`'s derive macros keep the surface small; the
  `derive` form is preferred over the builder form because it makes
  the CLI surface visible in one Rust struct.
- **Alternatives rejected:**
  - `argh` — smaller and lighter, but loses subcommand ergonomics
    that ABERP will want as commands accrete.
  - `pico-args` — too minimal for the medium-term surface.

---

## HTTP client — `reqwest` with `rustls-tls` feature

- **Pick:** `reqwest` for the NAV adapter and the Billingo migration
  read path, with the `rustls-tls`, `gzip`, `stream`, `json`
  features. Build the client with `add_root_certificate()` to bind
  the NAV issuing root per ADR-0020 §1; build the Billingo client
  using the OS trust store per ADR-0010.
- **Version line (2026-05-19):** 0.13.x current. v0.13 switched to
  `rustls` as the default backend.
  ([seanmonstar.com/blog/reqwest-v013-rustls-default](https://seanmonstar.com/blog/reqwest-v013-rustls-default/);
  [docs.rs/reqwest/tls](https://docs.rs/reqwest/latest/reqwest/tls/index.html);
  [docs.rs/reqwest/tls/Certificate](https://docs.rs/reqwest/latest/reqwest/tls/struct.Certificate.html))
- **License:** MIT / Apache-2.0 dual. Clears the allow-list.
- **TLS backend choice (`rustls` vs `native-tls`).** Picking
  `rustls` for the NAV path. Three reasons. (a) ADR-0020 pins the
  NAV issuing root in ABERP's trust store and explicitly does **not**
  consult the OS trust store for NAV traffic; `rustls`'s
  `RootCertStore` API makes this a static construction at process
  start rather than runtime mutation of the system store. (b) Pure
  Rust supply chain — no OpenSSL / Schannel / Secure Transport
  surface to track. (c) Reproducible builds (ADR-0007 §Supply
  chain) are easier when the TLS implementation is in-tree. The
  Billingo client uses the same `rustls` backend but configured
  with the platform-roots feature (`rustls-native-certs`) so that
  the OS trust store is consulted per ADR-0010.
- **`add_root_certificate()` confirmed available** in the current
  `reqwest::tls` API surface, takes PEM or DER. The NAV pin set
  ships embedded in the binary per ADR-0020 §1.
- **Alternatives rejected:**
  - `hyper` directly — `reqwest` is `hyper` plus the connection
    pool, timeout, redirect, and TLS-configuration surface that
    the NAV poll loop will need anyway. Building those on raw
    `hyper` is duplicate work.
  - `ureq` — synchronous; would fight the runtime decision.
  - `isahc` — libcurl backed; adds a C dependency for no benefit
    over `reqwest`+`rustls`.

---

## HTTP server (for UI ↔ backend wire protocol) — `axum` + `axum-server` + `rustls`

- **Pick:** `axum` for the HTTP router, `axum-server` for the
  listener with `tls_rustls::RustlsConfig` so the loopback listener
  is TLS-terminated by `rustls` per ADR-0007 §Transport ("All UI ↔
  backend traffic over TLS, even loopback. Self-signed cert on
  local, locked to a fingerprint the Tauri shell verifies.")
- **Version lines (2026-05-19):** `axum` 0.7.x / 0.8.x current
  generation; `axum-server` paired version. Pin at the current
  minor of each.
  ([docs.rs/axum-server/tls_rustls](https://docs.rs/axum-server/latest/axum_server/tls_rustls/index.html);
  [docs.rs/axum-server/tls_rustls/RustlsConfig](https://docs.rs/axum-server/latest/axum_server/tls_rustls/struct.RustlsConfig.html);
  [github.com/tokio-rs/axum/blob/main/examples/tls-rustls/src/main.rs](https://github.com/tokio-rs/axum/blob/main/examples/tls-rustls/src/main.rs))
- **License:** MIT for both. Clears the allow-list.
- **Rationale.** ADR-0021 selects HTTPS + JSON over loopback as the
  wire protocol. `axum` is the natural pick on top of `tokio` and
  `hyper`; `axum-server` adds the TLS bind path. The fingerprint
  pinning ADR-0007 calls for is implemented on the Svelte side as
  a one-time fetch-of-public-key check at Tauri shell start, not in
  the server crate.
- **Self-signed cert generation.** Generated at first launch per
  installation, stored alongside the keychain-bound material per
  ADR-0007 §Secrets, fingerprint persisted so the Tauri shell can
  verify on subsequent launches. The cert-generation crate choice
  (e.g., `rcgen`, MIT/Apache-2.0 dual) is a small follow-on
  decision — flagging it for ADR-0021 §Items deferred unless the
  ADR includes it in the stack baseline.
- **Alternatives rejected:**
  - Raw `hyper` + hand-rolled router — duplicate work; loses the
    typed extractor ergonomics `axum` provides.
  - `actix-web` — viable, but the wider Tokio integration story is
    less seamless and `actix-web`'s `unsafe` surface has been a
    point of community contention in years past. `axum` is the
    cleaner fit for ABERP's posture.
  - `warp` — maintained but less actively than `axum`; the
    type-level filter ergonomics are also a steeper learning curve
    with no upside for the ABERP surface.

---

## JSON serialization — `serde` + `serde_json`

- **Pick:** `serde` with `derive` feature; `serde_json` for the
  wire payloads on the UI ↔ backend loopback HTTP path; the same
  `serde` derives reused on `quick-xml`'s serde feature for the
  NAV XML payloads.
- **Version lines (2026-05-19):** both 1.x current; pin at the
  current minor.
  ([crates.io/serde](https://crates.io/crates/serde);
  [crates.io/serde_json](https://crates.io/crates/serde_json))
- **License:** MIT / Apache-2.0 dual. Clears the allow-list.
- **Rationale.** Universal. There is no realistic alternative
  inside the Rust ecosystem for typed JSON, and `quick-xml`'s
  `serialize` feature reuses the same derive machinery so XML and
  JSON share a single set of typed structs in the domain layer.
- **Alternatives rejected:**
  - Hand-rolled JSON parsing — refused.
  - `simd-json` — appealing on benchmark, but ADR-0001's no-`unsafe`
    posture in business modules and `simd-json`'s `unsafe` interior
    do not reconcile cleanly. `serde_json` is the conservative pick.

---

## XML / SOAP handling — `quick-xml` with the `serialize` feature

- **Pick:** `quick-xml` with the `serialize` feature (so `serde`
  derives drive XML serialization). Hand-roll the SOAP envelope as
  a thin wrapper around the NAV payload structs.
- **Version line (2026-05-19):** 0.36.x / 0.37.x current series.
  Pin at the current minor.
  ([github.com/tafia/quick-xml](https://github.com/tafia/quick-xml);
  [capnfabs.net](https://capnfabs.net/posts/parsing-huge-xml-quickxml-rust-serde/))
- **License:** MIT. Clears the allow-list.
- **Rationale.** NAV's interface is SOAP/XML; `quick-xml`'s
  `serialize` feature gives the same derive-based ergonomics as
  `serde_json` and is materially faster than the `xml-rs`-backed
  alternatives (`xmltree`, `roxmltree` indirect). Hand-rolling the
  SOAP envelope is acceptable for a single, stable counterparty
  (NAV's `invoiceService/v3/`) — the envelope is small, well
  documented, and codegen tooling would add a build-time generator
  for no proportional benefit.
  ([mainmatter.com/blog/2020/12/31/xml-and-rust](https://mainmatter.com/blog/2020/12/31/xml-and-rust/);
  [lib.rs/crates/roxmltree](https://lib.rs/crates/roxmltree))
- **Alternatives rejected:**
  - `xmltree` (xml-rs backed) — slower; DOM-only; less serde
    integration. Comparable studies cited above show roxmltree /
    quick-xml at a different performance class.
  - `roxmltree` — read-only; no serialize path; we need both read
    and write.
  - Generated-from-XSD client (e.g., a codegen tool targeting NAV's
    schemas) — bigger build-time surface and a long compile delta
    for a small protocol; reconsider only if the hand-rolled
    envelope creates a maintenance pain point.

### XSD validation at process start — [OPEN, build-phase]

ADR-0009 §1 calls for pinned NAV v3.0 XSD validation at process
start. `quick-xml` does **not** validate against XSD. The realistic
Rust options today are:

1. FFI to `libxml2` via the `libxml` crate (MIT, mature, but adds a
   C dependency and breaks reproducible-pure-Rust posture).
2. A pure-Rust XSD validator (none in the current ecosystem is
   mature enough to bind a load-bearing decision; `xsd-parser` and
   similar are codegen tools, not runtime validators).
3. Hand-rolled invariant checks against the parsed structs — works
   for a tightly scoped XSD, fails for schema drift.

**Decision deferred** to the ADR that lands the NAV adapter code.
Trigger: the first PR implementing schema-drift detection per
ADR-0009 §1. Commit #1 (a binary that generates the XML on disk
without submitting) does not need runtime XSD validation; it can
rely on the `quick-xml` serializer's structural correctness and on
in-development testing against the published XSDs.

---

## Cryptography — RustCrypto `sha2`, `sha3`, `aes`

- **Pick:** `sha2` (for SHA-512), `sha3` (for SHA3-512), `aes` (for
  AES-128/ECB) from the RustCrypto collection. All three are needed
  by the NAV adapter per ADR-0020 §2.
- **Version lines (2026-05-19):** `sha2` 0.10.x / 0.11.x series;
  `sha3` 0.10.x / 0.11.x series; `aes` 0.8.x / 0.9.x series. Pin
  each at the current minor.
  ([crates.io/sha2](https://crates.io/crates/sha2);
  [lib.rs/crates/sha2](https://lib.rs/crates/sha2);
  [lib.rs/crates/sha3](https://lib.rs/crates/sha3);
  [crates.io/aes](https://crates.io/crates/aes);
  [github.com/RustCrypto/hashes](https://github.com/RustCrypto/hashes))
- **License:** dual MIT / Apache-2.0 across the RustCrypto
  collection. Clears the allow-list.
- **Audit posture.** The `aes` crate has had at least one external
  audit (NCC Group, MobileCoin-funded; "no significant findings").
  `sha2` / `sha3` are reference implementations with broad
  ecosystem use. Pure Rust; no FFI surface.
- **MSRV (minimum supported Rust version):** 1.85 across the
  collection at current minors. ABERP's `rust-toolchain.toml` (per
  ADR-0001) will pin to a version ≥ 1.85; this is a constraint on
  the toolchain ADR-0021 declares, not a blocker.
- **AES-128/ECB specifically.** ECB mode is acceptable here because
  the protocol — not ABERP — chose it: NAV returns
  `<encodedExchangeToken>` as a base64-encoded AES-128/ECB
  ciphertext per ADR-0020 §2 and the research file
  `docs/research/nav-and-billingo.md` §"Auth and request signing".
  ABERP's role is to decrypt; we do not use AES/ECB for anything
  we control. A code comment at the call site is required so a
  future maintainer does not generalize the pattern.
- **`zeroize` adjunct.** ADR-0007 §Secrets names `zeroize` for
  in-memory secret zeroization. Pin at the current 1.x minor.
  Clears the allow-list (dual MIT / Apache-2.0).
- **Alternatives rejected:**
  - `ring` — common pick for compiled-binary crypto, but the
    API does not expose AES/ECB and the build story uses
    `cc`-driven assembly that complicates reproducible builds.
  - `openssl` crate — FFI to system OpenSSL; supply-chain surface
    larger than needed for three primitive operations.

---

## Embedded database driver — `duckdb` with `bundled`

- **Pick:** `duckdb` crate with the `bundled` feature, so the
  DuckDB native library is compiled into the ABERP binary at
  build time. Pinned at the current minor.
- **License:** MIT.
- **Rationale.** ADR-0008's audit ledger and ADR-0019's storage
  trait both require a DuckDB-backed adapter at commit #1.
  Vendoring via `bundled` keeps the single-static-binary
  posture (ADR-0001 §Consequences) intact and the reproducible-
  build story under ADR-0007 §Supply-chain self-contained. The
  alternative — linking to a system DuckDB — complicates the
  audit-evidence "the binary the inspector sees is the binary
  that signed the invoice" property by introducing a dynamic
  dependency at install time.
- **`unsafe` posture.** The binding contains `unsafe` (FFI). It
  lives in the adapter layer per ADR-0006 — outside
  business-logic modules — and is permitted by ADR-0001
  ("`unsafe` is permitted only in well-isolated adapters").

## Date and time — `time` crate

- **Pick:** `time` crate with features `formatting`, `parsing`,
  `macros`, `serde`, `serde-well-known`. Pinned at the current
  0.3.x minor.
- **License:** MIT/Apache-2.0 dual.
- **Rationale.** ADR-0008 §"Entry shape" requires RFC3339
  timestamps; ADR-0020 §2 plus `docs/research/nav-and-billingo.md`
  require `YYYYMMDDhhmmss` UTC for NAV `requestTimestamp`. The
  `time` crate covers both natively (`format_description!`
  macro for the NAV format; `serde-well-known` for RFC3339). The
  API is narrower and more correctness-focused than `chrono`,
  with no IANA-timezone-database dependency unless explicitly
  opted into.
- **Alternative rejected:** `chrono` — wider API, broader use,
  but a larger surface to audit and a history of timezone-
  database soundness issues. For a fresh project the
  conservative pick is `time`.

## Canonical byte encoding for the audit-ledger hash chain — `ciborium`

- **Pick:** `ciborium` with CBOR canonical encoding rules per
  RFC 8949 §4.2.1. Pinned at the current 0.2.x minor.
- **License:** MIT/Apache-2.0 dual.
- **Rationale.** ADR-0008 §"Hash chain" defines `entry_hash[N]`
  as a SHA-256 over a "canonical-serialized" entry but did not
  pin the canonical byte mapping. RFC 8949 §4.2.1 specifies a
  deterministic encoding (length-sorted map keys; smallest-form
  integers; no indefinite-length items; etc.) that produces the
  same bytes from the same logical entry on every machine and
  every Rust version. `ciborium` implements the canonical mode
  natively. The hash-chain function lives in **one place
  inside the audit-ledger crate**, not at every call site, so
  the canonical semantics are conformance-checkable.
- **Alternatives rejected:**
  - **Hand-rolled length-prefixed byte layout** — works for a
    closed entry shape, but every new entry-kind field adds a
    review surface; CBOR has the canonical rules already.
  - **Canonical JSON** — less mature canonical-mode libraries
    in Rust; subtle pitfalls with number representations and
    unicode normalization.

## ULID generation — `ulid` crate

- **Pick:** `ulid` crate. Pinned at the current 1.x minor.
- **License:** MIT.
- **Rationale.** ADR-0005 names this crate ("The `ulid` crate
  handles this; we wrap it in an injectable `IdProvider` port
  for testability"). Enumerated here for symmetry with the
  other explicit pins so `Cargo.toml` reflects an intentional
  version line.

## Cross-cutting: cargo-deny + cargo-audit in CI

ADR-0007 §Supply chain already requires `cargo-deny` and
`cargo-audit` in CI. The license allow-list above is what
`cargo-deny` will enforce; the version pins above are what
`cargo-audit`'s advisory check will run against. No additional
research needed here; this is a pointer for the build-phase ADR
that configures CI.

---

## Tracking items — not in scope for ADR-0021

These surfaced during the research lift but do not gate commit #1.
Filing them as just-in-time ADRs when the relevant code PR lands.

- **XSD validation at process start.** Trigger: first PR
  implementing ADR-0009 §1's schema-drift detection. Decision
  surface: `libxml` FFI vs hand-rolled invariant check.
- **Self-signed loopback cert generation crate.** Trigger: first
  PR wiring `axum-server::tls_rustls::RustlsConfig` to real
  startup. Likely pick: `rcgen` (MIT / Apache-2.0 dual). Small
  enough decision that flagging it here avoids surprise gates.
- **`rustls-native-certs` integration for the Billingo client.**
  Same trigger: first PR that wires the Billingo migration HTTP
  client. Crate is MIT / Apache-2.0 dual; the decision is
  configuration, not crate choice.
- **`cargo-deny` lints for thiserror-in-library / anyhow-in-binary
  discipline.** Trigger: first time a module imports `anyhow` in
  a non-test file. Likely added as an ADR-0006 conformance check.

---

## Open questions for external check

These are crate-stack analogues to the NAV / Billingo external-check
list. None are blocking for ADR-0021; they sit in the same
"checked when a Hungarian dev / Rust auditor is consulted" bucket.

1. Is there a Rust SOAP / XML-RPC client that has been used in a
   shipped Hungarian NAV integration? (Not surfaced in
   `docs/research/nav-and-billingo.md`'s consulted clients — those
   are PHP and Node. A Rust-side data point would let ADR-0021
   cite an in-the-wild reference for the `quick-xml` +
   hand-rolled-envelope choice.)
2. Are there Rust-side gotchas with the RustCrypto `aes` crate's
   ECB mode that the consulted clients (PHP `openssl_decrypt`,
   Node `crypto.createDecipheriv`) abstract over (e.g., padding
   handling on NAV's exchange tokens)?
3. Is anyone running `axum` + `axum-server` + `rustls` in a Tauri
   shell at scale today? The pattern is straightforward but a
   shipped reference would let ADR-0021 mention it.

---

# Sources

**Crate registry and docs**

- tokio — `https://crates.io/crates/tokio`
- tokio docs.rs — `https://docs.rs/crate/tokio/latest/source/README.md`
- tokio-rs/tokio — `https://github.com/tokio-rs/tokio`
- clap — `https://crates.io/crates/clap`
- clap docs.rs — `https://docs.rs/crate/clap/latest`
- tracing — `https://crates.io/crates/tracing`
- tracing-subscriber — `https://crates.io/crates/tracing-subscriber`
- tracing-subscriber docs.rs — `https://docs.rs/crate/tracing-subscriber/latest`
- tokio-rs/tracing — `https://github.com/tokio-rs/tracing`
- serde — `https://crates.io/crates/serde`
- serde_json — `https://crates.io/crates/serde_json`
- sha2 — `https://crates.io/crates/sha2`
- sha2 lib.rs — `https://lib.rs/crates/sha2`
- sha3 lib.rs — `https://lib.rs/crates/sha3`
- aes — `https://crates.io/crates/aes`
- RustCrypto/hashes — `https://github.com/RustCrypto/hashes`

**HTTP server and client**

- reqwest tls module — `https://docs.rs/reqwest/latest/reqwest/tls/index.html`
- reqwest tls Certificate — `https://docs.rs/reqwest/latest/reqwest/tls/struct.Certificate.html`
- reqwest v0.13 rustls default — `https://seanmonstar.com/blog/reqwest-v013-rustls-default/`
- axum-server tls_rustls — `https://docs.rs/axum-server/latest/axum_server/tls_rustls/index.html`
- axum-server RustlsConfig — `https://docs.rs/axum-server/latest/axum_server/tls_rustls/struct.RustlsConfig.html`
- axum tls-rustls example — `https://github.com/tokio-rs/axum/blob/main/examples/tls-rustls/src/main.rs`

**XML**

- tafia/quick-xml — `https://github.com/tafia/quick-xml`
- Mainmatter Rust XML — `https://mainmatter.com/blog/2020/12/31/xml-and-rust/`
- capnfabs quick-xml + serde — `https://capnfabs.net/posts/parsing-huge-xml-quickxml-rust-serde/`
- roxmltree lib.rs — `https://lib.rs/crates/roxmltree`

**Error handling**

- Caroline Morton — `https://www.carolinemorton.co.uk/blog/rust-error-handling-anyhow-thiserror/`
- Luca Palmieri error handling deep dive — `https://www.lpalmieri.com/posts/error-handling-rust/`
- OneUptime thiserror+anyhow — `https://oneuptime.com/blog/post/2026-01-25-error-types-thiserror-anyhow-rust/view`
