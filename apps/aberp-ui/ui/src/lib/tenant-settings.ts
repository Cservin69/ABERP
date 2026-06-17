// PR-53 / session-73 — pure-module helper for the TenantSettings
// page. Mirror-invariant composer-pin pattern (A156 / A161 / A163):
// the helper that converts the GET /api/seller-info wire response
// into the `SellerConfigForm` shape lives outside the Svelte
// component so vitest can pin it without mounting the page.
//
// The form shape is shared with the SellerConfigWizard (PR-51); this
// module's only job is the wire-shape → form-shape conversion (the
// reverse direction is `composeSellerConfigBody` in `seller-config.ts`).

import type { SellerInfoResponse } from "./api";
import type { SellerConfigForm } from "./seller-config";

/** PR-53 / session-73 — turn the backend's typed seller-info response
 * into the SPA's form-state shape. Nullable bank + EU-VAT fields fold
 * to empty strings (the form treats `""` as "operator skipped this";
 * the composer folds blank back to `null` on write). */
export function formFromSellerInfo(
  response: SellerInfoResponse,
): SellerConfigForm {
  return {
    legalName: response.legal_name,
    taxNumber: response.tax_number,
    euVatNumber: response.eu_vat_number ?? "",
    addressCountryCode: response.address.country_code,
    addressPostalCode: response.address.postal_code,
    addressCity: response.address.city,
    addressStreet: response.address.street,
    bankAccountNumber: response.bank.account_number ?? "",
    iban: response.bank.iban ?? "",
    bankName: response.bank.name ?? "",
    swiftBic: response.bank.swift_bic ?? "",
  };
}

// S443 — QC stale-calibration window is stored per-tenant in seconds but
// surfaced to the operator in whole-ish hours on the TenantsList page.
// Keep the seconds↔hours conversion pure so the row control + its
// validation can be pinned without mounting the Svelte component.

/** S443 — seconds → hours for display (e.g. 86400 → 24). */
export function secondsToHours(seconds: number): number {
  return seconds / 3600;
}

/** S443 — hours → seconds for the write. Returns `null` for any value
 * that isn't a finite number > 0 (blank/0/negative/NaN), so the caller
 * can refuse the save rather than persist a nonsensical window
 * ([[hulye-biztos]]). */
export function hoursToSeconds(hours: number): number | null {
  if (!Number.isFinite(hours) || hours <= 0) return null;
  return Math.round(hours * 3600);
}
