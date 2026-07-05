# ADR-0099 — Production durability-hardening lane (SAFE setup)

**Status:** Accepted (lane setup only; durability implementation deferred)
**Date:** 2026-07-05
**Context repo:** `Cservin69/ABERP` (production line)
**Branch:** `prod-durability-adr0099` off frozen `PROD_v2.27.76` (`f7519b4`)

## Context

The editions tree (`Cservin69/ABERP-Editions`) hardened its single-file DuckDB
durability under ADR-0093/0098: all runtime DB access was collapsed onto one
shared `aberp_db::Handle`, residual openers were frozen and pragma-guarded, and a
mechanical **cut-gate** (`tools/cut_gate_db_isolation.sh`) plus a negative-probe
harness make the invariants un-droppable in CI. The production line has not yet
received that durability work. Before writing any prod durability code we want the
**same mechanical guardrail lane** proven on the prod tree, so the durability
session (H1: mirror preserve-and-refuse; H3: shared Handle) lands on rails.

## Decision

Stand up the guardrail lane **without** any durability code or prod-runtime touch:

1. Port the toolchain-free **opener scanner** (`tools/adr0098_opener_scan.awk`,
   verbatim — comment/string/`cfg(test)`/alias-aware).
2. Port only the **census-freeze** mechanism of the editions gate (editions
   CHECK 10i + 10k) as `tools/cut_gate_opener_census.sh`:
   - **CHECK P1** — per-file runtime-opener count is frozen; no file may exceed
     its baseline and no new opener-bearing file may appear.
   - **CHECK P2** — the exact set of per-opener fingerprints is frozen (catches a
     count-preserving intra-file swap).
3. Seed the frozen baseline from the **current** prod census: **289 runtime
   openers across 42 files** (`tools/adr0098_prod_frozen_residuals.txt` +
   `tools/adr0098_prod_opener_fingerprints.txt`), machine-derived from `f7519b4`.
   This is the **pre-H3 census — every opener is currently ALLOWED**. The gate
   freezes the surface so it cannot GROW; it does **not** yet require zero.
4. Port the **negative-probe harness** (`tools/cut_gate_negative_probes.sh`) to
   prove P1/P2 have teeth.
5. Wire CI: a standalone toolchain-free **`cut-gate.yml`** (the intended required
   check) + the fast gate added as a fail-fast pre-build step in the existing
   single-arm **`ci.yml`** (prod is one product line, so the honest analog of the
   editions Portable+Defense 2-arm matrix is a single build+test+clippy+fmt+
   deny/audit arm — unchanged from prod's existing CI).

## Explicitly NOT ported (require durability code — out of this lane)

Editions CHECK 1–9, 10a–10h, 10j assert durability/edition-saw-off code that does
not exist in the frozen prod tree: the `aberp-db` Handle, `crash_safe.rs`
atomic-rename checkpoint, `mirror.rs` preserve-and-refuse (`MirrorAheadOfDb`),
`build_profile` Edition→root binding, `SAW-OFF.md`, per-edition launchers,
storefront gating, and `PRAGMA disable_checkpoint_on_shutdown` on residual
openers. Porting them here would go RED or force durability code into a lane whose
sole purpose is to be a GREEN, proven baseline. They land with H1/H3.

## Consequences

- The prod opener surface can no longer silently grow while durability is written.
- A GREEN baseline on this branch proves the lane (gate + CI) before any risky
  durability change.
- Follow-up (separate prod session, gated on Ervin's prod-stop + GL-2): H1 mirror
  preserve-and-refuse, then H3 shared Handle; as openers migrate onto the Handle,
  the frozen counts ratchet DOWN (P1 already forbids growth).
