// S262 / PR-251 — pins the aging-bucket boundaries against the backend
// `reports::aging_bucket_for`. If these drift, a dashboard bucket count
// and its click-through-filtered list disagree.

import { describe, it, expect } from "vitest";
import {
  agingBucketFor,
  canonicalDeadlineIso,
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
//
// Three MORE shapes were found after that fix, by looking at what the
// two languages do BEFORE and AFTER the shape check rather than at the
// shape check itself:
//
//   exotic whitespace  `String.prototype.trim` strips U+FEFF and U+00A0;
//                      Rust's `str::trim` strips U+0085 and U+00A0. Each
//                      difference put the two classifiers on opposite
//                      verdicts. Both sides now trim ASCII whitespace
//                      only, so every one of these is UNDATED on both.
//   a signed year      `time`'s `[year]` accepts an optional sign, so
//                      `+2026-06-15` was a date to Rust and never one
//                      here. Now rejected on both — and at the writer,
//                      so it cannot be stored.
//   a year below 1000  `0001-01-01` is an ordinary `time::Date`, but
//                      `Date.UTC(1, 0, 1)` means 1901, so this side
//                      rejected a date the report accepts. Fixed here,
//                      because Rust is right about it.
// ─────────────────────────────────────────────────────────────────────
// Written as escapes on purpose: these four code points are invisible or
// indistinguishable from a space in an editor, and a pin whose input can be
// silently normalised away by a formatter is not a pin.
const FEFF = "\uFEFF"; // ZWNBSP — JS trims it, Rust does not
const NEL = "\u0085"; // NEL — Rust trims it, JS does not
const NBSP = "\u00A0"; // NBSP — both trim it, agreeing by luck
const VT = "\u000B"; // VT — JS trims it, `is_ascii_whitespace` does not

const PARITY: ReadonlyArray<[string, boolean]> = [
  ["2026-06-30", true], // canonical
  [" 2026-06-30 ", true], // ASCII space — trimmed on both sides
  ["\t2026-06-30\n", true], // tabs/newlines trim the same way
  ["\r2026-06-30\f", true], // CR / form feed are ASCII whitespace too
  ["0001-01-01", true], // year < 1000 is a real date
  ["0026-06-15", true], // and so is a two-digit one
  ["2026-02-30", false], // impossible; JS rolled it to 2026-03-02
  ["2026-13-45", false], // out of range both ways
  ["2026-6-3", false], // unpadded — not the canonical shape
  ["2026-06-30T00:00:00Z", false], // RFC3339 is not a deadline shape
  ["30/06/2026", false], // swapped format
  ["+2026-06-15", false], // signed year — `time::Date::parse` said Ok
  ["-2026-06-15", false], // and the negative form too
  [`${FEFF}2026-06-30`, false], // JS trimmed it, Rust did not
  [`2026-06-30${FEFF}`, false],
  [`${NEL}2026-06-30`, false], // Rust trimmed it, JS did not
  [`2026-06-30${NEL}`, false],
  [`${NBSP}2026-06-30`, false], // both trimmed it — by luck
  [`2026-06-30${NBSP}`, false],
  [`${VT}2026-06-30`, false], // JS trims it; ASCII-whitespace does not
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

  it("a year below 1000 buckets from its real year, not from 19xx", () => {
    // `Date.UTC(26, …)` is 1926. Without the `setUTCFullYear` correction
    // the round-trip check rejected this outright; with a naive
    // correction it would bucket from the wrong century.
    expect(agingBucketFor(TODAY, "0026-06-15")).toBe("d90_plus");
    expect(canonicalDeadlineIso("0026-06-15")).toBe("0026-06-15");
    expect(canonicalDeadlineIso("0001-01-01")).toBe("0001-01-01");
  });
});

describe("canonicalDeadlineIso — the string form callers may compare", () => {
  it("renders the classifier's verdict as a padded YYYY-MM-DD", () => {
    expect(canonicalDeadlineIso("2026-06-30")).toBe("2026-06-30");
    // ASCII padding is stripped, so the result sorts by DATE and not by
    // the leading space the stored value happened to carry.
    expect(canonicalDeadlineIso(" 2026-12-30 ")).toBe("2026-12-30");
    expect(canonicalDeadlineIso("\t2026-12-30\n")).toBe("2026-12-30");
  });

  it("is null for exactly the rows hasNoRecordedDeadline excludes", () => {
    for (const [input] of PARITY) {
      expect(canonicalDeadlineIso(input) === null).toBe(hasNoRecordedDeadline(input));
    }
    expect(canonicalDeadlineIso(null)).toBeNull();
    expect(canonicalDeadlineIso(undefined)).toBeNull();
  });

  it("pads a year below 1000 to four digits so it still sorts first", () => {
    // The failure this prevents: a three-digit rendering ("26-06-15")
    // sorts AFTER "2026-08-14", so the oldest possible deadline would
    // read as a future one.
    expect(canonicalDeadlineIso("0026-06-15")).toBe("0026-06-15");
    expect(canonicalDeadlineIso("0026-06-15")! < "2026-08-14").toBe(true);
    expect(canonicalDeadlineIso("0001-01-01")! < "2026-08-14").toBe(true);
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
