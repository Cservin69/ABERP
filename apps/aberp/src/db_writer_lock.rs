//! ADR-0099 H3 (F-E) — cross-process whole-DB single-writer advisory lock.
//!
//! **The implementation moved to [`aberp_db::db_writer_lock`] (ADR-0110 D9).**
//! This file is the re-export that keeps every `crate::db_writer_lock::…` call
//! site, every `aberp::db_writer_lock::…` test, and every ADR/doc reference to
//! this path landing on something real.
//!
//! It moved because a DB-mutating one-shot outside this package needs the same
//! lock: `crates/aberp-inventory`'s `rebuild-stock-cache`, the recovery binary
//! ADR-0061 §3 tells operators to run, cannot depend on `apps/aberp`. The only
//! alternative was a second copy of the lock-path derivation — and two
//! derivations that drift by one character are two lock files, i.e. no lock at
//! all, silently. So the module went down to `aberp-db`, the crate that already
//! owns "one writer per tenant DB".
//!
//! The one API change: the fallible surface returns
//! [`aberp_db::db_writer_lock::DbWriterLockError`] instead of `anyhow::Error`
//! (`aberp-db` is a library crate — ADR-0021 Part A). `?` into an
//! `anyhow::Result` is unchanged at every call site, and the refusal message —
//! including the `single-writer` substring the F-E refusal tests match on — is
//! carried over verbatim.

pub use aberp_db::db_writer_lock::{
    acquire_or_refuse, try_acquire, DbWriterLockError, DbWriterLockGuard,
};
