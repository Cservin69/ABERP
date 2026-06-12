# ADR-0081 — Coverage hardening for the NAV-leakage gates (`extract_nav_xml`).

- **Status:** Proposed
- **Date:** 2026-06-12
- **Deciders:** Ervin (via S364 / PR-51 defense-pivot batch 9/10 brief)
- **Supersedes:** none.
- **Related:** ADR-0073..ADR-0079 (the seven `personnel.*`/`material.*`/`part.*`/`export.*`/`cui.*`/`supplier.*`/`incident.*` event families whose additions this hardens against), ADR-0035 §4 (the byte-equality + root-element NAV verification this protects), the defense-aerospace gap analysis (S330, `[[aberp-defense-aerospace-pivot]]`), and `[[customer-journey-e2e-gate]]`.

## Context

Two independent functions decide, per `EventKind`, whether an entry's payload carries verbatim NAV XML bytes:

- `aberp-verify::extract_nav_xml` — the offline bundle *verifier*: extracts the bytes so it can re-check byte-equality and root-element pins (ADR-0035 §4).
- `apps/aberp::export_invoice_bundle::extract_nav_xml` — the bundle *writer*: decides which entries get a `nav/<seq>_<kind>.xml` file in a per-OUTGOING-invoice export.

Both are the firewall that keeps non-invoice audit rows — including the new defense families, which may carry CUI markings, export-control verdicts, personnel access trails, or cyber-incident metadata — from ever being written into, or expected in, a NAV bundle. A row wrongly classified as NAV-bearing leaks app-internal payload into an artefact that crosses the NAV boundary.

Across S355–S363 (ADR-0073..ADR-0079) we added thirteen `EventKind` variants across seven new prefix families. Each session manually added the corresponding "no NAV bytes" arm to **both** functions. This "F12 ritual" worked, but it rested entirely on session diligence: nothing made a forgotten arm fail *before* production.

### Why exhaustive matches alone aren't enough

Both functions already `match entry.kind` with **no `_` catch-all** — verified in this session. So adding a variant *does* break the build until an arm exists. That is the primary, pre-existing gate and it is strong.

But the compiler only forces *an* arm, not a *correct* one. A contributor under time pressure can fold a new variant into the existing no-NAV group (`(None, "")` in the verifier, `None` in the writer) without actually deciding whether it carries NAV bytes — or, worse, can mis-route a genuinely-NAV-bearing kind. The exhaustive match cannot tell a thoughtless arm from a considered one. The leak it prevents is "no arm at all"; it does not prevent "wrong arm."

## Decision

**Add two belt-and-braces layers on top of the existing exhaustive matches, so that forgetting — or fumbling — an `aberp-verify` / bundle-writer arm fails at compile time or test time, never silently at runtime in production.**

Nothing here *replaces* the manual F12 ritual or the exhaustive matches; this is defense-in-depth, not a substitute. Proc-macro code-generation of the gates was explicitly left out of scope.

### 1. `EventKind::ALL_KINDS` + `ALL_KINDS_COUNT`, and a compile-time drift tripwire

`crates/audit-ledger/src/entry/event_kind.rs` gains a hand-maintained `pub const ALL_KINDS: &[EventKind]` (one entry per variant, in `as_str` order) and `pub const ALL_KINDS_COUNT: usize = Self::ALL_KINDS.len()`.

Each gate carries a compile-time assertion:

```rust
const _: () = {
    assert!(EventKind::ALL_KINDS_COUNT == 103, "…re-review extract_nav_xml…");
};
```

When a variant is added, `ALL_KINDS_COUNT` changes and **both** `const _` blocks fail to compile, with a message that names `extract_nav_xml` and ADR-0081. The contributor cannot bump the number without landing on the exact lines that ask "does this new kind carry NAV bytes?". The compiler thus forces not just *an arm* but a *deliberate revisit* of the decision.

The honest weak link is `ALL_KINDS` itself: without a proc-macro, nothing at the language level guarantees a hand-maintained array enumerates every variant. We close that with **double-entry**: the long-standing `round_trip_for_every_variant` test already keeps its own independent hand-list of every variant, and now asserts `&variants[..] == EventKind::ALL_KINDS`. A contributor who updates one list but not the other fails that test. So `ALL_KINDS_COUNT` is a trustworthy drift signal precisely because two independently-maintained enumerations must agree before it can move. (A `103`-line second list is duplication, but it is *functional* duplication — a checksum, not dead weight — so it earns its keep against CLAUDE.md rule 13.)

### 2. Per-family + sweep runtime "no NAV bytes" pins

The bundle writer already had `extract_nav_xml_returns_none_for_*_kinds` tests per family (S355+). This session brings the **verifier** to parity — it previously had *no* per-family NAV pin at all, only a 16-kind round-trip canary — adding `personnel_no_nav_bytes`, `material_no_nav_bytes`, `part_no_nav_bytes`, `export_no_nav_bytes`, `cui_no_nav_bytes`, `supplier_no_nav_bytes`, and `incident_no_nav_bytes`.

Both functions also gain a **future-proof sweep**: iterate `EventKind::ALL_KINDS`, skip the nine NAV-bearing invoice kinds via a single in-test allowlist, and assert every other kind extracts to no NAV bytes when fed a `b"{}"` payload. A *new* variant lands in this sweep automatically — no one has to remember to write a per-family pin. If its arm mis-routes it to a NAV-decode path, the `{}` payload fails to deserialize and the test panics; if it returns bytes, the assert fires. Either failure mode is a leak caught in CI.

## Consequences

- **Forgetting an arm now fails loud, early.** A new `EventKind` cannot reach production with an unreviewed NAV verdict: the exhaustive match forces an arm, the `const _` drift assertion forces a re-review, and the sweep test catches a wrong verdict. (CLAUDE.md rule 12 — fail loud.)
- **Adding a variant now costs four coordinated edits** (the variant, `as_str`, `from_storage_str`, `ALL_KINDS`) plus bumping the two `103` pins. The pins are the point: the friction *is* the re-review prompt. The failure messages name the files and this ADR so the next contributor knows exactly what to do.
- **No production behaviour changes.** All additions are a const, tests, and compile-time assertions; the gate logic is untouched.
- **The manual ritual stays.** This is belt-and-braces. If a future session wants to delete the ritual outright, the path is a proc-macro that derives the gates from a per-variant attribute — deliberately out of scope here.
- **Honest residual risk:** the two `103` pins and the two hand-lists are human-maintained. The double-entry test makes a silent single-list omission fail, but a contributor who edits *all* of them wrongly in the same direction could still drift. That is a much smaller surface than the original "remember to touch two match statements," and it is surfaced rather than hidden.
