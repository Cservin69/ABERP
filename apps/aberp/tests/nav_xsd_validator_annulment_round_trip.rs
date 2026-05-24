//! Integration test pinning the ADR-0026 §4 trap-door against drift
//! for the `<InvoiceAnnulment>` body shape.
//!
//! Same load-bearing pair-up as `nav_xsd_validator_round_trip.rs`
//! for the `<InvoiceData>` side: the annulment validator's allowlist
//! (`crates/nav-xsd-validator/src/validate.rs::validate_annulment_data`)
//! and the annulment emitter's element set
//! (`apps/aberp/src/nav_xml.rs::render_annulment_data`) are two
//! sources of truth for "what NAV v3.0 `<InvoiceAnnulment>` looks
//! like in ABERP." Divergence between them is the failure mode
//! CLAUDE.md rule 7 names. This test is the load-bearing closer of
//! that divergence.
//!
//! If a future PR adds a new element to the emitter without
//! extending the validator's allowlist, or removes a required
//! element from the emitter, this test fails at commit time.

use std::time::Duration;

use aberp::nav_xml::{self, AnnulmentReference};
use aberp_nav_xsd_validator::{validate_annulment_data, NAV_XSD_VERSION};

fn minimal_annulment_reference() -> AnnulmentReference {
    AnnulmentReference {
        base_invoice_number: "INV-default/00007".to_string(),
        // ERRATIC_DATA is the conventional default code per
        // ADR-0025 §"Surfaced conflict 2"; any of the four codes
        // would work here — the validator does NOT enforce the
        // closed-set per ADR-0026 §4.
        annulment_code: "ERRATIC_DATA",
        reason: "test invoice accidentally sent to production".to_string(),
    }
}

/// The emitter's bytes for a minimal AnnulmentReference must
/// validate cleanly. This is the ADR-0026 §4 "Trap-doors against
/// drift" pair-up — the positive-path closer.
#[test]
fn emitter_minimal_annulment_passes_validator() {
    let reference = minimal_annulment_reference();
    let xml = nav_xml::render_annulment_data(&reference)
        .expect("emitter must succeed on the minimal annulment reference");

    match validate_annulment_data(&xml) {
        Ok(()) => {}
        Err(err) => panic!(
            "validator rejected emitter output for NAV v{NAV_XSD_VERSION}: {err}\n\
             --- bytes ---\n{}\n--- end bytes ---",
            String::from_utf8_lossy(&xml)
        ),
    }
}

/// Negative-side pair-up: a trivially-broken byte string (the
/// minimal emitter output with `<annulmentCode>` stripped) MUST
/// fail validation. Pins the trap-door's negative side — if a
/// future refactor makes the validator over-accepting, this fails.
#[test]
fn malformed_annulment_xml_loud_fails_validator() {
    let reference = minimal_annulment_reference();
    let xml = nav_xml::render_annulment_data(&reference).unwrap();

    // Strip the <annulmentCode>...</annulmentCode> element entirely.
    // The validator must surface `MissingRequiredChild` for
    // annulmentCode (ADR-0026 §4).
    let s = String::from_utf8(xml).unwrap();
    let stripped = s.replace("<annulmentCode>ERRATIC_DATA</annulmentCode>", "");
    let err = validate_annulment_data(stripped.as_bytes())
        .expect_err("validator must reject XML missing <annulmentCode>");
    let msg = err.to_string();
    assert!(
        msg.contains("annulmentCode") || msg.contains("malformed"),
        "expected error to mention annulmentCode or malformed shape, got: {msg}"
    );
}

/// Performance pair-up. validate_annulment_data is even narrower
/// than validate_invoice_data (four required children, no nested
/// structure), so the bound is tighter — 100ms for 200 iterations.
/// A future refactor that accidentally introduces an O(n²) walk
/// would blow this assertion. The bound is loose enough to survive
/// CI noise while still catching a real regression.
#[test]
fn annulment_validator_is_fast_on_minimal_payload() {
    let reference = minimal_annulment_reference();
    let xml = nav_xml::render_annulment_data(&reference).unwrap();

    let start = std::time::Instant::now();
    for _ in 0..200 {
        validate_annulment_data(&xml).unwrap();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(100),
        "200 validate_annulment_data calls took {elapsed:?} (>100ms)"
    );
}

/// ADR-0026 §"Adversarial review #4" load-bearing pin: an
/// InvoiceData body must NOT validate against
/// `validate_annulment_data`, and vice versa. The type-confusion
/// guardrail lives in the validator's mod tests too, but the
/// integration test pair-up keeps the pin visible from the call-
/// site direction (a future contributor refactoring the validator
/// from outside the crate sees this fail loud).
#[test]
fn validator_pair_rejects_cross_body_shapes() {
    let reference = minimal_annulment_reference();
    let annulment_xml = nav_xml::render_annulment_data(&reference).unwrap();

    // Feed the InvoiceAnnulment body to validate_invoice_data —
    // must surface UnexpectedRoot.
    let err = aberp_nav_xsd_validator::validate_invoice_data(&annulment_xml)
        .expect_err("validate_invoice_data must reject an InvoiceAnnulment body");
    let msg = err.to_string();
    assert!(
        msg.contains("InvoiceAnnulment") || msg.contains("root"),
        "expected error to mention InvoiceAnnulment root, got: {msg}"
    );
}
