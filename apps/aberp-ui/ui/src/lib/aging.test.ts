// S262 / PR-251 — pins the aging-bucket boundaries against the backend
// `reports::aging_bucket_for`. If these drift, a dashboard bucket count
// and its click-through-filtered list disagree.

import { describe, it, expect } from "vitest";
import {
  agingBucketFor,
  hasNoRecordedDeadline,
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

  // A row with NO recorded deadline is a legacy NAV import, taken as
  // SETTLED, and is out of outstanding entirely — no total, no bucket, no
  // hygiene counter (`reports::aging_placement` returning `None`). This
  // mirror must return `null` for exactly those rows and never coerce
  // them into a bucket: PR #68 put them in `d90_plus`, and if this module
  // kept doing that the operator would click "90+ nap = 0" on an empty
  // tile and land on a list full of legacy invoices — the same tile↔list
  // drift in the opposite direction.
  it("returns null for an unreadable deadline — excluded, not coerced", () => {
    expect(agingBucketFor(TODAY, "not-a-date")).toBeNull();
    expect(agingBucketFor(TODAY, "30/06/2026")).toBeNull();
    expect(agingBucketFor(TODAY, "")).toBeNull();
  });

  it("returns null for a MISSING deadline", () => {
    expect(agingBucketFor(TODAY, null)).toBeNull();
    expect(agingBucketFor(TODAY, undefined)).toBeNull();
  });

  it("never lands a deadline-less row in d90_plus", () => {
    // The specific regression: PR #68's imputation. `d90_plus` must be
    // reachable ONLY from a deadline that was read and is >90 days past.
    for (const deadline of ["not-a-date", "", null, undefined]) {
      expect(agingBucketFor(TODAY, deadline)).not.toBe("d90_plus");
    }
    expect(agingBucketFor(TODAY, "2026-01-01")).toBe("d90_plus");
  });

  it("returns a real bucket for every readable deadline", () => {
    // The other direction: an exclusion that widened to swallow healthy
    // rows would empty the panel, and the buckets would still sum to the
    // total while both were wrong.
    for (const deadline of ["2026-05-31", "2026-06-30", "2026-08-14"]) {
      expect(AGING_BUCKETS).toContain(agingBucketFor(TODAY, deadline));
    }
  });
});

// ─────────────────────────────────────────────────────────────────────
// CLASSIFIER PARITY with the backend.
//
// `hasNoRecordedDeadline` and `reports::parse_iso_date` decide which
// invoices are outstanding AT ALL, on two sides of the wire. A shape they
// disagree about is a row one of them counts and the other excludes —
// tile says 3, list shows 2, or worse, a receivable that is in the total
// but in no drill-down.
//
// This table is duplicated verbatim in the Rust pin
// `deadline_classifier_parity_with_the_spa` (reports.rs). Both must
// change together; that is the point of writing it out twice.
//
// The naive `Date.parse(`${d}T00:00:00Z`)` this replaced disagreed on
// three of these. None was reachable — the writers store canonical
// `YYYY-MM-DD` and the SQL projections now truncate to the date head —
// but "unreachable" was doing a lot of unguarded work.
// ─────────────────────────────────────────────────────────────────────
const PARITY: ReadonlyArray<[string, boolean]> = [
  ["2026-06-30", true], // canonical
  [" 2026-06-30 ", true], // Rust `str::trim`; Date.parse said NaN
  ["\t2026-06-30\n", true], // tabs/newlines trim the same way
  ["2026-02-30", false], // impossible; JS rolled it to 2026-03-02
  ["2026-13-45", false], // out of range both ways
  ["2026-6-3", false], // unpadded — not the canonical shape
  ["2026-06-30T00:00:00Z", false], // RFC3339 is not a deadline shape
  ["30/06/2026", false], // swapped format
  ["not-a-date", false],
  ["", false],
];

describe("deadline classifier parity with reports::parse_iso_date", () => {
  for (const [input, isDated] of PARITY) {
    it(`${JSON.stringify(input)} → ${isDated ? "dated" : "undated"}`, () => {
      expect(hasNoRecordedDeadline(input)).toBe(!isDated);
    });
  }

  it("a rolled-over date never reaches a bucket", () => {
    // The sharpest of the three: JS turns 2026-02-30 into 2026-03-02, so
    // the old code bucketed a receivable from a date that does not
    // exist — and disagreed with a backend that had excluded it.
    expect(agingBucketFor(TODAY, "2026-02-30")).toBeNull();
  });

  it("trimmed whitespace buckets identically to the trimmed value", () => {
    expect(agingBucketFor(TODAY, " 2026-05-31 ")).toBe(agingBucketFor(TODAY, "2026-05-31"));
  });
});

describe("hasNoRecordedDeadline — the one predicate both facets share", () => {
  // Exported so the aging facet and the hygiene facet cannot drift on
  // this point one edit at a time. `payment_deadline === null` in a
  // component is the shape that silently keeps the unparseable half.
  it("is true for missing and for unreadable, alike", () => {
    for (const deadline of [null, undefined, "", "not-a-date", "30/06/2026", "2026-13-45"]) {
      expect(hasNoRecordedDeadline(deadline)).toBe(true);
    }
  });

  it("is false for a readable deadline", () => {
    for (const deadline of ["2026-05-31", "2026-08-14", "2027-01-01"]) {
      expect(hasNoRecordedDeadline(deadline)).toBe(false);
    }
  });

  it("agrees with agingBucketFor on exactly which rows are excluded", () => {
    // The two must not be able to disagree — the hygiene facet reads the
    // predicate and the aging facet reads the bucket, and they are the
    // drill-downs of two tiles that made the same exclusion.
    for (const deadline of [null, undefined, "", "junk", "2026-05-31", "2026-08-14"]) {
      expect(hasNoRecordedDeadline(deadline)).toBe(agingBucketFor(TODAY, deadline) === null);
    }
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
