# S332 / PR-31 — DuckDB ART crash on the email-outbox audit-write path

**Date:** 2026-06-10 · **Branch:** `session-332/pr-31-duckdb-art-crash-email-outbox`
**Reported against:** PROD_v2.27.14 (S329) · **Status:** diagnosed; **conservative
call — NO `audit_ledger` schema change shipped.** Regression harness added.

> **TL;DR for dispatch:** the brief's prescribed fix — "identify and DROP the
> offending secondary index, mirroring S288" — is **structurally inapplicable**
> to this crash. The `audit_ledger` table has no droppable secondary index; its
> only ART indexes are the inline `UNIQUE (seq)` / `UNIQUE (id)` integrity
> constraints, which (a) are not visible in `duckdb_indexes()` and have no
> droppable name, and (b) are load-bearing for the tamper-evident hash chain.
> Dropping `UNIQUE(seq)` (only possible via a full crown-jewel-table rebuild)
> would swap a **contained, caught-and-logged** audit-write error for a **silent
> integrity hole**. On a mis-premised, locally-unreproducible crash that is the
> wrong trade. I did **not** make that change. See "Recommended durable fix".

---

## 1. The crash (verbatim, from Ervin's console)

```
duckdb::FixedSizeBuffer::GetOffset → FixedSizeAllocator::New → Prefix::New →
ARTOperator::InsertIntoNode → ARTOperator::Insert →
ART::InsertKeys → ART::Insert → ART::Append → BoundIndex::Append →
DataTable::AppendToIndexes → LocalTableStorage::AppendToIndexes →
LocalStorage::Flush → LocalStorage::Commit → WriteToWAL → Commit → ...
This error signals an assertion failure within DuckDB.
Error code 1: Unknown error code kind=quote.email_outbox_fetched
```

## 2. Source of the write — confirmed

`apps/aberp/src/email_outbox_poll_daemon.rs::write_audit` (line ~1018) is the
only ABERP-side persistent write the daemon makes. The daemon keeps **no state
table** of its own — the storefront directory layout is the state machine
(module docs, lines 22–25). So the crashing insert is unambiguously into
`audit_ledger`, and the `kind=quote.email_outbox_fetched` token is the
structured-log field emitted on the caught error (line ~1038):

```rust
Ok(Err(e)) => tracing::error!(error = ?e, kind = %kind_label,
    "email-outbox audit write failed"),
```

Since S311 (F13/F18), `EmailOutboxFetched` fires on **every** poll cycle —
success **and** idle (`fetched_count: 0`) **and** errored — i.e. one audit row
every ~5 s, ~17k rows/day, making it by far the **highest-frequency** audit
producer. That is why this specific `kind` is the one that surfaces the ART bug.

## 3. The audit-ledger indexes — every one of them

`crates/audit-ledger/src/storage/schema.rs`:

```sql
CREATE TABLE IF NOT EXISTS audit_ledger ( ... ,
    UNIQUE (seq),          -- schema.rs:36
    UNIQUE (id)            -- schema.rs:37
);
```

There are **no `CREATE INDEX` statements** anywhere on `audit_ledger`. The only
ART indexes are the two inline `UNIQUE` constraints. Decisive evidence (probe
run during this investigation):

```
SELECT * FROM duckdb_indexes() WHERE table_name = 'audit_ledger';
→ TOTAL_INDEXES = 0
```

DuckDB does **not** list inline-constraint indexes in `duckdb_indexes()`, and
gives them no user-addressable name. The S288 mechanic —
`SELECT COUNT(*) FROM duckdb_indexes() WHERE index_name = '…'` then
`DROP INDEX IF EXISTS '…'` — therefore has **nothing to detect and nothing to
drop** here. It is not "the same family fixed the same way"; it is a different
table with a different (un-droppable, integrity-critical) index shape.

## 4. Which index is the ART culprit

The crash signature `Prefix::New → FixedSizeAllocator::New` is the ART
prefix-compression allocator. The worst case for it is a **monotonic key with
long shared prefixes** — which is exactly `seq` (a strictly increasing `BIGINT`;
big-endian keys 1,2,3,… share 7-byte prefixes, forcing deep prefix-node churn).
`id` is a high-entropy ULID (`aud_<random base32>`), a shallow, wide tree. So the
deterministic read of the crash stack points at the **`UNIQUE(seq)` ART index**.

That is the integrity-critical one:

* `append_in_tx` reads the chain head inside the tx, computes `seq = head+1`,
  and relies on `UNIQUE(seq)` to reject a racing second writer that read the
  same head — the **hash-chain fork guard** across audit producers (pricing
  daemon, relay daemon, invoice issuance, this daemon all append to the same
  per-tenant ledger). schema.rs:4 names `UNIQUE(seq)`/`UNIQUE(id)` "the
  integrity invariants".
* Removing it is only possible by **rebuilding the table without the
  constraint** (DuckDB 1.1.x cannot `ALTER … DROP` an unnamed inline UNIQUE).
  A rebuild of the crown-jewel ledger is high-risk, and the result is a ledger
  whose fork protection is now enforced only at `verify_chain` time (detection,
  not prevention) — a strict downgrade of the ADR-0008 guarantee.

## 5. Impact is contained — not an outage

`write_audit` runs the insert in `spawn_blocking` and **catches** the `Result`
(lines ~1035–1042): on `Err` it logs and the cycle continues. In a **release**
build (how PROD is cut), DuckDB's internal assertion is raised as a throwable
`InternalException` surfaced through the C API as `Error code 1` — i.e. a
catchable `Err`, **not** a process `abort()`. Consequences:

* **Email delivery is unaffected** — claim/send/writeback all complete; only the
  *audit row* for the cycle is lost.
* The "hot loop" Ervin sees is the **per-cycle error log repeating every ~5 s**,
  not a process crash/restart loop. (The supervisor's panic path is not even
  entered for a caught `Err`.)

This materially lowers the urgency for risky emergency schema surgery: the
failure is loud, bounded, and non-data-destructive today.

## 6. Reproduction attempts (DuckDB 1.1.x, lockfile `duckdb 1.10502.0`)

Harness mimicked `write_audit` byte-for-byte (fresh `Connection::open` +
`ensure_schema` + 1-row tx + `commit` per row, **file-backed** so the WAL /
checkpoint path is real):

| Mode | N | Result |
|---|---|---|
| reopen-per-write (faithful) | 100 | no crash |
| reopen-per-write (faithful) | ~tens of thousands | no crash (killed; **O(n²)** — see below) |
| single persistent conn | ~hundreds of thousands (19 min) | no crash (killed) |

**Could not reproduce the prod `InternalException` locally at any scale tried.**
The prod trigger needs prod-specific state (a far larger accumulated ledger
and/or the live on-disk ART image). A blind schema change justified only by an
unreproduced crash, on the tamper-evident ledger, is not a conservative move.

### Side-finding (real, worth a follow-up): O(n²) checkpoint cost
The faithful repro is CPU-bound and **super-linear**: because `write_audit`
opens a *new* connection per row, DuckDB **checkpoints the whole `audit_ledger`
ART index to disk on every single audit write**. As the ledger grows, each ~5 s
write re-serializes an ever-larger ART — O(table) per write, O(n²) over time.
On a large prod ledger this is both a latency problem **and** the most plausible
real-world trigger for the ART allocator bug (it stresses exactly the
serialize-to-WAL path in the crash stack).

## 7. What this PR ships

* **Regression harness** `apps/aberp/tests/s332_email_outbox_audit_write_no_crash.rs`
  — `s332_regression_email_outbox_fetched_audit_write_does_not_crash`. Writes N
  back-to-back `EmailOutboxFetched` rows in the daemon's exact reopen-per-write
  shape against a file-backed DB, and asserts (a) panic/Err-free, (b)
  `verify_chain` confirms all N entries with dense, monotonic `seq` — the exact
  integrity guarantee a "drop `UNIQUE(seq)`" fix would have broken. Honest
  docstring: it does **not** reproduce the prod ART crash at unit scale; it pins
  the invariant we *can* assert and is the harness a prod-state-seeded repro
  would extend (`S332_N` raises the count).
* **Supervisor unchanged** (per brief).
* **No `audit_ledger` schema change.** (Conservative call — section 4/5/8.)

## 8. Recommended durable fix (OUTSIDE this PR's frozen scope)

The brief froze the event-emit path and the schema. The *correct* durable fixes
both live in that frozen scope, so they are flagged here for an explicit
follow-up decision rather than smuggled in:

1. **Stop emitting `EmailOutboxFetched` on idle cycles.** The S311 F13/F18
   "emit every cycle even when `fetched_count == 0`" choice is what drives ~17k
   immutable rows/day into the monotonic-`seq` ART. Emit only on a non-empty
   fetch, a state change, or an error. This removes ~99% of the write volume and
   the ART pressure **without touching integrity**. (Preserves the F18 intent —
   token-rotation 401s still emit, because those are the error path.)
2. **Don't reopen + checkpoint the whole ledger per write.** Hold a persistent
   (or pooled) audit connection in the daemon so a single idle write doesn't
   re-serialize the entire ART. Kills the O(n²) cost in §6.
3. **If a schema change is ever truly required**, it must be a *tested table
   rebuild that PRESERVES* `UNIQUE(seq)`/`UNIQUE(id)` (regenerating the ART from
   a clean image to recover a corrupted on-disk index), **never** a constraint
   drop. Gate it behind an index-health probe so it runs once, not every boot.

## 9. Operator action requested from Ervin

* Confirm the **exact DuckDB version** on the PROD binary (`duckdb --version` /
  the cut's `Cargo.lock`) — a version skew vs our `1.10502.0` would change the
  ART analysis.
* If safe, capture a **copy of the live `audit_ledger` DuckDB file** so the
  crash can be reproduced offline against real state and the §8 fix verified
  against the actual trigger rather than a synthetic one.
