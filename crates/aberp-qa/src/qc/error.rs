//! S443 / ADR-0092 — QC error type. Distinct from [`crate::QaError`]
//! (the routing-op decision queue) per the ADR's altitude separation.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum QcError {
    /// Operator input failed an in-code invariant (bad tolerance band,
    /// missing field, duplicate (product, feature) plan).
    #[error("validation: {0}")]
    Validation(String),

    /// The SAMPLE/measurement units disagree with the plan's units —
    /// rejected, never silently coerced (CLAUDE.md rule 12).
    #[error("units mismatch: measurement is {got:?} but plan {plan_id} expects {expected:?}")]
    UnitsMismatch {
        plan_id: String,
        expected: String,
        got: String,
    },

    /// A referenced plan / inspection does not exist for this tenant.
    #[error("not found")]
    NotFound,

    /// DuckDB / audit-ledger storage failure.
    #[error("storage: {0}")]
    Storage(#[from] anyhow::Error),
}
