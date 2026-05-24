// PR-44ζ / session-59 — vitest pin for the IssueInvoice form-to-
// request-body composer.
//
// The composer is the load-bearing seam between the SPA form state
// and the wire `IssueInvoiceRequest` shape. A regression that
// renames a field (or drops the trim, or mis-maps the currency)
// would surface as a 400 from the backend with a confusing error
// rather than a silent bad-issuance — but the test catches it at
// gate time before any backend roundtrip.
//
// Mirror invariant per A156 / A161: the backend's
// `serve::IssueInvoiceRequest` Deserialize and this composer agree
// on the wire field names (camelCase JSON, UPPERCASE currency).

import { describe, expect, it } from "vitest";

import { composeIssueInvoiceBody, emptyForm } from "./issue-invoice";

describe("composeIssueInvoiceBody", () => {
  it("reshapes HUF form state into the wire body verbatim", () => {
    const form = {
      ...emptyForm(),
      supplierName: "ABERP Supplier Kft.",
      supplierTaxNumber: "12345678-1-42",
      supplierCountryCode: "HU",
      supplierPostalCode: "1011",
      supplierCity: "Budapest",
      supplierStreet: "Fő utca 1.",
      customerName: "Vevő Kft.",
      customerTaxNumber: "87654321-2-13",
      currency: "HUF" as const,
      lines: [
        {
          description: "Widget A",
          quantity: 2,
          unitPriceMinor: 1000,
          vatRatePercent: 27,
        },
      ],
    };

    const body = composeIssueInvoiceBody(form);

    expect(body).toEqual({
      supplier: {
        taxNumber: "12345678-1-42",
        name: "ABERP Supplier Kft.",
        address: {
          countryCode: "HU",
          postalCode: "1011",
          city: "Budapest",
          street: "Fő utca 1.",
        },
      },
      customer: {
        taxNumber: "87654321-2-13",
        name: "Vevő Kft.",
      },
      lines: [
        {
          description: "Widget A",
          quantity: 2,
          unitPrice: 1000,
          vatRatePercent: 27,
        },
      ],
      currency: "HUF",
    });
  });

  it("emits EUR currency verbatim on the EUR branch", () => {
    const form = {
      ...emptyForm(),
      supplierName: "ABERP Kft.",
      supplierTaxNumber: "12345678-1-42",
      supplierCountryCode: "HU",
      supplierPostalCode: "1011",
      supplierCity: "Budapest",
      supplierStreet: "Fő utca 1.",
      customerName: "EU Buyer GmbH",
      customerTaxNumber: "DE123456789",
      currency: "EUR" as const,
      lines: [
        {
          description: "Consulting (1h)",
          quantity: 8,
          unitPriceMinor: 12500, // 125.00 EUR in cents
          vatRatePercent: 27,
        },
      ],
    };

    const body = composeIssueInvoiceBody(form);

    expect(body.currency).toBe("EUR");
    expect(body.lines[0]).toEqual({
      description: "Consulting (1h)",
      quantity: 8,
      unitPrice: 12500,
      vatRatePercent: 27,
    });
  });

  it("trims whitespace on every string field the backend validates", () => {
    // Backend `validate_issue_request` `.trim()`-checks supplier
    // name + tax number + customer name + tax number; the composer
    // pre-trims so the wire body matches what the backend reads.
    // A regression that drops a trim would let a `"  "` value pass
    // the SPA's required-field check but fail the backend's.
    const form = {
      ...emptyForm(),
      supplierName: "  Trimmed Supplier  ",
      supplierTaxNumber: "  12345678-1-42  ",
      supplierCountryCode: " HU ",
      supplierPostalCode: " 1011 ",
      supplierCity: " Budapest ",
      supplierStreet: " Fő utca 1. ",
      customerName: "  Trimmed Customer  ",
      customerTaxNumber: "  87654321-2-13  ",
      currency: "HUF" as const,
      lines: [
        {
          description: "  Trimmed description  ",
          quantity: 1,
          unitPriceMinor: 500,
          vatRatePercent: 27,
        },
      ],
    };

    const body = composeIssueInvoiceBody(form);

    expect(body.supplier.name).toBe("Trimmed Supplier");
    expect(body.supplier.taxNumber).toBe("12345678-1-42");
    expect(body.supplier.address.countryCode).toBe("HU");
    expect(body.supplier.address.city).toBe("Budapest");
    expect(body.customer.name).toBe("Trimmed Customer");
    expect(body.customer.taxNumber).toBe("87654321-2-13");
    expect(body.lines[0].description).toBe("Trimmed description");
  });

  it("preserves all lines when there are multiple", () => {
    // Per-line ordering matters — the backend stamps the lines in
    // the order received. A regression that re-ordered or
    // deduplicated would corrupt the invoice silently.
    const form = {
      ...emptyForm(),
      supplierName: "S",
      supplierTaxNumber: "x",
      supplierCountryCode: "HU",
      supplierPostalCode: "1",
      supplierCity: "B",
      supplierStreet: "S",
      customerName: "C",
      customerTaxNumber: "y",
      currency: "HUF" as const,
      lines: [
        { description: "A", quantity: 1, unitPriceMinor: 100, vatRatePercent: 27 },
        { description: "B", quantity: 2, unitPriceMinor: 200, vatRatePercent: 5 },
        { description: "C", quantity: 3, unitPriceMinor: 300, vatRatePercent: 0 },
      ],
    };

    const body = composeIssueInvoiceBody(form);

    expect(body.lines.length).toBe(3);
    expect(body.lines.map((l) => l.description)).toEqual(["A", "B", "C"]);
    expect(body.lines.map((l) => l.unitPrice)).toEqual([100, 200, 300]);
    expect(body.lines.map((l) => l.vatRatePercent)).toEqual([27, 5, 0]);
  });
});
