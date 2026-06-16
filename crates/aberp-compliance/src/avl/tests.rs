//! Unit tests for the S345 AVL / DPAS types.

use super::{
    parse_categories, render_categories, ApprovalCategory, ApprovedStatus, ApprovedSupplierEntry,
    AvlScreeningResult, DpasPriority, DpasRating, ExportScreeningStatus, PartnerRef,
    ProgramSymbolError, QualLevel,
};

#[test]
fn s367_unrated_supplier_is_dpas_none() {
    // F13: "unrated commercial order" is the absence of a rating, not a variant.
    let entry = ApprovedSupplierEntry {
        partner_id: PartnerRef("p".to_string()),
        qualification_level: QualLevel::Bid,
        dpas: None,
        screening: ExportScreeningStatus::default(),
        last_audit_at_ms: None,
    };
    assert!(entry.dpas.is_none());
}

#[test]
fn s345_export_screening_status_defaults_to_not_screened() {
    assert_eq!(
        ExportScreeningStatus::default(),
        ExportScreeningStatus::NotScreened
    );
}

#[test]
fn s345_qual_level_bid_and_deliver_gating() {
    // Bid: may bid, may NOT deliver.
    assert!(QualLevel::Bid.can_bid());
    assert!(!QualLevel::Bid.can_deliver());
    // Approved: may bid AND deliver.
    assert!(QualLevel::Approved.can_bid());
    assert!(QualLevel::Approved.can_deliver());
    // Disapproved: neither.
    assert!(!QualLevel::Disapproved.can_bid());
    assert!(!QualLevel::Disapproved.can_deliver());
}

#[test]
fn s345_avl_entry_construction_and_roundtrip() {
    let entry = ApprovedSupplierEntry {
        partner_id: PartnerRef("partner-4711".to_string()),
        qualification_level: QualLevel::Approved,
        dpas: Some(DpasRating::new(DpasPriority::Dx, "A1").expect("valid rating")),
        screening: ExportScreeningStatus::Clear,
        last_audit_at_ms: Some(1_700_000_000_000),
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    let back: ApprovedSupplierEntry = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(entry, back);
    assert_eq!(back.partner_id, PartnerRef("partner-4711".to_string()));
    assert!(back.qualification_level.can_deliver());
}

#[test]
fn s345_avl_entry_unaudited_defaults() {
    // A freshly-listed supplier: bid-only, unrated, unscreened, never audited.
    let entry = ApprovedSupplierEntry {
        partner_id: PartnerRef("partner-new".to_string()),
        qualification_level: QualLevel::Bid,
        dpas: None,
        screening: ExportScreeningStatus::default(),
        last_audit_at_ms: None,
    };
    assert!(entry.qualification_level.can_bid());
    assert!(!entry.qualification_level.can_deliver());
    assert!(entry.dpas.is_none());
    assert_eq!(entry.screening, ExportScreeningStatus::NotScreened);
    assert!(entry.last_audit_at_ms.is_none());
}

#[test]
fn s345_dpas_rating_roundtrip() {
    for r in [
        DpasRating::new(DpasPriority::Do, "A1").unwrap(),
        DpasRating::new(DpasPriority::Dx, "A7").unwrap(),
        DpasRating::new(DpasPriority::Do, "C1").unwrap(),
    ] {
        let json = serde_json::to_string(&r).expect("serialize");
        let back: DpasRating = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r, back);
    }
}

// ── S361 / PR-48 (ADR-0078) — storage-string newtype validation ──
//
// The partners `dpas_rating` / `export_screening_status` columns and the
// `supplier.*` audit payloads store the canonical `as_str` form; the firing
// site (later session) validates an inbound string through `parse` /
// `from_storage_str` before it reaches the column / ledger. These pin that the
// `as_str` ⇄ `parse` pair round-trips real ratings and rejects garbage, so a
// malformed value can never reach storage.

#[test]
fn s361_dpas_rating_storage_str_round_trips_every_variant() {
    for r in [
        DpasRating::new(DpasPriority::Do, "A1").unwrap(),
        DpasRating::new(DpasPriority::Dx, "A7").unwrap(),
        DpasRating::new(DpasPriority::Do, "C1").unwrap(),
        DpasRating::new(DpasPriority::Dx, "F1").unwrap(),
    ] {
        let s = r.as_str();
        assert_eq!(
            DpasRating::parse(&s).expect("round-trip"),
            r,
            "round-trip mismatch for {s}"
        );
    }
}

#[test]
fn s361_dpas_rating_storage_tokens_are_pinned_and_reject_unknown() {
    // 15 CFR 700.12 form: <DO|DX>-<program symbol> (e.g. the regulation's own
    // worked example DO-A1).
    assert_eq!(
        DpasRating::new(DpasPriority::Do, "A1").unwrap().as_str(),
        "DO-A1"
    );
    assert_eq!(
        DpasRating::new(DpasPriority::Dx, "A7").unwrap().as_str(),
        "DX-A7"
    );
    // Unknown / wrong-case / out-of-range strings fail loud — never a silent
    // default.
    assert!(DpasRating::parse("").is_err());
    assert!(DpasRating::parse("do-a1").is_err()); // lowercase priority
    assert!(DpasRating::parse("DX").is_err()); // no program symbol
    assert!(DpasRating::parse("DO-G1").is_err()); // letter out of A-F
    assert!(DpasRating::parse("DO-A0").is_err()); // digit out of 1-9
    assert!(DpasRating::parse("DO-A12").is_err()); // too long
    assert!(matches!(
        DpasRating::parse("DZ-A1"),
        Err(ProgramSymbolError::BadPriority(_))
    ));
    assert!(matches!(DpasRating::validate_program_symbol("A1"), Ok(())));
    assert!(matches!(
        DpasRating::validate_program_symbol("ZZ"),
        Err(ProgramSymbolError::BadSymbol(_))
    ));
}

#[test]
fn s361_export_screening_status_storage_str_round_trips_every_variant() {
    for st in [
        ExportScreeningStatus::NotScreened,
        ExportScreeningStatus::Clear,
        ExportScreeningStatus::Hit,
        ExportScreeningStatus::Inconclusive,
    ] {
        let s = st.as_str();
        assert_eq!(
            ExportScreeningStatus::from_storage_str(s).expect("round-trip"),
            st,
            "round-trip mismatch for {s}"
        );
    }
}

#[test]
fn s361_export_screening_status_tokens_match_brief_vocab_and_reject_unknown() {
    // The exact `clear` / `hit` / `inconclusive` / `not_screened` vocab the
    // brief pins for the column + the `supplier.export_screened` payload.
    assert_eq!(ExportScreeningStatus::NotScreened.as_str(), "not_screened");
    assert_eq!(ExportScreeningStatus::Clear.as_str(), "clear");
    assert_eq!(ExportScreeningStatus::Hit.as_str(), "hit");
    assert_eq!(ExportScreeningStatus::Inconclusive.as_str(), "inconclusive");
    // A mis-parse to Clear would mark an unscreened / hit supplier clear to
    // transact — the worst-class export-control bug. Must fail loud.
    assert!(ExportScreeningStatus::from_storage_str("CLEAR").is_err());
    assert!(ExportScreeningStatus::from_storage_str("denied").is_err());
    assert!(ExportScreeningStatus::from_storage_str("").is_err());
}

// ── S431 — AVL CRUD lifecycle types ──────────────────────────────────────

#[test]
fn s431_approved_status_round_trips_every_variant() {
    for st in [
        ApprovedStatus::Pending,
        ApprovedStatus::Approved,
        ApprovedStatus::Conditional,
        ApprovedStatus::Suspended,
        ApprovedStatus::Revoked,
    ] {
        let s = st.as_str();
        assert_eq!(
            ApprovedStatus::from_storage_str(s).expect("round-trip"),
            st,
            "round-trip mismatch for {s}"
        );
    }
    assert_eq!(ApprovedStatus::default(), ApprovedStatus::Pending);
    // A mis-parse to Approved would un-block a suspended/revoked vendor's PO.
    assert!(ApprovedStatus::from_storage_str("APPROVED").is_err());
    assert!(ApprovedStatus::from_storage_str("").is_err());
}

#[test]
fn s431_only_suspended_and_revoked_block_po() {
    assert!(!ApprovedStatus::Pending.blocks_po());
    assert!(!ApprovedStatus::Approved.blocks_po());
    assert!(!ApprovedStatus::Conditional.blocks_po());
    assert!(ApprovedStatus::Suspended.blocks_po());
    assert!(ApprovedStatus::Revoked.blocks_po());
}

#[test]
fn s431_status_transitions_pending_to_approved_valid_revoked_terminal() {
    // Brief invariant: Pending → Approved is valid.
    assert!(ApprovedStatus::Pending.can_transition_to(ApprovedStatus::Approved));
    // Any non-revoked source may move to any status, including straight to Revoked.
    assert!(ApprovedStatus::Approved.can_transition_to(ApprovedStatus::Suspended));
    assert!(ApprovedStatus::Suspended.can_transition_to(ApprovedStatus::Approved));
    assert!(ApprovedStatus::Conditional.can_transition_to(ApprovedStatus::Revoked));
    // Brief invariant: Revoked → Approved is INVALID (terminal until manual override).
    assert!(!ApprovedStatus::Revoked.can_transition_to(ApprovedStatus::Approved));
    assert!(!ApprovedStatus::Revoked.can_transition_to(ApprovedStatus::Pending));
    assert!(!ApprovedStatus::Revoked.can_transition_to(ApprovedStatus::Conditional));
    // The only allowed normal transition out of Revoked is the no-op.
    assert!(ApprovedStatus::Revoked.can_transition_to(ApprovedStatus::Revoked));
}

#[test]
fn s431_approval_category_round_trips_and_multi_select_render_parse() {
    for c in [
        ApprovalCategory::General,
        ApprovalCategory::Itar,
        ApprovalCategory::Ear99,
        ApprovalCategory::Aerospace,
        ApprovalCategory::Defense,
        ApprovalCategory::Nuclear,
    ] {
        let s = c.as_str();
        assert_eq!(
            ApprovalCategory::from_storage_str(s).expect("round-trip"),
            c,
            "round-trip mismatch for {s}"
        );
    }
    // Multi-select comma-join round-trip.
    let cats = vec![
        ApprovalCategory::Itar,
        ApprovalCategory::Defense,
        ApprovalCategory::Aerospace,
    ];
    let joined = render_categories(&cats);
    assert_eq!(joined, "itar,defense,aerospace");
    assert_eq!(parse_categories(&joined).expect("parse"), cats);
    // Empty set ⇄ empty string (not an error).
    assert_eq!(render_categories(&[]), "");
    assert!(parse_categories("").expect("empty ok").is_empty());
    assert!(parse_categories("   ").expect("ws ok").is_empty());
    // Whitespace around tokens tolerated.
    assert_eq!(
        parse_categories(" itar , ear99 ").expect("trim"),
        vec![ApprovalCategory::Itar, ApprovalCategory::Ear99]
    );
    // Any unknown token fails loud — never silently dropped.
    assert!(parse_categories("itar,bogus").is_err());
}

#[test]
fn s431_avl_screening_result_round_trips_every_variant() {
    for r in [
        AvlScreeningResult::Pass,
        AvlScreeningResult::Conditional,
        AvlScreeningResult::Fail,
        AvlScreeningResult::SkippedNoIntegration,
    ] {
        let s = r.as_str();
        assert_eq!(
            AvlScreeningResult::from_storage_str(s).expect("round-trip"),
            r,
            "round-trip mismatch for {s}"
        );
    }
    // The mock-screening default (no real OFAC/SDN integration yet).
    assert_eq!(
        AvlScreeningResult::default(),
        AvlScreeningResult::SkippedNoIntegration
    );
    assert_eq!(
        AvlScreeningResult::SkippedNoIntegration.as_str(),
        "skipped_no_integration"
    );
    assert!(AvlScreeningResult::from_storage_str("PASS").is_err());
    assert!(AvlScreeningResult::from_storage_str("").is_err());
}
