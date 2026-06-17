// PR-53 / session-73 — vitest pin for the TenantSettings page's
// wire-to-form helper. Mirror-invariant composer-pin posture
// (A156 / A161 / A163): a regression that mis-maps a backend field
// to the wrong form slot would silently strand the operator's saved
// values when they open the Settings page. The helper is pure-
// module so it's testable without mounting the Svelte component.

import { describe, expect, it } from "vitest";

import type { SellerInfoResponse } from "./api";
import {
  formFromSellerInfo,
  hoursToSeconds,
  secondsToHours,
} from "./tenant-settings";

describe("formFromSellerInfo", () => {
  it("maps every populated field one-to-one", () => {
    const response: SellerInfoResponse = {
      legal_name: "ABERP Supplier Kft.",
      tax_number: "12345678-1-42",
      eu_vat_number: "HU12345678",
      address: {
        country_code: "HU",
        postal_code: "1011",
        city: "Budapest",
        street: "Fő utca 1.",
      },
      bank: {
        account_number: "12345678-12345678-12345678",
        iban: "HU12345678901234567890",
        name: "OTP Bank",
        swift_bic: "OTPVHUHB",
      },
    };
    const form = formFromSellerInfo(response);
    expect(form.legalName).toBe("ABERP Supplier Kft.");
    expect(form.taxNumber).toBe("12345678-1-42");
    expect(form.euVatNumber).toBe("HU12345678");
    expect(form.addressCountryCode).toBe("HU");
    expect(form.addressPostalCode).toBe("1011");
    expect(form.addressCity).toBe("Budapest");
    expect(form.addressStreet).toBe("Fő utca 1.");
    expect(form.bankAccountNumber).toBe("12345678-12345678-12345678");
    expect(form.iban).toBe("HU12345678901234567890");
    expect(form.bankName).toBe("OTP Bank");
    expect(form.swiftBic).toBe("OTPVHUHB");
  });

  it("folds null optional fields into empty strings", () => {
    // The form treats `""` as "operator skipped this"; the composer
    // (`composeSellerConfigBody`) re-folds `""` back to `null` on
    // write. A regression that surfaced `null` to the form layer
    // would crash the input bind value at the DOM seam.
    const response: SellerInfoResponse = {
      legal_name: "Solo Kft.",
      tax_number: "12345678-1-42",
      eu_vat_number: null,
      address: {
        country_code: "HU",
        postal_code: "1011",
        city: "Budapest",
        street: "Fő utca 1.",
      },
      bank: {
        account_number: null,
        iban: null,
        name: null,
        swift_bic: null,
      },
    };
    const form = formFromSellerInfo(response);
    expect(form.euVatNumber).toBe("");
    expect(form.bankAccountNumber).toBe("");
    expect(form.iban).toBe("");
    expect(form.bankName).toBe("");
    expect(form.swiftBic).toBe("");
  });
});

describe("QC calibration window seconds↔hours", () => {
  it("converts seconds to hours (24h default round-trips)", () => {
    expect(secondsToHours(86400)).toBe(24);
    expect(secondsToHours(3600)).toBe(1);
    expect(hoursToSeconds(24)).toBe(86400);
  });

  it("rounds fractional hours to whole seconds on write", () => {
    // 0.5h → 1800s; the input may carry a decimal so the write must
    // not strand a non-integer second count.
    expect(hoursToSeconds(0.5)).toBe(1800);
  });

  it("refuses non-positive / non-finite hours (returns null)", () => {
    // A regression here would let the operator persist a 0/negative
    // window, which the backend would treat as "never stale" or worse
    // ([[hulye-biztos]]); the control must refuse the save instead.
    expect(hoursToSeconds(0)).toBeNull();
    expect(hoursToSeconds(-4)).toBeNull();
    expect(hoursToSeconds(Number.NaN)).toBeNull();
    expect(hoursToSeconds(Number.POSITIVE_INFINITY)).toBeNull();
  });
});
