//! Compute the SHA-256 of the running binary at process start, per
//! ADR-0008 §"Entry shape": "binary_hash — SHA-256 of the binary that
//! produced the entry (recorded once per process start; referenced)".

use std::fs;
use std::io;

use aberp_audit_ledger::BinaryHash;
use sha2::{Digest, Sha256};

/// Compute the SHA-256 of `std::env::current_exe()` and wrap it in
/// [`BinaryHash`]. On failure (e.g. macOS sandbox where the exe path is
/// inaccessible), returns the I/O error to the caller; ADR-0008 makes
/// the hash a hard requirement, so this is fail-loud per ADR-0007.
pub fn compute() -> io::Result<BinaryHash> {
    let path = std::env::current_exe()?;
    let bytes = fs::read(&path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash: [u8; 32] = hasher.finalize().into();
    Ok(BinaryHash::from_bytes(hash))
}
