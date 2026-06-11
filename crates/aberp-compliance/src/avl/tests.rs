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
