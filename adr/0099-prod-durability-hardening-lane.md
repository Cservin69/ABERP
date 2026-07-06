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

---

## H1 — Class 4: mirror boot-reconcile → preserve-and-refuse

**Status:** Implemented on `prod-durability-adr0099` (code-only; no deploy/cut —
GL-2 not granted). **Date:** 2026-07-06.

### Invariant

The on-disk audit mirror (`<db>.audit.log`) is NEVER silently truncated, trimmed,
or rebuilt while it may hold entries the DB lacks. Any divergence between the
mirror and the DB ⇒ **preserve the evidence + refuse to serve** (boot exits
non-zero). The mirror is a derivable cache ONLY while it cannot hold entries the
DB lacks; once it might, it is treated as primary evidence.

### Problem (what H1 replaces)

`ensure_consistent_with_db` (Session 152b) reacted to divergence by silently
rewriting the mirror from the DB:

- **ahead-of-DB** (`mirror_max_seq > db_max_seq`) → `rebuild_mirror_from_db` →
  `RecoveryAction::Truncated`. This is the fingerprint of a torn-write / lost DB
  commit (the 2026-06-22 corruption class); truncating destroyed the ONLY
  surviving record of what the DB lost.
- **corrupt / torn mirror** → silent `rebuild_mirror_from_db` → `Rebuilt`,
  destroying an intact prefix that may hold entries the DB dropped via a WAL tail.
- **equal-length head-hash mismatch** → silent `Rebuilt`.

### Decision — the three arms (backported from editions ADR-0098 R1 @ `1a56872`)

Backported faithfully from the production-proven editions arms
(`crates/audit-ledger/src/mirror.rs`), which fired correctly in production
(Defense) refusing to boot on a deep-corrupt mirror:

- **(a) ahead-of-DB** → PRESERVE the ahead mirror to `<mirror>.ahead-<nanos>.bak`
  (byte-for-byte, original left intact) and return `AppendError::MirrorAheadOfDb`.
  Boot exits non-zero with an operator-actionable message + recovery pointer.
  Direct backport of editions `preserve_ahead_mirror`.
- **(b) corrupt / torn mirror** → a unified, side-effect-free torn-tail
  classifier (`read_mirror_under_tail_policy` → `MirrorTailPolicy`) re-verifies
  the newline-terminated prefix **genesis→head** (JSON + ascending-contiguous seq
  from 1 + inter-entry hash-chain links). A **lone torn trailing line** ("the
  append never durably happened") whose intact prefix the DB head COVERS is
  preserved to `<mirror>.corrupt-<nanos>.bak`, the ONE torn tail is durably
  trimmed, and boot CONTINUES with an audit event; the reconcile then extends the
  trimmed prefix from the DB. **Any deeper corruption** (a mid-file break/gap/JSON
  /chain mismatch) ⇒ preserve + REFUSE (`MirrorCorruptPreserved`). NEVER a silent
  rebuild-from-DB; NEVER operator JSONL hand-editing.
- **(c) equal-length head-hash mismatch** → PRESERVE to `<mirror>.corrupt-<nanos>.bak`
  + REFUSE (`MirrorCorruptPreserved`). Two equal-length chains with different heads
  on prod-class data is worse than a torn tail; never auto-resolve.

### Bug-3 pre-fix (prod-specific)

Editions' `read_mirror_under_tail_policy` TRIMMED the torn tail on disk INSIDE the
read, BEFORE the reconciler could confirm the DB head covers the trimmed prefix —
so a torn tail whose intact prefix was STILL AHEAD of the DB had its live file
mutated even though boot then refused (editions Bug 3). The prod port makes the
read **side-effect-free**: it only classifies and returns the intact prefix, and
the boot caller applies "preserve → trim ONE torn tail **only if DB head ≥ trimmed
head** → continue", routing a still-ahead trimmed prefix to the arm-(a)
preserve+refuse **without mutating the file**. The classifier is written as the
single reusable boot+recovery mirror-read policy (H5 reuses it); H1 wires ONLY the
boot side and does not touch recovery code.

### Scope / surface

- `crates/audit-ledger/src/mirror.rs` — the reconcile arms + the torn-tail policy
  (`MirrorTailPolicy`, `read_mirror_under_tail_policy`, `decide_tail`,
  `parse_and_reverify_prefix`, `classify_mirror_bytes`, `preserve_corrupt_mirror`,
  `trim_mirror_to`, `preserve_ahead_mirror`); `RecoveryAction::Truncated` removed.
- `crates/audit-ledger/src/error.rs` — two new `AppendError` variants
  (`MirrorAheadOfDb`, `MirrorCorruptPreserved`): the necessary TYPE surface the
  in-scope arms return.
- `apps/aberp/src/serve.rs` (~:942 boot call site) — the refuse arms log one
  operator-actionable line and exit boot non-zero.
- **No new live-DB opener** — the opener census (289/42) is unchanged; H1's file
  I/O is `std::fs`/`OpenOptions` on the mirror path, which the scanner does not
  count, and the boot `Connection::open` is pre-existing.

### Rollback — binary-only

H1 changes runtime behavior only; there is **no schema change, no data migration,
no mirror on-disk format change**. Rolling the binary back to `PROD_v2.27.76`
fully reverts to the prior behavior. The `.ahead-*.bak` / `.corrupt-*.bak` side
files a refuse arm may write are inert evidence artifacts — the old binary ignores
them (they do not match the `<db>.audit.log` mirror path). No forward-migration to
undo.

### Tests (RED-before / GREEN-after; every gate has a proving negative probe)

Unit matrix over all three arms in `mirror.rs`: ahead ⇒ refuse + `.ahead-*.bak` +
original intact; torn-tail ⇒ preserve + trim + continue; torn-tail-prefix-still-
ahead ⇒ refuse WITHOUT trimming (the Bug-3 pre-fix probe); deep-corrupt (mid-file
chain break) ⇒ refuse; equal-length head-hash mismatch ⇒ refuse; evidence
preserved in every refuse arm; plus the pure `decide_tail` truth table and a
`parse_and_reverify_prefix` chain-break/seq-jump probe. Authoritative build/test is
GitHub Actions (`ci.yml` + `cut-gate.yml`) — local DuckDB build is heavy.
