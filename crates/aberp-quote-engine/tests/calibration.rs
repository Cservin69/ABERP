//! S429 — engine integration: a calibration coefficient scales the routed
//! family's machining minutes (and cost), and only that family's coefficient
//! is consulted.

mod common;

use aberp_quote_engine::{
    quote, quote_with_calibration, CalibrationSample, CalibrationTable, MachineFamily,
};
use common::*;

#[test]
fn coefficient_scales_routed_family_minutes_and_cost() {
    let materials = vec![default_material("6061-T6")];
    let fg = simple_feature_graph("6061-T6"); // requires_5_axis = false → 3-axis-mill

    let base = quote(
        &fg,
        &materials,
        &catchall_complexity_rules(),
        &default_tolerance_multipliers(),
        &no_stock_adjustments(),
        &default_parameters(),
        DEFAULT_QTY,
        DEFAULT_TOL,
    )
    .expect("base quote");
    assert_eq!(base.calibration_coefficient, 1.0);

    // Build a table whose 3-axis coefficient is 1.5 (six samples ratio 1.5).
    let samples: Vec<CalibrationSample> = (0..6)
        .map(|_| CalibrationSample {
            family: MachineFamily::ThreeAxisMill,
            estimated_minutes: 10.0,
            actual_minutes: 15.0,
        })
        .collect();
    let table = CalibrationTable::from_samples(&samples);
    assert!((table.coefficient(MachineFamily::ThreeAxisMill) - 1.5).abs() < 1e-9);

    let calibrated = quote_with_calibration(
        &fg,
        &materials,
        &catchall_complexity_rules(),
        &default_tolerance_multipliers(),
        &no_stock_adjustments(),
        &default_parameters(),
        DEFAULT_QTY,
        DEFAULT_TOL,
        &table,
    )
    .expect("calibrated quote");

    assert!((calibrated.calibration_coefficient - 1.5).abs() < 1e-9);
    // Minutes scaled by exactly the coefficient.
    assert!(
        (calibrated.machining_minutes - base.machining_minutes * 1.5).abs() < 1e-9,
        "machining_minutes {} != base {} * 1.5",
        calibrated.machining_minutes,
        base.machining_minutes
    );
    // Cost rose (machining cost is proportional to machining minutes).
    assert!(calibrated.machining_cost > base.machining_cost);
    // A calibration reasoning line is present.
    assert!(calibrated
        .reasoning_log
        .iter()
        .any(|l| l.contains("[calibration]") && l.contains("3-axis-mill")));
}

#[test]
fn wrong_family_coefficient_is_not_applied() {
    let materials = vec![default_material("6061-T6")];
    let fg = simple_feature_graph("6061-T6"); // routes to 3-axis-mill

    // Table only carries a 5-axis coefficient; the 3-axis quote must ignore it.
    let samples: Vec<CalibrationSample> = (0..6)
        .map(|_| CalibrationSample {
            family: MachineFamily::FiveAxisMill,
            estimated_minutes: 10.0,
            actual_minutes: 18.0,
        })
        .collect();
    let table = CalibrationTable::from_samples(&samples);

    let base = quote(
        &fg,
        &materials,
        &catchall_complexity_rules(),
        &default_tolerance_multipliers(),
        &no_stock_adjustments(),
        &default_parameters(),
        DEFAULT_QTY,
        DEFAULT_TOL,
    )
    .expect("base quote");
    let calibrated = quote_with_calibration(
        &fg,
        &materials,
        &catchall_complexity_rules(),
        &default_tolerance_multipliers(),
        &no_stock_adjustments(),
        &default_parameters(),
        DEFAULT_QTY,
        DEFAULT_TOL,
        &table,
    )
    .expect("calibrated quote");

    assert_eq!(calibrated.calibration_coefficient, 1.0);
    assert!((calibrated.machining_minutes - base.machining_minutes).abs() < 1e-9);
}

#[test]
fn neutral_table_is_byte_identical_to_plain_quote() {
    let materials = vec![default_material("6061-T6")];
    let fg = simple_feature_graph("6061-T6");
    let plain = quote(
        &fg,
        &materials,
        &catchall_complexity_rules(),
        &default_tolerance_multipliers(),
        &no_stock_adjustments(),
        &default_parameters(),
        DEFAULT_QTY,
        DEFAULT_TOL,
    )
    .unwrap();
    let neutral = quote_with_calibration(
        &fg,
        &materials,
        &catchall_complexity_rules(),
        &default_tolerance_multipliers(),
        &no_stock_adjustments(),
        &default_parameters(),
        DEFAULT_QTY,
        DEFAULT_TOL,
        &CalibrationTable::neutral(),
    )
    .unwrap();
    assert_eq!(plain, neutral);
}
