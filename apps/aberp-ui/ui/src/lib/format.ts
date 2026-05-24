// PR-44ε / session-53 — currency-aware formatters for the operator
// surface, per ADR-0037 §1.a + §1.c.
//
// Pre-PR-44ε the only formatter on the SPA was the inline
// `Intl.NumberFormat("hu-HU", {style: "currency", currency: "HUF"})`
// in InvoiceList.svelte / InvoiceDetail.svelte (one copy each, byte-
// identical). PR-44γ stamped the EUR-cents interpretation on
// `total_gross` for non-HUF invoices, so the per-row formatter now
// needs the currency tag to pick the symbol AND the minor-unit
// divisor — without it, an EUR invoice's cents render as forints
// (off by 100× plus wrong symbol).
//
// Module-level rather than per-component for two reasons:
//
// 1. **Single source of truth.** The HUF formatter exists in two
//    Svelte files pre-PR-44ε; adding the EUR + rate-metadata
//    formatters in each file would triple the duplication and
//    every per-formatter tweak would need a search-and-replace.
//    Module-level matches `labels.ts`'s posture for per-label
//    affordances — affordance shape lives in one file, components
//    import.
// 2. **Testability.** Vitest pins the four formatters at gate
//    time (`format.test.ts`); inline formatters in Svelte files
//    are reachable from the component-render path only, which we
//    cannot exercise from vitest without a Svelte 5 test runner
//    setup (deferred per CLAUDE.md rule 2 — minimum code).
//
// Naming convention: every export is a small pure function with
// no implicit defaults — `formatTotal(value, currency)` makes the
// currency dependency syntactic, so a regression that drops the
// `currency` argument at a call site is a TS error (`Argument of
// type ... is not assignable`) rather than a silent
// misinterpretation.

import type { Currency } from "./api";

// Hungarian conventions per the reference invoice template
// (see agent memory `reference_aberp_invoice_template.md`):
//   - HUF: no fractional part, space-separated thousands, trailing
//     " Ft" suffix (e.g. `654 883 Ft`). `Intl.NumberFormat`'s
//     `style: "currency", currency: "HUF"` produces exactly this
//     shape under the `hu-HU` locale.
//   - EUR: two fractional digits, decimal comma (Hungarian
//     convention), thin-space thousands, leading `€` symbol
//     (e.g. `€8 636,00`). `Intl.NumberFormat`'s
//     `style: "currency", currency: "EUR"` under `hu-HU` produces
//     this shape; whether the symbol leads or trails depends on
//     the runtime ICU data, but on every modern browser/Node it
//     leads for EUR under `hu-HU`.
const HUF_FORMATTER = new Intl.NumberFormat("hu-HU", {
  style: "currency",
  currency: "HUF",
  minimumFractionDigits: 0,
  maximumFractionDigits: 0,
  useGrouping: true,
});

// `currencyDisplay: "narrowSymbol"` forces the `€` glyph in place of
// the ICU-default `EUR` ISO-code suffix that some Node / browser
// builds emit under `hu-HU` (verified empirically on Node 20 ICU —
// the default `"symbol"` falls back to the ISO code for EUR but
// `"narrowSymbol"` resolves to `€`). The printed-invoice reference
// template uses the `€` glyph, so the SPA matches.
//
// `useGrouping: true` is explicit because some ICU builds drop the
// thousand separator for EUR under `hu-HU` when the option is left
// at its default (also verified empirically on Node 20 — the
// default produced `"8636,00 €"` rather than `"8 636,00 €"`).
const EUR_FORMATTER = new Intl.NumberFormat("hu-HU", {
  style: "currency",
  currency: "EUR",
  currencyDisplay: "narrowSymbol",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
  useGrouping: true,
});

/** Format an invoice's `total_gross` for the operator surface.
 *
 * The minor-unit interpretation depends on `currency`:
 *   - `"HUF"` — `value` is whole forints (HUF has no sub-unit per
 *     ADR-0009 §1 / `Huf(pub i64)`); rendered as `"654 883 Ft"`.
 *   - `"EUR"` — `value` is EUR cents (the PR-44γ posture stores
 *     EUR amounts in the underlying `i64` as cents); divided by
 *     100 before formatting and rendered as `"€8 636,00"`.
 *
 * `null` renders as the em-dash `"—"` per the existing list-row
 * + detail-modal posture for unset totals.
 */
export function formatTotal(value: number | null, currency: Currency): string {
  if (value === null) return "—";
  if (currency === "EUR") {
    return EUR_FORMATTER.format(value / 100);
  }
  return HUF_FORMATTER.format(value);
}

/** Format the MNB exchange rate for the operator surface.
 *
 * Normalises to 6 decimal places per ADR-0037 §1.c / C11 — the
 * backend serialises at exactly 6 decimals (the audit-ledger
 * stamp + the NAV body's `<exchangeRate>` field both pin this
 * shape), but a future drift that drops decimals on the wire
 * would render with fewer here unless we re-format. A
 * non-parseable input passes through unchanged so a malformed
 * value is operator-visible rather than silently zeroed per
 * CLAUDE.md rule 12.
 */
export function formatRate(rate: string): string {
  const n = Number(rate);
  if (!Number.isFinite(n)) return rate;
  return n.toFixed(6);
}

/** Format an HUF-equivalent amount per the Hungarian convention.
 *
 * Reuses [`formatTotal`] with the HUF branch — the per-VAT-rate
 * HUF amount and the gross HUF-equivalent on the printed-invoice
 * reference template use the same `"654 883 Ft"` shape.
 */
export function formatHufEquivalent(value: number): string {
  return HUF_FORMATTER.format(value);
}

/** Format an MNB-rate publication date for the operator surface.
 *
 * Pass-through — the backend emits ISO-8601 `YYYY-MM-DD`
 * (`OffsetDateTime::format` + `format_description!("[year]-[month]-[day]")`
 * at PR-44γ), which is what the operator surface displays. A
 * future PR that wants a Hungarian-locale render (e.g.,
 * `2026. 05. 22.`) can extend this helper additively without
 * touching the call sites per CLAUDE.md rule 3.
 */
export function formatRateDate(date: string): string {
  return date;
}

/** PR-44ε.UI / session-58 — build the browser-side download filename
 * for the printed-invoice PDF. The Rust side emits the same shape on
 * the `Content-Disposition` header (`serve::pdf_filename_for_invoice`),
 * but the SPA cannot read that header through Tauri's `invoke`
 * boundary; the SPA composes the filename locally for the synthetic
 * `<a download>` click instead. Both sides emit
 * `invoice_<invoice_number>.pdf` verbatim — pinned at the Rust side
 * by `pdf_filename_uses_invoice_number` and at the SPA side by the
 * vitest in `format.test.ts`.
 */
export function filenameForInvoice(invoiceNumber: string): string {
  return `invoice_${invoiceNumber}.pdf`;
}
