//! Unit tests for the S345 AVL / DPAS types.

use super::{ApprovedSupplierEntry, DpasRating, ExportScreeningStatus, PartnerRef, QualLevel};

#[test]
fn s345_dpas_rating_defaults_to_none() {
    assert_eq!(DpasRating::default(), DpasRating::None);
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
        dpas: DpasRating::DxC1,
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
        dpas: DpasRating::default(),
        screening: ExportScreeningStatus::default(),
        last_audit_at_ms: None,
    };
    assert!(entry.qualification_level.can_bid());
    assert!(!entry.qualification_level.can_deliver());
    assert_eq!(entry.dpas, DpasRating::None);
    assert_eq!(entry.screening, ExportScreeningStatus::NotScreened);
    assert!(entry.last_audit_at_ms.is_none());
}

#[test]
fn s345_dpas_rating_roundtrip() {
    for r in [DpasRating::None, DpasRating::DoC1, DpasRating::DxC1] {
        let json = serde_json::to_string(&r).expect("serialize");
        let back: DpasRating = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r, back);
    }
}

// ── S361 / PR-48 (ADR-0078) — storage-string newtype validation ──
//
// The partners `dpas_rating` / `export_screening_status` columns and the
// `supplier.*` audit payloads store the canonical `as_str` form; the firing
// site (later session) validates an inbound string through `from_storage_str`
// before it reaches the column / ledger. These pin that the `as_str` ⇄
// `from_storage_str` pair round-trips every variant and rejects garbage, so a
// malformed value can never reach storage.

#[test]
fn s361_dpas_rating_storage_str_round_trips_every_variant() {
    for r in [DpasRating::None, DpasRating::DoC1, DpasRating::DxC1] {
        let s = r.as_str();
        assert_eq!(
            DpasRating::from_storage_str(s).expect("round-trip"),
            r,
            "round-trip mismatch for {s}"
        );
    }
}

#[test]
fn s361_dpas_rating_storage_tokens_are_pinned_and_reject_unknown() {
    assert_eq!(DpasRating::None.as_str(), "NONE");
    assert_eq!(DpasRating::DoC1.as_str(), "DO-C1");
    assert_eq!(DpasRating::DxC1.as_str(), "DX-C1");
    // Unknown / wrong-case strings fail loud — never a silent default.
    assert!(DpasRating::from_storage_str("").is_err());
    assert!(DpasRating::from_storage_str("do-c1").is_err());
    assert!(DpasRating::from_storage_str("DX").is_err());
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
