// S411 / session-411 — sort + filter helpers for the operator
// Auto-árazás (Pricing) tab. Ports the PR-94 invoice-list pattern
// (`invoice-list.ts :: compareInvoices` + `filterInvoices`) to the
// pricing-pipeline row shape so an operator staring at a 100-row dump
// can collapse it to "what needs my attention now" in one click
// ([[hulye-biztos]]).
//
// Two pure helpers, both pinned by `pricing-jobs-list.test.ts`:
//
//   1. `sortJobs(jobs, key, dir)` — stable, direction-aware comparator
//      over the four operator-meaningful columns (updated_at, customer,
//      state, price). Ties go to `quote_id` ascending so the render
//      order is reproducible across refreshes (CLAUDE.md rule 12 — no
//      silent reshuffle on identical inputs). Null prices cluster at the
//      bottom regardless of dir, same posture as the invoice-list total
//      column.
//   2. `filterJobs(jobs, spec)` — state-facet + free-text search. The
//      facet and the needle AND together (every gate must accept the row).
//
// ── CONFLICT SURFACED (CLAUDE.md rule 7) ───────────────────────────────
// The S411 brief named five state chips: Posted / Failed / Pending /
// Refused / Archived. Only THREE are reachable on THIS tab:
//
//   * `posted`  → terminal success                         → chip "posted"
//   * `failed`  → terminal failure (retryable)             → chip "failed"
//   * the five in-flight states (fetched / extracting / pricing /
//     rendering / posting_back)                            → chip "pending"
//
// `refused` is NOT a pricing-pipeline state — it belongs to the *other*
// operator tab (the approved-quote pickup queue, S403's `rejected`
// reuse). `archived` IS a pricing state (S414) but `quote_pricing_jobs ::
// list_jobs` excludes it server-side (`AND state != 'archived'`), so the
// SPA never receives an archived row. A "Refused" or "Archived" chip
// here would be a dead control that always matches zero rows — dropped
// per CLAUDE.md rule 13 (delete the part that shouldn't exist) and rule
// 12 (don't present a no-op affordance). The brief's parenthetical
// "(surviving states after S413's hygiene work)" describes the pickup
// queue, not this pipeline tab.

import { applySortDir, localeCompareHu, type SortDir } from "./list-sort";

/** S411 — closed-vocab of columns the operator can sort by. The four
 * the brief named as load-bearing; mirrors the renderable column set on
 * `PricingJobsList.svelte`. Material / Qty / Error stay unsortable
 * (CLAUDE.md rule 3 — surgical; the brief scoped these four). */
export type PricingSortKey = "updated_at" | "customer" | "state" | "price";

export type { SortDir };

/** S411 — state-facet vocab. `"All"` short-circuits the gate; the other
 * three map to the only reachable pipeline states (see module docblock).
 * `"pending"` folds the five in-flight states into one operator-facing
 * bucket ("still working on it"); the operator's attention-worthy bucket
 * is `"failed"`. */
export type PricingStateFacet = "All" | "pending" | "posted" | "failed";

/** S411 — the five non-terminal pipeline states the daemon advances
 * through. Folded into the `"pending"` facet. Mirrors the
 * `next_actionable_job` WHERE clause in `quote_pricing_jobs.rs`; kept as
 * its own export so the test pins the exact membership (a drift here vs.
 * the backend would silently mis-bucket a row). */
export const PRICING_PENDING_STATES: readonly string[] = [
  "fetched",
  "extracting",
  "pricing",
  "rendering",
  "posting_back",
];

/** S411 — structural shape the helpers inspect. A subset of
 * `PricingJobRow` (api.ts); both helpers read only these named fields, so
 * widening `PricingJobRow` is a transparent additive change. */
export interface PricingJobSortRow {
  quote_id: string;
  state: string;
  updated_at: string;
  customer_name: string;
  customer_company: string | null;
  material_grade: string;
  total_price_eur: number | null;
}

/** S411 — filter spec. `state === "All"` and an empty `search` are both
 * open gates. The "Clear filters" button on the empty-state resets to
 * [`EMPTY_PRICING_FILTER`]. */
export interface PricingFilterSpec {
  search: string;
  state: PricingStateFacet;
}

/** S411 — every gate open. */
export const EMPTY_PRICING_FILTER: PricingFilterSpec = {
  search: "",
  state: "All",
};

/** S411 — `true` iff no facet is engaged and no search needle is set.
 * The renderer surfaces the empty-state "Clear filters" button only when
 * this is `false` (CLAUDE.md rule 12 — no no-op affordance). */
export function isPricingFilterEmpty(spec: PricingFilterSpec): boolean {
  return spec.state === "All" && spec.search.trim().length === 0;
}

/** S411 — the operator-facing customer string for the `customer` sort
 * key: the buying company when present, else the contact name. Mirrors
 * the Customer-cell primary line (`pricing-customer-cell.ts` surfaces
 * company-or-placeholder), so sorting matches what the eye reads. A
 * blank / whitespace-only company falls back to the name (never the
 * placeholder string — sorting by a literal "— No company" would
 * cluster legacy rows in the middle of the alphabet). */
function customerSortKey(row: PricingJobSortRow): string {
  const company = (row.customer_company ?? "").trim();
  return company.length > 0 ? company : row.customer_name;
}

/** S411 — pipeline-natural ordinal for the `state` sort key. Mirrors the
 * daemon's progression fetched → … → posting_back → posted, with failed
 * last (the operator's attention bucket sinks to the bottom in ascending
 * order). Unknown states sort after everything (defensive; loud-fail on
 * an unrecognised state is the parser's job upstream, not the sort's). */
function stateOrdinal(state: string): number {
  switch (state) {
    case "fetched":
      return 0;
    case "extracting":
      return 1;
    case "pricing":
      return 2;
    case "rendering":
      return 3;
    case "posting_back":
      return 4;
    case "posted":
      return 5;
    case "failed":
      return 6;
    default:
      return 99;
  }
}

/** S411 — `quote_id` tiebreaker, ascending regardless of the selected
 * sort dir. ULIDs are lex-ordered = mint-time-ordered; an ascending
 * tiebreak keeps the render order reproducible on every dir toggle for
 * tied rows (CLAUDE.md rule 12). */
function quoteIdTiebreak(a: PricingJobSortRow, b: PricingJobSortRow): number {
  if (a.quote_id < b.quote_id) return -1;
  if (a.quote_id > b.quote_id) return 1;
  return 0;
}

/** S411 — per-column comparator. Returns a `(a, b) → number` suitable
 * for `Array.prototype.sort` (stable in ES2019+). Null prices cluster at
 * the bottom regardless of dir (the null carve-out is applied BEFORE the
 * dir flip — same load-bearing discipline as `invoice-list.ts ::
 * nullsLastCompare`). Ties fall to `quote_id` ascending. */
export function compareJobs(
  a: PricingJobSortRow,
  b: PricingJobSortRow,
  key: PricingSortKey,
  dir: SortDir,
): number {
  // Null-last carve-out FIRST (price is the only nullable sort column).
  if (key === "price") {
    const an = a.total_price_eur;
    const bn = b.total_price_eur;
    const aNull = an === null || an === undefined;
    const bNull = bn === null || bn === undefined;
    if (aNull && bNull) return quoteIdTiebreak(a, b);
    if (aNull) return 1; // a sinks, dir-invariant
    if (bNull) return -1; // b sinks, dir-invariant
    const cmp = (an as number) - (bn as number);
    if (cmp !== 0) return applySortDir(cmp, dir);
    return quoteIdTiebreak(a, b);
  }

  let cmp: number;
  switch (key) {
    case "updated_at":
      // RFC3339 from the daemon is fixed-offset → lex compare == chrono
      // compare. No `new Date()` (would re-interpret per local tz).
      cmp =
        a.updated_at < b.updated_at ? -1 : a.updated_at > b.updated_at ? 1 : 0;
      break;
    case "customer":
      cmp = localeCompareHu(customerSortKey(a), customerSortKey(b));
      break;
    case "state":
      cmp = stateOrdinal(a.state) - stateOrdinal(b.state);
      break;
  }
  if (cmp !== 0) return applySortDir(cmp, dir);
  return quoteIdTiebreak(a, b);
}

/** S411 — sort a job list by `key` + `dir`. Returns a NEW array (does
 * not mutate the input — the caller's `$state` rows array stays the
 * source of truth). */
export function sortJobs<R extends PricingJobSortRow>(
  jobs: R[],
  key: PricingSortKey,
  dir: SortDir,
): R[] {
  return jobs.slice().sort((a, b) => compareJobs(a, b, key, dir));
}

/** S411 — does a row's state pass the facet? `"pending"` matches the
 * five in-flight states; the other two facets match exactly. */
function statePasses(state: string, facet: PricingStateFacet): boolean {
  switch (facet) {
    case "All":
      return true;
    case "pending":
      return PRICING_PENDING_STATES.includes(state);
    case "posted":
      return state === "posted";
    case "failed":
      return state === "failed";
  }
}

/** S411 — does a row match the free-text needle? Case-insensitive
 * substring over Ref (quote_id) OR customer name OR customer company OR
 * material grade. An empty / whitespace-only needle is an open gate. */
function searchPasses(row: PricingJobSortRow, search: string): boolean {
  const needle = search.trim().toLowerCase();
  if (needle.length === 0) return true;
  const haystacks = [
    row.quote_id,
    row.customer_name,
    row.customer_company ?? "",
    row.material_grade,
  ];
  return haystacks.some((h) => h.toLowerCase().includes(needle));
}

/** S411 — state-facet + free-text filter. The facet and the needle AND
 * together. Returns a NEW array. `spec` fields are optional so the
 * renderer can pass a partial (`{ state: "failed" }`) without spelling
 * out an empty search. */
export function filterJobs<R extends PricingJobSortRow>(
  jobs: R[],
  spec: { state?: PricingStateFacet; search?: string },
): R[] {
  const facet = spec.state ?? "All";
  const search = spec.search ?? "";
  return jobs.filter(
    (row) => statePasses(row.state, facet) && searchPasses(row, search),
  );
}
