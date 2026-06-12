# S382 — Concurrency, Locking & DuckDB-Isolation Hardening Audit

**Session:** S382 (read-only research, doc-only PR)
**Date:** 2026-06-12
**Baseline audited:** `efae798` (`origin/session-375` = `origin/main` @ `59b648a` + PR #5 S375). All `file:line` citations below are at this commit. S375 is included deliberately — it is the imminent code state and the audit brief explicitly covers `Ledger::from_connection`.
**Locked DuckDB:** `duckdb` crate **1.10502.0** (Cargo.lock), which per the duckdb-rs `1.<MM><mm>.<patch>` scheme bundles **libduckdb 1.5.2**. (One internal review misread this as "DuckDB 1.10.5" — it is not; 1.10502 ⇒ 1.5.2.)

## Why now

Tonight's incident cluster is one pattern wearing four costumes: **DuckDB 1.5.x metadata/ART fragility meeting our open/reopen and commit-then-side-effect seams.**

- 2026-06-11: prod DuckDB ART/metadata corruption, 5h hand-surgery.
- 2026-06-12: DEV `Failed to load metadata pointer (id 46, idx 0, ptr 46)` on storno post-commit `Ledger::open` re-open (fixed by S375 `from_connection`).
- 2026-06-12: storno row committed to DB but XML missing on disk (fixed by S375 render-before-commit).
- TEST-ABERP/2026/0047 double-submitted to NAV ("already submitted" rejection).

S375 fixed two instances of the pattern. This audit maps **every remaining instance** plus the future-risk surface as load and tenancy grow.

---

## Executive summary

The in-process story is in good shape: every HTTP handler does DB work inside `spawn_blocking` with open-per-request connections (no connection ever crosses an `.await`), audit appends are serialized by a process-wide mutex acquired *before* `BEGIN TRANSACTION`, billing idempotency is a real `UNIQUE` constraint, and invoice-number allocation is gap-free per `(series, fiscal_year)` inside one transaction.

The exposures cluster in four places:

1. **One un-migrated sibling of the exact bug S375 just fixed** — `issue_modification.rs` still does post-commit `Ledger::open` (the #23046 crash trigger) *and* renders + writes XML after commit (the split-state defect). 🔴
2. **The commit-then-network seam** — NAV submission has no Layer-2 idempotency (named-deferred F44); the 0047 double-submission is this gap, not a fluke. Email relay has the same shape (SMTP-send then mark-sent). 🔴/🟡
3. **The process boundary** — every protection we built (AUDIT_APPEND_LOCK, INGEST_SERIALIZER, rerender queue) is process-local. Nothing stops a second `aberp serve` or a CLI subcommand racing the server; the only barrier is DuckDB's own file lock, whose failure mode under 1.5.x is exactly the corruption family we keep hitting. 🔴 (future-leaning)
4. **Non-atomic file I/O** — `nav_xml::write_to_path` is `File::create` + `write_all`: no tmp+rename, no fsync. PDF artifacts have a producer/consumer partial-read window. 🟡

Plus: backups are a manual pre-upgrade script with no automation and no verified restore; concurrency tests cover only in-process threads; libduckdb 1.5.3 shipped 2026-05-20 with ART fixes while #23046 remains open upstream.

---

## Table 1 — Lock / concurrency surface × behavior × failure mode × severity

| Surface | Where | Current behavior | Failure mode | Severity |
|---|---|---|---|---|
| `AUDIT_APPEND_LOCK` | `crates/audit-ledger/src/storage/mod.rs:92` | Process-wide `std::sync::Mutex<()>`; acquired **before** `conn.transaction()` in `Ledger::append` (mod.rs:221–227) and before `Connection::open` in `append_reopen` (mod.rs:450–458). Never held across `.await` (all call sites sync). Poison recovered via `into_inner()`. | In-process: none known. Cross-process: zero protection (doc comment at mod.rs:88–91 names this; backstop = hash-chain detection, prevention deferred to "single serialized audit-writer actor", S335 §3.4). Also serializes appends across *different tenants'* files unnecessarily. | 🟡 today / 🔴 multi-process |
| `AUDIT_MIGRATION_LOCK` | `storage/mod.rs:99` | Separate mutex for the one-time UNIQUE-ART drop migration; runs inside `ensure_schema`, never nests with the append lock (comment mod.rs:94–98). | None found. Deliberate anti-deadlock split. | 🟢 |
| `INGEST_SERIALIZER` | `apps/aberp/src/incoming_invoices.rs:422` (rationale :54–77) | Process-wide mutex over the whole AP-ingest critical section (find → insert → audit → verify → mirror). Exists because S186 proved DuckDB `UNIQUE` does **not** fire across two `Connection::open` handles in one process. | Same process-local ceiling as the append lock. | 🟢 in-process |
| `boot_state` | `serve.rs:2649` `Arc<RwLock<ServeBootState>>` | ~20 read sites, 3–4 write sites (setup routes). Guards dropped before awaits in spot checks; poison handled explicitly at every site (e.g. serve.rs:3442, 3517). | No explicit drop-scoping convention — a future edit could hold a guard across `.await`. Poison ⇒ 503s until restart. | 🟡 (hygiene) |
| `nav_poll_semaphore` | `serve.rs:2664`, cap 50 (`serve.rs:2826`); acquired `poll_ack.rs:929–931` | One permit per NAV-poll daemon, held for daemon lifetime (intentional concurrency cap, FIFO-fair). | None — by design. Cap is per-process only. | 🟢 |
| `AdapterManager.mutation_lock` | `mes_manager.rs:155` (`tokio::sync::Mutex`) | Fixed acquisition order mutation_lock → `adapter_registry.write()`; no opposite-order path found. | None found. | 🟢 |
| Rerender queue | `quote_pdf_rerender_queue.rs:43` `Mutex<HashSet<String>>` | Microsecond holds; idempotent enqueue; boot recovery from ledger scan (`quote_pdf_rerender_daemon.rs:153–215`). | In-memory loss on crash covered by boot recovery. | 🟢 |
| Status mutexes (email outbox `email_outbox_poll_daemon.rs:264–271`, catalogue `catalogue_push.rs:124–126`, pipeline python `quote_pricing_pipeline.rs:2580`, secrets `secrets_cache.rs:64`, storefront cred `storefront_credential.rs:77`) | various | All microsecond field-update holds, no I/O inside, no awaits inside. | None found. | 🟢 |
| ERP DB connections | AppState holds `db_path: Arc<PathBuf>` only (`serve.rs:2633–2732`); every handler opens inside `spawn_blocking` (e.g. serve.rs:10015, 10069); `block_in_place`: 0 hits | Open-per-request, dropped at task end. No pool, no shared handle, nothing crosses `.await`. | Many short-lived `Connection::open` calls on one file = many Database instances; each open is a walk through the 1.5.x checkpoint-load code that #23046 lives in. Works today; it is also the riskiest *pattern* under the known bug family. | 🟡 |
| Daemon-vs-operator races | NAV poll writes audit rows only, never UPDATEs billing rows (`poll_ack.rs:707–733`; `submit_invoice.rs:70–73` — submission state lives in the ledger per A6) | Storno creates a *new* chain child; poll appends evidence. No shared UPDATE column ⇒ no lost-update race found between storno click and poll daemon. | Re-spawned daemons after crash write duplicate `InvoiceAckStatus` rows (unbounded, benign — "latest wins" rendering). | 🟢 race / 🟡 duplication |
| Email relay send-vs-mark | `email_relay_daemon.rs:121–209`: claim → SMTP send → `mark_sent` | Crash between SMTP success and `mark_sent` ⇒ row still claimable ⇒ **duplicate email**. No per-row idempotency key. | Duplicate customer-facing email. | 🟡 |
| Two processes, one tenant dir | No pidfile/flock/single-instance guard anywhere in `serve.rs` boot (`serve.rs:408–721`); default port is kernel-assigned (`serve.rs:1041–1074`) so port binding doesn't guard | Only barrier = DuckDB's own file lock (second RW open errors). Mirror file appends (`mirror.rs:284–288`) and issued-XML writes have **no** cross-process guard. | Dev-mistake double-serve or CLI-during-serve: best case "database is locked" errors mid-request; worst case (1.5.x) the corruption family. Mirror interleaving. | 🔴 (future / dev-mistake) |

## Table 2 — DuckDB-specific risks × current mitigation × residual exposure

| Risk | Current mitigation | Residual exposure |
|---|---|---|
| duckdb#23046 ART/checkpoint corruption (**still OPEN upstream** as of 2026-06; 1.5.1 shipped two ART fixes, PR #21270/#21427, without closing it) | S341 dropped `UNIQUE(seq)/(id)` ART indexes with verified rebuild (`storage/mod.rs:330–390`); S375 killed the post-commit re-open in invoice+storno (`issue_invoice.rs:822`, `issue_storno.rs:550`) | `issue_modification.rs:364` still re-opens post-commit. Every `spawn_blocking` handler open replays checkpoint-load. We're on 1.5.2; **1.5.3 exists (2026-05-20)**. |
| Re-open while original Database instance alive ⇒ `Failed to load metadata pointer` | `Ledger::from_connection` (`storage/mod.rs:178`) reuses the post-commit handle, skips DDL/checkpoint replay | Modification path unfixed; any future code that "just opens the ledger" re-creates the trigger. No lint/grep gate prevents new `Ledger::open`-after-commit sites. |
| Two in-process Database instances fork the audit chain (S186/S335-proven; `apps/aberp/tests/s335_email_outbox_audit_write_coherence.rs:125–210` pins the hazard) | Reopen-per-write **under** `AUDIT_APPEND_LOCK` (`storage/mod.rs:450–458`); persistent-connection refusal (S335) | Protection is process-local. Cross-process fork is detect-only (`verify_chain`), after rows persisted. |
| Dropped UNIQUE ⇒ no DB-level seq uniqueness | App-layer: seq assigned only inside `append_in_tx` as `next_seq(head)` (`chain/compute.rs:37–42`) under the append lock; `verify_chain` detects OutOfOrder/ChainBroken/TamperedAt (`chain/verify.rs:35–55`) | Cross-process duplicate seq is possible in principle and detected only post-facto. |
| Checkpoint-under-write / crash-mid-checkpoint | None explicit — no `CHECKPOINT`/`PRAGMA` calls anywhere in production code (grep: 0 hits); DuckDB defaults | Default 16 MB WAL threshold checkpoints at uncontrolled moments. Crash mid-checkpoint with 1.5.x metadata bugs = the 2026-06-11 prod incident shape. No snapshot existed to restore. |
| `verify_chain` is O(n) full scan (`SELECT … ORDER BY seq`, `schema.rs:68–73`) run **on every issuance** post-commit (`issue_invoice.rs:823–825`, `issue_storno.rs:552`) and at boot | Fine at current volume | Issuance latency grows linearly with ledger size forever. At 10k–100k entries this becomes a per-invoice tax, all while holding the issuance code path. |
| Storage-format one-way upgrade | n/a | Upgrading to 1.5.3 is backward-compatible reads; downgrade is not guaranteed (EXPORT/IMPORT escape hatch). Bump must be snapshot-gated. |
| Mirror file durability | Append-mode writes + `sync_all` when appended (`mirror.rs:284–288, 385–387`); partial-trailing-line detection (`mirror.rs:194–211`); boot reconciliation Extended/Truncated/Rebuilt (`mirror.rs:474–565`) | Good design. Cross-process interleaved appends remain unguarded (same process-boundary theme). |

---

## Detailed findings

### 🔴 F1 — `issue_modification.rs` missed both S375 fixes

The modification path is a frozen copy of the pre-S375 storno bug, both halves:

- **Post-commit re-open:** `issue_modification.rs:364` calls `Ledger::open(db, …)` after `run_single_tx` commits — the exact #23046 trigger S375 eliminated in invoice (`issue_invoice.rs:822`) and storno (`issue_storno.rs:550`). Its `run_single_tx` (`issue_modification.rs:728–918`) doesn't return the `Connection`, so there is nothing to hand to `from_connection` without the same refactor.
- **Post-commit render+write:** render at `issue_modification.rs:441`, `nav_xml::write_to_path` at `:463` — both **after** commit. A render/validate/write failure leaves a committed modification row with no XML on disk: the precise split-state defect S375 fixed for storno.

Reproduction: issue a modification whose XML fails XSD validation (or kill the process between commit and `:463`) → committed invoice chain entry, no NAV XML, and on the old-crash variant a metadata-pointer abort at `:364`. *(Already flagged as background task `task_8de31599`; this audit confirms scope: same mechanical fix as S375, +tests.)* **Fix scope: 1 session.**

### 🔴 F2 — NAV double-submission (F44) is an open design gap, not a race fluke

`submit_invoice.rs:74–77` states it plainly: no `queryInvoiceCheck` consultation; Layer-2 idempotency per ADR-0009 §5 is named-deferred (F44). `:193` even warns the operator that retry "may produce a duplicate submission to NAV". There is no `status='submitting'` claim, no billing-row mutation at all (submission state lives only in the ledger, A6). So: append `InvoiceSubmissionAttempt`, then POST — crash/retry between them, or two concurrent submitters, both POST. TEST-ABERP/2026/0047 is this window realized. The audit found **no repo writeup of the 0047 incident** (grep of `docs/` came up empty) — worth capturing while memories are fresh.

Fix scope: 1 session — pre-submit `queryInvoiceCheck` (or transactionId lookup in own ledger) + a claim entry whose presence gates re-submission, per ADR-0032's open question. **This is fiscal-facing; highest business severity in this report.**

### 🔴 F3 — The process boundary is unguarded

- No pidfile/flock/single-instance check in boot (`serve.rs:408–721`). Default port `0` (`serve.rs:189–200, 1041–1074`) means port binding only guards when an env pins it.
- `AUDIT_APPEND_LOCK` / `INGEST_SERIALIZER` are statics — invisible to a second process. The append-lock doc comment (`storage/mod.rs:88–91`) openly defers cross-process to S335 §3.4's "single serialized audit-writer actor."
- Mirror appends (`mirror.rs:284–288`) and issued-XML writes have no cross-process exclusion.
- DuckDB's file lock rejects a second RW opener — which is real protection, but its *error path* lands mid-request as 500s, and 1.5.x's behavior when locks die under SIGKILL is exactly where the corruption family lives. No stale-`.lck` detection/cleanup code exists.

Scenarios: operator runs `./run/run_prod.sh` twice; a CLI subcommand (`aberp issue-invoice`, `poll-ack` — 16 subcommands exist) runs while serve is up; future multi-replica. Fix scope: 1 session for a tenant-dir flock + friendly "already running (pid N)" boot error; the writer-actor consolidation is a larger follow-on.

### 🟡 F4 — `nav_xml::write_to_path` is non-atomic, unsynced

`nav_xml.rs:1851–1857`: `File::create` (truncate) + `write_all`. No tmp+rename, no fsync, no `create_new`. Inside the S375 tx-closure (invoice `:757`-area, storno `:499`-area) a *failure* rolls the tx back — good — but a *machine* crash after commit can still lose the page-cache-only XML while the DB row survives fsync'd WAL: split-state returns through the kernel. Concurrent writers to one path would interleave. Same gap class for PDF artifacts: rerender daemon overwrites `priced.pdf` in place while `serve.rs:17169` `std::fs::read`s it → partial-read serving a torn PDF to a customer. Fix scope: one `write_atomic(path, bytes)` helper (same-dir tmp + `write_all` + `sync_all` + `rename` + dir-fsync) adopted at the 3 XML sites + PDF render. Quick win.

### 🟡 F5 — Email relay duplicate-send window

`email_relay_daemon.rs:121–209`: claim row → SMTP send (`:132`, network) → `mark_sent` (`:139–144`, DB). Crash between the last two re-queues an already-delivered email; rows carry no idempotency key (unlike the storefront outbox path, where claim is storefront-atomic). 5-attempt cap (`:53`) bounds it but each retry is a real duplicate email. Fix scope: half-session — persist a `sending` claim-state + message-id before SMTP, treat `sending` rows on boot as "verify-or-fail-loud", not auto-resend.

### 🟡 F6 — `verify_chain` O(n) on every issuance

Every invoice/storno issuance runs a full-ledger scan + hash walk post-commit (`issue_invoice.rs:823–825`) — correctness gold, linear-growth tax. NAV poll daemon terminal writes also verify+mirror (`poll_ack.rs:1020–1028`). Roadmap: incremental verification (verify from last-verified watermark, full scan at boot + on demand only). Not urgent; will be at 50k+ entries.

### 🟡 F7 — Backup posture: manual, unverified, incomplete

What exists: `tools/snapshot-prod.sh` (tarball of `~/.aberp/<tenant>/` incl. `aberp.duckdb`, `aberp.audit.log`, `seller.toml`, invoices/, ap-artifacts/ + password-protected keychain zip + upgrade-contract file), invoked only by `run/upgrade_prod.sh:236–251` before upgrades. What doesn't: any scheduled backup, any off-machine copy, any verified-restore procedure or drill, any restore *script* (restore is a 4-step manual recipe, `snapshot-prod.sh:268–272`), and **quote-artifacts/ is not in the snapshot scope** (serve-side dirs under `~/.aberp/serve/<tenant>/` need confirming against the tarball root — flagged, not fully verified). The 2026-06-11 5h hand-surgery is the cost of this gap. Fix scope: 1 session for `aberp snapshot` + `aberp restore` subcommands + launchd timer + a restore-drill test in CI against a fixture tenant.

### 🟡 F8 — Concurrency test coverage stops at the thread boundary

Covered (good): 16-thread dense-seq append (`crates/audit-ledger/tests/s341_concurrent_append.rs:28–97`), interleaved reopen-per-write coherence + the pinned persistent-connection fork hazard (`apps/aberp/tests/s335_email_outbox_audit_write_coherence.rs:85–210`), ART-crash regression at volume (`s332_email_outbox_audit_write_no_crash.rs`), concurrent AP ingest. Zero coverage: two-*process* append, CLI-vs-serve, kill-9 mid-append/mid-checkpoint, crash between commit and file write (S375 added from_connection tests, not crash-injection), torn-PDF reads. The s341 test exercises the lock we have, not the failure modes we've actually been bitten by. Fix scope: 1 session for a `std::process::Command`-based two-process harness + kill-injection tests.

### 🟡 F9 — DuckDB version posture

Locked: libduckdb **1.5.2** (crate 1.10502.0). Upstream since: **1.5.3** (2026-05-20, crate 1.10503.x exists); 1.5.1 had shipped two ART fixes (PR #21270, #21427). **#23046 remains open** — no release explicitly closes it, so S341/S375 mitigations stay load-bearing regardless. DuckDB v2.0 (with the cross-process "Quack" protocol) is slated Fall 2026. Bump risk: API delta 1.10502→1.10503 is minimal (the vector-lifetime break landed *in* 1.10502); storage upgrade is effectively one-way (downgrade = EXPORT/IMPORT). Recommendation: snapshot-gated bump to 1.10503.x soon — small, picks up the 1.5.x bugfix tail; re-evaluate at v2.0 for the multi-process story.

### 🟡 F10 — Operator visibility is near-zero

`/health` returns binary-hash/build flags only (`serve.rs:4287–4313`); quote-pipeline and email-outbox have status routes; there is **no** `/metrics`, no admin surface, no `aberp doctor` (0 hits), no on-demand chain-verify endpoint, no stale-lock/daemon-health inspection. During tonight's incidents every diagnosis was log-spelunking. Fix scope: 1 session — `aberp doctor` CLI (verify chain, mirror reconcile dry-run, `.lck`/second-process detection, disk space, duckdb version) + a thin authenticated `/api/admin/status`.

### 🟢 Verified-sound (keep doing this)

- `spawn_blocking` discipline is universal; no connection or std-mutex guard crosses `.await` in production paths (grep `block_in_place`: 0).
- Append lock acquired *before* `BEGIN`, covering read-head→insert→commit (`storage/mod.rs:221–227`); migration lock split prevents re-entrant deadlock by construction.
- Billing idempotency is a real `UNIQUE` (`modules/billing/src/adapters/duckdb_store.rs:85`) → `AllocateOutcome::Replay`; enforced, not advisory.
- Number allocation gap-free per `(series_id, fiscal_year)` PK + read-then-increment in one tx (`duckdb_store.rs:56–62, 491–531`).
- Mirror design (append-only, fsync-on-append, partial-line detection, boot reconciliation) is the most crash-literate file I/O in the codebase — F4's helper should copy its homework.
- All daemons honor `CancellationToken` and run under panic supervisors; no lock-ordering cycles found across the 21 primitives inventoried.

---

## Hardening roadmap (prioritized)

1. **S383 — Modification-path S375 parity** (🔴 F1). Port `run_single_tx`-returns-Connection + `from_connection` + render-before-commit into `issue_modification.rs`; add the same from_connection + render-failure-rollback tests S375 added. Mechanical, highest crash-adjacency.
2. **S384 — NAV Layer-2 idempotency, close F44** (🔴 F2). Pre-submit guard (own-ledger transactionId lookup, then `queryInvoiceCheck` as the authoritative tiebreak), a submission-claim ledger entry gating re-POST, and a written 0047 post-incident note in `docs/findings/`. Fiscal-facing.
3. **S385 — Atomic file-write helper** (🟡 F4). `write_atomic()` with tmp+fsync+rename; adopt in nav_xml (3 sites incl. the new modification path) and PDF render; partial-read fix falls out for free.
4. **S386 — Single-instance guard** (🔴 F3, first slice). flock on `~/.aberp/<tenant>/`, friendly boot refusal, stale-lock detection; CLI subcommands acquire the same lock or refuse while serve runs.
5. **S387 — Snapshot/restore as product** (🟡 F7). `aberp snapshot`/`aberp restore` subcommands wrapping+extending `tools/snapshot-prod.sh` (add quote-artifacts), launchd schedule, restore-drill test.
6. **S388 — DuckDB 1.5.3 bump** (🟡 F9). Crate 1.10502.0 → 1.10503.x, snapshot-gated, full gates + DEV soak before PROD cut. Keep S341/S375 mitigations (upstream #23046 still open).
7. **S389 — Cross-process & crash-injection test harness** (🟡 F8). Two-process append, CLI-vs-serve, kill-9 mid-append and mid-checkpoint, commit-vs-file-write crash windows.
8. **S390 — `aberp doctor` + admin status** (🟡 F10). On-demand chain verify, mirror dry-run, lock/process/disk checks, daemon health rollup.
9. **S391 — Email relay idempotency** (🟡 F5). `sending` claim-state + verify-don't-resend on recovery.
10. **Later — incremental `verify_chain`** (🟡 F6, watermark-based) and **per-tenant append locks / single writer actor** (S335 §3.4) — sequence behind real multi-tenant or multi-process demand; don't build speculatively (CLAUDE.md #2).

## Quick wins (<1 session-day each)

- **`write_atomic` helper + 4 call sites** (F4) — biggest risk-reduction per line of any item here.
- **flock single-instance guard** (F3 slice) — ~30 lines + tests; converts a corruption scenario into a boot error message.
- **Add `quote-artifacts/` + serve-side dirs to `snapshot-prod.sh`** and script the 4-step restore recipe (F7 slice).
- **`aberp doctor` v1** — just chain-verify + mirror-check + second-process detection; even this skeleton would have cut hours off both 2026-06-1x incidents.
- **Grep gate in CI** forbidding new `Ledger::open` within N lines after `tx.commit()`/`run_single_tx` (cheap tripwire against F1 regressions while S383 lands).
- **0047 incident note** in `docs/findings/` while the details are reconstructible.

## Flagged uncertainties (conservative readings taken)

- Whether `~/.aberp/serve/<tenant>/` (issued XML, quote-artifacts) falls inside the snapshot tarball root — the script tars `~/.aberp/<tenant>/`; the serve-side tree appears to live under a *different* root (`serve.rs:6336`). Treated as **not covered** until verified.
- Exact DuckDB behavior of a second same-process `Connection::open`: S186/S335 prove instances coexist without shared constraint enforcement in-process; official docs state cross-process second RW open errors. Both are cited as observed/documented respectively; no claim beyond that.
- Whether libduckdb 1.5.3 changes #23046 behavior — upstream issue open, release notes silent; assume unfixed.
- The 0047 double-submission reconstruction is inference from `submit_invoice.rs:74–77, 193` (F44) — consistent with the incident, but no repo record exists to confirm the trigger.
