//! **ADR-0110 D8 / F3 — DDL reached ONE CALL-FRAME AWAY from a `Handle::read()`.**
//!
//! # Why this exists next to `no_ddl_on_read_handle.rs` rather than duplicating it
//!
//! ADR-0108 R-1 already forbids DDL on a `read()` connection, and
//! `apps/aberp/tests/no_ddl_on_read_handle.rs` pins it tree-wide. That pin is the
//! authority on the rule; this file does not restate it.
//!
//! But that scan is **scope-local**: it flags `ensure_schema(&conn)` written
//! beside the `let conn = …read()` binding. It cannot see
//!
//! ```ignore
//! let conn = db.read()?;              // <- binding
//! some_helper(&conn, tenant)?;        // <- and `some_helper` ensure_schemas it
//! ```
//!
//! because the DDL is in another function. The D8 sweep walked straight into that
//! gap: six migrated reader paths — calibration overview, pipeline status,
//! both relay-queue readers, the PDF re-render prepare, and the invoice-email
//! recipient ladder — each handed a `read()` clone to a callee that ensure_schemas
//! it. R-1 stayed green through all six.
//!
//! The sharpest was `handle_quote_pipeline_status` → `count_recent_daemon_panics`
//! → the AUDIT `ensure_schema`, which on a legacy table reaches
//! `migrate_drop_unique_art_if_present`: a `DROP` + `CREATE` + bulk re-`INSERT`
//! of the entire `audit_ledger`, issued from a connection that holds no writer
//! mutex and can run concurrently with a real writer on the same instance.
//!
//! All six now take `db.write()`. This file pins the two halves of that fix that
//! a scope-local scanner cannot check:
//!
//! 1. the callees really do issue DDL (so the risk is a fact, not a worry), and
//! 2. the D8 entry points really do take `write()` and not `read()`.
//!
//! If someone "optimises" one of those routes back to `read()` — reasonable-
//! looking, since they are reads — R-1 will not notice. This will.
//!
//! Note the shape of this file: it never itself calls `ensure_schema` on a
//! `read()` connection. Doing so would trip R-1's tree-wide scan, which is
//! working as intended.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Extract the body of `fn <name>` — from its signature to the next top-level
/// `\n}` — with `//` comment lines STRIPPED.
///
/// Stripping comments is load-bearing, not tidiness: these functions carry
/// prose that quotes `db.read()` while explaining why they no longer call it,
/// and a naive `contains(".read()")` matches the explanation instead of the
/// code. `no_ddl_on_read_handle.rs` blanks comment lines for the same reason —
/// same trap, same fix. Fails LOUD if the fn vanishes.
fn fn_body(src: &str, name: &str) -> String {
    let sig = format!("fn {name}");
    let start = src
        .find(&sig)
        .unwrap_or_else(|| panic!("function `{name}` not found — renamed or deleted?"));
    let rest = &src[start..];
    let body = match rest.find("\n}") {
        Some(end) => &rest[..end],
        None => rest,
    };
    body.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Half 1 — the callees genuinely issue DDL. If one of these stops
/// ensure_schema-ing, the corresponding `write()` below could be relaxed to a
/// `read()`, and this assertion is what makes that a deliberate decision rather
/// than an accident.
#[test]
fn the_callees_on_these_reader_paths_really_do_issue_ddl() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "apps/aberp/src/quote_calibration.rs",
            "calibration_overview",
            "calibration overview",
        ),
        (
            "apps/aberp/src/email_relay_queue.rs",
            "list_rows",
            "relay-queue list",
        ),
        (
            "apps/aberp/src/email_relay_queue.rs",
            "read_row",
            "relay-queue single row",
        ),
        (
            "apps/aberp/src/quote_pricing_jobs.rs",
            "get_effective_lead_time_days",
            "pdf re-render prepare",
        ),
        // The partners pair — the premise `resolve_recipient_email` is keyed on
        // in half 2. These were the one premise asserted in prose but never
        // pinned, unlike their five siblings.
        (
            "apps/aberp/src/partners.rs",
            "get_partner",
            "invoice-email recipient, rung 2 by partner id",
        ),
        (
            "apps/aberp/src/partners.rs",
            "find_partner_by_tax_number",
            "invoice-email recipient, rung 2 legacy fallback",
        ),
    ];
    for (file, callee, label) in cases {
        let body = fn_body(&read_src(file), callee);
        assert!(
            body.contains("ensure_schema"),
            "ADR-0110 D8/F3: `{callee}` ({label}) no longer calls `ensure_schema`. \
             That may be GOOD — it is the R-1 'hoist the schema establishment' fix — \
             but the caller was put on `db.write()` SOLELY because this callee did \
             DDL. Re-check whether that caller can go back to `db.read()`, then \
             update this pin."
        );
    }

    // The audit one, which is the destructive-capable path.
    let serve = read_src("apps/aberp/src/serve.rs");
    let body = fn_body(&serve, "count_recent_daemon_panics");
    assert!(
        body.contains("ensure_schema"),
        "ADR-0110 D8/F3: `count_recent_daemon_panics` no longer ensure_schemas. See above."
    );
}

/// Half 2 — **the pin that matters**. Every D8 reader path whose callee does DDL
/// must acquire `write()`, never `read()`.
#[test]
fn d8_reader_paths_with_ddl_callees_take_the_writer_not_a_read_clone() {
    let serve = read_src("apps/aberp/src/serve.rs");

    let serve_sites: &[(&str, &str)] = &[
        ("calibration_overview_request", "calibration_overview"),
        ("handle_quote_pipeline_status", "count_recent_daemon_panics"),
        ("handle_list_email_relay_queue", "list_rows"),
        ("handle_get_email_relay_row", "read_row"),
        ("resolve_recipient_email", "partners::get_partner"),
    ];

    for (entry, callee) in serve_sites {
        let body = fn_body(&serve, entry);
        assert!(
            body.contains(".write()"),
            "ADR-0110 D8/F3 REGRESSION: `serve::{entry}` does not take `db.write()`.\n\
             It hands its connection to `{callee}`, which issues `ensure_schema` — DDL. \
             On a `db.read()` try_clone that DDL escapes the writer mutex the clone was \
             released from, which is ADR-0108 R-1's defect and a hole in the \
             single-writer invariant (CLAUDE.md rule 13).\n\
             `no_ddl_on_read_handle.rs` will NOT catch this: its scan is scope-local and \
             the DDL is one call-frame away. That is exactly why this test exists.\n\
             If you want this route back on `read()`, first hoist the `ensure_schema` out \
             of `{callee}` (the R-1 remedy), then update both pins."
        );
        assert!(
            !body.contains(".read()"),
            "ADR-0110 D8/F3 REGRESSION: `serve::{entry}` still contains a `.read()` — \
             a DDL-reaching path must not hold a read clone at all."
        );
    }

    // The daemon path, same rule, different file — but a WEAKER assertion, and
    // deliberately so.
    //
    // This originally read "must take `db.write()` for its whole body". That was
    // over-strong and it cemented a throughput bug: holding the process-wide
    // writer across `aberp_quote_pdf::render` (CPU) and `crate::fs::write_atomic`
    // (an fsync) blocked concurrent invoice-issue writers for ~13ms per quote,
    // N times over a sequential drain. The guard is now dropped right after the
    // last query.
    //
    // So the invariant is NOT "the writer spans the function". It is: **both
    // DDL-reaching calls happen under the writer, and neither happens on a read
    // clone.** A pin must forbid the defect, not freeze one particular fix.
    let rerender = read_src("apps/aberp/src/quote_pdf_rerender_daemon.rs");
    let body = fn_body(&rerender, "prepare_rerender");
    assert!(
        body.contains(".write()"),
        "ADR-0110 D8/F3 REGRESSION: `prepare_rerender` must acquire `db.write()` — it \
         ensure_schemas directly AND via `get_effective_lead_time_days`."
    );
    assert!(
        !body.contains(".read()"),
        "ADR-0110 D8/F3 REGRESSION: `prepare_rerender` must not hold a `read()` clone; \
         a write-for-DDL + read-for-queries split puts `get_effective_lead_time_days` \
         back on a reader."
    );
    // Ordering: both DDL-reaching calls must precede the guard release. This is
    // what lets the guard be narrowed WITHOUT re-opening R-1.
    let ensure_at = body
        .find("ensure_schema")
        .expect("prepare_rerender must still ensure the pricing-jobs schema");
    let lead_time_at = body
        .find("get_effective_lead_time_days")
        .expect("prepare_rerender must still resolve the effective lead time");
    let drop_at = body.find("drop(conn)").expect(
        "ADR-0110 D8/F3: `prepare_rerender` should release the writer explicitly before \
         its render + fsync tail. If the explicit `drop(conn)` is gone, the guard is \
         being held to end-of-scope again — across a PDF render and an fsync.",
    );
    assert!(
        ensure_at < drop_at && lead_time_at < drop_at,
        "ADR-0110 D8/F3 REGRESSION: a DDL-reaching call in `prepare_rerender` now happens \
         AFTER the writer is dropped. Both `ensure_schema` and \
         `get_effective_lead_time_days` must run while the guard is held."
    );
}

/// The one D8 read path that is genuinely DDL-free stays on `read()`.
///
/// Without this, the rule above would decay into "put everything on write()",
/// which would serialize reads that have no reason to serialize and quietly
/// discard the reason `read()` exists.
#[test]
fn the_ddl_free_reader_path_stays_on_a_read_clone() {
    let reports = read_src("apps/aberp/src/reports.rs");
    let body = fn_body(&reports, "compute_financial_report");
    assert!(
        body.contains("db.read()") || body.contains(".read()"),
        "ADR-0110 D8/F3: `compute_financial_report` should still read through \
         `db.read()`. Its four SQL-aggregate callees issue no DDL (verified), so it \
         scopes a brief `write()` for the schema bootstrap and reads the rest \
         concurrently. If this became a blanket `write()`, the D8 sweep would have \
         serialized the single heaviest read in the app for no reason."
    );
    assert!(
        body.contains(".write()"),
        "…and it must still take the scoped `write()` for its schema bootstrap \
         (ADR-0110 D8/F2)."
    );
}
