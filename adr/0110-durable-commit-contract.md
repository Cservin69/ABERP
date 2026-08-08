# ADR-0110 — The durable-commit contract and business-state boot replay

- **Status:** Proposed — **adversarially reviewed 2026-08-08.** Verdict: *sound after
  changes*. The changes are listed in §11 and are folded into §5/§6/§7 below.
- **Date:** 2026-08-08 (rev. 2, post-adversarial)
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
- **Authorises:** nothing to be built. This document is a design decision only.
  No runtime code, schema, or prod touch is authorised by it.

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

- **D3 — Implement H4 (Option B) as the durability floor.** Replace the
  `run_durable_checkpoint_locked` stub with the real validated build-aside +
  `atomic_install` + marker, and default `checkpoint_enabled = true`.

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

- **D6 — Prove it, don't assert it. *(Spec corrected — §7.6.)*** A crash-injection
  e2e in two tiers:
  - **D6a — process-crash tier.** `SIGKILL` at the ack boundary, restart, assert
    the **`invoice` row** (not the ledger row) is present. Catches user-space
    buffering. Must fail against `380ba8a`.
  - **D6b — power-loss tier.** At the ack boundary, copy the on-disk byte state
    (`aberp.duckdb`, `aberp.duckdb.wal`, `aberp.duckdb.audit.log`) into a fresh
    tenant directory **and boot from that copy**, asserting the same row. This
    isolates "what is on disk" from "what the live process holds", which SIGKILL
    alone cannot do.
  - Neither tier faithfully simulates power loss on macOS without root. **D6 must
    therefore be labelled as testing process-crash durability, and R1's
    machine-restart clause must not be claimed as proven by it.**
  - **D6 must be wired into the cut-gate**, not left as a lone e2e (§7.9).

---

## 6. Phased plan — **REORDERED**

The original draft ran Phase 1 = D1+D2+D6, Phase 2 = D4, Phase 3 = D3. The
adversarial found that ordering unsafe on two counts: D4-before-D3 removes the
only mechanism currently making prod durable (§2.3), and D2-before-D4 puts a large
materialisation transaction on a 16 MB-autocheckpoint connection (§7.4).

### Phase 0 — Forensic instrumentation *(desirable, no longer blocking)*

At boot, before and after each opener, record `<db>.wal` byte size, `sha256(db)`,
`MAX(seq)` in `audit_ledger`, mirror head seq, and each fold's outcome. One
structured boot record. **No behaviour change.** Downgraded from prerequisite
because §2.7 is now resolved; still worth having so the *next* incident is not
diagnosed by inference.

### Phase 1 — D1 + D6 *(the ack gate and its proof)*

Ships first and alone. D1 is correct under every hypothesis, needs no schema
change, no new write path, and closes the acknowledged-write window using a step
that already runs. Order: **D6a/D6b red first — that red is the specification** —
then D1, then D6 green, then D6 into the cut-gate.

**This is the revised recommended Phase 1.** D2 has been removed from it.

### Phase 2 — D3 (H4 for real)

The validated logical checkpoint, `checkpoint_enabled = true`. All primitives
exist; this is assembly plus a crash-injection gate over the fold itself. Promoted
ahead of D4 because D4 depends on it.

### Phase 3 — D4 (boot stops folding blind)

One `open_boot_connection`, thirteen call sites, one explicit validated checkpoint,
one new cut-gate check + negative probes. **Must not precede Phase 2.**

### Phase 4 — D2 (business-state replay, quarantining)

Now safe: the heal connection has a sane pragma posture (Phase 3) and the DB has a
real checkpoint (Phase 2). Scope is the three kinds in §5, reconstructing into a
quarantine table, per-table idempotency, satisfiability assertions under test.

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

### 7.10 The kind set is chosen from one incident

Choosing scope from a single incident is how the previous hardening lane ended up
solving the wrong axis. The next loss will be in inventory movements, work orders,
or the quote pipeline — families with no self-contained payload at all. Phase 2
(D3/H4) is what actually covers them, which is a further argument for its
promotion.

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
  coupling and should be stated in FOUNDATION.md.
- Money-path write latency gains one `fsync` (~24 ms by analogy with the Editions
  MES measurement; **unmeasured on this tree — measure before accepting D1**).
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
