# ADR-0110 — The durable-commit contract and business-state boot replay

- **Status:** **Partially implemented** — D3 landed 2026-08-09 (rev. 3); D7 (the
  WAL-truncation fence, added after incident 00012) landed 2026-08-12 and ships
  DISARMED pending the PR #3 opener sweep (D7.6). D1, D2,
  D4, D5 and the payload widening remain proposed. Adversarially reviewed
  2026-08-08; verdict *sound after changes*, listed in §11 and folded into
  §5/§6/§7. Rev-3's corrections are in §12.
- **Date:** 2026-08-09 (rev. 3, post-D3-implementation)
- **Deciders:** Ervin
- **Context repo:** `Cservin69/ABERP` (production line), at `380ba8a`
- **Landing note:** this file is held at
  `/Users/aben/Documents/Claude/Projects/ADR-0110-durable-commit-contract.md`.
  It must be committed to `~/ABERP/adr/0110-durable-commit-contract.md`
  (`0109` is the current max). The review session was barred from writing to
  `~/ABERP/.git`.
- **Related:** ADR-0095 (crash-safe durability + boot auto-recovery), ADR-0098
  (daemon-path durability / recovery-guard coherence), ADR-0099 (prod durability
  lane — H1/H2/H3, and the **H4 that was never built**), ADR-0030 + ADR-0008
  (audit mirror / hash chain), ADR-0082 (snapshot system), ADR-0107 (engine
  evaluation), ADR-0108 (SQLite DEV migration plan), ADR-0019 (storage strategy)
- **Authorises:** nothing further to be built. Rev. 2 authorised nothing at all;
  D3 was subsequently built and is recorded here as implemented (§5 D3, §12).
  Nothing else in this document is authorised by it, and it authorises no
  schema change and no prod touch — deployment is the operator's call.

---

## 0. Summary in one paragraph

We have spent two days hardening the audit chain against **forks**, and the
hardening worked. Every production incident we have actually suffered was not a
fork; it was **lost writes on an unclean restart**. Those are orthogonal axes.
The primary store (`aberp.duckdb`) is deliberately configured never to fold its
WAL into the live file, on the stated rationale that "the only checkpoint is the
validated logical one (H4)" — and **H4 was never implemented**, so the primary
store is effectively never durably flushed during operation. The one store that
*is* durable per write (the fsync'd audit mirror) is repaired at boot into
`audit_ledger` **only**, never into the business tables. The result on 2026-08-08
was a flawless audit ledger sitting on frozen business rows: the ledger knew
invoices 00010 and 00011 existed; the `invoice` table did not.

---

## 1. The miss — stated honestly

The hardening lane we ran (ADR-0098, ADR-0099, ADR-0105, cut-gate CHECK 10M/10N,
the one shared `aberp_db::Handle`) optimises **one** property:

> *the audit hash chain must not fork.*

Every gate we built tests that property. `tools/adr0099_write_fork_scan.awk`
flags a function only when it contains **both** an independent live-DB opener
**and** an audit append. That predicate is correct for fork-detection and
**structurally blind to durability**: a function that opens the live DB, writes,
and closes — with no audit append — is invisible to the gate no matter what it
does to the WAL. §2.3 shows that this blind spot is not hypothetical; it is where
the boot path lives.

The failure class we keep suffering is a different property:

> *an acknowledged write must survive an unclean restart.*

Nothing in the tree tests that. There is no durability gate, no crash-injection
e2e over the business tables, no boot-time WAL forensic. We hardened the lock on
the vault door while the deposits were never reaching the vault.

This ADR is scoped to the second property. It does not revisit the first.

---

## 2. Verified root cause

Every claim below was re-derived by reading the code at `380ba8a`, in an isolated
read-only clone. File:line citations are to `~/ABERP` (the production line).

### 2.1 We have three stores with three different durability postures

| Store | Durability posture | Evidence | Data lost on 2026-08-08 |
|---|---|---|---|
| Audit mirror `<db>.audit.log` | **fsync per append batch** | `crates/audit-ledger/src/mirror.rs:527`, `:703`, `:1268`, `:1296` (`file.sync_all()`) | **none** |
| Snapshots `~/.aberp/**/snapshots/` | **fsync file + parent dir before/after rename** | `crates/aberp-snapshot/src/crash_safe.rs:100–141`, `:211–236` | **none** |
| Primary DB `aberp.duckdb` | **no fsync anywhere; folding explicitly disabled at runtime** | §2.2 | **~22 h of writes** |

`grep -rn 'sync_all\|sync_data\|fsync' crates/aberp-db/src/` returns **zero
functional hits** (one match, `lib.rs:702`, is the word inside a log string). The
engine adapter that owns every production write contains no durability primitive
at all.

> **Adversarial correction.** The original draft argued "the two stores that
> survive are the two that call fsync", implying fsync is the discriminator.
> That is a *correlation*, and it is only causal under **power loss**. Under a
> plain process kill the OS page cache is retained, so anything that reached
> `write(2)` survives without any fsync. The table above is still the right
> summary of posture, but it does not by itself establish the mechanism.
> §2.7 now does that work properly.

### 2.2 The primary store is configured never to become durable at runtime

`crates/aberp-db/src/lib.rs:636–644` applies two pragmas to **every** runtime
connection:

```rust
conn.execute_batch("PRAGMA disable_checkpoint_on_shutdown;")?;
conn.execute_batch("PRAGMA wal_autocheckpoint='1TB';")?;
```

The first stops DuckDB folding the WAL when a connection closes. The second
raises the in-operation auto-fold threshold to effectively infinite. Together, as
the function's own doc comment states, "the only checkpoint that ever touches the
live file is the validated logical one (H4)."

That reasoning is sound **only if H4 exists**. It does not:

```rust
// crates/aberp-db/src/lib.rs:604–612
fn run_durable_checkpoint_locked(&self, inner: &mut Inner) {
    tracing::error!(
        db = %self.db_path.display(),
        "aberp-db: run_durable_checkpoint_locked reached while the runtime checkpoint \
         is DISABLED (H3) — the validated fold lands in H4; folding NOTHING this tick"
    );
    inner.debouncer.record_checkpoint(Instant::now());
}
```

`HandleConfig::checkpoint_enabled` defaults to `false` (`lib.rs:154`), so the stub
is not even reached; the `WriteGuard::drop` branch that would call it is gated off
(`:712`). `aberp_snapshot::live_durable_checkpoint` — the function the doc comment
promises H4 will call — **does not exist in this tree at all**.

**This is the single most important fact in this document.** The disable is
shipped; the replacement is a `tracing::error!`. We took the safety off and never
installed the new one.

The observable proof is in the snapshot metadata. `take_snapshot` records
`source_db_sha256 = sha256_file(db_path)` — the hash of the **live file**
(`crates/aberp-snapshot/src/take.rs:267`). Snapshots 52→57 carry an *identical*
`source_db_sha256` (`318087a6d83e1771…`) across ~20 h of active writing. The main
file did not change a byte, because nothing ever folded into it at runtime.

The snapshots themselves are complete because `EXPORT DATABASE` runs on a
`Handle::read()` clone of the shared instance, which observes WAL-resident rows.
So: the snapshots contain the invoices; the file they were taken from does not.
That asymmetry is the whole incident in one line.

### 2.3 Boot re-enables exactly the fold the runtime forbids — 13 times

`take.rs:215–253` documents — with a measurement — that `wal_autocheckpoint` and
`disable_checkpoint_on_shutdown` are **per-connection**, not per-database:

> Measured on the same file in the same process: a `Handle` clone reports
> `wal_autocheckpoint` = 931.3 GiB, a fresh `Connection::open` = 16.0 MiB.

A connection opened outside the `aberp-db` seam therefore carries DuckDB's
**defaults** — 16 MB auto-fold, checkpoint-on-shutdown **enabled** — and closing
it folds the WAL in place. That is the `duckdb#23046` torn-metadata path the
runtime design exists to avoid, and the documented cause of the 2026-07-19 boot
refusal.

Verified opener census on the boot path, before the shared `Handle` is ever
constructed (`apps/aberp/src/serve.rs`), each in its own scope so each is
**dropped — and therefore folds — immediately**:

| Site | Line |
|---|---|
| `probe_open_or_preserve` → `probe_open` → `Connection::open` + close | `:1216` → `crash_safe.rs:334–340` |
| `DuckDbBillingStore::open` (provisioning arm, new DBs only) | `:1206` |
| `DuckDbBillingStore::open` (billing migrations) | `:1231` |
| products / inventory / quoting_materials + further boot migrations | `:1248`, `:1267`, `:1283`, `:1307`, `:1324`, `:1341`, `:1359`, `:1376`, `:1396`, `:1412` |
| mirror reconcile + heal + ART index rebuild | `:1461` |

> **Adversarial correction — this is NOT the 2026-08-08 loss mechanism, and the
> original draft's framing was backwards in an important way.** See §2.7. Boot
> folding is, today, the *only* thing that ever makes prod data durable: the
> incident report's own words are "the live file only advances at process start,
> when DuckDB checkpoints after replaying the WAL." The 13 openers are a genuine
> **latent tearing hazard** (the mid-July tear at `serve.rs:1504–1526` proves the
> class can fire) but they are simultaneously the current de-facto durability
> mechanism. **Removing them without first landing H4 makes durability strictly
> worse.** This inverts the phase order of the original draft — see §6.

The tree already contains a written admission of the tearing class, at
`serve.rs:1504–1526`, explaining why the ART indexes must be rebuilt on every boot:

> the two `ap_invoice` secondary indexes were MISSING their entries for every row
> appended since **a mid-July WAL tear**

### 2.4 The boot heal repairs the ledger only — never the business rows

`heal_from_mirror_ahead` (`mirror.rs:1052–1230`) is careful, well-gated work:
boundary `entry_hash` equality, a full-genesis `verify_chain` over the decoded
mirror, preserve-before-heal, and an in-transaction full re-verify that rolls back
on any failure. It is a good piece of engineering aimed at the wrong target.

What it actually replays is in `crates/audit-ledger/src/storage/mod.rs:785–855`:

```rust
for entry in tail {
    insert_entry_verbatim(conn, entry)?;   // ← audit_ledger rows. Only.
}
insert_entry_verbatim(conn, &forensic)?;
```

So on 2026-08-08 the heal did precisely what it was built to do — restored a
byte-identical, genesis-verified chain — while the `invoice`, `invoice_line`,
`ap_invoice`, and payment rows those very entries *describe* stayed missing.

The mirror held every event. We threw away the business meaning of every one of them.

**This remains the highest-value gap in the system.** Everything else in this ADR
is about not losing rows; this is about the fact that we already have the data to
put them back and do not use it. §7 bounds *how much* of it we can put back.

### 2.5 The mirror is a redo log for *some* events and a pointer for others

**Sufficient to reconstruct (payload is self-contained):**

- `InvoiceSubmissionAttempt` — `request_xml: Vec<u8>` holds the **verbatim
  `<ManageInvoiceRequest>` envelope POSTed to NAV** (`audit_payloads.rs:583–598`).
- `InvoiceSubmissionResponse` — verbatim response + `transaction_id` (`:631–638`).
- `InvoicePaymentRecorded` — invoice id, date, `amount_minor`, currency, method,
  reference (`:1865–1901`).
- `InvoiceSequenceReserved` — the S444 durable number floor (`:60–93`). The one
  place we already got it right.

**Insufficient (payload is deliberately a pointer):**

- `InvoiceDraftCreated` — `audit_payloads.rs:138–177`: *"The fields are
  intentionally narrow today … because the full draft content is reconstructible
  from the `invoice` + `invoice_line` tables. The payload is a pointer, not a
  duplicate."* That sentence is the design assumption the incident destroyed.
- `IncomingInvoiceStatusChanged` — `audit_payloads.rs:2232–2252` carries
  `ap_invoice_id`, `idempotency_key`, `from_status`, `to_status`, `reason`. It is
  complete **as a transition** and **useless as a row constructor** — see §7.3.

So a blanket "the mirror is our redo log" is **false today** and must not be
asserted.

### 2.6 Snapshot × chain-heal: `snapshot now` on a rebuilt DB bricks boot

Confirmed as a real, code-visible hazard:

1. A restore does **not** rebuild the mirror (`apps/aberp/src/snapshot.rs:426–431`
   prints this as an operator warning). After a rebuild the mirror still holds the
   *pre-rebuild* chain and is ahead.
2. `aberp snapshot now` **appends its own audit entry** — `SnapshotCreated` via
   `append_audit` → `Handle::write` → `append_in_tx`. The rebuilt DB's head
   advances with an `entry_hash` computed from the rebuilt prefix.
3. The chains now diverge at the boundary.
4. At runtime `sync_mirror` detects it and returns `MirrorDivergent`
   (`mirror.rs:652–677`) — but the caller is `WriteGuard::drop`, which downgrades
   it to `tracing::warn!` (`aberp-db/src/lib.rs:696–705`). The mirror silently
   stops tracking.
5. At the next boot, `heal_from_mirror_ahead`'s Discriminator 1 mismatches and
   refuses; `serve.rs:1470–1484` then **refuses to boot**.

So the recovery tool, run in the obvious order (rebuild, then snapshot it), bricks
the system — and the warning was swallowed into a `warn!` four steps earlier.

### 2.7 Which hypothesis the evidence actually supports — **RESOLVED**

The original draft listed three hypotheses it could not separate and made Phase 1
"correct under all three". The adversarial pass resolved this from evidence that
already existed but had not been cross-referenced.

The three candidates:

- **H-A — the WAL's contents were not durable at the moment of the unclean
  restart** (never fsync'd, or fsync'd-but-lost with the page cache on a hard
  power-off).
- **H-B — a torn or aborted boot fold** discarded the WAL (the §2.3 mechanism).
- **H-C — the WAL file was deleted** by a recovery path (`atomic_install`
  `crash_safe.rs:150–152`, `restore_into` `take.rs:413–416`).

**The evidence supports H-A, and specifically refutes H-B.** Three discriminators:

1. **The revert was to a clean, exact prefix.** The DB "reverted to audit seq
   8327" — the last boot-fold point — with the loss window running contiguously
   8328–8399. A torn fold produces *partial* or *corrupt* state, not a
   byte-consistent prefix.
2. **The ART indexes were clean.** The incident report §4 measured row-scan vs
   index-lookup agreement exactly (111/107/4) and states "**No — clean.** Not a
   repeat of 2026-08-03." Index desync is the *signature* of the mid-July WAL
   tear (`serve.rs:1504–1526`). A torn fold in 2026-08-08 would have desynced the
   ART the same way. It did not. **This is the decisive discriminator and the
   original draft missed it.**
3. **H-C is unmotivated.** No restore ran in the window, and the recovery pass
   found a live `.wal` present beside the DB (preserved as
   `aberp.duckdb.wal.PRE-RECOVERY-20260808T170507Z`, 6 823 B).

The residual uncertainty inside H-A — "DuckDB fsynced and the restart lost it
anyway" vs "it was never fsynced" — is real, is named in the incident report §6,
and **does not affect any decision in this ADR**. Both sub-cases are closed by the
same fix (a store we fsync ourselves, plus a real checkpoint). Note that a plain
process kill would not have lost page-cache-resident bytes, so the sub-case that
best fits a *force-restart of the machine* is a hard power-off against an
unsynced WAL.

**Consequences of the resolution:**

- Phase 0 (forensic instrumentation) is **downgraded from prerequisite to
  desirable**. It is still worth building — we should never again be reasoning
  backwards from surviving stores — but it no longer blocks Phase 1.
- **D4 is demoted as a loss-fix and promoted as a D2 prerequisite** (§7.4).
- The §2.3 boot-fold census stays in the document as a *latent* hazard with a
  documented precedent, not as this incident's cause.

---

## 3. Requirements

- **R1 — No acknowledged write may be lost.** If the operator sees "invoice
  created" or "marked paid", that state survives an unclean restart of the
  process **and of the machine**.
- **R2 — Self-healing to business state.** A lost-writes restart must restore
  business rows, not merely the audit chain, without operator intervention —
  *within the scope §7 proves is reconstructible, and no further.*
- **R3 — Never silently.** Every heal, refusal, or degradation is a loud, typed,
  audited event. No `warn!`-and-continue on a durability fault (CLAUDE.md rule 11).
- **R4 — No fork regression.** Nothing here may weaken the one-Handle discipline
  or the chain invariants of ADR-0098/0099/0105 (CLAUDE.md rules 13/14/15).
- **R5 — Recovery tools cannot brick the system.** No supported operator sequence
  may leave `serve` unbootable.
- **R6 — Bounded cost.** Single operator, CNC shop. A few tens of milliseconds per
  write is fine; a second is not.
- **R7 — No reconstructed row may be silently wrong.** *(new, from §7.1.)* A
  replayed business row must be either byte-equivalent to what the original write
  path would have produced, or visibly marked as reconstructed. Producing a
  plausible-but-divergent row is a worse outcome than producing none.

---

## 4. Options for the durable-commit contract

### Option A — Re-enable DuckDB's WAL autocheckpoint + fsync

- **For:** smallest diff; bounds WAL growth.
- **Against:** reinstates the **exact** hazard the pragmas were added to remove.
  `duckdb#23046` in-place folding produced the torn-metadata incidents;
  `take.rs:236–244` measured six in-place folds under sustained writes. We would
  trade "loses the tail" for "tears the whole file" — and a torn file is
  unrecoverable where lost writes are replayable.
- **Verdict:** **reject as the primary mechanism.** A *reduced* threshold may be
  worth it as blast-radius control once a real checkpoint exists (D4).

### Option B — Actually implement H4 (the validated logical checkpoint)

Build `live_durable_checkpoint`: quiesce the shared connection, build a fresh
self-contained file aside, validate it, `fsync`, atomically rename it over the
live path, `fsync` the directory, write the verified-good marker. Every primitive
already exists in `crash_safe.rs` (`atomic_install:143`, `write_marker:168`,
`checkpoint_is_current:213`, `fsync_file:100`, `fsync_dir:110`) and the debounce
policy is coded and unit-tested (`aberp-db/src/debounce.rs`).

- **For:** finishes the design we already half-built. Never folds in place, so
  `duckdb#23046` stays closed. Makes the *whole* database durable — not just the
  events we thought to write payloads for. Bounds the WAL.
- **Against:** debounce is ≤ 1 checkpoint per minute (`debounce.rs:34–37`), so on
  its own it leaves **up to a minute** of acknowledged writes non-durable. It is a
  *bound*, not a *contract*.
- **Verdict:** **necessary, not sufficient.** Adopt — but not as the ack gate.

### Option C — Gate the ack on the already-fsync'd mirror

- **For:** the cheapest correct answer available, because **it is already
  happening**. `WriteGuard::drop` already calls `sync_mirror` after every commit
  (`lib.rs:692–705`), and `sync_mirror` already `fsync`s (`mirror.rs:701–704`),
  before the HTTP handler returns. The only defect is that its failure is a
  `tracing::warn!`. The change is to make the existing durable step **count**.
- **Against:** (i) makes the mirror the system of record for the write path;
  (ii) only as complete as the payloads (§2.5, §7.2); (iii) does nothing for the
  non-audited parts of the DB; (iv) puts an `fsync` on the critical path
  (~24 ms by analogy with the Editions MES measurement — **another tree, another
  workload; must be re-measured here**).
- **Verdict:** **adopt as the ack gate.**

---

## 5. Decision

Adopt **C as the ack gate, B as the floor, and a *bounded, quarantining*
business-state replay as the recovery path.**

- **D1 — Durable-commit contract (Option C).** No operator-visible success for a
  money-path write until the corresponding audit event is `fsync`'d to the mirror.
  Because the transaction has already committed by the time `WriteGuard::drop`
  runs, the guard cannot unwind it — so the ack gate must move **into** the write
  path. The seam is a new `Handle` method performing *commit → sync_mirror →
  return Result*; money-path routes surface a typed, audited failure if the mirror
  step fails. `Drop` keeps best-effort behaviour for everything not yet migrated,
  so this is incremental, not a flag day.

- **D2 — Business-state boot replay, RESCOPED.** *(Changed by the adversarial —
  see §7.)* After `heal_replay_mirror_tail` restores the chain, a **materialiser**
  walks the replayed tail and reconstructs business state for a closed, enumerated
  set of event kinds. Revised design constraints:
  - **Phase-1 kind set shrinks to three:** `InvoiceSequenceReserved` (floor only,
    no row), `InvoicePaymentRecorded`, and `InvoiceSubmissionAttempt` +
    `InvoiceSubmissionResponse` **as a quarantine pair**.
    `IncomingInvoiceStatusChanged` is **removed** from Phase 1 (§7.3).
  - **Reconstruct-and-quarantine, not reconstruct-and-serve.** Invoices rebuilt
    from `request_xml` land in a **separate table**, not in `invoice`. The tree
    has already made this exact decision once: `restored_invoice`
    (`apps/aberp/src/restore_from_nav_outgoing.rs:193`) is a NAV-derived invoice
    table deliberately kept out of `invoice`, with a *narrower* schema and no
    line-item table at all. That precedent is binding here (§7.2).
  - **Per-table, not per-aggregate, idempotency.** Insert-if-absent must be keyed
    per physical row (`invoice.id` *and* each `invoice_line(invoice_id, ordinal)`),
    never on the aggregate root alone (§7.5).
  - **Runs inside the same heal transaction**, after the MF-1 full-genesis verify,
    so a materialiser failure rolls the whole heal back and boot refuses — **but
    only once D4 has given the heal connection a non-default pragma posture**
    (§7.4).
  - **Refuses on any event kind it does not know.** *Necessary but not
    sufficient* — the kind whitelist does not catch a whitelisted kind whose
    payload cannot satisfy its target schema. Each enumerated kind additionally
    carries an explicit **satisfiability assertion** over the NOT NULL columns of
    its target table, checked at build time by a test, not at runtime by hope.
  - **Emits one audited `DbAutoRecovered`-class row** naming exactly what it
    reconstructed and what it could not, so the operator can reconcile against NAV.

- **D3 — Make the ack durable. IMPLEMENTED 2026-08-09 — and NOT as H4.**
  *(Corrected in rev. 3; see §12.1.)*

  Rev. 2 wrote D3 as "implement H4": replace `run_durable_checkpoint_locked`
  with a validated build-aside + `atomic_install` + marker and default
  `checkpoint_enabled = true`. **That is not what shipped, and the difference
  matters.**

  What shipped is `aberp_db::Handle::durable_ack` — Option B, §4's other arm.
  After a money-path commit it `fsync`s the main file, `<db>.wal`, and the
  tenant directory. It **does not fold**. Five ack sites call it, each as
  `drop(guard); db.durable_ack()?`, so the guard-drop mirror `fsync` always
  precedes the DB `fsync`: `issue_invoice::issue_from_parsed`,
  `issue_modification::modification_from_inputs`,
  `issue_storno::storno_from_inputs`, `mark_invoice_paid::mark_paid`,
  `incoming_invoices::change_status`.

  **Why Option B over Option A (fold + fsync), which rev. 2 assumed:**
  1. **A fold is not required for durability.** DuckDB replays `<db>.wal` on
     open, so an `fsync`'d WAL is already a complete durable record of the
     commit. Rev. 2's §5 D6b prose assumed the row could only become durable by
     reaching the main file. It was wrong — measurably: the D6b byte-copy tier
     boots a copy carrying only the WAL and gets the invoice back.
  2. **Folding per ack reopens the hazard the pragmas exist to close.** Option A
     rewrites the main file on every invoice. §2.3 measured six in-place folds
     under sustained writes and §4 Option A already rejects that as the
     `duckdb#23046` torn-metadata path. Doing it at *ack* frequency is Option A
     with a worse duty cycle. Option B leaves the main file untouched on the
     money path, so §7.8's snapshot-forks-chain interaction and the thirteen
     boot openers are entirely unaffected — nothing about boot changes.
  3. **Cost.** One `fsync` of a small append-only file against folding the whole
     WAL and rewriting main-file metadata, on every money write, forever.
  4. **It needs no new primitive.** `aberp_snapshot::live_durable_checkpoint`
     still does not exist in this tree (§2.2). Option A would have had to build
     it; Option B is `File::sync_all`, the same primitive `crash_safe.rs` and
     the audit mirror already use — and the mirror is the store that lost
     **nothing** on 2026-08-08, so the choice is evidenced rather than guessed.

  **Measured cost (R6, §9's unmeasured ~24 ms estimate now replaced by a real
  number).** 20 issuances, release build, this tree: **12.11 ms** per acked
  issuance with `durable_ack`, **11.00 ms** without — **≈1 ms marginal**. Not
  operator-visible. Reproduce with `durable_ack_latency_stays_inside_r6`.

  Read that figure as *marginal*, not as the cost of a device flush. Every ack
  already pays one `F_FULLFSYNC`: `WriteGuard::drop` runs `sync_mirror`, which
  `sync_all`s the mirror before `durable_ack` is ever called. D3's ~1 ms is
  what a **second** flush costs on a device the same ack has just flushed — it
  is not evidence that a full flush is cheap, and a workload that did not
  already sync would pay more.

  **What D3 explicitly does NOT do, and the cost of that:**
  - **H4 is still unbuilt.** `run_durable_checkpoint_locked` is still the
    `tracing::error!` stub and `checkpoint_enabled` still defaults `false`.
  - So the runtime WAL is now **durable but still unbounded**, and it is still
    the boot fold that truncates it. **D4 therefore still depends on H4, not on
    D3** — a boot that stops folding without H4 leaves the WAL growing without
    limit. §6 Phase 3 is unchanged in that respect.
  - Durability is **money-path only**, by choice: bounding the blast radius and
    the latency. Every non-money write (quotes, inventory, MES, catalogue,
    email outbox) is exactly as durable as it was, i.e. WAL-resident until the
    next boot fold. §7.10's warning stands and now has a name: the next loss
    will be in a family D3 does not cover, and **H4 is what covers them**.
  - **This IS power-loss durable on the internal disk** — stronger than rev. 3
    first claimed. `File::sync_all` is not `fsync(2)` on macOS: the pinned
    1.97.0 stdlib routes it to `fcntl(F_FULLFSYNC)`, a real device-cache flush
    (verified in `std/src/fs.rs` → `sys/fs/unix.rs`,
    `#[cfg(target_vendor = "apple")]`). The residual is one layer lower — the
    drive must honour the flush, which Apple guarantees for the internal NVMe
    and nobody guarantees for a third-party external enclosure. See §12.4.

- **D4 — Boot must stop folding blind.**
  1. Every boot-phase opener gets the same pragma posture as the Handle, via a
     single `aberp_db::open_boot_connection` used by all thirteen sites. Then run
     **one** explicit, validated H4 checkpoint at boot instead of twelve implicit
     unvalidated ones. **This step is only safe once D3 exists** (§2.3, §7.4).
  2. A **new cut-gate check** asserting that no live-DB opener in `apps/` or
     `crates/` (outside `aberp-db` and `/tests/`) uses a bare `Connection::open`
     without the pragma seam. This is the gate the fork scanner structurally
     cannot be (§1): its predicate is *opener*, not *opener AND audit append*.

- **D5 — Snapshot × chain-heal safety (R5).**
  1. `MirrorDivergent` from `sync_mirror` becomes a loud, audited,
     operator-visible fault, not a `warn!`. Under D1 it fails the write.
  2. `snapshot now` refuses to append `SnapshotCreated` when the mirror head does
     not agree with the DB head.
  3. Rebuild/restore gains an explicit, audited **mirror re-baselining** step,
     with the pre-restore mirror preserved byte-for-byte.

- **D6 — Prove it, don't assert it. *(Spec corrected twice: §7.6, then §12.2.
  BUILT and GREEN.)*** A crash-injection e2e, now in three tiers, all in
  `apps/aberp/tests/adr0110_d6b_ondisk_durability.rs`:
  - **D6a — process-crash tier.** `SIGKILL` at the ack boundary, restart, assert
    the **`invoice` row** (not the ledger row) is present. Catches user-space
    buffering. **Not built**, and rev. 3 does not intend to build it: §7.6
    already establishes that a system with zero `fsync` passes it, so it can
    only weaken the suite's apparent strength. The tier below strictly dominates
    it.
  - **D6b tier 1 — byte-copy tier. BUILT, and it PASSES against `380ba8a`,
    contradicting rev. 2's expectation.** Rev. 2 wrote this as the power-loss
    tier and expected it red. It is green, and was green *before any fix*:
    DuckDB pushes WAL records out to `aberp.duckdb.wal` at commit, so a copy of
    the tenant directory taken at the ack does carry the invoice and a fresh
    instance replays it. **This tier cannot be the Phase-1 specification** — a
    file copy reads through the OS page cache, so it measures "did the write
    reach the file", never "did it reach stable storage". §7.6's argument
    against `SIGKILL` applies to it verbatim. It is kept, un-ignored, as a
    genuine regression pin on the WAL-at-commit property.
  - **D6b tier 2 — the power-loss tier, and THE specification.** Reconstruct the
    set of files that were actually `fsync`'d and boot from **only** those,
    asserting the acked `invoice` **and** every `invoice_line`. Was RED against
    `380ba8a` — that red was the spec — and is GREEN since D3.
    - The durable set is **derived, never declared**: a file joins it only if
      `Handle::fsynced_paths` (a journal appended to only on a *successful*
      `sync_all`) says production code `fsync`'d it. Deleting the `fsync`
      removes the WAL from the set and turns the tier red again, so the
      derivation *is* the mutation proof rather than a comment claiming one.
      Verified in both directions before landing: neutering `durable_ack`
      re-reds it, and `fsync`ing the main file while skipping the WAL also
      re-reds it — so the WAL specifically is load-bearing.
    - Two members are unconditional and each warrant is a cited property of
      other code: the mirror (`sync_mirror`'s `sync_all`, before every ack) and
      the main file (folded and closed at provisioning — the modelling
      concession that makes a red mean "the money write was lost" rather than
      "the tenant was never on disk").
    - Asserting `invoice_line` rows **alongside** the parent is what discharges
      §7.5's per-physical-row requirement for this path. It cannot arise here
      anyway — WAL replay is transaction-atomic, so a parent cannot survive
      without its children — but the assertion pins it rather than assuming it.
  - **D6b teeth control.** The same flow with an explicit `CHECKPOINT` + `fsync`
    on a hard-coded set: Option A reached by the other primitive. It stays as
    the independent control that tells a broken harness apart from a broken
    write path.
  - **Real-production-path tier.** `mark_invoice_paid::mark_paid` driven with
    nothing modelled, through the same power-loss durable set. Issuance itself
    stays modelled (the real route needs the OS keychain, an `MnbRatesProvider`
    and a seller profile, so no unattended test can drive it); this closes the
    "does production actually call it?" gap for one real ack.
  - Neither tier faithfully simulates power loss on macOS without root. **D6 is
    therefore labelled as testing process-crash durability plus a *derived*
    power-loss model, and R1's machine-restart clause must not be claimed as
    proven by it.** The harness cannot observe an `fsync`; it takes the write
    path's word for it. Closing that needs fault injection below the
    filesystem, which is not available here.
  - **D6 is wired into the cut-gate** (§7.9), in two halves: the tests are
    un-ignored so `cargo test --workspace --locked` blocks on them in `ci.yml`,
    and `tools/cut_gate_durable_ack.sh` (ENFORCING in `cut-gate.yml`, with
    negative probes) holds the money-path ack census closed in both directions.
    The static half is the only cover for the three ack sites no unattended test
    can reach — modification, storno, and the AP status change.

- **D7 — The WAL-truncation fence (R1). *(Added 2026-08-12 after incident
  00012. BUILT, and SHIPPING DARK — see D7.6.)*** D3 made the acked bytes
  durable; it did not make the ack HONEST. A foreign `Connection::open` on the tenant DB with DuckDB's DEFAULT
  pragmas — the ADR-0099 GROUP-A shape — folds and truncates the live Handle's
  WAL when it closes, and every subsequent Handle `commit()` returns `Ok` while
  reaching no file. D3's `durable_ack` could not see it **by construction**,
  because it `fsync`s PATHS: after the truncation `<db>.wal` is absent, so
  `if wal_path.exists()` SKIPPED it, the main-file `fsync` succeeded, and the
  ack returned `Ok(())`. A green durability light with nothing behind it, and
  every D3 gate green through it.
  1. **A watermark on the `Handle`** (`WalMark`), sampled under the writer lock
     at every `WriteGuard::drop` — right after the lockstep `sync_mirror`, so
     the sample is ordered against every other write. It carries a **monotone**
     byte high-water for `<db>.wal`, the `(dev,ino)` of the WAL and of the main
     file, and a single-shot `folded_by_us` escape hatch for a fold the Handle
     performs itself. Sampling at the drop and not only at the ack is what lets
     it see a truncation that happened BETWEEN two writes: the intervening
     commit re-creates a small, self-consistent WAL that an ack-time
     stat-and-compare would read as healthy.
  2. **A fence at the top of `durable_ack`.** WAL gone below a non-zero
     high-water, WAL shorter than the high-water, WAL inode changed, or main
     file inode changed → `DbError::WalTruncatedUnderWriter`. Line 439's
     `if wal_path.exists()` is no longer where the missing-WAL decision is
     made.
  3. **The by-path `fsync` residual (A2) closed.** `fsync_and_record` now
     `fstat`s the descriptor it opened and refuses to certify an inode the
     watermark does not recognise — the "`fsync` the wrong inode and report
     success" hole D3 left.
  4. **KEEP SERVING, not hard-stop** (Ervin, 2026-08-12). The breach latch is
     consumed as it is reported, so the app is not bricked and the next ack on
     a healthy tenant succeeds. What persists is a **sticky
     `Handle::durability_alert`** plus a `db.durability_loss_detected` audit row
     (its own `EventKind` — **not** `db.auto_recovered`, which means "we healed
     it"; nothing is healed here). The audit row rides the lockstep mirror
     sync, so the durable copy lands in the `fsync`'d mirror even when the DB
     copy does not.
  4a. **Keep-serving must not degrade to keep-serving-and-FORGET** *(B2, PR #61
     adversarial)*. As first built it did: the alert was process memory, the
     `db.durability_loss_detected` row went into the very database whose WAL had
     just been truncated, and so **the restart the banner tells the operator to
     perform was the mute button**. Two changes close it. `Handle::open`
     re-derives the alert at construction by scanning the surviving `fsync`'d
     audit mirror — the alert is up exactly while the newest
     `db.durability_loss_detected` out-ranks the newest
     `db.durability_alert_acknowledged`. And `POST
     /health/acknowledge-durability-alert` lets the operator clear it *without*
     a restart, appending the hash-chained acknowledgement FIRST and only then
     clearing the flag, so a failed append leaves the banner up. Clearing is now
     an attributable act rather than an absence, and it is durable across
     restarts. Re-derivation is deliberately **not** gated on D7.6's flag: a loss
     recorded while the fence was armed must survive a boot where it is not.
  4b. **The acknowledgement is DB-authoritative** *(R2-B1, round-2
     adversarial)*. B2 as first built read the mirror alone, which made the
     Acknowledge button **inert in the one scenario it exists for**. A real
     truncation regresses the DB head below the append-only mirror's, so
     `sync_mirror` answers `MirrorDivergent` and appends nothing — and
     `WriteGuard::drop` only `warn!`s, so the mirror is frozen *permanently*.
     The ack still committed to the DB and still returned 200, so the operator
     watched the banner drop; the next boot, reading the mirror alone, re-raised
     it. Forever. Re-derivation now consults **both** stores for what each is
     authoritative about — the mirror survives a truncation and holds the loss;
     the DB is what still accepts writes afterwards and holds the ack — and
     orders them by RFC3339 `time_wall`, because after the regression the two
     stores' `seq` spaces overlap. A tie keeps the banner up.
  4c. **A torn mirror tail must not blind the alarm** *(R2-B2)*. The
     re-derivation reads through `read_mirror_under_tail_policy` (the boot
     reconciler's reader), not the strict `read_mirror_entries`, which rejects
     an unterminated final line — the commonest crash artifact there is, and
     precisely the condition most likely to co-occur with a durability incident.
  4d. **Residual — CORRECTED, and much narrower than rev-1 of this bullet
     claimed** *(R3-N2, round-3 adversarial)*. The first write-up said a second
     loss after a frozen mirror leaves no durable trace, and pinned a test to
     that effect. Both were wrong, because they assumed the mirror stays frozen.
     It does not: serve's boot mirror-reconcile
     (`ensure_consistent_with_db`, run BEFORE the Handle opens) attempts a
     **gated auto-heal** on every boot and, on success, replays the DB up to the
     mirror head — which brings the DB level again and **un-freezes**
     `sync_mirror`. So the acknowledgement and every later loss row do reach the
     mirror, and a second loss DOES re-raise across a restart. That is now
     pinned positively by
     `a_second_loss_after_an_acknowledged_one_re_raises_across_a_restart`; the
     old test passed only because its harness skipped the reconcile, i.e. it
     pinned a state production never reaches.
     What genuinely remains is the window inside a **single process that never
     restarts between the two incidents** — and there the in-session banner is
     up the whole time, so the operator is not blind. **D5**
     (`MirrorDivergent` becomes a loud audited fault rather than a `warn!`) is
     still right on its own merits; it should not be re-prioritised on the
     strength of this residual, which is what the previous over-broad wording
     would have caused.
  4e. **"Keep serving" does not extend to the NEXT boot** *(R3-N3)*. The
     keep-serving decision (D7.4) governs the running process. Boot is governed
     by the pre-existing H1 preserve-and-refuse posture instead: if the gated
     auto-heal REFUSES — the chain fails the in-tx full genesis→head re-verify,
     as opposed to merely being short — `ensure_consistent_with_db` returns
     `MirrorAheadOfDb`, the ahead mirror is preserved to a side file, and
     **`serve` exits non-zero and does not boot**. Stated here because a D7
     reader arrives expecting "the app keeps running" and that promise stops at
     the process boundary: a tenant whose audit chain no longer verifies is
     refused, loudly, rather than served.
  5. **The operator channel.** The alert is surfaced on `GET /health` as
     `durability_alert` (always present, explicitly `null` when quiet) and the
     SPA renders a full-width, high-contrast red banner above the topbar,
     outside every `viewMode` branch, with no dismiss control. Deliberately
     off-palette — ADR-0017's ambient language is overridden for this one
     element, because the keep-serving decision above rests entirely on the
     operator seeing it.
  6. **The fence SHIPS DISARMED** (`HandleConfig::wal_fence_enabled`, default
     `false`) *(B1, PR #61 adversarial)*. Three foreign GROUP-A openers are
     still live in-serve — `calibration_overview_request`,
     `resolve_recipient_email`, `handle_quote_pipeline_status` — plus the
     CLI-against-live openers, and each truncates the WAL on close. With the
     fence armed *before* that sweep, opening the quote-calibration overview or
     emailing an invoice arms a breach, and the next issuance or mark-paid then
     fails its `durable_ack` — a failure that PROPAGATES, because the D3-C
     cut-gate enforces exactly that. A committed invoice would report as failed
     with its NAV handoff skipped. That is strictly worse than the silent bug
     being detected, so the detection lands dark and the flag flips in a
     one-line PR once **PR #3** has swept the openers. **The real loss-stopper
     is the sweep; D7 is the belt-and-suspenders alarm that is safe to arm
     after it.** Both flag states are pinned.
  - **Naming note.** This is D7, not D4: **D4 already means "boot must stop
    folding blind"** and remains unbuilt and H4-dependent. The two are
    unrelated — D4 is about the boot openers' pragma posture, D7 is about
    detecting a runtime truncation after the fact.
  - **Not a coverage claim.** The fence detects a truncation that has ALREADY
    happened; it does not prevent one. The prevention work is the GROUP-A
    opener sweep (`calibration_overview_request`, `resolve_recipient_email`,
    `handle_quote_pipeline_status` are still open) and D4.

---

## 6. Phased plan — **REORDERED TWICE**

The original draft ran Phase 1 = D1+D2+D6, Phase 2 = D4, Phase 3 = D3. The
adversarial found that ordering unsafe on two counts: D4-before-D3 removes the
only mechanism currently making prod durable (§2.3), and D2-before-D4 puts a large
materialisation transaction on a 16 MB-autocheckpoint connection (§7.4). Rev. 2
therefore ran D1+D6 → D3 → D4 → D2 → D5 → widening.

**Rev. 3 corrects it again: Phase 1 is D3, not D1.** *(See §12.3.)*

Rev. 2 put D1 first because it was "the cheapest correct answer available,
because it is already happening" (§4 Option C) — the ack would gate on the
mirror `fsync` that `WriteGuard::drop` already performs. The flaw is what the
mirror can give back. §2.4 and §7.2 establish, in this same document, that the
boot heal replays the mirror into `audit_ledger` **and nothing else**, and that
`InvoiceDraftCreated` is deliberately a *pointer* payload — three NOT NULL
columns (`customer_id`, `idempotency_key`, `series_id`) are not derivable, so a
materialiser **cannot legally INSERT into `invoice` at all**.

So gating the ack on the mirror would have made the ack honest about an event
the system still could not turn back into an invoice. **The mirror cannot
reconstruct an invoice row; only the row being durable can.** D1-first would
have shipped a stronger *promise* on top of the same missing *data* — precisely
the 2026-08-08 shape: a flawless ledger on frozen rows.

D3 inverts that. It makes the row itself durable, so the ack is backed by the
row rather than by a witness to it, and §7.7's remaining windows narrow without
D1's failure-mode inversion (a 5xx on an already-committed transaction). D1
becomes a **hardening step on top of a durable store**, not the durability
mechanism — and correspondingly less urgent.

Note this reordering does **not** disturb the adversarial's two findings: D3
still precedes D4, and D2 still follows D4.

### Phase 0 — Forensic instrumentation *(desirable, no longer blocking)*

At boot, before and after each opener, record `<db>.wal` byte size, `sha256(db)`,
`MAX(seq)` in `audit_ledger`, mirror head seq, and each fold's outcome. One
structured boot record. **No behaviour change.** Downgraded from prerequisite
because §2.7 is now resolved; still worth having so the *next* incident is not
diagnosed by inference.

### Phase 1 — D6 + D3 *(the proof, then the durable ack)* — **DONE 2026-08-09**

Order actually run, and the order to keep: **D6b red first — that red is the
specification** — then D3, then D6b green, then D6 into the cut-gate.

Shipped: `Handle::durable_ack` at five money-path acks (§5 D3); D6b tiers 1 and
2 plus the teeth control and a real-production-path tier, all un-ignored;
`tools/cut_gate_durable_ack.sh` ENFORCING in `cut-gate.yml` with negative
probes. Measured cost ≈1 ms marginal per issuance. **D1 has been removed from
Phase 1**
for the reason above; D2 was already removed by rev. 2.

### Phase 2 — H4 for real *(was "D3"; D3 no longer means this)*

The validated logical checkpoint — build-aside, validate, `atomic_install`,
marker, `checkpoint_enabled = true`. All primitives exist bar
`live_durable_checkpoint` itself; this is assembly plus a crash-injection gate
over the fold. **Still ahead of D4, because D4 still depends on it** — D3 did
not change that. Its remaining value after D3 is no longer *durability of the
money path* but two things D3 does not give:
  1. **bounding the runtime WAL**, which D3 leaves unbounded (§5 D3); and
  2. **covering the families D3 deliberately excludes** — inventory, work
     orders, the quote pipeline (§7.10).

### Phase 3 — D4 (boot stops folding blind)

One `open_boot_connection`, thirteen call sites, one explicit validated checkpoint,
one new cut-gate check + negative probes. **Must not precede Phase 2.**

### Phase 4 — D2 (business-state replay, quarantining)

Now safe: the heal connection has a sane pragma posture (Phase 3) and the DB has a
real checkpoint (Phase 2). Scope is the three kinds in §5, reconstructing into a
quarantine table, per-table idempotency, satisfiability assertions under test.

**Rev. 3: further demoted, and it should be re-justified before anyone builds
it.** D2 exists to put back rows that were lost. D3 stops the money-path rows
being lost, so the case for a second, divergent write path over those families
(§7.1's proven Σ-per-bucket vs single-rounding HUF divergence, §7.2's three
non-derivable NOT NULL columns) is now much weaker than it was on 2026-08-08.
An operator-facing reconcile screen may well dominate it. Do not start Phase 4
on rev. 2's motivation.

### Phase 5 — D5 (recovery-tool safety)

Runtime `MirrorDivergent` promotion, `snapshot now` precondition, audited mirror
re-baselining on restore.

### Phase 6 — Payload widening *(closes §2.5)*

Enumerate every business write not covered above and decide, per event, whether its
payload becomes self-contained or whether that family relies on H4 alone.
`InvoiceDraftCreated` is the first and clearest candidate.

---

## 7. How this could still fail — the ruthless section, with the adversarial's answers

### 7.1 The materialiser as a second, divergent write path — **CONFIRMED, with a concrete case**

Business rows would have two producers, and a *subtly wrong* replayed invoice is
worse than a missing one, because a missing one is loud.

**Concrete divergence, proven in this tree.** For a non-HUF invoice:

- The `invoice` row stores `huf_equivalent_total`, computed as a **single**
  round-half-even over the invoice gross:
  `huf_equivalent_round_half_even(gross_cents, rate)`
  (`modules/billing/src/domain/money.rs:346`).
- The NAV envelope's `<invoiceGrossAmountHUF>` is computed as a **Σ of per-VAT-
  bucket HUF roundings** (`apps/aberp/src/nav_xml.rs:1995–2035` — `inv_gross_huf`
  is accumulated per bucket, then emitted).

`Σ round(xᵢ) ≠ round(Σ xᵢ)` in general. On a multi-rate non-HUF invoice the two
differ by up to (buckets − 1) Ft. A materialiser deriving `huf_equivalent_total`
from `request_xml` therefore writes a value that can differ from the one the
original issuance stored **and printed on the PDF already in the customer's
hands** — and the envelope carries only the Σ-form, so the original value is not
recoverable from it. Silent, off-by-1-Ft, on a filed regulatory document.

Invoice 00010 from the incident is exactly this shape: EUR, MNB 366.40, gross
9 499.60 → HUF 3 480 653.

**Does "refuse on any non-enumerated event kind" catch it? No.** The refusal
predicate is over the *event kind*, and `InvoiceSubmissionAttempt` is enumerated.
The defect lives *inside* a whitelisted kind, in the derivation of one column. A
kind whitelist is orthogonal to payload-to-schema satisfiability.

**Resolution adopted:** D2 reconstructs into a **quarantine table**, never into
`invoice`. The tree already reached this conclusion independently —
`restored_invoice` (`restore_from_nav_outgoing.rs:193`) is precisely a
NAV-derived-invoice table kept deliberately separate. Plus the per-kind
satisfiability assertion in D2.

### 7.2 Is every `invoice` / `invoice_line` column derivable from `request_xml`? — **NO. Enumerated.**

This was the original draft's open question and the stated most-likely reason for
slippage. Answered against the schema at
`modules/billing/src/adapters/duckdb_store.rs:93–205`:

| Column | NOT NULL | Derivable from `<ManageInvoiceRequest>`? |
|---|---|---|
| `invoice.invoice_note` (PR-82) | no | **NO — provably.** The schema comment states it is "Recipient-facing only; **never emitted into the NAV InvoiceData XML**" (`:148`). |
| `invoice_line.note` (PR-82) | no | **NO — provably.** Same comment at `:192`. |
| `invoice.customer_id` | **YES** | **NO.** NAV carries buyer name / tax number / address; not ABERP's internal `partners` ULID. Requires a lookup that may not resolve. |
| `invoice.idempotency_key` | **YES, UNIQUE** | **NO.** Not in the envelope. The submission payload's `idempotency_key` is the *submission's*, not the issuance's. Fabricating one breaks dedupe for a later retry of the original request. |
| `invoice.series_id` | **YES** | **NO** (at best guessable by parsing the invoice number prefix). |
| `bank_account_id / _currency / _bank_name / _swift_bic` (PR-73) | no | **NO.** Not in NAV; and the read path explicitly "never fabricates one from current `seller.toml` state, since the regulatory record is 'the bank account the invoice was issued with'" (`:135–140`). |
| `exchange_rate_source`, `exchange_rate_date` | no | **NO.** NAV carries the numeric `<exchangeRate>`, not its MNB provenance or fixing date. |
| `email_recipient_override` (PR-203) | no | **NO.** Operator-typed. |
| `huf_equivalent_total` | no | **DIVERGENT** — see §7.1. |

**The attack lands.** Three NOT NULL columns (`customer_id`, `idempotency_key`,
`series_id`) are not derivable, so the materialiser **cannot legally INSERT into
`invoice` at all** without fabricating values, and two columns are *provably*
absent from the XML by the tree's own comments.

**Phase-1 scope must shrink, and does:** D2 no longer targets `invoice`. It
reconstructs the NAV-visible subset into a quarantine table — which is exactly
what `restored_invoice` already is, down to carrying no line-item table and having
needed two later migrations (PR-216, PR-217) to get a buyer label, with the buyer
staying NULL for third-party-submitted invoices.

### 7.3 `IncomingInvoiceStatusChanged` cannot construct its own row — **removed from Phase 1**

`ap_invoice` (`apps/aberp/src/incoming_invoices.rs:383–403`) requires NOT NULL
`supplier_tax_number`, `supplier_name`, `nav_invoice_number`, `issue_date`,
`total_net_minor`, `total_vat_minor`, `total_gross_minor`, `currency`. The payload
(`audit_payloads.rs:2232–2252`) carries **none of them** — only `ap_invoice_id`,
`idempotency_key`, `from_status`, `to_status`, `reason`.

The incident makes this concrete rather than theoretical. Two of the four lost
mark-paid rows were re-ingested by AP sync at 16:28 **under fresh ULIDs**
(`apinv_01KZG33G20H55JSCG5CF8J5K4Q` → `apinv_01KZH38Z5M9EHCQRSYE6N716E4`). So the
`ap_invoice_id` in the replayed payload names a row that **no longer exists**, and
the row that *does* represent the same NAV invoice has a different primary key.
The natural key is `UNIQUE (tenant_id, supplier_tax_number, nav_invoice_number)` —
none of which is in the payload.

A boot materialiser would therefore have to choose between: refusing (bricking
boot — R5), skipping (silent partial recovery — R3), or inserting a phantom row it
cannot populate (R7). There is also an unwinnable ordering problem: the heal runs
at boot, the re-ingest runs later, so the materialiser can never see the new row —
and an insert-if-absent under the old id would leave a phantom `Paid` row
alongside the real `Outstanding` one, **double-counting payables**.

**Removed from Phase 1.** Recovering AP status decisions needs either a
natural-key-carrying payload (Phase 6) or an operator-facing reconcile screen.

### 7.4 The heal transaction runs on a default-pragma connection — **worse than the draft knew**

`heal_replay_mirror_tail`'s own doc states it runs "on the plain reconcile
`Connection` BEFORE the shared `Handle` opens"
(`crates/audit-ledger/src/storage/mod.rs:775–780`). That connection is the bare
`Connection::open` at `serve.rs:1461` — **default pragmas, 16 MB
`wal_autocheckpoint`**.

So D2's "materialise inside the heal transaction" would place a large write
transaction on the one connection specifically configured to **fold in place at
16 MB** — the `duckdb#23046` tearing path, during recovery, on the file being
repaired. The original §7.4 worried about "a large transaction"; the real hazard is
a large transaction on a fold-happy connection.

**This is why D2 moves behind D4 in the revised phasing.**

### 7.5 Idempotency is asserted, not enforced — **and the stated rule has a hole**

`audit_ledger` has no `UNIQUE(seq)` and the business tables have no FKs by
cornerstone (ADR-0019 §3), so "insert-if-absent" is a read-then-write in
application code — the exact shape that produced the original fork. Safe under a
single-threaded boot; silently broken if the materialiser ever becomes concurrent.

**Additional hole found:** the draft's rule was "insert-if-absent keyed on the row
id already in the payload" — an *aggregate-level* key. A torn fold is not
table-aligned, so `invoice` can survive while its `invoice_line` rows are lost.
Keying on `invoice.id` then sees the parent present, skips, and leaves a
**zero-line invoice, permanently and silently** — a wrong row produced *by* the
idempotency rule. D2's revised constraint requires per-physical-row keying.

**On double-apply after mirror-fsync-success-then-crash-before-DB-write:** D2 only
runs when the mirror is strictly ahead, and inserts are absent-checked per row, so
the replay itself is idempotent under the corrected keying. The genuinely
unhandled direction is the *inverse* — DB ahead of mirror (crash between commit
and `sync_mirror`) — which `heal_from_mirror_ahead` does not cover and D1 narrows
but does not eliminate.

### 7.6 D6's SIGKILL spec is necessary but not sufficient — **corrected**

`SIGKILL` terminates the process but does **not** discard the OS page cache;
anything that reached `write(2)` survives. So a SIGKILL test distinguishes
"buffered in user space" from "handed to the kernel" — and **cannot** distinguish
"handed to the kernel" from "fsync'd". A system with zero fsync can pass D6 as
originally specified while remaining vulnerable to exactly the hard power-off that
§2.7 identifies as the best fit for 2026-08-08.

Hence the two-tier D6 in §5, and the explicit instruction not to claim R1's
machine-restart clause from it.

### 7.7 Does D1 close the window or narrow it? — **narrows, materially**

Under D1 the order is commit → `sync_mirror` (fsync) → ack. The window it closes:
acked-but-not-durable. The windows that remain:

- **Commit lands, crash before mirror fsync.** DB may or may not have it; mirror
  does not. No ack was returned, so no promise was broken — but if the DB *did*
  keep it, the mirror is now behind and the operator has an invoice the ledger
  never witnessed. Not covered by `heal_from_mirror_ahead`.
- **Ack-gating inverts the failure mode.** A mirror failure becomes a 5xx on a
  transaction that has *already committed to the DB*. The operator is told "failed"
  about a write that partly happened. More honest, not obviously safer; the
  reconciliation story needs designing, not discovering in prod.

### 7.8 Does D3 (H4) interact badly with the 13 boot openers? — **yes, and the draft's ordering made it worse**

> **Rev. 3 note.** Both interactions below are about a *fold*. D3 as shipped
> does not fold (§12.1), so **neither applies to it**: `atomic_install` is never
> called on the money path, the live file is never replaced, and the thirteen
> boot openers see byte-for-byte what they saw before — the WAL still replays
> and still folds at boot, exactly as at `380ba8a`. The snapshot path is
> likewise untouched: `take_snapshot` reads through `Handle::read()` and D3 adds
> no writer. This section stands **unchanged and still binding for H4**, which
> is Phase 2 and still unbuilt.

Two interactions:

1. **Ordering (fatal as originally phased).** Boot folding is currently the only
   thing making prod data durable (§2.3). D4 gives the boot openers the Handle's
   no-fold posture. If D4 lands before D3, boot stops folding and nothing starts
   checkpointing — **durability goes from "at every boot" to "never"**. The draft
   itself acknowledged the dependency inside D4.1 ("then run one explicit,
   validated H4 checkpoint") while scheduling D4 as Phase 2 and D3 as Phase 3.
   That is a circular phase dependency. Fixed in §6.
2. **Snapshot-forks-chain.** H4's `atomic_install` replaces the live file and
   `crash_safe.rs:150–152` deletes the target's `.wal` unconditionally. That is
   correct *for a validated build-aside* (the fold is already in the new file), but
   it means H4 must never run while any other connection holds the old instance —
   and at boot there are thirteen such openers in sequence. H4 at boot must be a
   single explicit call after the last of them, which is what D4.1 specifies and
   which is another reason the two must land together.

### 7.9 This ADR still adds no gate for its own property

§1's complaint is that we had no durability gate. D6 is one e2e. **One test is not
a gate; a gate is a check that fails a cut.** If Phase 1 ships without wiring D6
into the cut-gate, we repeat the exact mistake this document opens by naming. The
D4.2 opener check is the second half of that gate.

> **Rev. 3 — ANSWERED for D3.** Phase 1 did not ship without the gate. Two
> halves, both enforcing:
> 1. the D6b tiers are un-ignored, so `cargo test --workspace --locked` blocks
>    on the power-loss spec in `ci.yml`; and
> 2. `tools/cut_gate_durable_ack.sh` runs ENFORCING in `cut-gate.yml`, holding
>    the money-path ack census closed in **both** directions — a deleted
>    `durable_ack()` and an unregistered new one are equally red — with
>    `cut_gate_durable_ack_probes.sh` proving it has teeth.
>
> The static half is not redundant with the tests: it is the **only** cover for
> the three ack sites no unattended test can reach (modification, storno, AP
> status change — they need NAV credentials, a NAV envelope, and an ingested AP
> row respectively). D4.2's opener check is still unbuilt and is still the
> remaining half of the *boot-side* gate.

### 7.10 The kind set is chosen from one incident

Choosing scope from a single incident is how the previous hardening lane ended up
solving the wrong axis. The next loss will be in inventory movements, work orders,
or the quote pipeline — families with no self-contained payload at all. Phase 2
(D3/H4) is what actually covers them, which is a further argument for its
promotion.

> **Rev. 3 — this is the sharpest open risk, and D3 did not close it.** D3 is
> money-path only by deliberate choice (blast radius, latency), so inventory
> movements, work orders and the quote pipeline are **exactly as durable as they
> were on 2026-08-08**: WAL-resident until a boot happens to fold. The families
> named in this section are the ones D3 does not cover, and H4 — Phase 2, still
> unbuilt — remains the thing that would cover them.

---

## 8. Is DuckDB the right primary store? — this evidence does not say so

The proximate cause is that **we configured the engine never to become durable and
never built the replacement** (§2.2). A stub that logs "folding NOTHING this tick"
is not an engine defect; an identical outcome is reachable on SQLite with
`synchronous=OFF` and no checkpointing. Swapping engines without the contract in
§5 would move the bug, not fix it, and every phase of §6 is engine-agnostic.

Two honest qualifications:

- **One engine-level fact counts against DuckDB.** `duckdb#23046` — in-place
  checkpoint tearing — is the *reason* we disabled folding, and therefore the
  upstream cause of the whole chain of decisions that lost the data. SQLite's WAL
  has no analogous hazard. That is a real, specific, durability-relevant
  difference.
- **The one finding that should reopen this** is proof that DuckDB's WAL is not
  durable per commit on our version. §2.7 narrows to H-A but does not settle that
  sub-question. If Phase 0 settles it against DuckDB, Option C's premise hardens
  into a permanent architectural dependency and ADR-0107 moves from *preference*
  to *requirement* — reopened **there**, by superseding amendment, not here.

Until then: ADR-0107 and ADR-0108 stand exactly as written, unexecuted, DEV-scoped.

---

## 9. Consequences

- The audit mirror becomes load-bearing for the write path, not merely
  evidentiary. If the mirror file cannot be written, money-path writes fail. That
  is the intended trade (fail loud > lose silently) but it is a genuine new
  coupling and should be stated in FOUNDATION.md. **Rev. 3: this is a
  consequence of D1, which has not shipped.** D3 did not make the mirror
  load-bearing — it made the *primary store* durable, which is the opposite
  direction of coupling and is why §6 now runs D3 before D1.
- Money-path write latency gains one `fsync` (~24 ms by analogy with the Editions
  MES measurement; **unmeasured on this tree — measure before accepting D1**).
  **Rev. 3: measured for D3.** ≈1 ms MARGINAL per acked issuance (12.11 ms
  with, 11.00 ms without; 20 rounds, release build on the dev Mac). Marginal on
  top of the `F_FULLFSYNC` the ack already pays inside `WriteGuard::drop`'s
  `sync_mirror` — not the cost of a device flush from cold. The ~24 ms estimate
  was an order of magnitude high for this workload. R6 is satisfied with room;
  the figure is not operator-visible.
- **A money-path `fsync` failure is now a 5xx on an already-committed
  transaction** (D3, five sites). This is §7.7's inverted failure mode arriving
  a phase early: the operator is told "failed" about a write that did land in
  the DB. It is the intended trade (R3 / rule 11: fail loud beats lose
  silently), and it is strictly better than D1's version of the same inversion
  because the row *is* durable — only our promise about it failed. The
  reconciliation story §7.7 asks for is still undesigned.
- Boot becomes slower and stricter. Every new refusal is a new way to be unable to
  start; R5 bounds that and Phase 5 is not optional.
- The cut-gate grows a check whose predicate is *durability*, not *chain
  consistency* — the first of its kind in the tree.
- **Reconstruction is explicitly partial.** §7.2 means a replayed invoice is the
  NAV-visible subset in a quarantine table, not a restored `invoice` row. This must
  be said plainly to the operator, in the UI, at the moment it happens.

---

## 10. Answers to the original §10 open questions

1. **Is §7.2 fatal to Phase 1's scope?** Yes, as originally scoped. Three NOT NULL
   columns are non-derivable and two more are provably absent from the NAV XML.
   D2 is rescoped to a quarantine table and moved out of Phase 1.
2. **Reconstruct-and-serve or reconstruct-and-quarantine?** **Quarantine**, on the
   §7.1 evidence and on the `restored_invoice` precedent.
3. **Materialiser inside the heal transaction?** Yes for atomicity — but only after
   D4 fixes the reconcile connection's pragmas (§7.4).
4. **D1 in a new `Handle` method, or abandon `WriteGuard::drop`?** New method.
   `Drop` cannot fail a committed transaction, and keeping it best-effort for
   unmigrated paths is what makes D1 incremental.
5. **Is D4 safe to schedule before Phase 0 reports?** Yes — §2.7 resolved the
   hypothesis question, so Phase 0 no longer gates anything. But D4 is **not** safe
   to schedule before **D3**, which is the ordering the draft got wrong.

## 11. Changes made by the adversarial review

1. §2.7 resolved: evidence supports **H-A**; **H-B refuted** by the clean ART index.
2. §2.3 reclassified: verified, but a *latent* hazard and the current *de-facto*
   durability mechanism — not this incident's cause.
3. §7.2 answered by enumeration: D2 cannot INSERT into `invoice`. Scope shrunk to a
   quarantine table.
4. §7.1 answered with a tree-proven divergence (Σ-per-bucket vs single-rounding
   HUF). The kind whitelist does **not** catch it; satisfiability assertions added.
5. `IncomingInvoiceStatusChanged` removed from the Phase-1 kind set (§7.3).
6. Idempotency keying corrected from aggregate-level to per-physical-row (§7.5).
7. D6 split into process-crash and on-disk tiers; R1's power-loss clause explicitly
   not claimed from it (§7.6).
8. **Phases reordered**: D1+D6 → D3 → D4 → D2 → D5 → widening. The draft's
   D4-before-D3 ordering would have made durability strictly worse (§7.8).
9. Phase 0 downgraded from prerequisite to desirable.
10. R7 added (no silently-wrong reconstructed rows).

## 12. Changes made by rev. 3 (D3 implementation, 2026-08-09)

Rev. 2 was a design document that authorised nothing. Rev. 3 records what
building D3 proved, including the two places rev. 2 was wrong.

### 12.1 D3 is Option B (`fsync` the WAL), not H4 (fold it) — §5 D3 rewritten

Rev. 2's D3 read "Implement H4 (Option B)", conflating two different things: §4
Option B is the *decision to make the ack durable*, H4 is *one mechanism* for
it. Rev. 2 assumed the mechanism had to be the fold, on the unstated premise
that a row is only durable once it reaches the main file.

That premise is false, and the D6b byte-copy tier disproves it empirically:
DuckDB writes WAL records at commit and replays them on open, so `fsync`ing
`<db>.wal` makes the acked rows durable with no fold at all. Folding per ack
would have reinstated the in-place `duckdb#23046` path §4 Option A rejects,
rewritten the main file on every invoice, and required building
`live_durable_checkpoint`, which does not exist in this tree.

Consequences carried into §5 D3: **H4 remains unbuilt**, the runtime WAL is
durable but **unbounded**, **D4 still depends on H4** rather than on D3, and
durability is **money-path only** so §7.10's warning is unaddressed.

### 12.2 §5 D6's tier spec was wrong about which tier is the spec — corrected

Rev. 2 specified D6b as "copy the on-disk byte state and boot from that copy"
and expected it RED. **It is GREEN, and was green before any fix** — a file copy
reads through the OS page cache, so it can never distinguish "reached the file"
from "reached stable storage". §7.6 had already made exactly this argument
against `SIGKILL`; rev. 2 did not notice it applies verbatim to a byte copy.

The real specification is the **power-loss tier**: boot from *only* the files
that were actually `fsync`'d. That tier was RED at `380ba8a`, that red was the
Phase-1 spec, and D3 turned it GREEN. §5 D6 now carries all three tiers with
what each can and cannot prove, and the byte-copy tier is kept as a regression
pin rather than promoted as evidence it cannot supply.

D6a (`SIGKILL`) is recorded as **not built and not planned**: §7.6 already
proves a zero-`fsync` system passes it, so it would add apparent rigour and no
information.

### 12.3 §6 Phase 1 is D3, not D1 — the mirror cannot reconstruct an invoice row

Rev. 2 put D1 (gate the ack on the already-`fsync`'d mirror) first as "the
cheapest correct answer, because it is already happening". But §2.4 and §7.2 of
this same document establish that the mirror replays into `audit_ledger` only,
and that `InvoiceDraftCreated` is a deliberate *pointer* payload with three
non-derivable NOT NULL columns — so no amount of mirror durability puts an
`invoice` row back.

D1-first would therefore have shipped a stronger promise over the same missing
data: the 2026-08-08 shape exactly, a flawless ledger on frozen rows. D3 makes
the row itself durable, so the ack is backed by the row rather than by a witness
to it. D1 survives as a hardening step on a durable store, materially less
urgent. The adversarial's two ordering findings are undisturbed: D3 before D4,
D2 after D4.

### 12.4 Residuals — flagged, not fixed

1. **`F_FULLFSYNC` on macOS — WITHDRAWN. This was wrong, in the conservative
   direction.** Rev. 3 first listed as a residual that `File::sync_all` is
   `fsync(2)` and therefore does not force the device's own write cache. It
   does. On Apple targets the pinned 1.97.0 stdlib routes `sync_all` →
   `inner.fsync()` → `fcntl(fd, F_FULLFSYNC)` (`std/src/fs.rs`,
   `std/src/sys/fs/unix.rs` under `#[cfg(target_vendor = "apple")]`) — the
   device-cache flush, not the weak one. The measured cost is consistent with
   that and not with a plain `fsync`: a real flush on this APFS/NVMe is
   milliseconds, which is exactly what the numbers show.

   So D3 **is** power-loss durable on the internal disk, which is the failure
   §2.7 identifies as the best fit for 2026-08-08 — the very hazard this
   residual claimed was still open. Understating a durability guarantee is a
   real defect: it invites someone to "fix" it by adding a second,
   platform-forked primitive that is already there, and it misprices the risk
   of every decision downstream.

   **The residual that actually remains** is one layer lower: the guarantee
   bottoms out at the drive honouring the flush. Apple guarantees that for the
   internal NVMe. A third-party external enclosure may acknowledge
   `F_FULLFSYNC` without flushing, so a tenant on external storage is outside
   what D3 can promise. Nothing in software closes that.
2. **The D6b harness cannot observe an `fsync`.** *(Narrowed — the worst case
   is now covered. See §12.5.)* It derives the durable set
   from `Handle::fsynced_paths`, i.e. it takes the write path's word that
   `sync_all` succeeded. That is strictly better than rev. 2's hard-coded list
   — delete the `fsync` and the file leaves the set — but it is not
   fault injection below the filesystem, which is what would settle it. **R1's
   machine-restart clause is still not proven.**
3. **§7.7's reconciliation story is still undesigned**, and D3 has made one of
   its cases live (see §9).
4. **The DB-ahead-of-mirror direction remains uncovered** (§7.5). D3 narrows the
   window — the DB `fsync` follows the mirror `fsync`, so a crash between them
   leaves the mirror ahead, which `heal_from_mirror_ahead` handles — but a crash
   between `commit` and the guard drop still leaves the DB ahead, and nothing
   heals that.

### 12.5 What the PR #59 adversarial pass changed (2026-08-09, verdict: merge-after-fixes)

Three defects, one of them serious. All three are fixed on the PR branch.

**F1 — deleting the `sync_all` was caught by nothing.** The durability evidence
had a circular hole: [`power_loss_durable_set`] derives its set from
`Handle::fsynced_paths`, and the journal is written by the same function that is
supposed to do the syncing. Mutate `fsync_and_record` to journal the path
**without** syncing it and the result is:

| gate | verdict under the mutation |
|---|---|
| D6b tier 1 / tier 2 / teeth / real-mark-paid | **4/4 pass** |
| `cut_gate_durable_ack.sh` | **PASSED** |
| `clippy -D warnings`, `cargo fmt` | **clean** |

— i.e. a total, silent revert to the 2026-08-08 loss with every gate green.
Reproduced here before fixing, not taken on trust.

Closed by `crates/aberp-db/tests/durable_ack_fault_injection.rs`: break the
filesystem reach (delete the main DB file out from under an open `Handle`) and
require `durable_ack` to return `Err`. Only code that really opens and syncs the
path can notice, so the mutation goes RED — verified in both directions. This
narrows residual §12.4.2: the harness still cannot *observe* an `fsync`, but the
worst case it was blind to — a reach that never happens at all — is now covered.

**F2 — the cut-gate counted a CALL, not a PROPAGATION.** Rewriting
`db.durable_ack().context(..)?` as
`if let Err(e) = db.durable_ack() { warn!(..) }` — the exact R3 / rule-11
downgrade this document forbids by name — left CHECK D3-A and D3-B green,
because the call is still there and still censused. Closed by **CHECK D3-C**,
which requires a `?` terminator within three lines of every censused call site,
with probes P7/P8 pinning that it fires on the swallowed site and *only* on it
(1 swallowed / 4 propagate).

**F3 — the `F_FULLFSYNC` residual was wrong, conservatively.** See §12.4.1: on
macOS `sync_all` *is* the device flush. Six places asserted otherwise and are
corrected. Understating a durability guarantee is a real defect, not a harmless
excess of caution — it invites a "fix" that re-implements what is already there,
and it misprices every downstream decision.

**Not changed, deliberately:** the 5xx-on-an-already-committed-transaction
behaviour (§9, §7.7). It is the intended R3 trade and it predates this branch.
Its operational consequence is now written down — see §12.6 — because it is
sharper than §7.7 implied.

### 12.6 Operator note — retrying a failed issuance DOUBLE-ISSUES

If a money-path ack fails **after** its transaction committed (a `durable_ack`
error, or any other post-commit error), the operator sees a 5xx for a write that
did land. What happens on a retry differs by path, and the difference matters:

- **Issuance, modification, storno — NOT retry-safe.** Each mints a **fresh**
  `IdempotencyKey` server-side per invocation (`build_command`
  `issue_invoice.rs:1611`, `build_modification_command` `:1179`,
  `build_storno_command` `:1524`); the wire body cannot supply one. So the retry
  does not match the first attempt's key, `allocate_in_tx` returns `Fresh`
  rather than `Replay`, and the retry **issues a second invoice and burns a
  second NAV number**. The first one already exists.
- **`mark_paid` — safe.** Its no-double-pay gate keys on `invoice_id` (an
  `InvoicePaymentRecorded` lookup via `payment_record_for`), not on the
  idempotency key, so the retry returns `AlreadyPaid`.
- **`change_status` — safe.** `from_parsed == to_status` short-circuits to a
  no-op before any write.

This is pre-existing behaviour that D3 neither introduces nor worsens; D3 only
adds one more (rare) way to reach the post-commit error branch. Recording it
because the correct operator response to a failed issuance is **check whether
the invoice exists before retrying**, and nothing in the UI says so today.
Closing it properly means an operator-supplied or request-derived idempotency
key on the issue routes — a separate change, out of D3's scope.

## 13. D8 — the GROUP-A opener sweep, and the CLI-opener class it does NOT close (2026-08-12)

D7 shipped the WAL-truncation fence **disarmed**, because arming it while a
foreign opener still folds the WAL turns a silent durability bug into a routine
money-path outage. D8 is the sweep that was supposed to make arming it safe. It
closes **one of the two** opener classes, and this section exists so nobody
reads "the sweep landed" as "the fence can be armed".

### 13.1 What D8 closed — GROUP A is empty

> **Read §13.3 first.** The first cut of D8 claimed this and was wrong: three
> post-Handle openers survived inside `serve::run`. They are migrated now and
> the claim has been re-verified by independent grep, but the *reason* it was
> wrong is the part worth carrying forward.

Every in-serve GROUP-A opener — the eleven of the first pass plus the three the
adversarial found — now routes through the shared `aberp_db::Handle`.
`tools/adr0099_read_fork_structural_baseline.txt` ratchets **31 → 20** and the
per-opener fingerprint census **79 → 62**: no in-serve request route and no
in-serve daemon opens its own connection to the tenant DB.
The remaining 20 are GROUP B (separate-process CLI) and GROUP C (not the
serve-held path at all); see the file for the per-family record.

Two findings from doing it are worth keeping:

- **`reports::compute_financial_report` held four openers, and the census saw
  one.** Two `Connection::open`, a `DuckDbBillingStore::open`, and a
  `Ledger::open`. Opening the Financial Report — or the workshop dashboard tile
  that reuses it for its one-day window — was on its own enough to fold the
  WAL. A shape scanner keyed on `Connection::open` is structurally blind to a
  typed store's constructor; the census undercounted for that reason, not
  because anyone hid anything.
- **The MES ledger writer proved rule 14 rather than asserting it.** Its
  end-to-end test polled the appended row back through a fresh
  `Connection::open`. The moment the write rode the Handle, that poller read
  zero *forever* — checkpointing is disabled under H3, so the row is
  WAL-resident and invisible to a second instance. The test could not stay as
  it was; migrating writers without their readers is not a style preference.

### 13.2 What D8 did NOT close — the CLI-against-live class (N7)

A CLI one-shot is a **separate OS process**. It cannot borrow serve's
in-process `Handle`, so the D8 fix does not apply to it and forcing it would be
wrong. These were scoped instead, and the scoping produced a **better result
than expected for most of them and one genuine hole**:

**`aberp serve` holds the F-E whole-DB writer flock for its entire process
lifetime** (`serve.rs:911`). Any CLI that acquires that flock is therefore
*refused* while serve is up. Checked per command, and the ordering is what
matters — the flock must be taken **before** the first DB open, or the
open/close pair folds the WAL before the refusal is ever reached:

| command | flock line | first DB open | verdict |
|---|---|---|---|
| `drain-submission-queue` | 121 | 161 | refused before any open — **safe** |
| `drain-pending-retries` | 175 | 215 | refused before any open — **safe** |
| `export-invoice-bundle` | 1233 | 1243 | refused before any open — **safe** |
| `recover-from-nav` | 161 | 188 | refused before any open — **safe** |
| `mark-abandoned` | 113 | 156 | refused before any open — **safe** |

So for the four commands the PR #61 adversarial named, **run-against-a-live-
serve is not a real operator path**: it is structurally refused, and refused
early enough that the tenant DB file is never touched. No fix is owed, and none
should be invented — adding a read-only pragma to a command that cannot run
would be cargo-cult.

**The residual is the openers that hold NO flock.** The ADR-0099 baseline
already flags exactly two, and they are not equivalent:

- **`print_invoice.rs|render_to_bytes` — not a fence hazard.** It opens
  `aberp_db::Handle::open_default`, which issues `PRAGMA
  disable_checkpoint_on_shutdown` + `wal_autocheckpoint='1TB'`
  (`aberp-db/src/lib.rs::open_runtime_connection`). Its close therefore does
  **not** fold the WAL. It remains a stale-*read* hazard (a second instance
  does not replay the live writer's WAL — that is the E1 defect), but it cannot
  sabotage the D7 fence. Serve itself does not reach it: the in-serve path uses
  the connection-taking `render_to_bytes_on_conn`.
- **`rebuild-stock-cache` — THE genuine hole. CLOSED by D9 (2026-08-13); §14
  records what was built and answers the deferral below on its own terms.**
  As diagnosed here: `crates/aberp-inventory/src/bin/rebuild_stock_cache.rs:61`
  opens a bare `duckdb::Connection::open` with DEFAULT pragmas and holds no
  flock, so its close folds and truncates a live serve's WAL. It is also a
  *documented operator recovery path* — ADR-0061 §3 tells the operator to run
  `cargo run -- rebuild-stock-cache` when the cache disagrees with
  `SUM(qty_delta)` — i.e. precisely something an operator does *while the shop
  is running*. With the fence armed, that recovery command would make the next
  invoice issuance hard-fail.

  It is *already* unsafe for a second reason the flock exists to prevent: it
  **writes** `products.stock_qty` with no cross-process mutual exclusion
  against serve.

**Recommended fix (NOT implemented in D8 — deliberately deferred):** give it
`db_writer_lock::acquire_or_refuse` before its first open, exactly as the
twenty sibling mutating CLIs do. Three lines, twenty precedents, and it makes
the command refuse rather than corrupt.

It is deferred rather than done because it is **outside D8's goal** (D8 routes
*in-serve* openers through the Handle) and because it is an operator-visible
behaviour change: a recovery command that runs today would start refusing while
serve is up. That is the *correct* behaviour, but an operator hitting it
mid-incident deserves the change to arrive with its own review, release note,
and a refusal message that tells them to stop serve first. Picking the
conservative fork and flagging it, per the working agreement.

The alternative — a read-only / no-checkpoint-on-close pragma — is **not**
recommended here: the command is a writer, so read-only is wrong, and pragmas
alone would leave the two-writer window open while fixing only the WAL symptom.

### 13.3 Gate on arming the fence — **CORRECTED after the D8 adversarial**

The first version of this section said "✅ GROUP A empty" with
`rebuild-stock-cache` as the only owed item. **That was wrong**, and it was the
most dangerous thing in the document, because it is the artifact someone would
read before flipping the flag.

Three foreign openers were still live inside `serve::run` *after* the shared
Handle opens (~:1633): the cad-blob key-provision audit write (~:2390), the
pricing-jobs index DDL (~:2432), and the pricing-jobs boot row count (~:2487).
The last ran on **every boot**, after the Handle had already written. Measured
against that shape with the fence armed: WAL 54107 → 0, and `durable_ack`
returned `Err(WalTruncatedUnderWriter { WalVanished })`. So arming the fence on
that head would have fired on the **first money-path `durable_ack` of every
serve session** — not an edge case, the common path.

They hid because the read-fork baseline collapses all of `serve::run` into one
line and filed it under GROUP C with the justification *"provision_atomic, at
boot, BEFORE the shared Handle is opened"*. That sentence is true of the first
half of a function that spans the Handle open and continues for hundreds of
lines past it. **A whole-function exemption granted on a property only part of
the function has** — the same per-fn granularity blindness that hid three of
`compute_financial_report`'s four openers. All three are now migrated onto
`st.db`, and the baseline entry has been re-triaged into an explicit
*line-region* exemption that names `open_tenant_handle` as the boundary and
points at the per-opener fingerprint census as the authoritative detector.

The corrected gate:

1. ✅ GROUP A empty — **re-verified 2026-08-12 by independent tree-wide grep**,
   not by the census alone. Every `Connection::open` / `Ledger::open` /
   `DuckDbBillingStore::open` / `Handle::open_default` in `apps modules crates`
   (minus `/tests/`) was enumerated and classified. The only in-serve openers
   that remain are in `serve::run` at :1206–:1461 and
   `record_upgrade_snapshot_mismatch_audit` (called at :1127) — all strictly
   **before** `open_tenant_handle` at :1633 — plus the demo/new-tenant openers,
   which target a different DB file. `ap_sync.rs:1417` and the ~70 other
   `Handle::open_default` hits are inside `#[cfg(test)]` modules.
2. ✅ `rebuild-stock-cache` flock-fenced — **done 2026-08-13 (D9, §14)**. It
   was the LAST opener in the tree that could fold a live serve's WAL, so with
   it fenced the GROUP-B live-fold hazard is closed and this gate's substantive
   items are both green.
3. ❌ A re-run adversarial over the corrected D8 **+ D9**.

Arming it before (2) would have reproduced exactly the failure mode D7's B1 test
describes, with a different command as the trigger. With (2) done, the flag flip
is a **separate PR** and is deliberately not bundled with the fix that unblocked
it: a fence armed in the same commit as its own precondition leaves (3) nothing
to check. The B1 test now pins that ORDER rather than an open hazard.

**Method note, worth more than the fix.** Both D8 misses — the three
`serve::run` openers and three of `compute_financial_report`'s four — were
*per-function* views of a *per-opener* problem. Where a function-keyed artifact
and an opener-keyed artifact disagree, the opener-keyed one wins; and "GROUP A
is empty" is a claim that must be re-derived from the tree by grep, never read
off a governance file that a previous change was supposed to have updated.

### 13.4 D8's second miss — DDL one call-frame from a `read()` (adversarial F3)

ADR-0108 R-1 forbids DDL on a `Handle::read()` connection: the try_clone is
writable, but it is released from the writer mutex the instant it is taken, so
DDL through it escapes the single-writer invariant the audit chain, the
invoice-number allocator and the stock cache all rest on. It is pinned tree-wide
by `apps/aberp/tests/no_ddl_on_read_handle.rs`.

**The D8 sweep broke that rule at six sites, and the pin stayed green.** The
pin's scan is scope-LOCAL — it flags `ensure_schema(&conn)` written beside the
`let conn = …read()` binding. Every one of the six put the DDL one call-frame
away:

| migrated path | callee that issues the DDL |
|---|---|
| `calibration_overview_request` | `quote_calibration::calibration_overview` |
| `handle_quote_pipeline_status` | `count_recent_daemon_panics` → **audit** `ensure_schema` |
| `handle_list_email_relay_queue` | `email_relay_queue::list_rows` |
| `handle_get_email_relay_row` | `email_relay_queue::read_row` |
| `prepare_rerender` | `quote_pricing_jobs::get_effective_lead_time_days` |
| `resolve_recipient_email` | `partners::get_partner` / `find_partner_by_tax_number` |

The second row is the sharp one: the audit `ensure_schema` reaches
`migrate_drop_unique_art_if_present`, which on a legacy table performs a `DROP` +
`CREATE` + bulk re-`INSERT` of the entire `audit_ledger`. A whole-table rebuild
issued from a mutex-free clone, concurrently with a real writer on the same
instance, is not something anyone designed. It is unreachable today only because
serve's boot migrates the audit schema before `open_tenant_handle` — a
precondition, not a guard, and one nobody had written down.

All six now take `db.write()` — R-1's own second remedy ("or take a
`Handle::write()` for it"). The cost is that these routes serialize behind the
writer mutex; they are low-frequency operator screens on a single-operator ERP,
where `aberp-db` already documents write serialization as an accepted throughput
ceiling. `compute_financial_report` deliberately stays on `read()`: its four
SQL-aggregate callees issue no DDL (verified), so it scopes a brief `write()`
for its bootstrap and reads the rest concurrently — and a test pins that
asymmetry, so "put everything on `write()`" cannot become the rule by drift.

**The alternative not taken**, and why: hoisting `ensure_schema` out of those
six callees (the R-1 remedy applied to `incoming_invoices`) would preserve read
concurrency, but those helpers have CLI callers that rely on the ensure, so it
is a broader change than D8's scope and belongs in its own PR.

`apps/aberp/tests/adr0110_d8_reader_schema_ddl.rs` pins the two halves R-1
structurally cannot: that the callees really do issue DDL, and that these entry
points really do take `write()`. Mutation-verified — flip one back to `read()`
and it reds with the site named, while R-1 stays green.

**The pattern across all three D8 misses.** F1 (per-function census entry hiding
per-opener facts), F2 (a fixture that pre-created the schema hiding a dropped
bootstrap), and F3 (a scope-local scan hiding a cross-frame call) are one
failure: *a detector whose granularity is coarser than the thing it detects*.
Each was green while wrong. When a gate's unit of analysis is a function, a
file, or a lexical scope, ask what it looks like one level down before believing
it.

### 13.5 The F3 fix's own cost, paid back (re-adversarial, non-blocking)

F3 put six reader paths on `db.write()`. That was correct for R-1, but two of
the six paid for it in a way the fix did not account for. Both are now narrowed.
Neither weakens R-1: in both cases every DDL-reaching call still runs under the
writer.

**`prepare_rerender` held the process-wide writer across a PDF render and an
fsync.** Widening it to "one writer for the whole body" swept in
`aberp_quote_pdf::render` (pure CPU) and `crate::fs::write_atomic`, which calls
`sync_all` — a real fsync. Measured ~14.5ms of guard hold, blocking a concurrent
invoice-issue `db.write()` for ~13.2ms; and `poll_once` drains the queue
*sequentially* through `process_one`, so an N-quote drain is N such windows
back-to-back. On production storage it is worse than the microbenchmark says,
because that fsync competes with `durable_ack`'s WAL fsync and the audit-mirror
fsync for the same device — the money path's own durability, slowed by a PDF.

The last DB use is `get_effective_lead_time_days`. The guard is now dropped
immediately after it, so render and fsync run unlocked. Both DDL-reaching calls
stay above the drop.

**The pin had to be relaxed, not just the code.** The F3 test asserted
`prepare_rerender` "must take `db.write()` for its whole body" — which would have
made this fix red. That is a pin cementing an implementation instead of
forbidding a defect. It now asserts the actual invariant: both DDL-reaching calls
happen under the writer, no `read()` clone is held, and the explicit `drop`
precedes the render/fsync tail. Mutation-verified in both directions — deleting
the `drop` reds it (the throughput regression returns) and swapping `write()` for
`read()` reds it (the R-1 defect returns).

*General form, worth more than the instance:* a pin written as "the code looks
like X" blocks the next legitimate improvement. Write it as "the defect cannot
occur" and the fix passes while the regression still fails.

**`resolve_recipient_email` took the writer on a tokio runtime worker.** It is
the one F3 site called bare from an `async fn` — the other five already sit
inside `spawn_blocking` — so it began taking the process-wide writer mutex on an
async worker and holding it across a transaction plus two partner queries. On
the auto-email-after-issue path, a contended writer parks that worker.

Moved onto `spawn_blocking`, which required taking `(&Handle, &str tenant)`
instead of `&AppState` so the call is movable. The alternative — narrowing the
hold — was rejected on inspection rather than preference: the partner lookups
*are* the DDL-reaching calls, so they must sit under the writer either way;
narrowing would have shortened the block without removing blocking-from-async,
while `spawn_blocking` removes it outright and matches what the other five sites
already do. A panic in the blocking task now lands on the same
"no recipient → audit the skip, banner the operator" branch a lookup error takes,
so it can never be mistaken for a successful send.

Also pinned: `partners::get_partner` and `find_partner_by_tax_number` were the
one DDL premise asserted in prose but never tested, unlike their five siblings.
They are in the half-1 pin now, mutation-verified.

## 14. D9 — the last CLI-against-live opener is fenced (2026-08-13)

§13.2 named `rebuild-stock-cache` as the one genuine remaining hole and
deliberately deferred it. This is that deferral, paid.

### 14.1 What was wrong

`crates/aberp-inventory/src/bin/rebuild_stock_cache.rs` opened the tenant DB
with a bare `duckdb::Connection::open` — DuckDB DEFAULT pragmas — and held no
flock. A default-pragma close checkpoints and TRUNCATES the WAL. Run against a
live `aberp serve`, its exit folded away every commit serve had made since the
last checkpoint while `commit()` kept returning `Ok`: the D7 write-loss
primitive, verbatim.

What made it the acute one is not the code, it is the documentation. ADR-0061 §3
tells the operator, in those words, that when `products.stock_qty` disagrees with
`SUM(qty_delta)` the recovery is to run this binary. So the tree shipped a
documented instruction to run a WAL-folding process against a running shop, at
exactly the moment an operator is already dealing with an inconsistency.

It was also, independently, a second unsynchronised WRITER of `products.stock_qty`.

### 14.2 The fix — the flock, and only the flock

`aberp_db::db_writer_lock::acquire_or_refuse(&db_path, &tenant,
"rebuild-stock-cache")` before the first open, bound to a named `_guard` that
lives to the end of `run` and is declared *before* `conn` so the drop order is
conn-then-guard (the DB closes while this process still owns the tenant). This
is the same call, in the same position, as the twenty sibling mutating CLIs —
§13.2's own recommendation, unmodified.

**The pragma alternative was considered and NOT taken**, per §13.2's reasoning
and re-verified here. Switching the opener to `Handle::open_default` (the
`disable_checkpoint_on_shutdown` path, what `print_invoice::render_to_bytes`
uses) would work mechanically — `WriteGuard` derefs to `&mut Connection`, so
`rebuild_stock_cache_for_tenant` runs unchanged — but it fixes the *symptom* and
leaves the *cause*: two processes writing one tenant's stock cache with no mutual
exclusion, which is the app-invariant class ADR-0108 M6 says this flock exists
for. It would also make a recovery tool acquire a Handle's full runtime
machinery to do a single-transaction repair. The flock refuses; that is the
correct outcome, and it is one call. Defence-in-depth on the pragma is a legible
follow-up, not a substitute, and it is worth strictly less now that the command
cannot run against a live serve at all.

**Operator-visible behaviour change, as §13.2 promised it would be:**
`rebuild-stock-cache` now REFUSES while `aberp serve` is running, with the F-E
message that names the single-writer rule and tells the operator to stop the
other writer and retry. Previously it ran and silently ate serve's unflushed
commits. Release note owed.

### 14.3 Where the flock lives now, and why it moved

`db_writer_lock` shipped in `apps/aberp/src/db_writer_lock.rs` while every caller
was an `aberp` subcommand. `rebuild-stock-cache` is not: it is a binary of
`crates/aberp-inventory`, which cannot depend on `apps/aberp`.

The fork was one shared module versus a second copy of `lock_path_for`, and a
second copy is much the worse hazard: two derivations that drift by one
character produce two lock files, and two lock files are no lock at all,
silently — with nothing red anywhere. So the module moved down to `aberp-db`,
the crate that already owns "one writer per tenant DB", and
`apps/aberp/src/db_writer_lock.rs` became a re-export. Every existing
`crate::db_writer_lock::…` call site, `aberp::db_writer_lock::…` test, and
doc/ADR reference to that path is unchanged. One API change: the fallible
surface returns a typed `DbWriterLockError` instead of `anyhow::Error`
(`aberp-db` is a library crate, ADR-0021 Part A) — `?` into an `anyhow::Result`
is identical at every call site, and the refusal `Display` body, including the
`single-writer` substring the F-E refusal tests match on, is carried over
verbatim.

`tools/cut_gate_read_fork.sh`'s `is_flock_fenced()` greps the *call token*
(`acquire_or_refuse|try_acquire`), not the module path, so the move is invisible
to it and the fenced binary now satisfies it.

### 14.4 The pin

`crates/aberp-inventory/tests/rebuild_stock_cache_flock.rs` drives the REAL
binary as a separate OS process (the flock is a cross-process primitive; an
in-process call could not prove it) against a DB whose cache is deliberately
drifted from its ledger. One corruption, two runs: **locked** → non-zero exit,
stderr cites the single-writer rule, cache untouched; **free** → exit 0, cache
re-derived to `SUM(qty_delta)`.

Mutation-verified in both directions. Removing the `acquire_or_refuse` makes the
locked arm exit 0 and rewrite the cache — red, and that run is also the empirical
proof that nothing else was stopping it (DuckDB's own file locking does not
refuse the second opener). The free arm is what stops the refusal assertions from
passing on a merely broken binary.

### 14.5 What this closes, and what it does not

Closed: the GROUP-B **live-fold** hazard. Every DB-mutating CLI one-shot now
takes the flock before opening, so none can fold a live serve's WAL — the last
precondition on ADR-0110 §13.3's arming gate.

Not closed, and knowingly out of scope:

- **`print_invoice::render_to_bytes` still holds no flock.** It is not a fold
  hazard (§13.2: it opens `Handle::open_default`, so its close cannot fold), and
  so it is not on the arming gate. It remains a stale-*read* debt — a second
  instance does not replay the live writer's WAL, so a just-issued invoice can
  render as absent. Loud, not silent. Recorded in the read-fork baseline with
  that triage; closing it is either a flock or a migration, in its own change.
- **`wal_fence_enabled` is NOT flipped here.** Separate PR, deliberately: a fence
  armed in the same commit as its own precondition leaves §13.3 item 3's re-run
  adversarial nothing to check.
- **Moving `rebuild_stock_cache.rs|run` to the read-fork allow-list.** Fencing
  EARNS that move; it does not perform it. The baseline header reserves it as a
  deliberate follow-up and it stays one.
