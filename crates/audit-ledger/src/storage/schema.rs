//! DuckDB schema for the audit-ledger table.
//!
//! Single table, one row per entry. Per ADR-0019, no foreign keys.
//!
//! # No DB-level `CHECK` (S410 — [[no-sql-specific]])
//!
//! This table used to carry `CHECK (seq >= 1)` and `CHECK (time_mono >= 0)`.
//! They were the same class of invariant the S341 `UNIQUE`-drop below
//! addressed — schema-encoded constraints that do not survive an engine
//! swap — yet they outlived that cleanup. They are now DROPPED. The
//! invariants live in Rust: `seq` is minted only via
//! [`crate::entry::Sequence::FIRST`] / [`crate::entry::Sequence::next`]
//! (both `≥ 1` by construction; [`crate::entry::Sequence::new`] rejects
//! `0`), and `time_mono` is an app-supplied monotonic reading that is
//! `≥ 0` by construction. Both columns are covered by the tamper-evident
//! hash chain (`verify_chain`), which detects any forged/garbage value
//! regardless of what the engine would have enforced.
//!
//! # No `UNIQUE` constraints (S341 — duckdb#23046 / S332)
//!
//! This table used to carry inline `UNIQUE(seq)` / `UNIQUE(id)`
//! constraints. Those were the ONLY ART (Adaptive Radix Tree) secondary
//! indexes on the table, and DuckDB 1.5.x corrupts the on-disk ART of a
//! file-backed database on insert (upstream `duckdb/duckdb#23046`,
//! introduced in 1.5.0, still open in the latest 1.5.3) — the
//! `FixedSizeAllocator::New → Prefix::New` panic that made every
//! audit-bearing commit abort (S332). They have been DROPPED: with no
//! secondary index, the corruption class cannot occur.
//!
//! This does NOT weaken integrity. The `UNIQUE` was never the
//! cross-writer fork guard it appeared to be — ABERP's own S186/PR-186
//! finding established that DuckDB's `UNIQUE` does not fire across
//! `Connection::open` handles. Integrity is enforced by (1) the
//! tamper-evident hash chain (`verify_chain`) which DETECTS any
//! duplicate/reordered/forked `seq`, and (2) the process-wide
//! `AUDIT_APPEND_LOCK` (`storage/mod.rs`) which PREVENTS in-process
//! forks. Existing prod files are migrated off the old schema
//! transparently at boot by `migrate_drop_unique_art_if_present`.
//!
//! Per ADR-0008 §"Storage", the ledger "lives in its own DuckDB table
//! inside the tenant database" — i.e. one `audit_ledger` table per
//! tenant DuckDB file. Multi-tenant separation is at the file level,
//! not at the row level (ADR-0002).
//!
//! The table name `audit_ledger` is inlined into the SQL constants
//! below rather than threaded through a `const TABLE: &str`. The name
//! never changes; an indirection would be ceremony per CLAUDE.md rule 2.

/// `CREATE TABLE IF NOT EXISTS` DDL for the audit-ledger table.
///
/// Column order intentionally matches ADR-0008 §"Entry shape" reading
/// order for review clarity. The canonical CBOR encoding does NOT use
/// this order — it uses [`crate::canonical`]'s RFC 8949 §4.2.1 order —
/// so changes to this DDL never affect the hash chain.
/// `CREATE TABLE IF NOT EXISTS` DDL for the audit-ledger table.
///
/// The last three columns (`session_id`, `session_pubkey`, `event_sig`)
/// are S441 / ADR-0087 additions — all NULLABLE (no `DEFAULT`, the DuckDB
/// replay-clobber trap). They are EXCLUDED from the `entry_hash` canonical
/// preimage, so adding them leaves every legacy `entry_hash` byte-identical
/// (`crate::canonical` is untouched). Existing tenant DBs gain the columns
/// via [`crate::storage::migrate_add_session_columns_if_absent`] at boot.
pub const CREATE_TABLE: &str = "
CREATE TABLE IF NOT EXISTS audit_ledger (
    id              VARCHAR     NOT NULL,
    seq             BIGINT      NOT NULL,
    prev_hash       BLOB        NOT NULL,
    time_wall       VARCHAR     NOT NULL,
    time_mono       BIGINT      NOT NULL,
    actor           VARCHAR     NOT NULL,
    binary_hash     BLOB        NOT NULL,
    tenant_id       VARCHAR     NOT NULL,
    kind            VARCHAR     NOT NULL,
    payload         BLOB        NOT NULL,
    idempotency_key VARCHAR,
    entry_hash      BLOB        NOT NULL,
    session_id      VARCHAR,
    session_pubkey  VARCHAR,
    event_sig       VARCHAR
);
";

/// SQL to insert a row. Parameter order matches the `?` placeholders.
pub const INSERT: &str = "
INSERT INTO audit_ledger
    (id, seq, prev_hash, time_wall, time_mono, actor,
     binary_hash, tenant_id, kind, payload, idempotency_key, entry_hash,
     session_id, session_pubkey, event_sig)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
";

/// SQL to read all rows in seq order.
pub const SELECT_ALL: &str = "
SELECT id, seq, prev_hash, time_wall, time_mono, actor,
       binary_hash, tenant_id, kind, payload, idempotency_key, entry_hash,
       session_id, session_pubkey, event_sig
FROM audit_ledger
ORDER BY seq ASC;
";

/// SQL to read the latest entry (highest seq) — used by `append` to
/// compute `prev_hash` and `seq` for the new row.
pub const SELECT_HEAD: &str = "
SELECT id, seq, prev_hash, time_wall, time_mono, actor,
       binary_hash, tenant_id, kind, payload, idempotency_key, entry_hash,
       session_id, session_pubkey, event_sig
FROM audit_ledger
ORDER BY seq DESC
LIMIT 1;
";

/// SQL to read the most-recent `N` entries (highest seq first). One
/// parameter: `LIMIT`. Powers the operator dashboard's recent-activity
/// tile (PR-231 / S235) — no offset, no tenant filter (the per-tenant
/// DuckDB file IS the tenant scope per ADR-0002).
pub const SELECT_RECENT: &str = "
SELECT id, seq, prev_hash, time_wall, time_mono, actor,
       binary_hash, tenant_id, kind, payload, idempotency_key, entry_hash,
       session_id, session_pubkey, event_sig
FROM audit_ledger
ORDER BY seq DESC
LIMIT ?;
";
