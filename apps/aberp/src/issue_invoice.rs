//! Orchestration for the `aberp issue-invoice` subcommand.
//!
//! Pipeline:
//!
//! 1. Parse the JSON input into a [`InvoiceInputJson`] struct.
//! 2. Open the tenant DuckDB file.
//! 3. Ensure schemas (billing + audit-ledger) exist.
//! 4. Ensure the requested series exists (auto-create on first run).
//! 5. Run the billing allocator via [`aberp_billing::issue_invoice`].
//! 6. Open the audit ledger and append two entries for this issuance
//!    (`InvoiceSequenceReserved` + `InvoiceDraftCreated`).
//! 7. Serialize the [`ReadyInvoice`] to NAV `InvoiceData` XML.
//! 8. Verify the audit chain before exiting.
//!
//! # Known deviation from ADR-0008 (flagged loudly)
//!
//! Steps 5 and 6 are NOT in the same DuckDB transaction. The billing
//! allocator commits its own transaction; the audit-ledger then appends
//! in a separate transaction. ADR-0008 §Storage requires "Entries are
//! written in the same transaction as the state change they describe."
//! Unifying the connection is a tracked item for the next adversarial
//! review; for the PR-5 / commit-#1 scope, the failure mode is bounded
//! (a crash between step 5 and step 6 leaves an invoice without its
//! audit entries; the chain still verifies, but the invoice is an
//! orphan that the reconciliation scan would surface).

use aberp_audit_ledger::{Actor, EventKind, Ledger, TenantId};
use aberp_billing::{
    BillingStore, CustomerId, DuckDbBillingStore, Huf, IdempotencyKey, InvoiceSeries,
    IssueInvoiceCommand, IssueInvoiceOutcome, LineItem, ResetPolicy, SeriesCode, SeriesId,
    SystemClock,
};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::binary_hash;
use crate::cli::IssueInvoiceArgs;
use crate::nav_xml::{self, CustomerInfo, NavParties, SupplierInfo};

// ──────────────────────────────────────────────────────────────────────
// Input JSON shape (NAV-aligned per Ervin's preference, session 5)
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InvoiceInputJson {
    pub supplier: SupplierJson,
    pub customer: CustomerJson,
    pub lines: Vec<LineJson>,
}

#[derive(Debug, Deserialize)]
pub struct SupplierJson {
    #[serde(rename = "taxNumber")]
    pub tax_number: String,
    pub name: String,
    pub address: AddressJson,
}

#[derive(Debug, Deserialize)]
pub struct AddressJson {
    #[serde(rename = "countryCode")]
    pub country_code: String,
    #[serde(rename = "postalCode")]
    pub postal_code: String,
    pub city: String,
    pub street: String,
}

#[derive(Debug, Deserialize)]
pub struct CustomerJson {
    #[serde(rename = "taxNumber")]
    pub tax_number: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct LineJson {
    pub description: String,
    pub quantity: u32,
    #[serde(rename = "unitPrice")]
    pub unit_price: i64,
    #[serde(rename = "vatRatePercent")]
    pub vat_rate_percent: u16,
}

// ──────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────

pub fn run(args: &IssueInvoiceArgs) -> Result<()> {
    let _span = tracing::info_span!("issue_invoice").entered();

    // 1. Read + parse the JSON input.
    let input_bytes = std::fs::read(&args.r#in)
        .with_context(|| format!("read input JSON from {}", args.r#in.display()))?;
    let input: InvoiceInputJson =
        serde_json::from_slice(&input_bytes).context("parse input JSON")?;
    tracing::info!(lines = input.lines.len(), "JSON input parsed");

    if input.lines.is_empty() {
        return Err(anyhow!("input JSON has no lines"));
    }

    // 2. Compute binary hash for audit-ledger entries.
    let binary_hash = binary_hash::compute().context("compute binary hash")?;

    // 3. Resolve tenant id (loud-fail on invalid input).
    let tenant = TenantId::new(args.tenant.clone()).ok_or_else(|| {
        anyhow!(
            "--tenant value '{}' is empty or has a null byte",
            args.tenant
        )
    })?;
    let series_code = SeriesCode::new(args.series.clone()).ok_or_else(|| {
        anyhow!(
            "--series value '{}' fails SeriesCode validation",
            args.series
        )
    })?;

    // 4. Open billing store; ensure schema + series.
    let mut billing = DuckDbBillingStore::open(&args.db)
        .with_context(|| format!("open billing DuckDB at {}", args.db.display()))?;
    billing.ensure_schema().context("ensure billing schema")?;
    ensure_series(&mut billing, &series_code)?;

    // 5. Build IssueInvoiceCommand.
    let command = build_command(&input, &series_code)?;
    let idempotency_key = command.idempotency_key;
    let clock = SystemClock;
    let outcome = aberp_billing::issue_invoice(&mut billing, &clock, command)
        .context("billing.issue_invoice failed")?;

    let invoice = outcome.invoice().clone();
    let reservation = outcome.reservation().clone();
    let is_fresh = matches!(outcome, IssueInvoiceOutcome::Fresh { .. });
    tracing::info!(
        seq = invoice.sequence_number,
        fresh = is_fresh,
        idempotency_key = ?idempotency_key,
        "invoice issued"
    );

    // 6. Append audit-ledger entries — NOT in the same transaction as
    //    step 5 (see module docs and ADR-0008 deviation note).
    let mut ledger =
        Ledger::open(&args.db, tenant.clone(), binary_hash).context("open audit ledger")?;
    if is_fresh {
        let actor = Actor::test_only(); // Real auth lands in a later PR.
        let payload_seq = format!(
            "{{\"invoice_id\":\"{}\",\"seq\":{},\"reservation_id\":\"{}\"}}",
            invoice.id.to_prefixed_string(),
            invoice.sequence_number,
            reservation.id.to_prefixed_string(),
        );
        ledger
            .append(
                EventKind::InvoiceSequenceReserved,
                payload_seq.into_bytes(),
                actor.clone(),
                Some(format!("{:?}", idempotency_key)),
            )
            .context("ledger append InvoiceSequenceReserved")?;
        let payload_draft = format!(
            "{{\"invoice_id\":\"{}\",\"lines\":{}}}",
            invoice.id.to_prefixed_string(),
            invoice.lines.len(),
        );
        ledger
            .append(
                EventKind::InvoiceDraftCreated,
                payload_draft.into_bytes(),
                actor,
                Some(format!("{:?}", idempotency_key)),
            )
            .context("ledger append InvoiceDraftCreated")?;
    } else {
        tracing::info!("replay path: no new audit entries written");
    }

    // 7. Serialize to NAV InvoiceData XML.
    let parties = NavParties {
        supplier: SupplierInfo {
            tax_number: input.supplier.tax_number,
            name: input.supplier.name,
            address_country_code: input.supplier.address.country_code,
            address_postal_code: input.supplier.address.postal_code,
            address_city: input.supplier.address.city,
            address_street: input.supplier.address.street,
        },
        customer: CustomerInfo {
            tax_number: input.customer.tax_number,
            name: input.customer.name,
        },
    };
    let xml =
        nav_xml::render_invoice_data(&invoice, &series_code, &parties).context("render NAV XML")?;
    nav_xml::write_to_path(&args.out, &xml)?;
    tracing::info!(path = %args.out.display(), bytes = xml.len(), "NAV XML written");

    // 8. Verify the audit chain — the success-criterion gate.
    let verified = ledger
        .verify_chain()
        .context("audit-ledger chain verification failed AFTER issuance")?;
    tracing::info!(entries_verified = verified, "audit chain verified");

    // Match the XML's invoice-number format exactly (5-digit padding) so
    // operator logs, audit entries, and the XML body all agree.
    println!(
        "issued invoice {}/{:05} -> {} (audit chain verified across {} entries)",
        series_code.as_str(),
        invoice.sequence_number,
        args.out.display(),
        verified,
    );
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────

fn ensure_series<S: BillingStore + ?Sized>(store: &mut S, code: &SeriesCode) -> Result<()> {
    if store.find_series_by_code(code)?.is_some() {
        return Ok(());
    }
    let series = InvoiceSeries {
        id: SeriesId::new(),
        code: code.clone(),
        reset_policy: ResetPolicy::Never,
        fiscal_year: None,
        created_at: OffsetDateTime::now_utc(),
    };
    store.create_series(&series).context("create series")?;
    tracing::info!(series = code.as_str(), "auto-created series");
    Ok(())
}

fn build_command(input: &InvoiceInputJson, code: &SeriesCode) -> Result<IssueInvoiceCommand> {
    let lines = input
        .lines
        .iter()
        .map(|l| LineItem {
            description: l.description.clone(),
            quantity: l.quantity,
            unit_price: Huf(l.unit_price),
            vat_rate_basis_points: percent_to_basis_points(l.vat_rate_percent),
        })
        .collect();
    Ok(IssueInvoiceCommand {
        idempotency_key: IdempotencyKey::new(),
        series_code: code.clone(),
        customer_id: CustomerId::new(),
        lines,
    })
}

fn percent_to_basis_points(percent: u16) -> u16 {
    percent.saturating_mul(100)
}
