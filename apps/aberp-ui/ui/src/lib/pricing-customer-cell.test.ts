// S401 — vitest pins for the Customer-cell renderer (Auto-árazás tab).
//
// These tests verify *intent* (CLAUDE.md #9): the operator must see the
// buying company as the primary anchor, and a missing company must
// surface a placeholder rather than a blank line. They fail if the
// placeholder logic regresses to passing company through verbatim.

import { describe, it, expect } from "vitest";

import { customerCell, COMPANY_PLACEHOLDER } from "./pricing-customer-cell";

describe("customerCell", () => {
  it("company-present: surfaces the company as primary, person + email below", () => {
    const c = customerCell(
      "Acme Manufacturing Kft.",
      "Ervin Csengeri",
      "ervin@aben.ch",
    );
    expect(c.company).toBe("Acme Manufacturing Kft.");
    expect(c.companyMissing).toBe(false);
    expect(c.person).toBe("Ervin Csengeri");
    expect(c.email).toBe("ervin@aben.ch");
  });

  it("company-present: trims surrounding whitespace", () => {
    const c = customerCell("  Acme Kft.  ", "Ervin", "ervin@aben.ch");
    expect(c.company).toBe("Acme Kft.");
    expect(c.companyMissing).toBe(false);
  });

  it("company-missing (null legacy row): shows placeholder, keeps person + email", () => {
    const c = customerCell(null, "Ervin Csengeri", "ervin@aben.ch");
    expect(c.company).toBe(COMPANY_PLACEHOLDER);
    expect(c.companyMissing).toBe(true);
    // The whole point: the operator still sees who to contact.
    expect(c.person).toBe("Ervin Csengeri");
    expect(c.email).toBe("ervin@aben.ch");
  });

  it("company-missing (undefined): shows placeholder", () => {
    const c = customerCell(undefined, "Ervin", "ervin@aben.ch");
    expect(c.company).toBe(COMPANY_PLACEHOLDER);
    expect(c.companyMissing).toBe(true);
  });

  it("company-missing (blank string, buyer left field empty): shows placeholder", () => {
    const c = customerCell("", "Ervin", "ervin@aben.ch");
    expect(c.company).toBe(COMPANY_PLACEHOLDER);
    expect(c.companyMissing).toBe(true);
  });

  it("company-missing (whitespace-only): treated as no company", () => {
    const c = customerCell("   ", "Ervin", "ervin@aben.ch");
    expect(c.company).toBe(COMPANY_PLACEHOLDER);
    expect(c.companyMissing).toBe(true);
  });

  it("person-only legacy shape: person present, company + nothing else fabricated", () => {
    const c = customerCell(null, "Solo Buyer", "solo@example.com");
    expect(c.companyMissing).toBe(true);
    expect(c.person).toBe("Solo Buyer");
    expect(c.email).toBe("solo@example.com");
    // Placeholder is the only synthesized value — person/email pass through.
    expect(c.company).toBe(COMPANY_PLACEHOLDER);
  });

  it("placeholder is bilingual so either-locale operators read it as absent", () => {
    expect(COMPANY_PLACEHOLDER).toContain("No company");
    expect(COMPANY_PLACEHOLDER).toContain("Nincs cég");
  });
});
