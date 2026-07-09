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

---

## H2 — Class 3: atomic creation + safe-open-on-boot

**Status:** Implemented on `prod-durability-adr0099` atop H1 (code-only; no
deploy/cut — GL-2 not granted). **Date:** 2026-07-06.

### Invariant

A crash during the FIRST creation of the tenant DB can never leave a torn file at
the live path; and a torn live file present at boot is detected at ONE guarded
chokepoint BEFORE any subsystem opens it — preserved as evidence + refused, never
opened half-torn by a downstream migration.

### Problem (what H2 replaces)

The serve boot (`apps/aberp/src/serve.rs`, "ensure billing schema" step) created
the DB by letting the FIRST `DuckDbBillingStore::open(&args.db)` materialise it
directly **at the live path**. A power loss mid-creation left a torn/unopenable
file exactly where the next boot expects a good one (the first-launch torn-create
class). And an already-torn live file was only discovered lazily, by whichever of
the ~11 subsystem boot opens hit it first — after other opens/daemons had already
begun.

### Decision — the boot chokepoint (backported from editions ADR-0095 §1·§2·§4)

A single guarded block runs right after the parent-dir `create_dir_all` and
BEFORE the first `DuckDbBillingStore::open` (hence before every subsequent boot
open and all daemon spawns):

- **stale-staging sweep** — `cleanup_stale_staging` removes any `<db>.creating-*`
  litter a crash-interrupted prior provision left, on BOTH arms.
- **DB MISSING ⇒ `provision_atomic`** — build the DB ASIDE at
  `<db>.creating-<tag>.duckdb` (the closure seeds the billing schema there, a
  faithful port of the editions wiring), fold its WAL (`CHECKPOINT`), then
  `atomic_install` (fsync → atomic `rename` → clear stale target WAL → fsync dir)
  onto the live path and `write_marker` the verified-good `<db>.ckpt-ok`. A crash
  before the rename leaves only a disposable temp; the live path is never written
  with a torn file. The remaining subsystem schemas complete idempotently on the
  now-safely-present DB.
- **DB PRESENT ⇒ `probe_open_or_preserve`** — the SINGLE validated probe-open
  (`Connection::open` + `PRAGMA database_list;`, the exact catalog-touch the
  editions boot-crash e2e uses to prove a torn file will not open). Success ⇒ boot
  proceeds. Failure ⇒ the corrupt file is PRESERVED byte-for-byte to
  `<db>.CORRUPT-<ts>` (a COPY — original left in place) and boot REFUSES non-zero
  with an operator-actionable line, exactly as H1's mirror arms do. **H2 stops at
  preserve-and-refuse; the guarded auto-recovery is H5** (the editions
  `attempt_db_auto_recovery` / `recover_or_refuse` path is deliberately NOT
  ported here).

### Prod-backport adaptation (flagged)

The editions `provision_atomic` / probe entrypoints each call `ensure_not_prod_path`
first, so an *editions* build can never act on the FROZEN prod line (`~/.aberp/`,
ADR-0093). That guard is DELIBERATELY OMITTED from the prod backport — in the prod
tree the live DB *is* the prod line H2 must provision, so porting the guard would
refuse the very path this code exists to create. This is the only intentional
divergence from the settled editions forms.

### Scope / surface

- `crates/aberp-snapshot/src/crash_safe.rs` (NEW) — the backported §1·§2·§4
  primitives, scoped to H2 (no recovery engine): `atomic_install`, `write_marker`
  / `read_marker` / `checkpoint_is_current` + `CheckpointMarker`, `provision_atomic`
  + `checkpoint_file`, `probe_open_or_preserve` + `preserve_corrupt_db`,
  `cleanup_stale_staging` + `cleanup_siblings_with_infix`, and the fsync/sibling/
  tag helpers.
- `crates/aberp-snapshot/src/lib.rs` — `mod crash_safe;` + re-exports; two new
  `SnapshotError` variants (`Provision`, `DbCorruptPreserved`), the type surface
  the in-scope arms return. `result_large_err` is workspace-allowed.
- `crates/aberp-snapshot/src/take.rs` — `sha256_file` promoted to `pub(crate)` so
  the §4 marker records the same file identity (no behaviour change).
- `apps/aberp/src/serve.rs` — the boot chokepoint block (provision / probe /
  sweep) ahead of the first `DuckDbBillingStore::open`.

### Opener census — legitimately altered boot open path (+3: 289→292 / 42→43)

H2 replaces the implicit torn-create with the atomic/probe path, so the frozen
census is ratcheted up by exactly the atomic/probe openers, with the fingerprint
set updated to match (CHECK P2 re-proven to still catch a count-preserving swap
on the new openers):

- `apps/aberp/src/serve.rs` 144→145 — the `DuckDbBillingStore::open(creating)`
  provision seed-open (replaces the old implicit live-path creation).
- `crates/aberp-snapshot/src/crash_safe.rs` +2 (new file) — `checkpoint_file`'s
  fold-open + `probe_open`'s validated safe-open.

No NEW un-gated opener is added; the count-freeze (P1) still forbids growth
elsewhere, and the `#[cfg(test)]` opens in `crash_safe.rs` are correctly excluded
by the scanner.

### Rollback — binary-only

H2 changes runtime behaviour only; there is **no schema change, no data migration,
no on-disk format change**. Rolling the binary back to `PROD_v2.27.76` fully
reverts to the prior behaviour. The `<db>.ckpt-ok` marker and any `<db>.CORRUPT-*`
/ swept `<db>.creating-*` side files are inert to the old binary (they do not match
the live DB path). No forward-migration to undo.

### Tests (RED-before / GREEN-after; every gate has a proving negative probe)

Plain-file unit matrix in `crash_safe.rs` (no DuckDB → runs in every arm):
`atomic_install` replace / crash-before-rename-leaves-old-good / stale-target-WAL
clear; marker round-trip + `checkpoint_is_current` (matching / staled / pending-WAL
/ no-marker); stale-staging sweep keeps `.CORRUPT-*` evidence + never touches the
live DB; `preserve_corrupt_db` copies aside + leaves original intact; and the
load-bearing real-subprocess crash-injection test — a child writes the `.creating-`
staging then `abort()`s before the rename, and the parent asserts **no file at the
live path** + the temp survives + the retry finishes the install with zero manual
steps. DuckDB-backed e2e in `tests/crash_safe_boot_e2e.rs` (CI gate): provision ⇒
valid openable DB + verified-good marker + no staging litter; stale `.creating-*`
swept by the next provision; probe OK on a clean DB; and the **refuse-arm form of
`boot_crash_recovery_e2e`** — a torn live DB ⇒ `DbCorruptPreserved` + one
`<db>.CORRUPT-<ts>` byte-for-byte copy + original left in place (no recovery).
Authoritative build/test is GitHub Actions (`ci.yml` + `cut-gate.yml`); the
cut-gate + negative probes are toolchain-free and were run locally green (the P2
teeth re-proven against the new openers).

---

## H3 — Class 1a/1b: one shared DuckDB `Handle` (in-process single-writer)

Backported from the production-proven editions consolidation
(`Cservin69/ABERP-Editions` @ `1e6097d`, ADR-0098/0099) under the LOCKED plan
`PROD-HARDEN-2027.v1.0`. Landed incrementally on this branch; this section
tracks the state and the exact remaining migration surface.

### Invariant
Exactly ONE `duckdb::Database` instance per `serve` process. Every runtime
write routes through the shared `aberp_db::Handle` (`db.write()` +
`append_in_tx`); every runtime read through `db.read()` (a `try_clone` of the
one instance). ZERO non-Handle in-process runtime write-forks — where a
write-fork is the TRUE fork primitive: ANY independent live-DB opener
(`Connection::open` / `Ledger::open` / `append_reopen` / `DuckDbBillingStore::open`)
followed by an audit append, in the same `serve`-process fn, outside the shared
Handle. Allow-list ONLY: the `aberp-db`/`aberp-snapshot` seams,
`#[cfg(test)]`/`open_in_memory`/`from_connection`, the pre-serve boot
create/probe/recover openers (H2), and separate-process CLI one-shots (fenced by
the whole-DB flock, F-E).

### Problem (what H3 removes)
The `serve` process hosts ~7 daemons + every request handler, each historically
calling its OWN `Connection::open`/`Ledger::open`/`append_reopen` on the same
single-file tenant DB, concurrently. DuckDB single-file storage is
single-writer: N separate opens = N checkpoint actors racing one file
(`duckdb#23046` torn metadata), AND two openers off the same audit head each
self-assign the next `seq` → a forked audit ledger. The Defense line forked 4×
(seq 369→416→428→515) precisely because openers were migrated PIECEMEAL; H3's
gate is therefore ZERO residual, atomic — never partial.

### Decision — the shared `Handle` (backported from editions `1e6097d`)
`crates/aberp-db`: `Handle` / `WriteGuard` / `read()` = `try_clone` of the ONE
instance + a post-commit lockstep `sync_mirror` hook, plus the pure D2
`debounce` module. `AppState { db: HandleArc }`; the Handle is constructed at
boot AFTER the H2 `provision_atomic` / `probe_open_or_preserve` chokepoint and
threaded into every daemon `Deps`.

Three deliberate prod adaptations vs. the editions source:
1. **No `ensure_not_prod_path`** — that editions guard stops a Defense/dev build
   from opening the real prod DB; the prod build legitimately operates on the
   prod DB (the prod `aberp-snapshot` omits the guard by design).
2. **`checkpoint_enabled` defaults `false`** — the runtime VALIDATED durable
   checkpoint (`aberp_snapshot::live_durable_checkpoint`, quiesce→EXPORT→
   atomic_install→reopen) is **H4's step**. In H3 the single-instance discipline
   makes DuckDB's own bounded auto-checkpoint safe in the interim.
   `Handle::run_durable_checkpoint_locked` is a clearly-marked H4 seam (a one-line
   swap); the `aberp-snapshot` dep is deferred to H4 accordingly.
3. Otherwise faithful — no re-derivation.

### Mandatory pre-fixes (landed with the crate)
- **Bug 5 (poison policy).** A panic while holding the shared `WriteGuard` would
  poison the ONE process-wide writer mutex — a NEW single point of failure the
  shared instance introduces. `Handle::write`/`read`/`checkpoint_on_idle` route
  through `lock_recovering`: on a poisoned lock they `clear_poison`, reclaim the
  guard, drop+reopen FRESH, and re-verify the audit hash-chain genesis→head. A
  benign prior panic RESUMES; a FAILED re-verify is surfaced HARD
  (`PoisonRecoveryFailed`, never served from a bad DB). The recovery emits a
  `db.auto_recovered` (trigger `writer_poison_recovered`) forensic audit row.
- **F-C (try_clone coherence).** `read()` is a `try_clone` of the shared
  instance (one buffer cache), so a read observes every committed write
  immediately — the S335 coherence property a separate read-only instance could
  not provide. Pinned by the ported coherence e2e tests.
- **F-A (runtime pragma + policy marker).** `open_runtime_connection` issues the
  engine-adapter pragmas (`disable_checkpoint_on_shutdown` + `wal_autocheckpoint`
  raise) behind an in-code policy marker documenting the authorized exception to
  ADR-0021 `[[no-SQL-specific]]`. A pragma-presence gate check asserts the marker
  + pragma are present.

### New audit event
`EventKind::DbAutoRecovered` (`db.auto_recovered`) added via the full F12 ritual:
variant + `as_str` + `from_storage_str` + both `ALL_KINDS` lists + the three
count pins (138→139: `all_kinds_count_is_pinned`, `aberp-verify` and
`export_invoice_bundle` `const _` drift asserts) + the `db.`-scoped (never NAV
XML) arm in both `extract_nav_xml` sweeps (ADR-0081).

### Cut-gate evolution
- The frozen opener-census gate (`cut_gate_opener_census.sh`, CHECK P1/P2) now
  EXCLUDES the `crates/aberp-db` shared-instance seam (its `Connection::open` is
  the fix, not a residual) — mirroring the editions gate. Negative probes still
  green (teeth intact).
- The **zero-residual write-fork gate** (editions CHECK 10M form:
  `tools/adr0099_write_fork_scan.awk` + `tools/adr0099_write_fork_allowlist.txt`,
  run by `tools/cut_gate_write_fork.sh`, opener+append per fn) is the H3 cut gate.
  It runs **INFORMATIONAL** in CI today (prints the remaining in-serve forks +
  count, exit 0 — the branch stays green and the exact remainder is visible), and
  flips to **ENFORCING** (`ENFORCE_WRITE_FORK=1`, fail on any non-allow-listed
  fork) the moment the migration reaches zero. That flip is the H3 acceptance
  cut. Allow-list: the `append_reopen` primitive, separate-process CLI one-shots
  (fenced by the F-E flock), and pre-serve boot openers.

### Tests (RED-before / GREEN-after)
`crates/aberp-db/tests/handle_concurrency_e2e.rs` (checkpoint-DISABLED subset;
the checkpoint-fold tests land with H4):
`concurrent_separate_opens_tear_the_file_but_shared_handle_never_does`,
`daemon_write_appends_to_mirror_in_lockstep`, the F-C/S335 coherence pair,
the separate-boot-opener fork repro + shared-Handle coherence pair, and
`poisoned_writer_is_recovered_in_place_not_bricked` (Bug 5). Plus the 9 pure D2
`debounce` unit tests.

### Landed state / remaining migration

#### Migration progress (2026-07-09, updated as waves land)
- **Wave 1** — Handle wired into `AppState`/boot (`open_tenant_handle`); **10
  serve.rs request-handler write-forks** migrated onto `state.db`. Residual
  34 → 24. Census 285 → 271.
- **Wave 2a** — **2 remote-queue daemons** (`email_outbox_poll_daemon`,
  `catalogue_push`) migrated **100%** onto the Handle. Residual 24 → 22. Census
  271 → 267.
- **RESHAPED SCOPE (critical, from the interleaved-fold probe):** it is NOT
  enough to migrate a subsystem's audit write-fork. With the runtime checkpoint
  disabled in H3, the Handle holds a persistent WAL-resident connection; any
  SEPARATE `Connection::open`/`Ledger::open` on the live file — even a read —
  close-folds the Handle's pending audit WAL, and a fresh reader then sees only a
  SUBSET of the audit rows. So a subsystem that touches the Handle must route
  **100% of its DB access (reads AND writes)** through it. Request handlers whose
  whole flow runs on one guard are safe (wave 1); long-running **daemons** and
  handlers that interleave a separate business write with a Handle audit must be
  fully migrated together. This is the real content of "migrate 100% of runtime
  openers, atomic".
- **Remaining (22 write-forks; each a FULL-subsystem migration):**
  `email_relay_daemon`, `quote_pdf_rerender_daemon` (both reverted to coherent
  all-reopen, await full migration), `ap_sync`, `avl_vendors`, `email_invoice`,
  `incoming_invoices` (×2), `material_inventory`, `mes_manager`,
  `quote_calibration`, `quoting_machines` (12 caller handlers on the shared
  `append_machine_event`), `restore_from_nav_outgoing` (×2),
  `quote_pricing_pipeline` (×9). `bash tools/cut_gate_write_fork.sh` prints the
  live list. Each removes openers → re-cut the census baselines in the same
  commit; the write-fork gate flips to `ENFORCE_WRITE_FORK=1` when it hits zero.

**Landed on this branch (all genuinely green — ci both arms + cut-gate):**
- `crates/aberp-db`: the shared `Handle`/`WriteGuard`/`read()` + pure D2
  `debounce` module + the checkpoint-disabled e2e suite + poison-recovery (Bug 5)
  + F-C/F-A pre-fixes.
- `EventKind::DbAutoRecovered` (full F12 ritual).
- F-E: the cross-process whole-DB single-writer flock (`db_writer_lock.rs`) +
  serve boot acquisition + the cross-process refusal e2e, **and** all 14
  DB-mutating CLI one-shots now `acquire_or_refuse` the whole-DB writer lock
  before opening the tenant DB (COMPLETE — closes the CLI-vs-serve two-writer
  class that forced a hand-stop of prod on 2026-07-09).
- The zero-residual write-fork gate machinery (scanner + allow-list + the
  informational CI tracker) enumerating the exact remainder.
- The EXACT toolchain pin (`channel = "1.97.0"`) so the gates are reproducible
  for the effort.

**Remainder (the ATOMIC step — deliberately NOT landed piecemeal):**
Wiring the `Handle` into `AppState`/every daemon `Deps` and migrating **all**
in-serve write-forks onto it is a SINGLE atomic change, per this lane's binding
rule ("100% migration, ATOMIC, gated to ZERO — never partial; Defense forked 4×
precisely because openers were migrated piecemeal"). It is **not** landable in
shrinking waves because:
  1. Each migrated opener removal diverges the frozen opener-census P2
     fingerprint set — the census baselines must be re-cut in the SAME change.
  2. With the runtime checkpoint disabled in H3 (H4's step), Handle writes are
     WAL-resident; the ~24 serve-route **test harnesses** open the Handle eagerly
     in `build_state` (which also eager-creates the DB, tripping the
     "no-DB-write-before-gate" route tests) and then seed/verify through SEPARATE
     connections, so a partial migration leaves those harnesses reading a stale
     instance. Routing the harnesses coherently is part of the same atomic change.

**Proven fix strategy for (2) (empirically verified 2026-07-09).** Two DuckDB
coherence probes settle the test-harness approach — it is a *reorder*, not a
rewrite:
  - A separate write **before** the Handle opens IS visible to `Handle::read()`
    (the pre-open write folds to main on the seed-conn close; the Handle opens
    fresh over it). ⇒ the fix is **seed-before-`build_state`** so migrated
    handlers (reading via `state.db`) see the seed.
  - A fresh separate `Connection::open` **sees the Handle's WAL-only committed
    write while the Handle is still held** (fresh opens replay the WAL). ⇒ test
    **verifies via fresh opens keep working** with no change.
So the atomic change is: (a) a serve `open_tenant_handle` helper +
`ensure_all_tenant_schemas` (so a test/boot Handle owns schema creation, editions
form); (b) `AppState.db` + boot construction + daemon `Deps.db` threading;
(c) migrate all 34 in-serve write-forks to `db.write()`/`db.read()`; (d) per
affected route test, move seeding **before** `build_state` and adjust the handful
of "no-DB-leak" assertions (the Handle now legitimately creates the DB);
(e) re-cut the `adr0098_prod_*` census baselines; (f) flip the write-fork gate to
`ENFORCE_WRITE_FORK=1` + add the F-A pragma-presence gate check. The informational
tracker prints the exact fork list (`bash tools/cut_gate_write_fork.sh` — 34
in-serve forks). That whole set landing green together is the H3 acceptance cut.

---

## Addendum — post-freeze advisory documented-ignore (2026-07-06, owner Ervin)

**Status:** Accepted (config-only; supersedes nothing).
**Decision (verbatim in intent):** the pre-existing, post-freeze `cargo-deny`
security-advisory drift on this branch is handled by **reachability-assessed
documented-ignore (config only)** — NOT dependency bumps. Dependency bumps remain
a plan §2 **NON-GOAL**; the real dependency remediation is **deferred to a future
PROD re-harden**. This turns `ci.yml` fully green for the first time on this lane.

### Why the drift exists
The `PROD_v2.27.76` tree was frozen on 2026-07-05, but `cargo-deny` / `cargo-audit`
fetch the **latest RustSec advisory DB at run time**. Advisories published *after*
the freeze therefore surface against an unchanged, pinned `Cargo.lock`. The set spans
two scan surfaces. `cargo deny check` (feature-resolved graph) failed on **four**
in the red run on `f477f47` (GitHub Actions run 28798583891 — `advisories FAILED,
bans ok, licenses ok, sources ok`): RUSTSEC-2026-0187/-0190/-0194/-0195. Because
that step failed first, `cargo audit` never ran in the old red run; once the deny
ignores made `cargo deny check` green, `cargo audit` (raw-lockfile scan of all 729
lock entries) ran and surfaced a **fifth**, RUSTSEC-2026-0185 (quinn-proto),
which cargo-deny does **not** report because quinn-proto is not in ABERP's resolved
feature graph (see its entry below). All five are documented-ignored here.
(`RUSTSEC-2024-0429`, listed in the original planning note, was already covered by
the pre-existing GTK3 ignore block and is **not** part of the current failing set.)

### Scope of change — CONFIG ONLY
No `Cargo.toml` / `Cargo.lock` edit, no dependency added/removed/bumped, no
application code touched. Three advisory-ignore surfaces are updated in lockstep,
each with a specific per-advisory justification (no blanket ignore):
`deny.toml [advisories].ignore`, `audit.toml [advisories].ignore`, and the
`ci.yml` `cargo audit --ignore …` inline list (the audit step passes ignores
inline because audit.toml auto-discovery is unreliable in CI, per S303).

### Per-advisory reachability justification
- **RUSTSEC-2026-0187** — lopdf 0.34.0, stack overflow via deeply nested PDF
  objects. **Unreachable at runtime.** Production code only *generates* PDFs
  (`crates/invoice-pdf`, `crates/aberp-quote-pdf` render ABERP's own invoice /
  quote data). The only PDF-*parse* callsites (`lopdf::Document::load_mem`,
  `pdf_extract::extract_text_from_mem`) are inside `#[cfg(test)]`
  (`crates/aberp-quote-pdf/src/lib.rs:812` `mod tests`) round-tripping
  self-generated PDFs to assert render fidelity. No untrusted PDF is ever parsed.
- **RUSTSEC-2026-0190** — anyhow 1.0.102, unsoundness in `Error::downcast_mut()`.
  **Unreachable.** `downcast_mut` is never called anywhere in ABERP source
  (grep-verified empty); anyhow is used purely as an error-propagation type, so
  the vulnerable API is never exercised.
- **RUSTSEC-2026-0194** — quick-xml 0.36.2, unbounded namespace-declaration
  allocation in `NsReader` (memory-exhaustion DoS). **Low reachability.**
  quick-xml parses only responses from known, authenticated endpoints — NAV
  (Hungarian tax authority) SOAP over pinned TLS, MNB (Hungarian National Bank)
  FX-rate SOAP over TLS, LAN MTConnect agent telemetry — plus ABERP's own
  catalogue / XSD XML. No attacker-controlled internet input. Worst case is a DoS
  (hang / OOM) affecting only the single local operator on a loopback-only,
  single-tenant desktop; there is no multi-tenant blast radius.
- **RUSTSEC-2026-0195** — quick-xml 0.36.2, quadratic run time when checking a
  start tag for duplicate attribute names (DoS). **Same reachability envelope as
  -0194:** known-endpoint / self-authored XML only, single-tenant loopback
  desktop, DoS-only blast radius.
- **RUSTSEC-2026-0185** — quinn-proto 0.11.14, remote memory exhaustion via
  unbounded out-of-order QUIC stream reassembly (cargo-audit-only surface).
  **Not compiled / unreachable.** quinn-proto enters `Cargo.lock` solely through
  reqwest's optional `http3`/QUIC path, which ABERP does not enable — the
  workspace pins `reqwest = { default-features = false, features = ["rustls",
  "gzip", "stream", "json"] }` (no `http3`). `cargo tree -i quinn-proto` resolves
  to nothing in the active graph, so cargo-deny's feature-resolved scan never
  flags it; only cargo-audit's raw-lockfile scan sees the phantom entry. The
  vulnerable QUIC reassembly code is never built into the binary, and ABERP makes
  only outbound HTTPS client requests to known NAV/MNB endpoints — it never runs a
  QUIC listener accepting inbound streams. (The lockstep `deny.toml` entry is a
  placeholder for the surface cargo-deny doesn't currently reach.)

### Re-harden hook
When the next PROD re-harden lands, revisit all five: bump lopdf / anyhow /
quick-xml (and, if the http3 path is ever enabled, quinn via reqwest) to fixed
releases and delete the corresponding ignore entries from `deny.toml`,
`audit.toml`, and `ci.yml` together. Until then the ignores are the
owner-approved, reachability-justified posture and `ci.yml` is genuinely green.

---

## Addendum 2 — owner-approved surgical NAV recovery fix (2026-07-09, owner Ervin)

**Status:** Accepted (owner-authorised deviation from the plan's §2 non-goal —
"the plan bans drive-by fixes"). Sequenced deliberately **ahead of H3** at the
owner's explicit direction.

**Trigger.** A live PROD incident on 2026-07-09 (operator restarted PROD after
the incident) exposed **three real defects** in the NAV submission-recovery CLI
(`aberp retry-submission` / `drain-pending-retries` / `recover-from-nav` /
`mark-abandoned`). All three were found by adversarial review and cited to the
frozen tree `PROD_v2.27.76`; all three are still present there. Real NAV / real
tax → the owner ordered the fix now rather than folding it into H3. This is a
**deliberate, owner-authorised** deviation from §2 (which otherwise forbids
drive-by fixes in this lane). No PROD runtime was touched; all work is on
`prod-durability-adr0099`, pushed to origin, genuinely green on GitHub Actions.

### Defect 1 (critical) — the Layer-2 duplicate guard never worked

`derive_nav_invoice_number` SYNTHESISED the NAV-facing number as
`format!("{}/{:05}", series.code, invoice.sequence_number)`
(`drain_pending_retries.rs`, `retry_submission.rs`, `recover_from_nav.rs`).
`series.code` is the legacy literal `INV-default` (`numbering.rs`) — the
pre-PR-89 hardcoded shape alive at a ninth emit site PR-89 never migrated. The
**real** invoice number lives ONLY in the on-disk `<InvoiceData>` XML. That
synthesised string went verbatim into `<invoiceNumberQuery><invoiceNumber>`
(`query_invoice_check` → `soap`), so NAV was asked about a number it has **never
seen** ⇒ `queryInvoiceCheck` always returns `Absent` ⇒ `Layer2Decision::SkipRePost`
was **unreachable** (the duplicate guard never fired). `recover-from-nav` used
the synthesised number for `queryInvoiceData` AND for its derived-vs-recorded
drift check (both wrong the same way, so the check silently agreed).
`mark-abandoned`'s F49 guard reads the recorded `InvoiceCheckPerformed` outcome,
which was therefore always `absent` — it would have let an operator abandon an
invoice NAV actually holds.

**Fix.** Replace every Layer-2 / NAV-query use with the existing correct helper
**`nav_xml::read_invoice_number_from_xml`** (`nav_xml.rs`) — the byte-exact
`<invoiceNumber>` NAV holds on file, written at issuance and never re-rewritten;
the **S184** discipline already used by `issue_storno`, `issue_modification`, and
`observe_receiver_confirmation` (S184's own doc warns that re-deriving "silently
drifts the reference"). `recover-from-nav` resolves the base XML path via the
canonical ledger-walk `issue_storno::find_base_nav_xml_path_for_chain` within its
existing precondition-ledger scope; its drift check is now genuinely load-bearing
(on-disk XML vs the recorded check number). The three now-dead
`derive_nav_invoice_number` copies were deleted. **`mark-abandoned` needs no code
change** — once the query sites record the real number, its F49 guard reads the
correct `exists` outcome and blocks abandonment as intended. (The S392 issuance
pre-flight `nav_number_probe` legitimately renders a candidate number via the
template — there is no on-disk XML at pre-issuance time — and is NOT a defect
site; left untouched.)

### Defect 2 — a stuck STORNO would be re-POSTed as a CREATE

`prepare_for_attempt_audit` is forked 4×. `submit_invoice` and
`drain_submission_queue` take a ledger-derived `operation`; `retry_submission`
and `drain_pending_retries` **hardcoded `InvoiceOperation::Create`** (`PendingRetry`
had no `operation` field). Because NAV v3.0 STORNO / MODIFY bodies are
byte-identical to CREATE (the operation is not sniffable from the body), a stuck
STORNO retried through either path would be re-POSTed to NAV as a CREATE.

**Fix.** Added `PendingRetry::operation`, stamped from the ledger chain-link
entries via `submission_queue::operation_for_invoice` at classify time — the
exact mirror of `PendingInvoice::operation`. `retry_submission` derives the
operation from the ledger it already reads in `resolve_stuck_or_loud_fail` (no
new opener). Threaded through both `prepare_for_attempt_audit` sites.

### Defect 3 — the drain could fork the audit ledger

`drain_pending_retries` called `Ledger::open` 4× per invoice (TX0 mirror-sync,
TX1 mirror-sync, both TX2 arms) and `DuckDbBillingStore::open` once (inside the
Defect-1 synthesiser) — the TX0 site opened a **second DuckDB instance while
`conn` was still alive**, and every re-open re-runs DuckDB 1.5.x's
LoadCheckpoint/ReadIndex replay, the duckdb#23046 / S332 checkpoint-ART
corruption trigger (`storage/mod.rs` names `Ledger::open` as the trigger;
`submit_invoice` was migrated OFF it under S388). An active corruption hazard on
the very machine the operator is using today.

**Fix.** Migrate the drain's per-invoice openers onto the live handle:
- TX0 (`perform_layer_2_check`) and TX1 mirror-syncs now call the free
  `audit_ledger::sync_mirror(&conn, …)` on the already-open, just-committed
  `conn` — no second instance. `perform_layer_2_check` drops its now-unused
  `tenant` / `binary_hash` params.
- Both TX2 arms route through a new `verify_chain_and_sync_reusing_conn` (direct
  mirror of `submit_invoice`'s S388 helper) using `Ledger::from_connection` —
  the file is never re-opened.
- Sequenced so at most **one** live DuckDB handle exists at any point (`conn#1`
  for load/Layer-2/TX1, dropped before the wire send; `conn#2` for TX2,
  consumed by the reuse helper).

This is one slice of H3's wider opener migration, done now because it is an
active hazard; the rest of H3 (the atomic `AppState`/daemon `Handle` wiring and
the in-serve write-fork gate flip) remains the ATOMIC remainder per the "Landed
state / remaining migration" section. Per that section's rule, each opener
removal is **re-cut into the frozen census in the same change**: the
`tools/adr0098_prod_frozen_residuals.txt` counts and
`tools/adr0098_prod_opener_fingerprints.txt` fingerprint set ratchet DOWN −7
(292→285 across 43 files; drain 8→3, recover 4→3, retry 9→8) — the three deleted
billing-store opens (Defect 1) plus the four reused drain `Ledger::open` sites
(Defect 3). `from_connection` and the free `sync_mirror` are census-excluded
seams, so no opener was added. `cut-gate.yml` (P1 count-freeze + P2
fingerprint-freeze + negative-probe teeth) stays green.

### Tests (behavioural; RED-before / GREEN-after)

- **Equality pin (Defect 1)** — `tests/adr0099_addendum2_equality_pin.rs`: the
  `<invoiceNumber>` inside a real `queryInvoiceCheck` request EQUALS the
  `<invoiceNumber>` read from the on-disk `<InvoiceData>` XML, and is NOT the
  synthesised `INV-default/{seq:05}` form. Exercises the exact
  `read_invoice_number_from_xml` → `build_request` seam all three modules now
  share (fixture number chosen to differ from the synthesis for the same seq).
- **Operation pin (Defect 2)** — `submission_queue` unit tests: a stuck STORNO
  (Draft + Attempt, no Response/Abandon) classifies as a `PendingRetry` carrying
  `operation = Storno`; a plain invoice carries `Create`. RED-before (the field
  did not exist), GREEN-after.
- **Opener pin (Defect 3)** — `drain_pending_retries` unit test: the new
  `verify_chain_and_sync_reusing_conn` verifies a heavy (N=64) on-disk ledger and
  syncs the mirror through the REUSED connection, no re-open. Mirror of
  `submit_invoice`'s S388 helper test.

### Scope / integrity

Source touched: `drain_pending_retries.rs`, `retry_submission.rs`,
`recover_from_nav.rs`, `submission_queue.rs` (+ two module-header doc
refreshes), the equality-pin integration test, and `apps/aberp/Cargo.toml`
(dev-dep on `aberp-nav-transport` with the `test-support` feature for
`NavCredentials::from_parts`, same pattern as audit-ledger's `Actor::test_only`).
Census baselines re-cut as above. `cargo fmt --check`, clippy `-D warnings`, and
the full `aberp` test suite are green; no `#[ignore]`, no `continue-on-error`,
zero durability skips. The `PROD_v2.27.76` tree hash and `main` are unchanged;
nothing merged to `main`, no cut. H3 is sequenced off the resulting HEAD.

### Toolchain drift — clippy 1.97 (separate, flagged commit)

**Not part of the three defects.** While verifying `ci.yml` green, the shared
`ci` job was found already RED on the H3 base (`7a169f3`) at the **Clippy** step
— NOT a regression of this Addendum. Root cause is the same class as Addendum 1's
`cargo-deny` advisory drift: `rust-toolchain.toml` pins the **channel** (`stable`,
per ADR-0001 / ADR-0021), and CI installs stable **fresh at run time**. Stable
**1.97.0** shipped 2026-07-07 (two days before this incident) and tightened
`clippy::useless_borrows_in_formatting` + `clippy::question_mark`, so
`cargo clippy --workspace -D warnings` now flags code the pinned-local 1.95.0
did not. Reproduced locally under 1.97.0: **5 sites**, all mechanical and
semantically no-ops —
`aberp-quote-intake/src/transport.rs`, `apps/aberp/src/catalogue_push.rs`,
`apps/aberp/src/quote_pricing_pipeline.rs`, `apps/aberp/src/serve.rs`
(all `format!("… {}", &*x)` → `*x`, a redundant `&` removal on a
`Zeroizing<String>` bearer token), and `apps/aberp/src/partners.rs`
(`match s { Some(v) => …, None => return None }` → `s?`). Fixed in a **separate,
clearly-labelled commit** so the three-defect change stays reviewable in
isolation; `cargo-deny` / `cargo-audit` were re-run against the live advisory DB
and are clean (no new supply-chain drift), so this clippy sweep is the sole
delta needed to bring `ci.yml` fully green. The sweep touches unrelated crates
only to satisfy the newer linter; it is flagged here for the owner exactly as
Addendum 1 flagged the supply-chain-drift decision. (A future re-harden may
instead pin an exact toolchain version on this branch — the escape hatch
`rust-toolchain.toml`'s own comment names — but that is a policy call left to
the owner; the mechanical sweep changes no documented policy.)

#### Decision — EXACT toolchain pin for the hardening effort (2026-07-09, owner Ervin)

Taken. The channel-only pin has now cost CI red **twice** mid-effort with no
source change of ours (the Addendum-1 advisory drift, then this clippy-1.97
drift). A floating compiler is incompatible with a multi-week **gated** change
whose entire value is that a RED gate means a REAL regression — reproducibility
of the gates is itself a hardening property. So `rust-toolchain.toml` is changed
from `channel = "stable"` to `channel = "1.97.0"` — the EXACT stable version
currently green on this branch (`rustc 1.97.0 (2d8144b78 2026-07-07)`) — in its
own clearly-labelled commit. A contributor (and CI) now builds the branch with
the same compiler; a gate going red from here is our code, not a toolchain bump.
This is scoped to the hardening lane; **un-pinning back to floating `stable` is a
separate, deliberate decision at H7** (end of effort), not to be reverted
piecemeal. `rust-version` (MSRV floor) and `Cargo.lock` (dependency pin) are
unchanged; this only removes the compiler-version degree of freedom.
