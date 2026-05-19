//! ABERP audit-ledger crate.
//!
//! Tamper-evident, hash-chained, append-only audit ledger.
//! Implementation lands in PR-3 per `_handoffs/05-session-5-code-can-start.md`.
//!
//! Design references:
//!
//! - ADR-0008  Tamper-evident audit ledger (entry shape, hash chain, storage,
//!             external attestation, what goes in vs what does not).
//! - ADR-0019  Storage strategy: relational source-of-truth, no foreign keys,
//!             per-module storage port.
//! - ADR-0021  Pre-code consolidated baseline. Items 10 (DuckDB bundled),
//!             12 (canonical CBOR via `ciborium`), and 9 (SHA-256 via `sha2`)
//!             are the relevant pins.
