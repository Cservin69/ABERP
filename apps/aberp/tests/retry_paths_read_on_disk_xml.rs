//! PR-44δ.1 pin tests — `retry_submission` + `drain_pending_retries`
//! read the NAV InvoiceData XML from disk and do NOT re-render it.
//!
//! # Why this PR exists
//!
//! The session-54 close handoff named PR-44δ.1 as a spot-check on the
//! two wire-side retry surfaces the §5 row "PR-44γ" envelope flagged
//! as deferred: when an invoice is re-submitted via
//! `aberp retry-submission` (operator-driven) or
//! `aberp drain-pending-retries` (automatic), does the NAV body get
//! re-rendered (which would re-call `nav_xml::render_invoice_data` /
//! `render_storno_data` / `render_modification_data` and could in
//! principle re-fetch the MNB rate), or does it read the on-disk
//! `<InvoiceData>` XML that was written at original-submit time
//! (which is the regulator-correct behaviour — same body, same
//! signature-body-suffix, same exchange rate, byte-for-byte)?
//!
//! ADR-0031 §2 + PR-18 established the on-disk posture as the
//! invariant. PR-44γ.1 (chain-currency-match) extended the chain
//! issuance paths to land currency + rate-metadata on disk at
//! issuance time; the wire-side retry surfaces pick those bytes up
//! verbatim only if they DO read from disk (and don't re-render).
//!
//! # What this file pins
//!
//! Source-text invariants on the two retry-path modules:
//!
//!   1. They MUST call `std::fs::read(...)` on the on-disk XML path
//!      (`args.invoice_xml` for the operator surface;
//!      `retry.nav_xml_path` for the automatic surface).
//!   2. They MUST NOT call `nav_xml::render_invoice_data(...)` or
//!      `nav_xml::render_storno_data(...)` or
//!      `nav_xml::render_modification_data(...)` (the three render
//!      entry points). A doc-comment prose mention of any of those
//!      function names (in backticks, no open paren) is fine; only
//!      a CALL is the regression.
//!
//! A future regression that introduces a re-render path on either
//! retry surface (which would re-fetch MNB at retry time and risk
//! a drifted `<exchangeRate>` value relative to the audit ledger's
//! stamped rate per ADR-0037 §4 C6) loud-fails here at compile +
//! test time.

const RETRY_SUBMISSION_SRC: &str = include_str!("../src/retry_submission.rs");
const DRAIN_PENDING_RETRIES_SRC: &str = include_str!("../src/drain_pending_retries.rs");

/// The three NAV-body render entry points. A call site looks like
/// `nav_xml::render_invoice_data(`, `render_invoice_data(` (after a
/// `use crate::nav_xml::render_invoice_data;`), or
/// `crate::nav_xml::render_invoice_data(`. All three forms end in an
/// open paren — a doc-comment prose mention (in backticks, no paren)
/// is not a call.
const RENDER_CALL_FORMS: &[&str] = &[
    "render_invoice_data(",
    "render_storno_data(",
    "render_modification_data(",
];

fn assert_no_render_call(src: &str, module_name: &str) {
    for form in RENDER_CALL_FORMS {
        assert!(
            !src.contains(form),
            "{module_name} contains a call to `{form})` — \
             PR-44δ.1 invariant violated: the retry path MUST NOT \
             re-render the NAV InvoiceData XML; it MUST read the \
             on-disk XML written at original-submit time per ADR-0031 \
             §2 + PR-18 + ADR-0037 §4 C6 (frozen-rate posture). \
             If you are intentionally adding a re-render path, this \
             is the wrong place — file a new ADR on chain-rate \
             drift first."
        );
    }
}

fn assert_reads_from_disk(src: &str, module_name: &str) {
    assert!(
        src.contains("std::fs::read("),
        "{module_name} no longer calls `std::fs::read(...)` — \
         PR-44δ.1 invariant violated: the retry path MUST read the \
         on-disk NAV InvoiceData XML written at original-submit time \
         per ADR-0031 §2 + PR-18."
    );
}

#[test]
fn retry_submission_reads_on_disk_xml_not_re_renders() {
    assert_reads_from_disk(RETRY_SUBMISSION_SRC, "apps/aberp/src/retry_submission.rs");
    assert_no_render_call(RETRY_SUBMISSION_SRC, "apps/aberp/src/retry_submission.rs");
}

#[test]
fn drain_pending_retries_reads_on_disk_xml_not_re_renders() {
    assert_reads_from_disk(
        DRAIN_PENDING_RETRIES_SRC,
        "apps/aberp/src/drain_pending_retries.rs",
    );
    assert_no_render_call(
        DRAIN_PENDING_RETRIES_SRC,
        "apps/aberp/src/drain_pending_retries.rs",
    );
}
