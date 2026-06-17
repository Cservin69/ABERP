//! S443 / ADR-0092 — QC dimensional-inspection module.
//!
//! The per-feature MEASUREMENT side of quality, distinct from the
//! routing-op DECISION queue in [`crate`] root (`qa_inspections`). Per
//! ADR-0092 §"Reconciliation" the two coexist at different altitudes;
//! this module adds new tables (`qc_inspection_plans`, `qc_inspections`)
//! rather than overloading `qa_inspections`, so the qa state machine and
//! its WO-completion gate are untouched.
//!
//! What lives here (the domain core):
//! - [`verdict::compute_verdict`] — the pure pass/minor/major/critical
//!   tier + calibration-stale rule ([[trust-code-not-operator]]).
//! - [`plans`] — `qc_inspection_plans` master-data CRUD (the nominal/tol
//!   source of truth; unique (product, feature) in code).
//! - [`inspections::record_inspection`] — the write chokepoint: verdict +
//!   row + the five inspection audit events (in the caller's tx). It does
//!   NOT create the NCR (the app layer does — see module docs).
//! - [`probe`] — the `ProbeIngestionSource` trait + a working
//!   `MockProbeSource` + the `todo!()`-stubbed MTConnect / Renishaw
//!   transports (no machine wired yet; the manual pipeline ships today).

mod error;
pub mod inspections;
pub mod plans;
pub mod probe;
pub mod verdict;

use duckdb::Connection;

pub use error::QcError;
pub use inspections::{
    link_auto_ncr, list_inspections_for_part, list_inspections_for_wo,
    list_recent_stale_calibration, record_ingestion_failure, record_inspection, QcInspection,
    QcSource, QcWriteContext, RecordInspectionInputs, RecordedInspection,
};
pub use plans::{
    archive_plan, create_plan, get_plan, list_plans, update_plan, InspectionPlan, NewInspectionPlan,
};
pub use probe::{
    MockProbeSource, MtconnectProbeSource, ProbeCursor, ProbeError, ProbeIngestionSource,
    RawProbeEvent, RenishawCentralSource,
};
pub use verdict::{compute_verdict, Verdict};

/// Apply `V002__qc.sql` (the two QC tables). Idempotent. Called by
/// [`crate::ensure_schema`] so the QC tables exist wherever the QA queue
/// does.
pub fn ensure_qc_schema(conn: &Connection) -> anyhow::Result<()> {
    use anyhow::Context;
    conn.execute_batch(include_str!("../../migrations/V002__qc.sql"))
        .context("ensure qc schema")
}
