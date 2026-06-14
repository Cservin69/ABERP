//! Newtype identity & hash types for audit-ledger entries.
//!
//! Grouped here because they all serve as opaque value types referenced
//! by [`crate::entry::Entry`]'s fields. Per ADR-0005 every entity gets a
//! newtype so type confusion is a compile error, not a runtime hunt.
//!
//! No `serde::{Serialize, Deserialize}` derives here: PR-3 does not go
//! through serde for these types. The canonical CBOR encoder
//! ([`crate::canonical`]) builds `ciborium::Value` manually, and the
//! DuckDB layer ([`crate::storage`]) uses raw column types. If a future
//! path needs serde on these (e.g. an export-bundle format), add the
//! derive then — and enable the `ulid` crate's `serde` feature for
//! [`EntryId`].

use ulid::Ulid;

// ──────────────────────────────────────────────────────────────────────
// EntryId
// ──────────────────────────────────────────────────────────────────────

/// Audit-ledger entry identifier — a ULID with the `aud_` prefix per
/// ADR-0005 §"Entity prefixes". The storage key is the bare ULID; the
/// prefixed form is used at serialization boundaries (logs, exports,
/// the canonical encoder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(pub Ulid);

impl EntryId {
    /// Generate a new entry id using the `ulid` crate's monotonic generator
    /// (ADR-0005 §"Consequences").
    pub fn new() -> Self {
        Self(Ulid::new())
    }

    /// Render in the ADR-0005 prefixed form: `aud_<ULID>`.
    pub fn to_prefixed_string(&self) -> String {
        format!("aud_{}", self.0)
    }

    /// The bare ULID is the storage key per ADR-0005 §"Decision".
    pub fn as_ulid(&self) -> Ulid {
        self.0
    }
}

impl Default for EntryId {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────
// TenantId
// ──────────────────────────────────────────────────────────────────────

/// Tenant identifier. Used both as a row column and as input to the
/// genesis-hash construction in [`crate::chain::genesis_hash`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantId(String);

impl TenantId {
    /// Construct a tenant id from a non-empty string with no null byte.
    /// The null byte is reserved as the genesis-hash domain separator
    /// in [`crate::chain::genesis_hash`].
    pub fn new(s: impl Into<String>) -> Option<Self> {
        let s = s.into();
        if s.is_empty() || s.contains('\0') {
            None
        } else {
            Some(Self(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

// ──────────────────────────────────────────────────────────────────────
// EntryHash
// ──────────────────────────────────────────────────────────────────────

/// SHA-256 hash of an entry's canonical CBOR bytes, or of the previous
/// entry's `entry_hash` (the chain link). 32 bytes per SHA-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryHash([u8; 32]);

impl EntryHash {
    pub const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

// ──────────────────────────────────────────────────────────────────────
// BinaryHash
// ──────────────────────────────────────────────────────────────────────

/// SHA-256 of the ABERP binary that produced an entry per ADR-0008
/// §"Entry shape": "recorded once per process start; referenced". 32 bytes.
///
/// PR-3 takes this as a constructor parameter; the binary in PR-5 will
/// compute it from `/proc/self/exe` (Linux), the equivalent on macOS, or
/// from the `CARGO_BIN_EXE_aberp` env var in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BinaryHash([u8; 32]);

impl BinaryHash {
    pub const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

// ──────────────────────────────────────────────────────────────────────
// Sequence
// ──────────────────────────────────────────────────────────────────────

/// Contiguous per-tenant 64-bit sequence number — the entry's position in
/// the chain. Starts at 1; gap-free. Integrity is carried in Rust + the
/// hash chain, NOT by the DB: S341 dropped the `UNIQUE(seq)` ART index
/// (duckdb#23046) and S410 dropped the `CHECK (seq >= 1)` constraint
/// ([[no-sql-specific]]). The `seq >= 1` floor now lives here, in
/// [`Sequence::new`] / [`Sequence::FIRST`] / [`Sequence::next`], and a
/// forged `seq` is caught by `verify_chain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sequence(pub u64);

impl Sequence {
    pub const FIRST: Self = Self(1);

    /// Construct a `Sequence` from a raw value, enforcing the `>= 1`
    /// floor that used to be a DB `CHECK (seq >= 1)` (S410). Returns
    /// `None` for `0` — the only out-of-range value a 64-bit unsigned
    /// can carry. Use at decode/import boundaries where the value did
    /// not come from [`Sequence::FIRST`] / [`Sequence::next`].
    pub fn new(value: u64) -> Option<Self> {
        if value >= 1 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub fn next(self) -> Self {
        Self(
            self.0
                .checked_add(1)
                .expect("audit-ledger sequence overflow"),
        )
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod sequence_tests {
    use super::Sequence;

    /// S410 / [[no-sql-specific]] — the `CHECK (seq >= 1)` DDL constraint
    /// was dropped; this pins the `>= 1` floor in code instead.
    #[test]
    fn new_enforces_seq_floor() {
        assert_eq!(
            Sequence::new(0),
            None,
            "seq 0 must be rejected (was CHECK seq >= 1)"
        );
        assert_eq!(Sequence::new(1).map(|s| s.as_u64()), Some(1));
        assert_eq!(Sequence::new(42).map(|s| s.as_u64()), Some(42));
    }

    #[test]
    fn first_and_next_stay_at_or_above_one() {
        assert_eq!(Sequence::FIRST.as_u64(), 1);
        assert_eq!(Sequence::FIRST.next().as_u64(), 2);
    }
}
