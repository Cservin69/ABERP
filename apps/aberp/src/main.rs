//! ABERP — modular multi-tenant ERP backend.
//!
//! Commit #1 surface: an `issue-invoice` subcommand that wires the
//! billing module (ADR-0009) and the audit ledger (ADR-0008) to produce
//! a NAV-compatible invoice XML on disk, while writing audit-ledger
//! entries for the issuance into the tenant DuckDB. See
//! `docs/commit-1-success-criterion.md`.

#![forbid(unsafe_code)]

use anyhow::Result;
use clap::Parser;

mod binary_hash;
mod cli;
mod issue_invoice;
mod nav_xml;

fn main() -> Result<()> {
    init_tracing();
    let args = cli::Cli::parse();
    match args.command {
        cli::Command::IssueInvoice(a) => issue_invoice::run(&a),
    }
}

fn init_tracing() {
    // Human-readable logs to stderr by default; production deployments
    // can flip to JSON via RUST_LOG / a config flag in a later PR.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
