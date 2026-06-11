# ADR-0071 — `aberp-compliance`: home crate for defense-grade compliance subsystems. Type/trait/mock scaffold (S345 / PR-39).

- **Status:** Proposed
- **Date:** 2026-06-11
- **Deciders:** Ervin (via S345 / PR-39 defense-pivot Day-1 #2 brief)
- **Supersedes:** none — first ADR of the compliance-subsystems strand.
- **Related:** ADR-0070 (digital-identity provider — the sibling Day-1 foundation), ADR-0008 (audit ledger — the eventual tagging consumer via `nist_800_171`), the aerospace/defense gap analysis (S330, `[[defense-aerospace-pivot]]`), and `[[mock-everything-principle]]`, `[[trust-code-not-operator]]`.

## Context

The aerospace/defense pivot gap analysis (S330) named a family of compliance capabilities ABERP's commercial-ERP core does not have, and which cannot be retrofitted late: export-control classification + denied-party screening (ITAR / EAR), CUI marking (32 CFR Part 2002 / DoD CUI Registry), lot/heat material traceability (AS9100D §8.5.2), an approved-vendor list with DPAS priority ratings (FAR 11.6), and the NIST SP 800-171 control set (DFARS 252.204-7012).

These are distinct concerns from the audit ledger (ADR-0008, *what changed* + tamper evidence) and the digital-identity layer (ADR-0070, *who acted*). Accreting them onto either crate would blur those boundaries and pull defense-specific surface into low-level infrastructure that has its own clean dependency posture. They need a home.

We also do not yet have a target defense customer, so — exactly as with ADR-0070 — wiring any real backend (a denied-party screening API, a mill-cert capture flow) now would be the CLAUDE.md #2 / #13 speculative-coupling trap. The proven ABERP pattern is **abstraction-then-implementations**: define the seam, ship a mock, let real backends land per customer demand.

## Decision

**A new crate `crates/aberp-compliance` that homes every defense-grade compliance subsystem going forward. S345 ships the skeleton — module boundaries, public types, swap-point traits, and explicitly-non-production mock backends — and nothing else. No real backends, no audit `EventKind`s, and the crate is deliberately NOT yet a dependency of `apps/aberp`. S346+ fill the modules and wire the audit events.**

### 1. Module map (mirrors the S330 subsystem list)

- `export_control` — `ExportControlProvider` trait (`classify` + `screen_party`), `ExportClassification` (`ECCN` / `USMLCategory` / `EAR99` / `NotClassified` / `Pending`), `ScreeningResult`, the `Classifiable` input trait, and `MockExportControlProvider` (answers `NotClassified` + `Clear` for everything, WARNs on construction).
- `cui` — `CuiMarking` (`Unclassified` / `Cui(CuiCategory)` / `Confidential` / `Secret` / `TopSecret`), a starter `CuiCategory` subset of the DoD CUI Registry, and the `is_cui()` / `is_classified()` / `display_marking()` helpers (banner marking per DoD conventions: `UNCLASSIFIED`, `CUI//CTI`, `SECRET`, …).
- `lot_heat` — validated `LotId` / `HeatId` newtypes (`[A-Za-z0-9-]`, non-empty, ≤ 32 chars) + the `MaterialTraceabilitySeed` record. Types only; capture wiring lands S348.
- `avl` — `ApprovedSupplierEntry` + `QualLevel` (`Bid` / `Approved` / `Disapproved`, with `can_bid()` / `can_deliver()` gating), `DpasRating` (`None` / `DoC1` / `DxC1`), `ExportScreeningStatus`.
- `nist_800_171` — all 110 NIST SP 800-171 Rev. 2 control identifiers as `&'static str` constants (`AC_3_1_1` … `SI_3_14_7`), plus an `ALL_CONTROLS` array. These are tags a future audit `EventKind` references so ledger events trace back to the control they satisfy.

### 2. Mock-first, loud about it

Per `[[mock-everything-principle]]`, every provider seam ships a deterministic mock that is **impossible to mistake for production**. `MockExportControlProvider` performs no real classification or screening and logs `ExportControlProvider: MOCK — … NOT FOR PRODUCTION USE` at WARN on every construction — the same guardrail as the ADR-0070 `MockProvider`. The lot/heat / CUI / AVL modules are pure value types with no I/O, so they need no mock; their integrity comes from constructor validation (`LotId::new` rejects whitespace / symbols / over-length) rather than a stub backend.

### 3. Style + dependency posture

The crate mirrors `aberp-digital-id` exactly: `#![forbid(unsafe_code)]`, `#![warn(missing_debug_implementations)]`, `[lints] workspace = true`, workspace-inherited package fields, and the minimal dependency set `serde` (derive only) + `thiserror` + `tracing`. No `serde_json` at runtime — the crate defines and validates types, it never parses; `serde_json` is a dev-dependency for the roundtrip tests only. No new top-level workspace dependencies.

### 4. Not yet a dependency of `aberp`

`crates/aberp-compliance` is registered in the workspace `members` list so it compiles + tests in CI, but it is **not** added to `apps/aberp`'s `Cargo.toml`. That edge lands in S346 alongside the first audit `EventKind` that references a compliance type — keeping the F12 four-edit ritual and the binary-layer coupling out of a pure-scaffold session.

## Consequences

**Positive.** The module boundaries the next 5+ sessions depend on exist and are pinned by ~30 tests. The seams (export-control provider) are proven; the value types (lot/heat, CUI, AVL) are validated at construction. No vendor or backend chosen prematurely. Downstream sessions add real providers + audit wiring without re-litigating the shape.

**Negative / deferred.** Nothing is enforced yet: the export-control mock screens nothing, no record carries a `CuiMarking`, no material receipt captures a `MaterialTraceabilitySeed`, no partner carries an `ApprovedSupplierEntry`, and no audit event tags a NIST control. `CuiCategory` is a deliberate starter subset of the DoD CUI Registry, not the full index. The newtype `Deserialize` paths bypass `new()` validation (acceptable for a foundation type; a `try_from`-on-deserialize hardening is future work if untrusted CUI ingest appears).

**Future work (not S345):** real export-control / screening backends per customer demand; the audit `EventKind`s that reference these types + the `aberp → aberp-compliance` dependency edge (S346); the AVL wiring onto partner master data (S347); the lot/heat capture flow on receiving + consumption (S348); CUI marking enforcement on PDFs and storage; extending `CuiCategory` as real flowdowns demand.
