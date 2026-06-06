# ADR-0066 — Auto-quoting architecture: Python CAD-extract behind a Rust wrapper, pure-Rust deterministic scoring, DB-backed tunables

- **Status:** Proposed
- **Date:** 2026-06-06
- **Deciders:** Ervin (via S265 auto-quoting ground-zero brief)
- **Supersedes:** none — extends ADR-0014 (CAD/CAM artifact storage — stub) by deciding the storage + hash posture for quoting; complements ADR-0057 (quote intake — the *manual* quote bridge; this ADR is the *auto* engine).
- **Related:** ADR-0004 (local desktop, no inbound), ADR-0007 (secrets/encryption baseline), ADR-0019 (relational SoT, no FK, invariants in app), ADR-0057 (operator-pull daemon shape), ADR-0061 (inventory ledger), ADR-0062 (work orders), ADR-0064 (dispatch), the design doc [`docs/design/auto-quoting-ground-zero.md`](../docs/design/auto-quoting-ground-zero.md), and the memory pins [[no-sql-specific]], [[trust-code-not-operator]], [[hulye-biztos]], [[spacex-vertical-integration]].

## Context

The Stage 2 vision (e2e-shop ground-zero §8, Phase 5) is auto-quoting: a customer uploads a CAD file and receives a price computed from geometry + catalogue + manufacturing time + complexity. Three forces shape the architecture:

1. **The geometry kernel is not ours to build.** Parsing STEP/IGES/3MF/STL/X_T into a feature graph means a B-rep kernel. The mature ones (OpenCASCADE) are Python-first (`build123d`, OCP/pythonOCC). Rewriting tessellation in Rust is a multi-year sink for zero competitive differentiation. Per spacex-vertical-integration, we build in-house *everything except* the parts where an external dependency is genuinely load-bearing and undifferentiated — the geometry kernel is exactly that exception (the same class as "we don't build NAV's XSD, we validate against theirs").

2. **The CAD parser is hostile-input-facing.** It parses customer-supplied files of unknown provenance. It can crash, hang, OOM, or emit garbage. It must not be able to take down `aberp serve` or feed unvalidated data into pricing.

3. **Pricing must be deterministic and idempotent.** The same geometry must price identically every time, the override/learn loop must compare estimate-vs-actual cleanly, and `feature_graph_hash` must be a meaningful idempotency key. That requires the scoring step to be a pure function — no I/O, no clock, no RNG in the path.

## Decision

**A three-component split: a Python extractor, a Rust subprocess wrapper, and a pure-Rust scoring crate. Tunables live in DuckDB.**

### 1. `python/aberp-cad-extract` — the extractor (NOT a Rust crate)

A standalone Python program under `python/aberp-cad-extract/` (not `crates/` — it is not Rust). It loads STEP/IGES/3MF/STL/X_T via **build123d (OCP backend)** and emits a **FeatureGraph JSON** on stdout: bounding box, volume, surface area, hole count/sizes, pocket/slot counts by size bucket, min wall thickness, detected tolerances where annotated. It stamps every result with `extractor_version`.

**build123d over raw pythonOCC** because build123d wraps OCP with a saner, better-maintained API surface; we drop to raw OCP only for primitives build123d doesn't expose. Pinned via `requirements.txt` + a vendored venv; the wrapper refuses to run against an unpinned interpreter.

The extractor does **one job**: bytes → FeatureGraph. No pricing, no I/O beyond stdin/stdout, no network.

### 2. `crates/aberp-cad-extract-wrapper` — the blast door

A Rust crate that is the **only** caller of the Python program. It:

- spawns the program as a subprocess with a **hard timeout** (default 60s) and a **memory cap** (rlimit), killing it on breach;
- runs it **sandboxed** — no network, restricted FS (read the one blob, write nothing), per OS facilities;
- validates stdout against a **pinned JSON schema** before deserializing into a `FeatureGraph` struct;
- carries `extractor_version` into the result so a re-extraction after an extractor upgrade is distinguishable;
- returns `Result<FeatureGraph, ExtractError>` — typed errors (`Timeout`, `OutOfMemory`, `Unparseable`, `SchemaViolation`, `UnsupportedFormat`), never raw Python output.

The rest of ABERP never sees the Python program. This is the trust boundary: hostile bytes go in one side, a validated struct or a typed error comes out the other.

### 3. `crates/aberp-quote-engine` — pure deterministic scoring

A Rust crate exposing **one pure function**:

```rust
pub fn score(
    graph: &FeatureGraph,
    catalogue: &CatalogueSnapshot,   // the 8 quoting_* tables, read AS-OF
    params: &QuoteParameters,
    margin: &MarginProfile,
) -> QuoteBreakdown;
```

No I/O, no clock, no RNG, no global state. Same inputs → byte-identical `QuoteBreakdown`. This is what makes `feature_graph_hash` an idempotency key and what the property tests pin (idempotence, monotonicity in quantity, determinism across runs). The daemon (`crates/aberp-quoting`) does all the I/O — read the catalogue snapshot, call the engine, persist the breakdown — and the engine stays a referentially-transparent core.

### 4. Tunables in DuckDB, not TOML

The eight `quoting_*` tables (design doc §11) live in DuckDB. **Reversing the brief's original TOML instinct**: parameters are DB-backed so the operator gets CRUD + per-row history (rendered from the audit ledger) and the learn-loop (design doc §9) has a row to write. Invariants live in app code per ADR-0019 — closed-vocab enums (`stock_status`, margin profile name) validated in Rust, no CHECK on business columns, no triggers. A `CatalogueSnapshot` is the engine's read-only view of these tables at one instant.

### 5. CAD blob + hashing

- Blob encrypted at rest (AES-GCM, keychain key per ADR-0007), content-addressed by `BLAKE3(plaintext)` in the per-tenant blob dir (ADR-0014). Every read audited.
- `feature_graph_hash = BLAKE3(canonical(FeatureGraph))` — the *extracted geometry*, not raw bytes. Canonical = sorted keys + floats rounded to extraction tolerance. Two CAD-version exports of one part hit the same quote; a trivial re-export does not force a re-price.

## Consequences

- **The geometry kernel stays out of our tree.** We own the wrapper and the scoring, not the B-rep math. If build123d/OCP is abandoned, the wrapper's typed boundary means we swap the Python program without touching the engine or the daemon.
- **Hostile input is contained.** A malicious or malformed CAD file can at worst time out or fail schema validation; it cannot crash `aberp serve` or inject into pricing.
- **Pricing is testable in isolation.** The engine is property-testable with synthetic FeatureGraphs and no fixtures — the highest-value test surface in the strand.
- **A Python toolchain is now a build/runtime dependency.** ABERP was pure-Rust + Svelte; this adds a pinned venv to the deploy. The wrapper refuses to run without it, so the failure is loud at boot, not silent at first quote. This is the cost of not building a kernel.
- **Two-language debugging.** A wrong quote could be an extractor bug (Python) or a scoring bug (Rust). The `extractor_version` stamp + the FeatureGraph being persisted alongside the breakdown means either side is reproducible from the stored artifact.

## Adversarial review

- *"A subprocess-per-quote is slow and fragile."* Quote volume is artisan-scale (a handful per day), not a hot path. 60s/quote of extraction latency is invisible against the human acceptance loop (email → customer clicks hours later). Robustness (the blast door) matters far more than throughput here.
- *"Why not a long-lived Python service instead of subprocess-per-quote?"* A persistent service is a stateful attack surface and a lifecycle to manage (health, restart, leak). Subprocess-per-quote is stateless: every quote starts clean, a crash affects exactly one quote, and there is nothing to keep alive. At this volume the spawn cost is noise. Revisit only if volume makes spawn cost real.
- *"Pinning the engine to a pure function forbids it from, e.g., calling the MNB FX API for currency."* Correct, by design. FX, stock reads, and catalogue reads are the **daemon's** job; it resolves them into the `CatalogueSnapshot`/`params` it hands the engine. The engine prices in one currency and the daemon converts. Keeping I/O out of the engine is the whole point.
- *"`feature_graph_hash` could collide for genuinely different parts if the extractor under-describes geometry."* The hash is an idempotency/cache key, not a security boundary. A coarse extractor that maps two different parts to one graph would mis-quote regardless of hashing; the fix is a richer extractor, which the FeatureGraph schema versions additively. The hash faithfully reflects whatever the extractor saw.
- *"A Python venv on a customer's prod machine is an ops liability (CVE patching, interpreter drift)."* Real. Mitigation: vendored pinned venv shipped with the release, wrapper version-checks it at boot, and the sandbox denies it network so a compromised dependency has no exfil path. The geometry kernel is undifferentiated enough that this cost is still cheaper than owning the kernel.

## Alternatives considered

- **Pure-Rust CAD parsing** (e.g. `truck`, `opencascade-rs` bindings). Rejected — `opencascade-rs` is itself a thin binding to the C++ OCCT (so the C++ kernel is still a dependency, just with a worse-maintained surface than build123d); `truck` is immature for STEP/IGES feature extraction. Either way the kernel is not ours; build123d is the better-maintained access path.
- **CAM-as-a-service vendor** (Xometry-style API). Rejected for v1 — streams customer CAD to a third party (trade-secret exposure, ADR-0014 confidentiality), adds a per-quote external dependency and cost, and surrenders the differentiating logic. Reconsider only if in-house extraction proves intractable.
- **Engine with I/O (reads catalogue itself).** Rejected — destroys determinism, makes property tests need a DB, and breaks `feature_graph_hash` idempotency. The snapshot-in, breakdown-out boundary is load-bearing.
- **Tunables in TOML** (brief's original). Rejected — no per-row history, no CRUD ergonomics, and the learn-loop has nowhere to write. DB-backed wins (design doc §11).
- **Run the engine on the storefront** (e2e-shop §8 sketch). Rejected — the catalogue, margin profiles, and reservations all live in ABERP; bringing the CAD to ABERP and pricing there keeps logic and data co-located. The storefront stays thin.

## Open questions

1. **build123d vs raw OCP coverage.** Which feature extractors need raw OCP because build123d doesn't expose them. Resolved during S269 as real extractors land.
2. **Sandbox mechanism per OS.** macOS `sandbox-exec` vs a seccomp/landlock path vs a container. Resolved in S270; the wrapper's typed boundary holds regardless of mechanism.
3. **FeatureGraph schema evolution policy.** How new extractors add fields without breaking old persisted graphs. Additive-only with `extractor_version` gating is the presumptive answer; pinned when the second extractor version ships.

## Invariants pinned

1. **The Python extractor is reachable ONLY through `aberp-cad-extract-wrapper`.** No other crate spawns it. Pinned by code review + a grep test.
2. **`aberp-quote-engine::score` is pure** — no I/O, no clock, no RNG. Pinned by `score_is_deterministic_across_runs` and `score_idempotent_on_same_inputs`.
3. **The wrapper returns a typed error, never raw subprocess output, on any extractor failure.** Pinned by `wrapper_maps_timeout_to_typed_error` and a schema-violation test.
4. **`feature_graph_hash` is BLAKE3 of the canonicalized FeatureGraph, not the raw blob.** Pinned by `same_geometry_different_bytes_same_feature_hash`.
5. **CAD blobs are encrypted at rest; every read emits an access audit.** Pinned by `blob_read_emits_access_audit`.
