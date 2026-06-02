// S225 / PR-221 — pure helpers for the Statistics page.
//
// Period-selector options + a few small formatters. No DOM, no fetch.
// Pinned by `statistics.test.ts`.

import { formatHufEquivalent, formatTotal } from "./format";
import type { Currency } from "./api";

/** Closed-vocab union of date-axis values the SPA emits to the
 * backend. Mirrors the Rust `aberp::reports::DateBasis` parse vocab. */
export type DateBasis = "teljesites" | "issued";

/** A pre-built period option for the dropdown. `wire` is the literal
 * `?period=` query value (`"2026-06"`, `"2026-Q2"`, `"all"`, etc.);
 * `label` is the bilingual display string. */
export interface PeriodOption {
  wire: string;
  label: string;
}

/** Two-digit zero-padded month, year unchanged. */
function pad2(n: number): string {
  return n.toString().padStart(2, "0");
}

/** Quarter index (1-4) for a 1-based month (1-12). */
function quarterFor(month: number): number {
  return Math.floor((month - 1) / 3) + 1;
}

/** Build the quick-pick period options the SPA renders in the
 * dropdown. Order matters: "current month" is first so the default
 * matches `parsePeriod("")` on the backend (current month). The
 * `today` arg is injected for vitest pinning; callers pass `new Date()`.
 *
 * The list is intentionally short — adding "this fiscal year" or
 * other operator-driven slices is a one-line widening here. */
export function buildPeriodOptions(today: Date): PeriodOption[] {
  const y = today.getFullYear();
  const m = today.getMonth() + 1;
  const q = quarterFor(m);

  // Previous month (wraps to December of prior year)
  let pmY = y;
  let pmM = m - 1;
  if (pmM < 1) {
    pmM = 12;
    pmY = y - 1;
  }

  // Previous quarter (wraps to Q4 of prior year)
  let pqY = y;
  let pqQ = q - 1;
  if (pqQ < 1) {
    pqQ = 4;
    pqY = y - 1;
  }

  return [
    { wire: `${y}-${pad2(m)}`, label: `This month — ${y}-${pad2(m)}` },
    { wire: `${pmY}-${pad2(pmM)}`, label: `Last month — ${pmY}-${pad2(pmM)}` },
    { wire: `${y}-Q${q}`, label: `This quarter — ${y} Q${q}` },
    { wire: `${pqY}-Q${pqQ}`, label: `Last quarter — ${pqY} Q${pqQ}` },
    { wire: `${y}`, label: `This year — ${y}` },
    { wire: `${y - 1}`, label: `Last year — ${y - 1}` },
    { wire: "all", label: "All time" },
  ];
}

/** Format a per-currency minor-unit amount via the existing
 * `formatTotal` helper. Kept as a thin wrapper so the dashboard's
 * call sites read declaratively. */
export function formatMinor(value: number, currency: Currency): string {
  return formatTotal(value, currency);
}

/** Format a HUF minor-unit amount specifically — for the parallel
 * HUF column in cards that show both currencies. */
export function formatHuf(value: number): string {
  return formatHufEquivalent(value);
}

/** Format a VAT-rate basis-points value (`2700`) into the operator-
 * facing percentage label (`"27%"`). Basis points → percent is integer
 * division by 100; non-integer percentages (`2705` = `27.05%`) keep
 * two decimal places. */
export function formatVatRate(basisPoints: number): string {
  if (!Number.isFinite(basisPoints)) return "—";
  if (basisPoints % 100 === 0) {
    return `${basisPoints / 100}%`;
  }
  return `${(basisPoints / 100).toFixed(2)}%`;
}

/** Format a period-over-period percentage change as a signed string
 * with one decimal place (`"+22.3%"` / `"-5.0%"` / `"n/a"` for
 * `null`). */
export function formatPctChange(pct: number | null): string {
  if (pct === null || !Number.isFinite(pct)) return "n/a";
  const rounded = Math.round(pct * 10) / 10;
  const sign = rounded > 0 ? "+" : "";
  return `${sign}${rounded}%`;
}

/** Decide whether to show a card's HUF column. Either there's a
 * non-zero amount, or there is a non-zero count (which can happen if
 * gross_minor saturated to 0 — defensive). */
export function hasHuf(amt: { gross_minor: number; count: number }): boolean {
  return amt.gross_minor !== 0 || amt.count > 0;
}

export function hasEur(amt: { gross_minor: number; count: number }): boolean {
  return amt.gross_minor !== 0 || amt.count > 0;
}

/** True iff a `CurrencyAggregate` has no data in either currency.
 * Used by cards to switch into an em-dash empty state instead of
 * rendering "0 Ft / €0,00". */
export function isAggregateEmpty(agg: {
  huf: { gross_minor: number; count: number };
  eur: { gross_minor: number; count: number };
}): boolean {
  return !hasHuf(agg.huf) && !hasEur(agg.eur);
}
