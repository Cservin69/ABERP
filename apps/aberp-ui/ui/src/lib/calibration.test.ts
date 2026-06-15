// S429 — unit tests for the Calibration page pure logic.

import { describe, expect, it } from "vitest";
import {
  coefficientChipClass,
  coefficientHint,
  formatCoefficient,
  normalizeOverview,
  sortFamilies,
  sparklineBars,
  type CalibrationSamplePoint,
  type FamilyCalibration,
} from "./calibration";

function pt(est: number, act: number): CalibrationSamplePoint {
  return { estimated_minutes: est, actual_minutes: act, ratio: est > 0 ? act / est : 0 };
}

function fam(over: Partial<FamilyCalibration> = {}): FamilyCalibration {
  return {
    machine_family: "3-axis-mill",
    coefficient: 1.0,
    sample_count: 0,
    samples: [],
    ...over,
  };
}

describe("formatCoefficient", () => {
  it("renders the 0.93x badge", () => {
    expect(formatCoefficient(0.931)).toBe("0.93x");
    expect(formatCoefficient(1)).toBe("1.00x");
    expect(formatCoefficient(1.18)).toBe("1.18x");
  });
});

describe("coefficientChipClass", () => {
  it("neutral within ±0.05", () => {
    expect(coefficientChipClass(1.0)).toBe("chip-neutral");
    expect(coefficientChipClass(0.96)).toBe("chip-neutral");
    expect(coefficientChipClass(1.04)).toBe("chip-neutral");
  });
  it("under (green) below 1, over (amber) above 1 in mid band", () => {
    expect(coefficientChipClass(0.85)).toBe("chip-under");
    expect(coefficientChipClass(1.2)).toBe("chip-over");
  });
  it("strong bands beyond ±0.25", () => {
    expect(coefficientChipClass(0.5)).toBe("chip-under-strong");
    expect(coefficientChipClass(1.6)).toBe("chip-over-strong");
  });
});

describe("coefficientHint", () => {
  it("describes direction", () => {
    expect(coefficientHint(1.0)).toMatch(/Calibrated/);
    expect(coefficientHint(0.8)).toMatch(/faster/);
    expect(coefficientHint(1.3)).toMatch(/longer/);
  });
});

describe("sparklineBars", () => {
  it("shares one scale across est + actual", () => {
    const bars = sparklineBars([pt(10, 20), pt(5, 10)]);
    // max value is 20 → first actual is full height.
    expect(bars[0].actualFraction).toBeCloseTo(1.0);
    expect(bars[0].estimatedFraction).toBeCloseTo(0.5);
    expect(bars[1].estimatedFraction).toBeCloseTo(0.25);
  });
  it("empty input → empty output", () => {
    expect(sparklineBars([])).toEqual([]);
  });
  it("all-zero minutes → zero fractions, no NaN", () => {
    const bars = sparklineBars([pt(0, 0)]);
    expect(bars[0].estimatedFraction).toBe(0);
    expect(bars[0].actualFraction).toBe(0);
  });
});

describe("sortFamilies", () => {
  it("most samples first, then name", () => {
    const sorted = sortFamilies([
      fam({ machine_family: "lathe", sample_count: 2 }),
      fam({ machine_family: "5-axis-mill", sample_count: 9 }),
      fam({ machine_family: "grinder", sample_count: 2 }),
    ]);
    expect(sorted.map((f) => f.machine_family)).toEqual([
      "5-axis-mill",
      "grinder",
      "lathe",
    ]);
  });
});

describe("normalizeOverview", () => {
  it("fills defaults for a malformed payload", () => {
    const ov = normalizeOverview(null);
    expect(ov.families).toEqual([]);
    expect(ov.recent_skips).toEqual([]);
    expect(ov.coefficient_set_hash).toBe("");
  });
  it("passes through a well-formed payload", () => {
    const ov = normalizeOverview({
      families: [fam({ sample_count: 3 })],
      recent_skips: [
        { at_utc: "t", quote_id: "q", work_order_id: "w", reason: "no time" },
      ],
      coefficient_set_hash: "abc",
    });
    expect(ov.families).toHaveLength(1);
    expect(ov.recent_skips).toHaveLength(1);
    expect(ov.coefficient_set_hash).toBe("abc");
  });
});
