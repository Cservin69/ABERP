// S225 / PR-221 — vitest pins for the Statistics page's pure helpers.
//
// Period-options builder, VAT-rate formatter, percent-change formatter,
// and empty-aggregate detection. No DOM, no fetch — every assertion is
// a function output match.

import { describe, expect, it } from "vitest";
import {
  buildPeriodOptions,
  formatPctChange,
  formatVatRate,
  hasHuf,
  hasEur,
  isAggregateEmpty,
} from "./statistics";

describe("buildPeriodOptions", () => {
  it("first option is the current month in YYYY-MM wire form", () => {
    const today = new Date(2026, 5, 15); // June 15 2026 (month is 0-based)
    const opts = buildPeriodOptions(today);
    expect(opts[0].wire).toBe("2026-06");
    expect(opts[0].label).toContain("This month");
  });

  it("emits last-month option wrapping to December of prior year", () => {
    const today = new Date(2026, 0, 15); // January 15 2026
    const opts = buildPeriodOptions(today);
    const lastMonth = opts.find((o) => o.label.startsWith("Last month"));
    expect(lastMonth?.wire).toBe("2025-12");
  });

  it("emits this-quarter + last-quarter options correctly", () => {
    const today = new Date(2026, 5, 15); // June → Q2
    const opts = buildPeriodOptions(today);
    expect(opts.find((o) => o.label.startsWith("This quarter"))?.wire).toBe(
      "2026-Q2",
    );
    expect(opts.find((o) => o.label.startsWith("Last quarter"))?.wire).toBe(
      "2026-Q1",
    );
  });

  it("last quarter wraps to Q4 of prior year when current is Q1", () => {
    const today = new Date(2026, 1, 15); // February → Q1
    const opts = buildPeriodOptions(today);
    expect(opts.find((o) => o.label.startsWith("Last quarter"))?.wire).toBe(
      "2025-Q4",
    );
  });

  it("always includes an `all` time option", () => {
    const today = new Date(2026, 5, 15);
    const opts = buildPeriodOptions(today);
    const all = opts.find((o) => o.wire === "all");
    expect(all).toBeDefined();
    expect(all?.label).toBe("All time");
  });
});

describe("formatVatRate", () => {
  it("formats whole-percent basis points as `N%`", () => {
    expect(formatVatRate(2700)).toBe("27%");
    expect(formatVatRate(500)).toBe("5%");
    expect(formatVatRate(0)).toBe("0%");
  });

  it("formats fractional-percent basis points with two decimal places", () => {
    expect(formatVatRate(2705)).toBe("27.05%");
    expect(formatVatRate(1850)).toBe("18.50%");
  });

  it("returns em-dash for non-finite", () => {
    expect(formatVatRate(Number.NaN)).toBe("—");
  });
});

describe("formatPctChange", () => {
  it("emits `+` prefix for positive change", () => {
    expect(formatPctChange(22.3)).toBe("+22.3%");
  });

  it("emits bare minus for negative change", () => {
    expect(formatPctChange(-5.0)).toBe("-5%");
    expect(formatPctChange(-5.4)).toBe("-5.4%");
  });

  it("renders `n/a` for null", () => {
    expect(formatPctChange(null)).toBe("n/a");
  });

  it("renders `n/a` for non-finite", () => {
    expect(formatPctChange(Number.NaN)).toBe("n/a");
    expect(formatPctChange(Number.POSITIVE_INFINITY)).toBe("n/a");
  });
});

describe("isAggregateEmpty", () => {
  it("returns true when both currencies are zero with zero count", () => {
    expect(
      isAggregateEmpty({
        huf: { gross_minor: 0, count: 0 },
        eur: { gross_minor: 0, count: 0 },
      }),
    ).toBe(true);
  });

  it("returns false when one currency has data", () => {
    expect(
      isAggregateEmpty({
        huf: { gross_minor: 1000, count: 1 },
        eur: { gross_minor: 0, count: 0 },
      }),
    ).toBe(false);
  });

  it("hasHuf / hasEur respect both amount and count", () => {
    expect(hasHuf({ gross_minor: 0, count: 0 })).toBe(false);
    expect(hasHuf({ gross_minor: 0, count: 1 })).toBe(true);
    expect(hasEur({ gross_minor: 100, count: 0 })).toBe(true);
  });
});
