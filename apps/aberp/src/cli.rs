//! Clap CLI structs for the `aberp` binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "aberp", version, about = "ABERP — modular ERP backend")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Issue an invoice: read a JSON spec, allocate a sequence number,
    /// emit NAV v3.0 InvoiceData XML on disk, and write audit-ledger
    /// entries for the issuance.
    ///
    /// Commit #1 success criterion (see docs/commit-1-success-criterion.md):
    /// the XML structurally matches NAV InvoiceData and the audit chain
    /// verifies cleanly after the run.
    IssueInvoice(IssueInvoiceArgs),
}

#[derive(Debug, Parser)]
pub struct IssueInvoiceArgs {
    /// Path to the input JSON file (NAV-aligned shape; see
    /// fixtures/invoice_minimal.json for the canonical example).
    #[arg(long)]
    pub r#in: PathBuf,

    /// Path to write the NAV InvoiceData XML.
    #[arg(long)]
    pub out: PathBuf,

    /// Path to the tenant DuckDB file. Created on first run.
    #[arg(long, default_value = "./aberp.duckdb")]
    pub db: PathBuf,

    /// Tenant identifier — used for the audit-ledger genesis hash.
    #[arg(long, default_value = "default")]
    pub tenant: String,

    /// Invoice series code. Auto-created on first run if it does not
    /// already exist (with reset_policy = Never).
    #[arg(long, default_value = "INV-default")]
    pub series: String,
}
