# ADR-0030 — audit-ledger mirror file — `<db>.audit.log` carries a per-entry JSON-Lines copy of the chain, written post-commit + fsync'd, consumed by the per-invoice bundle reader as second-source assertion; closes finding F10 and clears the session-6 sharpening review-bar named in ADR-0029 §5

- **Status:** Accepted
- **Date:** 2026-05-22
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR — first PR after
  ADR-0029 / PR-16 to extend the audit-ledger crate's surface
  (PR-16 was consumer-only). Lifts F10 additively: the bundle
  reader's manifest fields
  `mirror_file_present` and `mirror_file_status` flip from the
  deferred-string values to the live values without changing
  the bundle's shape. Load-bearing deltas: §1 (mirror file
  path + format — JSON-Lines per entry, same shape
  `chain.jsonl` uses), §2 (write-time hook — post-commit
  `sync_mirror` from the binary path; the audit-ledger crate
  does NOT couple the mirror to `append_in_tx` because the
  mirror must reflect committed state only), §3 (recovery
  posture on partial writes — fail loud on next append per
  CLAUDE.md rule 12; new `AppendError::MirrorCorrupt` /
  `MirrorDivergent` / `MirrorIo` variants), §4 (read-time
  surface — `read_mirror_entries` for the bundle reader;
  agreement check at the `entry_hash` level), §5 (bundle
  reader's additive manifest flip — `mirror_file_present`
  becomes `true` when the mirror is present; `mirror_file_status`
  becomes `"verified-agreement"` / `"divergence-detected"` /
  `"absent-pre-pr-17"`), §6 (per-tenant lock posture — matches
  DuckDB single-writer file-lock; cloud multi-writer per
  ADR-0016 deferred unchanged), §7 (bootstrap — implicit
  one-time backfill when the mirror file is absent on a non-
  empty DB; INFO-level log line names the backfill loud). Does
  **not** supersede ADR-0008 §"Storage", ADR-0029, or any
  prior ADR; all remain in force.
- **Related:**
  - **ADR-0008 §"Storage"** — the parent posture: "the ledger
    is also mirrored to an append-only file
    (`<tenant>.audit.log`) outside the DB on every commit,
    fsync'd. This protects against DB-file corruption and
    gives us a second-source artifact for export." ADR-0030
    realises this posture; the convention shifts from
    `<tenant>.audit.log` (implied per-tenant) to
    `<db_path>.audit.log` (literal suffix on the actual DB
    file path), which is operationally identical because
    ADR-0002 names one DB file per tenant.
  - **ADR-0008 §"Adversarial review" bullet 1** — "The DB
    file is editable; what stops an attacker from rewriting
    both the DB and the mirror file? — Nothing locally, by
    themselves. The attestation checkpoints raise the bar."
    The mirror's value is unintentional-corruption recovery,
    not anti-tamper. The intentional-tamper trust anchor is
    F5 / ADR-0016 (deferred). ADR-0030 honours this scope
    — the mirror is "best-effort secondary evidence" per
    ADR-0008's own framing.
  - **ADR-0029 §5** — the deferral predicate this ADR lifts:
    "PR-17's lift extends the bundle reader additively — the
    reader, on detecting the mirror file at the conventional
    path `<db>.audit.log`, reads it, asserts agreement against
    the DB-sourced entries, and reports
    `mirror_file_status: \"verified-agreement\"` (or
    `\"divergence-detected\"` if the mirror diverges, with the
    bundle output refused per CLAUDE.md rule 12)." ADR-0030
    holds the deferral predicate verbatim.
  - **ADR-0019** — storage strategy. The mirror is a side-
    artifact, NOT a second source-of-truth: the DB stays
    canonical, the mirror is recoverable from the DB but the
    DB is not recoverable from the mirror.
  - **ADR-0016** — cloud multi-writer attestation. Out-of-
    scope for ADR-0030; per-tenant lock is the local-desktop
    single-writer file-lock today, same posture DuckDB uses
    for the DB file itself.
  - **Session 6 fortnightly sharpening on F10** — "F10 must
    land before per-invoice export PR is reviewed for merge."
    ADR-0029 §5 surfaced this honestly as Reading-B rejected
    at PR-16 time and named PR-17 as the deferred-clearance
    PR. ADR-0030 is that clearance — Ervin's review of this
    ADR + PR-17 IS the session-6 sharpening's review-bar.
- **Source material:** ADR-0008 §"Storage" (the parent
  posture), ADR-0029 §5 (the deferral predicate), session-20
  handoff (the pre-PR-17 housekeeping list).

## Context

After ADR-0029 / PR-16 landed the per-invoice export bundle
reader, the loudest active deferred-gate string in the
codebase is `mirror_file_status: "deferred-per-f10"` —
emitted in every bundle's `manifest.json`. The session-6
fortnightly sharpening explicitly named this PR as the
clearance step: "F10 must land before per-invoice export PR is
reviewed for merge." ADR-0029 §5 deferred it honestly with the
gap NAMED LOUD in the manifest and committed the clearance to
this PR.

PR-17 lifts F10. The mirror file becomes a real on-disk artifact
that the bundle reader consults as second-source assertion. The
bundle's existing shape is unchanged — the manifest fields flip
additively, the bundle's chain verification is still the
primary integrity check, and the mirror's value (per ADR-0008's
own framing) is "best-effort secondary evidence" against
unintentional DB-file corruption.

### Prerequisite-gate state at PR-17 time

- **ADR-0009 §8** — CLOSED at the audit-evidence-bundle level
  by ADR-0029 / PR-16. The remaining §8 surfaces are F5
  (attestation signing), F38 (bundle verifier tool), F10 (THIS
  ADR), and F36 (parsed `receiver_state` field, NAV-testbed
  gated).
- **ADR-0008 §"Storage"** — the parent posture exists; PR-17
  realises the "mirrored to an append-only file outside the
  DB on every commit, fsync'd" sentence.
- **ADR-0008 §"Adversarial review" bullet 1** — the mirror is
  not anti-tamper; intentional-attack defence is F5 +
  ADR-0016. PR-17 stays inside the unintentional-corruption
  scope.
- **F10** — open since the session-6 fortnightly review;
  deferred at PR-16 per ADR-0029 §5; lifted here.
- **F12 four-edit ritual** — NOT fired by this PR. The mirror
  writer adds error variants (`AppendError::MirrorCorrupt`,
  `MirrorDivergent`, `MirrorIo`) but no new `EventKind`. The
  mirror records the same kinds the DB records; the ritual
  remains at its ninth landing.

### Surfaced conflicts (CLAUDE.md rule 7)

Three ambiguities the build-phase will otherwise paper over:

1. **Where the mirror append happens — inside `append_in_tx`
   or post-commit at the binary path.** Two readings:

   - **Reading A: Inside `append_in_tx`, before the tx
     commit.** The mirror is written as part of the same
     atomic-ish unit as the DB write. Rejected — the tx may
     roll back (e.g., the binary's `run_single_tx` returns
     `Err(_)` between two `append_in_tx` calls and the tx
     drops), but the mirror is `O_APPEND`-written to the OS
     and cannot be rolled back. A normal-path rollback would
     create permanent divergence; the rollback conformance
     tests in `apps/aberp/tests/rollback_conformance.rs`
     exercise this exact path and would flake.

   - **Reading B: Post-commit at the binary path, via an
     explicit `sync_mirror(conn, meta, mirror_path)` call
     after `tx.commit()`** (this ADR's pick). The mirror
     reflects committed state only. Each command that drives
     `append_in_tx` + `tx.commit()` adds ONE line after the
     commit: `audit_ledger::sync_mirror(&conn, &meta,
     &mirror_path).context("sync mirror after commit")?;`.
     The mirror writer reads the mirror's head, reads the DB
     entries with seq > mirror_head, and appends them.

   PR-17 commits to **Reading B**. The cost is ~13 call-site
   touches (one extra line per command that appends); the
   benefit is structural correctness against rollback.

2. **Mirror format — JSON-Lines per entry, or canonical CBOR
   per entry, or the chain-hash CBOR shape.** Two readings:

   - **Reading A: Canonical CBOR (the chain-hash shape).**
     Identical bytes to what the entry_hash was computed
     over. The most "verifiable" shape. Rejected — operator
     tooling for `.jsonl` is universal (`jq`, `grep`, any
     editor); CBOR tooling is niche. ADR-0008 §"Export"
     itself names verifiability as a property of the chain
     hashes embedded in each entry, not of the on-disk
     format. The mirror's value is operator-readable
     second-source, not a redundant hash anchor.

   - **Reading B: JSON-Lines per entry, same shape
     `chain.jsonl` uses inside the bundle** (this ADR's
     pick). Hashes hex-encoded; payload bytes base64-
     encoded; one entry per line; UTF-8; newline-terminated.
     The bundle reader's mirror-file consumption path is
     SYMMETRIC with the DB-sourced consumption path — both
     produce a `Vec<MirrorEntry>` that maps 1:1 with `Vec<Entry>`
     by `seq` and `entry_hash`. Session-20 handoff already
     called this "the cheapest pick."

   PR-17 commits to **Reading B**. The
   `ChainJsonlEntry`-shaped line lives in the audit-ledger
   crate (single source of truth) and is reused by the
   bundle reader at read time.

3. **Backward compatibility with pre-PR-17 DBs.** A DB that
   existed before PR-17 has DB entries but NO mirror file.
   Three readings:

   - **Reading A: Refuse to operate; require operator to run
     a separate `aberp audit-mirror-init` command.** Most
     fail-loud; requires operator action. Rejected — every
     command's first post-PR-17 invocation would break.

   - **Reading B: Implicit one-time backfill on first
     `sync_mirror` call when mirror is absent, LOUDLY
     logged at INFO level** (this ADR's pick). The
     `sync_mirror` writer detects the missing file,
     creates it, and appends every DB entry from seq=1
     onward in one pass. The post-condition is identical
     to "operator ran `audit-mirror-init` first." The INFO
     line names `audit_mirror_initialized` with the entry
     count so the operator sees the one-time event in the
     command's output. Subsequent calls follow the normal
     `sync_mirror` incremental path. CLAUDE.md rule 12 is
     honoured by the LOUD log line — silent backfill is
     the rejected mode.

   - **Reading C: Pre-PR-17-DB compatibility marker — the
     bundle reader special-cases "no mirror file"** as a
     legitimate state distinct from "mirror file present
     but missing entries." Partially adopted: the bundle
     reader's `mirror_file_status` distinguishes
     `"absent-pre-pr-17"` (no file) from
     `"divergence-detected"` (file present but disagrees);
     see §5 below. The `sync_mirror` writer's posture is
     Reading B's implicit backfill — the absent-file state
     only persists until the next command appends.

   PR-17 commits to **Reading B + Reading C's marker** in
   combination. The implicit backfill is the write-side
   compatibility path; the
   `absent-pre-pr-17` marker is the bundle reader's
   read-side compatibility path for the brief window between
   a DB being upgraded to a PR-17 binary and the next
   command appending.

## Decision

### 1. Mirror file path and format

**Path convention:** `<db_path>.audit.log`. The literal string
`.audit.log` is appended to the full DB file path. For tenant
DB `t-1.duckdb`, the mirror is `t-1.duckdb.audit.log`. ADR-0008
§"Storage" named the file as `<tenant>.audit.log`; the literal-
suffix convention is operationally identical (ADR-0002 names
one DB file per tenant) and avoids a separate path-resolution
surface.

**Format:** UTF-8 JSON-Lines (RFC 7464 §2.2, NL-terminated).
One JSON object per entry; one entry per line; lines are
seq-ordered ascending. The object shape MUST match
`ChainJsonlEntry` (the shape `bundle/chain.jsonl` uses) field-
for-field. Hashes are hex-encoded; `payload` bytes are base64-
encoded; `actor` is the typed serde-roundtrip JSON shape.

**Why one format, used in two places.** Per CLAUDE.md rule 7
(surface conflicts, don't average them) and ADR-0021 §12 (the
canonical encoding lives in one place): the mirror line and
the `chain.jsonl` line are bit-identical. The audit-ledger
crate owns the encoder; the bundle reader's
`ChainJsonlEntry::from_entry` becomes a call into the crate's
shared serializer. Two formats would be the CLAUDE.md rule 7
violation; one format with two consumers is the surgical-
change posture.

### 2. Write-time hook — `sync_mirror` post-commit

**New function in `crates/audit-ledger/src/storage/`:**

```rust
pub fn sync_mirror(
    conn: &Connection,
    meta: &LedgerMeta,
    mirror_path: &Path,
) -> Result<u64, AppendError>
```

Returns the new mirror head seq after sync. Reads the mirror
file's last line (if present); if absent, treats head as 0
and runs the implicit backfill (§7 below). Reads DB entries
with `seq > mirror_head`; for each, appends one JSON-Lines
line + fsyncs. Returns the new head seq.

**Why a free function, not a method on `Ledger`.** Per the
existing crate posture (storage/mod.rs §"Cross-crate
transactional appends (PR-6)"), `Ledger::append` is the
trait-style wrapper that owns its own tx; `append_in_tx` is
the free function the binary path uses to share a tx with
`aberp-billing`. The mirror sync's caller-side ergonomics
match the binary path: callers already own `conn` post-
commit; threading a `Ledger` re-construct just for sync is
ceremony per CLAUDE.md rule 2. The `Ledger::append` trait-
style wrapper does NOT auto-sync the mirror — it has no path
context (tests use `:memory:` and have no mirror).
Production callers all use `append_in_tx`; they call
`sync_mirror` explicitly post-commit.

**Caller-side change at the binary path** (one extra line per
command):

```rust
tx.commit().context("...")?;
audit_ledger::sync_mirror(&conn, &meta, &mirror_path)
    .context("sync audit-ledger mirror file after commit")?;
```

The `mirror_path` is derived once at the top of each command
via `audit_ledger::mirror_path_for(&args.db)` — a public
helper that returns `db_path.with_extension(...)` with the
`.audit.log` suffix appended.

### 3. Recovery posture on partial writes — fail loud, refuse next append

**Pre-append integrity check.** Before each `sync_mirror`
call appends ANY new line, the writer reads the mirror's
last line via a tailing read. Two failure modes:

- **Mirror file non-empty but last line lacks a trailing
  newline.** Indicates an interrupted prior write. Return
  `Err(AppendError::MirrorCorrupt { reason: "last line not
  newline-terminated — prior write interrupted" })`. The
  operator's recovery is to inspect the file, truncate the
  partial line, and re-run.
- **Mirror file last line parses as JSON but its `seq` or
  `entry_hash` does NOT match the corresponding DB entry's
  `seq` / `entry_hash`.** Indicates divergence between the
  mirror and the DB. Return
  `Err(AppendError::MirrorDivergent { seq, reason })`.
  Operator recovery: investigate (was the mirror tampered
  with? Was the DB tampered with?), repair the inconsistency,
  re-run.
- **Mirror file I/O error during append or fsync.** Return
  `Err(AppendError::MirrorIo { source })` wrapping the
  `std::io::Error`. The DB has the entry; the mirror does
  not. Operator recovery: investigate disk space /
  permissions / FS readiness, re-run (the next `sync_mirror`
  call catches up the mirror).

**The DB-committed entry is NOT rolled back** when a mirror
write fails. This is per ADR-0008 §"Adversarial review"
bullet 1's framing: the mirror is "best-effort secondary
evidence, not primary." The DB stays canonical. The fail-
loud posture (refuse the next append until the operator
investigates) is the CLAUDE.md rule 12 honour — silent
catch-up after a mirror divergence is the silent-omission
failure mode.

**Why fail-loud-on-next-append instead of fail-loud-now-but-
rollback-DB.** Rolling back a committed DB tx means the
audit chain LOSES the entry that already landed. That's a
worse outcome than mirror divergence: an operator can recover
the mirror from the DB (the §7 backfill path), but cannot
recover a DB entry that was rolled back. ADR-0008 §"Storage"
already states: "Entries are written in the same transaction
as the state change they describe." The mirror is OUTSIDE
that transaction by design (it's a second-source for export,
not the source of truth). Rolling back to keep mirror parity
would violate ADR-0008's own framing.

### 4. Read-time surface — `read_mirror_entries`

**New function in `crates/audit-ledger/src/storage/`:**

```rust
pub fn read_mirror_entries(
    mirror_path: &Path,
) -> Result<Vec<MirrorEntry>, AppendError>
```

Reads the entire mirror file line-by-line; decodes each line
as a `MirrorEntry`; returns the seq-ordered vector. Returns
`Err(AppendError::MirrorCorrupt { reason })` if any line
fails to parse, if seqs are not strictly ascending from 1,
or if duplicate seqs appear. Empty file = empty vector;
absent file = `Err(AppendError::MirrorIo { source: NotFound })`.

`MirrorEntry` mirrors `Entry`'s public fields; the
`entry_hash` field is the canonical agreement key — see §5.

### 5. Bundle reader's additive manifest flip

**`apps/aberp/src/export_invoice_bundle.rs`** extends
`build_manifest` with two new parameters:

```rust
fn build_manifest<'a>(
    invoice_id: &'a str,
    tenant_id: &'a str,
    binary_hash: BinaryHash,
    chain_verified_entries: u64,
    entries_in_bundle: u64,
    mirror_status: MirrorAgreementStatus,  // NEW
) -> Result<BundleManifest<'a>>
```

The new `MirrorAgreementStatus` enum lives in
`export_invoice_bundle.rs` (consumer-side, NOT in the
audit-ledger crate — surgical-changes posture per CLAUDE.md
rule 3):

```rust
enum MirrorAgreementStatus {
    /// Mirror file present and every entry's entry_hash
    /// matches the DB. `mirror_file_present: true`,
    /// `mirror_file_status: "verified-agreement"`.
    VerifiedAgreement { mirror_entries: u64 },
    /// Mirror file absent (pre-PR-17 DB, not yet bootstrapped
    /// by `sync_mirror`). `mirror_file_present: false`,
    /// `mirror_file_status: "absent-pre-pr-17"`.
    AbsentPrePr17,
}
```

A third state — `DivergenceDetected` — is NOT a variant
because the bundle reader REFUSES the bundle output when
divergence is detected (ADR-0029 §5: "with the bundle output
refused per CLAUDE.md rule 12"). The refusal happens inside
`run` before `build_manifest` is called; the
`MirrorAgreementStatus` enum encodes only the success states
that produce a manifest.

**`run` flow update:** between step 4 (verify_chain) and
step 5 (filter slice), the bundle reader calls
`detect_mirror_agreement(&args.db, &entries) ->
Result<MirrorAgreementStatus>`. The helper:
1. Resolves `mirror_path = mirror_path_for(&args.db)`.
2. If mirror absent → returns `AbsentPrePr17`.
3. If mirror present → reads it; compares against `entries`
   (DB-sourced, already loaded for the slice step) by
   `entry_hash` at each seq.
4. If agreement → returns `VerifiedAgreement { mirror_entries }`.
5. If divergence → returns `Err(anyhow!(...))` with the
   diagnostic naming the first divergent seq + DB-vs-mirror
   hash pair. The bundle is NOT produced.

**Operator-visible message update:** when
`MirrorAgreementStatus::VerifiedAgreement` is the path, the
`println!` line drops the "without mirror-file second-
source assertion (deferred per F10)" half and adds
"verified against mirror file at <path> across <N>
entries." When `AbsentPrePr17` is the path, the message
notes "no mirror file present at <path> — the next command
that appends will initialise it; this bundle's chain
verification is sourced from the DB alone."

### 6. Per-tenant lock posture

**DuckDB single-writer file-lock today.** The audit-ledger's
DB write path holds a DuckDB file lock for the duration of
`tx.commit()`. The mirror write happens AFTER the commit
releases that lock. Between commit and `sync_mirror`, no
other process can have appended to the mirror IF the same
per-tenant single-writer discipline is preserved on the
mirror file.

**Mirror-side lock:** an exclusive `fcntl`-style advisory
lock on `mirror_path` for the duration of the `sync_mirror`
call. On Linux/macOS this is `flock(LOCK_EX)`; on Windows it
is `LockFileEx`. The `fs2` crate provides a cross-platform
wrapper.

**Cloud multi-writer per ADR-0016 — deferred unchanged.**
ADR-0016 is the cross-tenant cloud-attestation ADR; the
per-tenant cloud-side write-coordination question is part
of its scope. PR-17 stays at the local-desktop single-
writer posture per ADR-0029 §5's deferral discipline.

### 7. Bootstrap — implicit one-time backfill

When `sync_mirror` is called on a DB that has entries but the
mirror file does not exist, the writer:

1. Creates the mirror file (CREATE | WRITE_ONLY | APPEND).
2. Reads every DB entry in seq order.
3. Serializes each as a JSON-Lines line; appends in one
   contiguous write.
4. Fsyncs the file once at the end.
5. Logs at INFO level:
   `audit_mirror_initialized mirror_path=<path>
   entries_backfilled=<N>`.
6. Returns `Ok(N)`.

**Why implicit, loudly logged, one-time:** the operator's
mental model is "the binary upgraded; the next command
worked." The alternative (a separate `aberp audit-mirror-
init` CLI verb) breaks every command's first post-PR-17
invocation with a hard error and forces the operator to
read release notes before the binary will work. The
LOUDLY-LOGGED implicit backfill is the same posture
`Ledger::open`'s `CREATE TABLE IF NOT EXISTS` already uses
for the audit_ledger DB schema — an idempotent bootstrap
that's invisible-to-the-operator-when-everything-is-fine
but appears clearly in `tracing` output when something
interesting happens (a new tenant, a new install, a binary
upgrade).

**Backfill is a one-time event.** Once the mirror file has
been initialised, subsequent `sync_mirror` calls follow the
incremental path (read mirror head, read DB entries with
seq > head, append). The INFO log line on backfill is
distinct from the per-append log line, so an operator
searching logs can find the single backfill event.

## Open questions

- **Cold-storage offload of old mirror ranges.** ADR-0008
  §Consequences names "Cold-storage offload (older ranges
  signed and archived) is allowed and designed later." PR-17
  does NOT design this; trigger is the first operational
  pattern around mirror-file size. Honest deferral per
  CLAUDE.md rule 2.
- **Mirror compaction during long-running processes.** ADR-0008
  §Consequences also names that compaction is NOT allowed.
  PR-17 honours this — the mirror grows linearly with audit
  activity. The first operational complaint about mirror size
  is the deferred-trigger for a cold-storage design ADR.
- **F5 + ADR-0016 trust anchors.** Still deferred; PR-17 does
  NOT publish the mirror to an external trust anchor. F5's
  trigger (named in ADR-0029 §4 + adr/README.md §Deferred)
  has not fired.

## Consequences

- **Every command that appends gains one line of code.** The
  `sync_mirror` call post-commit. ~13 call sites; the change
  is mechanical and surfaced by grep.
- **The bundle reader's deferred-gate strings retire from the
  manifest.** `mirror_file_status` becomes load-bearing
  (`"verified-agreement"`, `"absent-pre-pr-17"`, or — by
  refusal-path — never produced for `"divergence-detected"`).
- **Pre-PR-17 DBs are forward-compatible.** The first
  appending command after the binary upgrade initialises
  the mirror; bundle exports between the upgrade and the
  first append surface `"absent-pre-pr-17"` as the honest
  state. No operator action required.
- **Mirror corruption is fail-loud.** An interrupted prior
  write, an entry-hash disagreement with the DB, or a
  mirror-side I/O error refuses the next append. The DB
  stays canonical; the operator's recovery is to inspect,
  repair, and re-run. CLAUDE.md rule 12.
- **The session-6 fortnightly sharpening's review-bar is
  cleared.** Ervin's review of this ADR + PR-17 IS the
  clearance. The next fortnightly review (~2026-06-03) no
  longer carries F10 as an open item.
- **F38 (bundle verifier tool) remains open.** Independent of
  F10. Named trigger per ADR-0029 §"Adversarial review 4":
  "first PR that produces a signed bundle OR first external-
  inspector test of an existing bundle, whichever fires
  first." Unchanged by PR-17.
- **Storage cost: 2× the audit-ledger size on disk.** The
  mirror is a verbatim per-entry copy. ADR-0008 §Consequences
  named this growth as expected ("Storage grows linearly with
  activity. Ledger compaction is not allowed"). Per-tenant
  volumes for a single SME's annual invoice count are
  comfortably bounded.

## Adversarial review

Five concerns named, four answered, one accepted-as-residual:

1. **"An attacker who deletes the mirror file gets a clean
   re-bootstrap that 'verifies' against the corrupted DB."**
   Answered: ADR-0008 §"Adversarial review" bullet 1 already
   names this — the mirror is not anti-tamper; the
   intentional-attack trust anchor is F5 + ADR-0016
   (deferred). PR-17 stays inside the unintentional-corruption
   scope; the bootstrap path is loudly logged so an operator
   reviewing logs sees the (legitimate or otherwise) re-
   initialisation event. A future F5 lift adds the external
   signature that makes mirror deletion observable to the
   external trust anchor.

2. **"What if `sync_mirror` runs concurrently from two
   processes (e.g., operator runs two commands at once)?"**
   Answered: the per-tenant DuckDB file-lock blocks concurrent
   commits at the DB level; the mirror's `flock(LOCK_EX)`
   blocks concurrent mirror appends at the file level. Two
   processes cannot both be inside `sync_mirror` for the same
   mirror_path simultaneously. The second waits for the lock.

3. **"What if the mirror's `flock` is held when the second
   process tries to append and the first process crashes
   mid-write?"** Answered partially: `flock`-style advisory
   locks are released on process exit, so the second process
   acquires the lock after the crash. The pre-append
   integrity check then detects the partial line (no trailing
   newline) and returns `AppendError::MirrorCorrupt`. The
   operator's recovery is to truncate the partial line and
   re-run; the next `sync_mirror` catches up. CLAUDE.md rule
   12 honoured.

4. **"What if the DB and the mirror are on different
   filesystems with different fsync semantics?"** Accepted as
   residual: ADR-0008 §"Storage" treats the mirror as best-
   effort secondary; cross-FS divergence is a corruption
   mode the operator must investigate via the divergence-
   detected fail-loud path. PR-17 does NOT enforce same-FS
   placement; the convention (`<db>.audit.log` alongside the
   DB) makes same-FS the default but does not enforce it.

5. **"The mirror's JSON-Lines format is denser than canonical
   CBOR. Does this matter for storage?"** Answered: yes, the
   JSON encoding is roughly 1.6× the CBOR encoding for
   ABERP-shaped entries (hex-encoded hashes + base64-encoded
   payloads add overhead). Accepted in trade for operator-
   readability (CLAUDE.md rule 7's surfaced conflict 2 above).
   A future PR may add an optional `--format=cbor` shape if
   storage pressure surfaces operationally; not pre-emptively
   per CLAUDE.md rule 2.

## Alternatives considered

- **No mirror file; keep the deferred-gate strings forever.**
  Refused — F10 is a real ADR-0008 §"Storage" commitment.
  The session-6 sharpening's review-bar is a legitimate
  expectation. Deferring forever is the soft-assertion mode
  CLAUDE.md rule 12 names.
- **CBOR-shaped mirror file.** Refused per §"Surfaced
  conflict 2" — operator tooling for JSON-Lines is universal;
  CBOR tooling is niche. The chain-hash CBOR shape lives at
  the in-memory canonical encoder layer per ADR-0021 §12;
  re-using it on disk is value-mismatched.
- **Per-process write-through mirror buffer.** A buffered
  writer that batches mirror appends across multiple
  `sync_mirror` calls. Refused — buffering trades
  durability for throughput; the mirror's whole purpose is
  durability against DB-file corruption. ADR-0008
  §"Storage" names the fsync per commit, not per buffer
  flush.
- **Hook `sync_mirror` inside `Ledger::open`.** Refused —
  `Ledger::open` is called by both write paths AND read
  paths (the bundle reader opens the ledger read-only at
  `export_invoice_bundle.rs` line 610). A read-path mirror
  sync would catch a corrupted DB up into the mirror, which
  is actively wrong (the operator's evidence of
  corruption-pre-corruption disappears).
- **Wrap `tx.commit()` in a single helper that also calls
  `sync_mirror`.** Considered. The single-line replacement
  per call site is roughly equivalent. The explicit two-
  line pattern is preferred because the failure modes are
  distinct: a DB commit failure is one error class; a
  mirror sync failure is another. Bundling them into one
  helper averages the two errors per CLAUDE.md rule 7 and
  costs the operator the ability to distinguish.

## Follow-on PRs unblocked by this decision

- **F5 attestation signing.** Named trigger unchanged. When
  fired, the future PR extends the bundle manifest additively
  with a `signature_*` block and a sibling detached-signature
  file inside the archive; the manifest's `signed: false`
  flips to `true` and `signature_status` shifts from
  `"deferred-per-f5"` to the chosen algorithm name (e.g.,
  `"ed25519"`).
- **F38 bundle verifier tool.** Named trigger unchanged.
  When fired, the future tool re-checks the chain against
  the bundle's own bytes and (when present) the mirror's
  bytes; PR-17's `read_mirror_entries` is the shared read
  surface.
- **Cold-storage offload.** Trigger remains "first
  operational pattern complaint about ledger size." Both DB
  and mirror cold-storage are designed together when the
  trigger fires.
- **Cloud-side mirror replication.** Trigger remains
  ADR-0016. The mirror file is the local-side artifact
  ADR-0016 will publish; PR-17 lays the groundwork without
  pre-empting the cloud design.
