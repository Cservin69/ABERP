// S443 — vitest pins for the verdict-chip pure helpers. The colour
// mapping is load-bearing (a green chip on a critical deviation hides
// a defect), so pin every verdict → class + the minor/major-share-
// warning collapse.

import { describe, expect, it } from "vitest";

import type { Verdict } from "./api";
import { verdictChipClass, verdictLabel } from "./verdict";

describe("verdictChipClass", () => {
  it("pass → positive (green)", () => {
    expect(verdictChipClass("pass")).toBe("verdict-chip verdict-chip--pass");
  });

  it("minor AND major share the warning (yellow) class", () => {
    expect(verdictChipClass("minor")).toBe(
      "verdict-chip verdict-chip--warning",
    );
    expect(verdictChipClass("major")).toBe(
      "verdict-chip verdict-chip--warning",
    );
  });

  it("critical → negative (red)", () => {
    expect(verdictChipClass("critical")).toBe(
      "verdict-chip verdict-chip--critical",
    );
  });

  it("calibration_stale → muted (grey)", () => {
    expect(verdictChipClass("calibration_stale")).toBe(
      "verdict-chip verdict-chip--stale",
    );
  });

  it("covers every Verdict variant (exhaustive over the closed vocab)", () => {
    const all: Verdict[] = [
      "pass",
      "minor",
      "major",
      "critical",
      "calibration_stale",
    ];
    for (const v of all) {
      expect(verdictChipClass(v).startsWith("verdict-chip ")).toBe(true);
    }
  });

  it("falls back to the muted class for an unknown value", () => {
    expect(verdictChipClass("nonsense")).toBe(
      "verdict-chip verdict-chip--stale",
    );
  });
});

describe("verdictLabel", () => {
  it("returns a bilingual label for each known verdict", () => {
    expect(verdictLabel("pass")).toContain("Pass");
    expect(verdictLabel("minor")).toContain("Minor");
    expect(verdictLabel("major")).toContain("Major");
    expect(verdictLabel("critical")).toContain("Critical");
    expect(verdictLabel("calibration_stale")).toContain("Calibration stale");
  });

  it("falls back to the raw string for an unknown verdict", () => {
    expect(verdictLabel("plasma")).toBe("plasma");
  });
});
