//! Unit tests for the S345 CUI marking vocabulary.

use super::{CuiCategory, CuiMarking};

#[test]
fn s345_cui_is_cui_only_for_cui_variant() {
    assert!(CuiMarking::Cui(CuiCategory::Cti).is_cui());
    assert!(!CuiMarking::Unclassified.is_cui());
    assert!(!CuiMarking::Confidential.is_cui());
    assert!(!CuiMarking::Secret.is_cui());
    assert!(!CuiMarking::TopSecret.is_cui());
}

#[test]
fn s345_cui_is_classified_only_for_classification_levels() {
    assert!(CuiMarking::Confidential.is_classified());
    assert!(CuiMarking::Secret.is_classified());
    assert!(CuiMarking::TopSecret.is_classified());
    // Unclassified and CUI are explicitly NOT classified.
    assert!(!CuiMarking::Unclassified.is_classified());
    assert!(!CuiMarking::Cui(CuiCategory::Expt).is_classified());
}

#[test]
fn s345_cui_display_marking_unclassified_and_classified() {
    assert_eq!(CuiMarking::Unclassified.display_marking(), "UNCLASSIFIED");
    assert_eq!(CuiMarking::Confidential.display_marking(), "CONFIDENTIAL");
    assert_eq!(CuiMarking::Secret.display_marking(), "SECRET");
    assert_eq!(CuiMarking::TopSecret.display_marking(), "TOP SECRET");
}

#[test]
fn s345_cui_display_marking_renders_cui_banner_with_abbrev() {
    assert_eq!(
        CuiMarking::Cui(CuiCategory::Cti).display_marking(),
        "CUI//CTI"
    );
    assert_eq!(
        CuiMarking::Cui(CuiCategory::Prvcy).display_marking(),
        "CUI//PRVCY"
    );
    assert_eq!(
        CuiMarking::Cui(CuiCategory::Expt).display_marking(),
        "CUI//EXPT"
    );
}

#[test]
fn s345_cui_category_abbreviations_are_all_distinct() {
    let cats = [
        CuiCategory::Cti,
        CuiCategory::Prvcy,
        CuiCategory::Expt,
        CuiCategory::Crit,
        CuiCategory::Lei,
        CuiCategory::Ifg,
        CuiCategory::Inf,
        CuiCategory::Isvi,
        CuiCategory::Proc,
        CuiCategory::Prop,
    ];
    let mut seen = std::collections::HashSet::new();
    for c in cats {
        assert!(
            seen.insert(c.abbreviation()),
            "duplicate abbreviation {:?}",
            c.abbreviation()
        );
    }
    assert_eq!(seen.len(), 10, "expected 10 distinct starter categories");
}

#[test]
fn s345_cui_category_roundtrip() {
    let c = CuiCategory::Isvi;
    let json = serde_json::to_string(&c).expect("serialize");
    let back: CuiCategory = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(c, back);
}

#[test]
fn s345_cui_marking_roundtrip() {
    for m in [
        CuiMarking::Unclassified,
        CuiMarking::Cui(CuiCategory::Proc),
        CuiMarking::Confidential,
        CuiMarking::Secret,
        CuiMarking::TopSecret,
    ] {
        let json = serde_json::to_string(&m).expect("serialize");
        let back: CuiMarking = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m, back);
    }
}
