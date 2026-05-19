//! ABERP — modular multi-tenant ERP backend.
//!
//! Commit #1 surface: an `issue-invoice` subcommand wires the billing module
//! (ADR-0009) and the audit ledger (ADR-0008) to produce a NAV-compatible
//! invoice XML on disk, while writing audit-ledger entries for the issuance
//! into the tenant DuckDB. That subcommand lands in PR-5.
//!
//! PR-1 ships only the workspace scaffold; this main prints the version and
//! exits 0, which is sufficient to verify the workspace builds and links.

fn main() {
    println!("aberp {}", env!("CARGO_PKG_VERSION"));
}
