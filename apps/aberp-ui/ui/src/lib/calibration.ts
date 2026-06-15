// S429 — closed-loop calibration page: pure data-shaping helpers.
//
// The component (`routes/Calibration.svelte`) is a thin shell: it fetches
// `getCalibration()` and renders what these pure functions return. All logic
// that can be unit-tested offline (chip colour, coefficient formatting,
// sparkline geometry) lives here so vitest pins it cold.

/** One sample point for a family's chart (chronological, oldest→newest). */
export interface CalibrationSamplePoint {
  estimated_minutes: number;
  actual_minutes: number;
  /** actual / estimated — the empirical coefficient for this single job. */
  ratio: number;
}

/** Per-family calibration view. */
export interface FamilyCalibration {
  machine_family: string;
  coefficient: number;
  sample_count: number;
  samples: CalibrationSamplePoint[];
}

/** A WO that lost calibration signal (no MES actual / no priced breakdown). */
export interface CalibrationSkip {
  at_utc: string;
  quote_id: string;
  work_order_id: string;
  reason: string;
}

/** The whole Calibration page payload (mirrors the Rust `CalibrationOverview`). */
export interface CalibrationOverview {
  families: FamilyCalibration[];
  recent_skips: CalibrationSkip[];
  coefficient_set_hash: string;
}

/** Defensive normalize of an untyped invoke() payload into a safe overview. */
export function normalizeOverview(raw: unknown): CalibrationOverview {
  const obj = (raw ?? {}) as Partial<CalibrationOverview>;
  return {
    families: Array.isArray(obj.families) ? obj.families : [],
    recent_skips: Array.isArray(obj.recent_skips) ? obj.recent_skips : [],
    coefficient_set_hash:
      typeof obj.coefficient_set_hash === "string" ? obj.coefficient_set_hash : "",
  };
}

/** Format a coefficient as the operator-facing "0.93x" badge. */
export function formatCoefficient(coefficient: number): string {
  return `${coefficient.toFixed(2)}x`;
}

/**
 * Dark-theme chip colour class for a coefficient. A coefficient near 1.0 is
 * calibrated (neutral); far below 1 means we over-estimate (green, we can
 * quote faster); far above 1 means we under-estimate (red, jobs run long).
 * Bands: |Δ| ≤ 0.05 neutral, ≤ 0.25 warn, else strong.
 */
export function coefficientChipClass(coefficient: number): string {
  const delta = coefficient - 1.0;
  const mag = Math.abs(delta);
  if (mag <= 0.05) return "chip-neutral";
  if (mag <= 0.25) return delta < 0 ? "chip-under" : "chip-over";
  return delta < 0 ? "chip-under-strong" : "chip-over-strong";
}

/** A bar in the sparkline (normalized 0..1 heights for est + actual). */
export interface SparkBar {
  estimatedFraction: number;
  actualFraction: number;
  ratio: number;
}

/**
 * Normalize a family's samples into bar fractions for an inline SVG chart.
 * Both estimated and actual bars share one scale (the max minute value across
 * both series) so they're visually comparable. Empty input → empty array.
 */
export function sparklineBars(samples: CalibrationSamplePoint[]): SparkBar[] {
  const max = samples.reduce(
    (m, s) => Math.max(m, s.estimated_minutes, s.actual_minutes),
    0,
  );
  if (max <= 0) {
    return samples.map((s) => ({
      estimatedFraction: 0,
      actualFraction: 0,
      ratio: s.ratio,
    }));
  }
  return samples.map((s) => ({
    estimatedFraction: Math.max(0, s.estimated_minutes) / max,
    actualFraction: Math.max(0, s.actual_minutes) / max,
    ratio: s.ratio,
  }));
}

/** Sort families for stable display: most samples first, then family name. */
export function sortFamilies(families: FamilyCalibration[]): FamilyCalibration[] {
  return [...families].sort(
    (a, b) =>
      b.sample_count - a.sample_count ||
      a.machine_family.localeCompare(b.machine_family),
  );
}

/** Human label for a coefficient's direction, for the chip tooltip. */
export function coefficientHint(coefficient: number): string {
  const delta = coefficient - 1.0;
  if (Math.abs(delta) <= 0.05) return "Calibrated — estimates match actuals";
  if (delta < 0) return "Jobs run faster than estimated";
  return "Jobs run longer than estimated";
}
