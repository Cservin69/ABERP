// S262 / PR-251 — pure aging-bucket helpers shared by the Finance
// dashboard (StatisticsPage) and the two invoice lists (InvoiceList /
// IncomingInvoiceList). No DOM, no fetch. Pinned by `aging.test.ts`.
//
// The bucket boundaries MIRROR the backend `reports::aging_bucket_for`
// (apps/aberp/src/reports.rs) EXACTLY so a dashboard bucket count and the
// click-through-filtered list agree on which rows fall in a bucket:
//
//     overdue_days = today − deadline   (whole calendar days)
//       <= 0  → current        (not yet due / due today)
//       1..30 → d1_30
//      31..60 → d31_60
//      61..90 → d61_90
//        > 90 → d90_plus
//     NO recorded deadline (missing or unreadable) → not outstanding at
//       all: excluded from every bucket, mirroring `aging_placement`
//       returning `None`.
//
// If the two ever drift, the operator clicks "31–60 nap = 3 invoices" and
// lands on a list showing 2 — the canonical fail-loud regression this
// shared module + its pins exist to prevent (CLAUDE.md rule 7/12).

import type { AgingPanel, AmountAggregate } from "./api";

/** Closed-vocab aging bucket. The wire form (`d1_30` …) is the URL
 * deep-link token used by the dashboard → list click-through. */
export type AgingBucket = "current" | "d1_30" | "d31_60" | "d61_90" | "d90_plus";

/** Render / iteration order — overdue severity ascending. */
export const AGING_BUCKETS: readonly AgingBucket[] = [
  "current",
  "d1_30",
  "d31_60",
  "d61_90",
  "d90_plus",
];

/** Runtime membership table for URL-param validation (a hand-typed or
 * stale-bookmark `?aging=garbage` is discarded, never coerced). */
const LEGAL: ReadonlySet<string> = new Set(AGING_BUCKETS);

/** Parse an untrusted value into an [`AgingBucket`], or `null` if it is
 * absent or not in the closed vocab. Accepts `undefined` so callers can
 * pass a `Map.get` result directly. */
export function parseAgingBucket(s: string | null | undefined): AgingBucket | null {
  return s != null && LEGAL.has(s) ? (s as AgingBucket) : null;
}

/** Bilingual (HU primary, EN secondary) labels for the dashboard rows. */
export const AGING_LABELS: Readonly<Record<AgingBucket, string>> = {
  current: "Lejárat előtt / Not due",
  d1_30: "1–30 nap / days",
  d31_60: "31–60 nap / days",
  d61_90: "61–90 nap / days",
  d90_plus: "90+ nap / days",
};

/** Map a bucket to its field on the backend [`AgingPanel`] wire shape so
 * the dashboard reads `panel[fieldFor(bucket)]` in one expression. */
export function panelField(bucket: AgingBucket): keyof AgingPanel {
  switch (bucket) {
    case "current":
      return "current";
    case "d1_30":
      return "days_1_30";
    case "d31_60":
      return "days_31_60";
    case "d61_90":
      return "days_61_90";
    case "d90_plus":
      return "days_90_plus";
  }
}

/** Read a bucket's [`AmountAggregate`] off a panel. */
export function bucketAmount(panel: AgingPanel, bucket: AgingBucket): AmountAggregate {
  return panel[panelField(bucket)];
}

// The former `dayDiff` helper lived here and did its own
// `Date.parse(`${iso}T00:00:00Z`)`. It is gone deliberately rather than
// left unused: a SECOND, laxer date parser sitting next to the canonical
// one is how the two classifiers drifted apart in the first place. Day
// arithmetic now happens on the `Date` objects `parseRecordedDeadline`
// returns — still anchored at UTC midnight, so the difference is an exact
// integer day count with no DST drift, matching the backend's
// `time::Date` `whole_days()`.

/** True when a row has NO recorded payment deadline — the field is
 * `null`/`undefined`, or holds a string that will not parse as a date.
 *
 * Such an invoice is a LEGACY import from NAV, issued and settled under
 * the prior system, and the operator's ruling is that they are all paid.
 * It is therefore not outstanding: excluded from the receivables /
 * payables total, from every aging bucket, and from the past-deadline
 * hygiene counters alike. Mirrors `reports::aging_placement` returning
 * `None`.
 *
 * THE ONE PREDICATE both drill-down facets share. The aging facet and the
 * hygiene facet must agree about these rows — each is the click-through
 * for a dashboard tile that has already excluded them, so a list that
 * kept them shows more rows than the tile it came from. Sharing the
 * definition is what stops the two from drifting apart one edit at a
 * time; `payment_deadline === null` in a component is the shape that
 * misses the unparseable half. */
export function hasNoRecordedDeadline(deadlineIso: string | null | undefined): boolean {
  return parseRecordedDeadline(deadlineIso) === null;
}

/** Canonical `YYYY-MM-DD` shape. Anchored at both ends — a bare
 * `Date.parse` would accept far more than the backend does. */
const CANONICAL_DEADLINE = /^(\d{4})-(\d{2})-(\d{2})$/;

/** Parse a recorded payment deadline, or `null` when there isn't one.
 *
 * MIRRORS `reports::parse_iso_date` EXACTLY, and the parity is pinned in
 * both languages over one shared vocabulary (`aging.test.ts` here,
 * `deadline_classifier_parity_with_the_spa` there). The two classifiers
 * decide which invoices are outstanding at all, so a shape they disagree
 * about is a row the tile counts and the list does not, or vice versa.
 *
 * The naive form — `Date.parse(`${d}T00:00:00Z`)` — disagreed on three
 * shapes, none of them reachable today but none of them guarded either:
 *
 *   `" 2026-06-30 "`   Rust trims and accepts; `Date.parse` returned NaN,
 *                      so the SPA called a dated invoice undated.
 *   `"2026-02-30"`     Rust rejects an impossible calendar date; JS
 *                      SILENTLY ROLLS OVER to 2026-03-02, so the SPA
 *                      called an undated invoice dated — and bucketed it
 *                      from a date that does not exist.
 *   `"2026-06-30T…Z"`  both reject, but for the wrong reason: the
 *                      interpolation produced a double suffix. Guarded at
 *                      source now (the SQL projections truncate to the
 *                      date head), and rejected here deliberately rather
 *                      than by accident.
 *
 * So: trim, require the canonical shape, then confirm the date did not
 * roll over. Whitespace tolerance matches Rust's `str::trim`; the
 * round-trip check is what `time::Date::parse` does for free. */
export function parseRecordedDeadline(deadlineIso: string | null | undefined): Date | null {
  if (deadlineIso == null) return null;
  const m = CANONICAL_DEADLINE.exec(deadlineIso.trim());
  if (m === null) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const parsed = new Date(Date.UTC(y, mo - 1, d));
  // `Date.UTC(2026, 1, 30)` happily yields 2026-03-02. Comparing the
  // components back is the only way to tell a real date from a rolled
  // one, and it is exactly the check the backend parser applies.
  if (
    parsed.getUTCFullYear() !== y ||
    parsed.getUTCMonth() !== mo - 1 ||
    parsed.getUTCDate() !== d
  ) {
    return null;
  }
  return parsed;
}

/** Classify a payment deadline into its aging bucket relative to `today`.
 * `todayIso` is ISO `YYYY-MM-DD`.
 *
 * Returns `null` for a row with no recorded deadline — callers MUST read
 * that as "not outstanding, show it under no bucket", not as "unknown, so
 * put it somewhere safe". See [`hasNoRecordedDeadline`].
 *
 * Mirrors `reports::aging_bucket_for` for every readable deadline —
 * unchanged, and deliberately so: this change moves undated rows only.
 * A revision that also nudged healthy invoices between buckets would be
 * worse than either behaviour it replaced. */
export function agingBucketFor(
  todayIso: string,
  deadlineIso: string | null | undefined,
): AgingBucket | null {
  const deadline = parseRecordedDeadline(deadlineIso);
  if (deadline === null) return null;
  // `todayIso` goes through the SAME parser, so a caller cannot get a
  // bucket out of a deadline the backend would have rejected by pairing
  // it with a today the backend would have rejected too. Unreachable in
  // practice — `todayIsoLocal()` is canonical by construction.
  const today = parseRecordedDeadline(todayIso);
  if (today === null) return null;
  const overdue = Math.round((today.getTime() - deadline.getTime()) / 86_400_000);
  if (overdue <= 0) return "current";
  if (overdue <= 30) return "d1_30";
  if (overdue <= 60) return "d31_60";
  if (overdue <= 90) return "d61_90";
  return "d90_plus";
}

/** Today's date as a local ISO `YYYY-MM-DD` string. The aging anchor;
 * the report's `period.today` echo uses the same wall-clock day. */
export function todayIsoLocal(): string {
  const d = new Date();
  const y = d.getFullYear();
  const m = `${d.getMonth() + 1}`.padStart(2, "0");
  const day = `${d.getDate()}`.padStart(2, "0");
  return `${y}-${m}-${day}`;
}
