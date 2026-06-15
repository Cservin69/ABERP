//! S429 — closed-loop calibration of quote→actual machining minutes.
//!
//! Pure math. The wiring layer (`apps/aberp`) records one
//! [`CalibrationSample`] per Completed work-order that was linked back to an
//! auto-quote and carried a recorded actual machining time. This module turns
//! a stream of those samples into a per-family coefficient that scales the
//! engine's geometry-driven estimate so the next quote learns from the last
//! job.
//!
//! **Pure.** No I/O, no clock, no RNG, no global state — same inputs ⇒
//! byte-identical output, matching the engine's purity contract.

use crate::capacity::MachineFamily;
use std::collections::BTreeMap;

/// The coefficient when calibration has nothing to say (too few samples, or a
/// family with no history). Multiplying by 1.0 is the identity — the engine
/// prices exactly as it did before any samples accrued.
pub const CALIBRATION_DEFAULT_COEFFICIENT: f64 = 1.0;
/// Lower clamp: a coefficient can at most halve the estimate.
pub const CALIBRATION_MIN_COEFFICIENT: f64 = 0.5;
/// Upper clamp: a coefficient can at most double the estimate.
pub const CALIBRATION_MAX_COEFFICIENT: f64 = 2.0;
/// Minimum samples (post-window) below which we refuse to trust the data and
/// fall back to [`CALIBRATION_DEFAULT_COEFFICIENT`].
pub const CALIBRATION_MIN_SAMPLES: usize = 5;
/// Trailing window: only the N most-recent samples per family are considered.
pub const CALIBRATION_WINDOW: usize = 10;

/// One observed (estimated, actual) machining-minutes pair for a family.
///
/// `estimated_minutes` is the engine's PRE-coefficient base projection (total
/// over the batch); `actual_minutes` is the MES/operator-recorded actual. The
/// ratio `actual / estimated` is the empirical coefficient the trimmed mean
/// averages. Callers pass samples in chronological order (oldest first); the
/// window takes the most recent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibrationSample {
    /// The machine family the sample belongs to.
    pub family: MachineFamily,
    /// Engine's PRE-coefficient base estimate (total batch minutes).
    pub estimated_minutes: f64,
    /// MES/operator-recorded actual (total batch minutes).
    pub actual_minutes: f64,
}

/// Pure trimmed-mean coefficient for one family.
///
/// Takes the [`CALIBRATION_WINDOW`] most-recent samples for `family`, drops the
/// single highest and single lowest `actual/estimated` ratio, and averages the
/// rest. Clamps to `[0.5, 2.0]`. Returns `1.0` when fewer than
/// [`CALIBRATION_MIN_SAMPLES`] usable samples are present.
///
/// Samples with a non-positive `estimated_minutes` are skipped (no ratio can
/// be formed) rather than silently treated as zero — fail-quiet on a single
/// bad row, never divide by zero.
pub fn coefficient(family: MachineFamily, samples: &[CalibrationSample]) -> f64 {
    let mut ratios: Vec<f64> = samples
        .iter()
        .filter(|s| s.family == family && s.estimated_minutes > 0.0 && s.actual_minutes >= 0.0)
        .rev()
        .take(CALIBRATION_WINDOW)
        .map(|s| s.actual_minutes / s.estimated_minutes)
        .collect();

    if ratios.len() < CALIBRATION_MIN_SAMPLES {
        return CALIBRATION_DEFAULT_COEFFICIENT;
    }

    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // Trimmed mean: drop the lowest and highest one each. `ratios.len() >= 5`
    // so the trimmed slice always has >= 3 elements.
    let trimmed = &ratios[1..ratios.len() - 1];
    let mean = trimmed.iter().sum::<f64>() / trimmed.len() as f64;
    mean.clamp(CALIBRATION_MIN_COEFFICIENT, CALIBRATION_MAX_COEFFICIENT)
}

/// A materialized per-family coefficient table — the set the engine applies to
/// a quote. Built once per quote-create from the current samples.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalibrationTable {
    coeffs: BTreeMap<MachineFamily, f64>,
}

impl CalibrationTable {
    /// The identity table — every family resolves to `1.0`. Existing callers
    /// of [`crate::quote`] use this so behaviour is byte-identical to
    /// pre-calibration pricing.
    pub fn neutral() -> Self {
        Self::default()
    }

    /// Materialize coefficients for every family that appears in `samples`.
    /// Families with no samples are absent and resolve to `1.0` on lookup.
    pub fn from_samples(samples: &[CalibrationSample]) -> Self {
        let mut families: Vec<MachineFamily> = samples.iter().map(|s| s.family).collect();
        families.sort();
        families.dedup();
        let mut coeffs = BTreeMap::new();
        for family in families {
            coeffs.insert(family, coefficient(family, samples));
        }
        Self { coeffs }
    }

    /// The coefficient for `family` (clamped), or `1.0` if the family has no
    /// entry.
    pub fn coefficient(&self, family: MachineFamily) -> f64 {
        self.coeffs
            .get(&family)
            .copied()
            .unwrap_or(CALIBRATION_DEFAULT_COEFFICIENT)
            .clamp(CALIBRATION_MIN_COEFFICIENT, CALIBRATION_MAX_COEFFICIENT)
    }

    /// A stable hash of the (family, coefficient) set for reproducibility — the
    /// `coefficient_set_hash` stamped on every priced quote so a later audit
    /// can answer "which coefficients were in force." Deterministic FNV-1a over
    /// the BTreeMap's sorted entries; the empty (neutral) table hashes to the
    /// FNV offset basis.
    pub fn set_hash(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (family, coeff) in &self.coeffs {
            for byte in format!("{}={coeff:.6};", family.as_db_str()).bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        format!("{hash:016x}")
    }

    /// Iterate the materialized (family, coefficient) pairs in family order.
    pub fn entries(&self) -> impl Iterator<Item = (MachineFamily, f64)> + '_ {
        self.coeffs.iter().map(|(f, c)| (*f, *c))
    }

    /// True iff no family carries a non-default coefficient.
    pub fn is_neutral(&self) -> bool {
        self.coeffs
            .values()
            .all(|c| (c - CALIBRATION_DEFAULT_COEFFICIENT).abs() <= f64::EPSILON)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(estimated: f64, actual: f64) -> CalibrationSample {
        CalibrationSample {
            family: MachineFamily::ThreeAxisMill,
            estimated_minutes: estimated,
            actual_minutes: actual,
        }
    }

    #[test]
    fn fewer_than_five_samples_defaults_to_one() {
        // 4 samples, all ratio 1.5 — still defaults because < MIN_SAMPLES.
        let samples: Vec<_> = (0..4).map(|_| s(10.0, 15.0)).collect();
        assert_eq!(
            coefficient(MachineFamily::ThreeAxisMill, &samples),
            1.0,
            "4 samples must fall back to default 1.0"
        );
    }

    #[test]
    fn nine_samples_trimmed_mean() {
        // 9 ratios: one low outlier (0.1), one high outlier (9.0), seven 1.2s.
        // Trim drops 0.1 and 9.0; mean of the seven 1.2 = 1.2.
        let mut samples = vec![s(10.0, 1.0), s(10.0, 90.0)];
        for _ in 0..7 {
            samples.push(s(10.0, 12.0));
        }
        let c = coefficient(MachineFamily::ThreeAxisMill, &samples);
        assert!((c - 1.2).abs() < 1e-9, "got {c}, expected 1.2 after trim");
    }

    #[test]
    fn ten_samples_uses_full_window() {
        // 10 ratios: 0.5 (low), 5.0 (high), eight 1.0s. Trim → mean 1.0.
        let mut samples = vec![s(10.0, 5.0), s(10.0, 50.0)];
        for _ in 0..8 {
            samples.push(s(10.0, 10.0));
        }
        let c = coefficient(MachineFamily::ThreeAxisMill, &samples);
        assert!((c - 1.0).abs() < 1e-9, "got {c}, expected 1.0");
    }

    #[test]
    fn eleven_samples_window_drops_oldest() {
        // The OLDEST (first) sample is an extreme low that must fall outside
        // the 10-window. 11 samples: first ratio 0.01 (oldest, dropped by
        // window), then ten ratios of 1.3. Window = last 10 → all 1.3, trim
        // → 1.3.
        let mut samples = vec![s(100.0, 1.0)];
        for _ in 0..10 {
            samples.push(s(10.0, 13.0));
        }
        let c = coefficient(MachineFamily::ThreeAxisMill, &samples);
        assert!(
            (c - 1.3).abs() < 1e-9,
            "got {c}, expected 1.3 (oldest extreme outside window)"
        );
    }

    #[test]
    fn clamps_high() {
        // All ratios 10.0 → mean 10.0 → clamped to 2.0.
        let samples: Vec<_> = (0..10).map(|_| s(10.0, 100.0)).collect();
        assert_eq!(coefficient(MachineFamily::ThreeAxisMill, &samples), 2.0);
    }

    #[test]
    fn clamps_low() {
        // All ratios 0.05 → mean 0.05 → clamped to 0.5.
        let samples: Vec<_> = (0..10).map(|_| s(100.0, 5.0)).collect();
        assert_eq!(coefficient(MachineFamily::ThreeAxisMill, &samples), 0.5);
    }

    #[test]
    fn zero_estimated_is_skipped_not_divided() {
        // 5 valid 1.4s + 3 zero-estimated rows. The zeros are skipped; 5
        // valid >= MIN_SAMPLES, trimmed mean of 1.4s = 1.4.
        let mut samples: Vec<_> = (0..5).map(|_| s(10.0, 14.0)).collect();
        for _ in 0..3 {
            samples.push(s(0.0, 5.0));
        }
        let c = coefficient(MachineFamily::ThreeAxisMill, &samples);
        assert!((c - 1.4).abs() < 1e-9, "got {c}, expected 1.4");
    }

    #[test]
    fn table_isolates_families() {
        let three = MachineFamily::ThreeAxisMill;
        let five = MachineFamily::FiveAxisMill;
        let mut samples: Vec<_> = (0..6)
            .map(|_| CalibrationSample {
                family: three,
                estimated_minutes: 10.0,
                actual_minutes: 18.0, // ratio 1.8
            })
            .collect();
        for _ in 0..6 {
            samples.push(CalibrationSample {
                family: five,
                estimated_minutes: 10.0,
                actual_minutes: 6.0, // ratio 0.6
            });
        }
        let table = CalibrationTable::from_samples(&samples);
        assert!((table.coefficient(three) - 1.8).abs() < 1e-9);
        assert!((table.coefficient(five) - 0.6).abs() < 1e-9);
        // A family with no samples resolves to the default.
        assert_eq!(table.coefficient(MachineFamily::Lathe), 1.0);
    }

    #[test]
    fn neutral_table_hash_is_stable_and_neutral() {
        assert!(CalibrationTable::neutral().is_neutral());
        assert_eq!(
            CalibrationTable::neutral().set_hash(),
            CalibrationTable::neutral().set_hash()
        );
    }

    #[test]
    fn set_hash_changes_with_coefficients() {
        let a = CalibrationTable::from_samples(&(0..6).map(|_| s(10.0, 18.0)).collect::<Vec<_>>());
        let b = CalibrationTable::from_samples(&(0..6).map(|_| s(10.0, 6.0)).collect::<Vec<_>>());
        assert_ne!(a.set_hash(), b.set_hash());
        assert_ne!(a.set_hash(), CalibrationTable::neutral().set_hash());
    }
}
