# Read-fork audit for the SQLite crossing — ADR-0108 T-20 / T-21 / Q10 / Q11

**Date:** 2026-07-31
**Base:** `adr0108/steps-1-4-foundation` @ `ae925a7` (PR #51 head)
**Scope:** analysis + test harness. No family's storage crosses here; nothing
touches `~/.aberp/**`; the DuckDB file is not opened outside temp dirs.
**Harness:** `crates/aberp-db/tests/adr0108_read_fork.rs` — 2 DuckDB arms
(default build) + 6 SQLite arms (`--features sqlite-engine`), all mutation-verified.

---

## VERDICT

> **The read surface is fork-safe enough to begin Step 5 — and more safely than
> ADR-0108 argues, because the mechanism the migration removes is worse than the
> one it claims to remove.** Two things must change first, neither of them large:
> **R-1** (three `ensure_schema` calls that run DDL through `Handle::read()` and
> become a *second writer* the moment `read()` stops being a `try_clone`) and
> **R-3** (T-21 is unwritable as specified — the nested-read failure mode under
> SQLite depends on an implementation choice in `Handle::read()`'s SQLite arm
> that nobody has made or recorded).
>
> Three further findings are corrections to ADR-0108 rather than blockers: its
> `read()` denominator is **102, not 50** (R-2); its stated WAL snapshot
> semantics are **wrong** and a T-20 written to its wording would pin a false
> claim (R-4); and the read-fork class's actual failure mode is **silent
> permanent data loss on a foreign connection's close**, not the stale read the
> frozen baseline describes (R-5).

**Counts by class**

| Class | Count | Notes |
|---|---:|---|
| (a) goes through the single `Handle` | **238** non-test call sites | 102 `read()` + 136 `write()`; +121 in tests |
| (b) genuine second connection on the live path | **49** opener sites / **33** frozen fns | `tools/adr0099_read_fork_structural_baseline.txt`; 13 live in-serve |
| (b′) second connections the read-fork gate does not classify | **4** fn-sites | in the ADR-0098 census, in neither read-fork list |
| (c) read-only open | **1** implementation, **0** legacy sites | `aberp_db::readonly::open_read_only` |
| ADR-0098 frozen openers (the superset that actually matters — see R-5) | **81** across 20 files | |

**`busy_timeout`: keep 5000 ms, but re-argue what it is for.** The number is
fine; ADR-0108 Q11's *justification* for it does not survive measurement. See §4.

**MUST-fix before Step 5:** R-1, R-3.
**MUST-correct in the ADR before Step 5 (documentation, not code):** R-2, R-4.
**Own PR, not this migration (but it is a live production defect):** R-5.

---

## 1. The census

### 1.1 Method, and why the ADR's number is half the real one

ADR-0108 §1.2 records "84 `state.db.write()` / `.read()` call sites", split
"50 `read()` / 34 `write()`", and §7 Step 3 commits to auditing "**all 50**"
with the denominator stated so completeness is checkable.

The 84 is reproducible exactly:

```bash
grep -rn '\.db\.read()\|\.db\.write()' --include='*.rs' apps crates modules | wc -l   # 84
```

That grep is **single-line** and requires a literal `.db.` prefix. It therefore
misses two large populations:

1. **rustfmt-wrapped chains.** `serve.rs` overwhelmingly formats these as
   `state\n    .db\n    .read()`. The receiver and the method land on different
   lines and the pattern cannot match. This is precisely the failure PR #43
   (D1a) found in the read-fork *scanner* and fixed there — the same defect
   survived in the ADR's census because the census was written as a one-line
   grep.
2. **Handles bound to a local.** `db.read()`, `handle.read()`, `h.read()`,
   `svc.deps.db.read()`, `state_for_task.db.read()` — every one of these is a
   `Handle`, and only the `state.db.` spelling is matched.

Re-measured with the receiver reconstructed across line breaks and the
non-`Handle` `.read()` receivers excluded by name (`state.boot_state`,
`self.inner`, `self.registry`, `self.smtp_password` — all `RwLock`s):

| | `read()` | `write()` | total |
|---|---:|---:|---:|
| non-test | **102** | **136** | **238** |
| test | 48 | 73 | 121 |
| **all** | **150** | **209** | **359** |

**Finding R-2 — the audit's denominator is 102, not 50.** ADR-0108 §13.2 says
"an audit with an unstated denominator is a sample". The denominator was stated;
it was measured with a grep that cannot see two thirds of the tree's formatting.
An audit over 50 of 102 sites is a 49 % sample presented as exhaustive.

*Ruling:* correct §1.2 and §7 Step 3 to 102 / 136 / 238 before Step 5. This audit
covers all 102.

Non-test `read()` sites by file (102):

| File | Sites |
|---|---:|
| `apps/aberp/src/serve.rs` | 74 |
| `apps/aberp/src/restore_from_nav_outgoing.rs` | 4 |
| `apps/aberp/src/incoming_invoices.rs` | 3 |
| `apps/aberp/src/submit_invoice.rs`, `quote_pricing_pipeline.rs`, `poll_ack.rs`, `crates/aberp-quote-intake/src/service.rs` | 2 each |
| `avl_vendors.rs`, `print_invoice.rs`, `ap_sync.rs`, `catalogue_push.rs`, `issue_modification.rs`, `issue_invoice.rs`, `email_outbox_poll_daemon.rs`, `mark_invoice_paid.rs`, `submission_queue.rs`, `snapshot.rs`, `issue_storno.rs` | 1 each |
| `crates/audit-ledger/src/serve_tripwire.rs`, `crates/aberp-db/src/lib.rs` | 1 each (doc-string false positives, excluded) |

### 1.2 Axis (a) — does a `read()` site read inside an open transaction?

**Zero of 102.** No `read()` site calls `.transaction()`, `begin_immediate()`,
or `execute("BEGIN")` on the returned connection, and none passes it to a callee
that does. Every one is either an inline `prepare`/`query_row`, or hands the
connection to a repository function that runs single statements.

This is the frozen-snapshot class, and it is empty. It is also the class R-4
below shows the ADR describes incorrectly — so the class being empty is what
keeps R-4 a documentation defect rather than a live one.

### 1.3 Axis (b) — is a `read()` reached while a `write()` guard is live?

**Zero confirmed, of 136 `write()` sites.** A name-based call-graph reach
(closed under calls, depth 4, guard region conservatively taken to the end of the
enclosing fn) produced five candidates. All five are refuted:

| Candidate | Refutation |
|---|---|
| `issue_invoice.rs:822` → `render_to_bytes` | `render_to_bytes` has exactly one non-doc caller: `print_invoice.rs:103`, the CLI arm. Not reachable from `issue_from_parsed`. |
| `issue_invoice.rs:1121` → `render_to_bytes` | same |
| `issue_modification.rs:322` → `render_to_bytes` | same |
| `issue_storno.rs:359` → `render_to_bytes` | same |
| `quote_pricing_pipeline.rs:836` → `snapshot_pending_writebacks` | no such symbol exists in `apps/aberp/src/**` — call-graph name collision |

Two independent facts corroborate the zero:

* `Handle::write()` sites in this tree consistently pass `guard.conn()` /
  `&Connection` **down** rather than re-acquiring — the pattern the tripwire's
  panic message prescribes, and it is followed.
* The `#[cfg(debug_assertions)]` re-entrancy tripwire (`lib.rs:414`) panics on
  exactly this shape, and the whole test suite runs in debug. Any test-covered
  nested acquire would already be a hard failure.

**Caveat, stated rather than hidden:** the tripwire is per-`Handle`-id and
per-thread. A `read()` inside a `spawn_blocking` closure is a different thread
and would not trip; a `read()` on a *second* `Handle` (a different id) would not
trip either. Neither gap produces a hit here, but neither is closed by the
tripwire, so axis (b)'s zero rests on the static reach above and not on the
tripwire alone.

### 1.4 The axis the ADR does not have — writes through `read()`

Three `read()` sites run **DDL** on the connection they obtain:

| Site | Statement |
|---|---|
| `apps/aberp/src/incoming_invoices.rs:769` | `ensure_schema(&conn)` — "ensure ap_invoice schema (list)" |
| `apps/aberp/src/incoming_invoices.rs:825` | `ensure_schema(&conn)` — "(get)" |
| `apps/aberp/src/incoming_invoices.rs:857` | `ensure_schema(&conn)` — "(nav_xml_path read)" |

`incoming_invoices::ensure_schema` is `conn.execute_batch(AP_INVOICE_SCHEMA_SQL)`
— `CREATE TABLE IF NOT EXISTS` plus the family's `ADD COLUMN IF NOT EXISTS`
ladder. It is a write.

**Today this is invisible and nearly harmless.** `Handle::read()` is a
`try_clone` of the *one* DuckDB instance; the DDL runs on the shared instance
and lands in the Handle's own WAL. The only anomaly is that it escapes the
writer mutex — the guard is released the instant the clone is taken — so the
"single serialized writer" invariant is already, quietly, not quite true.

**Finding R-1 — under `sqlite-engine` these become a genuine second writer.**
`read()` becomes a real connection. `ensure_schema` on it takes SQLite's write
lock, outside the writer `Mutex`, concurrently with the `Handle`'s writer.
Consequences, in order of severity:

1. ADR-0108 §2.4's "**Single-writer.** The writer `Mutex` stays" becomes false.
   The mutex no longer serialises all writers; DDL escapes through `read()`.
2. Every AP-invoice list/get request contends for the write lock. Under a live
   `write()` it waits out `busy_timeout` — **5 seconds on a read route** — then
   returns `SQLITE_BUSY`. It fails loud (the error is `?`-propagated with
   context), so rule 11 holds, but the latency is a user-visible regression on a
   path that is a pure read today.
3. It makes `busy_timeout` load-bearing on the *read* surface, which §4 shows it
   otherwise is not.

*Ruling:* **MUST-fix before Step 5.** The AP-invoice family (`incoming_invoices`)
crosses as one fused unit under rule 14; the fix belongs in that family's step,
and it is small — hoist the three `ensure_schema` calls to the family's
boot/first-write path, or take a `write()` for them. What must not happen is the
family crossing with DDL still on the read connection.

### 1.5 Class (b) — the genuine second connections

The frozen structural baseline holds **33 fn-entries**, which resolve to **49
concrete opener sites**. The gate is green (`✓ 0 new`) at the base commit.

**GROUP A — live in-serve on the serve-held DB (13 entries, 16 opener sites).**
These are the ones that matter.

| Site | Opener |
|---|---|
| `apps/aberp/src/serve.rs:10610` `read_invoice_currency` | `Connection::open` |
| `apps/aberp/src/serve.rs:10635` `read_invoice_total_gross_minor` | `Connection::open` |
| `apps/aberp/src/serve.rs:21128` `resolve_recipient_email` | `Connection::open` |
| `apps/aberp/src/serve.rs:11147` `calibration_overview_request` | `Connection::open` |
| `apps/aberp/src/serve.rs:23368` `handle_quote_pipeline_status` | `duckdb::Connection::open` |
| `apps/aberp/src/serve.rs:28514` `handle_list_email_relay_queue` | `duckdb::Connection::open` |
| `apps/aberp/src/serve.rs:28568` `handle_get_email_relay_row` | `duckdb::Connection::open` |
| `apps/aberp/src/serve.rs:27560` `spawn_dap_audit_chain` | `Ledger::open` |
| `apps/aberp/src/serve.rs:28401` `handle_relay_send_email` | `duckdb::Connection::open` (also writes) |
| `apps/aberp/src/reports.rs:1300 / :1313 / :1319 / :1327` `compute_financial_report` | `Connection::open` ×2, `DuckDbBillingStore::open`, `Ledger::open` |
| `apps/aberp/src/quote_pdf_rerender_daemon.rs:416` `prepare_rerender` | `Connection::open` |
| `apps/aberp/src/audit_dap_boot.rs:161` `run_heartbeat_supervised` | `Ledger::open` |
| `crates/aberp-mes/src/ledger_writer.rs:143` `write_one` | `duckdb::Connection::open` (also appends; CHECK 10M reports zero) |

**GROUP B — separate-process CLI one-shots (12 entries, 14 opener sites).**
Coherent by mutual exclusion, not by architecture. Two hold **no flock**
(`print_invoice.rs:166` `render_to_bytes`, `rebuild_stock_cache.rs:61` `run`);
four are dual-context (`issue_invoice.rs:631`, `issue_modification.rs:141`,
`issue_storno.rs:165`, `poll_ack.rs:193`), coherent only because their in-serve
entry points are different functions.

**GROUP C — not the serve-held live path (8 entries, 19 opener sites).**
Boot-phase `serve::run` (12 sites), the demo/new-tenant files, the snapshot
machinery on temp/staging files, and `Ledger::open` itself.

### 1.6 Class (b′) — openers the read-fork gate classifies in neither direction

Cross-referencing the ADR-0098 opener census (81 openers / 41 fn-sites) against
the read-fork baseline (33) and allow-list (11) leaves **four** fn-sites in
neither list:

| Site | Assessment |
|---|---|
| `apps/aberp/src/serve.rs:6181` `record_upgrade_snapshot_mismatch_audit` | `Ledger::open`, called from `serve::run` at `:1146` — boot phase, **before** `register_serve_handle` (`:1615`) and before the shared Handle. GROUP-C-equivalent, **safe**, but unlisted in any read-fork list. |
| `crates/audit-ledger/src/storage/mod.rs:561` `append_reopen` | **Zero live callers.** Every remaining mention is a doc comment (incl. `aberp-db`'s crate docs and CLAUDE.md's framing). Dead code that two design documents still cite as a live hazard. |
| `modules/billing/src/adapters/duckdb_store.rs` `open` | The `DuckDbBillingStore::open` implementation — the opener seam, not a caller. |
| `apps/aberp/src/request_technical_annulment.rs` `run` | CLI one-shot; its read helper `load_base_invoice_issue_year` **is** in the baseline (GROUP B). The `run` arm writes. |

And the reverse direction — **seven** baseline entries are absent from the
ADR-0098 opener census, all of them `Handle::open_default` or in
demo/new-tenant paths the census's token set does not recognise
(`issue_invoice|run_with_provider`, `issue_modification|run`, `issue_storno|run`,
`poll_ack|run`, `print_invoice|render_to_bytes`, `serve|bootstrap_demo_tenant`,
`serve|create_tenant_request`).

*Ruling:* no live defect, but the two censuses have **different, partially
disjoint coverage**, and neither is a superset of the other. That is worth one
line in the ADR so a future session does not treat either as the complete set.

### 1.7 Class (c) — read-only opens

`aberp_db::readonly::open_read_only` (`crates/aberp-db/src/readonly.rs:62`),
landed in Step 1. ADR-0108 §1.2's sweep for `access_mode` / `read_only` /
`READ_ONLY` returns **0** non-test hits elsewhere, re-confirmed here. There is no
legacy read-only reader to audit.

One property worth recording for Step 4: a SQLite read-only open of a **WAL**
database still needs to create or write the `-shm` file. A genuinely read-only
filesystem, or a `-shm` owned by another uid, makes the open fail rather than
degrade. `harden_permissions` sets `0600` / `0700` (single-uid), so this is not
a live risk — but it means "read-only" is not permission-free under WAL, and
Step 4's read-only capability is DuckDB-side only, so nothing here changes.

---

## 2. What SQLite WAL actually does — measured, not cited

All six SQLite arms pass at the base commit. Two are the load-bearing ones.

### 2.1 The claim ADR-0108 §2.4 makes and never pinned

> "On SQLite `read()` becomes a genuine second connection rather than a
> `try_clone` — semantically *stronger* (it sees every prior commit)."

`t20a_autocommit_reader_sees_a_later_commit` pins it: a connection opened
**before** the write, sitting in autocommit, sees the commit with no reopen, no
checkpoint, no coordination. **The claim holds.**

### 2.2 Finding R-4 — the in-transaction half is stated wrongly, and a T-20 written to the ADR's wording would pin a false claim

ADR-0108 §7 Step 3:

> "the reader freezes its snapshot **at `BEGIN`** and will not see a commit that
> lands after it"

and §8's T-20: "B inside an explicit `BEGIN` → B does **not** see it."

**Measured, that is false.** `BEGIN` is `BEGIN DEFERRED`: it acquires nothing and
starts no read transaction. The snapshot is taken at the **first read
statement**. `t20b_the_snapshot_is_taken_at_the_first_read_not_at_begin` pins
both halves:

| Sequence | Result |
|---|---|
| `BEGIN` → writer commits → `SELECT` | **sees** the commit (1 row) |
| `BEGIN` → `SELECT` → writer commits → `SELECT` | does **not** see it (still 1) |
| … → `COMMIT` → `SELECT` | re-syncs (2) |

The practical consequence for Step 5: the frozen-snapshot exposure begins at a
transaction's **first `SELECT`**, not at its `BEGIN`. Since §1.2 found **zero**
`read()` sites that open a transaction at all, the class is empty either way —
but T-20 as specified would have been written, failed, and then "fixed" into
whichever assertion happened to go green. That is how a false semantics claim
gets pinned.

*Ruling:* correct §7 Step 3 and §8 T-20's wording. This audit's `t20b` is the
corrected pin and it is already written.

### 2.3 The other two SQLite properties

* `t20c_a_reader_never_observes_an_uncommitted_write` — 18 rows inserted inside
  an open `IMMEDIATE` transaction are invisible to a concurrent reader, then
  appear atomically at `COMMIT`. No torn intermediate state exists on the read
  side.
* `t20d_a_foreign_close_does_not_drop_wal_resident_commits` — a foreign
  connection opens, reads, and closes while five commits are WAL-resident. The
  live writer still has all five; a connection opened afterwards sees all five.

`t20d` is the direct contrast with DuckDB, and §3 is why it is the most
important line in this document.

### 2.4 Which of these actually depend on WAL

Mutation-verified by flipping `apply_posture`'s `journal_mode` to `DELETE`:

| Test | Under `journal_mode=DELETE` |
|---|---|
| `t20a` | still green |
| `t20b` | **RED** |
| `t20c` | still green |
| `t20d` | still green |
| `q11_a_wal_reader_does_not_contend_with_a_live_writer` | still green |
| `q11_busy_timeout_does_not_retry_a_snapshot_conflict` | **RED** |

Stated plainly because it cuts against the framing: **only two of the six
discriminate WAL.** The other four are properties of SQLite as such. That makes
the headline result *stronger* — "a foreign connection's close cannot cost a
committed row" does not depend on a pragma anyone can change — and it means
`t20d`'s green is not evidence that the WAL posture is correct. `T-3d` remains
the pin for that.

---

## 3. Finding R-5 — the read-fork class is not a stale read. It is silent, permanent data loss.

This is the finding that reframes the rest, and it also settles ADR-0107 §1.3 F1,
which ADR-0108 §9 records as "unsettled; needs a measurement; the migration makes
it moot but does not answer it."

### 3.1 What the frozen baseline says

`tools/adr0099_read_fork_structural_baseline.txt`, GROUP A:

> "Under H3 checkpointing is disabled, so anything the Handle has written is
> WAL-resident and these read the last-checkpointed **SUBSET** — the identical
> primitive behind #40/#41/#42/E1."

### 3.2 What measurement says

`duckdb_the_forked_read_itself_is_coherent`: **the forked read is not stale.** A
co-resident second DuckDB instance *replays the WAL on open* and returns all five
committed rows. The stated mechanism is wrong.

`duckdb_a_foreign_close_silently_destroys_every_later_commit` finds the real one.
Deterministic, 3/3 runs, with a no-fork control in the same test:

| Step | writer's view | `.wal` bytes | any other reader |
|---|---:|---:|---:|
| Handle commits 5 rows | 5 | 742 | 5 |
| **a foreign connection opens, reads, closes** | 5 | **0** | 5 |
| Handle commits 10 more | 15 | **0** | 5 |
| Handle drops; DB reopened | — | 0 | **5** |

**Ten committed rows, permanently lost. Every `COMMIT` returned `Ok`.**

The mechanism, precisely:

1. The `Handle` sets `disable_checkpoint_on_shutdown` + `wal_autocheckpoint='1TB'`
   (`lib.rs:648`), so its commits stay WAL-resident *by design*.
2. The foreign connection carries neither pragma. **Its close checkpoints**: the
   WAL is folded into the main file and truncated to zero.
3. From that moment the writer's WAL is gone and does not come back. Subsequent
   commits are visible only to the writer's own in-memory instance and are
   written nowhere. The `.wal` stays at 0 bytes through ten more commits.
4. *Now* other readers are stale — they see the pre-fold state. **The stale read
   is a symptom of a prior fork's close, not the primitive.** A second fork in
   the same run reads 5 while the writer reports 15.
5. On process exit, everything after the first foreign close is gone.

**Control:** identical pragmas, no fork at any point → 15 of 15 survive. The loss
is attributable to the foreign close and to nothing else.
**Mutation:** give the fork the same two pragmas → 0 rows lost. The close is the
injury; the read is not.

### 3.3 What this changes

1. **Severity.** The 13 live in-serve GROUP-A entries are not stale-read
   nuisances. The **first** hit of any one of them in a `serve` process ends that
   process's write durability. Everything committed afterwards — invoices, audit
   rows, the invoice-number floor — is in-memory only until restart.
2. **It explains the incident cluster as one primitive.** #40's "no
   `InvoiceDraftCreated` audit entry found" for an invoice that had just been
   NAV-acked; D2a's empty `Vec` that made an ADR-0101 S2 guard pass vacuously;
   S444's invoice-number floor rewinding in the business tables while the audit
   ledger's floor held. It is also the exact signature of the 2026-07-19
   mirror-ahead-of-DB divergence (mirror 8060 > DB 8058): the mirror is an
   append-only sidecar and is untouched by a DuckDB checkpoint, so it retains
   what the DB threw away. *Consistent with, not proof of* — that incident has
   its own record and this audit did not re-open it.
3. **Read/write is the wrong axis.** The injury is the *close*, so it does not
   matter whether the second connection read or wrote. The read-fork gate and
   the write-fork gate are both partitioning a set whose hazard does not respect
   the partition. **The census that matters is the ADR-0098 opener census (81),
   not the read-fork subset (33).**
4. **It strengthens the case for Step 5.** `t20d` shows SQLite has no analogue:
   a closing connection may checkpoint, but a checkpoint *folds committed frames
   into the main database* — it cannot discard them. The migration does not merely
   move the read-fork class; it removes it.
5. **But prod stays on DuckDB.** ADR-0108 §11 does not authorise a cutover, and
   §7 is DEV-only. So this is a **live production defect for as long as prod runs
   DuckDB**, and the migration is not its fix.

*Ruling:* **R-5 is not a Step-5 blocker and must not be folded into this
migration** (CLAUDE.md rule 3). It needs its own PR. Two candidate shapes, both
small, in preference order:

* **Make every opener carry the pragmas.** `disable_checkpoint_on_shutdown` +
  `wal_autocheckpoint='1TB'` at every `Connection::open` on a live tenant DB —
  measured above to reduce the loss to zero. Cheap, mechanical, and it does not
  require migrating any of the 13 routes. It is a containment, not a fix: the
  forks remain.
* **Migrate the 13 GROUP-A routes to the Handle**, which is what the baseline
  says should happen. Larger, and it is product work on live invoice/NAV routes.

The containment should land first regardless, because it is hours and it stops
the bleeding on a defect that is live today.

---

## 4. `busy_timeout` — the decision (Q11 / T-21)

**Decision: keep `BUSY_TIMEOUT_MS = 5_000`** (`crates/aberp-db/src/sqlite.rs:92`).
The number is right. **ADR-0108 Q11's justification for it is not**, and the
condition it attaches is unsatisfiable as written.

### 4.1 What ADR-0108 says

> "a `read()` taken *while a `write()` guard is live* now contends for a real
> file lock instead of sharing one in-process instance, so M7's finite
> `busy_timeout` converts DuckDB's immediate mutex self-deadlock into a **timed
> hang, then `SQLITE_BUSY`** — rule 13's known failure mode with its loudness
> removed. The number *is* the observability of the worst case."

and Q11 is closed "**5000 ms, conditional on T-21 landing first**".

### 4.2 Finding R-3 — that reasoning has an unexamined step, and T-21 cannot be written until it is made

`Handle::read()` (`lib.rs:362`) does **not** simply hand out a connection. It:

1. runs the debug re-entrancy tripwire,
2. takes the writer `Mutex` via `lock_recovering()` — for poison recovery,
3. calls `ensure_open()`,
4. `try_clone`s.

Under `sqlite-engine` there is no `try_clone` of a shared cache, so step 4
becomes an open. **Steps 2 and 3 are a free choice, and nobody has made it.**

* **If the SQLite arm keeps `lock_recovering()`** (the faithful port — it is what
  preserves poison-recovery parity and `ensure_open`), then a nested
  `read()`-inside-`write()` deadlocks on the **`std::sync::Mutex`**, exactly as
  today. `busy_timeout` is never reached, because SQLite is never asked for a
  lock. The failure mode is *unchanged*, the debug tripwire still catches it,
  and the ADR's stated coupling between Q10 and Q11 does not exist.
* **If the SQLite arm drops the mutex** (open a fresh connection directly — the
  natural implementation, and the one that makes `read()` not serialise behind
  the writer), then the tripwire's premise disappears with it: `read()` no longer
  touches the mutex, so there is nothing to deadlock and nothing for
  `assert_not_reentrant` to protect. The nested case becomes *legal*, and the
  loud abort T-21 demands must be **deliberately added** — it is no longer a
  behaviour anyone gets for free.

Either way, **T-21 as specified ("a nested `read()`-inside-`write()` aborts
loudly rather than waiting out `busy_timeout`") describes a race between the Rust
mutex and the SQLite busy handler that, in the first arm, cannot occur, and in
the second arm, has no abort to assert.** The test cannot be written until the
implementation choice is recorded.

*Ruling:* **MUST-fix before Step 5.** The decision is one paragraph, not a
redesign. Recommendation: **keep `lock_recovering()`** — it preserves
poison-recovery parity, keeps the tripwire load-bearing, and keeps
"single-writer" meaningful. Then T-21 is rewritten as what it actually pins: a
nested `read()`-inside-`write()` **panics on the tripwire in debug and deadlocks
on the mutex in release, and never reaches SQLite** — which is the same shape as
today and therefore not a regression. That is a testable statement; the ADR's is
not.

### 4.3 Why 5000 ms is nevertheless the right number

Measured, on the read surface `busy_timeout` is close to irrelevant:

* `q11_a_wal_reader_does_not_contend_with_a_live_writer` — with the reader's
  timeout set to **0 ms**, a read succeeds immediately while a writer holds an
  open `IMMEDIATE` transaction. WAL readers do not block on writers. A reader
  reaches the busy handler only against a *checkpointer*, which under a
  single-operator desktop with one writer is rare and short.
* `q11_busy_timeout_does_not_retry_a_snapshot_conflict` — a DEFERRED
  read-then-write across a concurrent commit returns `SQLITE_BUSY_SNAPSHOT`
  **immediately** (measured well under 2 s with a 5000 ms timeout set). The busy
  handler is **not** invoked: retrying cannot help, because the transaction's
  snapshot is already stale. Only `BEGIN IMMEDIATE` (M5) prevents it — the same
  test shows the `IMMEDIATE` path allocating 63 after B took 62, with no number
  reissued.

So 5000 ms is a **write**-contention knob, for two situations only:

1. two `BEGIN IMMEDIATE` writers — in-process this cannot happen (the writer
   mutex serialises them); cross-process it is the CLI one-shots vs `serve`,
   which the `db_writer_lock` flock already mutually excludes;
2. a checkpointer waiting on a long reader.

**Write-contention profile of a single-operator desktop ERP:** one operator, one
`serve` process, one writer mutex, occasional flock-fenced CLI one-shots. Genuine
`SQLITE_BUSY` should be approximately never. 5 s is therefore chosen not as a
throughput parameter but as a ceiling: long enough that a `synchronous=FULL` +
`fullfsync` commit on the NAV path (the slowest write in the tree, and the one
that must not fail spuriously) cannot produce a false `SQLITE_BUSY`; short enough
that a UI request fails visibly rather than appearing to hang.

**Behaviour on timeout — refuse loudly, never stale, never empty.** This is
already true and should stay pinned: `rusqlite` surfaces `SQLITE_BUSY` as an
`Error`, every call site in this tree `?`-propagates it with `.context(...)`, and
there is no code path that maps a busy result to an empty `Vec` or a default.
The one shape that would violate it — `reports.rs:872`'s
`decimal_str_to_i64(&s).unwrap_or(0)` — is a *parse* fail-open, is already in
ADR-0108 §9's deferral ledger, and is closed by the Step-5 `reports.rs:861` fold.

**Revision rule (unchanged from the ADR):** Step 2's measured p99 write-hold may
revise this **downward**, with the measurement in the PR body. Raising it
requires re-arguing R-3.

---

## 5. Per-site ruling

### 5.1 The 102 `Handle::read()` sites

| Ruling | Sites |
|---|---:|
| **SAFE under WAL as-is** | **99** |
| **SAFE-with-a-required-change** | **3** — `incoming_invoices.rs:769/825/857`: move `ensure_schema` off the read connection (R-1) |
| **MUST-route-through-the-Handle** | 0 (they already are) |

Basis for the 99: axis (a) is empty (no site holds a transaction), axis (b) is
empty (no site is reached under a live `write()` guard), and `t20a` pins that a
second connection in autocommit sees every prior commit. Under SQLite these
sites strictly gain coherence relative to `try_clone` and lose nothing.

### 5.2 The 49 second-connection opener sites

| Group | Sites | Ruling |
|---|---:|---|
| **A — live in-serve** | 16 opener sites / 13 fns | **Not a Step-5 blocker; a live production defect (R-5).** These families do not cross in Step 5. Each is a durability-destroying close *today, on DuckDB*. Containment (pragmas at every opener) in its own PR; migration to the Handle is the real fix and is product work per the baseline. Once a family crosses to SQLite, its GROUP-A sites become merely incoherent-if-transactional rather than destructive — but they must still be Handle-routed under rule 14, in that family's step. |
| **B — CLI one-shots** | 14 sites / 12 fns | **SAFE by mutual exclusion**, except: `print_invoice.rs:166` and `rebuild_stock_cache.rs:61` hold **no flock**, so their premise is unestablished. Unchanged by the migration; already flagged in the baseline. |
| **C — not the live path** | 19 sites / 8 fns | **SAFE.** Boot-phase (before `register_serve_handle`), different tenant files, or temp/staging. |
| **b′ — unclassified** | 4 fn-sites | **SAFE.** `record_upgrade_snapshot_mismatch_audit` is boot-phase; `append_reopen` is dead; the other two are opener implementations. Worth listing so the gap is visible. |

### 5.3 Read-only

`readonly::open_read_only` — **SAFE**, one implementation, no legacy callers,
DuckDB-side only, unaffected by the SQLite crossing.

---

## 6. Deferral ledger (CLAUDE.md rule 3)

| Item | Closed by |
|---|---|
| **R-5 — a foreign connection's close silently destroys every subsequent commit's durability (live in prod today, DuckDB, 13 in-serve routes).** Measured, deterministic, with a control. | **Its own PR, before anything else.** Containment (the two pragmas at every live-tenant opener) is hours; migrating the 13 GROUP-A routes is product work. Explicitly **not** folded into ADR-0108 — the migration is DEV-only and does not reach prod. |
| The frozen baseline's GROUP-A rationale states the mechanism as a stale read of "the last-checkpointed SUBSET". Measured false. | Same PR as R-5 — the baseline's header text is where the next reader learns the mechanism. |
| The read-fork and write-fork gates partition on read/write, but the hazard is the *close*. The ADR-0098 opener census (81) is the set that matters. | Recorded here; a gate change belongs with R-5, not with the migration. |
| `crates/audit-ledger/src/storage/mod.rs:561` `append_reopen` has **zero live callers**, yet `aberp-db`'s crate docs and CLAUDE.md rule 13 both cite it as a live hazard. | Rule 12 (delete the part). Own PR; trivial. |
| ADR-0098's opener census and ADR-0099's read-fork baseline have partially disjoint coverage; neither is a superset (7 entries in one only, 4 in the other). | One line in ADR-0108 §1.2 alongside the R-2 correction. |
| `readonly` under SQLite: a read-only open of a WAL database still needs `-shm` write access. | Not reachable today (single-uid, `0600`/`0700`); recorded for a future prod-cutover session (§11). |
| The re-entrancy tripwire is per-thread and per-`Handle`-id, so a `read()` inside `spawn_blocking`, or on a second `Handle`, does not trip. | Out of scope. Recorded because axis (b)'s zero rests on static reach, not on the tripwire. |

---

## 7. What must change before Step 5

1. **R-1** — move `ensure_schema` off the read connection at
   `incoming_invoices.rs:769`, `:825`, `:857`. Lands in the AP-invoice family's
   own step, before that family's storage crosses.
2. **R-3** — decide and record whether `Handle::read()`'s SQLite arm takes the
   writer mutex, then rewrite T-21 to pin the failure mode that choice produces.
   Recommendation: keep the mutex; T-21 becomes "nested `read()`-inside-`write()`
   never reaches SQLite".
3. **R-2** — correct ADR-0108 §1.2 and §7 Step 3: **102 `read()` / 136 `write()`
   / 238 total**, not 50 / 34 / 84.
4. **R-4** — correct §7 Step 3 and §8 T-20: the snapshot is taken at the first
   read statement, not at `BEGIN`. The corrected pin is already written
   (`t20b`).

None of the four is large. With them done, the read surface is fork-safe and
Step 5 can begin.
