// S267 / PR-256 — shared display helpers for the four quoting-tunables
// pages. Bilingual labels for the closed-vocab enums (FeatureType,
// SizeBucket, ToleranceRange) so each page renders the operator-
// readable name beside the durable wire form.
//
// Closed-vocab + deny-default: an unknown wire form returns the
// verbatim string. The Rust side already validated on write, so
// "unknown" should only appear when the SPA is older than the backend.

import type { FeatureType, SizeBucket, ToleranceRange } from "./api";

export function featureTypeLabel(t: FeatureType | string): string {
  switch (t) {
    case "pocket":
      return "Pocket / Zseb";
    case "hole":
      return "Hole / Furat";
    case "slot":
      return "Slot / Horony";
    case "thread":
      return "Thread / Menet";
    case "undercut_5axis":
      return "Undercut (5-axis) / Alávágás";
    case "thin_wall":
      return "Thin wall / Vékony fal";
    case "surface":
      return "Surface / Felület";
    case "engraving":
      return "Engraving / Gravírozás";
    default:
      return t;
  }
}

export function sizeBucketLabel(b: SizeBucket | string): string {
  switch (b) {
    case "XS":
      return "XS (< 10mm)";
    case "S":
      return "S (10–30mm)";
    case "M":
      return "M (30–80mm)";
    case "L":
      return "L (80–200mm)";
    case "XL":
      return "XL (≥ 200mm)";
    default:
      return b;
  }
}

export function toleranceRangeLabel(t: ToleranceRange | string): string {
  switch (t) {
    case "loose":
      return "Loose (±0.1mm+)";
    case "standard":
      return "Standard (±0.05mm)";
    case "tight":
      return "Tight (±0.02mm)";
    case "precision":
      return "Precision (±0.01mm)";
    case "ultra_precision":
      return "Ultra-precision (≤ ±0.005mm)";
    default:
      return t;
  }
}

/** Format a signed fractional adjustment (e.g. -0.05 → "−5.0%"). */
export function fmtPct(p: number): string {
  if (!Number.isFinite(p)) return "—";
  const sign = p > 0 ? "+" : p < 0 ? "−" : "";
  const abs = Math.abs(p) * 100;
  return `${sign}${abs.toFixed(1)}%`;
}
