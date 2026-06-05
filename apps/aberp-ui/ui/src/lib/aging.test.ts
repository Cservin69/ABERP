// S262 / PR-251 — pins the aging-bucket boundaries against the backend
// `reports::aging_bucket_for`. If these drift, a dashboard bucket count
// and its click-through-filtered list disagree.

import { describe, it, expect } from "vitest";
import {
  agingBucketFor,
  parseAgingBucket,
  panelField,
  AGING_BUCKETS,
  type AgingBucket,
} from "./aging";

const TODAY = "2026-06-30";

describe("agingBucketFor — boundaries mirror reports::aging_bucket_for", () => {
  // overdue_days = today − deadline; thresholds at 0 / 30 / 60 / 90.
  const cases: Array<[string, AgingBucket]> = [
    ["2026-07-15", "current"], // future deadline → not due
    ["2026-06-30", "current"], // due today → overdue 0 → current
    ["2026-06-29", "d1_30"], // overdue 1
    ["2026-05-31", "d1_30"], // overdue 30 (boundary, inclusive)
    ["2026-05-30", "d31_60"], // overdue 31
    ["2026-05-01", "d31_60"], // overdue 60 (boundary)
    ["2026-04-30", "d61_90"], // overdue 61
    ["2026-04-01", "d61_90"], // overdue 90 (boundary)
    ["2026-03-31", "d90_plus"], // overdue 91
  ];
  for (const [deadline, bucket] of cases) {
    it(`${deadline} → ${bucket}`, () => {
      expect(agingBucketFor(TODAY, deadline)).toBe(bucket);
    });
  }

  it("returns null on an unparseable deadline (row excluded, not coerced)", () => {
    expect(agingBucketFor(TODAY, "not-a-date")).toBeNull();
  });
});

describe("parseAgingBucket — closed vocab", () => {
  it("accepts every legal bucket", () => {
    for (const b of AGING_BUCKETS) expect(parseAgingBucket(b)).toBe(b);
  });
  it("discards unknown vocab", () => {
    expect(parseAgingBucket("days_1_30")).toBeNull();
    expect(parseAgingBucket("")).toBeNull();
    expect(parseAgingBucket("CURRENT")).toBeNull();
  });
});

describe("panelField — maps to the AgingPanel wire keys", () => {
  it("maps each bucket to its backend field name", () => {
    expect(panelField("current")).toBe("current");
    expect(panelField("d1_30")).toBe("days_1_30");
    expect(panelField("d31_60")).toBe("days_31_60");
    expect(panelField("d61_90")).toBe("days_61_90");
    expect(panelField("d90_plus")).toBe("days_90_plus");
  });
});
