//! S275 / PR-264 / F22 — DuckDB-binding helpers + the project's
//! timestamp-storage convention.
//!
//! # Convention: RFC3339 `VARCHAR` for audit-adjacent timestamps
//!
//! The DuckDB Rust binding can decode a `TIMESTAMP` column into
//! `Option<OffsetDateTime>` but NOT into `Option<String>` — and every
//! call site in this codebase that surfaces a timestamp to the SPA
//! routes it as an ISO-8601 string. Two choices reconcile that:
//!
//! 1. Store the column as RFC3339 `VARCHAR` (the convention adopted by
//!    `partners.updated_at`, `quoting_materials.updated_at`,
//!    `invoice.issue_date`, every audit-payload timestamp, every NAV
//!    column the queryInvoiceDigest reader populates, etc.).
//! 2. Store as `TIMESTAMP` and `CAST(... AS VARCHAR)` on every read
//!    (the path S272 took for `deal_issued_at` + `refresh_acked_at`
//!    and S262's `payment_deadline`, reports.rs's `delivery_date`).
//!
//! Choice **(1)** is the convention going forward. The
//! `CAST(... AS VARCHAR)` dance is a per-column papercut and a sharp
//! edge for the next contributor — the DuckDB Rust-binding limitation
//! is not a thing they should have to discover.
//!
//! Already-shipped `TIMESTAMP` columns stay — rewriting them is a
//! migration risk for no behavioural gain. The convention applies to
//! new columns: store the RFC3339 string.
//!
//! # Helpers
//!
//! [`ts_cast_as_varchar`] is a tiny SQL-fragment helper that survives
//! the existing `TIMESTAMP` call sites. Use it to centralise the cast
//! when you are adding a new read against an already-`TIMESTAMP`
//! column; do NOT use it as an excuse to add a new `TIMESTAMP` column.

/// Wrap a column reference (or expression) in a `CAST(... AS VARCHAR)`
/// so the DuckDB Rust binding can decode it as `Option<String>`. Use
/// against EXISTING `TIMESTAMP` columns only; new columns should store
/// RFC3339 `VARCHAR` from day one per the convention above.
///
/// # Example
///
/// ```ignore
/// use aberp::duckdb_helpers::ts_cast_as_varchar;
///
/// let sql = format!(
///     "SELECT {iso_issued}, deal_sales_order_id FROM quote_intake_log
///       WHERE quote_id = ?1",
///     iso_issued = ts_cast_as_varchar("deal_issued_at"),
/// );
/// ```
pub fn ts_cast_as_varchar(col_or_expr: &str) -> String {
    format!("CAST({col_or_expr} AS VARCHAR)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_cast_as_varchar_emits_the_expected_fragment() {
        assert_eq!(
            ts_cast_as_varchar("deal_issued_at"),
            "CAST(deal_issued_at AS VARCHAR)"
        );
        // Works on a sub-expression too — same wrapping.
        assert_eq!(
            ts_cast_as_varchar("COALESCE(a, b)"),
            "CAST(COALESCE(a, b) AS VARCHAR)"
        );
    }
}
