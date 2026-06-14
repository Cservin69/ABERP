// S401 — Customer-cell renderer for the Pricing tab (Auto-árazás).
//
// Extracted from `PricingJobsList.svelte` so the company-placeholder
// logic can be vitest-pinned without component-render tooling, mirroring
// the `lib/pricing-failure-kind.ts` pattern. The Svelte component renders
// the returned three-line shape directly into the Customer column:
//
//   Line 1 (primary, large)  → company (or the placeholder)
//   Line 2 (muted, smaller)  → person name
//   Line 3 (muted, smaller)  → email
//
// Honors [[hulye-biztos]]: the operator must see *who they are quoting*
// — i.e. the buying company — at a glance, not just a contact name.
// Legacy rows (PROD_v2.27.[0-55]) carry `null` company and buyers who
// left the field blank carry `""`; both render an italic placeholder so
// the cell is never visually empty (CLAUDE.md #12 — surface the gap,
// don't hide it behind a blank line).

export interface CustomerCell {
  /** Company name, or [`COMPANY_PLACEHOLDER`] when none was captured. */
  company: string;
  /** `true` when no company was captured → render the placeholder
   *  italic + muted instead of as a bold primary company name. */
  companyMissing: boolean;
  /** Buyer's contact person name (verbatim from the row). */
  person: string;
  /** Buyer's email (verbatim from the row). */
  email: string;
}

/** Shown in the primary company line when the row carries no company.
 *  Bilingual HU / EN so an operator on either locale understands the
 *  field is absent rather than empty by mistake. */
export const COMPANY_PLACEHOLDER = "— Nincs cég / No company";

/** Resolve a pricing-job row's buyer fields into the Customer-cell shape.
 *
 *  `company` accepts `string | null | undefined` so the same helper
 *  covers the legacy `null` row, the blank `""` row, and the
 *  whitespace-only `"  "` row — all three are treated as "no company"
 *  and surface the placeholder. */
export function customerCell(
  company: string | null | undefined,
  person: string,
  email: string,
): CustomerCell {
  const trimmed = (company ?? "").trim();
  const companyMissing = trimmed.length === 0;
  return {
    company: companyMissing ? COMPANY_PLACEHOLDER : trimmed,
    companyMissing,
    person,
    email,
  };
}
